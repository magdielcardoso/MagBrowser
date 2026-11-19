//! Basic `AbortController` / `AbortSignal` support.
//!
//! Signals are stored on [`HostState`] so both `fetch()` and scripts can observe
//! the `aborted` flag. A signal is identified by a numeric id carried on its
//! JavaScript wrapper as `__abortId`.

use std::cell::RefCell;
use std::rc::Rc;

use browser_js::{JsError, Runtime, Value};

use crate::HostState;

/// Mutable state of a single abort signal.
#[derive(Default)]
pub struct AbortSignalState {
    /// Whether `abort()` has been called.
    pub aborted: bool,
    /// Callbacks registered through `addEventListener('abort', fn)` or `onabort`.
    pub listeners: Vec<Value>,
}

/// Reads the numeric signal id from a signal wrapper object.
pub fn signal_id(runtime: &mut Runtime, value: &Value) -> Option<u64> {
    match runtime.get_property(value, "__abortId") {
        Value::Number(number) if number >= 0.0 => Some(number as u64),
        _ => None,
    }
}

/// Installs the `AbortController`, `AbortSignal` and `DOMException` globals.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function("AbortController", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let object = match this {
                Value::Object(_) => this.clone(),
                _ => runtime.new_object(),
            };
            install_controller(runtime, &state, object.clone());
            Ok(object)
        })
    });

    runtime.define_global_function("AbortSignal", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let object = match this {
                Value::Object(_) => this.clone(),
                _ => runtime.new_object(),
            };
            install_signal(runtime, &state, object.clone());
            Ok(object)
        })
    });

    // `AbortSignal.abort()` produces an already-aborted signal.
    let ctor = runtime.get_global("AbortSignal");
    runtime.define_host_method(ctor.clone(), "abort", {
        let state = state.clone();
        Box::new(move |runtime, _this, _args| {
            let signal = runtime.new_object();
            install_signal(runtime, &state, signal.clone());
            if let Some(id) = signal_id(runtime, &signal) {
                state.borrow_mut().abort_signals.insert(
                    id,
                    AbortSignalState {
                        aborted: true,
                        listeners: Vec::new(),
                    },
                );
            }
            runtime.set_property(signal.clone(), "aborted", Value::Bool(true));
            Ok(signal)
        })
    });

    // A minimal DOMException shape used for `AbortError` rejections.
    runtime.define_global_function(
        "DOMException",
        Box::new(move |runtime, this, args| {
            let object = match this {
                Value::Object(_) => this.clone(),
                _ => runtime.new_object(),
            };
            let message = args
                .first()
                .map(|value| value.to_string_value())
                .unwrap_or_default();
            let name = args
                .get(1)
                .map(|value| value.to_string_value())
                .unwrap_or_else(|| "Error".to_owned());
            runtime.set_property(object.clone(), "message", Value::String(message));
            runtime.set_property(object.clone(), "name", Value::String(name));
            Ok(object)
        }),
    );
}

fn install_controller(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, controller: Value) {
    let signal = runtime.new_object();
    install_signal(runtime, state, signal.clone());
    runtime.set_property(controller.clone(), "signal", signal);

    runtime.define_host_method(controller, "abort", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let signal = runtime.get_property(&this, "signal");
            let Some(id) = signal_id(runtime, &signal) else {
                return Ok(Value::Undefined);
            };
            let listeners = {
                let mut state = state.borrow_mut();
                let Some(entry) = state.abort_signals.get_mut(&id) else {
                    return Ok(Value::Undefined);
                };
                if entry.aborted {
                    return Ok(Value::Undefined);
                }
                entry.aborted = true;
                std::mem::take(&mut entry.listeners)
            };
            runtime.set_property(signal.clone(), "aborted", Value::Bool(true));
            let event = runtime.new_object();
            runtime.set_property(event.clone(), "type", Value::String("abort".to_owned()));
            runtime.set_property(event.clone(), "target", signal.clone());
            for listener in listeners {
                let _ = runtime.call_function(&listener, Value::Undefined, vec![event.clone()]);
            }
            Ok(Value::Undefined)
        })
    });
}

fn install_signal(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, signal: Value) {
    let id = {
        let mut state = state.borrow_mut();
        let id = state.next_abort_id + 1;
        state.next_abort_id = id;
        state.abort_signals.insert(id, AbortSignalState::default());
        id
    };
    runtime.set_property(signal.clone(), "__abortId", Value::Number(id as f64));
    runtime.set_property(signal.clone(), "aborted", Value::Bool(false));

    runtime.define_host_method(signal.clone(), "addEventListener", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            if args
                .first()
                .is_some_and(|value| value.to_string_value() == "abort")
                && let Some(listener) = args.get(1).cloned()
                && let Some(id) = signal_id(runtime, &this)
            {
                let mut state = state.borrow_mut();
                if let Some(entry) = state.abort_signals.get_mut(&id)
                    && !entry.aborted
                {
                    entry.listeners.push(listener);
                }
            }
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(signal.clone(), "removeEventListener", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let is_abort = args
                .first()
                .is_some_and(|value| value.to_string_value() == "abort");
            if is_abort && let (Some(id), Some(listener)) = (signal_id(runtime, &this), args.get(1))
            {
                let mut state = state.borrow_mut();
                if let Some(entry) = state.abort_signals.get_mut(&id) {
                    entry.listeners.retain(|value| value != listener);
                }
            }
            Ok(Value::Undefined)
        })
    });

    // `signal.onabort = fn` is the other common way of registering a listener.
    runtime.define_accessor(
        signal,
        "onabort",
        Box::new(|_runtime, _this, _args| Ok(Value::Null)),
        {
            let state = state.clone();
            Box::new(move |runtime, this, mut args| {
                let Some(listener) = args.drain(..).next() else {
                    return Ok(Value::Undefined);
                };
                if let Some(id) = signal_id(runtime, &this) {
                    let mut state = state.borrow_mut();
                    if let Some(entry) = state.abort_signals.get_mut(&id)
                        && !entry.aborted
                    {
                        entry.listeners.push(listener);
                    }
                }
                Ok(Value::Undefined)
            })
        },
    );
}

/// Builds an `AbortError`-shaped rejection value.
pub fn abort_error(runtime: &mut Runtime) -> Value {
    let object = runtime.new_object();
    runtime.set_property(
        object.clone(),
        "name",
        Value::String("AbortError".to_owned()),
    );
    runtime.set_property(
        object.clone(),
        "message",
        Value::String("The operation was aborted".to_owned()),
    );
    object
}

/// Raised by host code when an abort signal is already aborted.
pub fn aborted_error() -> JsError {
    JsError::new("AbortError: The operation was aborted")
}
