//! JavaScript values, objects, interpreter and garbage collector.
//!
//! This crate implements a self contained tree walking JavaScript engine. It has
//! no dependencies besides the companion parser crate and does not use `unsafe`.

#![forbid(unsafe_code)]

mod builtins;
mod errors;
mod execution;
mod function;
mod gc;
mod interpreter;
mod object;
mod promise;
mod scope;
mod value;

use std::collections::VecDeque;

pub use errors::JsError;
pub use object::HostFunction;
pub use value::{ObjectId, Value, number_to_string};

use object::{Object, ObjectKind, Prototypes};
use scope::{Scope, ScopeId};

/// Result of calling into the host or evaluating JavaScript.
pub type HostResult = Result<Value, JsError>;

/// A microtask queued on the runtime.
pub(crate) type Microtask = Box<dyn FnOnce(&mut Runtime) -> Result<(), JsError>>;

/// A JavaScript runtime: an object heap, an environment arena and the
/// interpreter state.
pub struct Runtime {
    pub(crate) heap: Vec<Option<Object>>,
    pub(crate) heap_free: Vec<usize>,
    pub(crate) scopes: Vec<Option<Scope>>,
    pub(crate) scope_free: Vec<usize>,
    pub(crate) prototypes: Prototypes,
    pub(crate) global: ObjectId,
    pub(crate) global_scope: ScopeId,
    pub(crate) microtasks: VecDeque<Microtask>,
    pub(crate) console: Box<dyn FnMut(&str) + Send>,
    pub(crate) gc_roots: Vec<Value>,
    pub(crate) temp_roots: Vec<Value>,
    pub(crate) gc_scope_roots: Vec<ScopeId>,
    pub(crate) env_stack: Vec<ScopeId>,
    pub(crate) var_scope_stack: Vec<ScopeId>,
    pub(crate) call_stack: Vec<String>,
    pub(crate) completion_value: Value,
    pub(crate) allocations_since_gc: usize,
    pub(crate) gc_threshold: usize,
    pub(crate) rng_state: u64,
    pub(crate) steps: u64,
    pub(crate) max_steps: u64,
    pub(crate) call_depth: usize,
    pub(crate) max_call_depth: usize,
    pub(crate) in_microtask: bool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Create a runtime with the global environment fully installed.
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let mut runtime = Self {
            heap: Vec::new(),
            heap_free: Vec::new(),
            scopes: Vec::new(),
            scope_free: Vec::new(),
            prototypes: Prototypes {
                object: ObjectId(0),
                array: ObjectId(0),
                function: ObjectId(0),
                string: ObjectId(0),
                number: ObjectId(0),
                boolean: ObjectId(0),
                error: ObjectId(0),
                promise: ObjectId(0),
                regexp: ObjectId(0),
            },
            global: ObjectId(0),
            global_scope: ScopeId(0),
            microtasks: VecDeque::new(),
            console: Box::new(|line: &str| println!("{line}")),
            gc_roots: Vec::new(),
            temp_roots: Vec::new(),
            gc_scope_roots: Vec::new(),
            env_stack: Vec::new(),
            var_scope_stack: Vec::new(),
            call_stack: Vec::new(),
            completion_value: Value::Undefined,
            allocations_since_gc: 0,
            gc_threshold: usize::MAX,
            rng_state: seed | 1,
            steps: 0,
            max_steps: 20_000_000,
            call_depth: 0,
            max_call_depth: 512,
            in_microtask: false,
        };
        runtime.bootstrap();
        runtime.gc_threshold = 2048;
        runtime
    }

    /// Parse and evaluate JavaScript source in the global scope.
    pub fn eval(&mut self, source: &str) -> Result<Value, JsError> {
        let program = browser_js_parser::parse(source)
            .map_err(|error| JsError::syntax_error(error.to_string()))?;
        self.steps = 0;
        self.exec_program(&program)
    }

    /// Evaluate source with a name attached to the call stack.
    pub fn eval_named(&mut self, source: &str, name: &str) -> Result<Value, JsError> {
        self.call_stack.push(name.to_owned());
        let result = self.eval(source);
        self.call_stack.pop();
        result
    }

    /// The global object.
    pub fn global(&mut self) -> Value {
        Value::Object(self.global)
    }

    /// Read a global binding, falling back to `undefined`.
    pub fn get_global(&mut self, name: &str) -> Value {
        self.read_binding(name).unwrap_or(Value::Undefined)
    }

    /// Create or overwrite a global binding.
    pub fn set_global(&mut self, name: &str, value: Value) {
        self.declare_global(name, value);
    }

    /// Install a host function as a global.
    pub fn define_global_function(&mut self, name: &str, f: HostFunction) {
        let function = self.new_native_value(name, 0, f);
        self.declare_global(name, function);
    }

    /// Allocate a JavaScript string value.
    pub fn alloc_string(&mut self, s: &str) -> Value {
        Value::String(s.to_owned())
    }

    /// Replace the console output sink.
    pub fn set_console_sink(&mut self, sink: Box<dyn FnMut(&str) + Send>) {
        self.console = sink;
    }

    /// Allocate a plain object with the given prototype (used while bootstrapping).
    #[allow(dead_code)]
    pub(crate) fn allocate_ordinary(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        self.alloc_object(ObjectKind::Ordinary, prototype)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn eval_ok(runtime: &mut Runtime, source: &str) -> Value {
        runtime
            .eval(source)
            .unwrap_or_else(|error| panic!("eval failed for {source:?}: {error}"))
    }

    fn number(runtime: &mut Runtime, source: &str) -> f64 {
        eval_ok(runtime, source).to_number()
    }

    #[test]
    fn arithmetic_and_precedence() {
        let mut runtime = Runtime::new();
        assert_eq!(number(&mut runtime, "1 + 2 * 3"), 7.0);
        assert_eq!(number(&mut runtime, "(1 + 2) * 3"), 9.0);
        assert_eq!(number(&mut runtime, "10 / 4"), 2.5);
        assert_eq!(number(&mut runtime, "7 % 3"), 1.0);
        assert_eq!(number(&mut runtime, "2 ** 10"), 1024.0);
        assert_eq!(number(&mut runtime, "-(2 ** 2)"), -4.0);
        assert_eq!(number(&mut runtime, "1 / 0"), f64::INFINITY);
        assert!(number(&mut runtime, "0 / 0").is_nan());
    }

    #[test]
    fn numeric_coercion() {
        let mut runtime = Runtime::new();
        assert_eq!(number(&mut runtime, "'10' - 3"), 7.0);
        assert_eq!(
            eval_ok(&mut runtime, "'10' + 3"),
            Value::String("103".into())
        );
        assert_eq!(eval_ok(&mut runtime, "true + 1"), Value::Number(2.0));
        assert!(number(&mut runtime, "undefined + 1").is_nan());
        assert_eq!(number(&mut runtime, "null + 1"), 1.0);
        assert_eq!(eval_ok(&mut runtime, "1 == '1'"), Value::Bool(true));
        assert_eq!(eval_ok(&mut runtime, "1 === '1'"), Value::Bool(false));
        assert_eq!(
            eval_ok(&mut runtime, "null == undefined"),
            Value::Bool(true)
        );
        assert_eq!(eval_ok(&mut runtime, "NaN === NaN"), Value::Bool(false));
    }

    #[test]
    fn strings_and_templates() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(&mut runtime, "'abc'.toUpperCase()"),
            Value::String("ABC".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "'a,b,c'.split(',').length"),
            Value::Number(3.0)
        );
        assert_eq!(
            eval_ok(&mut runtime, "var name = 'world'; `hello ${name}!`"),
            Value::String("hello world!".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "'  padded  '.trim()"),
            Value::String("padded".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "'abcabc'.replace('b', 'X')"),
            Value::String("aXcabc".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "'abc'.charCodeAt(0)"),
            Value::Number(97.0)
        );
    }

    #[test]
    fn closures_capture_bindings() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "function counter() { var count = 0; return function () { count = count + 1; return count; }; } \
             var next = counter();",
        );
        assert_eq!(number(&mut runtime, "next()"), 1.0);
        assert_eq!(number(&mut runtime, "next()"), 2.0);
        assert_eq!(number(&mut runtime, "next()"), 3.0);
    }

    #[test]
    fn function_hoisting_allows_forward_calls() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "sum(2, 3); function sum(a, b) { return a + b; }"
            ),
            5.0
        );
    }

    #[test]
    fn var_hoisting_and_undefined_reads() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(&mut runtime, "typeof later; var later = 1;"),
            Value::String("undefined".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "typeof missing"),
            Value::String("undefined".into())
        );
    }

    #[test]
    fn this_binding_for_methods() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var obj = { value: 5, get: function () { return this.value; } }; obj.get()"
            ),
            Value::Number(5.0)
        );
        assert_eq!(
            eval_ok(&mut runtime, "var f = obj.get; f() === undefined"),
            Value::Bool(true)
        );
    }

    #[test]
    fn arrow_functions_keep_lexical_this() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var obj = { value: 3, get: function () { return (() => this.value)(); } }; obj.get()"
            ),
            Value::Number(3.0)
        );
    }

    #[test]
    fn objects_and_arrays() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var arr = [1, 2, 3]; arr.push(4); arr[0] = 9; var obj = { a: 1, 'b': 2 }; obj.c = 3;",
        );
        assert_eq!(number(&mut runtime, "arr.length"), 4.0);
        assert_eq!(number(&mut runtime, "arr[3]"), 4.0);
        assert_eq!(number(&mut runtime, "arr[0]"), 9.0);
        assert_eq!(number(&mut runtime, "obj.a + obj.b + obj.c"), 6.0);
        assert_eq!(
            eval_ok(&mut runtime, "Object.keys(obj).join(',')"),
            Value::String("a,b,c".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "Array.isArray(arr)"),
            Value::Bool(true)
        );
    }

    #[test]
    fn array_methods() {
        let mut runtime = Runtime::new();
        eval_ok(&mut runtime, "var arr = [1, 2, 3, 4];");
        assert_eq!(
            eval_ok(
                &mut runtime,
                "arr.map(function (x) { return x * 2; }).join('-')"
            ),
            Value::String("2-4-6-8".into())
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "arr.filter(function (x) { return x % 2 === 0; }).join(',')"
            ),
            Value::String("2,4".into())
        );
        assert_eq!(
            number(
                &mut runtime,
                "arr.reduce(function (a, b) { return a + b; }, 0)"
            ),
            10.0
        );
        assert_eq!(
            eval_ok(&mut runtime, "arr.slice(1, 3).join(',')"),
            Value::String("2,3".into())
        );
        assert_eq!(number(&mut runtime, "arr.indexOf(3)"), 2.0);
        assert_eq!(eval_ok(&mut runtime, "arr.shift()"), Value::Number(1.0));
        assert_eq!(eval_ok(&mut runtime, "arr.pop()"), Value::Number(4.0));
        assert_eq!(number(&mut runtime, "arr.unshift(0)"), 3.0);
    }

    #[test]
    fn compound_assignment_and_updates() {
        let mut runtime = Runtime::new();
        eval_ok(&mut runtime, "var x = 1; x += 2; x *= 3;");
        assert_eq!(number(&mut runtime, "x"), 9.0);
        assert_eq!(number(&mut runtime, "x++"), 9.0);
        assert_eq!(number(&mut runtime, "x"), 10.0);
        assert_eq!(number(&mut runtime, "++x"), 11.0);
        assert_eq!(number(&mut runtime, "x--"), 11.0);
        assert_eq!(number(&mut runtime, "x"), 10.0);
        eval_ok(&mut runtime, "var obj = { n: 2 }; obj.n += 5;");
        assert_eq!(number(&mut runtime, "obj.n"), 7.0);
    }

    #[test]
    fn exceptions_are_thrown_and_caught() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var log = []; try { throw 'boom'; } catch (error) { log.push(error); } log[0]"
            ),
            Value::String("boom".into())
        );
        let error = runtime
            .eval("throw new Error('bad')")
            .expect_err("should fail");
        assert!(error.message.contains("bad"));
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var value = 0; try { value = 1; } finally { value = 2; } value"
            ),
            Value::Number(2.0)
        );
        assert!(runtime.eval("undefinedVariable").is_err());
        assert!(runtime.eval("null.x").is_err());
    }

    #[test]
    fn try_finally_preserves_return() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "function f() { try { return 1; } finally { var ignored = 2; } } f()"
            ),
            1.0
        );
    }

    #[test]
    fn for_loops_and_break_continue() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "var total = 0; for (var i = 0; i < 10; i++) { if (i === 3) continue; if (i === 6) break; total += i; } total"
            ),
            12.0
        );
        assert_eq!(
            number(&mut runtime, "var n = 0; while (n < 5) { n++; } n"),
            5.0
        );
        assert_eq!(
            number(&mut runtime, "var m = 0; do { m++; } while (m < 3); m"),
            3.0
        );
    }

    #[test]
    fn for_of_and_for_in() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "var sum = 0; for (const item of [1, 2, 3]) { sum += item; } sum"
            ),
            6.0
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var keys = []; for (var key in { a: 1, b: 2 }) { keys.push(key); } keys.join(',')"
            ),
            Value::String("a,b".into())
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var out = []; for (var ch of 'abc') { out.push(ch); } out.join('')"
            ),
            Value::String("abc".into())
        );
    }

    #[test]
    fn labeled_break_and_switch() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "var found = 0; outer: for (var i = 0; i < 5; i++) { for (var j = 0; j < 5; j++) { if (i * j === 6) { found = i * 10 + j; break outer; } } } found"
            ),
            23.0
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var result; switch (2) { case 1: result = 'one'; break; case 2: result = 'two'; break; default: result = 'other'; } result"
            ),
            Value::String("two".into())
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var fall = ''; switch (1) { case 1: fall += 'a'; case 2: fall += 'b'; break; default: fall += 'c'; } fall"
            ),
            Value::String("ab".into())
        );
    }

    #[test]
    fn let_const_and_block_scope() {
        let mut runtime = Runtime::new();
        assert_eq!(number(&mut runtime, "let x = 1; { let x = 2; } x"), 1.0);
        assert!(runtime.eval("const c = 1; c = 2;").is_err());
        assert!(runtime.eval("{ y; let y = 1; }").is_err());
    }

    #[test]
    fn functions_are_first_class_and_recursive() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); } fact(5)"
            ),
            120.0
        );
        assert_eq!(
            number(
                &mut runtime,
                "function apply(f, value) { return f(value); } apply(function (x) { return x + 1; }, 41)"
            ),
            42.0
        );
    }

    #[test]
    fn typeof_and_delete() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(&mut runtime, "typeof function () {}"),
            Value::String("function".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "typeof null"),
            Value::String("object".into())
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var obj = { a: 1 }; delete obj.a; String(obj.a)"
            ),
            Value::String("undefined".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "var none; typeof none"),
            Value::String("undefined".into())
        );
    }

    #[test]
    fn builtins_math_and_parsing() {
        let mut runtime = Runtime::new();
        assert_eq!(number(&mut runtime, "Math.max(1, 5, 3)"), 5.0);
        assert_eq!(number(&mut runtime, "Math.min(1, 5, 3)"), 1.0);
        assert_eq!(number(&mut runtime, "Math.floor(2.9)"), 2.0);
        assert_eq!(number(&mut runtime, "Math.ceil(2.1)"), 3.0);
        assert_eq!(number(&mut runtime, "Math.round(2.5)"), 3.0);
        assert_eq!(number(&mut runtime, "Math.pow(2, 8)"), 256.0);
        assert_eq!(number(&mut runtime, "Math.abs(-4)"), 4.0);
        assert_eq!(number(&mut runtime, "Math.sqrt(81)"), 9.0);
        let random = number(&mut runtime, "Math.random()");
        assert!((0.0..1.0).contains(&random));
        assert_eq!(number(&mut runtime, "parseInt('42px')"), 42.0);
        assert_eq!(number(&mut runtime, "parseFloat('2.5rem')"), 2.5);
        assert_eq!(eval_ok(&mut runtime, "isNaN('abc')"), Value::Bool(true));
    }

    #[test]
    fn json_serialisation() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(&mut runtime, "JSON.stringify({ a: 1, b: [true, null] })"),
            Value::String("{\"a\":1,\"b\":[true,null]}".into())
        );
        assert_eq!(number(&mut runtime, "JSON.parse('{\"x\":2}').x"), 2.0);
        assert!(runtime.eval("JSON.parse('{bad}')").is_err());
    }

    #[test]
    fn prototypes_and_instanceof() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(
                &mut runtime,
                "function Animal(name) { this.name = name; } Animal.prototype.speak = function () { return this.name; }; var dog = new Animal('rex'); dog.speak()"
            ),
            Value::String("rex".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "var d = new Animal('x'); d instanceof Animal"),
            Value::Bool(true)
        );
        assert_eq!(
            eval_ok(&mut runtime, "var a = []; a instanceof Array"),
            Value::Bool(true)
        );
        assert_eq!(eval_ok(&mut runtime, "'x' in { x: 1 }"), Value::Bool(true));
    }

    #[test]
    fn closures_are_collected_without_references() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "function make() { var secret = { a: 1 }; return function () { return secret; }; } var holder = make();",
        );
        runtime.collect_garbage();
        assert_eq!(number(&mut runtime, "holder().a"), 1.0);
    }

    #[test]
    fn host_api_globals_and_functions() {
        let mut runtime = Runtime::new();
        runtime.set_global("hostValue", Value::Number(7.0));
        assert_eq!(number(&mut runtime, "hostValue"), 7.0);
        runtime.define_global_function(
            "hostAdd",
            Box::new(|_runtime, _this, args| {
                let total: f64 = args.iter().map(Value::to_number).sum();
                Ok(Value::Number(total))
            }),
        );
        assert_eq!(number(&mut runtime, "hostAdd(1, 2, 3)"), 6.0);
        let object = runtime.new_object();
        runtime.set_property(object.clone(), "name", Value::String("node".into()));
        runtime.define_host_method(
            object.clone(),
            "greet",
            Box::new(|_runtime, this, _args| {
                let name = match this {
                    Value::Object(_) => "object".to_owned(),
                    other => other.to_string_value(),
                };
                Ok(Value::String(format!("hi {name}")))
            }),
        );
        runtime.set_global("thing", object);
        assert_eq!(
            eval_ok(&mut runtime, "thing.greet()"),
            Value::String("hi object".into())
        );
        let global = runtime.global();
        let keys = runtime.object_keys(&global);
        assert!(keys.contains(&"console".to_owned()));
    }

    #[test]
    fn console_sink_receives_output() {
        let mut runtime = Runtime::new();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        runtime.set_console_sink(Box::new(move |line: &str| {
            sink.lock().expect("lock").push(line.to_owned());
        }));
        eval_ok(
            &mut runtime,
            "console.log('hello', 42); console.error('oops');",
        );
        let lines = captured.lock().expect("lock").clone();
        assert_eq!(lines, vec!["hello 42".to_owned(), "oops".to_owned()]);
    }

    #[test]
    fn microtasks_from_host_run_in_order() {
        let mut runtime = Runtime::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        for index in 0..3 {
            let order = Arc::clone(&order);
            runtime.enqueue_microtask(Box::new(move |_runtime, _this, _args| {
                order.lock().expect("lock").push(index);
                Ok(Value::Undefined)
            }));
        }
        runtime.run_microtasks().expect("microtasks");
        assert_eq!(*order.lock().expect("lock"), vec![0, 1, 2]);
    }

    #[test]
    fn host_roots_preserve_objects() {
        let mut runtime = Runtime::new();
        let object = runtime.new_object();
        runtime.set_property(object.clone(), "kept", Value::Number(1.0));
        runtime.add_gc_root(object.clone());
        runtime.collect_garbage();
        assert_eq!(runtime.get_property(&object, "kept"), Value::Number(1.0));
    }
}
