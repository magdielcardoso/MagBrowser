//! `localStorage` bindings.

use std::cell::RefCell;
use std::rc::Rc;

use browser_js::{JsError, Runtime, Value};

use crate::HostState;

/// Installs `localStorage` and `sessionStorage` as in-memory stores.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let storage = build_storage(runtime, state);
    runtime.set_global("localStorage", storage.clone());
    runtime.set_global("sessionStorage", storage);
}

fn build_storage(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> Value {
    let storage = runtime.new_object();

    runtime.define_host_method(storage.clone(), "getItem", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let key = args.first().map(Value::to_string_value).unwrap_or_default();
            let value = state.borrow().storage.get(&key).cloned();
            Ok(match value {
                Some(value) => Value::String(value),
                None => Value::Null,
            })
        })
    });

    runtime.define_host_method(storage.clone(), "setItem", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            if args.len() < 2 {
                return Err(JsError::type_error("setItem needs a key and a value"));
            }
            let key = args[0].to_string_value();
            let value = args[1].to_string_value();
            state.borrow_mut().storage.insert(key, value);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(storage.clone(), "removeItem", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let key = args.first().map(Value::to_string_value).unwrap_or_default();
            state.borrow_mut().storage.remove(&key);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(storage.clone(), "clear", {
        let state = state.clone();
        Box::new(move |_runtime, _this, _args| {
            state.borrow_mut().storage.clear();
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(storage.clone(), "key", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let index = args.first().map(|v| v.to_number() as usize).unwrap_or(0);
            let keys: Vec<String> = state.borrow().storage.keys().cloned().collect();
            Ok(match keys.get(index) {
                Some(key) => Value::String(key.clone()),
                None => Value::Null,
            })
        })
    });

    runtime.define_accessor(
        storage.clone(),
        "length",
        {
            let state = state.clone();
            Box::new(move |_runtime, _this, _args| {
                let length = state.borrow().storage.len() as f64;
                Ok(Value::Number(length))
            })
        },
        { Box::new(|_runtime, _this, _args| Err(JsError::type_error("length is read-only"))) },
    );

    storage
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;

    #[test]
    fn storage_roundtrip() {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(Document::new()));
        let _state = crate::install(&mut runtime, document, None);
        runtime
            .eval("localStorage.setItem('a', '1'); localStorage.setItem('b', '2');")
            .unwrap();
        assert_eq!(
            runtime.eval("localStorage.getItem('a')").unwrap(),
            Value::String("1".to_string())
        );
        assert_eq!(
            runtime.eval("localStorage.length").unwrap(),
            Value::Number(2.0)
        );
        runtime.eval("localStorage.removeItem('a')").unwrap();
        assert_eq!(
            runtime.eval("localStorage.length").unwrap(),
            Value::Number(1.0)
        );
        assert_eq!(
            runtime.eval("localStorage.getItem('zzz')").unwrap(),
            Value::Null
        );
    }
}
