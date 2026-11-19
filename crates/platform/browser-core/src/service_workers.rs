//! Host-side service worker bridge.
//!
//! `browser-web-apis` owns the JavaScript globals (`navigator.serviceWorker`,
//! `ServiceWorkerGlobalScope`) and a [`ServiceWorkerRegistry`] inside
//! `HostState`. Its script handler is intentionally simple: it records events
//! in an outbox so the page runtime can dispatch listeners later. That design
//! cannot synthesize a response *synchronously*, so `handle_fetch` alone never
//! intercepts a request.
//!
//! This module adds the missing host half without changing `browser-web-apis`:
//!
//! * [`ServiceWorkerMirror`] owns a second registry with a real script handler.
//!   The handler evaluates a worker's `install` / `activate` / `fetch`
//!   listeners in a fresh JavaScript runtime and returns a
//!   [`WorkerEventResult`]. A `fetch` listener that calls
//!   `event.respondWith(new Response(body, {status}))` therefore produces a
//!   real synthesized response.
//! * [`install_bridge`] installs a forwarding handler on the `HostState`
//!   registry. Every event is forwarded to the mirror synchronously (so
//!   interception works) *and* queued in the web-apis outbox (so `run_events`
//!   still dispatches listeners in the page's runtime).
//! * The mirror is captured by the `fetch` bridge in [`crate::script`], which
//!   calls `handle_fetch` before touching the network. Capturing an
//!   `Arc<Mutex<…>>` avoids re-borrowing `HostState` from inside `fetch()`,
//!   where the immutable borrow taken by `perform_fetch` is still live.
//!
//! Registrations and script sources made by JavaScript through `HostState`
//! are mirrored after each script batch by [`ServiceWorkerMirror::sync`].
//! Workers registered through the host API are available immediately.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use browser_event_loop::{
    Registration, ServiceWorkerEvent, ServiceWorkerRegistry, WorkerEventKind, WorkerEventResult,
    WorkerScriptHandler, WorkerState,
};
use browser_js::Runtime;
use browser_url::Url;
use browser_web_apis::HostState;

/// A JS harness installed before a worker script. It provides the small slice
/// of the service worker global scope the engine needs: listener registration,
/// `self`, a `Response` constructor and a no-op `waitUntil`/`skipWaiting`.
const HARNESS: &str = r#"
var __listeners = { install: [], activate: [], fetch: [] };
var __result = '';
function addEventListener(type, fn) {
  if (__listeners[type] && typeof fn === 'function') __listeners[type].push(fn);
}
var self = globalThis;
var ServiceWorkerGlobalScope = globalThis;
self.addEventListener = addEventListener;
self.skipWaiting = function () {};
self.clients = { claim: function () {} };
function Response(body, init) {
  this.body = body === undefined ? '' : String(body);
  this.status = (init && init.status) || 200;
  this.statusText = (init && init.statusText) || 'OK';
  this.headers = (init && init.headers) || {};
}
var Event = function (type) { this.type = type; };
"#;

/// True when `url` is a secure context (HTTPS, or loopback HTTP for tests).
pub fn is_secure_context(url: &str) -> bool {
    match Url::parse(url) {
        Ok(url) if url.scheme() == "https" => true,
        Ok(url) if url.scheme() == "http" => matches!(
            url.host(),
            Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
        ),
        _ => false,
    }
}

/// Normalises a scope to an absolute URL so prefix matching respects origin.
fn absolute_scope(scope: &str, base: Option<&Url>) -> Option<String> {
    if let Ok(url) = Url::parse(scope) {
        return url.scheme().contains("http").then(|| url.as_str());
    }
    let base = base?;
    base.join(scope).ok().map(|url| url.as_str())
}

/// Escapes a string for embedding in a JavaScript string literal.
fn js_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Dispatch source appended after each worker script for one event.
fn dispatch_source(event: &ServiceWorkerEvent) -> String {
    let kind = event.kind().as_str();
    let (url, scope) = match event {
        ServiceWorkerEvent::Fetch { scope, url } => (url.as_str(), scope.as_str()),
        ServiceWorkerEvent::Install { scope, script_url }
        | ServiceWorkerEvent::Activate { scope, script_url } => {
            (script_url.as_str(), scope.as_str())
        }
    };
    let scope = js_string(scope);
    let url = js_string(url);
    let mut source = format!(
        "try {{ var __event = {{ type: {kind:?}, scope: {scope}, \
         request: {{ url: {url}, method: 'GET' }}, \
         waitUntil: function () {{}}, \
         respondWith: function (r) {{ __event._response = r; }} }}; \
         for (var __i = 0; __i < __listeners[{kind:?}].length; __i++) \
             __listeners[{kind:?}][__i](__event);"
    );
    source.push_str("__result = 'ok';");
    if event.kind() == WorkerEventKind::Fetch {
        source.push_str(
            "if (__event._response) { var __r = __event._response; \
             __result = 'respond|' + (__r.status || 200) + '|' + \
             (__r.statusText || 'OK') + '|' + String(__r.body || '').length + '|' + \
             String(__r.body || ''); } else { __result = 'pass'; }",
        );
    }
    source.push_str("} catch (e) { __result = 'error: ' + (e && e.message ? e.message : e); }");
    source
}

fn evaluate(source: &str, event: &ServiceWorkerEvent) -> WorkerEventResult {
    let mut runtime = Runtime::new();
    let mut script = String::from(HARNESS);
    script.push_str(source);
    script.push_str(&dispatch_source(event));
    if runtime.eval_named(&script, "service-worker").is_err() {
        return match event.kind() {
            WorkerEventKind::Fetch => WorkerEventResult::fetch(None),
            WorkerEventKind::Install => WorkerEventResult::install(false),
            WorkerEventKind::Activate => WorkerEventResult::activate(false),
        };
    }
    let result = runtime.get_global("__result").to_string_value();
    match event.kind() {
        WorkerEventKind::Fetch => parse_fetch_result(&result),
        WorkerEventKind::Install => WorkerEventResult::install(result == "ok"),
        WorkerEventKind::Activate => WorkerEventResult::activate(result == "ok"),
    }
}

fn parse_fetch_result(result: &str) -> WorkerEventResult {
    if result == "pass" || !result.starts_with("respond|") {
        return WorkerEventResult::fetch(None);
    }
    let fields: Vec<&str> = result.splitn(5, '|').collect();
    let status = fields
        .get(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(200);
    let status_text = fields.get(2).copied().unwrap_or("OK").to_string();
    let body = fields.get(4).copied().unwrap_or("").as_bytes().to_vec();
    let response = browser_event_loop::Response::new(
        status,
        status_text,
        vec![(
            "content-type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        )],
        body,
    );
    WorkerEventResult::fetch(Some(response))
}

/// Host-owned service worker state, shared with the fetch bridge.
#[derive(Clone)]
pub struct ServiceWorkerMirror {
    registry: Arc<Mutex<ServiceWorkerRegistry>>,
    scripts: Arc<Mutex<HashMap<String, String>>>,
    outbox: Arc<Mutex<Vec<ServiceWorkerEvent>>>,
}

impl ServiceWorkerMirror {
    /// Creates an empty mirror.
    pub fn new() -> Self {
        let scripts: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let outbox: Arc<Mutex<Vec<ServiceWorkerEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let mirror_scripts = Arc::clone(&scripts);
        let mirror_outbox = Arc::clone(&outbox);
        let handler: WorkerScriptHandler = Arc::new(move |event| {
            if let Ok(mut events) = mirror_outbox.lock() {
                events.push(event.clone());
            }
            let source = event_script_url(event)
                .and_then(|url| {
                    mirror_scripts
                        .lock()
                        .ok()
                        .and_then(|map| map.get(url).cloned())
                })
                .or_else(|| {
                    mirror_scripts
                        .lock()
                        .ok()
                        .and_then(|map| map.get(event.scope()).cloned())
                })
                .unwrap_or_default();
            if source.is_empty() {
                return match event.kind() {
                    WorkerEventKind::Fetch => WorkerEventResult::fetch(None),
                    WorkerEventKind::Install => WorkerEventResult::install(true),
                    WorkerEventKind::Activate => WorkerEventResult::activate(true),
                };
            }
            evaluate(&source, event)
        });
        let mut registry = ServiceWorkerRegistry::new();
        registry.set_script_handler(handler.clone());
        Self {
            registry: Arc::new(Mutex::new(registry)),
            scripts,
            outbox,
        }
    }

    /// The registry the fetch bridge should consult before the network.
    pub fn registry(&self) -> Arc<Mutex<ServiceWorkerRegistry>> {
        Arc::clone(&self.registry)
    }

    /// Records worker script source for a script URL.
    pub fn set_script(&self, script_url: impl Into<String>, source: impl Into<String>) {
        if let Ok(mut scripts) = self.scripts.lock() {
            scripts.insert(script_url.into(), source.into());
        }
    }

    /// Registers a worker, enforcing HTTPS (or loopback) and origin-scoped scope.
    pub fn register(
        &self,
        scope: &str,
        script_url: &str,
        source: Option<&str>,
        base: Option<&Url>,
    ) -> Result<(), String> {
        if !is_secure_context(script_url) {
            return Err("service workers require a secure (HTTPS) script URL".to_string());
        }
        let scope = absolute_scope(scope, base)
            .ok_or_else(|| "invalid service worker scope".to_string())?;
        if let Some(source) = source {
            self.set_script(script_url, source);
            self.set_script(&scope, source);
        }
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "service worker registry poisoned".to_string())?;
        registry
            .register(scope, script_url)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Activates a registered worker.
    pub fn activate(&self, scope: &str) -> Result<(), String> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| "service worker registry poisoned".to_string())?;
        registry.activate(scope).map_err(|error| error.to_string())
    }

    /// Unregisters a worker.
    pub fn unregister(&self, scope: &str) -> bool {
        self.registry
            .lock()
            .is_ok_and(|mut registry| registry.unregister(scope))
    }

    /// Intercepts a URL on behalf of an active worker.
    pub fn handle_fetch(&self, url: &str) -> Option<browser_event_loop::Response> {
        if !is_secure_context(url) {
            return None;
        }
        self.registry
            .lock()
            .ok()
            .and_then(|mut registry| registry.handle_fetch(url))
    }

    /// A snapshot of registrations for inspection.
    pub fn registrations(&self) -> Vec<Registration> {
        self.registry
            .lock()
            .map_or_else(|_| Vec::new(), |registry| registry.registrations().to_vec())
    }

    /// Events awaiting JavaScript dispatch.
    pub fn take_events(&self) -> Vec<ServiceWorkerEvent> {
        self.outbox
            .lock()
            .map_or_else(|_| Vec::new(), |mut events| std::mem::take(&mut *events))
    }

    /// Mirrors registrations and script sources made through the web-apis
    /// globals into this host-owned registry.
    ///
    /// Call after a script batch (see [`crate::script::execute_scripts`]) so
    /// later fetches see workers registered by JavaScript. `run_events` in
    /// `browser-web-apis` remains responsible for dispatching listeners in the
    /// page's own runtime.
    pub fn sync(&self, state: &HostState) {
        let base = state.url.as_ref().and_then(|url| Url::parse(url).ok());
        let sources: Vec<(String, String)> = state
            .service_worker_scripts
            .iter()
            .map(|(url, source)| (url.clone(), source.clone()))
            .collect();
        if let Ok(mut scripts) = self.scripts.lock() {
            for (url, source) in sources {
                // Web-apis keys sources by the raw script URL used at
                // registration time; the mirror also needs the absolute form.
                let absolute = base
                    .as_ref()
                    .and_then(|base| base.join(&url).ok())
                    .map(|url| url.as_str());
                scripts.insert(url.clone(), source.clone());
                if let Some(absolute) = absolute {
                    scripts.insert(absolute.to_string(), source);
                }
            }
        }
        let registrations: Vec<Registration> = state.service_workers.registrations().to_vec();
        let mut registry = match self.registry.lock() {
            Ok(registry) => registry,
            Err(_) => return,
        };
        let known: Vec<String> = registry
            .registrations()
            .iter()
            .map(|registration| registration.scope.clone())
            .collect();
        for registration in registrations {
            if registration.state == WorkerState::Redundant {
                continue;
            }
            let scope = absolute_scope(&registration.scope, base.as_ref())
                .unwrap_or_else(|| registration.scope.clone());
            let script_url = base
                .as_ref()
                .and_then(|base| base.join(&registration.script_url).ok())
                .map_or_else(
                    || registration.script_url.clone(),
                    |url| url.as_str().to_string(),
                );
            // Fetch events carry no script URL, so the handler also looks the
            // worker source up by scope.
            if let Some(source) = state.service_worker_scripts.get(&registration.script_url)
                && let Ok(mut scripts) = self.scripts.lock()
            {
                scripts.insert(scope.clone(), source.clone());
            }
            if !known.contains(&scope) {
                let _ = registry.register(&scope, &script_url);
            }
            if registration.state == WorkerState::Active
                && registry
                    .get(&scope)
                    .is_some_and(|current| current.state != WorkerState::Active)
            {
                let _ = registry.activate(&scope);
            }
        }
    }
}

impl Default for ServiceWorkerMirror {
    fn default() -> Self {
        Self::new()
    }
}

/// Descriptive alias for the host-side service worker bridge.
pub type ServiceWorkerBridge = ServiceWorkerMirror;

impl std::fmt::Debug for ServiceWorkerMirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceWorkerMirror")
            .field("registrations", &self.registrations().len())
            .finish()
    }
}

fn event_script_url(event: &ServiceWorkerEvent) -> Option<&str> {
    match event {
        ServiceWorkerEvent::Install { script_url, .. }
        | ServiceWorkerEvent::Activate { script_url, .. } => Some(script_url.as_str()),
        ServiceWorkerEvent::Fetch { .. } => None,
    }
}

/// Installs the forwarding handler on `HostState`'s registry.
///
/// The web-apis `install` function is called first (it installs its own
/// outbox-only handler); this override keeps the outbox behaviour and adds
/// synchronous evaluation for fetch interception.
pub fn install_bridge(host: &Rc<RefCell<HostState>>, mirror: &ServiceWorkerMirror) {
    let outbox = host.borrow().service_worker_events.clone();
    let mirror_registry = mirror.registry();
    let handler: WorkerScriptHandler = Arc::new(move |event| {
        if let Ok(mut events) = outbox.lock() {
            events.push(event.clone());
        }
        // Lifecycle listeners run once, in the page runtime, through web-apis'
        // `run_events`; the host bridge only reports success here. Fetch events
        // need a synchronous result, so those are evaluated against the mirror.
        match event.kind() {
            WorkerEventKind::Install => WorkerEventResult::install(true),
            WorkerEventKind::Activate => WorkerEventResult::activate(true),
            WorkerEventKind::Fetch => {
                let url = match event {
                    ServiceWorkerEvent::Fetch { url, .. } => url.as_str(),
                    _ => event.scope(),
                };
                mirror_registry
                    .lock()
                    .ok()
                    .and_then(|mut registry| registry.handle_fetch(url))
                    .map_or_else(
                        || WorkerEventResult::fetch(None),
                        |response| WorkerEventResult::fetch(Some(response)),
                    )
            }
        }
    });
    host.borrow_mut()
        .service_workers
        .set_script_handler(handler);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_context_requires_https_or_loopback() {
        assert!(is_secure_context("https://example.com/app"));
        assert!(is_secure_context("http://localhost:8080/app"));
        assert!(is_secure_context("http://127.0.0.1/app"));
        assert!(!is_secure_context("http://example.com/app"));
        assert!(!is_secure_context("ftp://example.com/app"));
    }

    #[test]
    fn evaluates_fetch_listener_and_responds() {
        let source = "ServiceWorkerGlobalScope.addEventListener('fetch', function (e) { \
                       e.respondWith(new Response('from-worker', {status: 201, statusText: 'Created'})); \
                     });";
        let event = ServiceWorkerEvent::Fetch {
            scope: "https://example.com/app/".to_string(),
            url: "https://example.com/app/data".to_string(),
        };
        match evaluate(source, &event) {
            WorkerEventResult::Fetched {
                response: Some(response),
            } => {
                assert_eq!(response.status, 201);
                assert_eq!(response.status_text, "Created");
                assert_eq!(response.body, b"from-worker");
            }
            other => panic!("expected synthesized response, got {other:?}"),
        }
    }

    #[test]
    fn missing_listener_passes_through() {
        let event = ServiceWorkerEvent::Fetch {
            scope: "https://example.com/".to_string(),
            url: "https://example.com/data".to_string(),
        };
        assert_eq!(evaluate("", &event), WorkerEventResult::fetch(None));
    }

    #[test]
    fn registration_requires_secure_context() {
        let mirror = ServiceWorkerMirror::new();
        let base = Url::parse("https://example.com/app/").unwrap();
        assert!(
            mirror
                .register("/app/", "http://example.com/sw.js", None, Some(&base))
                .is_err()
        );
        mirror
            .register("/app/", "https://example.com/sw.js", None, Some(&base))
            .unwrap();
        assert_eq!(mirror.registrations().len(), 1);
    }
}
