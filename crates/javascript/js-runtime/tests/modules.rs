//! Integration tests for ES module loading, linking and evaluation.

use std::collections::HashMap;

use browser_js::{JsError, ModuleLoader, Runtime, Value};

fn loader(sources: &[(&str, &str)]) -> ModuleLoader {
    let sources: HashMap<String, String> = sources
        .iter()
        .map(|(url, source)| ((*url).to_owned(), (*source).to_owned()))
        .collect();
    Box::new(move |specifier, _referrer| {
        let source = sources.get(specifier).cloned().ok_or_else(|| {
            JsError::new(format!("no source registered for module '{specifier}'"))
        })?;
        Ok((specifier.to_owned(), source))
    })
}

fn number(runtime: &mut Runtime, source: &str) -> f64 {
    runtime
        .eval(source)
        .unwrap_or_else(|error| panic!("eval failed for {source:?}: {error}"))
        .to_number()
}

#[test]
fn modules_import_each_others_exports() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[
        (
            "a.js",
            "import { twice, from_a } from './b.js';\n\
             export function a() { return 10; }\n\
             export function run() { return twice(a()) + from_a(); }\n\
             globalThis.result = run();",
        ),
        (
            "b.js",
            "import { a } from './a.js';\n\
             export function twice(x) { return x * 2; }\n\
             export function from_a() { return a(); }",
        ),
    ]));

    runtime
        .import_module("a.js")
        .unwrap_or_else(|error| panic!("import failed: {error}"));
    assert_eq!(number(&mut runtime, "result"), 30.0);
}

#[test]
fn default_export_and_import() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[
        ("value.js", "export default 42;"),
        (
            "main.js",
            "import answer from './value.js';\n\
             globalThis.answer = answer;",
        ),
    ]));

    runtime
        .import_module("main.js")
        .unwrap_or_else(|error| panic!("import failed: {error}"));
    assert_eq!(number(&mut runtime, "answer"), 42.0);
}

#[test]
fn default_function_export_is_hoisted() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[
        ("fn.js", "export default function () { return 7; }"),
        (
            "main.js",
            "import compute from './fn.js';\n\
             globalThis.value = compute();",
        ),
    ]));

    runtime
        .import_module("main.js")
        .unwrap_or_else(|error| panic!("import failed: {error}"));
    assert_eq!(number(&mut runtime, "value"), 7.0);
}

#[test]
fn namespace_import_exposes_named_and_default_exports() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[
        (
            "ns.js",
            "export default 5; export const answer = 2; export function add(x) { return x + 3; }",
        ),
        (
            "main.js",
            "import * as ns from './ns.js';\n\
             globalThis.total = ns.default + ns.answer + ns.add(ns.answer);",
        ),
    ]));

    runtime
        .import_module("main.js")
        .unwrap_or_else(|error| panic!("import failed: {error}"));
    assert_eq!(number(&mut runtime, "total"), 12.0);
}

#[test]
fn dynamic_import_returns_a_fulfilled_promise() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[("dyn.js", "export const value = 99;")]));

    runtime
        .eval(
            "globalThis.seen = null; \
             import('dyn.js').then(function (ns) { globalThis.seen = ns.value; });",
        )
        .expect("eval");
    assert_eq!(runtime.eval("seen").expect("eval"), Value::Null);
    runtime.run_microtasks().expect("microtasks");
    assert_eq!(number(&mut runtime, "seen"), 99.0);
}

#[test]
fn dynamic_import_rejection_is_observable() {
    let mut runtime = Runtime::new();
    runtime
        .eval(
            "globalThis.error = null; \
             import('missing.js').catch(function (error) { globalThis.error = error; });",
        )
        .expect("eval");
    runtime.run_microtasks().expect("microtasks");
    assert!(matches!(
        runtime.eval("error").expect("eval"),
        Value::String(message) if message.contains("missing.js")
    ));
}

#[test]
fn cycles_execute_each_module_once() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[
        (
            "a.js",
            "import { b } from './b.js';\n\
             export function a() { return 1; }\n\
             globalThis.runs = (globalThis.runs || 0) + 1;\n\
             globalThis.cycle = a() + b();",
        ),
        (
            "b.js",
            "import { a } from './a.js';\n\
             export function b() { return 2 + a(); }\n\
             globalThis.runs = (globalThis.runs || 0) + 1;",
        ),
    ]));

    runtime
        .import_module("a.js")
        .unwrap_or_else(|error| panic!("import failed: {error}"));
    // a() = 1, b() = 2 + a() = 3, total 4.
    assert_eq!(number(&mut runtime, "cycle"), 4.0);
    assert_eq!(number(&mut runtime, "runs"), 2.0);

    // A second import must not re-execute either module.
    runtime.import_module("b.js").expect("second import");
    assert_eq!(number(&mut runtime, "runs"), 2.0);
}

#[test]
fn exported_bindings_are_live() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[
        (
            "counter.js",
            "export let count = 1; export function inc() { count += 1; }",
        ),
        (
            "main.js",
            "import * as ns from './counter.js';\n\
             import { count, inc } from './counter.js';\n\
             globalThis.before = ns.count + count;\n\
             inc();\n\
             globalThis.after = ns.count + count;",
        ),
    ]));

    runtime.import_module("main.js").expect("import");
    assert_eq!(number(&mut runtime, "before"), 2.0);
    assert_eq!(number(&mut runtime, "after"), 4.0);
}

#[test]
fn imported_bindings_are_read_only() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[("value.js", "export let value = 1;")]));
    let error = runtime
        .evaluate_module(
            "assign.js",
            "import { value } from './value.js'; value = 2;",
        )
        .expect_err("assignment to an import should fail");
    assert!(
        error.message.contains("imported binding"),
        "unexpected error: {}",
        error.message
    );
}

#[test]
fn re_exports_and_side_effect_imports_work() {
    let mut runtime = Runtime::new();
    runtime.set_module_loader(loader(&[
        ("base.js", "export const x = 3; export const y = 4;"),
        (
            "reexport.js",
            "export { x as renamed } from './base.js';\n\
             export * from './base.js';",
        ),
        (
            "main.js",
            "import './side.js';\n\
             import * as ns from './reexport.js';\n\
             globalThis.total = ns.renamed + ns.x + ns.y + globalThis.side;",
        ),
        ("side.js", "globalThis.side = 5;"),
    ]));

    runtime.import_module("main.js").expect("import");
    assert_eq!(number(&mut runtime, "total"), 15.0);
}

#[test]
fn import_meta_exposes_url_and_relative_resolution() {
    let mut runtime = Runtime::new();
    let namespace = runtime
        .evaluate_module(
            "https://example.com/js/main.js",
            "globalThis.metaUrl = import.meta.url; \
             globalThis.resolved = import.meta.resolve('./other.js');",
        )
        .expect("evaluate module");
    assert!(matches!(namespace, Value::Object(_)));
    assert_eq!(
        runtime.eval("metaUrl").expect("eval"),
        Value::String("https://example.com/js/main.js".to_owned())
    );
    assert_eq!(
        runtime.eval("resolved").expect("eval"),
        Value::String("https://example.com/js/other.js".to_owned())
    );
}

#[test]
fn execution_budget_still_applies_inside_modules() {
    let mut runtime = Runtime::new();
    runtime.set_execution_budget(10);
    let error = runtime
        .evaluate_module("loop.js", "while (true) {}")
        .expect_err("budget should be exceeded");
    assert!(
        error.message.contains("execution budget"),
        "unexpected error: {}",
        error.message
    );
}
