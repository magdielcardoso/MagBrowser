//! `setTimeout`/`setInterval` and the timer drain loop.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use browser_js::{JsError, Runtime, Value};

use crate::{HostState, Timer};

/// Installs timer globals on the runtime.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function("setTimeout", {
        let state = state.clone();
        Box::new(move |runtime, _this, mut args| {
            if args.is_empty() {
                return Err(JsError::type_error("setTimeout needs a callback"));
            }
            let callback = args.remove(0);
            let delay = args.first().map(|v| v.to_number().max(0.0)).unwrap_or(0.0);
            let extra: Vec<Value> = if args.len() > 1 {
                args[1..].to_vec()
            } else {
                Vec::new()
            };
            Ok(Value::Number(add_timer(
                runtime,
                &state,
                callback,
                Duration::from_millis(delay as u64),
                None,
                extra,
            )))
        })
    });

    runtime.define_global_function("setInterval", {
        let state = state.clone();
        Box::new(move |runtime, _this, mut args| {
            if args.is_empty() {
                return Err(JsError::type_error("setInterval needs a callback"));
            }
            let callback = args.remove(0);
            let delay = args.first().map(|v| v.to_number().max(1.0)).unwrap_or(1.0);
            let extra: Vec<Value> = if args.len() > 1 {
                args[1..].to_vec()
            } else {
                Vec::new()
            };
            Ok(Value::Number(add_timer(
                runtime,
                &state,
                callback,
                Duration::from_millis(delay as u64),
                Some(Duration::from_millis(delay.max(1.0) as u64)),
                extra,
            )))
        })
    });

    runtime.define_global_function("clearTimeout", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let id = args.first().map(|v| v.to_number() as u64).unwrap_or(0);
            state.borrow_mut().timers.retain(|timer| timer.id != id);
            Ok(Value::Undefined)
        })
    });

    runtime.define_global_function("clearInterval", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let id = args.first().map(|v| v.to_number() as u64).unwrap_or(0);
            state.borrow_mut().timers.retain(|timer| timer.id != id);
            Ok(Value::Undefined)
        })
    });

    runtime.define_global_function("requestAnimationFrame", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let callback = args
                .first()
                .cloned()
                .ok_or_else(|| JsError::type_error("requestAnimationFrame needs a callback"))?;
            if callback.type_of() != "function" {
                return Err(JsError::type_error("callback must be a function"));
            }
            Ok(Value::Number(add_timer(
                runtime,
                &state,
                callback,
                Duration::from_millis(16),
                None,
                Vec::new(),
            )))
        })
    });
}

fn add_timer(
    _runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    callback: Value,
    delay: Duration,
    period: Option<Duration>,
    args: Vec<Value>,
) -> f64 {
    let mut state = state.borrow_mut();
    let id = state.next_timer_id;
    state.next_timer_id += 1;
    let deadline = Instant::now() + delay;
    state.timers.push(Timer {
        id,
        deadline,
        period,
        callback,
        args,
    });
    id as f64
}

/// Runs every timer due at `now`.
pub fn drain_due(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, now: Instant) -> usize {
    let mut executed = 0;
    loop {
        let due: Option<(Value, Vec<Value>)> = {
            let mut state = state.borrow_mut();
            let index = state.timers.iter().position(|timer| timer.deadline <= now);
            match index {
                Some(index) => {
                    let period = state.timers[index].period;
                    if let Some(period) = period {
                        let timer = &mut state.timers[index];
                        timer.deadline = now + period;
                        Some((timer.callback.clone(), timer.args.clone()))
                    } else {
                        let timer = state.timers.remove(index);
                        Some((timer.callback, timer.args))
                    }
                }
                None => None,
            }
        };
        let Some((callback, args)) = due else { break };
        let _ = runtime.call_function(&callback, Value::Undefined, args);
        let _ = runtime.run_microtasks();
        executed += 1;
        if executed > 10_000 {
            break;
        }
    }
    executed
}

/// Number of pending timers.
pub fn pending(state: &Rc<RefCell<HostState>>) -> usize {
    state.borrow().timers.len()
}
