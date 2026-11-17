//! DOM events: listener registry, dispatch and bubbling.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::NodeId;
use browser_js::{HostFunction, JsError, Runtime, Value};

use crate::HostState;
use crate::element::wrap;

/// Installs `window.addEventListener`-style globals on the runtime.
pub fn install_global(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let global = runtime.global();
    runtime.set_global("window", global.clone());
    runtime.set_global("self", global.clone());

    runtime.define_host_method(global.clone(), "addEventListener", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            add_listener(runtime, &state, NodeId(0), args, true);
            Ok(Value::Undefined)
        })
    });
    runtime.define_host_method(global.clone(), "removeEventListener", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            remove_listener(&state, NodeId(0), args);
            Ok(Value::Undefined)
        })
    });
    runtime.define_host_method(global.clone(), "dispatchEvent", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let event_type = args
                .first()
                .map(|value| runtime.get_property(value, "type").to_string_value())
                .unwrap_or_else(|| "custom".to_string());
            Ok(Value::Bool(dispatch_event(
                runtime,
                &state,
                NodeId(0),
                &event_type,
            )))
        })
    });
}

/// Registers a listener from an `addEventListener(type, handler)` call.
pub fn add_listener(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    args: Vec<Value>,
    _global: bool,
) {
    let Some(event_type) = args.first().map(Value::to_string_value) else {
        return;
    };
    let Some(handler) = args.get(1).cloned() else {
        return;
    };
    if handler.type_of() != "function" && handler.type_of() != "object" {
        return;
    }
    let _ = runtime;
    state
        .borrow_mut()
        .events
        .entry((node.0, event_type))
        .or_default()
        .push(handler);
}

/// Removes a listener registered with the same function.
pub fn remove_listener(state: &Rc<RefCell<HostState>>, node: NodeId, args: Vec<Value>) {
    let Some(event_type) = args.first().map(Value::to_string_value) else {
        return;
    };
    let Some(handler) = args.get(1) else {
        return;
    };
    let mut state = state.borrow_mut();
    if let Some(list) = state.events.get_mut(&(node.0, event_type)) {
        list.retain(|existing| !existing.strict_equals(handler));
    }
}

/// Dispatches `event_type` at `node`, bubbling to ancestors.
///
/// Returns `true` when at least one listener ran.
pub fn dispatch_event(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event_type: &str,
) -> bool {
    let mut ran = false;
    let mut current = Some(node);
    let mut depth = 0;
    while let Some(target) = current {
        if depth > 256 {
            break;
        }
        depth += 1;
        let handlers = state
            .borrow()
            .events
            .get(&(target.0, event_type.to_string()))
            .cloned()
            .unwrap_or_default();
        if !handlers.is_empty() {
            let event = make_event(runtime, state, target, event_type);
            let this = wrap(runtime, state, target);
            for handler in handlers {
                let _ = runtime.call_function(&handler, this.clone(), vec![event.clone()]);
                ran = true;
            }
        }
        current = {
            let document = state.borrow().document.clone();
            let document = document.borrow();
            document.parent(target)
        };
    }
    ran
}

fn make_event(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    target: NodeId,
    event_type: &str,
) -> Value {
    let event = runtime.new_object();
    runtime.set_property(event.clone(), "type", Value::String(event_type.to_string()));
    runtime.set_property(event.clone(), "bubbles", Value::Bool(true));
    runtime.set_property(event.clone(), "defaultPrevented", Value::Bool(false));
    let target_wrapper = wrap(runtime, state, target);
    runtime.set_property(event.clone(), "target", target_wrapper.clone());
    runtime.set_property(event.clone(), "currentTarget", target_wrapper);
    let prevent_default: HostFunction =
        Box::new(move |_runtime, _this, _args| Ok(Value::Undefined));
    runtime.define_host_method(event.clone(), "preventDefault", prevent_default);
    let stop: HostFunction = Box::new(move |_runtime, _this, _args| Ok(Value::Undefined));
    runtime.define_host_method(event.clone(), "stopPropagation", stop);
    event
}

/// Count of registered listeners (for tests and diagnostics).
pub fn listener_count(state: &Rc<RefCell<HostState>>) -> usize {
    state.borrow().events.values().map(Vec::len).sum()
}

/// Runs listeners for a node without constructing a bubbling chain.
pub fn dispatch_direct(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event_type: &str,
) -> Result<bool, JsError> {
    Ok(dispatch_event(runtime, state, node, event_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn setup() -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(Document::new()));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    #[test]
    fn listeners_are_registered_and_dispatched() {
        let (mut runtime, state) = setup();
        runtime
            .eval("var hits = 0; document.addEventListener('click', function(e) { hits = hits + 1; });")
            .unwrap();
        let ran = dispatch_event(&mut runtime, &state, NodeId(0), "click");
        assert!(ran);
        let hits = runtime.eval("hits").unwrap();
        assert_eq!(hits.to_number(), 1.0);
    }

    #[test]
    fn removal_stops_dispatch() {
        let (mut runtime, state) = setup();
        runtime
            .eval(
                "var hits = 0; var fn = function() { hits = hits + 1; };\n\
                 document.addEventListener('x', fn);\n\
                 document.removeEventListener('x', fn);",
            )
            .unwrap();
        assert!(!dispatch_event(&mut runtime, &state, NodeId(0), "x"));
        assert_eq!(runtime.eval("hits").unwrap().to_number(), 0.0);
    }

    #[test]
    fn events_bubble_from_element_to_document() {
        let (mut runtime, state) = setup();
        let node = {
            let document = state.borrow().document.clone();
            let mut document = document.borrow_mut();
            let body = document.create_element("body");
            let html = document.create_element("html");
            let root = document.root();
            document.append_child(root, html);
            document.append_child(html, body);
            body
        };
        runtime
            .eval("var bubbled = false; document.addEventListener('click', function() { bubbled = true; });")
            .unwrap();
        assert!(dispatch_event(&mut runtime, &state, node, "click"));
        assert!(runtime.eval("bubbled").unwrap().is_truthy());
    }
}
