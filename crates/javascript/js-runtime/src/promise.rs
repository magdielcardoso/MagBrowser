//! Promises and the microtask queue.

use std::cell::RefCell;
use std::rc::Rc;

use crate::errors::JsError;
use crate::object::{HostFunction, ObjectKind};
use crate::value::{ObjectId, Value};

/// Settlement state of a promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromiseStatus {
    Pending,
    Fulfilled,
    Rejected,
}

/// A `then` reaction waiting for settlement.
pub(crate) struct Reaction {
    pub(crate) on_fulfilled: Option<Value>,
    pub(crate) on_rejected: Option<Value>,
    pub(crate) next: ObjectId,
}

/// Mutable state shared by a promise object and its reactions.
pub(crate) struct PromiseState {
    pub(crate) status: PromiseStatus,
    pub(crate) result: Value,
    pub(crate) reactions: Vec<Reaction>,
}

impl PromiseState {
    fn new() -> Self {
        Self {
            status: PromiseStatus::Pending,
            result: Value::Undefined,
            reactions: Vec::new(),
        }
    }
}

impl crate::Runtime {
    pub(crate) fn new_promise(&mut self) -> (ObjectId, Rc<RefCell<PromiseState>>) {
        let state = Rc::new(RefCell::new(PromiseState::new()));
        let prototype = self.prototypes.promise;
        let id = self.alloc_object(ObjectKind::Promise(Rc::clone(&state)), Some(prototype));
        (id, state)
    }

    pub(crate) fn promise_state(&self, id: ObjectId) -> Option<Rc<RefCell<PromiseState>>> {
        let object = self.object(id)?;
        match &object.kind {
            ObjectKind::Promise(state) => Some(Rc::clone(state)),
            _ => None,
        }
    }

    pub(crate) fn settle_promise(
        &mut self,
        state: &Rc<RefCell<PromiseState>>,
        status: PromiseStatus,
        value: Value,
    ) {
        let reactions = {
            let mut borrowed = state.borrow_mut();
            if borrowed.status != PromiseStatus::Pending {
                return;
            }
            borrowed.status = status;
            borrowed.result = value;
            std::mem::take(&mut borrowed.reactions)
        };
        for reaction in reactions {
            self.schedule_reaction(Rc::clone(state), reaction);
        }
    }

    pub(crate) fn resolve_promise(&mut self, id: ObjectId, value: Value) {
        if let Some(state) = self.promise_state(id) {
            self.settle_promise(&state, PromiseStatus::Fulfilled, value);
        }
    }

    pub(crate) fn reject_promise(&mut self, id: ObjectId, value: Value) {
        if let Some(state) = self.promise_state(id) {
            self.settle_promise(&state, PromiseStatus::Rejected, value);
        }
    }

    /// The `Promise.resolve` assimilation step: promises pass through, other
    /// values are wrapped in a new fulfilled promise.
    #[allow(clippy::wrong_self_convention)] // wrapping a non-promise allocates a new promise
    pub(crate) fn to_promise_value(&mut self, value: Value) -> Value {
        if let Value::Object(id) = &value
            && self.promise_state(*id).is_some()
        {
            return value;
        }
        let (id, _state) = self.new_promise();
        self.resolve_promise(id, value);
        Value::Object(id)
    }

    fn schedule_reaction(&mut self, state: Rc<RefCell<PromiseState>>, reaction: Reaction) {
        self.microtasks
            .push_back(Box::new(move |runtime: &mut crate::Runtime| {
                let (status, value) = {
                    let borrowed = state.borrow();
                    (borrowed.status, borrowed.result.clone())
                };
                if status == PromiseStatus::Pending {
                    return Ok(());
                }
                let handler = match status {
                    PromiseStatus::Fulfilled => reaction.on_fulfilled,
                    PromiseStatus::Rejected => reaction.on_rejected,
                    PromiseStatus::Pending => None,
                };
                match handler {
                    Some(handler) if runtime.is_callable(&handler) => {
                        match runtime.call_function(&handler, Value::Undefined, vec![value]) {
                            Ok(result) => {
                                runtime.resolve_promise(reaction.next, result);
                            }
                            Err(error) => {
                                runtime.reject_promise(reaction.next, error.thrown_value());
                            }
                        }
                        Ok(())
                    }
                    _ => {
                        if status == PromiseStatus::Fulfilled {
                            runtime.resolve_promise(reaction.next, value);
                        } else {
                            runtime.reject_promise(reaction.next, value);
                        }
                        Ok(())
                    }
                }
            }));
    }

    /// Queue a host function as a microtask.
    pub fn enqueue_microtask(&mut self, f: HostFunction) {
        let mut f = f;
        self.microtasks
            .push_back(Box::new(move |runtime: &mut crate::Runtime| {
                f(runtime, Value::Undefined, Vec::new()).map(|_| ())
            }));
    }

    /// Run queued microtasks in FIFO order.
    pub fn run_microtasks(&mut self) -> Result<(), JsError> {
        self.in_microtask = true;
        let mut outcome = Ok(());
        while let Some(microtask) = self.microtasks.pop_front() {
            if let Err(error) = microtask(self) {
                outcome = Err(error);
                break;
            }
        }
        self.in_microtask = false;
        outcome
    }

    /// Install `Promise` and its prototype methods.
    pub(crate) fn install_promise(&mut self) {
        let prototype = self.prototypes.promise;
        self.define_builtin_method(prototype, "then", 2, promise_then);
        self.define_builtin_method(prototype, "catch", 1, promise_catch);
        self.define_builtin_method(prototype, "toString", 0, promise_to_string);

        let constructor = self.new_builtin_value("Promise", 1, promise_constructor);
        self.define_data_property(prototype, "constructor", constructor.clone());
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
            let resolve = self.new_builtin_value("resolve", 1, promise_resolve);
            let reject = self.new_builtin_value("reject", 1, promise_reject);
            let all = self.new_builtin_value("all", 1, promise_all);
            let all_settled = self.new_builtin_value("allSettled", 1, promise_all_settled);
            let any = self.new_builtin_value("any", 1, promise_any);
            let race = self.new_builtin_value("race", 1, promise_race);
            self.define_data_property(*id, "resolve", resolve);
            self.define_data_property(*id, "reject", reject);
            self.define_data_property(*id, "all", all);
            self.define_data_property(*id, "allSettled", all_settled);
            self.define_data_property(*id, "any", any);
            self.define_data_property(*id, "race", race);
        }
        self.declare_global("Promise", constructor);
    }
}

fn promise_constructor(
    runtime: &mut crate::Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let executor = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&executor) {
        return Err(JsError::type_error("Promise executor is not a function"));
    }
    let (id, _state) = runtime.new_promise();
    let promise = Value::Object(id);
    let root_mark = runtime.temp_roots.len();
    runtime.temp_roots.push(promise.clone());

    let resolve = runtime.new_native_value(
        "resolve",
        1,
        Box::new(move |runtime, _this, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            runtime.resolve_promise(id, value);
            Ok(Value::Undefined)
        }),
    );
    let reject = runtime.new_native_value(
        "reject",
        1,
        Box::new(move |runtime, _this, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            runtime.reject_promise(id, value);
            Ok(Value::Undefined)
        }),
    );

    let result = runtime.call_function(&executor, Value::Undefined, vec![resolve, reject]);
    if let Err(error) = result {
        runtime.reject_promise(id, error.thrown_value());
    }
    runtime.temp_roots.truncate(root_mark);
    Ok(promise)
}

fn promise_then(
    runtime: &mut crate::Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    then_impl(runtime, this, args.first().cloned(), args.get(1).cloned())
}

fn promise_catch(
    runtime: &mut crate::Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    then_impl(runtime, this, None, args.first().cloned())
}

fn then_impl(
    runtime: &mut crate::Runtime,
    this: Value,
    on_fulfilled: Option<Value>,
    on_rejected: Option<Value>,
) -> Result<Value, JsError> {
    let Value::Object(id) = this else {
        return Err(JsError::type_error("Promise.prototype.then on non-promise"));
    };
    let Some(state) = runtime.promise_state(id) else {
        return Err(JsError::type_error("Promise.prototype.then on non-promise"));
    };
    let (next, _next_state) = runtime.new_promise();
    let reaction = Reaction {
        on_fulfilled,
        on_rejected,
        next,
    };
    let settled = {
        let borrowed = state.borrow();
        borrowed.status != PromiseStatus::Pending
    };
    if settled {
        runtime.schedule_reaction(state, reaction);
    } else {
        state.borrow_mut().reactions.push(reaction);
    }
    Ok(Value::Object(next))
}

fn promise_to_string(
    _runtime: &mut crate::Runtime,
    _this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::String("[object Promise]".to_owned()))
}

fn promise_resolve(
    runtime: &mut crate::Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(runtime.to_promise_value(value))
}

#[derive(Clone, Copy)]
enum Combinator {
    All,
    AllSettled,
    Any,
    Race,
}

fn promise_all(
    runtime: &mut crate::Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    promise_combine(runtime, this, args, Combinator::All)
}

fn promise_all_settled(
    runtime: &mut crate::Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    promise_combine(runtime, this, args, Combinator::AllSettled)
}

fn promise_any(
    runtime: &mut crate::Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    promise_combine(runtime, this, args, Combinator::Any)
}

fn promise_race(
    runtime: &mut crate::Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    promise_combine(runtime, this, args, Combinator::Race)
}

fn promise_combine(
    runtime: &mut crate::Runtime,
    _this: Value,
    args: Vec<Value>,
    kind: Combinator,
) -> Result<Value, JsError> {
    let iterable = args.first().cloned().unwrap_or(Value::Undefined);
    let Some(items) = runtime.to_array_like(&iterable) else {
        let (id, _state) = runtime.new_promise();
        runtime.reject_promise(
            id,
            JsError::type_error("Promise combinator requires an iterable").thrown_value(),
        );
        return Ok(Value::Object(id));
    };

    let (id, _state) = runtime.new_promise();
    let result = Value::Object(id);
    let results = match kind {
        Combinator::All | Combinator::AllSettled => {
            let array = runtime.new_array(vec![Value::Undefined; items.len()]);
            // Native handlers are invisible to the GC, so keep shared state on
            // the returned promise, which the script normally keeps alive.
            runtime.define_data_property(id, "\u{0}promise_results", array.clone());
            array
        }
        Combinator::Any | Combinator::Race => Value::Undefined,
    };
    let errors = match kind {
        Combinator::Any => {
            let array = runtime.new_array(vec![Value::Undefined; items.len()]);
            runtime.define_data_property(id, "\u{0}promise_errors", array.clone());
            array
        }
        _ => Value::Undefined,
    };

    if items.is_empty() {
        match kind {
            Combinator::All | Combinator::AllSettled => {
                runtime.resolve_promise(id, results);
            }
            Combinator::Any | Combinator::Race => {
                runtime.reject_promise(id, JsError::new("no promises to settle").thrown_value());
            }
        }
        return Ok(result);
    }

    let remaining = Rc::new(RefCell::new(items.len()));
    let mark = runtime.temp_roots.len();
    for (index, item) in items.into_iter().enumerate() {
        let promise = runtime.to_promise_value(item);
        runtime.temp_roots.push(promise.clone());

        let fulfilled_results = results.clone();
        let fulfilled_remaining = Rc::clone(&remaining);
        let on_fulfilled = runtime.new_native_value(
            "promiseFulfilled",
            1,
            Box::new(move |rt, _this, args| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                match kind {
                    Combinator::All => {
                        rt.set_property(fulfilled_results.clone(), &index.to_string(), value);
                    }
                    Combinator::AllSettled => {
                        let outcome = rt.new_object();
                        rt.set_property(
                            outcome.clone(),
                            "status",
                            Value::String("fulfilled".to_owned()),
                        );
                        rt.set_property(outcome.clone(), "value", value);
                        rt.set_property(fulfilled_results.clone(), &index.to_string(), outcome);
                    }
                    Combinator::Any | Combinator::Race => {
                        rt.resolve_promise(id, value);
                        return Ok(Value::Undefined);
                    }
                }
                let mut remaining = fulfilled_remaining.borrow_mut();
                *remaining -= 1;
                if *remaining == 0 {
                    rt.resolve_promise(id, fulfilled_results.clone());
                }
                Ok(Value::Undefined)
            }),
        );
        runtime.temp_roots.push(on_fulfilled.clone());

        let rejected_results = results.clone();
        let rejected_errors = errors.clone();
        let rejected_remaining = Rc::clone(&remaining);
        let on_rejected = runtime.new_native_value(
            "promiseRejected",
            1,
            Box::new(move |rt, _this, args| {
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                match kind {
                    Combinator::All | Combinator::Race => {
                        rt.reject_promise(id, reason);
                        return Ok(Value::Undefined);
                    }
                    Combinator::AllSettled => {
                        let outcome = rt.new_object();
                        rt.set_property(
                            outcome.clone(),
                            "status",
                            Value::String("rejected".to_owned()),
                        );
                        rt.set_property(outcome.clone(), "reason", reason);
                        rt.set_property(rejected_results.clone(), &index.to_string(), outcome);
                    }
                    Combinator::Any => {
                        rt.set_property(rejected_errors.clone(), &index.to_string(), reason);
                    }
                }
                let mut remaining = rejected_remaining.borrow_mut();
                *remaining -= 1;
                if *remaining == 0 {
                    match kind {
                        Combinator::AllSettled => {
                            rt.resolve_promise(id, rejected_results.clone());
                        }
                        Combinator::Any => {
                            rt.reject_promise(
                                id,
                                JsError::new("all promises rejected").thrown_value(),
                            );
                        }
                        Combinator::All | Combinator::Race => {}
                    }
                }
                Ok(Value::Undefined)
            }),
        );
        runtime.temp_roots.push(on_rejected.clone());

        let _ = then_impl(runtime, promise, Some(on_fulfilled), Some(on_rejected))?;
    }
    runtime.temp_roots.truncate(mark);
    Ok(result)
}

fn promise_reject(
    runtime: &mut crate::Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let (id, _state) = runtime.new_promise();
    runtime.reject_promise(id, value);
    Ok(Value::Object(id))
}

#[cfg(test)]
mod tests {
    use crate::value::Value;

    #[test]
    fn static_combinators_are_installed_and_settle() {
        let mut runtime = crate::Runtime::new();
        runtime
            .eval(
                "var out = {}; \
                 Promise.all([Promise.resolve(1), 2]).then(function (values) { out.all = values.join(','); }); \
                 Promise.allSettled([Promise.reject('bad'), 3]).then(function (values) { \
                     out.settled = values.map(function (entry) { return entry.status; }).join(','); \
                 }); \
                 Promise.any([Promise.reject('x'), Promise.resolve('ok')]).then(function (value) { out.any = value; }); \
                 Promise.race([Promise.resolve('fast')]).then(function (value) { out.race = value; });",
            )
            .expect("eval");
        for _ in 0..8 {
            runtime.run_microtasks().expect("microtasks");
        }
        assert_eq!(
            runtime.eval("out.all").expect("eval"),
            Value::String("1,2".to_owned())
        );
        assert_eq!(
            runtime.eval("out.settled").expect("eval"),
            Value::String("rejected,fulfilled".to_owned())
        );
        assert_eq!(
            runtime.eval("out.any").expect("eval"),
            Value::String("ok".to_owned())
        );
        assert_eq!(
            runtime.eval("out.race").expect("eval"),
            Value::String("fast".to_owned())
        );
    }

    #[test]
    fn then_handlers_run_in_registration_order() {
        let mut runtime = crate::Runtime::new();
        runtime
            .eval(
                "var order = []; \
                 var p = new Promise(function (resolve) { resolve(1); }); \
                 p.then(function (value) { order.push('a' + value); }); \
                 p.then(function (value) { order.push('b' + value); });",
            )
            .expect("eval");
        runtime.run_microtasks().expect("microtasks");
        assert_eq!(
            runtime.eval("order.join(',')").expect("eval"),
            Value::String("a1,b1".to_owned())
        );
    }

    #[test]
    fn chains_propagate_values() {
        let mut runtime = crate::Runtime::new();
        runtime
            .eval(
                "var result = []; \
                 new Promise(function (resolve) { resolve(2); }) \
                   .then(function (value) { return value * 3; }) \
                   .then(function (value) { result.push(value); });",
            )
            .expect("eval");
        runtime.run_microtasks().expect("microtasks");
        assert_eq!(runtime.eval("result[0]").expect("eval"), Value::Number(6.0));
    }

    #[test]
    fn rejection_is_caught() {
        let mut runtime = crate::Runtime::new();
        runtime
            .eval(
                "var seen = []; \
                 new Promise(function (_resolve, reject) { reject('nope'); }) \
                   .catch(function (error) { seen.push(error); });",
            )
            .expect("eval");
        runtime.run_microtasks().expect("microtasks");
        assert_eq!(
            runtime.eval("seen[0]").expect("eval"),
            Value::String("nope".to_owned())
        );
    }

    #[test]
    fn throwing_handler_rejects_next_promise() {
        let mut runtime = crate::Runtime::new();
        runtime
            .eval(
                "var caught = []; \
                 new Promise(function (resolve) { resolve(1); }) \
                   .then(function () { throw new Error('bad'); }) \
                   .catch(function (error) { caught.push(error.message); });",
            )
            .expect("eval");
        runtime.run_microtasks().expect("microtasks");
        assert_eq!(
            runtime.eval("caught[0]").expect("eval"),
            Value::String("bad".to_owned())
        );
    }

    #[test]
    fn then_after_settlement_still_schedules() {
        let mut runtime = crate::Runtime::new();
        runtime
            .eval(
                "var log = []; \
                 var p = Promise.resolve(9); \
                 p.then(function (value) { log.push(value); });",
            )
            .expect("eval");
        runtime.run_microtasks().expect("microtasks");
        assert_eq!(runtime.eval("log[0]").expect("eval"), Value::Number(9.0));
    }
}
