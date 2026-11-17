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
            self.define_data_property(*id, "resolve", resolve);
            self.define_data_property(*id, "reject", reject);
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
    let (id, _state) = runtime.new_promise();
    runtime.resolve_promise(id, value);
    Ok(Value::Object(id))
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
