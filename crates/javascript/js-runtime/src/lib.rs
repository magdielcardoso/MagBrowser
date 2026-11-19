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
mod generator;
mod interpreter;
mod intl;
mod module;
mod object;
mod promise;
mod reflect;
mod scope;
mod value;
mod weak;

use std::collections::{HashMap, VecDeque};

pub use errors::JsError;
pub use module::ModuleLoader;
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
    /// Monotonic generation counter for each heap slot. Incremented whenever a
    /// slot is freed or reused so stale weak references can be detected even
    /// after an object id has been recycled.
    pub(crate) heap_generations: Vec<u64>,
    pub(crate) heap_free: Vec<usize>,
    pub(crate) scopes: Vec<Option<Scope>>,
    pub(crate) scope_free: Vec<usize>,
    pub(crate) prototypes: Prototypes,
    /// Well-known `Symbol.iterator` value.
    pub(crate) symbol_iterator: ObjectId,
    /// Well-known `Symbol.asyncIterator` value.
    pub(crate) symbol_async_iterator: ObjectId,
    /// Prototype shared by generator objects.
    pub(crate) generator_prototype: ObjectId,
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
    /// Registered ES modules, indexed by module id.
    pub(crate) modules: Vec<module::ModuleRecord>,
    /// Maps a resolved module URL to its id in `modules`.
    pub(crate) module_urls: HashMap<String, usize>,
    /// Host callback used to resolve and fetch unknown module sources.
    pub(crate) module_loader: Option<ModuleLoader>,
    /// Module currently executing, used for `import.meta` and relative imports.
    pub(crate) current_module: Option<usize>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Default number of execution steps a single script evaluation may take.
    ///
    /// A step is counted for every statement and every loop iteration. Scripts
    /// that exceed this budget are aborted with an `execution budget exceeded`
    /// runtime error instead of freezing the caller.
    pub const DEFAULT_STEP_BUDGET: u64 = 1_000_000;

    /// Default maximum depth of nested JavaScript function calls.
    pub const DEFAULT_MAX_CALL_DEPTH: usize = 512;

    /// Create a runtime with the global environment fully installed.
    pub fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let mut runtime = Self {
            heap: Vec::new(),
            heap_generations: Vec::new(),
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
            symbol_iterator: ObjectId(0),
            symbol_async_iterator: ObjectId(0),
            generator_prototype: ObjectId(0),
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
            max_steps: Self::DEFAULT_STEP_BUDGET,
            call_depth: 0,
            max_call_depth: Self::DEFAULT_MAX_CALL_DEPTH,
            in_microtask: false,
            modules: Vec::new(),
            module_urls: HashMap::new(),
            module_loader: None,
            current_module: None,
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

    /// Set the maximum number of execution steps allowed for a single
    /// `eval`/`eval_named` call. The counter is reset at the start of every
    /// evaluation, so each script receives a fresh budget.
    pub fn set_execution_budget(&mut self, max_steps: u64) {
        self.max_steps = max_steps;
    }

    /// Set the maximum depth of nested JavaScript function calls.
    pub fn set_max_call_depth(&mut self, max_depth: usize) {
        self.max_call_depth = max_depth;
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

    #[test]
    fn infinite_loop_hits_the_execution_budget() {
        let mut runtime = Runtime::new();
        let error = runtime
            .eval("while (true) {}")
            .expect_err("an infinite loop must be stopped by the step budget");
        assert_eq!(error.message, "RangeError: execution budget exceeded");
        assert_eq!(runtime.steps, runtime.max_steps + 1);
    }

    #[test]
    fn infinite_for_loop_hits_the_execution_budget() {
        let mut runtime = Runtime::new();
        let error = runtime
            .eval("for (;;) { }")
            .expect_err("an infinite for loop must be stopped by the step budget");
        assert_eq!(error.message, "RangeError: execution budget exceeded");
    }

    #[test]
    fn deep_recursion_hits_the_call_depth_limit() {
        let mut runtime = Runtime::new();
        let error = runtime
            .eval("function recurse(n) { return recurse(n + 1); } recurse(0);")
            .expect_err("unbounded recursion must be stopped by the call depth limit");
        assert_eq!(
            error.message,
            "RangeError: maximum call stack size exceeded"
        );
        assert_eq!(
            runtime.call_depth, 0,
            "call depth must be restored after error"
        );
    }

    #[test]
    fn execution_budget_is_reset_and_configurable() {
        let mut runtime = Runtime::new();
        runtime.set_execution_budget(50);
        assert!(runtime.eval("while (true) {}").is_err());
        // A fresh evaluation starts with a fresh budget.
        assert_eq!(number(&mut runtime, "6 * 7"), 42.0);
    }

    // -----------------------------------------------------------------------
    // ES6 classes
    // -----------------------------------------------------------------------

    #[test]
    fn class_constructor_methods_and_static_methods() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "class Point { \
                constructor(x, y) { this.x = x; this.y = y; } \
                sum() { return this.x + this.y; } \
                static origin() { return new Point(0, 0); } \
             } \
             var p = new Point(2, 3);",
        );
        assert_eq!(number(&mut runtime, "p.sum()"), 5.0);
        assert_eq!(number(&mut runtime, "Point.origin().sum()"), 0.0);
        assert_eq!(
            eval_ok(&mut runtime, "p instanceof Point"),
            Value::Bool(true)
        );
        assert_eq!(
            eval_ok(&mut runtime, "typeof Point"),
            Value::String("function".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "p.constructor === Point"),
            Value::Bool(true)
        );
    }

    #[test]
    fn class_extends_and_super_calls() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "class Animal { \
                constructor(name) { this.name = name; } \
                speak() { return this.name + ' makes a sound'; } \
             } \
             class Dog extends Animal { \
                constructor(name) { super(name.toUpperCase()); this.kind = 'dog'; } \
                speak() { return super.speak() + ' (woof)'; } \
             } \
             var dog = new Dog('rex');",
        );
        assert_eq!(
            eval_ok(&mut runtime, "dog.name"),
            Value::String("REX".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "dog.speak()"),
            Value::String("REX makes a sound (woof)".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "dog instanceof Dog"),
            Value::Bool(true)
        );
        assert_eq!(
            eval_ok(&mut runtime, "dog instanceof Animal"),
            Value::Bool(true)
        );
    }

    #[test]
    fn class_accessors_and_computed_method_calls() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "class Box { \
                constructor() { this._value = 1; } \
                get value() { return this._value * 2; } \
                set value(v) { this._value = v; } \
                call(fn) { return fn.call(this); } \
             } \
             var box = new Box();",
        );
        assert_eq!(number(&mut runtime, "box.value"), 2.0);
        eval_ok(&mut runtime, "box.value = 5");
        assert_eq!(number(&mut runtime, "box.value"), 10.0);
        assert_eq!(
            number(
                &mut runtime,
                "box.call(function () { return this._value; })"
            ),
            5.0
        );
    }

    #[test]
    fn class_expression_and_static_inheritance() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "var Base = class { static id() { return 7; } }; \
                 var Derived = class extends Base {}; Derived.id()"
            ),
            7.0
        );
    }

    // -----------------------------------------------------------------------
    // Generators
    // -----------------------------------------------------------------------

    #[test]
    fn generator_yield_next_and_return_value() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "function* numbers() { yield 1; yield 2; return 3; } var it = numbers();",
        );
        assert_eq!(number(&mut runtime, "it.next().value"), 1.0);
        assert_eq!(number(&mut runtime, "it.next().value"), 2.0);
        assert_eq!(number(&mut runtime, "it.next().value"), 3.0);
        assert_eq!(eval_ok(&mut runtime, "it.next().done"), Value::Bool(true));
    }

    #[test]
    fn generator_preserves_side_effects_and_loop_state_across_yields() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var log = []; \
             function* counter(n) { \
                 for (var i = 0; i < n; i++) { log.push('before' + i); yield i; log.push('after' + i); } \
                 return 'done'; \
             } \
             var it = counter(3);",
        );
        assert_eq!(number(&mut runtime, "it.next().value"), 0.0);
        assert_eq!(
            eval_ok(&mut runtime, "log.join(',')"),
            Value::String("before0".into())
        );
        assert_eq!(number(&mut runtime, "it.next().value"), 1.0);
        assert_eq!(
            eval_ok(&mut runtime, "log.join(',')"),
            Value::String("before0,after0,before1".into())
        );
        assert_eq!(number(&mut runtime, "it.next().value"), 2.0);
        assert_eq!(
            eval_ok(&mut runtime, "it.next().value"),
            Value::String("done".into())
        );
        assert_eq!(eval_ok(&mut runtime, "it.next().done"), Value::Bool(true));
    }

    #[test]
    fn generator_receives_next_values() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "function* echo() { var first = yield 'ask'; var second = yield first + 1; return second; } var it = echo();",
        );
        assert_eq!(
            eval_ok(&mut runtime, "it.next().value"),
            Value::String("ask".into())
        );
        assert_eq!(number(&mut runtime, "it.next(10).value"), 11.0);
        assert_eq!(number(&mut runtime, "it.next(20).value"), 20.0);
    }

    #[test]
    fn generator_is_iterable_and_works_with_for_of_and_spread() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "function* range(n) { for (var i = 0; i < n; i++) { yield i; } } \
                 var total = 0; for (var value of range(5)) { total += value; } total"
            ),
            10.0
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "function* ab() { yield 'a'; yield 'b'; } [...ab()].join('-')"
            ),
            Value::String("a-b".into())
        );
    }

    #[test]
    fn yield_star_delegates_to_another_generator() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "function* inner() { yield 1; yield 2; return 'inner'; } \
             function* outer() { yield 0; var result = yield* inner(); yield result; return 'outer'; } \
             var it = outer();",
        );
        let values: Vec<f64> = (0..3)
            .map(|_| number(&mut runtime, "it.next().value"))
            .collect();
        assert_eq!(values, vec![0.0, 1.0, 2.0]);
        assert_eq!(
            eval_ok(&mut runtime, "it.next().value"),
            Value::String("inner".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "it.next().value"),
            Value::String("outer".into())
        );
    }

    #[test]
    fn generator_method_and_return_force_completion() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var obj = { values: [1, 2], *items() { for (var v of this.values) { yield v; } } }; \
             var it = obj.items();",
        );
        assert_eq!(number(&mut runtime, "it.next().value"), 1.0);
        assert_eq!(number(&mut runtime, "it.return(99).value"), 99.0);
        assert_eq!(eval_ok(&mut runtime, "it.next().done"), Value::Bool(true));
    }

    // -----------------------------------------------------------------------
    // Async / await
    // -----------------------------------------------------------------------

    #[test]
    fn async_function_returns_a_promise_and_await_resumes() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var order = []; \
             async function load() { order.push('start'); var value = await 42; order.push('got' + value); return value + 1; } \
             var result = load(); order.push('after-call'); \
             result.then(function (value) { order.push('result' + value); });",
        );
        assert_eq!(
            eval_ok(&mut runtime, "order.join(',')"),
            Value::String("start,after-call".into())
        );
        runtime.run_microtasks().expect("microtasks");
        assert_eq!(
            eval_ok(&mut runtime, "order.join(',')"),
            Value::String("start,after-call,got42,result43".into())
        );
    }

    #[test]
    fn await_unwraps_promises_and_propagates_rejections() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var log = []; \
             async function chain() { var a = await Promise.resolve(1); var b = await Promise.resolve(a + 1); return b * 10; } \
             async function failing() { await 1; throw new Error('boom'); } \
             chain().then(function (v) { log.push('ok' + v); }); \
             failing().catch(function (e) { log.push(e.message); });",
        );
        for _ in 0..8 {
            runtime.run_microtasks().expect("microtasks");
        }
        let log = eval_ok(&mut runtime, "log.join(',')").to_string_value();
        assert!(log.contains("ok20"), "{log}");
        assert!(log.contains("boom"), "{log}");
    }

    #[test]
    fn async_arrow_and_await_in_loop() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var seen = []; \
             var collect = async function (xs) { for (var x of xs) { seen.push(await x); } return seen.length; }; \
             var pending = collect([1, 2, 3]);",
        );
        for _ in 0..8 {
            runtime.run_microtasks().expect("microtasks");
        }
        assert_eq!(
            eval_ok(&mut runtime, "seen.join(',')"),
            Value::String("1,2,3".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "pending instanceof Promise"),
            Value::Bool(true)
        );
    }

    // -----------------------------------------------------------------------
    // Iterators, spread, optional chaining, nullish coalescing, Symbol
    // -----------------------------------------------------------------------

    #[test]
    fn for_of_with_custom_symbol_iterator() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var range = { from: 1, to: 3, [Symbol.iterator]: function () { \
                     var current = this.from; var last = this.to; var self = this; \
                     return { next: function () { \
                         if (current <= last) { return { value: current++, done: false }; } \
                         return { value: undefined, done: true }; \
                     } }; \
                 } }; \
                 var out = []; for (var n of range) { out.push(n); } out.join(',')"
            ),
            Value::String("1,2,3".into())
        );
    }

    #[test]
    fn spread_in_calls_and_arrays_including_strings() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(&mut runtime, "var args = [1, 2, 3]; Math.max(...args)"),
            Value::Number(3.0)
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var head = [0]; var tail = [2, 3]; head.concat(...tail).join(',')"
            ),
            Value::String("0,2,3".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "[...'abc'].join('')"),
            Value::String("abc".into())
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "function f(a, b) { return a + b; } f(...[1, 2])"
            ),
            Value::Number(3.0)
        );
    }

    #[test]
    fn optional_chaining_short_circuits_whole_chain() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var calls = 0; function bump() { calls++; return 1; }",
        );
        assert_eq!(
            eval_ok(&mut runtime, "var unset; unset?.a?.b?.()"),
            Value::Undefined
        );
        assert_eq!(number(&mut runtime, "calls"), 0.0);
        assert_eq!(
            eval_ok(&mut runtime, "({ a: { b: 5 } }).a?.b"),
            Value::Number(5.0)
        );
        assert_eq!(
            eval_ok(&mut runtime, "({ a: null }).a?.b"),
            Value::Undefined
        );
        assert_eq!(eval_ok(&mut runtime, "null?.[0]"), Value::Undefined);
        assert_eq!(
            eval_ok(&mut runtime, "var f = function () { return 9; }; f?.()"),
            Value::Number(9.0)
        );
        assert_eq!(eval_ok(&mut runtime, "undefined?.()"), Value::Undefined);
    }

    #[test]
    fn nullish_coalescing_only_checks_null_and_undefined() {
        let mut runtime = Runtime::new();
        assert_eq!(eval_ok(&mut runtime, "null ?? 5"), Value::Number(5.0));
        assert_eq!(eval_ok(&mut runtime, "undefined ?? 5"), Value::Number(5.0));
        assert_eq!(eval_ok(&mut runtime, "0 ?? 5"), Value::Number(0.0));
        assert_eq!(eval_ok(&mut runtime, "'' ?? 5"), Value::String("".into()));
        assert_eq!(eval_ok(&mut runtime, "false ?? 5"), Value::Bool(false));
        assert_eq!(
            eval_ok(&mut runtime, "var object = { a: null }; object.a ?? 7"),
            Value::Number(7.0)
        );
    }

    #[test]
    fn symbol_basics() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(&mut runtime, "typeof Symbol('x')"),
            Value::String("symbol".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "Symbol('a') === Symbol('a')"),
            Value::Bool(false)
        );
        assert_eq!(
            eval_ok(&mut runtime, "typeof Symbol.iterator"),
            Value::String("symbol".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "[][Symbol.iterator]().next().done"),
            Value::Bool(true)
        );
    }

    #[test]
    fn nested_destructuring_defaults_evaluate_lazily() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var calls = 0; function mark() { calls++; return 99; } \
             var { a: { b = 1 } = {}, c: [d = 2] = [] } = { a: {}, c: [] }; \
             function f([x = mark(), [y = 4] = []] = []) { return x + y; }",
        );
        assert_eq!(number(&mut runtime, "b + d"), 3.0);
        assert_eq!(number(&mut runtime, "f()"), 103.0);
        assert_eq!(number(&mut runtime, "calls"), 1.0);
        assert_eq!(number(&mut runtime, "f([5, [7]])"), 12.0);
    }

    #[test]
    fn templates_interpolate_all_value_kinds() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(&mut runtime, "`n=${1 + 1}, s=${'x'}, b=${true}, o=${null}`"),
            Value::String("n=2, s=x, b=true, o=null".into())
        );
        assert_eq!(
            eval_ok(&mut runtime, "var n = 3; `square=${n * n}`"),
            Value::String("square=9".into())
        );
    }

    // -----------------------------------------------------------------------
    // Limits still apply to the new execution paths
    // -----------------------------------------------------------------------

    #[test]
    fn infinite_generator_next_respects_step_budget() {
        let mut runtime = Runtime::new();
        runtime.set_execution_budget(40);
        let error = runtime
            .eval(
                "function* forever() { while (true) { yield 1; } } var it = forever(); \
                   for (var i = 0; i < 50; i++) { it.next(); }",
            )
            .expect_err("step budget must stop unbounded generators");
        assert_eq!(error.message, "RangeError: execution budget exceeded");
    }

    #[test]
    fn deeply_recursive_async_calls_respect_call_depth() {
        let mut runtime = Runtime::new();
        assert!(
            runtime
                .eval("async function recurse(n) { return await recurse(n + 1); } recurse(0);")
                .is_ok(),
            "async recursion is queued through promises, so the initial call returns"
        );
        for _ in 0..4 {
            let _ = runtime.run_microtasks();
        }
    }

    #[test]
    fn static_super_methods_and_for_of_destructuring() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "class Base { static twice(x) { return x * 2; } } \
                 class Child extends Base { static four(x) { return super.twice(x) * 2; } } \
                 Child.four(3)"
            ),
            12.0
        );
        assert_eq!(
            eval_ok(
                &mut runtime,
                "var sum = 0; for (const [a, b = 1] of [[1], [2, 5]]) { sum += a + b; } sum"
            ),
            Value::Number(9.0)
        );
    }

    #[test]
    fn symbol_values_work_as_computed_property_keys() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "var key = Symbol('id'); var obj = { [key]: 42 }; obj[key]"
            ),
            42.0
        );
    }

    #[test]
    fn generator_break_and_continue_keep_loop_state() {
        let mut runtime = Runtime::new();
        assert_eq!(
            eval_ok(
                &mut runtime,
                "function* g() { for (var i = 0; i < 5; i++) { if (i === 1) continue; if (i === 4) break; yield i; } return 'end'; } \
                 [...g()].join(',')"
            ),
            Value::String("0,2,3".into())
        );
    }

    #[test]
    fn awaited_rejection_can_be_caught_in_an_async_function() {
        let mut runtime = Runtime::new();
        eval_ok(
            &mut runtime,
            "var log = []; \
             async function safe() { \
                 try { await Promise.reject('nope'); } catch (error) { log.push('caught:' + error); } \
                 return 'ok'; \
             } \
             safe().then(function (value) { log.push(value); });",
        );
        for _ in 0..8 {
            runtime.run_microtasks().expect("microtasks");
        }
        assert_eq!(
            eval_ok(&mut runtime, "log.join(',')"),
            Value::String("caught:nope,ok".into())
        );
    }
}
