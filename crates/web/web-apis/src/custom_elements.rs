//! The `window.customElements` binding.
//!
//! [`browser_dom::CustomElementRegistry`] is JS-agnostic: it stores opaque
//! [`browser_dom::CallbackHandle`]s instead of JavaScript functions. This module
//! owns the handle → JS-function map (on [`CustomElementHost`]) and the promises
//! created by `whenDefined`, and delegates name validation and definition
//! storage to the DOM registry.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use browser_dom::{CallbackHandle, CustomElementRegistry, Document};
use browser_js::{JsError, Runtime, Value};

use crate::{HostState, deferred};

/// Host bookkeeping for the custom element registry.
#[derive(Default)]
pub struct CustomElementHost {
    /// Maps a DOM [`CallbackHandle`] to the JS constructor function.
    pub callbacks: HashMap<CallbackHandle, Value>,
    /// Promises created by `whenDefined` for names that are not defined yet.
    pub pending: HashMap<String, Vec<deferred::Deferred>>,
}

impl CustomElementHost {
    /// Creates empty bookkeeping.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Installs `window.customElements`.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let registry = runtime.new_object();
    runtime.set_global("customElements", registry.clone());

    runtime.define_host_method(registry.clone(), "define", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| define(runtime, &state, args))
    });

    runtime.define_host_method(registry.clone(), "get", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("customElements.get needs a name"))?;
            Ok(constructor_for(&state, &name).unwrap_or(Value::Undefined))
        })
    });

    runtime.define_host_method(registry.clone(), "whenDefined", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("customElements.whenDefined needs a name"))?;
            Ok(when_defined(runtime, &state, &name))
        })
    });
}

fn document_registry(state: &Rc<RefCell<HostState>>) -> Rc<RefCell<Document>> {
    state.borrow().document.clone()
}

fn define(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    if args.len() < 2 {
        return Err(JsError::type_error(
            "customElements.define needs a name and constructor",
        ));
    }
    let name = args[0].to_string_value();
    let constructor = args[1].clone();
    if !matches!(constructor, Value::Object(_)) {
        return Err(JsError::type_error(
            "customElements.define needs a constructor function",
        ));
    }

    // Validate through the DOM registry before mutating any host state.
    if let Err(error) = CustomElementRegistry::validate_name(&name) {
        return Err(JsError::new(error.to_string()));
    }

    let document = document_registry(state);
    let handle = {
        let mut document = document.borrow_mut();
        let registry = document.custom_elements_mut();
        if registry.is_defined(&name) {
            return Err(JsError::new(format!(
                "custom element '{name}' is already defined"
            )));
        }
        registry.allocate_handle()
    };
    {
        let mut document = document.borrow_mut();
        if let Err(error) = document.custom_elements_mut().define(&name, handle) {
            return Err(JsError::new(error.to_string()));
        }
    }
    state
        .borrow_mut()
        .custom_elements
        .callbacks
        .insert(handle, constructor);

    // Settle any promises queued by whenDefined before this definition.
    flush(runtime, state);
    Ok(Value::Undefined)
}

fn constructor_for(state: &Rc<RefCell<HostState>>, name: &str) -> Option<Value> {
    let document = document_registry(state);
    let handle = document.borrow().custom_elements().get(name)?.constructor;
    state
        .borrow()
        .custom_elements
        .callbacks
        .get(&handle)
        .cloned()
}

fn when_defined(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, name: &str) -> Value {
    if let Err(error) = CustomElementRegistry::validate_name(name) {
        return deferred::rejected(runtime, &error.to_string());
    }
    // A definition might have arrived from Rust since the last JS call.
    flush(runtime, state);

    let deferred = deferred::new_deferred(runtime);
    if let Some(constructor) = constructor_for(state, name) {
        deferred.fulfil(runtime, constructor);
    } else {
        state
            .borrow_mut()
            .custom_elements
            .pending
            .entry(name.to_owned())
            .or_default()
            .push(deferred.clone());
    }
    deferred.promise
}

/// Resolves pending `whenDefined` promises for every name that is now defined.
pub fn flush(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> usize {
    let defined = {
        let document = document_registry(state);
        let document = document.borrow();
        state
            .borrow()
            .custom_elements
            .pending
            .keys()
            .filter(|name| document.custom_elements().is_defined(name))
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut settled = 0;
    for name in defined {
        let waiters = state
            .borrow_mut()
            .custom_elements
            .pending
            .remove(&name)
            .unwrap_or_default();
        let constructor = constructor_for(state, &name).unwrap_or(Value::Undefined);
        for waiter in waiters {
            waiter.fulfil(runtime, constructor.clone());
            settled += 1;
        }
    }
    settled
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse_document;

    fn setup() -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document("<html><body></body></html>")));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    #[test]
    fn define_get_and_when_defined() {
        let (mut runtime, state) = setup();
        runtime
            .eval("function XCard() {} customElements.define('x-card', XCard);")
            .unwrap();
        assert!(matches!(
            runtime
                .eval("customElements.get('x-card') === XCard")
                .unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            runtime
                .eval("customElements.get('x-other') === undefined")
                .unwrap(),
            Value::Bool(true)
        ));
        assert_eq!(state.borrow().document.borrow().custom_elements().len(), 1);

        runtime
            .eval("var result; customElements.whenDefined('x-card').then(function (c) { result = c; });")
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert!(matches!(
            runtime.eval("result === XCard").unwrap(),
            Value::Bool(true)
        ));
    }

    #[test]
    fn when_defined_reacts_to_later_definition() {
        let (mut runtime, _state) = setup();
        runtime
            .eval(
                "var seen = null; \
                 customElements.whenDefined('x-later').then(function (c) { seen = c; }); \
                 function XLater() {} \
                 customElements.define('x-later', XLater);",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert!(matches!(
            runtime.eval("seen === XLater").unwrap(),
            Value::Bool(true)
        ));
    }

    #[test]
    fn invalid_names_and_duplicates_reject_or_throw() {
        let (mut runtime, _state) = setup();
        assert!(
            runtime
                .eval("customElements.define('plain', function(){})")
                .is_err()
        );
        let rejected = runtime
            .eval(
                "var err = null; \
                 customElements.whenDefined('plain').catch(function (e) { err = e.name || 'error'; }); \
                 'ok'",
            )
            .unwrap();
        assert_eq!(rejected.to_string_value(), "ok");
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.eval("err").unwrap().to_string_value(), "TypeError");

        runtime
            .eval("customElements.define('x-a', function(){});")
            .unwrap();
        assert!(
            runtime
                .eval("customElements.define('x-a', function(){})")
                .is_err()
        );
    }
}
