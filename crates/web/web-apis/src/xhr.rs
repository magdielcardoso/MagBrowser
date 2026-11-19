//! `XMLHttpRequest`, with synchronous and microtask-deferred async completion.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use browser_js::{JsError, Runtime, Value};

use crate::HostState;
use crate::fetch::{FetchRequest, perform_fetch, resolve_url};

#[derive(Default)]
struct XhrState {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    response_headers: Vec<(String, String)>,
    is_async: bool,
    ready_state: u16,
}

/// Installs the `XMLHttpRequest` global.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let store: Rc<RefCell<HashMap<usize, XhrState>>> = Rc::new(RefCell::new(HashMap::new()));

    runtime.define_global_function("XMLHttpRequest", {
        let store = store.clone();
        Box::new(move |runtime, this, _args| {
            let object = match this {
                Value::Object(_) => this.clone(),
                _ => runtime.new_object(),
            };
            if let Value::Object(id) = &object {
                store.borrow_mut().insert(id.0, XhrState::default());
            }
            for (name, value) in [
                ("readyState", Value::Number(0.0)),
                ("status", Value::Number(0.0)),
                ("statusText", Value::String(String::new())),
                ("responseText", Value::String(String::new())),
                ("response", Value::String(String::new())),
            ] {
                runtime.set_property(object.clone(), name, value);
            }
            Ok(object)
        })
    });

    let constructor = runtime.get_global("XMLHttpRequest");
    let prototype = runtime.new_object();
    runtime.set_property(constructor.clone(), "prototype", prototype.clone());
    for (name, value) in [
        ("UNSENT", 0),
        ("OPENED", 1),
        ("HEADERS_RECEIVED", 2),
        ("LOADING", 3),
        ("DONE", 4),
    ] {
        runtime.set_property(constructor.clone(), name, Value::Number(value as f64));
    }

    runtime.define_host_method(prototype.clone(), "open", {
        let state = state.clone();
        let store = store.clone();
        Box::new(move |runtime, this, args| {
            let id = object_id(&this)?;
            let method = args
                .first()
                .map_or_else(|| "GET".to_owned(), |value| value.to_string_value())
                .to_ascii_uppercase();
            let raw_url = args
                .get(1)
                .map_or_else(String::new, |value| to_string(runtime, value));
            let is_async = args.get(2).is_none_or(|value| value.is_truthy());
            let url = {
                let borrowed = state.borrow();
                resolve_url(&borrowed, &raw_url).map_err(JsError::type_error)?
            };
            {
                let mut store = store.borrow_mut();
                let xhr = store
                    .get_mut(&id)
                    .ok_or_else(|| JsError::type_error("bad XHR"))?;
                xhr.method = method;
                xhr.url = url;
                xhr.is_async = is_async;
                xhr.headers.clear();
                xhr.response_headers.clear();
                xhr.ready_state = 1;
            }
            runtime.set_property(this.clone(), "readyState", Value::Number(1.0));
            runtime.set_property(this.clone(), "status", Value::Number(0.0));
            runtime.set_property(this.clone(), "statusText", Value::String(String::new()));
            runtime.set_property(this.clone(), "responseText", Value::String(String::new()));
            fire_event(runtime, &this, "readystatechange");
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(prototype.clone(), "setRequestHeader", {
        let store = store.clone();
        Box::new(move |_runtime, this, args| {
            let id = object_id(&this)?;
            if args.len() < 2 {
                return Err(JsError::type_error(
                    "setRequestHeader needs a name and value",
                ));
            }
            let mut store = store.borrow_mut();
            let xhr = store
                .get_mut(&id)
                .ok_or_else(|| JsError::type_error("bad XHR"))?;
            if xhr.ready_state != 1 {
                return Err(JsError::new(
                    "InvalidStateError: setRequestHeader requires OPENED",
                ));
            }
            xhr.headers
                .push((args[0].to_string_value(), args[1].to_string_value()));
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(prototype.clone(), "send", {
        let state = state.clone();
        let store = store.clone();
        Box::new(move |runtime, this, args| {
            let id = object_id(&this)?;
            let (request, is_async) = {
                let store = store.borrow();
                let xhr = store
                    .get(&id)
                    .ok_or_else(|| JsError::type_error("bad XHR"))?;
                if xhr.ready_state != 1 {
                    return Err(JsError::new("InvalidStateError: send requires OPENED"));
                }
                let body = args
                    .first()
                    .map_or_else(Vec::new, |value| to_string(runtime, value).into_bytes());
                (
                    FetchRequest {
                        url: xhr.url.clone(),
                        method: xhr.method.clone(),
                        headers: xhr.headers.clone(),
                        body,
                        signal: None,
                    },
                    xhr.is_async,
                )
            };

            let result = {
                let borrowed = state.borrow();
                perform_fetch(&borrowed, request)
            };
            let (status, status_text, text, headers, succeeded) = match result {
                Ok(response) => (
                    response.status,
                    response.status_text,
                    String::from_utf8_lossy(&response.body).into_owned(),
                    response.headers,
                    response.ok,
                ),
                Err(_) => (0, String::new(), String::new(), Vec::new(), false),
            };
            if let Ok(mut store) = store.try_borrow_mut()
                && let Some(xhr) = store.get_mut(&id)
            {
                xhr.response_headers = headers;
            }

            let instance = this.clone();
            let apply = move |runtime: &mut Runtime| {
                runtime.set_property(instance.clone(), "readyState", Value::Number(4.0));
                runtime.set_property(instance.clone(), "status", Value::Number(status as f64));
                runtime.set_property(
                    instance.clone(),
                    "statusText",
                    Value::String(status_text.clone()),
                );
                runtime.set_property(
                    instance.clone(),
                    "responseText",
                    Value::String(text.clone()),
                );
                runtime.set_property(instance.clone(), "response", Value::String(text.clone()));
                fire_event(runtime, &instance, "readystatechange");
                fire_event(runtime, &instance, if succeeded { "load" } else { "error" });
                fire_event(runtime, &instance, "loadend");
            };
            if is_async {
                runtime.enqueue_microtask(Box::new(move |runtime, _this, _args| {
                    apply(runtime);
                    Ok(Value::Undefined)
                }));
            } else {
                apply(runtime);
            }
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(prototype.clone(), "getAllResponseHeaders", {
        let store = store.clone();
        Box::new(move |_runtime, this, _args| {
            let id = object_id(&this)?;
            let text = store
                .borrow()
                .get(&id)
                .map(|xhr| {
                    xhr.response_headers
                        .iter()
                        .map(|(name, value)| format!("{name}: {value}\r\n"))
                        .collect::<String>()
                })
                .unwrap_or_default();
            Ok(Value::String(text))
        })
    });

    runtime.define_host_method(prototype.clone(), "getResponseHeader", {
        let store = store.clone();
        Box::new(move |_runtime, this, args| {
            let id = object_id(&this)?;
            let name = args
                .first()
                .map_or(String::new(), |value| value.to_string_value());
            let value = store.borrow().get(&id).and_then(|xhr| {
                xhr.response_headers
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(&name))
                    .map(|(_, value)| value.clone())
            });
            Ok(value.map_or(Value::Null, Value::String))
        })
    });

    runtime.define_host_method(prototype.clone(), "abort", {
        let store = store.clone();
        Box::new(move |runtime, this, _args| {
            let id = object_id(&this)?;
            if let Ok(mut store) = store.try_borrow_mut()
                && let Some(xhr) = store.get_mut(&id)
            {
                xhr.ready_state = 4;
            }
            runtime.set_property(this.clone(), "readyState", Value::Number(4.0));
            runtime.set_property(this.clone(), "status", Value::Number(0.0));
            fire_event(runtime, &this, "abort");
            fire_event(runtime, &this, "loadend");
            Ok(Value::Undefined)
        })
    });

    runtime.set_property(prototype, "constructor", constructor);
}

fn object_id(this: &Value) -> Result<usize, JsError> {
    match this {
        Value::Object(id) => Ok(id.0),
        _ => Err(JsError::type_error(
            "XMLHttpRequest method on invalid receiver",
        )),
    }
}

fn fire_event(runtime: &mut Runtime, instance: &Value, event_type: &str) {
    let handler = runtime.get_property(instance, &format!("on{event_type}"));
    if matches!(handler, Value::Object(_)) {
        let event = runtime.new_object();
        runtime.set_property(event.clone(), "type", Value::String(event_type.to_owned()));
        runtime.set_property(event.clone(), "target", instance.clone());
        let _ = runtime.call_function(&handler, instance.clone(), vec![event]);
    }
}

fn to_string(runtime: &mut Runtime, value: &Value) -> String {
    if let Value::Object(_) = value {
        let method = runtime.get_property(value, "toString");
        if matches!(method, Value::Object(_))
            && let Ok(result) = runtime.call_function(&method, value.clone(), Vec::new())
        {
            return result.to_string_value();
        }
    }
    value.to_string_value()
}
