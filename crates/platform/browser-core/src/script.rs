//! Page script execution: `<script>` collection and the JS runtime bridge.

use browser_js::Runtime;

use crate::page::Page;

/// Collected inline scripts of a page, in document order.
pub fn collect_inline_scripts(page: &Page) -> Vec<String> {
    let document = page.document.borrow();
    let mut out = Vec::new();
    for node in document.get_elements_by_tag_name("script") {
        if document.has_attr(node, "src") {
            continue; // external scripts are fetched by the browser layer
        }
        let kind = document
            .attr(node, "type")
            .unwrap_or("text/javascript")
            .to_ascii_lowercase();
        if kind.contains("javascript") || kind.is_empty() {
            out.push(document.text_content(node));
        }
    }
    out
}

/// Creates the page's runtime and installs the web platform bindings.
pub fn ensure_runtime(page: &mut Page) -> bool {
    if page.runtime.is_some() {
        return true;
    }
    let mut runtime = Runtime::new();
    let url = page.url.as_ref().map(|url| url.as_str());
    let host = browser_web_apis::install(&mut runtime, page.document.clone(), url);
    page.runtime = Some(runtime);
    page.host = Some(host);
    true
}

/// Executes the page's inline scripts, returning how many ran and console output.
pub fn execute_in_page(page: &mut Page) -> (usize, Vec<String>) {
    let scripts = collect_inline_scripts(page);
    execute_scripts(page, &scripts)
}

/// Executes the given scripts in order, returning how many ran and console output.
pub fn execute_scripts(page: &mut Page, scripts: &[String]) -> (usize, Vec<String>) {
    if page.scripts_executed {
        return (0, Vec::new());
    }
    if scripts.is_empty() {
        return (0, Vec::new());
    }
    if !ensure_runtime(page) {
        return (0, Vec::new());
    }
    let Some(host) = page.host.clone() else {
        return (0, Vec::new());
    };
    let Some(mut runtime) = page.runtime.take() else {
        return (0, Vec::new());
    };

    page.scripts_executed = true;
    let mut executed = 0;
    for (index, script) in scripts.iter().enumerate() {
        let name = format!("inline-{index}");
        let _ = runtime.eval_named(script, &name);
        let _ = runtime.run_microtasks();
        executed += 1;
    }
    browser_web_apis::drain_timers_now(&mut runtime, &host);
    let _ = runtime.run_microtasks();

    page.runtime = Some(runtime);
    let output = host.borrow_mut().take_console();
    (executed, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_layout::Size;

    #[test]
    fn collects_inline_scripts_only() {
        let mut page = Page::new(Size::default());
        page.load_html(
            "<html><body><script>var a = 1;</script><script src='x.js'></script></body></html>",
            None,
        );
        let scripts = collect_inline_scripts(&page);
        assert_eq!(scripts.len(), 1);
        assert!(scripts[0].contains("var a = 1"));
    }

    #[test]
    fn scripts_mutate_the_dom() {
        let mut page = Page::new(Size::default());
        page.load_html(
            "<html><body><div id='out'>before</div><script>document.getElementById('out').textContent = 'after';</script></body></html>",
            None,
        );
        let (count, _console) = execute_in_page(&mut page);
        assert_eq!(count, 1);
        page.recompute();
        assert_eq!(page.text().trim(), "after");
    }
}
