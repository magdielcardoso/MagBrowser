//! The `Map`, `Set`, `WeakMap` and `WeakSet` collection builtins.
//!
//! `Map` and `Set` keep an ordered list of entries and compare keys with
//! `SameValueZero`: `NaN` equals `NaN`, `+0` equals `-0`, and everything else
//! uses strict equality (so object keys compare by identity). Iterators are
//! live: they read the shared entry list on every `next()` call.
//!
//! `WeakMap` and `WeakSet` accept object keys only. This engine does not do
//! real weak cleanup: entries are retained until the collection dies. To keep
//! recycled heap slots from aliasing a dead key, each weak entry records the
//! object's heap generation and lookups ignore entries whose key object is no
//! longer live (or whose slot has been reused). The collector treats weak keys
//! as weak (only values are traced), so a key can be collected when nothing
//! else holds it.

use std::cell::RefCell;
use std::rc::Rc;

use crate::Runtime;
use crate::errors::JsError;
use crate::object::{CollectionData, CollectionEntry, ObjectKind, SYMBOL_ITERATOR_KEY};
use crate::value::{ObjectId, Value};

/// Which internal slot a method expects on its receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollectionKind {
    Map,
    Set,
    WeakMap,
    WeakSet,
}

/// Which component of an entry an iterator yields.
#[derive(Clone, Copy)]
enum IterMode {
    Keys,
    Values,
    Entries,
}

impl Runtime {
    /// Install the four collection constructors and their prototypes.
    pub(crate) fn install_collections(&mut self) {
        self.install_map();
        self.install_set();
        self.install_weak_map();
        self.install_weak_set();
    }

    fn install_map(&mut self) {
        let prototype = self.prototypes.map;
        self.define_builtin_method(prototype, "set", 2, map_set);
        self.define_builtin_method(prototype, "get", 1, map_get);
        self.define_builtin_method(prototype, "has", 1, map_has);
        self.define_builtin_method(prototype, "delete", 1, map_delete);
        self.define_builtin_method(prototype, "clear", 0, map_clear);
        self.define_builtin_method(prototype, "keys", 0, map_keys);
        self.define_builtin_method(prototype, "values", 0, map_values);
        self.define_builtin_method(prototype, "forEach", 1, map_for_each);
        let size_getter = self.new_builtin_value("get size", 0, map_size);
        self.define_accessor_property(prototype, "size", Some(size_getter), None);
        // `entries` and `[Symbol.iterator]` are the same operation for Map.
        let entries = self.new_builtin_value("entries", 0, map_entries);
        self.define_data_property(prototype, "entries", entries.clone());
        self.define_data_property(prototype, SYMBOL_ITERATOR_KEY, entries);

        let constructor = self.new_builtin_value("Map", 1, map_constructor);
        self.define_data_property(prototype, "constructor", constructor.clone());
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
        }
        self.declare_global("Map", constructor);
    }

    fn install_set(&mut self) {
        let prototype = self.prototypes.set;
        self.define_builtin_method(prototype, "add", 1, set_add);
        self.define_builtin_method(prototype, "has", 1, set_has);
        self.define_builtin_method(prototype, "delete", 1, set_delete);
        self.define_builtin_method(prototype, "clear", 0, set_clear);
        self.define_builtin_method(prototype, "keys", 0, set_values);
        self.define_builtin_method(prototype, "forEach", 1, set_for_each);
        let size_getter = self.new_builtin_value("get size", 0, set_size);
        self.define_accessor_property(prototype, "size", Some(size_getter), None);
        let values = self.new_builtin_value("values", 0, set_values);
        self.define_data_property(prototype, "values", values.clone());
        self.define_data_property(prototype, SYMBOL_ITERATOR_KEY, values);
        self.define_builtin_method(prototype, "entries", 0, set_entries);

        let constructor = self.new_builtin_value("Set", 1, set_constructor);
        self.define_data_property(prototype, "constructor", constructor.clone());
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
        }
        self.declare_global("Set", constructor);
    }

    fn install_weak_map(&mut self) {
        let prototype = self.prototypes.weakmap;
        self.define_builtin_method(prototype, "set", 2, weak_map_set);
        self.define_builtin_method(prototype, "get", 1, weak_map_get);
        self.define_builtin_method(prototype, "has", 1, weak_map_has);
        self.define_builtin_method(prototype, "delete", 1, weak_map_delete);

        let constructor = self.new_builtin_value("WeakMap", 1, weak_map_constructor);
        self.define_data_property(prototype, "constructor", constructor.clone());
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
        }
        self.declare_global("WeakMap", constructor);
    }

    fn install_weak_set(&mut self) {
        let prototype = self.prototypes.weakset;
        self.define_builtin_method(prototype, "add", 1, weak_set_add);
        self.define_builtin_method(prototype, "has", 1, weak_set_has);
        self.define_builtin_method(prototype, "delete", 1, weak_set_delete);

        let constructor = self.new_builtin_value("WeakSet", 1, weak_set_constructor);
        self.define_data_property(prototype, "constructor", constructor.clone());
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
        }
        self.declare_global("WeakSet", constructor);
    }
}

// ---------------------------------------------------------------------------
// Key comparison and storage helpers
// ---------------------------------------------------------------------------

/// JavaScript's `SameValueZero`: `NaN` equals `NaN` and `+0` equals `-0`.
fn same_value_zero(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => {
            if a.is_nan() && b.is_nan() {
                true
            } else {
                a == b
            }
        }
        _ => left.strict_equals(right),
    }
}

fn find_index(data: &CollectionData, key: &Value) -> Option<usize> {
    data.entries
        .iter()
        .position(|entry| same_value_zero(&entry.key, key))
}

/// Fetch the shared entry list for a receiver, validating the internal slot.
fn collection_state(
    runtime: &Runtime,
    this: &Value,
    kind: CollectionKind,
) -> Result<Rc<RefCell<CollectionData>>, JsError> {
    let Value::Object(id) = this else {
        return Err(JsError::type_error(format!(
            "{kind:?} method called on an incompatible receiver"
        )));
    };
    let Some(object) = runtime.object(*id) else {
        return Err(JsError::type_error("collection object was collected"));
    };
    let data = match (&object.kind, kind) {
        (ObjectKind::Map(data), CollectionKind::Map)
        | (ObjectKind::Set(data), CollectionKind::Set)
        | (ObjectKind::WeakMap(data), CollectionKind::WeakMap)
        | (ObjectKind::WeakSet(data), CollectionKind::WeakSet) => data,
        _ => {
            return Err(JsError::type_error(format!(
                "{kind:?} method called on an incompatible receiver"
            )));
        }
    };
    Ok(Rc::clone(data))
}

/// Drop weak entries whose key object has been collected or whose heap slot has
/// been recycled.
fn prune_weak(runtime: &Runtime, data: &mut CollectionData) {
    data.entries.retain(|entry| match &entry.key {
        Value::Object(id) => {
            runtime.object(*id).is_some()
                && runtime.heap_generations.get(id.0).copied() == Some(entry.generation)
        }
        _ => false,
    });
}

/// Prune stale weak entries, then locate `key` in the remaining entries.
fn weak_find(runtime: &Runtime, data: &Rc<RefCell<CollectionData>>, key: &Value) -> Option<usize> {
    let mut entries = data.borrow_mut();
    prune_weak(runtime, &mut entries);
    find_index(&entries, key)
}

fn require_object_key(key: &Value, name: &str) -> Result<ObjectId, JsError> {
    match key {
        Value::Object(id) => Ok(*id),
        _ => Err(JsError::type_error(format!("{name} keys must be objects"))),
    }
}

fn map_store(data: &Rc<RefCell<CollectionData>>, key: Value, value: Value) {
    let mut data = data.borrow_mut();
    if let Some(index) = find_index(&data, &key) {
        data.entries[index].value = value;
    } else {
        data.entries.push(CollectionEntry {
            key,
            value,
            generation: 0,
        });
    }
}

fn set_store(data: &Rc<RefCell<CollectionData>>, value: Value) -> bool {
    let mut data = data.borrow_mut();
    if find_index(&data, &value).is_some() {
        return false;
    }
    data.entries.push(CollectionEntry {
        key: value.clone(),
        value,
        generation: 0,
    });
    true
}

fn weak_map_store(
    runtime: &Runtime,
    data: &Rc<RefCell<CollectionData>>,
    key: Value,
    value: Value,
) -> Result<(), JsError> {
    let id = require_object_key(&key, "WeakMap")?;
    let generation = runtime.heap_generations.get(id.0).copied().unwrap_or(0);
    let mut entries = data.borrow_mut();
    prune_weak(runtime, &mut entries);
    if let Some(index) = find_index(&entries, &key) {
        entries.entries[index].value = value;
    } else {
        entries.entries.push(CollectionEntry {
            key,
            value,
            generation,
        });
    }
    Ok(())
}

fn weak_set_store(
    runtime: &Runtime,
    data: &Rc<RefCell<CollectionData>>,
    value: Value,
) -> Result<(), JsError> {
    let id = require_object_key(&value, "WeakSet")?;
    let generation = runtime.heap_generations.get(id.0).copied().unwrap_or(0);
    let mut entries = data.borrow_mut();
    prune_weak(runtime, &mut entries);
    if find_index(&entries, &value).is_none() {
        entries.entries.push(CollectionEntry {
            key: value.clone(),
            value,
            generation,
        });
    }
    Ok(())
}

/// Initialize a collection from an optional iterable argument.
fn init_from_iterable(
    runtime: &mut Runtime,
    data: &Rc<RefCell<CollectionData>>,
    iterable: &Value,
    kind: CollectionKind,
) -> Result<(), JsError> {
    if matches!(iterable, Value::Undefined | Value::Null) {
        return Ok(());
    }
    let items = runtime.iterate_values(iterable)?;
    for item in items {
        match kind {
            CollectionKind::Map => {
                let pair = runtime.iterate_values(&item)?;
                let key = pair.first().cloned().unwrap_or(Value::Undefined);
                let value = pair.get(1).cloned().unwrap_or(Value::Undefined);
                map_store(data, key, value);
            }
            CollectionKind::Set => {
                set_store(data, item);
            }
            CollectionKind::WeakSet => {
                weak_set_store(runtime, data, item)?;
            }
            CollectionKind::WeakMap => {
                let pair = runtime.iterate_values(&item)?;
                let key = pair.first().cloned().unwrap_or(Value::Undefined);
                let value = pair.get(1).cloned().unwrap_or(Value::Undefined);
                weak_map_store(runtime, data, key, value)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Iterator construction
// ---------------------------------------------------------------------------

/// Build a live iterator over a collection's shared entry list.
fn new_collection_iterator(
    runtime: &mut Runtime,
    data: Rc<RefCell<CollectionData>>,
    mode: IterMode,
) -> Value {
    let position = Rc::new(RefCell::new(0usize));
    let next = runtime.new_native_value(
        "next",
        0,
        Box::new(move |runtime, _this, _args| {
            let (key, value, done) = {
                let entries = data.borrow();
                let index = *position.borrow();
                if index >= entries.entries.len() {
                    (Value::Undefined, Value::Undefined, true)
                } else {
                    let entry = &entries.entries[index];
                    (entry.key.clone(), entry.value.clone(), false)
                }
            };
            if done {
                return Ok(runtime.new_iterator_result(Value::Undefined, true));
            }
            *position.borrow_mut() += 1;
            let result = match mode {
                IterMode::Keys => key,
                IterMode::Values => value,
                IterMode::Entries => runtime.new_array(vec![key, value]),
            };
            Ok(runtime.new_iterator_result(result, false))
        }),
    );
    let iterator = runtime.new_object();
    runtime.set_property(iterator.clone(), "next", next);
    if let Value::Object(id) = iterator {
        // Reusing the generator's self-iterator makes this object satisfy the
        // iterator protocol through `Symbol.iterator` as well.
        let self_iterator = runtime.new_builtin_value(
            "[Symbol.iterator]",
            0,
            crate::builtins::generator_self_iterator,
        );
        runtime.define_data_property(id, SYMBOL_ITERATOR_KEY, self_iterator);
        Value::Object(id)
    } else {
        iterator
    }
}

// ---------------------------------------------------------------------------
// Map
// ---------------------------------------------------------------------------

fn map_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let data = Rc::new(RefCell::new(CollectionData::default()));
    let id = runtime.alloc_object(
        ObjectKind::Map(Rc::clone(&data)),
        Some(runtime.prototypes.map),
    );
    let value = Value::Object(id);
    let mark = runtime.temp_roots.len();
    runtime.temp_roots.push(value.clone());
    let iterable = args.first().cloned().unwrap_or(Value::Undefined);
    let result = init_from_iterable(runtime, &data, &iterable, CollectionKind::Map);
    runtime.temp_roots.truncate(mark);
    result?;
    Ok(value)
}

fn map_set(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Map)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
    map_store(&data, key, value);
    Ok(this)
}

fn map_get(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Map)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(data
        .borrow()
        .entries
        .iter()
        .find(|entry| same_value_zero(&entry.key, &key))
        .map(|entry| entry.value.clone())
        .unwrap_or(Value::Undefined))
}

fn map_has(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Map)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(Value::Bool(find_index(&data.borrow(), &key).is_some()))
}

fn map_delete(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Map)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = data.borrow_mut();
    if let Some(index) = find_index(&entries, &key) {
        entries.entries.remove(index);
        Ok(Value::Bool(true))
    } else {
        Ok(Value::Bool(false))
    }
}

fn map_clear(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Map)?;
    data.borrow_mut().entries.clear();
    Ok(Value::Undefined)
}

fn map_size(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Map)?;
    Ok(Value::Number(data.borrow().entries.len() as f64))
}

fn iterator_from_receiver(
    runtime: &mut Runtime,
    this: &Value,
    kind: CollectionKind,
    mode: IterMode,
) -> Result<Value, JsError> {
    let data = collection_state(runtime, this, kind)?;
    Ok(new_collection_iterator(runtime, data, mode))
}

fn map_keys(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    iterator_from_receiver(runtime, &this, CollectionKind::Map, IterMode::Keys)
}

fn map_values(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    iterator_from_receiver(runtime, &this, CollectionKind::Map, IterMode::Values)
}

fn map_entries(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    iterator_from_receiver(runtime, &this, CollectionKind::Map, IterMode::Entries)
}

fn map_for_each(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Map)?;
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&callback) {
        return Err(JsError::type_error(
            "Map.prototype.forEach callback is not a function",
        ));
    }
    let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    // Snapshot so the callback can mutate the map without invalidating a borrow.
    let entries: Vec<(Value, Value)> = data
        .borrow()
        .entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect();
    for (key, value) in entries {
        runtime.call_function(&callback, this_arg.clone(), vec![value, key, this.clone()])?;
    }
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------------------
// Set
// ---------------------------------------------------------------------------

fn set_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let data = Rc::new(RefCell::new(CollectionData::default()));
    let id = runtime.alloc_object(
        ObjectKind::Set(Rc::clone(&data)),
        Some(runtime.prototypes.set),
    );
    let value = Value::Object(id);
    let mark = runtime.temp_roots.len();
    runtime.temp_roots.push(value.clone());
    let iterable = args.first().cloned().unwrap_or(Value::Undefined);
    let result = init_from_iterable(runtime, &data, &iterable, CollectionKind::Set);
    runtime.temp_roots.truncate(mark);
    result?;
    Ok(value)
}

fn set_add(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Set)?;
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    set_store(&data, value);
    Ok(this)
}

fn set_has(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Set)?;
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(Value::Bool(find_index(&data.borrow(), &value).is_some()))
}

fn set_delete(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Set)?;
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = data.borrow_mut();
    if let Some(index) = find_index(&entries, &value) {
        entries.entries.remove(index);
        Ok(Value::Bool(true))
    } else {
        Ok(Value::Bool(false))
    }
}

fn set_clear(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Set)?;
    data.borrow_mut().entries.clear();
    Ok(Value::Undefined)
}

fn set_size(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Set)?;
    Ok(Value::Number(data.borrow().entries.len() as f64))
}

fn set_values(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    iterator_from_receiver(runtime, &this, CollectionKind::Set, IterMode::Values)
}

fn set_entries(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    iterator_from_receiver(runtime, &this, CollectionKind::Set, IterMode::Entries)
}

fn set_for_each(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::Set)?;
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&callback) {
        return Err(JsError::type_error(
            "Set.prototype.forEach callback is not a function",
        ));
    }
    let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    let values: Vec<Value> = data
        .borrow()
        .entries
        .iter()
        .map(|entry| entry.value.clone())
        .collect();
    for value in values {
        runtime.call_function(
            &callback,
            this_arg.clone(),
            vec![value.clone(), value, this.clone()],
        )?;
    }
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------------------
// WeakMap / WeakSet
// ---------------------------------------------------------------------------

fn weak_map_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let data = Rc::new(RefCell::new(CollectionData::default()));
    let id = runtime.alloc_object(
        ObjectKind::WeakMap(Rc::clone(&data)),
        Some(runtime.prototypes.weakmap),
    );
    let value = Value::Object(id);
    let mark = runtime.temp_roots.len();
    runtime.temp_roots.push(value.clone());
    let iterable = args.first().cloned().unwrap_or(Value::Undefined);
    let result = init_from_iterable(runtime, &data, &iterable, CollectionKind::WeakMap);
    runtime.temp_roots.truncate(mark);
    result?;
    Ok(value)
}

fn weak_map_set(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::WeakMap)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
    weak_map_store(runtime, &data, key, value)?;
    Ok(this)
}

fn weak_map_get(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::WeakMap)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    require_object_key(&key, "WeakMap")?;
    Ok(weak_find(runtime, &data, &key)
        .and_then(|index| {
            data.borrow()
                .entries
                .get(index)
                .map(|entry| entry.value.clone())
        })
        .unwrap_or(Value::Undefined))
}

fn weak_map_has(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::WeakMap)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    require_object_key(&key, "WeakMap")?;
    Ok(Value::Bool(weak_find(runtime, &data, &key).is_some()))
}

fn weak_map_delete(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::WeakMap)?;
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    require_object_key(&key, "WeakMap")?;
    if let Some(index) = weak_find(runtime, &data, &key) {
        data.borrow_mut().entries.remove(index);
        Ok(Value::Bool(true))
    } else {
        Ok(Value::Bool(false))
    }
}

fn weak_set_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let data = Rc::new(RefCell::new(CollectionData::default()));
    let id = runtime.alloc_object(
        ObjectKind::WeakSet(Rc::clone(&data)),
        Some(runtime.prototypes.weakset),
    );
    let value = Value::Object(id);
    let mark = runtime.temp_roots.len();
    runtime.temp_roots.push(value.clone());
    let iterable = args.first().cloned().unwrap_or(Value::Undefined);
    let result = init_from_iterable(runtime, &data, &iterable, CollectionKind::WeakSet);
    runtime.temp_roots.truncate(mark);
    result?;
    Ok(value)
}

fn weak_set_add(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::WeakSet)?;
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    weak_set_store(runtime, &data, value)?;
    Ok(this)
}

fn weak_set_has(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::WeakSet)?;
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    require_object_key(&value, "WeakSet")?;
    Ok(Value::Bool(weak_find(runtime, &data, &value).is_some()))
}

fn weak_set_delete(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let data = collection_state(runtime, &this, CollectionKind::WeakSet)?;
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    require_object_key(&value, "WeakSet")?;
    if let Some(index) = weak_find(runtime, &data, &value) {
        data.borrow_mut().entries.remove(index);
        Ok(Value::Bool(true))
    } else {
        Ok(Value::Bool(false))
    }
}

#[cfg(test)]
mod tests {
    use crate::Runtime;
    use crate::value::Value;

    fn eval(source: &str) -> Value {
        let mut runtime = Runtime::new();
        runtime
            .eval(source)
            .unwrap_or_else(|error| panic!("{source}: {error}"))
    }

    fn truth(source: &str) -> bool {
        match eval(source) {
            Value::Bool(value) => value,
            other => panic!("{source}: expected boolean, got {other:?}"),
        }
    }

    fn number(source: &str) -> f64 {
        match eval(source) {
            Value::Number(value) => value,
            other => panic!("{source}: expected number, got {other:?}"),
        }
    }

    fn error(source: &str) -> String {
        let mut runtime = Runtime::new();
        runtime.eval(source).expect_err(source).message
    }

    #[test]
    fn constructors_are_installed_as_functions() {
        truth("typeof Map === 'function'");
        truth("typeof Set === 'function'");
        truth("typeof WeakMap === 'function'");
        truth("typeof WeakSet === 'function'");
        truth("new Map() instanceof Map");
        truth("new Set() instanceof Set");
        truth("Object.prototype.toString.call(new Map()) === '[object Map]'");
        truth("Object.prototype.toString.call(new Set()) === '[object Set]'");
        truth("Object.prototype.toString.call(new WeakMap()) === '[object WeakMap]'");
        truth("Object.prototype.toString.call(new WeakSet()) === '[object WeakSet]'");
        truth("Map.prototype.constructor === Map");
        truth("Map.prototype.prototype === undefined");
        truth("typeof Map.prototype.set === 'function'");
    }

    #[test]
    fn map_set_get_has_delete_clear_and_size() {
        assert_eq!(
            number("var m = new Map(); m.set('a', 1); m.set('b', 2); m.size"),
            2.0
        );
        truth("var m = new Map(); m.set('a', 1) === m");
        truth("var m = new Map(); m.set('a', 1); m.get('a') === 1");
        truth("var m = new Map(); m.get('missing') === undefined");
        truth("var m = new Map(); m.set('a', 1); m.has('a')");
        truth("var m = new Map(); !m.has('a')");
        truth("var m = new Map(); m.set('a', 1); m.delete('a') === true && m.size === 0");
        truth("var m = new Map(); m.delete('a') === false");
        truth("var m = new Map(); m.set('a', 1); m.clear(); m.size === 0 && !m.has('a')");
        truth("var m = new Map(); m.clear() === undefined");
        // Updating an existing key must not grow the collection.
        assert_eq!(
            number("var m = new Map(); m.set('a', 1); m.set('a', 9); m.get('a') + m.size"),
            10.0
        );
    }

    #[test]
    fn map_from_iterable() {
        truth("var m = new Map([['a', 1], ['b', 2]]); m.size === 2 && m.get('b') === 2");
        truth("var m = new Map(); m.set('x', 1); var n = new Map(m); n.get('x') === 1 && n !== m");
        truth("var s = new Set([['k', 7]]); new Map(s).get('k') === 7");
        truth("var g = (function*(){ yield ['a', 1]; yield ['b', 2]; })(); new Map(g).size === 2");
        truth("new Map(null).size === 0");
        truth("new Map(undefined).size === 0");
        assert!(error("new Map(42)").contains("TypeError"));
    }

    #[test]
    fn map_uses_same_value_zero_keys() {
        truth("var m = new Map(); m.set(NaN, 'nan'); m.get(NaN) === 'nan'");
        truth("var m = new Map(); m.set(0, 'zero'); m.get(-0) === 'zero'");
        truth("var m = new Map(); m.set(-0, 'zero'); m.get(0) === 'zero'");
        assert_eq!(
            number("var m = new Map(); m.set(NaN, 1); m.set(NaN, 2); m.size"),
            1.0
        );
        truth(
            "var m = new Map(); m.set(1, 'number'); m.set(BigInt(1), 'bigint'); m.get(1) === 'number' && m.get(BigInt(1)) === 'bigint'",
        );
        truth(
            "var k = {}; var m = new Map(); m.set(k, 'obj'); m.get({}) === undefined && m.get(k) === 'obj'",
        );
        truth(
            "var m = new Map(); m.set('1', 1); m.set(1, 2); m.size === 2 && m.get('1') === 1 && m.get(1) === 2",
        );
    }

    #[test]
    fn map_iteration_and_for_of() {
        truth("var m = new Map([['a', 1], ['b', 2]]); [...m.keys()].join(',') === 'a,b'");
        truth("var m = new Map([['a', 1], ['b', 2]]); [...m.values()].join(',') === '1,2'");
        truth(
            "var m = new Map([['a', 1], ['b', 2]]); [...m.entries()].map(function(p){ return p[0] + p[1]; }).join(',') === 'a1,b2'",
        );
        truth(
            "var m = new Map([['a', 1], ['b', 2]]); [...m].map(function(p){ return p.join(':'); }).join(',') === 'a:1,b:2'",
        );
        truth(
            "var out = []; var m = new Map([['a', 1], ['b', 2]]); for (var e of m) { out.push(e[0] + e[1]); } out.join(',') === 'a1,b2'",
        );
        truth(
            "var out = []; var s = new Set([3, 4]); for (var v of s) { out.push(v); } out.join(',') === '3,4'",
        );
        truth(
            "var m = new Map([['a', 1]]); var it = m[Symbol.iterator](); it.next().value[0] === 'a' && it.next().done === true",
        );
        truth(
            "var m = new Map([['a', 1], ['b', 2]]); JSON.stringify([...m]) === '[[\"a\",1],[\"b\",2]]'",
        );
        truth(
            "var m = new Map([['a', 1], ['b', 2]]); [...m.values()].reduce(function(a, b){ return a + b; }, 0) === 3",
        );
    }

    #[test]
    fn map_iterators_are_live() {
        truth(
            "var m = new Map([['a', 1]]); var it = m.keys(); m.set('b', 2); [...it].join(',') === 'a,b'",
        );
    }

    #[test]
    fn map_for_each() {
        truth(
            "var out = []; var m = new Map([['a', 1]]); m.forEach(function(v, k, owner) { out.push(v + k + (owner === m)); }); out[0] === '1atrue'",
        );
        truth(
            "var out = []; var ctx = { tag: 'x' }; new Map([['a', 1]]).forEach(function(v, k) { out.push(this.tag + v); }, ctx); out[0] === 'x1'",
        );
        assert!(error("new Map().forEach(1)").contains("TypeError"));
    }

    #[test]
    fn set_basics_and_iteration() {
        assert_eq!(number("var s = new Set(); s.add(1); s.add(2); s.size"), 2.0);
        truth("var s = new Set(); s.add(1) === s");
        truth("var s = new Set(); s.add(1); s.has(1) && !s.has(2)");
        truth("var s = new Set(); s.add(NaN); s.has(NaN)");
        truth("var s = new Set(); s.add(0); s.has(-0)");
        truth("var s = new Set(); s.add(1); s.add(1); s.size === 1");
        truth("var s = new Set(); s.add(1); s.delete(1) === true && s.size === 0");
        truth("var s = new Set(); s.delete(1) === false");
        truth("var s = new Set(); s.add(1); s.clear(); !s.has(1) && s.size === 0");
        truth("var s = new Set([1, 2, 2, 3]); s.size === 3");
        truth("var s = new Set('aba'); s.size === 2");
        truth("var s = new Set(); s.add(1); var t = new Set(s); t.has(1) && t !== s");
        truth("var s = new Set([1, 2]); [...s.keys()].join(',') === '1,2'");
        truth("var s = new Set([1, 2]); [...s.values()].join(',') === '1,2'");
        truth(
            "var s = new Set([1, 2]); [...s.entries()].map(function(p){ return p.join(':'); }).join(',') === '1:1,2:2'",
        );
        truth("var s = new Set([1, 2]); [...s].join(',') === '1,2'");
        truth(
            "var out = []; new Set([1, 2]).forEach(function(v, v2, owner) { out.push(v + ':' + v2 + ':' + (owner instanceof Set)); }); out.join('|') === '1:1:true|2:2:true'",
        );
        assert!(error("new Set().forEach(null)").contains("TypeError"));
        assert!(error("new Set(42)").contains("TypeError"));
    }

    #[test]
    fn weak_collections_use_object_keys() {
        truth("var k = {}; var wm = new WeakMap(); wm.set(k, 7); wm.get(k) === 7 && wm.has(k)");
        truth(
            "var k = {}; var wm = new WeakMap(); wm.set(k, 7); wm.delete(k) === true && !wm.has(k)",
        );
        truth("var k = {}; var wm = new WeakMap(); wm.set(k, 1); wm.set(k, 2); wm.get(k) === 2");
        truth(
            "var wm = new WeakMap(); wm.get({}) === undefined && !wm.has({}) && wm.delete({}) === false",
        );
        truth(
            "var a = {}, b = {}; var wm = new WeakMap(); wm.set(a, 'a'); wm.set(b, 'b'); wm.get(a) === 'a' && wm.get(b) === 'b'",
        );
        truth("var k = {}; var ws = new WeakSet(); ws.add(k); ws.has(k) && !ws.has({})");
        truth("var k = {}; var ws = new WeakSet(); ws.add(k); ws.delete(k) === true && !ws.has(k)");
        truth(
            "var k = {}; var ws = new WeakSet(); ws.add(k); ws.add(k); ws.delete(k) === true && ws.delete(k) === false",
        );
        truth("var k = {}; var wm = new WeakMap([[k, 5]]); wm.get(k) === 5");
        truth("var k = {}; var ws = new WeakSet([k]); ws.has(k)");

        assert!(error("new WeakMap().set(1, 2)").contains("TypeError"));
        assert!(error("new WeakMap().get('x')").contains("TypeError"));
        assert!(error("new WeakMap().has(null)").contains("TypeError"));
        assert!(error("new WeakSet().add('x')").contains("TypeError"));
        assert!(error("new WeakSet().has(1)").contains("TypeError"));
        truth("new WeakMap().size === undefined");
        truth("new WeakSet().size === undefined");
        truth("typeof new WeakMap().keys === 'undefined'");
        assert!(error("[...new WeakSet()]").contains("TypeError"));
    }

    #[test]
    fn methods_validate_receiver() {
        assert!(error("Map.prototype.get.call({}, 'a')").contains("TypeError"));
        assert!(error("Set.prototype.has.call(new Map(), 1)").contains("TypeError"));
        assert!(error("WeakMap.prototype.get.call(new Map(), {})").contains("TypeError"));
        truth("new Map().size === 0");
    }

    #[test]
    fn weak_keys_are_not_traced() {
        let mut runtime = Runtime::new();
        runtime
            .eval(
                "var wm = new WeakMap(); var ws = new WeakSet(); \
                 (function () { var key = {}; wm.set(key, 'secret'); ws.add(key); })();",
            )
            .expect("eval");
        runtime.collect_garbage();
        // A fresh object must never observe the value of a collected key, even
        // if its heap slot is recycled (the stored generation guards that).
        assert_eq!(
            runtime
                .eval("var fresh = {}; wm.get(fresh) === undefined && !ws.has(fresh)")
                .expect("eval"),
            Value::Bool(true)
        );
    }

    #[test]
    fn collections_are_traced_by_gc() {
        let mut runtime = Runtime::new();
        runtime
            .eval(
                "var m = new Map(); m.set('k', { answer: 42 }); \
                 var s = new Set([{ n: 7 }]);",
            )
            .expect("eval");
        runtime.collect_garbage();
        assert_eq!(
            runtime.eval("m.get('k').answer").expect("eval"),
            Value::Number(42.0)
        );
        assert_eq!(
            runtime.eval("[...s][0].n").expect("eval"),
            Value::Number(7.0)
        );
    }
}
