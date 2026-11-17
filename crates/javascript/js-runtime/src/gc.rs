//! Mark and sweep garbage collector over the object and scope arenas.

use crate::errors::JsError;
use crate::function::FunctionImpl;
use crate::object::{Object, ObjectKind, PropertyData};
use crate::promise::{PromiseState, PromiseStatus};
use crate::scope::{Scope, ScopeId};
use crate::value::{ObjectId, Value};

impl crate::Runtime {
    /// Count a statement or loop iteration against the execution budget.
    pub(crate) fn tick(&mut self) -> Result<(), JsError> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return Err(JsError::range_error("execution step limit exceeded"));
        }
        Ok(())
    }

    /// Run a collection when enough objects have been allocated.
    pub(crate) fn maybe_gc(&mut self) {
        if self.allocations_since_gc >= self.gc_threshold
            && !self.in_microtask
            && self.microtasks.is_empty()
        {
            self.collect_garbage();
        }
    }

    /// Number of live heap objects.
    pub fn heap_objects(&self) -> usize {
        self.heap.iter().filter(|slot| slot.is_some()).count()
    }

    /// Add a value to the root set held by the host.
    pub fn add_gc_root(&mut self, value: Value) {
        self.gc_roots.push(value);
    }

    /// Remove every host supplied root.
    pub fn clear_gc_roots(&mut self) {
        self.gc_roots.clear();
    }

    /// Collect unreachable objects and scopes.
    pub fn collect_garbage(&mut self) {
        // Queued microtasks capture values in Rust closures that the collector
        // cannot see, so collection is deferred until the queue is empty.
        if self.in_microtask || !self.microtasks.is_empty() {
            return;
        }

        let heap_len = self.heap.len();
        let scope_len = self.scopes.len();
        let mut marked = vec![false; heap_len];
        let mut marked_scopes = vec![false; scope_len];
        let mut values: Vec<Value> = Vec::new();
        let mut scopes: Vec<ScopeId> = Vec::new();

        values.push(Value::Object(self.global));
        values.push(Value::Object(self.prototypes.object));
        values.push(Value::Object(self.prototypes.array));
        values.push(Value::Object(self.prototypes.function));
        values.push(Value::Object(self.prototypes.string));
        values.push(Value::Object(self.prototypes.number));
        values.push(Value::Object(self.prototypes.boolean));
        values.push(Value::Object(self.prototypes.error));
        values.push(Value::Object(self.prototypes.promise));
        values.push(Value::Object(self.prototypes.regexp));
        values.push(self.completion_value.clone());
        values.extend(self.gc_roots.iter().cloned());
        values.extend(self.temp_roots.iter().cloned());
        scopes.extend(self.env_stack.iter().copied());
        scopes.extend(self.var_scope_stack.iter().copied());
        scopes.extend(self.gc_scope_roots.iter().copied());
        scopes.push(self.global_scope);

        while !values.is_empty() || !scopes.is_empty() {
            if let Some(value) = values.pop() {
                if let Value::Object(id) = value {
                    mark_object(id, &self.heap, &mut marked, &mut values, &mut scopes);
                }
                continue;
            }
            if let Some(scope) = scopes.pop() {
                mark_scope(
                    scope,
                    &self.scopes,
                    &mut marked_scopes,
                    &mut values,
                    &mut scopes,
                );
            }
        }

        for (index, slot) in self.heap.iter_mut().enumerate() {
            if slot.is_some() && !marked[index] {
                *slot = None;
                self.heap_free.push(index);
            }
        }
        for (index, slot) in self.scopes.iter_mut().enumerate() {
            if slot.is_some() && !marked_scopes[index] {
                *slot = None;
                if index != self.global_scope.0 {
                    self.scope_free.push(index);
                }
            }
        }
        self.allocations_since_gc = 0;
    }
}

fn mark_object(
    id: ObjectId,
    heap: &[Option<Object>],
    marked: &mut [bool],
    values: &mut Vec<Value>,
    scopes: &mut Vec<ScopeId>,
) {
    let Some(slot) = marked.get_mut(id.0) else {
        return;
    };
    if *slot {
        return;
    }
    *slot = true;
    let Some(object) = heap.get(id.0).and_then(Option::as_ref) else {
        return;
    };
    if let Some(prototype) = object.prototype {
        values.push(Value::Object(prototype));
    }
    for (_, property) in &object.properties {
        match property {
            PropertyData::Data(value) => values.push(value.clone()),
            PropertyData::Accessor { getter, setter } => {
                if let Some(getter) = getter {
                    values.push(getter.clone());
                }
                if let Some(setter) = setter {
                    values.push(setter.clone());
                }
            }
        }
    }
    match &object.kind {
        ObjectKind::Ordinary => {}
        ObjectKind::Array(items) => values.extend(items.iter().cloned()),
        ObjectKind::Function(function) => {
            if let FunctionImpl::User(user) = &function.implementation {
                scopes.push(user.env);
            }
        }
        ObjectKind::Promise(state) => {
            push_promise_values(state, values);
        }
        ObjectKind::RegExp(_) => {}
    }
}

fn push_promise_values(
    state: &std::rc::Rc<std::cell::RefCell<PromiseState>>,
    values: &mut Vec<Value>,
) {
    let Ok(state) = state.try_borrow() else {
        return;
    };
    if !matches!(state.status, PromiseStatus::Pending) {
        values.push(state.result.clone());
    }
    for reaction in &state.reactions {
        if let Some(handler) = &reaction.on_fulfilled {
            values.push(handler.clone());
        }
        if let Some(handler) = &reaction.on_rejected {
            values.push(handler.clone());
        }
        values.push(Value::Object(reaction.next));
    }
}

fn mark_scope(
    id: ScopeId,
    scopes: &[Option<Scope>],
    marked: &mut [bool],
    values: &mut Vec<Value>,
    scope_stack: &mut Vec<ScopeId>,
) {
    let Some(slot) = marked.get_mut(id.0) else {
        return;
    };
    if *slot {
        return;
    }
    *slot = true;
    let Some(scope) = scopes.get(id.0).and_then(Option::as_ref) else {
        return;
    };
    if let Some(parent) = scope.parent {
        scope_stack.push(parent);
    }
    for (_, binding) in &scope.bindings {
        values.push(binding.value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_id(value: &Value) -> ObjectId {
        match value {
            Value::Object(id) => *id,
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn collects_unreachable_cycles() {
        let mut runtime = crate::Runtime::new();
        let baseline = runtime.heap_objects();
        let first = runtime.new_object();
        let second = runtime.new_object();
        runtime.set_property(first.clone(), "peer", second.clone());
        runtime.set_property(second.clone(), "peer", first.clone());
        let first_id = object_id(&first);
        let second_id = object_id(&second);
        runtime.set_property(first.clone(), "self_ref", first.clone());
        let _ = (first, second);

        runtime.collect_garbage();

        assert!(!runtime.object_has(&Value::Object(first_id), "peer"));
        assert!(!runtime.object_has(&Value::Object(second_id), "peer"));
        assert!(runtime.heap_objects() <= baseline);
    }

    #[test]
    fn keeps_reachable_objects() {
        let mut runtime = crate::Runtime::new();
        let value = runtime.new_object();
        runtime.set_property(value.clone(), "kept", Value::Number(42.0));
        let id = object_id(&value);
        runtime.add_gc_root(value);
        runtime.collect_garbage();
        assert_eq!(
            runtime.get_property(&Value::Object(id), "kept"),
            Value::Number(42.0)
        );

        runtime.clear_gc_roots();
        runtime.collect_garbage();
        assert!(!runtime.object_has(&Value::Object(id), "kept"));
    }

    #[test]
    fn reuses_freed_slots() {
        let mut runtime = crate::Runtime::new();
        for _ in 0..10 {
            let value = runtime.new_object();
            let _ = value;
        }
        let before = runtime.heap_objects();
        runtime.collect_garbage();
        let after_collection = runtime.heap_objects();
        assert!(after_collection < before);
        let reused = runtime.new_object();
        assert!(object_id(&reused).0 < before);
    }

    #[test]
    fn survives_closure_capture() {
        let mut runtime = crate::Runtime::new();
        runtime
            .eval(
                "function make() { var hidden = { value: 7 }; return function () { return hidden.value; }; } \
                 var closure = make();",
            )
            .expect("eval");
        runtime.collect_garbage();
        assert_eq!(runtime.eval("closure()").expect("eval"), Value::Number(7.0));
    }

    #[test]
    fn allocation_pressure_triggers_collection() {
        let mut runtime = crate::Runtime::new();
        runtime.gc_threshold = 8;
        runtime
            .eval("var count = 0; for (var i = 0; i < 500; i++) { ({ temp: i }); count = count + 1; }")
            .expect("eval");
        assert_eq!(runtime.eval("count").expect("eval"), Value::Number(500.0));
        assert!(runtime.heap_objects() < 500);
    }
}
