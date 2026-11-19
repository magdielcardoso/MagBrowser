//! Heap objects, properties, arrays and prototype chains.

use std::cell::RefCell;
use std::rc::Rc;

use crate::errors::JsError;
use crate::generator::GeneratorFrame;
use crate::promise::PromiseState;
use crate::value::{ObjectId, Value};

/// Internal property key used for `Symbol.iterator` methods. Symbols are
/// represented by unique objects; this private string key keeps symbol-keyed
/// properties separate from ordinary string-keyed ones.
pub(crate) const SYMBOL_ITERATOR_KEY: &str = "\u{0}Symbol.iterator";

/// Internal property key used for `Symbol.asyncIterator` methods.
pub(crate) const SYMBOL_ASYNC_ITERATOR_KEY: &str = "\u{0}Symbol.asyncIterator";

/// A builtin function implemented as a stateless function pointer.
pub(crate) type BuiltinFn = fn(&mut crate::Runtime, Value, Vec<Value>) -> Result<Value, JsError>;

/// A host supplied function.
pub type HostFunction =
    Box<dyn FnMut(&mut crate::Runtime, Value, Vec<Value>) -> Result<Value, JsError>>;

/// The value stored for an own property.
#[derive(Clone)]
pub(crate) enum PropertyData {
    /// A data property holding a value.
    Data(Value),
    /// An accessor property with optional getter and setter functions.
    Accessor {
        getter: Option<Value>,
        setter: Option<Value>,
    },
}

/// The kind specific payload of a heap object.
pub(crate) enum ObjectKind {
    /// A plain object.
    Ordinary,
    /// An array backed by a dense vector of elements.
    Array(Vec<Value>),
    /// A callable object.
    Function(crate::function::FunctionObject),
    /// A promise.
    Promise(Rc<RefCell<PromiseState>>),
    /// A regular expression.
    RegExp(Rc<browser_js_parser::Regex>),
    /// A symbol primitive, identified by its description.
    Symbol(String),
    /// A suspended generator object.
    Generator(Rc<RefCell<GeneratorFrame>>),
    /// A proxy that forwards internal operations through handler traps.
    Proxy {
        /// The object being wrapped.
        target: Value,
        /// The object supplying the traps.
        handler: Value,
    },
    /// A weak reference to a heap object. The target id is deliberately not
    /// traced by the collector, so holding a `WeakRef` never keeps its target
    /// alive; the generation guards against id reuse.
    WeakRef {
        /// Heap slot of the referent.
        target: ObjectId,
        /// Generation of the slot when the reference was created.
        generation: u64,
    },
    /// A finalization registry whose registrations are inspected by the collector.
    FinalizationRegistry(Rc<RefCell<FinalizationRegistryState>>),
}

/// A single `FinalizationRegistry.register` entry. Only the target id is held
/// weakly; the held value and token are ordinary strong values.
#[derive(Clone)]
pub(crate) struct FinalizationRegistration {
    pub(crate) target: ObjectId,
    pub(crate) generation: u64,
    pub(crate) held: Value,
    pub(crate) token: Option<Value>,
}

/// Shared state behind a finalization registry object.
#[derive(Default)]
pub(crate) struct FinalizationRegistryState {
    pub(crate) registrations: Vec<FinalizationRegistration>,
}

/// A heap allocated object.
pub(crate) struct Object {
    pub(crate) kind: ObjectKind,
    pub(crate) prototype: Option<ObjectId>,
    pub(crate) properties: Vec<(String, PropertyData)>,
    pub(crate) extensible: bool,
}

/// Well known prototype objects.
pub(crate) struct Prototypes {
    pub(crate) object: ObjectId,
    pub(crate) array: ObjectId,
    pub(crate) function: ObjectId,
    pub(crate) string: ObjectId,
    pub(crate) number: ObjectId,
    pub(crate) boolean: ObjectId,
    pub(crate) error: ObjectId,
    pub(crate) promise: ObjectId,
    pub(crate) regexp: ObjectId,
}

impl crate::Runtime {
    pub(crate) fn alloc_object(
        &mut self,
        kind: ObjectKind,
        prototype: Option<ObjectId>,
    ) -> ObjectId {
        self.maybe_gc();
        let object = Object {
            kind,
            prototype,
            properties: Vec::new(),
            extensible: true,
        };
        let id = if let Some(index) = self.heap_free.pop() {
            // Bump the generation so weak references to the previous occupant
            // of this slot stop resolving to the new object.
            self.heap_generations[index] += 1;
            self.heap[index] = Some(object);
            index
        } else {
            self.heap.push(Some(object));
            self.heap_generations.push(1);
            self.heap.len() - 1
        };
        self.allocations_since_gc += 1;
        ObjectId(id)
    }

    pub(crate) fn object(&self, id: ObjectId) -> Option<&Object> {
        self.heap.get(id.0).and_then(Option::as_ref)
    }

    pub(crate) fn object_mut(&mut self, id: ObjectId) -> Option<&mut Object> {
        self.heap.get_mut(id.0).and_then(Option::as_mut)
    }

    /// Create a new plain object.
    pub fn new_object(&mut self) -> Value {
        let prototype = self.prototypes.object;
        Value::Object(self.alloc_object(ObjectKind::Ordinary, Some(prototype)))
    }

    /// Create a new array with the given elements.
    pub fn new_array(&mut self, items: Vec<Value>) -> Value {
        let prototype = self.prototypes.array;
        Value::Object(self.alloc_object(ObjectKind::Array(items), Some(prototype)))
    }

    /// Compile a regular expression and wrap it in a JavaScript object.
    pub(crate) fn new_regexp_value(&mut self, source: &str, flags: &str) -> Result<Value, JsError> {
        let regex = browser_js_parser::Regex::new(source, flags)
            .map_err(|error| JsError::syntax_error(error.message))?;
        let regex_flags = regex.flags();
        let prototype = self.prototypes.regexp;
        let id = self.alloc_object(ObjectKind::RegExp(Rc::new(regex)), Some(prototype));
        self.define_data_property(id, "source", Value::String(source.to_owned()));
        self.define_data_property(id, "flags", Value::String(regex_flags.as_string()));
        self.define_data_property(id, "global", Value::Bool(regex_flags.global));
        self.define_data_property(id, "ignoreCase", Value::Bool(regex_flags.ignore_case));
        self.define_data_property(id, "multiline", Value::Bool(regex_flags.multiline));
        self.define_data_property(id, "dotAll", Value::Bool(regex_flags.dot_all));
        self.define_data_property(id, "sticky", Value::Bool(regex_flags.sticky));
        self.define_data_property(id, "hasIndices", Value::Bool(regex_flags.has_indices));
        self.define_data_property(id, "lastIndex", Value::Number(0.0));
        Ok(Value::Object(id))
    }

    /// The compiled program behind a regular expression value.
    pub(crate) fn regex_of(&self, value: &Value) -> Option<Rc<browser_js_parser::Regex>> {
        let Value::Object(id) = value else {
            return None;
        };
        match self.object(*id).map(|object| &object.kind) {
            Some(ObjectKind::RegExp(regex)) => Some(Rc::clone(regex)),
            _ => None,
        }
    }

    /// Create an error-like object.
    pub(crate) fn new_error_value(&mut self, name: &str, message: &str) -> Value {
        let prototype = self.prototypes.error;
        let object = self.alloc_object(ObjectKind::Ordinary, Some(prototype));
        self.define_data_property(object, "name", Value::String(name.to_owned()));
        self.define_data_property(object, "message", Value::String(message.to_owned()));
        let stack: Vec<&str> = self.call_stack.iter().map(String::as_str).collect();
        let stack = format!("{name}: {message}\n    at {}", stack.join("\n    at "));
        self.define_data_property(object, "stack", Value::String(stack));
        Value::Object(object)
    }

    /// Define (or overwrite) an own data property.
    pub(crate) fn define_data_property(&mut self, object: ObjectId, name: &str, value: Value) {
        self.set_own_property_data(object, name, PropertyData::Data(value));
    }

    fn set_own_property_data(&mut self, object: ObjectId, name: &str, data: PropertyData) {
        if let Some(target) = self.object_mut(object) {
            if let Some((_, existing)) = target.properties.iter_mut().find(|(key, _)| key == name) {
                *existing = data;
            } else {
                target.properties.push((name.to_owned(), data));
            }
        }
    }

    /// The getter and setter currently installed for an own accessor property.
    pub(crate) fn accessor_pair(&self, id: ObjectId, key: &str) -> (Option<Value>, Option<Value>) {
        match self.object(id).and_then(|object| {
            object
                .properties
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, data)| data)
        }) {
            Some(PropertyData::Accessor { getter, setter }) => (getter.clone(), setter.clone()),
            _ => (None, None),
        }
    }

    /// Define (or overwrite) an own accessor property from function values.
    pub(crate) fn define_accessor_property(
        &mut self,
        object: ObjectId,
        name: &str,
        getter: Option<Value>,
        setter: Option<Value>,
    ) {
        self.set_own_property_data(object, name, PropertyData::Accessor { getter, setter });
    }

    /// Define a getter/setter pair backed by host functions.
    ///
    /// Reading the property calls `getter(runtime, receiver, vec![])`; assigning
    /// calls `setter(runtime, receiver, vec![value])` and discards its result.
    /// The receiver is the object the property was read from, which may be an
    /// object that inherits the accessor from its prototype. Getters and setters
    /// are traced by the garbage collector as part of the owning object, and a
    /// later data property with the same name replaces the accessor.
    pub fn define_accessor(
        &mut self,
        object: Value,
        name: &str,
        getter: HostFunction,
        setter: HostFunction,
    ) {
        let Value::Object(id) = object else {
            return;
        };
        let mark = self.temp_roots.len();
        let getter = self.new_native_value(&format!("get {name}"), 0, getter);
        self.temp_roots.push(getter.clone());
        let setter = self.new_native_value(&format!("set {name}"), 1, setter);
        self.temp_roots.truncate(mark);
        self.define_accessor_property(id, name, Some(getter), Some(setter));
    }

    /// Define a builtin method on an object.
    pub(crate) fn define_builtin_method(
        &mut self,
        object: ObjectId,
        name: &str,
        length: usize,
        f: BuiltinFn,
    ) {
        let function = self.new_builtin_value(name, length, f);
        self.define_data_property(object, name, function);
    }

    /// Define a method backed by a host closure on a value.
    pub fn define_host_method(&mut self, object: Value, name: &str, f: HostFunction) {
        if let Value::Object(id) = object {
            let function = self.new_native_value(name, 0, f);
            self.define_data_property(id, name, function);
        }
    }

    pub(crate) fn has_own_property(&self, id: ObjectId, key: &str) -> bool {
        match self.object(id) {
            Some(object) => match &object.kind {
                ObjectKind::Array(items) => {
                    key == "length"
                        || array_index(key).is_some_and(|index| index < items.len())
                        || object.properties.iter().any(|(name, _)| name == key)
                }
                ObjectKind::Function(_) => {
                    let overridden = object.properties.iter().any(|(name, _)| name == key);
                    ((key == "length" || key == "name") && !overridden)
                        || object.properties.iter().any(|(name, _)| name == key)
                }
                ObjectKind::Ordinary
                | ObjectKind::Promise(_)
                | ObjectKind::RegExp(_)
                | ObjectKind::Symbol(_)
                | ObjectKind::Generator(_)
                | ObjectKind::Proxy { .. }
                | ObjectKind::WeakRef { .. }
                | ObjectKind::FinalizationRegistry(_) => {
                    object.properties.iter().any(|(name, _)| name == key)
                }
            },
            None => false,
        }
    }

    /// Look up an own property descriptor, resolving the built-in array and
    /// function data properties. Returns `None` when the object does not have
    /// the property as an own property.
    pub(crate) fn own_property_descriptor(&self, id: ObjectId, key: &str) -> Option<PropertyData> {
        let object = self.object(id)?;
        match &object.kind {
            ObjectKind::Array(items) => {
                if key == "length" {
                    return Some(PropertyData::Data(Value::Number(items.len() as f64)));
                }
                if let Some(index) = array_index(key) {
                    return Some(PropertyData::Data(
                        items.get(index).cloned().unwrap_or(Value::Undefined),
                    ));
                }
            }
            ObjectKind::Function(function) => {
                let overridden = object.properties.iter().any(|(name, _)| name == key);
                if !overridden {
                    if key == "length" {
                        return Some(PropertyData::Data(Value::Number(function.length as f64)));
                    }
                    if key == "name" {
                        return Some(PropertyData::Data(Value::String(function.name.clone())));
                    }
                }
            }
            ObjectKind::Ordinary
            | ObjectKind::Promise(_)
            | ObjectKind::RegExp(_)
            | ObjectKind::Symbol(_)
            | ObjectKind::Generator(_)
            | ObjectKind::Proxy { .. }
            | ObjectKind::WeakRef { .. }
            | ObjectKind::FinalizationRegistry(_) => {}
        }
        object
            .properties
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, data)| data.clone())
    }

    /// Read an own data property. Accessor properties yield `None`.
    pub(crate) fn get_own_property(&self, id: ObjectId, key: &str) -> Option<Value> {
        match self.own_property_descriptor(id, key)? {
            PropertyData::Data(value) => Some(value),
            PropertyData::Accessor { .. } => None,
        }
    }

    /// Read a property starting at `start`, invoking getters with `receiver`.
    pub(crate) fn lookup_property(
        &mut self,
        receiver: &Value,
        start: ObjectId,
        key: &str,
    ) -> Result<Option<Value>, JsError> {
        let mut current = Some(start);
        while let Some(id) = current {
            if let Some(descriptor) = self.own_property_descriptor(id, key) {
                return match descriptor {
                    PropertyData::Data(value) => Ok(Some(value)),
                    PropertyData::Accessor { getter, .. } => {
                        let Some(getter) = getter else {
                            return Ok(Some(Value::Undefined));
                        };
                        if !self.is_callable(&getter) {
                            return Ok(Some(Value::Undefined));
                        }
                        Ok(Some(self.call_function(
                            &getter,
                            receiver.clone(),
                            Vec::new(),
                        )?))
                    }
                };
            }
            current = self.object(id).and_then(|object| object.prototype);
        }
        Ok(None)
    }

    /// Read a property, raising `TypeError` for null or undefined receivers.
    pub(crate) fn get_property_checked(
        &mut self,
        value: &Value,
        key: &str,
    ) -> Result<Value, JsError> {
        if let Some((target, handler)) = self.as_proxy(value) {
            if let Some(trap) = self.proxy_trap(&handler, "get") {
                return self.call_function(
                    &trap,
                    handler,
                    vec![target, Value::String(key.to_owned()), value.clone()],
                );
            }
            return self.get_property_checked(&target, key);
        }
        match value {
            Value::Undefined | Value::Null => Err(JsError::type_error(format!(
                "cannot read property '{key}' of {}",
                value.to_string_value()
            ))),
            Value::String(text) => {
                if key == "length" {
                    return Ok(Value::Number(text.chars().count() as f64));
                }
                if let Some(index) = array_index(key) {
                    return Ok(match text.chars().nth(index) {
                        Some(ch) => Value::String(ch.to_string()),
                        None => Value::Undefined,
                    });
                }
                let receiver = Value::String(text.clone());
                Ok(self
                    .lookup_property(&receiver, self.prototypes.string, key)?
                    .unwrap_or(Value::Undefined))
            }
            Value::Number(_) => Ok(self
                .lookup_property(value, self.prototypes.number, key)?
                .unwrap_or(Value::Undefined)),
            Value::Bool(_) => Ok(self
                .lookup_property(value, self.prototypes.boolean, key)?
                .unwrap_or(Value::Undefined)),
            Value::Object(id) => Ok(self
                .lookup_property(value, *id, key)?
                .unwrap_or(Value::Undefined)),
        }
    }

    /// Read a property. Missing properties and invalid receivers yield `undefined`.
    pub fn get_property(&mut self, object: &Value, key: &str) -> Value {
        self.get_property_checked(object, key)
            .unwrap_or(Value::Undefined)
    }

    /// Write a property, raising `TypeError` for null or undefined receivers.
    pub(crate) fn set_property_checked(
        &mut self,
        value: &Value,
        key: &str,
        new_value: Value,
    ) -> Result<(), JsError> {
        if let Some((target, handler)) = self.as_proxy(value) {
            if let Some(trap) = self.proxy_trap(&handler, "set") {
                let _ = self.call_function(
                    &trap,
                    handler,
                    vec![
                        target,
                        Value::String(key.to_owned()),
                        new_value,
                        value.clone(),
                    ],
                )?;
                return Ok(());
            }
            return self.set_property_checked(&target, key, new_value);
        }
        let Value::Object(id) = value else {
            if matches!(value, Value::Undefined | Value::Null) {
                return Err(JsError::type_error(format!(
                    "cannot set property '{key}' of {}",
                    value.to_string_value()
                )));
            }
            // Assignments to primitive properties are silently ignored in sloppy mode.
            return Ok(());
        };
        let id = *id;
        let is_array = self
            .object(id)
            .is_some_and(|object| matches!(&object.kind, ObjectKind::Array(_)));
        if is_array {
            if key == "length" {
                let length = new_value.to_number();
                if length.is_nan() || length < 0.0 || length.fract() != 0.0 {
                    return Err(JsError::range_error("invalid array length"));
                }
                let length = length as usize;
                if let Some(Object {
                    kind: ObjectKind::Array(items),
                    ..
                }) = self.object_mut(id)
                {
                    items.resize(length, Value::Undefined);
                }
                return Ok(());
            }
            if let Some(index) = array_index(key) {
                if let Some(Object {
                    kind: ObjectKind::Array(items),
                    ..
                }) = self.object_mut(id)
                {
                    if index >= items.len() {
                        items.resize(index + 1, Value::Undefined);
                    }
                    items[index] = new_value;
                }
                return Ok(());
            }
        }
        // Walk the prototype chain looking for an accessor. The setter runs
        // with the original receiver as `this`, not the prototype.
        let mut current = Some(id);
        'chain: while let Some(cursor) = current {
            let accessor = match self
                .object(cursor)
                .and_then(|object| object.properties.iter().find(|(name, _)| name == key))
            {
                Some((_, PropertyData::Accessor { setter, .. })) => Some(setter.clone()),
                // A data property (own or inherited) is shadowed by a new own
                // data property on the receiver.
                Some((_, PropertyData::Data(_))) => break 'chain,
                None => None,
            };
            if let Some(setter) = accessor {
                let Some(setter) = setter else {
                    // Accessor without a setter: ignore the write (sloppy mode).
                    return Ok(());
                };
                if !self.is_callable(&setter) {
                    return Ok(());
                }
                self.call_function(&setter, Value::Object(id), vec![new_value])?;
                return Ok(());
            }
            current = self.object(cursor).and_then(|object| object.prototype);
        }
        let extensible = self.object(id).is_some_and(|object| object.extensible);
        let exists = self.has_own_property(id, key);
        if !extensible && !exists {
            return Ok(());
        }
        self.define_data_property(id, key, new_value);
        Ok(())
    }

    /// Write a property, ignoring type errors.
    pub fn set_property(&mut self, object: Value, key: &str, value: Value) {
        let _ = self.set_property_checked(&object, key, value);
    }

    /// Delete an own property. Returns whether the delete succeeded.
    ///
    /// Deleting an accessor removes the whole getter/setter pair (property
    /// attributes are not modelled); deleting an array index leaves a hole
    /// filled with `undefined` and deleting `length` fails.
    pub fn delete_property(&mut self, object: Value, key: &str) -> bool {
        if let Some((target, handler)) = self.as_proxy(&object) {
            if let Some(trap) = self.proxy_trap(&handler, "deleteProperty") {
                return self
                    .call_function(&trap, handler, vec![target, Value::String(key.to_owned())])
                    .map(|value| value.is_truthy())
                    .unwrap_or(false);
            }
            return self.delete_property(target, key);
        }
        let Value::Object(id) = object else {
            return true;
        };
        let is_array = self
            .object(id)
            .is_some_and(|object| matches!(&object.kind, ObjectKind::Array(_)));
        if is_array {
            if key == "length" {
                return false;
            }
            if let Some(index) = array_index(key)
                && let Some(Object {
                    kind: ObjectKind::Array(items),
                    ..
                }) = self.object_mut(id)
                && index < items.len()
            {
                items[index] = Value::Undefined;
            }
        }
        if let Some(object) = self.object_mut(id)
            && let Some(index) = object.properties.iter().position(|(name, _)| name == key)
        {
            object.properties.remove(index);
        }
        true
    }

    /// Enumerable own property keys, array indices first.
    ///
    /// Accessor property names are included: attributes are not modelled, so
    /// every own property is treated as enumerable.
    pub fn object_keys(&mut self, object: &Value) -> Vec<String> {
        if let Some((target, handler)) = self.as_proxy(object) {
            if let Some(trap) = self.proxy_trap(&handler, "ownKeys") {
                let result = self.call_function(&trap, handler, vec![target]);
                if let Ok(result) = result
                    && let Some(items) = self.object_array_items(&result)
                {
                    let mut keys = Vec::with_capacity(items.len());
                    for item in items {
                        if let Value::Object(symbol) = &item
                            && let Some(Value::String(internal)) =
                                self.get_own_property(*symbol, "\u{0}symbol_key")
                        {
                            keys.push(internal);
                        } else {
                            keys.push(self.to_string_rt(&item).unwrap_or_default());
                        }
                    }
                    return keys;
                }
                return Vec::new();
            }
            return self.object_keys(&target);
        }
        let Value::Object(id) = object else {
            return Vec::new();
        };
        let Some(heap_object) = self.object(*id) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        if let ObjectKind::Array(items) = &heap_object.kind {
            for index in 0..items.len() {
                keys.push(index.to_string());
            }
        }
        for (name, _) in &heap_object.properties {
            if !keys.iter().any(|existing| existing == name) {
                keys.push(name.clone());
            }
        }
        keys
    }

    /// Whether a key exists on the object or its prototype chain.
    pub fn object_has(&mut self, object: &Value, key: &str) -> bool {
        if let Some((target, handler)) = self.as_proxy(object) {
            if let Some(trap) = self.proxy_trap(&handler, "has") {
                return self
                    .call_function(&trap, handler, vec![target, Value::String(key.to_owned())])
                    .map(|value| value.is_truthy())
                    .unwrap_or(false);
            }
            return self.object_has(&target, key);
        }
        let Value::Object(id) = object else {
            return false;
        };
        let mut current = Some(*id);
        while let Some(id) = current {
            if self.has_own_property(id, key) {
                return true;
            }
            current = self.object(id).and_then(|object| object.prototype);
        }
        false
    }

    /// Whether a value is an array.
    pub fn object_is_array(&mut self, object: &Value) -> bool {
        match object {
            Value::Object(id) => self
                .object(*id)
                .is_some_and(|object| matches!(&object.kind, ObjectKind::Array(_))),
            _ => false,
        }
    }

    /// Snapshot the elements of an array.
    pub fn object_array_items(&mut self, object: &Value) -> Option<Vec<Value>> {
        let Value::Object(id) = object else {
            return None;
        };
        match self.object(*id).map(|object| &object.kind) {
            Some(ObjectKind::Array(items)) => Some(items.clone()),
            _ => None,
        }
    }

    /// Replace the elements of an array.
    pub(crate) fn set_array_items(&mut self, value: &Value, items: Vec<Value>) {
        if let Value::Object(id) = value
            && let Some(Object {
                kind: ObjectKind::Array(existing),
                ..
            }) = self.object_mut(*id)
        {
            *existing = items;
        }
    }

    /// The prototype object of a value, when it has one.
    pub(crate) fn prototype_of(&self, value: &Value) -> Option<ObjectId> {
        match value {
            Value::Object(id) => self.object(*id).and_then(|object| object.prototype),
            Value::String(_) => Some(self.prototypes.string),
            Value::Number(_) => Some(self.prototypes.number),
            Value::Bool(_) => Some(self.prototypes.boolean),
            Value::Undefined | Value::Null => None,
        }
    }

    /// Create an array prototype-less object shell used during bootstrap.
    pub(crate) fn bootstrap_object(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        self.alloc_object(ObjectKind::Ordinary, prototype)
    }

    /// Allocate a symbol value with the given description.
    pub(crate) fn new_symbol(&mut self, description: &str) -> Value {
        let prototype = self.prototypes.object;
        let id = self.alloc_object(ObjectKind::Symbol(description.to_owned()), Some(prototype));
        let key = if description == "Symbol.iterator" {
            SYMBOL_ITERATOR_KEY.to_owned()
        } else if description == "Symbol.asyncIterator" {
            SYMBOL_ASYNC_ITERATOR_KEY.to_owned()
        } else {
            format!("\u{0}Symbol({description})")
        };
        self.define_data_property(id, "\u{0}symbol_key", Value::String(key));
        Value::Object(id)
    }

    /// The target and handler of a proxy object, when `id` is one.
    pub(crate) fn proxy_parts(&self, id: ObjectId) -> Option<(Value, Value)> {
        match self.object(id).map(|object| &object.kind) {
            Some(ObjectKind::Proxy { target, handler }) => Some((target.clone(), handler.clone())),
            _ => None,
        }
    }

    /// The target and handler of a proxy value, when it is one.
    pub(crate) fn as_proxy(&self, value: &Value) -> Option<(Value, Value)> {
        match value {
            Value::Object(id) => self.proxy_parts(*id),
            _ => None,
        }
    }

    /// Look up a callable trap on a proxy handler. Missing or non-callable
    /// properties mean the operation falls back to the target.
    pub(crate) fn proxy_trap(&mut self, handler: &Value, name: &str) -> Option<Value> {
        let trap = self.get_property(handler, name);
        self.is_callable(&trap).then_some(trap)
    }

    /// The generator frame captured by a generator object, if any.
    pub(crate) fn generator_frame_of(&self, value: &Value) -> Option<Rc<RefCell<GeneratorFrame>>> {
        let Value::Object(id) = value else {
            return None;
        };
        match self.object(*id).map(|object| &object.kind) {
            Some(ObjectKind::Generator(frame)) => Some(Rc::clone(frame)),
            _ => None,
        }
    }

    /// Wrap a value in an iterator result object (`{ value, done }`).
    pub(crate) fn new_iterator_result(&mut self, value: Value, done: bool) -> Value {
        let object = self.new_object();
        self.set_property(object.clone(), "value", value);
        self.set_property(object.clone(), "done", Value::Bool(done));
        object
    }

    /// Build an iterator over a pre-computed item sequence.
    pub(crate) fn new_array_iterator(&mut self, items: Vec<Value>) -> Value {
        let index = Rc::new(RefCell::new(0usize));
        let captured = Rc::clone(&index);
        let values = Rc::new(items);
        let next_values = Rc::clone(&values);
        let next = self.new_native_value(
            "next",
            0,
            Box::new(move |runtime, _this, _args| {
                let position = *captured.borrow();
                if position >= next_values.len() {
                    return Ok(runtime.new_iterator_result(Value::Undefined, true));
                }
                *captured.borrow_mut() = position + 1;
                Ok(runtime.new_iterator_result(next_values[position].clone(), false))
            }),
        );
        let iterator = self.new_object();
        self.set_property(iterator.clone(), "next", next);
        if let Value::Object(iterator_id) = iterator {
            let self_iterator = self.new_builtin_value(
                "[Symbol.iterator]",
                0,
                crate::builtins::generator_self_iterator,
            );
            self.define_data_property(iterator_id, SYMBOL_ITERATOR_KEY, self_iterator);
            Value::Object(iterator_id)
        } else {
            iterator
        }
    }
}

/// Parse a canonical array index. Returns `None` for non-index keys.
pub(crate) fn array_index(key: &str) -> Option<usize> {
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    key.parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_index_recognises_canonical_indices() {
        assert_eq!(array_index("0"), Some(0));
        assert_eq!(array_index("12"), Some(12));
        assert_eq!(array_index("01"), None);
        assert_eq!(array_index("-1"), None);
        assert_eq!(array_index("length"), None);
        assert_eq!(array_index("1.5"), None);
    }

    #[test]
    fn arrays_grow_and_truncate_with_length() {
        let mut runtime = crate::Runtime::new();
        let array = runtime.new_array(vec![Value::Number(1.0)]);
        runtime.set_property(array.clone(), "3", Value::Number(4.0));
        assert_eq!(runtime.get_property(&array, "length"), Value::Number(4.0));
        assert_eq!(runtime.get_property(&array, "2"), Value::Undefined);
        runtime.set_property(array.clone(), "length", Value::Number(1.0));
        assert_eq!(runtime.get_property(&array, "length"), Value::Number(1.0));
    }

    #[test]
    fn reading_property_of_undefined_errors_internally() {
        let mut runtime = crate::Runtime::new();
        assert!(
            runtime
                .get_property_checked(&Value::Undefined, "x")
                .is_err()
        );
        assert_eq!(
            runtime.get_property(&Value::Undefined, "x"),
            Value::Undefined
        );
    }

    #[test]
    fn prototype_chain_lookup() {
        let mut runtime = crate::Runtime::new();
        let child = runtime.new_object();
        let parent = runtime.new_object();
        runtime.set_property(parent.clone(), "inherited", Value::Number(7.0));
        if let (Value::Object(child_id), Value::Object(parent_id)) = (&child, &parent) {
            runtime.object_mut(*child_id).expect("child").prototype = Some(*parent_id);
        }
        assert_eq!(
            runtime.get_property(&child, "inherited"),
            Value::Number(7.0)
        );
        assert!(runtime.object_has(&child, "inherited"));
    }

    #[test]
    fn string_length_and_indexing() {
        let mut runtime = crate::Runtime::new();
        let value = Value::String("abc".to_owned());
        assert_eq!(runtime.get_property(&value, "length"), Value::Number(3.0));
        assert_eq!(
            runtime.get_property(&value, "1"),
            Value::String("b".to_owned())
        );
    }
}
