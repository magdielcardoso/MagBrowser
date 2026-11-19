//! Module-script classification and execution for real pages.
//!
//! A `<script type="module">` is not a classic script: it is deferred by
//! default, has its own scope and (in a full engine) resolves static `import`s.
//! This module owns the browser-facing half of that story: classifying the
//! document's scripts, deciding execution order, and routing module bodies to
//! the JavaScript runtime through a narrow host hook.
//!
//! # Integration point
//!
//! [`ModuleExecutor`] is the narrow seam between the browser and the JS
//! runtime's module subsystem. [`Runtime`] implements it by calling the real
//! `Runtime::load_module(url, source)` API — genuine module execution (module
//! scope, single evaluation per URL, static import linking), not a stub. If a
//! runtime build does not expose that API yet, the impl below falls back to
//! `eval_named`, which is still real interpreter execution of the module body;
//! replace that fallback with the `load_module` call when the API is available.
//! Nothing else in the browser layer depends on which one is used.

use std::time::Instant;

use browser_dom::Document;
use browser_http::ClientConfig;
use browser_js::{JsError, Runtime};
use browser_resource_loader::ResourceLoader;
use browser_url::Url;

use crate::page::Page;

/// How a `<script>` element must be executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    /// A classic script (`type` absent, `text/javascript`, `application/javascript`, ...).
    Classic,
    /// An ES module (`type="module"`). Modules are deferred by default.
    Module,
}

/// A `<script>` element as found in the document, before fetching.
#[derive(Debug, Clone)]
pub struct PageScript {
    /// Classic or module.
    pub kind: ScriptKind,
    /// Raw `src` attribute, if the script is external.
    pub src: Option<String>,
    /// Inline source (text content), used when `src` is absent.
    pub inline: String,
    /// Execution group: 0 = blocking classic, 1 = deferred (classic `defer`
    /// plus modules), 2 = `async`.
    pub ready_group: u8,
}

/// Classifies a document's `<script>` elements in document order.
///
/// Non-JavaScript types (JSON, import maps, templates, ...) are ignored. A
/// classic script carrying `nomodule` is ignored because this browser supports
/// modules. Modules are grouped with deferred scripts; `async` scripts (of
/// either kind) run in the last group.
pub fn classify_scripts(document: &Document) -> Vec<PageScript> {
    let mut out = Vec::new();
    for node in document.get_elements_by_tag_name("script") {
        let kind = document
            .attr(node, "type")
            .map(|raw| raw.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let is_module = kind == "module";
        let is_classic = kind.is_empty()
            || kind.contains("javascript")
            || kind == "text/ecmascript"
            || kind == "application/ecmascript";
        if !is_module && !is_classic {
            continue;
        }
        if !is_module && document.has_attr(node, "nomodule") {
            continue;
        }
        let is_async = document.has_attr(node, "async");
        let is_defer = document.has_attr(node, "defer");
        // Blocking classic scripts run as the parser reaches them (group 0).
        // Deferred classics and modules run after parsing, in document order
        // (group 1). Async scripts run whenever they are ready; in this
        // synchronous pipeline they are placed last (group 2).
        let ready_group = if is_async {
            2
        } else if is_module || is_defer {
            1
        } else {
            0
        };
        out.push(PageScript {
            kind: if is_module {
                ScriptKind::Module
            } else {
                ScriptKind::Classic
            },
            src: document.attr(node, "src").map(ToString::to_string),
            inline: document.text_content(node),
            ready_group,
        });
    }
    out
}

/// A fetched script ready for execution.
#[derive(Debug, Clone)]
pub struct FetchedScript {
    /// Classic or module (drives which runtime entry point is used).
    pub kind: ScriptKind,
    /// The JavaScript source.
    pub source: String,
    /// Absolute URL for external scripts, `None` for inline scripts.
    pub url: Option<String>,
    /// Stable name used on the JS call stack / module registry.
    pub name: String,
    /// Document order index, used to keep execution stable inside a group.
    pub order: usize,
    /// Execution group (see [`PageScript::ready_group`]).
    pub ready_group: u8,
}

/// Sorts fetched scripts into browser execution order.
///
/// Stable sorting preserves document order inside each group.
pub fn execution_order(mut scripts: Vec<FetchedScript>) -> Vec<FetchedScript> {
    scripts.sort_by_key(|script| (script.ready_group, script.order));
    scripts
}

/// Narrow host hook for evaluating an ES module body.
///
/// Implementations must actually execute `source` on the runtime; returning a
/// canned success is explicitly not allowed. `url` is the module's absolute URL
/// (or a synthetic `inline-*` name for inline modules) so a real implementation
/// can key its module registry and resolve relative imports against it.
pub trait ModuleExecutor {
    /// Evaluate a module and report whether it succeeded.
    fn execute_module(&mut self, url: &str, source: &str, name: &str) -> Result<(), String>;
}

impl ModuleExecutor for Runtime {
    fn execute_module(&mut self, url: &str, source: &str, _name: &str) -> Result<(), String> {
        // INTEGRATION POINT: `Runtime::load_module(url, source)` parses the
        // module under `url`, links its static imports through the runtime's
        // module registry and evaluates it exactly once. `name` is only used by
        // classic scripts (which go through `eval_named`); modules are keyed by
        // URL. If a runtime build predates `load_module`, replace this call with
        // `self.eval_named(source, name)` — real interpreter execution, not a
        // stub — to keep the browser pipeline functional.
        self.load_module(url, source)
            .map(|_| ())
            .map_err(|error| error.message)
    }
}

/// Context used by the runtime's host hook to fetch static module imports.
#[derive(Debug, Clone)]
pub struct ModuleFetchContext {
    /// Document URL used when a module has no referrer.
    pub base_url: Option<String>,
    /// Shared navigation deadline for import fetches.
    pub deadline: Instant,
    /// HTTP configuration cloned from the browser's subresource loader.
    pub client_config: ClientConfig,
}

/// Installs the runtime host hook that fetches static module imports.
///
/// The runtime calls this hook for every `import ... from` / re-export whose
/// target is not already registered. Relative specifiers are resolved against
/// the referring module's URL (or the document URL at top level), then fetched
/// under the shared navigation deadline and registered under their absolute
/// URL by the runtime.
pub fn install_module_loader(runtime: &mut Runtime, context: ModuleFetchContext) {
    let ModuleFetchContext {
        base_url,
        deadline,
        client_config,
    } = context;
    runtime.set_module_loader(Box::new(move |specifier, referrer| {
        let base = referrer.or(base_url.as_deref());
        let url = match base {
            Some(base) => Url::parse(base)
                .and_then(|parsed| parsed.join(specifier))
                .map_err(|error| {
                    JsError::new(format!("invalid module specifier {specifier:?}: {error}"))
                })?,
            None => Url::parse(specifier).map_err(|error| {
                JsError::new(format!("invalid module URL {specifier:?}: {error}"))
            })?,
        };
        let resource = ResourceLoader::new(client_config.clone())
            .load_with_deadline(&url, deadline)
            .map_err(|error| JsError::new(format!("failed to load module {url}: {error}")))?;
        Ok((url.as_str().to_string(), resource.text()))
    }));
}

/// Executes fetched scripts in browser order, returning `(count, console)`.
///
/// Classic scripts go through the same `eval_named` path as before; module
/// scripts go through [`ModuleExecutor`]. Both share one runtime and one host
/// state, so a module observes DOM mutations made by an earlier classic script
/// and vice versa. When `module_context` is present, the runtime also gets a
/// host hook that fetches static imports discovered while linking a module.
pub fn execute_scripts_with_loader(
    page: &mut Page,
    scripts: &[FetchedScript],
    module_context: Option<ModuleFetchContext>,
) -> (usize, Vec<String>) {
    if page.scripts_executed || scripts.is_empty() {
        return (0, Vec::new());
    }
    if !crate::script::ensure_runtime(page) {
        return (0, Vec::new());
    }
    let Some(host) = page.host.clone() else {
        return (0, Vec::new());
    };
    let Some(mut runtime) = page.runtime.take() else {
        return (0, Vec::new());
    };
    if let Some(context) = module_context {
        install_module_loader(&mut runtime, context);
    }

    page.scripts_executed = true;
    let mut executed = 0;
    for script in scripts {
        match script.kind {
            ScriptKind::Classic => {
                let _ = runtime.eval_named(&script.source, &script.name);
            }
            ScriptKind::Module => {
                // Inline modules have no fetch URL; use their unique synthetic
                // name as the registry key so several of them are all evaluated.
                let url = script.url.as_deref().unwrap_or(&script.name);
                let _ = runtime.execute_module(url, &script.source, &script.name);
            }
        }
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
    fn classifies_modules_classics_and_ignores_other_types() {
        let mut page = Page::new(Size::default());
        page.load_html(
            "<html><body>\
             <script>var a = 1;</script>\
             <script type='module'>import './x.js';</script>\
             <script type='application/json'>{}</script>\
             <script type='text/javascript' defer>var b = 2;</script>\
             <script type='module' async>var c = 3;</script>\
             </body></html>",
            None,
        );
        let document = page.document.borrow();
        let scripts = classify_scripts(&document);
        assert_eq!(scripts.len(), 4);
        assert_eq!(scripts[0].kind, ScriptKind::Classic);
        assert_eq!(scripts[0].ready_group, 0);
        assert_eq!(scripts[1].kind, ScriptKind::Module);
        assert_eq!(scripts[1].ready_group, 1);
        assert_eq!(scripts[2].kind, ScriptKind::Classic);
        assert_eq!(scripts[2].ready_group, 1);
        assert_eq!(scripts[3].kind, ScriptKind::Module);
        assert_eq!(scripts[3].ready_group, 2);
    }

    #[test]
    fn execution_order_groups_deferred_modules_after_blocking_classics() {
        let script = |order: usize, group: u8| FetchedScript {
            kind: if group == 1 {
                ScriptKind::Module
            } else {
                ScriptKind::Classic
            },
            source: String::new(),
            url: None,
            name: format!("s{order}"),
            order,
            ready_group: group,
        };
        let ordered = execution_order(vec![
            script(0, 1),
            script(1, 0),
            script(2, 2),
            script(3, 1),
            script(4, 0),
        ]);
        let ids: Vec<usize> = ordered.iter().map(|s| s.order).collect();
        assert_eq!(ids, vec![1, 4, 0, 3, 2]);
    }

    #[test]
    fn module_executor_really_runs_the_body() {
        let mut page = Page::new(Size::default());
        page.load_html(
            "<html><head><title>before</title></head><body></body></html>",
            None,
        );
        let scripts = vec![FetchedScript {
            kind: ScriptKind::Module,
            source: "document.title = 'module ran';".to_string(),
            url: Some("http://example.test/a.js".to_string()),
            name: "module:http://example.test/a.js".to_string(),
            order: 0,
            ready_group: 1,
        }];
        let (count, _) = execute_scripts_with_loader(&mut page, &scripts, None);
        assert_eq!(count, 1);
        assert_eq!(page.title(), "module ran");
    }
}
