//! Web platform bindings: the `document`, `window` and DOM objects exposed to
//! JavaScript, plus the timer loop.
//!
//! The bindings are thin: every call goes straight to [`browser_dom`] through the
//! shared document handle, so a script mutation is immediately visible to the
//! style and layout engines. There is no shadow copy of the DOM.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use browser_dom::Document;
//! use browser_js::Runtime;
//! use browser_web_apis::install;
//!
//! let mut runtime = Runtime::new();
//! let document = Rc::new(RefCell::new(Document::new()));
//! let host = install(&mut runtime, document, None);
//! runtime.eval("document.title = 'demo'").unwrap();
//! assert_eq!(runtime.eval("document.title").unwrap().to_string_value(), "demo");
//! assert!(host.borrow().events.is_empty());
//! ```

#![forbid(unsafe_code)]

pub mod abort;
pub mod console;
pub mod cssom;
pub mod custom_elements;
pub mod deferred;
pub mod document;
pub mod element;
pub mod events;
pub mod fetch;
pub mod form;
pub mod globals;
pub mod indexed_db;
pub mod location;
pub mod observers;
pub mod service_worker;
pub mod shadow;
pub mod storage;
pub mod timers;
pub mod traversal;
pub mod url_search_params;
pub mod websocket;
pub mod xhr;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use browser_dom::{Document, NodeId};
use browser_event_loop::{
    ServiceWorkerEvent, ServiceWorkerRegistry, WebSocketManager, WebSocketTransport,
};
use browser_js::{Runtime, Value};

use abort::AbortSignalState;
use fetch::{FetchHandler, FetchRequest, FetchResponse};

pub use fetch::{FetchError, FetchHandler as FetchHostHandler};
pub use form::{FormSubmission, default_submit_button, form_for_input, single_text_control};

/// A pending timer.
pub struct Timer {
    /// Timer id returned to scripts.
    pub id: u64,
    /// When the callback becomes due.
    pub deadline: Instant,
    /// Repeat period for `setInterval`.
    pub period: Option<std::time::Duration>,
    /// The callback function value.
    pub callback: Value,
    /// Arguments passed to the callback.
    pub args: Vec<Value>,
}

/// Resolver used by `getComputedStyle`; supplied by the embedder because the
/// computed style tree lives in `browser-core`.
pub type ComputedStyleResolver = Box<dyn Fn(NodeId, &str) -> Option<String>>;

/// State shared between the runtime's host functions.
pub struct HostState {
    /// The live document.
    pub document: Rc<RefCell<Document>>,
    /// Registered DOM listeners keyed by `(node id, event type)`.
    pub events: HashMap<(usize, String), Vec<Value>>,
    /// Pending timers.
    pub timers: Vec<Timer>,
    /// Next timer id.
    pub next_timer_id: u64,
    /// Console lines produced by scripts (shared with the runtime sink).
    pub console: Arc<Mutex<Vec<String>>>,
    /// `localStorage` contents.
    pub storage: HashMap<String, String>,
    /// A navigation requested by a script (`location.href = ...`).
    pub navigation: Option<String>,
    /// A form submission requested by a script (`form.submit()` /
    /// `form.requestSubmit()`) or by implicit Enter activation.
    pub form_submission: Option<FormSubmission>,
    /// The currently focused element, set by `focus()` and cleared by `blur()`.
    pub focused: Option<NodeId>,
    /// Cache of element wrapper objects keyed by node id.
    pub elements: HashMap<usize, Value>,
    /// The document URL, if any.
    pub url: Option<String>,
    /// Optional resolver for computed CSS values. The embedder installs one
    /// when a computed style tree is available; `window.getComputedStyle`
    /// returns empty strings while this is `None`.
    pub computed_style: Option<ComputedStyleResolver>,
    /// Host bridge for `fetch()` and `XMLHttpRequest`. Without one, both APIs
    /// reject with a `TypeError` explaining that networking is unavailable.
    pub fetch_handler: Option<FetchHandler>,
    /// Abort signals created by `AbortController`, keyed by their numeric id.
    pub abort_signals: HashMap<u64, AbortSignalState>,
    /// Next abort signal id.
    pub next_abort_id: u64,
    /// Intersection, resize and mutation observers plus their host hooks.
    pub observers: observers::Observers,
    /// `customElements` host bookkeeping: callback handles, JS constructors and
    /// promises created by `whenDefined`.
    pub custom_elements: custom_elements::CustomElementHost,
    /// Shared state of the default host WebSocket transport, used to inject
    /// inbound events and inspect outbound frames without a network.
    pub websocket_queue: websocket::HostWebSocketQueue,
    /// Host-side service worker registry and the worker scripts/listeners used
    /// by the script handler bridge.
    pub service_workers: ServiceWorkerRegistry,
    /// Worker script source, keyed by script URL, evaluated by the bridge.
    pub service_worker_scripts: HashMap<String, String>,
    /// Worker event listeners registered from JavaScript, keyed by event type.
    pub service_worker_listeners: HashMap<String, Vec<Value>>,
    /// Script URLs already evaluated by the service worker bridge.
    pub service_worker_evaluated: std::collections::HashSet<String>,
    /// Worker events recorded by the installed script handler, awaiting JS
    /// dispatch.
    pub service_worker_events: Arc<Mutex<Vec<ServiceWorkerEvent>>>,
    /// Managed WebSocket connections. The default transport is an in-memory
    /// one; embedders may replace it with a real network transport.
    pub websocket_manager: WebSocketManager<Box<dyn WebSocketTransport>>,
    /// JS event handlers for each managed WebSocket, keyed by connection id.
    pub websocket_handlers: HashMap<u64, websocket::WebSocketJsHandlers>,
    /// Databases opened through the `indexedDB` global, keyed by name.
    pub indexed_db: indexed_db::IdbHost,
    /// Live IndexedDB transaction objects created from JavaScript.
    pub idb_transactions: HashMap<u64, indexed_db::IdbTransactionHost>,
    /// Next IndexedDB transaction id.
    pub next_idb_tx: u64,
}

impl HostState {
    /// Creates state for a document.
    pub fn new(document: Rc<RefCell<Document>>, url: Option<String>) -> Self {
        let websocket_queue = websocket::HostWebSocketQueue::default();
        Self {
            document,
            events: HashMap::new(),
            timers: Vec::new(),
            next_timer_id: 1,
            console: Arc::new(Mutex::new(Vec::new())),
            storage: HashMap::new(),
            navigation: None,
            form_submission: None,
            focused: None,
            elements: HashMap::new(),
            url,
            computed_style: None,
            fetch_handler: None,
            abort_signals: HashMap::new(),
            next_abort_id: 0,
            observers: observers::Observers::new(),
            custom_elements: custom_elements::CustomElementHost::new(),
            service_workers: ServiceWorkerRegistry::new(),
            service_worker_scripts: HashMap::new(),
            service_worker_listeners: HashMap::new(),
            service_worker_evaluated: std::collections::HashSet::new(),
            service_worker_events: Arc::new(Mutex::new(Vec::new())),
            websocket_queue: websocket_queue.clone(),
            websocket_manager: WebSocketManager::new(Box::new(websocket::HostWebSocketTransport {
                shared: websocket_queue,
            })),
            websocket_handlers: HashMap::new(),
            indexed_db: indexed_db::IdbHost::new(),
            idb_transactions: HashMap::new(),
            next_idb_tx: 1,
        }
    }

    /// Installs the resolver used by `window.getComputedStyle`.
    pub fn set_computed_style_resolver(
        &mut self,
        resolver: impl Fn(NodeId, &str) -> Option<String> + 'static,
    ) {
        self.computed_style = Some(Box::new(resolver));
    }

    /// Installs the host callback that services `fetch` and `XMLHttpRequest`.
    pub fn set_fetch_handler(&mut self, handler: FetchHandler) {
        self.fetch_handler = Some(handler);
    }

    /// Runs the installed fetch bridge.
    pub fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        fetch::perform_fetch(self, request)
    }

    /// Installs the callback that maps a node to its current layout box. Called
    /// by browser-core once a layout tree is available; observers use it to
    /// compute `boundingClientRect`, `contentRect` and intersection ratios.
    pub fn set_layout_box_resolver(
        &mut self,
        resolver: impl Fn(NodeId) -> Option<observers::Rect> + 'static,
    ) {
        self.observers.set_layout_box_resolver(resolver);
    }

    /// Records a console line (used by the host to surface script errors).
    pub fn push_console(&mut self, line: impl Into<String>) {
        if let Ok(mut lines) = self.console.lock() {
            lines.push(line.into());
        }
    }

    /// Drains collected console output.
    pub fn take_console(&mut self) -> Vec<String> {
        match self.console.lock() {
            Ok(mut lines) => std::mem::take(&mut *lines),
            Err(_) => Vec::new(),
        }
    }

    /// Takes the pending navigation request.
    pub fn take_navigation(&mut self) -> Option<String> {
        self.navigation.take()
    }

    /// Takes a pending form submission request, if any.
    pub fn take_form_submission(&mut self) -> Option<FormSubmission> {
        self.form_submission.take()
    }
}

/// Installs the web platform bindings on a runtime.
///
/// Returns the shared host state, which the embedder uses to drain console output,
/// dispatch events and run timers.
pub fn install(
    runtime: &mut Runtime,
    document: Rc<RefCell<Document>>,
    url: Option<String>,
) -> Rc<RefCell<HostState>> {
    let state = Rc::new(RefCell::new(HostState::new(document, url)));

    let sink_console = state.borrow().console.clone();
    runtime.set_console_sink(Box::new(move |line: &str| {
        if let Ok(mut lines) = sink_console.lock() {
            lines.push(line.to_string());
        }
    }));

    console::install(runtime, &state);
    document::install(runtime, &state);
    traversal::install(runtime, &state);
    timers::install(runtime, &state);
    storage::install(runtime, &state);
    location::install(runtime, &state);
    events::install_global(runtime, &state);
    cssom::install_global(runtime, &state);
    abort::install(runtime, &state);
    fetch::install(runtime, &state);
    url_search_params::install(runtime);
    xhr::install(runtime, &state);
    observers::install(runtime, &state);
    custom_elements::install(runtime, &state);
    shadow::install(runtime, &state);
    service_worker::install(runtime, &state);
    websocket::install(runtime, &state);
    indexed_db::install(runtime, &state);
    globals::install(runtime, &state);

    state
}

/// Runs every timer that is due at `now`.
pub fn drain_timers(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, now: Instant) -> usize {
    timers::drain_due(runtime, state, now)
}

/// Aliases [`drain_timers`] with the current clock.
pub fn drain_timers_now(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> usize {
    drain_timers(runtime, state, Instant::now())
}

/// Polls the WebSocket transport and dispatches buffered events to the
/// JavaScript `onopen` / `onmessage` / `onclose` / `onerror` handlers as
/// microtasks. Returns the number of transport events applied.
pub fn pump_websockets(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> usize {
    websocket::pump(runtime, state)
}

/// Delivers recorded service worker events to JavaScript listeners, evaluating
/// each worker's registered script source first. Returns the number of events
/// dispatched.
pub fn run_service_worker_events(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> usize {
    service_worker::run_events(runtime, state)
}

/// Resolves `customElements.whenDefined()` promises for names that were defined
/// since the last check (including definitions made from Rust). Returns the
/// number of promises settled.
pub fn flush_custom_elements(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> usize {
    custom_elements::flush(runtime, state)
}

/// Replaces the transport used by the managed WebSockets. Existing connections
/// are closed first because the event-loop manager owns a single transport.
pub fn set_websocket_transport(
    state: &Rc<RefCell<HostState>>,
    transport: Box<dyn WebSocketTransport>,
) {
    let mut borrowed = state.borrow_mut();
    let _ = borrowed
        .websocket_manager
        .close_all(1001, "transport replaced");
    borrowed.websocket_manager = WebSocketManager::new(transport);
}

pub use events::{
    DispatchOutcome, EventInit, dispatch_event, dispatch_event_object, dispatch_event_with,
    dispatch_keyboard_event, dispatch_mouse_event,
};

pub use observers::{
    LayoutBoxResolver, Mutation, MutationKind, Observers, Rect, deliver_observer_callbacks,
    deliver_observer_callbacks_now, notify_observers, record_mutation,
};

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse_document;

    #[test]
    fn new_platform_globals_are_exposed() {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document("<html><body></body></html>")));
        let _state = install(&mut runtime, document, None);
        for expression in [
            "typeof customElements === 'object'",
            "typeof ServiceWorkerContainer === 'function'",
            "typeof navigator.serviceWorker === 'object'",
            "typeof WebSocket === 'function'",
            "typeof indexedDB === 'object'",
            "typeof ShadowRoot === 'function'",
        ] {
            assert!(
                matches!(runtime.eval(expression).unwrap(), Value::Bool(true)),
                "expected {expression}"
            );
        }
    }
}
