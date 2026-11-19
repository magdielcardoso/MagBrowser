//! The `indexedDB` global, backed by [`browser_navigation::IdbDatabase`].
//!
//! Opening a database returns a request object that is also a promise: scripts
//! may use `.then()` or the `onsuccess` / `onupgradeneeded` event handlers.
//! Object-store and transaction methods return promises for their results.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use browser_js::{JsError, Runtime, Value};
use browser_navigation::{
    IdbBackend, IdbDatabase, IdbError, IdbKey, IdbTransactionMode, IdbValue, MemoryIdbBackend,
};

use crate::{HostState, deferred};

/// Databases opened through the `indexedDB` global, keyed by name.
#[derive(Default)]
pub struct IdbHost {
    /// The concrete databases.
    pub databases: HashMap<String, IdbDatabase<MemoryIdbBackend>>,
}

impl IdbHost {
    /// Creates an empty host.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Host-side record of a JavaScript `IDBTransaction`.
pub struct IdbTransactionHost {
    /// Database the transaction belongs to.
    pub db_name: String,
    /// Access mode.
    pub mode: IdbTransactionMode,
    /// Stores in scope.
    pub stores: Vec<String>,
    /// Writes buffered until commit. `None` represents a deletion.
    pub pending: Vec<(String, IdbKey, Option<IdbValue>)>,
    /// Whether the transaction still accepts operations.
    pub active: bool,
}

/// Installs the `indexedDB` global.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let factory = runtime.new_object();
    runtime.set_global("indexedDB", factory.clone());

    runtime.define_host_method(factory.clone(), "open", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("indexedDB.open needs a name"))?;
            let version = args
                .get(1)
                .map(|value| value.to_number().max(1.0) as u32)
                .unwrap_or(1);
            Ok(open(runtime, &state, &name, version))
        })
    });

    runtime.define_host_method(factory.clone(), "deleteDatabase", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("deleteDatabase needs a name"))?;
            state.borrow_mut().indexed_db.databases.remove(&name);
            Ok(deferred::resolved(runtime, Value::Undefined))
        })
    });

    // Constructor shims so feature checks do not throw.
    for name in [
        "IDBDatabase",
        "IDBObjectStore",
        "IDBTransaction",
        "IDBRequest",
    ] {
        runtime.define_global_function(
            name,
            Box::new(|runtime, this, _args| {
                Ok(if matches!(this, Value::Object(_)) {
                    this
                } else {
                    runtime.new_object()
                })
            }),
        );
    }
}

fn open(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, name: &str, version: u32) -> Value {
    let is_new = !state.borrow().indexed_db.databases.contains_key(name);
    if is_new {
        state
            .borrow_mut()
            .indexed_db
            .databases
            .insert(name.to_owned(), IdbDatabase::open(name, version));
    }

    let db_wrapper = make_database(runtime, state, name);
    let pending = deferred::new_deferred(runtime);
    let request = pending.promise.clone();
    runtime.set_property(request.clone(), "result", db_wrapper.clone());
    runtime.set_property(
        request.clone(),
        "readyState",
        Value::String("done".to_owned()),
    );

    let captured_request = request.clone();
    let captured_db = db_wrapper;
    runtime.enqueue_microtask(Box::new(move |runtime, _this, _args| {
        if is_new {
            let event = runtime.new_object();
            runtime.set_property(
                event.clone(),
                "type",
                Value::String("upgradeneeded".to_owned()),
            );
            runtime.set_property(event.clone(), "target", captured_request.clone());
            runtime.set_property(event.clone(), "oldVersion", Value::Number(0.0));
            runtime.set_property(
                event.clone(),
                "newVersion",
                Value::Number(f64::from(version)),
            );
            let handler = runtime.get_property(&captured_request, "onupgradeneeded");
            if matches!(handler, Value::Object(_)) {
                let _ = runtime.call_function(&handler, captured_request.clone(), vec![event]);
            }
        }
        let event = runtime.new_object();
        runtime.set_property(event.clone(), "type", Value::String("success".to_owned()));
        runtime.set_property(event.clone(), "target", captured_request.clone());
        let handler = runtime.get_property(&captured_request, "onsuccess");
        if matches!(handler, Value::Object(_)) {
            let _ = runtime.call_function(&handler, captured_request.clone(), vec![event]);
        }
        pending.fulfil(runtime, captured_db.clone());
        Ok(Value::Undefined)
    }));
    request
}

fn make_database(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, name: &str) -> Value {
    let object = runtime.new_object();
    runtime.set_property(object.clone(), "__dbName", Value::String(name.to_owned()));
    runtime.set_property(object.clone(), "name", Value::String(name.to_owned()));
    let version = state
        .borrow()
        .indexed_db
        .databases
        .get(name)
        .map_or(0, IdbDatabase::version);
    runtime.set_property(object.clone(), "version", Value::Number(f64::from(version)));
    refresh_store_names(runtime, state, &object, name);

    runtime.define_host_method(object.clone(), "createObjectStore", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let db_name = database_name(runtime, &this);
            let store = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("createObjectStore needs a name"))?;
            let result = {
                let mut borrowed = state.borrow_mut();
                match borrowed.indexed_db.databases.get_mut(&db_name) {
                    Some(database) => database.create_object_store(&store),
                    None => Err(IdbError::DatabaseClosed),
                }
            };
            if let Err(error) = result {
                return Err(JsError::new(error.to_string()));
            }
            refresh_store_names(runtime, &state, &this, &db_name);
            let wrapper = make_store(runtime, &db_name, &store, 0);
            install_store_members(runtime, &state, wrapper.clone());
            Ok(wrapper)
        })
    });

    runtime.define_host_method(object.clone(), "transaction", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let db_name = database_name(runtime, &this);
            let stores = string_list(runtime, args.first().cloned());
            let mode = args
                .get(1)
                .map(Value::to_string_value)
                .unwrap_or_else(|| "readonly".to_owned());
            if stores.is_empty() {
                return Err(JsError::type_error("transaction needs at least one store"));
            }
            let mode = if mode == "readwrite" {
                IdbTransactionMode::ReadWrite
            } else {
                IdbTransactionMode::ReadOnly
            };
            let valid = {
                let mut borrowed = state.borrow_mut();
                let names = stores.iter().map(String::as_str).collect::<Vec<_>>();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .is_some_and(|db| db.transaction(mode, &names).is_ok())
            };
            if !valid {
                return Err(JsError::new("transaction references an unknown store"));
            }
            let mut borrowed = state.borrow_mut();
            let id = borrowed.next_idb_tx;
            borrowed.next_idb_tx += 1;
            borrowed.idb_transactions.insert(
                id,
                IdbTransactionHost {
                    db_name,
                    mode,
                    stores,
                    pending: Vec::new(),
                    active: true,
                },
            );
            drop(borrowed);
            let wrapper = make_transaction(runtime, id);
            install_transaction_members(runtime, &state, wrapper.clone());
            Ok(wrapper)
        })
    });

    runtime.define_host_method(object.clone(), "put", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let db_name = database_name(runtime, &this);
            let (store, key, value) = direct_args(runtime, args, true);
            let result = {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .and_then(|db| db.put(&store, key.clone(), value).ok())
            };
            if result.is_none() {
                return Ok(deferred::rejected(runtime, "IndexedDB put failed"));
            }
            Ok(deferred::resolved(runtime, key_to_js(&key)))
        })
    });

    runtime.define_host_method(object.clone(), "get", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let db_name = database_name(runtime, &this);
            let (store, key, _) = direct_args(runtime, args, false);
            let found = {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .and_then(|db| db.get(&store, &key).ok())
                    .flatten()
            };
            let value = found.map_or(Value::Undefined, |value| js_from_idb_value(runtime, &value));
            Ok(deferred::resolved(runtime, value))
        })
    });

    runtime.define_host_method(object.clone(), "delete", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let db_name = database_name(runtime, &this);
            let (store, key, _) = direct_args(runtime, args, false);
            let result = {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .and_then(|db| db.delete(&store, &key).ok())
            };
            Ok(deferred::resolved(
                runtime,
                Value::Bool(result.unwrap_or(false)),
            ))
        })
    });

    object
}

fn direct_args(
    runtime: &mut Runtime,
    args: Vec<Value>,
    with_value: bool,
) -> (String, IdbKey, IdbValue) {
    let store = args.first().map(Value::to_string_value).unwrap_or_default();
    let key = key_from_value(args.get(1).cloned().unwrap_or(Value::Undefined));
    let value = if with_value {
        idb_value_from_js(runtime, args.get(2).cloned().unwrap_or(Value::Undefined))
    } else {
        IdbValue::Null
    };
    (store, key, value)
}

fn refresh_store_names(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    object: &Value,
    name: &str,
) {
    let names = state
        .borrow()
        .indexed_db
        .databases
        .get(name)
        .map_or_else(Vec::new, IdbDatabase::object_store_names);
    let items = names.into_iter().map(Value::String).collect();
    let array = runtime.new_array(items);
    runtime.set_property(object.clone(), "objectStoreNames", array);
}

fn database_name(runtime: &mut Runtime, value: &Value) -> String {
    runtime.get_property(value, "__dbName").to_string_value()
}

fn make_store(runtime: &mut Runtime, db_name: &str, store: &str, tx_id: u64) -> Value {
    let object = runtime.new_object();
    runtime.set_property(
        object.clone(),
        "__dbName",
        Value::String(db_name.to_owned()),
    );
    runtime.set_property(
        object.clone(),
        "__storeName",
        Value::String(store.to_owned()),
    );
    runtime.set_property(object.clone(), "__txId", Value::Number(tx_id as f64));
    runtime.set_property(object.clone(), "name", Value::String(store.to_owned()));
    object
}

fn make_transaction(runtime: &mut Runtime, id: u64) -> Value {
    let object = runtime.new_object();
    runtime.set_property(object.clone(), "__txId", Value::Number(id as f64));
    object
}

fn string_list(runtime: &mut Runtime, value: Option<Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Value::String(text) = &value {
        return vec![text.clone()];
    }
    let length = match runtime.get_property(&value, "length") {
        Value::Number(n) => n as usize,
        _ => 0,
    };
    (0..length)
        .map(|index| {
            runtime
                .get_property(&value, &index.to_string())
                .to_string_value()
        })
        .collect()
}

fn key_from_value(value: Value) -> IdbKey {
    match value {
        Value::Number(number) => IdbKey::number(number as i64),
        Value::String(text) => IdbKey::string(text),
        other => IdbKey::string(other.to_string_value()),
    }
}

fn key_to_js(key: &IdbKey) -> Value {
    match key {
        IdbKey::Number(number) => Value::Number(*number as f64),
        IdbKey::String(text) => Value::String(text.clone()),
    }
}

fn idb_value_from_js(runtime: &mut Runtime, value: Value) -> IdbValue {
    match value {
        Value::Undefined | Value::Null => IdbValue::Null,
        Value::Bool(flag) => IdbValue::Bool(flag),
        Value::Number(number) => IdbValue::Number(number),
        Value::String(text) => IdbValue::String(text),
        Value::BigInt(_) => IdbValue::String(value.to_string_value()),
        Value::Object(_) => {
            let json = runtime.get_global("JSON");
            let stringify = runtime.get_property(&json, "stringify");
            match runtime.call_function(&stringify, Value::Undefined, vec![value]) {
                Ok(Value::String(text)) => IdbValue::String(text),
                Ok(other) => IdbValue::String(other.to_string_value()),
                Err(_) => IdbValue::Null,
            }
        }
    }
}

fn js_from_idb_value(runtime: &mut Runtime, value: &IdbValue) -> Value {
    match value {
        IdbValue::Null => Value::Null,
        IdbValue::Bool(flag) => Value::Bool(*flag),
        IdbValue::Number(number) => Value::Number(*number),
        IdbValue::String(text) => Value::String(text.clone()),
        IdbValue::Bytes(bytes) => {
            let items = bytes
                .iter()
                .map(|byte| Value::Number(f64::from(*byte)))
                .collect();
            let object = runtime.new_object();
            runtime.set_property(
                object.clone(),
                "byteLength",
                Value::Number(bytes.len() as f64),
            );
            let array = runtime.new_array(items);
            runtime.set_property(object.clone(), "bytes", array);
            object
        }
    }
}

/// Installs `put`, `get`, `delete` and `clear` on an object-store wrapper.
pub fn install_store_members(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, object: Value) {
    runtime.define_host_method(object.clone(), "put", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let (db_name, store, tx_id) = store_info(runtime, &this);
            let key = key_from_value(args.first().cloned().unwrap_or(Value::Undefined));
            let value =
                idb_value_from_js(runtime, args.get(1).cloned().unwrap_or(Value::Undefined));
            if tx_id != 0 {
                let accepted = {
                    let mut borrowed = state.borrow_mut();
                    match borrowed.idb_transactions.get_mut(&tx_id) {
                        Some(transaction)
                            if transaction.active
                                && transaction.mode == IdbTransactionMode::ReadWrite
                                && transaction.stores.contains(&store) =>
                        {
                            transaction.pending.push((store, key.clone(), Some(value)));
                            true
                        }
                        _ => false,
                    }
                };
                if accepted {
                    return Ok(deferred::resolved(runtime, key_to_js(&key)));
                }
                return Ok(deferred::rejected(
                    runtime,
                    "transaction cannot accept a write",
                ));
            }
            let result = {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .and_then(|db| db.put(&store, key.clone(), value).ok())
            };
            if result.is_none() {
                return Ok(deferred::rejected(runtime, "IndexedDB put failed"));
            }
            Ok(deferred::resolved(runtime, key_to_js(&key)))
        })
    });

    runtime.define_host_method(object.clone(), "get", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let (db_name, store, tx_id) = store_info(runtime, &this);
            let key = key_from_value(args.first().cloned().unwrap_or(Value::Undefined));
            let found = if tx_id != 0 {
                let borrowed = state.borrow();
                match borrowed.idb_transactions.get(&tx_id) {
                    Some(transaction) if transaction.active => pending_or_backend(
                        &borrowed.indexed_db.databases,
                        &db_name,
                        transaction,
                        &store,
                        &key,
                    ),
                    _ => None,
                }
            } else {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .and_then(|db| db.get(&store, &key).ok())
                    .flatten()
            };
            let value = found.map_or(Value::Undefined, |value| js_from_idb_value(runtime, &value));
            Ok(deferred::resolved(runtime, value))
        })
    });

    runtime.define_host_method(object.clone(), "delete", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let (db_name, store, tx_id) = store_info(runtime, &this);
            let key = key_from_value(args.first().cloned().unwrap_or(Value::Undefined));
            if tx_id != 0 {
                let accepted = {
                    let mut borrowed = state.borrow_mut();
                    match borrowed.idb_transactions.get_mut(&tx_id) {
                        Some(transaction)
                            if transaction.active
                                && transaction.mode == IdbTransactionMode::ReadWrite
                                && transaction.stores.contains(&store) =>
                        {
                            transaction.pending.push((store, key, None));
                            true
                        }
                        _ => false,
                    }
                };
                return Ok(deferred::resolved(runtime, Value::Bool(accepted)));
            }
            let result = {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .and_then(|db| db.delete(&store, &key).ok())
            };
            Ok(deferred::resolved(
                runtime,
                Value::Bool(result.unwrap_or(false)),
            ))
        })
    });

    runtime.define_host_method(object, "clear", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let (db_name, store, tx_id) = store_info(runtime, &this);
            if tx_id != 0 {
                let accepted = {
                    let mut borrowed = state.borrow_mut();
                    match borrowed.idb_transactions.get_mut(&tx_id) {
                        Some(transaction) if transaction.active => {
                            transaction.pending.retain(|(name, _, _)| name != &store);
                            true
                        }
                        _ => false,
                    }
                };
                return Ok(deferred::resolved(runtime, Value::Bool(accepted)));
            }
            let result = {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .indexed_db
                    .databases
                    .get_mut(&db_name)
                    .and_then(|db| db.backend_mut().clear(&store).ok())
            };
            Ok(deferred::resolved(runtime, Value::Bool(result.is_some())))
        })
    });
}

fn pending_or_backend(
    databases: &HashMap<String, IdbDatabase<MemoryIdbBackend>>,
    db_name: &str,
    transaction: &IdbTransactionHost,
    store: &str,
    key: &IdbKey,
) -> Option<IdbValue> {
    if !transaction.stores.iter().any(|name| name == store) {
        return None;
    }
    if let Some(write) = transaction
        .pending
        .iter()
        .rev()
        .find(|(name, candidate, _)| name == store && candidate == key)
    {
        return write.2.clone();
    }
    databases
        .get(db_name)
        .and_then(|db| db.backend().get(store, key).ok().flatten())
}

fn store_info(runtime: &mut Runtime, value: &Value) -> (String, String, u64) {
    let db_name = runtime.get_property(value, "__dbName").to_string_value();
    let store = runtime.get_property(value, "__storeName").to_string_value();
    let tx_id = match runtime.get_property(value, "__txId") {
        Value::Number(n) if n >= 0.0 => n as u64,
        _ => 0,
    };
    (db_name, store, tx_id)
}

/// Installs the transaction methods on a transaction wrapper.
pub fn install_transaction_members(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    object: Value,
) {
    runtime.define_host_method(object.clone(), "objectStore", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let tx_id = match runtime.get_property(&this, "__txId") {
                Value::Number(n) => n as u64,
                _ => 0,
            };
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("objectStore needs a name"))?;
            let db_name = {
                let borrowed = state.borrow();
                borrowed
                    .idb_transactions
                    .get(&tx_id)
                    .map_or_else(String::new, |transaction| transaction.db_name.clone())
            };
            let store = make_store(runtime, &db_name, &name, tx_id);
            install_store_members(runtime, &state, store.clone());
            Ok(store)
        })
    });

    runtime.define_host_method(object.clone(), "commit", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let tx_id = match runtime.get_property(&this, "__txId") {
                Value::Number(n) => n as u64,
                _ => 0,
            };
            let Some(transaction) = state.borrow_mut().idb_transactions.remove(&tx_id) else {
                return Ok(deferred::rejected(
                    runtime,
                    "transaction is no longer active",
                ));
            };
            match apply_transaction(&state, &transaction) {
                Ok(()) => {
                    let event = runtime.new_object();
                    runtime.set_property(
                        event.clone(),
                        "type",
                        Value::String("complete".to_owned()),
                    );
                    let handler = runtime.get_property(&this, "oncomplete");
                    if matches!(handler, Value::Object(_)) {
                        let _ = runtime.call_function(&handler, this.clone(), vec![event]);
                    }
                    Ok(deferred::resolved(runtime, Value::Undefined))
                }
                Err(message) => Ok(deferred::rejected(runtime, &message)),
            }
        })
    });

    runtime.define_host_method(object, "abort", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let tx_id = match runtime.get_property(&this, "__txId") {
                Value::Number(n) => n as u64,
                _ => 0,
            };
            state.borrow_mut().idb_transactions.remove(&tx_id);
            Ok(Value::Undefined)
        })
    });
}

fn apply_transaction(
    state: &Rc<RefCell<HostState>>,
    transaction: &IdbTransactionHost,
) -> Result<(), String> {
    let mut borrowed = state.borrow_mut();
    let database = borrowed
        .indexed_db
        .databases
        .get_mut(&transaction.db_name)
        .ok_or_else(|| "database was closed".to_owned())?;
    let stores = transaction
        .stores
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut handle = database
        .transaction(transaction.mode, &stores)
        .map_err(|error| error.to_string())?;
    for (store, key, value) in &transaction.pending {
        let outcome = match value {
            Some(value) => handle.put(store, key.clone(), value.clone()),
            None => handle.delete(store, key).map(|_| ()),
        };
        outcome.map_err(|error| error.to_string())?;
    }
    handle
        .commit()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse_document;

    fn setup() -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document("<html><body></body></html>")));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    #[test]
    fn open_store_put_get_delete() {
        let (mut runtime, _state) = setup();
        runtime
            .eval(
                "var upgraded = false, succeeded = false, db; \
                 var request = indexedDB.open('notes', 1); \
                 request.onupgradeneeded = function () { upgraded = true; }; \
                 request.onsuccess = function () { succeeded = true; }; \
                 request.then(function (database) { db = database; });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert!(matches!(
            runtime.eval("upgraded").unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            runtime.eval("succeeded").unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            runtime.eval("db.name === 'notes'").unwrap(),
            Value::Bool(true)
        ));

        runtime
            .eval(
                "var store = db.createObjectStore('items'); \
                 var out; \
                 store.put('a', 'hello'); \
                 store.get('a').then(function (value) { out = value; });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.eval("out").unwrap().to_string_value(), "hello");
        assert_eq!(
            runtime
                .eval("db.objectStoreNames.join(',')")
                .unwrap()
                .to_string_value(),
            "items"
        );
    }

    #[test]
    fn database_level_helpers_and_delete() {
        let (mut runtime, _state) = setup();
        runtime
            .eval(
                "var db; \
                 indexedDB.open('kv', 1).then(function (database) { db = database; });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        runtime
            .eval(
                "db.createObjectStore('s'); \
                 db.put('s', 1, 42); \
                 var first; \
                 db.get('s', 1).then(function (v) { first = v; });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.eval("first").unwrap().to_number(), 42.0);
        runtime
            .eval(
                "db.put('s', 2, first * 2); \
                 var final; \
                 db.get('s', 2).then(function (v) { final = v; });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.eval("final").unwrap().to_number(), 84.0);
        runtime
            .eval("var removed; db.delete('s',1).then(function(v){removed=v;});")
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert!(matches!(
            runtime.eval("removed").unwrap(),
            Value::Bool(true)
        ));
    }

    #[test]
    fn transaction_buffers_writes_until_commit() {
        let (mut runtime, _state) = setup();
        runtime
            .eval(
                "var db; \
                 indexedDB.open('txdb', 1).then(function (database) { \
                     db = database; \
                     db.createObjectStore('items'); \
                 });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        runtime
            .eval(
                "var tx = db.transaction(['items'], 'readwrite'); \
                 var store = tx.objectStore('items'); \
                 var seen; \
                 store.put('k', 'v'); \
                 store.get('k').then(function (v) { seen = v; }); \
                 tx.commit();",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.eval("seen").unwrap().to_string_value(), "v");
        runtime
            .eval("var after; db.get('items','k').then(function(v){after=v;});")
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert_eq!(runtime.eval("after").unwrap().to_string_value(), "v");
    }

    #[test]
    fn read_only_transaction_rejects_writes() {
        let (mut runtime, _state) = setup();
        runtime
            .eval(
                "var db; indexedDB.open('ro', 1).then(function (d) { db = d; db.createObjectStore('s'); });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        runtime
            .eval(
                "var tx = db.transaction(['s'], 'readonly'); \
                 var ok = false; \
                 tx.objectStore('s').put('a', 1).catch(function () { ok = true; });",
            )
            .unwrap();
        runtime.run_microtasks().unwrap();
        assert!(matches!(runtime.eval("ok").unwrap(), Value::Bool(true)));
    }
}
