//! Feature tests for bit shifts, regular expressions, destructuring, and
//! default and rest parameters.

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

// ---------------------------------------------------------------------------
// Bit shifts
// ---------------------------------------------------------------------------

#[test]
fn left_shift_uses_int32_semantics() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "1 << 4"), 16.0);
    assert_eq!(number(&mut runtime, "3 << 1"), 6.0);
    assert_eq!(number(&mut runtime, "1 << 31"), -2147483648.0);
    assert_eq!(number(&mut runtime, "1 << 32"), 1.0);
    assert_eq!(number(&mut runtime, "5.9 << 1"), 10.0);
}

#[test]
fn arithmetic_right_shift_preserves_sign() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "16 >> 2"), 4.0);
    assert_eq!(number(&mut runtime, "-8 >> 1"), -4.0);
    assert_eq!(number(&mut runtime, "-1 >> 0"), -1.0);
    assert_eq!(number(&mut runtime, "-1 >> 31"), -1.0);
}

#[test]
fn unsigned_right_shift_converts_to_uint32() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "-1 >>> 0"), 4294967295.0);
    assert_eq!(number(&mut runtime, "-1 >>> 28"), 15.0);
    assert_eq!(number(&mut runtime, "8 >>> 2"), 2.0);
    assert_eq!(number(&mut runtime, "-2147483648 >>> 31"), 1.0);
}

#[test]
fn shift_precedence_and_compound_assignment() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "1 + 2 << 3"), 24.0);
    assert_eq!(number(&mut runtime, "1 << 2 + 1"), 8.0);
    assert_eq!(number(&mut runtime, "16 >> 2 >> 1"), 2.0);
    eval(&mut runtime, "var x = 1; x <<= 4; x >>= 1; x >>>= 1;");
    assert_eq!(number(&mut runtime, "x"), 4.0);
}

#[test]
fn division_is_not_a_regex() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "10 / 2 / 5"), 1.0);
    assert_eq!(number(&mut runtime, "var n = 8; n / 2"), 4.0);
    assert_eq!(number(&mut runtime, "[8][0] / 2"), 4.0);
}

// ---------------------------------------------------------------------------
// Regular expressions
// ---------------------------------------------------------------------------

#[test]
fn regex_literal_test_and_flags() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(&mut runtime, "/abc/.test('xxabcxx')"),
        Value::Bool(true)
    );
    assert_eq!(eval(&mut runtime, "/^abc$/.test('abc')"), Value::Bool(true));
    assert_eq!(
        eval(&mut runtime, "/^abc$/.test('abcd')"),
        Value::Bool(false)
    );
    assert_eq!(eval(&mut runtime, "/ABC/i.test('abc')"), Value::Bool(true));
    assert_eq!(
        eval(&mut runtime, "/a.b/.test('a\\nb')"),
        Value::Bool(false)
    );
    assert_eq!(
        eval(&mut runtime, "/a.b/s.test('a\\nb')"),
        Value::Bool(true)
    );
    assert_eq!(eval(&mut runtime, "/^b/m.test('a\\nb')"), Value::Bool(true));
}

#[test]
fn regex_character_classes_and_quantifiers() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(&mut runtime, "/^\\d{3}$/.test('123')"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "/^\\d{3}$/.test('12a')"),
        Value::Bool(false)
    );
    assert_eq!(
        eval(&mut runtime, "/^[a-z0-9_]+$/.test('abc_09')"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "/^[^0-9]+$/.test('ab1')"),
        Value::Bool(false)
    );
    assert_eq!(
        eval(&mut runtime, "/^ab+c$/.test('abbbc')"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "/^(cat|dog)$/.test('dog')"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "/\\bcat\\b/.test('a cat here')"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "/\\bcat\\b/.test('scatter')"),
        Value::Bool(false)
    );
}

#[test]
fn regex_exec_reports_index_and_captures() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "var m = /(\\d+)-(\\d+)/.exec('x 12-34 y'); m.index"
        ),
        2.0
    );
    assert_eq!(
        text(
            &mut runtime,
            "var m = /(\\d+)-(\\d+)/.exec('x 12-34 y'); m[0]"
        ),
        "12-34"
    );
    assert_eq!(
        text(
            &mut runtime,
            "var m = /(\\d+)-(\\d+)/.exec('x 12-34 y'); m[2]"
        ),
        "34"
    );
    assert_eq!(
        eval(&mut runtime, "/(a)(?:b)(c)/.exec('abc').length"),
        Value::Number(3.0)
    );
    assert_eq!(eval(&mut runtime, "/nope/.exec('abc')"), Value::Null);
}

#[test]
fn global_regex_advances_last_index() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(&mut runtime, "var re = /a/g; re.test('aba'); re.lastIndex"),
        1.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "var re = /a/g; re.exec('aba'); re.exec('aba').index"
        ),
        2.0
    );
    assert_eq!(
        eval(
            &mut runtime,
            "var re = /a/g; re.exec('aba'); re.exec('aba'); re.exec('aba')"
        ),
        Value::Null
    );
    assert_eq!(
        number(
            &mut runtime,
            "var re = /a/g; re.exec('aba'); re.exec('aba'); re.exec('aba'); re.lastIndex"
        ),
        0.0
    );
}

#[test]
fn regexp_constructor_forms() {
    let mut runtime = Runtime::new();
    assert_eq!(
        eval(&mut runtime, "new RegExp('a+').test('caa')"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(&mut runtime, "RegExp('^x$', 'i').test('X')"),
        Value::Bool(true)
    );
    assert_eq!(text(&mut runtime, "String(new RegExp('ab', 'g'))"), "/ab/g");
    assert_eq!(text(&mut runtime, "new RegExp('a+').source"), "a+");
}

#[test]
fn invalid_regexp_throws_syntax_error() {
    let mut runtime = Runtime::new();
    let error = runtime
        .eval("new RegExp('(unclosed')")
        .expect_err("invalid pattern should throw");
    assert!(error.message.contains("SyntaxError"));
    assert!(runtime.eval("new RegExp('a', 'gg')").is_err());
    assert!(runtime.eval("new RegExp('a', 'q')").is_err());
}

#[test]
fn string_match_with_regex() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "'a1b2c3'.match(/\\d/g).length"), 3.0);
    assert_eq!(text(&mut runtime, "'a1b2c3'.match(/\\d/g)[2]"), "3");
    assert_eq!(
        eval(&mut runtime, "'abc'.match(/z/)".to_owned().as_str()),
        Value::Null
    );
    assert_eq!(number(&mut runtime, "'x 42 y'.match(/(\\d+)/).index"), 2.0);
    assert_eq!(text(&mut runtime, "'x 42 y'.match(/(\\d+)/)[1]"), "42");
    assert_eq!(text(&mut runtime, "'abc'.match('b')[0]"), "b");
}

#[test]
fn string_replace_with_regex_and_groups() {
    let mut runtime = Runtime::new();
    assert_eq!(
        text(
            &mut runtime,
            "'John Smith'.replace(/(\\w+) (\\w+)/, '$2, $1')"
        ),
        "Smith, John"
    );
    assert_eq!(text(&mut runtime, "'a1b2'.replace(/\\d/g, '#')"), "a#b#");
    assert_eq!(text(&mut runtime, "'abc'.replace(/b/, '[$&]')"), "a[b]c");
    assert_eq!(text(&mut runtime, "'abc'.replace(/b/, '$$')"), "a$c");
    assert_eq!(text(&mut runtime, "'abcabc'.replace('b', 'X')"), "aXcabc");
}

#[test]
fn string_split_with_regex() {
    let mut runtime = Runtime::new();
    assert_eq!(
        text(&mut runtime, "'a1b22c'.split(/\\d+/).join('|')"),
        "a|b|c"
    );
    assert_eq!(
        text(&mut runtime, "'a,b;c'.split(/[,;]/).join('-')"),
        "a-b-c"
    );
    assert_eq!(number(&mut runtime, "'a1b'.split(/\\d/).length"), 2.0);
    assert_eq!(
        text(&mut runtime, "'a1b'.split(/(\\d)/).join('|')"),
        "a|1|b"
    );
}

// ---------------------------------------------------------------------------
// Destructuring
// ---------------------------------------------------------------------------

#[test]
fn array_destructuring_basics() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "var [a, b] = [1, 2]; a + b"), 3.0);
    assert_eq!(
        number(
            &mut runtime,
            "var [first, , third] = [1, 2, 3]; first + third"
        ),
        4.0
    );
    assert_eq!(number(&mut runtime, "let [x, y] = [3, 4]; x * y"), 12.0);
    assert_eq!(number(&mut runtime, "const [c] = [9]; c"), 9.0);
}

#[test]
fn array_destructuring_defaults_and_rest() {
    let mut runtime = Runtime::new();
    assert_eq!(number(&mut runtime, "var [a = 5, b = 7] = [1]; a + b"), 8.0);
    assert_eq!(number(&mut runtime, "var [a = 5] = [undefined]; a"), 5.0);
    assert_eq!(
        text(
            &mut runtime,
            "var [head, ...tail] = [1, 2, 3]; tail.join(',')"
        ),
        "2,3"
    );
    assert_eq!(
        number(&mut runtime, "var [head, ...tail] = [1]; tail.length"),
        0.0
    );
    assert_eq!(
        text(&mut runtime, "var [...all] = 'abc'; all.join('')"),
        "abc"
    );
}

#[test]
fn nested_array_destructuring() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(&mut runtime, "var [[a, b], [c]] = [[1, 2], [3]]; a + b + c"),
        6.0
    );
    assert_eq!(
        number(&mut runtime, "var [a, [b = 10]] = [1, []]; a + b"),
        11.0
    );
}

#[test]
fn object_destructuring_basics() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(&mut runtime, "var {a, b} = {a: 1, b: 2}; a + b"),
        3.0
    );
    assert_eq!(
        number(&mut runtime, "var {a: renamed} = {a: 4}; renamed"),
        4.0
    );
    assert_eq!(
        number(&mut runtime, "var {a = 7, b = 8} = {a: 1}; a + b"),
        9.0
    );
    assert_eq!(number(&mut runtime, "var {a: {b}} = {a: {b: 6}}; b"), 6.0);
    assert_eq!(
        text(&mut runtime, "var {'x': value} = {x: 'hi'}; value"),
        "hi"
    );
}

#[test]
fn object_destructuring_rest() {
    let mut runtime = Runtime::new();
    assert_eq!(
        text(
            &mut runtime,
            "var {a, ...rest} = {a: 1, b: 2, c: 3}; JSON.stringify(rest)"
        ),
        "{\"b\":2,\"c\":3}"
    );
    assert_eq!(
        number(&mut runtime, "var {...all} = {x: 1, y: 2}; all.x + all.y"),
        3.0
    );
}

#[test]
fn destructuring_assignment_expressions() {
    let mut runtime = Runtime::new();
    assert_eq!(
        text(
            &mut runtime,
            "var a = 1, b = 2; [a, b] = [b, a]; a + ',' + b"
        ),
        "2,1"
    );
    assert_eq!(
        number(&mut runtime, "var a, b; ({a, b} = {a: 3, b: 4}); a + b"),
        7.0
    );
    assert_eq!(
        number(&mut runtime, "var a = 1, b; [a, b = 5] = [2]; a + b"),
        7.0
    );
    assert_eq!(
        text(
            &mut runtime,
            "var obj = {}; ({x: obj.value} = {x: 9}); String(obj.value)"
        ),
        "9"
    );
}

#[test]
fn destructuring_in_parameters() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "function f([a, b]) { return a + b; } f([1, 2])"
        ),
        3.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function f({a, b = 2}) { return a + b; } f({a: 1})"
        ),
        3.0
    );
    assert_eq!(
        number(&mut runtime, "var f = ({a}) => a * 2; f({a: 5})"),
        10.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function f([a, ...rest]) { return a + rest.length; } f([1, 2, 3])"
        ),
        3.0
    );
}

#[test]
fn destructuring_errors_are_reported() {
    let mut runtime = Runtime::new();
    assert!(runtime.eval("var [a] = 5;").is_err());
    assert!(runtime.eval("var {a} = null;").is_err());
    assert!(runtime.eval("var [a];").is_err());
    assert!(runtime.eval("function f([a]) {} f(undefined)").is_err());
}

// ---------------------------------------------------------------------------
// Default and rest parameters
// ---------------------------------------------------------------------------

#[test]
fn default_parameters() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(&mut runtime, "function f(a, b = 10) { return a + b; } f(1)"),
        11.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function f(a, b = 10) { return a + b; } f(1, 2)"
        ),
        3.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function f(a = 1, b = a + 1) { return b; } f()"
        ),
        2.0
    );
    assert_eq!(
        number(&mut runtime, "function f(a, b = a * 2) { return b; } f(3)"),
        6.0
    );
    assert_eq!(
        number(&mut runtime, "var f = (a, b = 5) => a + b; f(1)"),
        6.0
    );
    assert_eq!(number(&mut runtime, "function f(a = 1) {} f.length"), 0.0);
}

#[test]
fn rest_parameters() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "function sum(...values) { var total = 0; for (var v of values) { total += v; } return total; } sum(1, 2, 3, 4)"
        ),
        10.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function f(first, ...rest) { return first + rest.length; } f(1, 2, 3)"
        ),
        3.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function f(...rest) { return rest.length; } f()"
        ),
        0.0
    );
    assert_eq!(
        number(&mut runtime, "var f = (...xs) => xs.length; f(1, 2)"),
        2.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "function f(a = 1, ...rest) { return a + rest[0]; } f(undefined, 5)"
        ),
        6.0
    );
}

#[test]
fn combined_destructuring_and_defaults() {
    let mut runtime = Runtime::new();
    assert_eq!(
        number(
            &mut runtime,
            "function f({a, b = 2} = {}, [c = 3] = []) { return a + b + c; } f({a: 1})"
        ),
        6.0
    );
    assert_eq!(
        number(
            &mut runtime,
            "var {items: [first = 9, ...rest]} = {items: [undefined, 2, 3]}; first + rest.length"
        ),
        11.0
    );
}
