//! `Reflect` and `Proxy`.
//!
//! Proxy traps are dispatched from the engine's property and call operations
//! (see `object.rs` and `function.rs`). This module installs the two globals
//! and implements the reflective operations that are not already exposed as
//! ordinary property access.

use crate::Runtime;
use crate::errors::JsError;
use crate::object::{ObjectKind, PropertyData};
use crate::value::Value;

impl Runtime {
    /// Install `Proxy` and the `Reflect` namespace.
    pub(crate) fn install_reflect(&mut self) {
        let proxy = self.new_builtin_value("Proxy", 2, proxy_constructor);
        self.declare_global("Proxy", proxy);

        let reflect = self.new_object();
        let Value::Object(id) = reflect else {
            return;
        };
        self.define_builtin_method(id, "get", 2, reflect_get);
        self.define_builtin_method(id, "set", 3, reflect_set);
        self.define_builtin_method(id, "has", 2, reflect_has);
        self.define_builtin_method(id, "deleteProperty", 2, reflect_delete_property);
        self.define_builtin_method(id, "ownKeys", 1, reflect_own_keys);
        self.define_builtin_method(
            id,
            "getOwnPropertyDescriptor",
            2,
            reflect_get_own_property_descriptor,
        );
        self.define_builtin_method(id, "defineProperty", 3, reflect_define_property);
        self.define_builtin_method(id, "apply", 3, reflect_apply);
        self.define_builtin_method(id, "construct", 2, reflect_construct);
        self.define_builtin_method(id, "getPrototypeOf", 1, reflect_get_prototype_of);
        self.define_builtin_method(id, "setPrototypeOf", 2, reflect_set_prototype_of);
        self.define_builtin_method(id, "isExtensible", 1, reflect_is_extensible);
        self.define_builtin_method(id, "preventExtensions", 1, reflect_prevent_extensions);
        self.declare_global("Reflect", reflect);
    }
}

fn proxy_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
    if !matches!(target, Value::Object(_)) || !matches!(handler, Value::Object(_)) {
        return Err(JsError::type_error(
            "Proxy target and handler must be objects",
        ));
    }
    let id = runtime.alloc_object(
        ObjectKind::Proxy { target, handler },
        Some(runtime.prototypes.object),
    );
    Ok(Value::Object(id))
}

/// Convert a property-key argument to the internal string key. Well-known
/// symbols carry a private `\0` prefixed key which is preserved here.
fn key_from(runtime: &mut Runtime, value: &Value) -> Result<String, JsError> {
    if let Value::Object(id) = value
        && let Some(Value::String(key)) = runtime.get_own_property(*id, "\u{0}symbol_key")
    {
        return Ok(key);
    }
    runtime.to_string_rt(value)
}

fn reflect_get(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let key = key_from(runtime, args.get(1).unwrap_or(&Value::Undefined))?;
    runtime.get_property_checked(&target, &key)
}

fn reflect_set(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    // `Reflect.set(target, propertyKey, value[, receiver])`.
    let key = key_from(runtime, args.get(1).unwrap_or(&Value::Undefined))?;
    let value = args.get(2).cloned().unwrap_or(Value::Undefined);
    Ok(Value::Bool(
        runtime.set_property_checked(&target, &key, value).is_ok(),
    ))
}

fn reflect_has(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let key = key_from(runtime, args.get(1).unwrap_or(&Value::Undefined))?;
    Ok(Value::Bool(runtime.object_has(&target, &key)))
}

fn reflect_delete_property(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let key = key_from(runtime, args.get(1).unwrap_or(&Value::Undefined))?;
    Ok(Value::Bool(runtime.delete_property(target, &key)))
}

fn reflect_own_keys(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let iterator_key = crate::object::SYMBOL_ITERATOR_KEY;
    let async_key = crate::object::SYMBOL_ASYNC_ITERATOR_KEY;
    let keys = runtime
        .object_keys(&target)
        .into_iter()
        .map(|key| {
            if key == iterator_key {
                Value::Object(runtime.symbol_iterator)
            } else if key == async_key {
                Value::Object(runtime.symbol_async_iterator)
            } else {
                Value::String(key)
            }
        })
        .collect();
    Ok(runtime.new_array(keys))
}

fn descriptor_value(runtime: &mut Runtime, descriptor: &PropertyData) -> Value {
    let object = runtime.new_object();
    match descriptor {
        PropertyData::Data(value) => {
            runtime.set_property(object.clone(), "value", value.clone());
            runtime.set_property(object.clone(), "writable", Value::Bool(true));
        }
        PropertyData::Accessor { getter, setter } => {
            runtime.set_property(
                object.clone(),
                "get",
                getter.clone().unwrap_or(Value::Undefined),
            );
            runtime.set_property(
                object.clone(),
                "set",
                setter.clone().unwrap_or(Value::Undefined),
            );
        }
    }
    runtime.set_property(object.clone(), "enumerable", Value::Bool(true));
    runtime.set_property(object.clone(), "configurable", Value::Bool(true));
    object
}

fn reflect_get_own_property_descriptor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let key = key_from(runtime, args.get(1).unwrap_or(&Value::Undefined))?;

    if let Some((object_target, handler)) = runtime.as_proxy(&target)
        && let Some(trap) = runtime.proxy_trap(&handler, "getOwnPropertyDescriptor")
    {
        return runtime.call_function(&trap, handler, vec![object_target, Value::String(key)]);
    }

    let Value::Object(id) = target else {
        return Err(JsError::type_error(
            "Reflect.getOwnPropertyDescriptor called on a primitive",
        ));
    };
    Ok(match runtime.own_property_descriptor(id, &key) {
        Some(descriptor) => descriptor_value(runtime, &descriptor),
        None => Value::Undefined,
    })
}

fn reflect_define_property(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let key = key_from(runtime, args.get(1).unwrap_or(&Value::Undefined))?;
    let descriptor = args.get(2).cloned().unwrap_or(Value::Undefined);

    if let Some((object_target, handler)) = runtime.as_proxy(&target)
        && let Some(trap) = runtime.proxy_trap(&handler, "defineProperty")
    {
        return runtime.call_function(
            &trap,
            handler,
            vec![object_target, Value::String(key), descriptor],
        );
    }

    let Value::Object(id) = target else {
        return Err(JsError::type_error(
            "Reflect.defineProperty called on a primitive",
        ));
    };
    let getter = runtime.get_property(&descriptor, "get");
    let setter = runtime.get_property(&descriptor, "set");
    let has_accessor =
        runtime.object_has(&descriptor, "get") || runtime.object_has(&descriptor, "set");
    if has_accessor {
        runtime.define_accessor_property(
            id,
            &key,
            runtime.is_callable(&getter).then_some(getter),
            runtime.is_callable(&setter).then_some(setter),
        );
    } else {
        let value = runtime.get_property(&descriptor, "value");
        runtime.define_data_property(id, &key, value);
    }
    Ok(Value::Bool(true))
}

fn reflect_apply(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&target) {
        return Err(JsError::type_error("Reflect.apply target is not callable"));
    }
    let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    let list = args.get(2).cloned().unwrap_or(Value::Undefined);
    let arguments = runtime.object_array_items(&list).unwrap_or_default();
    runtime.call_function(&target, this_arg, arguments)
}

fn reflect_construct(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let list = args.get(1).cloned().unwrap_or(Value::Undefined);
    let arguments = runtime.object_array_items(&list).unwrap_or_default();
    let new_target = args
        .get(2)
        .cloned()
        .filter(|value| !matches!(value, Value::Undefined))
        .unwrap_or_else(|| target.clone());
    runtime.construct_value(target, arguments, new_target)
}

fn reflect_get_prototype_of(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    if let Some((object_target, handler)) = runtime.as_proxy(&target)
        && let Some(trap) = runtime.proxy_trap(&handler, "getPrototypeOf")
    {
        return runtime.call_function(&trap, handler, vec![object_target]);
    }
    let Value::Object(id) = target else {
        return Err(JsError::type_error(
            "Reflect.getPrototypeOf called on a primitive",
        ));
    };
    Ok(runtime
        .prototype_of(&Value::Object(id))
        .map(Value::Object)
        .unwrap_or(Value::Null))
}

fn reflect_set_prototype_of(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let prototype = args.get(1).cloned().unwrap_or(Value::Null);
    if let Some((object_target, handler)) = runtime.as_proxy(&target)
        && let Some(trap) = runtime.proxy_trap(&handler, "setPrototypeOf")
    {
        return Ok(runtime
            .call_function(&trap, handler, vec![object_target, prototype])?
            .is_truthy()
            .into());
    }
    let Value::Object(id) = target else {
        return Ok(Value::Bool(false));
    };
    let prototype_id = match prototype {
        Value::Object(prototype) => Some(prototype),
        Value::Null => None,
        _ => return Ok(Value::Bool(false)),
    };
    if let Some(object) = runtime.object_mut(id) {
        object.prototype = prototype_id;
    }
    Ok(Value::Bool(true))
}

fn reflect_is_extensible(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let extensible = match target {
        Value::Object(id) => runtime.object(id).is_some_and(|object| object.extensible),
        _ => false,
    };
    Ok(Value::Bool(extensible))
}

fn reflect_prevent_extensions(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    if let Some((object_target, handler)) = runtime.as_proxy(&target)
        && let Some(trap) = runtime.proxy_trap(&handler, "preventExtensions")
    {
        return runtime.call_function(&trap, handler, vec![object_target]);
    }
    if let Value::Object(id) = target
        && let Some(object) = runtime.object_mut(id)
    {
        object.extensible = false;
    }
    Ok(Value::Bool(true))
}
