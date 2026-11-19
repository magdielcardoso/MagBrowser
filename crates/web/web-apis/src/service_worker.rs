//! Service worker bindings: `navigator.serviceWorker` and the
//! `ServiceWorkerContainer` / `ServiceWorkerGlobalScope` globals.
//!
//! The registry itself lives in [`browser_event_loop`]. Because its
//! [`browser_event_loop::WorkerScriptHandler`] must be `Send + Sync` while a
//! JavaScript [`Runtime`] is not, the installed bridge records events in a
//! thread-safe outbox. [`run_events`] drains that outbox, evaluates each
//! worker's registered script source once and invokes its JavaScript listeners
//! through the runtime. This keeps script execution deterministic and on the
//! thread that owns the JS runtime.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use browser_event_loop::{Registration, ServiceWorkerEvent, WorkerEventKind, WorkerEventResult};
use browser_js::{JsError, Runtime, Value};

use crate::{HostState, deferred};

/// Installs the service worker globals and the script handler bridge.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    // Bridge: the Send + Sync handler records events; JS dispatch happens in
    // `run_events` on the runtime's thread.
    let outbox = state.borrow().service_worker_events.clone();
    state
        .borrow_mut()
        .service_workers
        .set_script_handler(Arc::new(move |event| {
            if let Ok(mut events) = outbox.lock() {
                events.push(event.clone());
            }
            match event.kind() {
                WorkerEventKind::Install => WorkerEventResult::install(true),
                WorkerEventKind::Activate => WorkerEventResult::activate(true),
                WorkerEventKind::Fetch => WorkerEventResult::fetch(None),
            }
        }));

    define_worker_scope(runtime, state);

    define_container_constructor(runtime, state);

    let navigator = match runtime.get_global("navigator") {
        Value::Object(_) => runtime.get_global("navigator"),
        _ => {
            let object = runtime.new_object();
            runtime.set_global("navigator", object.clone());
            object
        }
    };
    let instance = runtime.new_object();
    install_container_members(runtime, state, &instance, "/");
    runtime.set_property(instance.clone(), "scope", Value::String("/".to_owned()));
    runtime.set_property(navigator.clone(), "serviceWorker", instance.clone());
    runtime.set_global("serviceWorker", instance);
}

fn define_container_constructor(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function("ServiceWorkerContainer", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let scope = args
                .first()
                .map(Value::to_string_value)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "/".to_owned());
            let object = if matches!(this, Value::Object(_)) {
                this
            } else {
                runtime.new_object()
            };
            install_container_members(runtime, &state, &object, &scope);
            runtime.set_property(object.clone(), "scope", Value::String(scope));
            Ok(object)
        })
    });
}

fn default_scope(script_url: &str) -> String {
    script_url
        .rfind('/')
        .map_or_else(|| "/".to_owned(), |index| script_url[..=index].to_owned())
}

fn install_container_members(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    object: &Value,
    default: &str,
) {
    runtime.set_property(
        object.clone(),
        "__defaultScope",
        Value::String(default.to_owned()),
    );

    runtime.define_host_method(object.clone(), "register", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let script_url = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("register needs a script URL"))?;
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let scope = {
                let value = runtime.get_property(&options, "scope");
                match value {
                    Value::String(ref text) if !text.is_empty() => text.clone(),
                    _ => {
                        // Without an explicit scope, fall back to the
                        // container's scope or the script's directory.
                        let fallback = runtime
                            .get_property(&this, "__defaultScope")
                            .to_string_value();
                        if fallback.is_empty() || fallback == "/" {
                            default_scope(&script_url)
                        } else {
                            fallback
                        }
                    }
                }
            };
            let source = {
                let value = runtime.get_property(&options, "source");
                if matches!(value, Value::String(_)) {
                    Some(value.to_string_value())
                } else {
                    None
                }
            };

            let result = {
                let mut borrowed = state.borrow_mut();
                if let Some(source) = source {
                    borrowed
                        .service_worker_scripts
                        .insert(script_url.clone(), source);
                }
                borrowed
                    .service_workers
                    .register(scope.clone(), script_url.clone())
            };
            match result {
                Ok(_) => {
                    let registration = active_or_latest(runtime, &state, &scope);
                    Ok(deferred::resolved(runtime, registration))
                }
                Err(error) => Ok(deferred::rejected(runtime, &error.to_string())),
            }
        })
    });

    runtime.define_host_method(object.clone(), "unregister", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let scope = args
                .first()
                .map(Value::to_string_value)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    runtime
                        .get_property(&this, "__defaultScope")
                        .to_string_value()
                });
            let removed = state.borrow_mut().service_workers.unregister(&scope);
            Ok(deferred::resolved(runtime, Value::Bool(removed)))
        })
    });

    runtime.define_host_method(object.clone(), "getRegistration", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let scope = args
                .first()
                .map(Value::to_string_value)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "/".to_owned());
            let registration = state.borrow().service_workers.get(&scope).cloned();
            let value = match registration {
                Some(registration) => make_registration(runtime, &state, &registration),
                None => Value::Null,
            };
            Ok(deferred::resolved(runtime, value))
        })
    });

    runtime.define_accessor(
        object.clone(),
        "controller",
        {
            let state = state.clone();
            Box::new(move |runtime, _this, _args| {
                let active = {
                    let borrowed = state.borrow();
                    let url = borrowed.url.clone();
                    url.as_deref()
                        .and_then(|url| borrowed.service_workers.registration_for(url))
                        .or_else(|| {
                            borrowed
                                .service_workers
                                .registrations()
                                .iter()
                                .find(|registration| registration.is_active())
                        })
                        .cloned()
                };
                Ok(active.map_or(Value::Null, |registration| {
                    make_registration(runtime, &state, &registration)
                }))
            })
        },
        { Box::new(|_runtime, _this, _args| Err(JsError::type_error("controller is read-only"))) },
    );
}

fn active_or_latest(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, scope: &str) -> Value {
    let registration = state.borrow().service_workers.get(scope).cloned();
    registration.map_or(Value::Null, |registration| {
        make_registration(runtime, state, &registration)
    })
}

fn make_registration(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    registration: &Registration,
) -> Value {
    let object = runtime.new_object();
    runtime.set_property(
        object.clone(),
        "scope",
        Value::String(registration.scope.clone()),
    );
    runtime.set_property(
        object.clone(),
        "scriptURL",
        Value::String(registration.script_url.clone()),
    );
    runtime.set_property(
        object.clone(),
        "state",
        Value::String(registration.state.as_str().to_owned()),
    );
    runtime.set_property(
        object.clone(),
        "active",
        Value::Bool(registration.is_active()),
    );
    runtime.set_property(
        object.clone(),
        "installing",
        Value::Bool(!registration.is_active()),
    );
    let scope = registration.scope.clone();
    runtime.define_host_method(object.clone(), "unregister", {
        let state = state.clone();
        Box::new(move |runtime, _this, _args| {
            let removed = state.borrow_mut().service_workers.unregister(&scope);
            Ok(deferred::resolved(runtime, Value::Bool(removed)))
        })
    });
    object
}

fn define_worker_scope(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let scope = runtime.new_object();
    runtime.set_global("ServiceWorkerGlobalScope", scope.clone());

    runtime.define_host_method(scope.clone(), "addEventListener", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            if args.len() < 2 || !matches!(args[1], Value::Object(_)) {
                return Err(JsError::type_error(
                    "addEventListener needs a type and callback",
                ));
            }
            let kind = args[0].to_string_value();
            let callback = args[1].clone();
            state
                .borrow_mut()
                .service_worker_listeners
                .entry(kind)
                .or_default()
                .push(callback);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(scope.clone(), "removeEventListener", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            if args.len() < 2 {
                return Ok(Value::Undefined);
            }
            let kind = args[0].to_string_value();
            let callback = args[1].clone();
            if let Some(listeners) = state.borrow_mut().service_worker_listeners.get_mut(&kind) {
                listeners.retain(|listener| !listener.strict_equals(&callback));
            }
            Ok(Value::Undefined)
        })
    });
}

/// Drains recorded worker events, evaluates each worker script once and calls
/// the JavaScript listeners registered on `ServiceWorkerGlobalScope`.
pub fn run_events(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> usize {
    let events = match state.borrow().service_worker_events.lock() {
        Ok(mut events) => std::mem::take(&mut *events),
        Err(_) => Vec::new(),
    };
    let mut delivered = 0;
    for event in events {
        let script_url = match &event {
            ServiceWorkerEvent::Install { script_url, .. }
            | ServiceWorkerEvent::Activate { script_url, .. } => Some(script_url.clone()),
            ServiceWorkerEvent::Fetch { .. } => None,
        };
        if let Some(script_url) = script_url {
            let source = state
                .borrow()
                .service_worker_scripts
                .get(&script_url)
                .cloned();
            let fresh = state
                .borrow_mut()
                .service_worker_evaluated
                .insert(script_url.clone());
            if fresh
                && let Some(source) = source
                && runtime.eval(&source).is_err()
            {
                // A broken worker script simply contributes no listeners.
            }
        }

        let kind = event.kind().as_str().to_owned();
        let script_url = match &event {
            ServiceWorkerEvent::Install { script_url, .. }
            | ServiceWorkerEvent::Activate { script_url, .. } => script_url.clone(),
            ServiceWorkerEvent::Fetch { url, .. } => url.clone(),
        };
        let scope = event.scope().to_owned();
        let js_event = make_event(runtime, &kind, &scope, &script_url);
        let listeners = state
            .borrow()
            .service_worker_listeners
            .get(&kind)
            .cloned()
            .unwrap_or_default();
        let worker_scope = runtime.get_global("ServiceWorkerGlobalScope");
        for listener in listeners {
            let _ = runtime.call_function(&listener, worker_scope.clone(), vec![js_event.clone()]);
            delivered += 1;
        }
    }
    delivered
}

fn make_event(runtime: &mut Runtime, kind: &str, scope: &str, url: &str) -> Value {
    let object = runtime.new_object();
    runtime.set_property(object.clone(), "type", Value::String(kind.to_owned()));
    runtime.set_property(object.clone(), "scope", Value::String(scope.to_owned()));
    runtime.set_property(object.clone(), "scriptURL", Value::String(url.to_owned()));
    let request = runtime.new_object();
    runtime.set_property(request.clone(), "url", Value::String(url.to_owned()));
    runtime.set_property(object.clone(), "request", request);
    runtime.define_host_method(
        object.clone(),
        "respondWith",
        Box::new({
            let object = object.clone();
            move |runtime, _this, args| {
                let response = args.first().cloned().unwrap_or(Value::Undefined);
                runtime.set_property(object.clone(), "_response", response);
                Ok(Value::Undefined)
            }
        }),
    );
    object
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_event_loop::WorkerState;
    use browser_html_parser::parse_document;

    fn setup() -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document("<html><body></body></html>")));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    #[test]
    fn register_exposes_a_registration_and_controller() {
        let (mut runtime, state) = setup();
        runtime
            .eval(
                "var result; \
                 navigator.serviceWorker.register('/sw.js').then(function (r) { result = r; });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.eval("result.scope").unwrap().to_string_value(), "/");
        assert_eq!(
            runtime.eval("result.scriptURL").unwrap().to_string_value(),
            "/sw.js"
        );
        // A freshly registered worker is installed but not controlling yet.
        assert!(matches!(
            runtime
                .eval("navigator.serviceWorker.controller === null")
                .unwrap(),
            Value::Bool(true)
        ));
        state.borrow_mut().service_workers.activate("/").unwrap();
        assert!(matches!(
            runtime
                .eval("navigator.serviceWorker.controller.active")
                .unwrap(),
            Value::Bool(true)
        ));
        assert_eq!(state.borrow().service_workers.len(), 1);
    }

    #[test]
    fn unregister_removes_the_registration() {
        let (mut runtime, _state) = setup();
        runtime
            .eval(
                "var ok; \
                 navigator.serviceWorker.register('/app/sw.js', {scope: '/app/'}).then(function (r) { \
                     r.unregister().then(function (v) { ok = v; }); \
                 });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert!(matches!(runtime.eval("ok").unwrap(), Value::Bool(true)));
        assert!(matches!(
            runtime
                .eval("navigator.serviceWorker.controller === null")
                .unwrap(),
            Value::Bool(true)
        ));
    }

    #[test]
    fn script_handler_bridge_evaluates_worker_listeners() {
        let (mut runtime, state) = setup();
        runtime
            .eval(
                "var seen = []; \
                 ServiceWorkerGlobalScope.addEventListener('install', function (e) { \
                     seen.push('install:' + e.scope); \
                 }); \
                 navigator.serviceWorker.register('/sw.js', { \
                     scope: '/', \
                     source: \"ServiceWorkerGlobalScope.addEventListener('activate', function(e){ seen.push('activate:'+e.type); });\" \
                 });",
            )
            .unwrap();
        assert_eq!(crate::run_service_worker_events(&mut runtime, &state), 1);
        runtime.run_microtasks().unwrap();
        let seen = runtime.eval("seen.join(',')").unwrap().to_string_value();
        assert_eq!(seen, "install:/");
        assert_eq!(
            state.borrow().service_workers.get("/").unwrap().state,
            WorkerState::Installed
        );

        runtime.eval("navigator.serviceWorker.controller;").unwrap();
        // Activation dispatches another event through the bridge.
        state.borrow_mut().service_workers.activate("/").unwrap();
        assert_eq!(crate::run_service_worker_events(&mut runtime, &state), 1);
        runtime.run_microtasks().unwrap();
        let seen = runtime.eval("seen.join(',')").unwrap().to_string_value();
        assert_eq!(seen, "install:/,activate:activate");
    }
}
