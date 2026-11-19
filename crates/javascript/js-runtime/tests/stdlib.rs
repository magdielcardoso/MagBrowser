//! Tests for the extended standard library: Proxy/Reflect, weak references,
//! improved RegExp, Intl, and async iteration.

use browser_js::{Runtime, Value};

fn eval(runtime: &mut Runtime, source: &str) -> Value {
    runtime
        .eval(source)
        .unwrap_or_else(|error| panic!("eval failed for {source:?}: {error}"))
}

fn number(runtime: &mut Runtime, source: &str) -> f64 {
    eval(runtime, source).to_number()
}

fn text(runtime: &mut Runtime, source: &str) -> String {
    eval(runtime, source).to_string_value()
}

fn drain(runtime: &mut Runtime) {
    runtime
        .run_microtasks()
        .unwrap_or_else(|error| panic!("microtasks failed: {error}"));
}

// ---------------------------------------------------------------------------
// Reflect
// ---------------------------------------------------------------------------

#[test]
fn reflect_property_operations() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(&mut runtime, "Reflect.get({ a: 1 }, 'a')"),
        Value::Number(1.0)
    );
    assert_eq!(
        eval(&mut runtime, "Reflect.set({}, 'a', 2)"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "Reflect.has({ a: 1 }, 'a')"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "Reflect.has({ a: 1 }, 'b')"),
        Value::Bool(false)
    );
    assert_eq!(
        eval(&mut runtime, "Reflect.deleteProperty({ a: 1 }, 'a')"),
        Value::Bool(true)
    );
    assert_eq!(
        text(&mut runtime, "Reflect.ownKeys({ a: 1, b: 2 }).join(',')"),
        "a,b"
    );
    assert_eq!(
        number(
            &mut runtime,
            "Reflect.getOwnPropertyDescriptor({ a: 7 }, 'a').value"
        ),
        7.0
    );
    assert_eq!(
        eval(
            &mut runtime,
            "var o = {}; Reflect.defineProperty(o, 'x', { value: 3 }); o.x"
        ),
        Value::Number(3.0)
    );
}

#[test]
fn reflect_apply_and_construct() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "Reflect.apply(function (a, b) { return a + b; }, null, [2, 3])"
        ),
        5.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "Reflect.construct(function (x) { this.x = x; }, [7]).x"
        ),
        7.0
    );
}

// ---------------------------------------------------------------------------
// Proxy
// ---------------------------------------------------------------------------

#[test]
fn proxy_get_set_has_and_delete_traps() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var target = { x: 1 }; \
         var handler = { \
           get: function (t, key, receiver) { return key === 'x' ? t[key] + 10 : Reflect.get(t, key, receiver); }, \
           set: function (t, key, value) { t[key] = value * 2; return true; }, \
           has: function (t, key) { return key === 'hidden' || Reflect.has(t, key); }, \
           deleteProperty: function (t, key) { return Reflect.deleteProperty(t, key); } \
         }; \
         var p = new Proxy(target, handler);",
    );
    assert_eq!(number(&mut runtime, "p.x"), 11.0);
    eval(&mut runtime, "p.y = 5");
    assert_eq!(number(&mut runtime, "target.y"), 10.0);
    assert_eq!(eval(&mut runtime, "'hidden' in p"), Value::Bool(true));
    assert_eq!(eval(&mut runtime, "'x' in p"), Value::Bool(true));
    assert_eq!(eval(&mut runtime, "delete p.x"), Value::Bool(true));
    assert_eq!(
        eval(&mut runtime, "typeof target.x"),
        Value::String("undefined".into())
    );
}

#[test]
fn proxy_own_keys_descriptor_and_define_traps() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var target = { x: 1 }; \
         var handler = { \
           ownKeys: function () { return ['x', 'virtual']; }, \
           getOwnPropertyDescriptor: function () { return { value: 42, writable: true, enumerable: true, configurable: true }; }, \
           defineProperty: function (t, key, desc) { t[key] = desc.value + 1; return true; } \
         }; \
         var p = new Proxy(target, handler);",
    );
    assert_eq!(text(&mut runtime, "Object.keys(p).join(',')"), "x,virtual");
    assert_eq!(
        number(
            &mut runtime,
            "Reflect.getOwnPropertyDescriptor(p, 'x').value"
        ),
        42.0
    );
    assert_eq!(
        eval(
            &mut runtime,
            "Reflect.defineProperty(p, 'z', { value: 4 }); target.z"
        ),
        Value::Number(5.0)
    );
}

#[test]
fn proxy_apply_and_construct_traps() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var callable = new Proxy(function (a, b) { return a + b; }, { \
           apply: function (t, thisArg, args) { return args[0] * args[1]; } \
         }); \
         function C(v) { this.v = v; } \
         var PC = new Proxy(C, { \
           construct: function (t, args) { return { v: args[0] + 1, trapped: true }; } \
         });",
    );
    assert_eq!(number(&mut runtime, "callable(3, 4)"), 12.0);
    assert_eq!(
        eval(&mut runtime, "typeof callable"),
        Value::String("function".into())
    );
    assert_eq!(number(&mut runtime, "new PC(4).v"), 5.0);
    assert_eq!(eval(&mut runtime, "new PC(4).trapped"), Value::Bool(true));
}

#[test]
fn proxy_without_trap_forwards_to_target() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(&mut runtime, "var p = new Proxy({ a: 3 }, {}); p.a"),
        3.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "var f = new Proxy(function (x) { return x + 1; }, {}); f(9)"
        ),
        10.0
    );
}

// ---------------------------------------------------------------------------
// WeakRef / FinalizationRegistry
// ---------------------------------------------------------------------------

#[test]
fn weak_ref_dereferences_a_live_object() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(
            &mut runtime,
            "var obj = { a: 1 }; var ref = new WeakRef(obj); ref.deref() === obj"
        ),
        Value::Bool(true)
    );
}

#[test]
fn weak_ref_does_not_keep_object_alive() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var ref = (function () { var temporary = { a: 1 }; return new WeakRef(temporary); })();",
    );
    runtime.collect_garbage();
    assert_eq!(
        eval(&mut runtime, "ref.deref()"),
        Value::Undefined,
        "collected target must no longer be reachable through the WeakRef"
    );
}

#[test]
fn finalization_registry_runs_cleanup_for_collected_targets() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var seen = []; \
         var registry = new FinalizationRegistry(function (held, token) { seen.push([held, token]); }); \
         (function () { var temporary = { a: 1 }; registry.register(temporary, 'held', 'token'); })();",
    );
    runtime.collect_garbage();
    drain(&mut runtime);
    assert_eq!(number(&mut runtime, "seen.length"), 1.0);
    assert_eq!(text(&mut runtime, "seen[0][0]"), "held");
    assert_eq!(text(&mut runtime, "seen[0][1]"), "token");
}

#[test]
fn finalization_registry_unregister_cancels_cleanup() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var seen = []; \
         var registry = new FinalizationRegistry(function (held) { seen.push(held); }); \
         var token = {}; \
         (function () { var temporary = {}; registry.register(temporary, 'value', token); })(); \
         registry.unregister(token);",
    );
    runtime.collect_garbage();
    drain(&mut runtime);
    assert_eq!(number(&mut runtime, "seen.length"), 0.0);
}

// ---------------------------------------------------------------------------
// RegExp: sticky, named groups, indices and matchAll
// ---------------------------------------------------------------------------

#[test]
fn sticky_flag_only_matches_at_last_index() {
    let mut runtime = Runtime::new();
    eval(&mut runtime, "var re = /a/y; re.lastIndex = 1;");
    assert_eq!(
        eval(&mut runtime, "re.exec('ba') !== null"),
        Value::Bool(true)
    );
    assert_eq!(number(&mut runtime, "re.lastIndex"), 2.0);
    eval(&mut runtime, "re.lastIndex = 0;");
    assert_eq!(
        eval(&mut runtime, "re.exec('ba')"),
        Value::Null,
        "a sticky regexp must not skip ahead to find a match"
    );
    assert_eq!(eval(&mut runtime, "re.sticky"), Value::Bool(true));
}

#[test]
fn named_capture_groups_are_exposed_on_the_match() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "/(?<year>\\d{4})-(?<month>\\d{2})/.exec('2024-07').groups.year"
        ),
        2024.0
    );
    assert_eq!(
        text(&mut runtime, "/(?<a>x)(?<b>y)/.exec('xy').groups.b"),
        "y"
    );
}

#[test]
fn indices_flag_reports_group_ranges() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(&mut runtime, "/(\\d)(b)/d.exec('1b').indices[1][0]"),
        0.0
    );
    assert_eq!(
        number(&mut runtime, "/(\\d)(b)/d.exec('1b').indices[1][1]"),
        1.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "/(?<letter>b)/d.exec('ab').indices.groups.letter[0]"
        ),
        1.0
    );
}

#[test]
fn match_all_returns_an_iterator_of_match_objects() {
    let mut runtime = Runtime::new();
    assert_eq!(
        text(
            &mut runtime,
            "var out = []; for (const m of 'a1b2'.matchAll(/\\d/g)) { out.push(m[0]); } out.join(',')"
        ),
        "1,2"
    );
    assert_eq!(
        number(&mut runtime, "'a1b2'.matchAll(/\\d/g).next().value.index"),
        1.0
    );
}

// ---------------------------------------------------------------------------
// Intl
// ---------------------------------------------------------------------------

#[test]
fn number_format_groups_by_locale() {
    let mut runtime = Runtime::new();
    assert_eq!(
        text(
            &mut runtime,
            "new Intl.NumberFormat('en-US').format(1234567.891)"
        ),
        "1,234,567.891"
    );
    assert_eq!(
        text(
            &mut runtime,
            "new Intl.NumberFormat('pt-BR').format(1234567.891)"
        ),
        "1.234.567,891"
    );
    assert_eq!(
        text(
            &mut runtime,
            "new Intl.NumberFormat('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(12.5)"
        ),
        "12.50"
    );
    assert_eq!(
        text(
            &mut runtime,
            "new Intl.NumberFormat('en-US', { style: 'percent' }).format(0.25)"
        ),
        "25%"
    );
    assert_eq!(
        text(
            &mut runtime,
            "new Intl.NumberFormat('pt-BR', { style: 'currency', currency: 'BRL' }).format(1234.5)"
        ),
        "R$ 1.234,50"
    );
}

#[test]
fn date_time_format_uses_locale_patterns() {
    let mut runtime = Runtime::new();
    // 2024-07-02T00:00:00Z
    let timestamp = "1719888000000";
    assert_eq!(
        text(
            &mut runtime,
            &format!("new Intl.DateTimeFormat('en-US').format({timestamp})")
        ),
        "7/2/24"
    );
    assert_eq!(
        text(
            &mut runtime,
            &format!("new Intl.DateTimeFormat('pt-BR').format({timestamp})")
        ),
        "02/07/24"
    );
    assert_eq!(
        text(
            &mut runtime,
            &format!(
                "new Intl.DateTimeFormat('en-US', {{ year: 'numeric', month: 'long', day: 'numeric' }}).format({timestamp})"
            )
        ),
        "July 2, 2024"
    );
    assert_eq!(
        text(
            &mut runtime,
            &format!(
                "new Intl.DateTimeFormat('pt-BR', {{ year: 'numeric', month: 'long', day: 'numeric' }}).format({timestamp})"
            )
        ),
        "2 de julho de 2024"
    );
}

// ---------------------------------------------------------------------------
// Async iterators
// ---------------------------------------------------------------------------

#[test]
fn symbol_async_iterator_is_installed() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(&mut runtime, "typeof Symbol.asyncIterator"),
        Value::String("symbol".into())
    );
}

#[test]
fn async_generator_can_be_consumed_by_for_await() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var out = []; \
         async function* numbers() { yield 1; await null; yield 2; return 3; } \
         async function run() { for await (const value of numbers()) { out.push(value); } } \
         run();",
    );
    drain(&mut runtime);
    assert_eq!(text(&mut runtime, "out.join(',')"), "1,2");
}

#[test]
fn async_generator_next_returns_promise_of_iterator_result() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var iterator = (async function* () { yield 7; })(); \
         var result = iterator.next();",
    );
    assert_eq!(
        eval(&mut runtime, "typeof result.then"),
        Value::String("function".into())
    );
    eval(
        &mut runtime,
        "var seen; result.then(function (r) { seen = r; });",
    );
    drain(&mut runtime);
    assert_eq!(number(&mut runtime, "seen.value"), 7.0);
    assert_eq!(eval(&mut runtime, "seen.done"), Value::Bool(false));
}

#[test]
fn for_await_works_over_a_synchronous_iterable() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var out = []; \
         async function run() { for await (const value of [1, 2, 3]) { out.push(value); } } \
         run();",
    );
    drain(&mut runtime);
    assert_eq!(text(&mut runtime, "out.join(',')"), "1,2,3");
}

#[test]
fn reflect_own_keys_exposes_well_known_symbols() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(
            &mut runtime,
            "Reflect.ownKeys(Array.prototype).indexOf(Symbol.iterator) !== -1"
        ),
        Value::Bool(true)
    );
}

#[test]
fn for_await_works_over_a_custom_async_iterable() {
    let mut runtime = Runtime::new();
    eval(
        &mut runtime,
        "var out = []; \
         var iterable = {}; \
         iterable[Symbol.asyncIterator] = function () { \
           var i = 0; \
           return { next: function () { i++; return Promise.resolve({ value: i, done: i > 2 }); } }; \
         }; \
         async function run() { for await (const value of iterable) { out.push(value); } } \
         run();",
    );
    drain(&mut runtime);
    assert_eq!(text(&mut runtime, "out.join(',')"), "1,2");
}
