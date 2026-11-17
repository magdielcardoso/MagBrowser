//! The `console` object and global error reporting.

use std::cell::RefCell;
use std::rc::Rc;

use browser_js::{Runtime, Value};

use crate::HostState;

/// Ensures `console.log/warn/error/info/debug` exist and write to the host log.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let existing = runtime.get_global("console");
    if existing.type_of() == "object" && runtime.object_has(&existing, "log") {
        // The runtime ships its own console bound to the console sink.
        return;
    }
    let console = runtime.new_object();
    for (level, prefix) in [
        ("log", ""),
        ("info", "[info] "),
        ("warn", "[warn] "),
        ("error", "[error] "),
        ("debug", "[debug] "),
    ] {
        let state = state.clone();
        let prefix = prefix.to_string();
        runtime.define_host_method(
            console.clone(),
            level,
            Box::new(move |_runtime, _this, args| {
                let line = args
                    .iter()
                    .map(Value::to_string_value)
                    .collect::<Vec<_>>()
                    .join(" ");
                if let Ok(mut lines) = state.borrow().console.lock() {
                    lines.push(format!("{prefix}{line}"));
                }
                Ok(Value::Undefined)
            }),
        );
    }
    runtime.set_global("console", console);
}

/// Records an uncaught script error in the host console log.
pub fn report_error(state: &Rc<RefCell<HostState>>, message: &str) {
    if let Ok(mut lines) = state.borrow().console.lock() {
        lines.push(format!("[uncaught] {message}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;

    #[test]
    fn console_lines_are_captured() {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(Document::new()));
        let state = crate::install(&mut runtime, document, None);
        runtime.eval("console.log('hello', 42)").unwrap();
        let lines = state.borrow_mut().take_console();
        assert!(lines.iter().any(|line| line.contains("hello")));
    }

    #[test]
    fn errors_are_prefixed() {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(Document::new()));
        let state = crate::install(&mut runtime, document, None);
        runtime.eval("console.error('boom')").unwrap();
        let lines = state.borrow_mut().take_console();
        assert!(lines.iter().any(|line| line.contains("boom")));
    }
}
