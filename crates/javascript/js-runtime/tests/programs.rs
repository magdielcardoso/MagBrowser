//! End to end smoke tests that exercise whole programs through the public API.

use browser_js::{Runtime, Value};

fn eval(runtime: &mut Runtime, source: &str) -> Value {
    runtime
        .eval(source)
        .unwrap_or_else(|error| panic!("eval failed for {source:?}: {error}"))
}

fn number(runtime: &mut Runtime, source: &str) -> f64 {
    eval(runtime, source).to_number()
}

#[test]
fn allocation_pressure_keeps_live_objects() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var items = [];
         for (var i = 0; i < 4000; i++) { items.push({ index: i, label: 'item' + i }); }",
    );
    assert_eq!(number(&mut runtime, "items.length"), 4000.0);
    assert_eq!(number(&mut runtime, "items[3999].index"), 3999.0);
    assert_eq!(
        eval(&mut runtime, "items[123].label"),
        Value::String("item123".into())
    );
    runtime.collect_garbage();
    assert_eq!(number(&mut runtime, "items[3999].index"), 3999.0);
}

#[test]
fn method_shorthand_and_this() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "var counter = { total: 0, add(n) { this.total = this.total + n; return this.total; } };
             counter.add(2); counter.add(3);"
        ),
        5.0
    );
}

#[test]
fn nested_data_structures_and_loops() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "var grid = [[1, 2], [3, 4]];
             var sum = 0;
             for (var row of grid) { for (var cell of row) { sum += cell; } }
             sum"
        ),
        10.0
    );
    assert_eq!(
        eval(&mut runtime, "JSON.stringify([{ a: 1 }, { b: [2, 3] }])"),
        Value::String("[{\"a\":1},{\"b\":[2,3]}]".into())
    );
}

#[test]
fn recursion_and_memoised_closures() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } fib(16)"
        ),
        987.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function makeMemo() {
               var cache = {};
               return function (n) {
                 if (cache[n] === undefined) { cache[n] = n * n; }
                 return cache[n];
               };
             }
             var square = makeMemo();
             square(4); square(4); square(5)"
        ),
        25.0
    );
}

#[test]
fn per_iteration_bindings_for_for_of() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var fns = [];
         for (const value of [1, 2, 3]) { fns.push(function () { return value; }); }",
    );
    assert_eq!(number(&mut runtime, "fns[0]()"), 1.0);
    assert_eq!(number(&mut runtime, "fns[1]()"), 2.0);
    assert_eq!(number(&mut runtime, "fns[2]()"), 3.0);
}

#[test]
fn exceptions_propagate_through_calls() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(
            &mut runtime,
            "function inner() { throw new Error('deep'); }
             function outer() { inner(); }
             var message;
             try { outer(); } catch (error) { message = error.message; }
             message"
        ),
        Value::String("deep".into())
    );
    let error = runtime.eval("function boom() { throw new TypeError('nope'); } boom();");
    let error = error.expect_err("should throw");
    assert!(error.message.contains("TypeError"));
    assert!(error.message.contains("nope"));
    assert!(!error.stack.is_empty());
}

#[test]
fn string_methods_behave_reasonably() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(
            &mut runtime,
            "['hello', 'world'].map(function (word) { return word.toUpperCase(); }).join(' ')"
        ),
        Value::String("HELLO WORLD".into())
    );
    assert_eq!(
        eval(&mut runtime, "'abcdef'.slice(-3)"),
        Value::String("def".into())
    );
    assert_eq!(
        eval(&mut runtime, "'abcdef'.substring(4, 2)"),
        Value::String("cd".into())
    );
    assert_eq!(
        eval(&mut runtime, "'a-b-c'.split('-').length"),
        Value::Number(3.0)
    );
    assert_eq!(
        eval(
            &mut runtime,
            "'abc'.includes('bc') && 'abc'.startsWith('a') && 'abc'.endsWith('c')"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        eval(
            &mut runtime,
            "String(123) + '|' + Number('45') + '|' + Boolean(0)"
        ),
        Value::String("123|45|false".into())
    );
}

#[test]
fn prototype_chains_and_inheritance() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(
            &mut runtime,
            "function Shape(name) { this.name = name; }
             Shape.prototype.describe = function () { return 'shape ' + this.name; };
             function Circle(radius) { Shape.call(this, 'circle'); this.radius = radius; }
             Circle.prototype = Object.create(Shape.prototype);
             Circle.prototype.constructor = Circle;
             var c = new Circle(3);
             c.describe() + '/' + c.radius + '/' + (c instanceof Circle) + '/' + (c instanceof Shape)"
        ),
        Value::String("shape circle/3/true/true".into())
    );
}

#[test]
fn switch_labels_and_continue() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(
            &mut runtime,
            "var out = '';
             outer: for (var i = 0; i < 3; i++) {
               for (var j = 0; j < 3; j++) {
                 if (j === 2) { continue outer; }
                 out += '' + i + j;
               }
             }
             out"
        ),
        Value::String("000110112021".into())
    );
    assert_eq!(
        eval(
            &mut runtime,
            "var text = '';
             for (var n = 0; n < 5; n++) {
               switch (n % 3) {
                 case 0: text += 'a'; break;
                 case 1: text += 'b';
                 case 2: text += 'c'; break;
                 default: text += 'z';
               }
             }
             text"
        ),
        Value::String("abccabc".into())
    );
}

#[test]
fn promise_pipeline_with_multiple_stages() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var trace = [];
         var start = Promise.resolve(1);
         start
           .then(function (v) { trace.push('one:' + v); return v + 1; })
           .then(function (v) { trace.push('two:' + v); return v * 10; })
           .then(function (v) { trace.push('three:' + v); });
         trace.push('sync');",
    );
    assert_eq!(
        eval(&mut runtime, "trace.join(',')"),
        Value::String("sync".into())
    );
    runtime.run_microtasks().expect("microtasks");
    assert_eq!(
        eval(&mut runtime, "trace.join(',')"),
        Value::String("sync,one:1,two:2,three:20".into())
    );
}

#[test]
fn rejected_promise_chain_skips_fulfilled_handlers() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var log = [];
         Promise.reject('first')
           .then(function () { log.push('skipped'); })
           .catch(function (reason) { log.push('caught:' + reason); return 'recovered'; })
           .then(function (value) { log.push(value); });
         log.push('sync');",
    );
    runtime.run_microtasks().expect("microtasks");
    assert_eq!(
        eval(&mut runtime, "log.join(',')"),
        Value::String("sync,caught:first,recovered".into())
    );
}

#[test]
fn host_can_call_interpreted_functions() {
    let mut runtime = Runtime::new();
    eval(&mut runtime, "function twice(x) { return x * 2; }");
    let callee = runtime.get_global("twice");
    let result = runtime
        .call_function(&callee, Value::Undefined, vec![Value::Number(21.0)])
        .expect("call");
    assert_eq!(result, Value::Number(42.0));
}

#[test]
fn gc_pressure_inside_array_callbacks() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var source = [];
         for (var i = 0; i < 2500; i++) { source.push(i); }
         var mapped = source.map(function (value) { return { doubled: value * 2 }; });",
    );
    assert_eq!(number(&mut runtime, "mapped.length"), 2500.0);
    assert_eq!(number(&mut runtime, "mapped[2499].doubled"), 4998.0);
    runtime.collect_garbage();
    assert_eq!(number(&mut runtime, "mapped[2499].doubled"), 4998.0);
}

#[test]
fn global_object_properties_are_globals() {
    let mut runtime = Runtime::new();
    let global = runtime.global();
    runtime.set_property(global.clone(), "hostFlag", Value::Bool(true));
    assert_eq!(eval(&mut runtime, "hostFlag"), Value::Bool(true));
    assert_eq!(
        eval(&mut runtime, "typeof hostFlag"),
        Value::String("boolean".into())
    );
    eval(&mut runtime, "hostFlag = false;");
    assert_eq!(
        runtime.get_property(&global, "hostFlag"),
        Value::Bool(false)
    );
    runtime.define_host_method(
        global.clone(),
        "triple",
        Box::new(|_runtime, _this, args| {
            Ok(Value::Number(
                args.first().map(Value::to_number).unwrap_or(0.0) * 3.0,
            ))
        }),
    );
    assert_eq!(number(&mut runtime, "triple(4)"), 12.0);
}

#[test]
fn arrays_are_objects_with_length() {
    let mut runtime = Runtime::new();
    let array = runtime.new_array(vec![Value::Number(1.0), Value::Number(2.0)]);
    assert!(runtime.object_is_array(&array));
    assert_eq!(runtime.get_property(&array, "length"), Value::Number(2.0));
    assert_eq!(
        runtime.object_array_items(&array),
        Some(vec![Value::Number(1.0), Value::Number(2.0)])
    );
    runtime.set_property(array.clone(), "4", Value::Number(5.0));
    assert_eq!(runtime.get_property(&array, "length"), Value::Number(5.0));
    let plain = runtime.new_object();
    assert!(!runtime.object_is_array(&plain));
}
