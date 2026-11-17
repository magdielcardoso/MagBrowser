//! Tests for host defined accessor properties (getters and setters).

use std::sync::{Arc, Mutex};

use browser_js::{JsError, ObjectId, Runtime, Value};

fn object_id(value: &Value) -> ObjectId {
    match value {
        Value::Object(id) => *id,
        other => panic!("expected object, got {other:?}"),
    }
}

/// Install a getter/setter pair over a shared f64 cell.
fn install_number_accessor(
    runtime: &mut Runtime,
    object: Value,
    name: &str,
    cell: Arc<Mutex<f64>>,
) {
    let getter_cell = Arc::clone(&cell);
    let getter = Box::new(
        move |_runtime: &mut Runtime, _this: Value, _args: Vec<Value>| {
            Ok(Value::Number(*getter_cell.lock().expect("lock")))
        },
    );
    let setter_cell = Arc::clone(&cell);
    let setter = Box::new(
        move |_runtime: &mut Runtime, _this: Value, args: Vec<Value>| {
            *setter_cell.lock().expect("lock") = args.first().map(Value::to_number).unwrap_or(0.0);
            Ok(Value::Undefined)
        },
    );
    runtime.define_accessor(object, name, getter, setter);
}

#[test]
fn getter_returns_value() {
    let mut runtime = Runtime::new();
    let object = runtime.new_object();
    runtime.define_accessor(
        object.clone(),
        "answer",
        Box::new(|_runtime, _this, _args| Ok(Value::Number(42.0))),
        Box::new(|_runtime, _this, _args| Ok(Value::Undefined)),
    );
    assert_eq!(runtime.get_property(&object, "answer"), Value::Number(42.0));
    runtime.set_global("obj", object);
    assert_eq!(
        runtime.eval("obj.answer").expect("eval"),
        Value::Number(42.0)
    );
}

#[test]
fn setter_mutates_state() {
    let mut runtime = Runtime::new();
    let state = Arc::new(Mutex::new(String::new()));
    let object = runtime.new_object();

    let getter_state = Arc::clone(&state);
    let getter = Box::new(
        move |_runtime: &mut Runtime, _this: Value, _args: Vec<Value>| {
            Ok(Value::String(getter_state.lock().expect("lock").clone()))
        },
    );
    let setter_state = Arc::clone(&state);
    let setter = Box::new(
        move |_runtime: &mut Runtime, _this: Value, args: Vec<Value>| {
            *setter_state.lock().expect("lock") =
                args.first().map(Value::to_string_value).unwrap_or_default();
            Ok(Value::Undefined)
        },
    );
    runtime.define_accessor(object.clone(), "text", getter, setter);
    runtime.set_global("node", object);

    runtime
        .eval("node.text = 'hello';")
        .expect("assignment should call the setter");
    assert_eq!(*state.lock().expect("lock"), "hello");
    assert_eq!(
        runtime.eval("node.text").expect("eval"),
        Value::String("hello".into())
    );
}

#[test]
fn getter_and_setter_receive_the_receiver_as_this() {
    let mut runtime = Runtime::new();
    let seen_get: Arc<Mutex<Option<ObjectId>>> = Arc::new(Mutex::new(None));
    let seen_set: Arc<Mutex<Option<ObjectId>>> = Arc::new(Mutex::new(None));
    let object = runtime.new_object();
    let id = object_id(&object);

    let getter_seen = Arc::clone(&seen_get);
    let getter = Box::new(
        move |_runtime: &mut Runtime, this: Value, _args: Vec<Value>| {
            if let Value::Object(found) = this {
                *getter_seen.lock().expect("lock") = Some(found);
            }
            Ok(Value::String("read".into()))
        },
    );
    let setter_seen = Arc::clone(&seen_set);
    let setter = Box::new(
        move |_runtime: &mut Runtime, this: Value, _args: Vec<Value>| {
            if let Value::Object(found) = this {
                *setter_seen.lock().expect("lock") = Some(found);
            }
            Ok(Value::Undefined)
        },
    );
    runtime.define_accessor(object.clone(), "value", getter, setter);
    runtime.set_global("target", object);

    assert_eq!(
        runtime.eval("target.value").expect("eval"),
        Value::String("read".into())
    );
    runtime.eval("target.value = 1;").expect("eval");
    assert_eq!(*seen_get.lock().expect("lock"), Some(id));
    assert_eq!(*seen_set.lock().expect("lock"), Some(id));
}

#[test]
fn compound_assignment_reads_then_writes() {
    let mut runtime = Runtime::new();
    let cell = Arc::new(Mutex::new(1.0));
    let object = runtime.new_object();
    install_number_accessor(&mut runtime, object.clone(), "count", Arc::clone(&cell));
    runtime.set_global("counter", object);

    runtime.eval("counter.count += 5;").expect("eval");
    assert_eq!(*cell.lock().expect("lock"), 6.0);
    assert_eq!(
        runtime.eval("counter.count").expect("eval"),
        Value::Number(6.0)
    );
    runtime.eval("counter.count *= 2;").expect("eval");
    assert_eq!(*cell.lock().expect("lock"), 12.0);
}

#[test]
fn accessor_on_prototype_binds_the_original_receiver() {
    let mut runtime = Runtime::new();
    runtime
        .eval("var proto = {}; var child = Object.create(proto); child.name = 'kid';")
        .expect("eval");
    let proto = runtime.get_global("proto");
    let child = runtime.get_global("child");
    let child_id = object_id(&child);

    runtime.define_accessor(
        proto,
        "greeting",
        Box::new(
            move |runtime: &mut Runtime, this: Value, _args: Vec<Value>| {
                let name = runtime.get_property(&this, "name").to_string_value();
                let receiver_is_child = matches!(this, Value::Object(id) if id == child_id);
                Ok(Value::String(format!("hi {name} {receiver_is_child}")))
            },
        ),
        Box::new(|_runtime, _this, _args| Ok(Value::Undefined)),
    );

    assert_eq!(
        runtime.eval("child.greeting").expect("eval"),
        Value::String("hi kid true".into())
    );
}

#[test]
fn read_only_setter_raises_a_type_error() {
    let mut runtime = Runtime::new();
    runtime.eval("var frozen = {};").expect("eval");
    let object = runtime.get_global("frozen");
    runtime.define_accessor(
        object,
        "constant",
        Box::new(|_runtime, _this, _args| Ok(Value::Number(1.0))),
        Box::new(|_runtime, _this, _args| Err(JsError::type_error("property is read only"))),
    );

    let error = runtime
        .eval("frozen.constant = 2;")
        .expect_err("the setter should throw");
    assert!(error.message.contains("TypeError"), "{}", error.message);
    assert!(error.message.contains("read only"), "{}", error.message);
    assert_eq!(
        runtime.eval("frozen.constant").expect("eval"),
        Value::Number(1.0)
    );
}

#[test]
fn delete_removes_an_accessor() {
    let mut runtime = Runtime::new();
    let object = runtime.new_object();
    runtime.define_accessor(
        object.clone(),
        "removable",
        Box::new(|_runtime, _this, _args| Ok(Value::String("here".into()))),
        Box::new(|_runtime, _this, _args| Ok(Value::Undefined)),
    );
    assert!(runtime.object_has(&object, "removable"));
    assert!(runtime.delete_property(object.clone(), "removable"));
    assert_eq!(runtime.get_property(&object, "removable"), Value::Undefined);
    assert!(!runtime.object_has(&object, "removable"));
    assert!(runtime.delete_property(object.clone(), "removable"));
}

#[test]
fn object_keys_include_accessors_and_later_data_properties_replace_them() {
    let mut runtime = Runtime::new();
    let object = runtime.new_object();
    runtime.set_property(object.clone(), "plain", Value::Number(1.0));
    runtime.define_accessor(
        object.clone(),
        "computed",
        Box::new(|_runtime, _this, _args| Ok(Value::Number(7.0))),
        Box::new(|_runtime, _this, _args| Ok(Value::Undefined)),
    );
    let mut keys = runtime.object_keys(&object);
    keys.sort();
    assert_eq!(keys, vec!["computed".to_owned(), "plain".to_owned()]);

    // Defining a data property with the same name replaces the accessor.
    runtime.define_host_method(
        object.clone(),
        "computed",
        Box::new(|_runtime, _this, _args| Ok(Value::Number(9.0))),
    );
    let replacement = runtime.get_property(&object, "computed");
    assert!(matches!(replacement, Value::Object(_)));
    assert_eq!(
        runtime
            .call_function(&replacement, object.clone(), Vec::new())
            .expect("call"),
        Value::Number(9.0)
    );
    assert_eq!(runtime.get_property(&object, "plain"), Value::Number(1.0));
}

#[test]
fn plain_properties_arrays_and_builtins_still_work() {
    let mut runtime = Runtime::new();
    assert_eq!(
        runtime
            .eval(
                "var array = [1, 2]; array.push(3);
                 var record = { p: 1 }; record.q = 2; record.p = 5;
                 array.length + record.p + record.q + array[2]"
            )
            .expect("eval"),
        Value::Number(13.0)
    );
    let array = runtime.new_array(vec![Value::Number(1.0)]);
    runtime.set_property(array.clone(), "length", Value::Number(3.0));
    assert_eq!(runtime.get_property(&array, "length"), Value::Number(3.0));
    assert_eq!(
        runtime
            .eval("'abc'.toUpperCase() + String(Math.max(1, 2))")
            .expect("eval"),
        Value::String("ABC2".into())
    );
}

#[test]
fn accessor_functions_are_garbage_collected_roots() {
    let mut runtime = Runtime::new();
    let object = runtime.new_object();
    runtime.define_accessor(
        object.clone(),
        "live",
        Box::new(|_runtime, _this, _args| Ok(Value::String("alive".into()))),
        Box::new(|_runtime, _this, _args| Ok(Value::Undefined)),
    );
    runtime.add_gc_root(object.clone());
    runtime.collect_garbage();
    // If the getter function object had been collected this would fail.
    assert_eq!(
        runtime.get_property(&object, "live"),
        Value::String("alive".into())
    );
    runtime.clear_gc_roots();
    runtime.collect_garbage();
}

#[test]
fn accessors_on_the_global_object_are_globals() {
    let mut runtime = Runtime::new();
    let state = Arc::new(Mutex::new(String::from("initial")));
    let global = runtime.global();

    let getter_state = Arc::clone(&state);
    let getter = Box::new(
        move |_runtime: &mut Runtime, _this: Value, _args: Vec<Value>| {
            Ok(Value::String(getter_state.lock().expect("lock").clone()))
        },
    );
    let setter_state = Arc::clone(&state);
    let setter = Box::new(
        move |_runtime: &mut Runtime, _this: Value, args: Vec<Value>| {
            *setter_state.lock().expect("lock") =
                args.first().map(Value::to_string_value).unwrap_or_default();
            Ok(Value::Undefined)
        },
    );
    runtime.define_accessor(global, "hostName", getter, setter);

    assert_eq!(
        runtime.eval("hostName").expect("eval"),
        Value::String("initial".into())
    );
    assert_eq!(
        runtime.eval("typeof hostName").expect("eval"),
        Value::String("string".into())
    );
    runtime.eval("hostName = 'mag';").expect("eval");
    assert_eq!(*state.lock().expect("lock"), "mag");
    assert_eq!(runtime.get_global("hostName"), Value::String("mag".into()));
}
