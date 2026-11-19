//! Small helper for creating a JavaScript `Promise` whose resolvers are kept on
//! the Rust side.
//!
//! The JS runtime's promise internals are crate-private, so host code builds a
//! deferred with a plain JavaScript expression and keeps the `resolve` /
//! `reject` functions as [`Value`]s. This mirrors the way timers and event
//! listeners already keep JS callables alive in [`crate::HostState`].

use browser_js::{Runtime, Value};

/// A JavaScript promise together with the functions that settle it.
#[derive(Clone, Debug)]
pub struct Deferred {
    /// The promise object handed back to scripts.
    pub promise: Value,
    /// Calls the promise's `resolve` function.
    pub resolve: Value,
    /// Calls the promise's `reject` function.
    pub reject: Value,
}

impl Deferred {
    /// Fulfils the promise with `value`.
    pub fn fulfil(&self, runtime: &mut Runtime, value: Value) {
        let _ = runtime.call_function(&self.resolve, Value::Undefined, vec![value]);
    }

    /// Rejects the promise with `reason`.
    pub fn reject_with(&self, runtime: &mut Runtime, reason: Value) {
        let _ = runtime.call_function(&self.reject, Value::Undefined, vec![reason]);
    }
}

/// Builds a fresh deferred promise.
///
/// The executor stores its two arguments on the promise object so they can be
/// read back from Rust. The object still behaves as an ordinary promise for any
/// script that calls `.then()` on it.
pub fn new_deferred(runtime: &mut Runtime) -> Deferred {
    let promise = runtime
        .eval(
            "(function () { \
                 var res, rej; \
                 var p = new Promise(function (resolve, reject) { res = resolve; rej = reject; }); \
                 p.__magResolve = res; \
                 p.__magReject = rej; \
                 return p; \
             })()",
        )
        .expect("deferred promise source is valid JavaScript");
    let resolve = runtime.get_property(&promise, "__magResolve");
    let reject = runtime.get_property(&promise, "__magReject");
    Deferred {
        promise,
        resolve,
        reject,
    }
}

/// Builds a rejected promise carrying a `TypeError`-like value.
pub fn rejected(runtime: &mut Runtime, message: &str) -> Value {
    let promise = runtime.get_global("Promise");
    let reject = runtime.get_property(&promise, "reject");
    let reason = type_error(runtime, message);
    runtime
        .call_function(&reject, Value::Undefined, vec![reason])
        .unwrap_or(Value::Undefined)
}

/// Builds a resolved promise.
pub fn resolved(runtime: &mut Runtime, value: Value) -> Value {
    let promise = runtime.get_global("Promise");
    let resolve = runtime.get_property(&promise, "resolve");
    runtime
        .call_function(&resolve, Value::Undefined, vec![value])
        .unwrap_or(Value::Undefined)
}

/// Creates an `Error`-like value for promise rejections.
pub fn type_error(runtime: &mut Runtime, message: &str) -> Value {
    let constructor = runtime.get_global("TypeError");
    if matches!(constructor, Value::Object(_))
        && let Ok(value) = runtime.call_function(
            &constructor,
            Value::Undefined,
            vec![Value::String(message.to_owned())],
        )
    {
        return value;
    }
    Value::String(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_resolves_and_runs_then() {
        let mut runtime = Runtime::new();
        runtime.eval("var seen = null;").unwrap();
        let deferred = new_deferred(&mut runtime);
        let then = runtime.get_property(&deferred.promise, "then");
        let handler = runtime
            .eval("(function (value) { seen = value; })")
            .unwrap();
        runtime
            .call_function(&then, deferred.promise.clone(), vec![handler])
            .unwrap();
        deferred.fulfil(&mut runtime, Value::Number(42.0));
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.get_global("seen"), Value::Number(42.0));
    }
}
