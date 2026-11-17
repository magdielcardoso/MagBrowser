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

pub mod console;
pub mod document;
pub mod element;
pub mod events;
pub mod location;
pub mod storage;
pub mod timers;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use browser_dom::Document;
use browser_js::{Runtime, Value};

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
    /// Cache of element wrapper objects keyed by node id.
    pub elements: HashMap<usize, Value>,
    /// The document URL, if any.
    pub url: Option<String>,
}

impl HostState {
    /// Creates state for a document.
    pub fn new(document: Rc<RefCell<Document>>, url: Option<String>) -> Self {
        Self {
            document,
            events: HashMap::new(),
            timers: Vec::new(),
            next_timer_id: 1,
            console: Arc::new(Mutex::new(Vec::new())),
            storage: HashMap::new(),
            navigation: None,
            elements: HashMap::new(),
            url,
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
    timers::install(runtime, &state);
    storage::install(runtime, &state);
    location::install(runtime, &state);
    events::install_global(runtime, &state);

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

pub use events::dispatch_event;
