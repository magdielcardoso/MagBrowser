//! `URLSearchParams`: query-string parsing, mutation and serialisation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use browser_js::{JsError, Runtime, Value};

/// Shared storage of every `URLSearchParams` wrapper, keyed by object id.
type Store = Rc<RefCell<HashMap<usize, Vec<(String, String)>>>>;

/// Installs the `URLSearchParams` global.
pub fn install(runtime: &mut Runtime) {
    let store: Store = Rc::new(RefCell::new(HashMap::new()));

    runtime.define_global_function("URLSearchParams", {
        let store = store.clone();
        Box::new(move |runtime, this, args| {
            let object = match this {
                Value::Object(_) => this.clone(),
                _ => runtime.new_object(),
            };
            let initial = args.first().cloned().unwrap_or(Value::Undefined);
            let pairs = parse_init(runtime, &initial);
            if let Value::Object(id) = &object {
                store.borrow_mut().insert(id.0, pairs);
            }
            install_members(runtime, &store, &object);
            Ok(object)
        })
    });
}

fn parse_init(runtime: &mut Runtime, value: &Value) -> Vec<(String, String)> {
    match value {
        Value::Undefined | Value::Null => Vec::new(),
        Value::String(text) => parse_query(text),
        Value::Object(_) => {
            let text = to_string(runtime, value);
            if text == "[object Object]" {
                runtime
                    .object_keys(value)
                    .into_iter()
                    .map(|key| {
                        let item = runtime.get_property(value, &key).to_string_value();
                        (key, item)
                    })
                    .collect()
            } else {
                parse_query(&text)
            }
        }
        other => parse_query(&other.to_string_value()),
    }
}

fn parse_query(query: &str) -> Vec<(String, String)> {
    let query = query.trim_start_matches('?').trim_start_matches('#');
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            (decode(name), decode(value))
        })
        .collect()
}

fn decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1]);
                let low = hex_value(bytes[index + 2]);
                if let (Some(high), Some(low)) = (high, low) {
                    out.push((high << 4) | low);
                    index += 3;
                } else {
                    out.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(b'0' + (byte >> 4)));
                out.push(char::from(b'0' + (byte & 0x0f)));
            }
        }
    }
    out
}

fn serialise(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn install_members(runtime: &mut Runtime, store: &Store, object: &Value) {
    runtime.set_property(object.clone(), "__isSearchParams", Value::Bool(true));

    runtime.define_host_method(object.clone(), "get", {
        let store = store.clone();
        Box::new(move |_runtime, this, args| {
            let Some(name) = args.first().map(|value| value.to_string_value()) else {
                return Ok(Value::Null);
            };
            let value = pairs_for(&store, &this)
                .into_iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value);
            Ok(value.map_or(Value::Null, Value::String))
        })
    });

    runtime.define_host_method(object.clone(), "getAll", {
        let store = store.clone();
        Box::new(move |runtime, this, args| {
            let name = args
                .first()
                .map_or(String::new(), |value| value.to_string_value());
            let values: Vec<Value> = pairs_for(&store, &this)
                .into_iter()
                .filter(|(key, _)| *key == name)
                .map(|(_, value)| Value::String(value))
                .collect();
            Ok(runtime.new_array(values))
        })
    });

    runtime.define_host_method(object.clone(), "set", {
        let store = store.clone();
        Box::new(move |_runtime, this, args| {
            if args.len() < 2 {
                return Err(JsError::type_error(
                    "URLSearchParams.set needs a name and value",
                ));
            }
            let name = args[0].to_string_value();
            let value = args[1].to_string_value();
            let pairs = pairs_for(&store, &this);
            let mut updated = Vec::with_capacity(pairs.len() + 1);
            let mut replaced = false;
            for (key, existing) in pairs {
                if key == name {
                    if !replaced {
                        updated.push((key, value.clone()));
                        replaced = true;
                    }
                } else {
                    updated.push((key, existing));
                }
            }
            if !replaced {
                updated.push((name, value));
            }
            store_pairs(&store, &this, updated);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "append", {
        let store = store.clone();
        Box::new(move |_runtime, this, args| {
            if args.len() < 2 {
                return Err(JsError::type_error(
                    "URLSearchParams.append needs a name and value",
                ));
            }
            let mut pairs = pairs_for(&store, &this);
            pairs.push((args[0].to_string_value(), args[1].to_string_value()));
            store_pairs(&store, &this, pairs);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "has", {
        let store = store.clone();
        Box::new(move |_runtime, this, args| {
            let name = args
                .first()
                .map_or(String::new(), |value| value.to_string_value());
            Ok(Value::Bool(
                pairs_for(&store, &this).iter().any(|(key, _)| *key == name),
            ))
        })
    });

    runtime.define_host_method(object.clone(), "delete", {
        let store = store.clone();
        Box::new(move |_runtime, this, args| {
            let name = args
                .first()
                .map_or(String::new(), |value| value.to_string_value());
            let pairs = pairs_for(&store, &this)
                .into_iter()
                .filter(|(key, _)| *key != name)
                .collect();
            store_pairs(&store, &this, pairs);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "toString", {
        let store = store.clone();
        Box::new(move |_runtime, this, _args| {
            Ok(Value::String(serialise(&pairs_for(&store, &this))))
        })
    });

    runtime.define_host_method(object.clone(), "entries", {
        let store = store.clone();
        Box::new(move |runtime, this, _args| {
            let pairs = pairs_for(&store, &this)
                .into_iter()
                .map(|(name, value)| {
                    runtime.new_array(vec![Value::String(name), Value::String(value)])
                })
                .collect();
            Ok(runtime.new_array(pairs))
        })
    });

    runtime.define_host_method(object.clone(), "forEach", {
        let store = store.clone();
        Box::new(move |runtime, this, args| {
            let Some(callback) = args.first().cloned() else {
                return Ok(Value::Undefined);
            };
            for (name, value) in pairs_for(&store, &this) {
                let _ = runtime.call_function(
                    &callback,
                    Value::Undefined,
                    vec![Value::String(value), Value::String(name), this.clone()],
                );
            }
            Ok(Value::Undefined)
        })
    });
}

fn object_id(this: &Value) -> Option<usize> {
    match this {
        Value::Object(id) => Some(id.0),
        _ => None,
    }
}

fn pairs_for(store: &Store, this: &Value) -> Vec<(String, String)> {
    object_id(this)
        .and_then(|id| store.borrow().get(&id).cloned())
        .unwrap_or_default()
}

fn store_pairs(store: &Store, this: &Value, pairs: Vec<(String, String)>) {
    if let Some(id) = object_id(this) {
        store.borrow_mut().insert(id, pairs);
    }
}

fn to_string(runtime: &mut Runtime, value: &Value) -> String {
    if let Value::Object(_) = value {
        let method = runtime.get_property(value, "toString");
        if matches!(method, Value::Object(_))
            && let Ok(result) = runtime.call_function(&method, value.clone(), Vec::new())
        {
            return result.to_string_value();
        }
    }
    value.to_string_value()
}
