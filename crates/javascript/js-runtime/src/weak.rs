//! `WeakRef` and `FinalizationRegistry`.
//!
//! Weak references store only a heap slot id plus the slot's generation. The
//! collector never traces that id, so a `WeakRef` cannot keep its target alive.
//! Generations make stale references safe when a slot is recycled.
//!
//! Finalization registrations are likewise held weakly. During collection the
//! collector compares each registration's target with the mark set and queues
//! the cleanup callback as a microtask for targets that died.

use crate::Runtime;
use crate::errors::JsError;
use crate::object::{FinalizationRegistration, FinalizationRegistryState, ObjectKind};
use crate::value::Value;

/// Property holding the cleanup callback on a finalization registry object.
pub(crate) const FINALIZATION_CLEANUP: &str = "\u{0}finalization_cleanup";

impl Runtime {
    /// Install the weak-reference builtins.
    pub(crate) fn install_weak_refs(&mut self) {
        let weak_ref = self.new_builtin_value("WeakRef", 1, weak_ref_constructor);
        self.declare_global("WeakRef", weak_ref);

        let registry =
            self.new_builtin_value("FinalizationRegistry", 1, finalization_registry_constructor);
        self.declare_global("FinalizationRegistry", registry);
    }
}

fn weak_ref_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let Value::Object(target) = args.first().cloned().unwrap_or(Value::Undefined) else {
        return Err(JsError::type_error("WeakRef target must be an object"));
    };
    let generation = runtime.heap_generations.get(target.0).copied().unwrap_or(0);
    let id = runtime.alloc_object(
        ObjectKind::WeakRef { target, generation },
        Some(runtime.prototypes.object),
    );
    let weak_ref = Value::Object(id);

    let deref = runtime.new_native_value(
        "deref",
        0,
        Box::new(|runtime, this, _args| {
            Ok(weak_ref_deref(runtime, &this).unwrap_or(Value::Undefined))
        }),
    );
    runtime.define_data_property(id, "deref", deref);
    Ok(weak_ref)
}

fn finalization_registry_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let cleanup = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&cleanup) {
        return Err(JsError::type_error(
            "FinalizationRegistry cleanup must be callable",
        ));
    }
    let id = runtime.alloc_object(
        ObjectKind::FinalizationRegistry(std::rc::Rc::new(std::cell::RefCell::new(
            FinalizationRegistryState::default(),
        ))),
        Some(runtime.prototypes.object),
    );
    runtime.define_data_property(id, FINALIZATION_CLEANUP, cleanup);

    let register = runtime.new_native_value(
        "register",
        3,
        Box::new(move |runtime, _this, args| {
            let Value::Object(target) = args.first().cloned().unwrap_or(Value::Undefined) else {
                return Err(JsError::type_error(
                    "FinalizationRegistry target must be an object",
                ));
            };
            let held = args.get(1).cloned().unwrap_or(Value::Undefined);
            let token = args
                .get(2)
                .cloned()
                .filter(|value| !matches!(value, Value::Undefined));
            let generation = runtime.heap_generations.get(target.0).copied().unwrap_or(0);
            if let Some(object) = runtime.object_mut(id)
                && let ObjectKind::FinalizationRegistry(state) = &mut object.kind
            {
                state
                    .borrow_mut()
                    .registrations
                    .push(FinalizationRegistration {
                        target,
                        generation,
                        held,
                        token,
                    });
            }
            Ok(Value::Undefined)
        }),
    );
    let unregister = runtime.new_native_value(
        "unregister",
        1,
        Box::new(move |runtime, _this, args| {
            let token = args.first().cloned().unwrap_or(Value::Undefined);
            let Some(object) = runtime.object(id) else {
                return Ok(Value::Bool(false));
            };
            let ObjectKind::FinalizationRegistry(state) = &object.kind else {
                return Ok(Value::Bool(false));
            };
            let before = state.borrow().registrations.len();
            state.borrow_mut().registrations.retain(|registration| {
                registration
                    .token
                    .as_ref()
                    .is_none_or(|candidate| !candidate.strict_equals(&token))
            });
            Ok(Value::Bool(state.borrow().registrations.len() < before))
        }),
    );
    runtime.define_data_property(id, "register", register);
    runtime.define_data_property(id, "unregister", unregister);
    Ok(Value::Object(id))
}

/// Resolve a `WeakRef` value to its live target, if the referent is still alive.
pub(crate) fn weak_ref_deref(runtime: &Runtime, value: &Value) -> Option<Value> {
    let Value::Object(id) = value else {
        return None;
    };
    let ObjectKind::WeakRef { target, generation } =
        runtime.object(*id).map(|object| &object.kind)?
    else {
        return None;
    };
    let target = *target;
    let generation = *generation;
    if runtime.object(target).is_some()
        && runtime.heap_generations.get(target.0).copied() == Some(generation)
    {
        Some(Value::Object(target))
    } else {
        None
    }
}
