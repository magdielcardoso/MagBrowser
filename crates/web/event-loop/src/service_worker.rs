//! Service worker registration, lifecycle and fetch interception.
//!
//! This module models the *control* side of service workers. It does not run
//! JavaScript: script execution is delegated to the embedder through a
//! [`WorkerScriptHandler`] callback. The registry owns the registration table,
//! the install/activate/fetch event model and the fetch-interception hook; the
//! host supplies the callback that evaluates the worker script in its JS runtime.
//!
//! Without a handler installed, lifecycle events still advance the state machine
//! (there is simply no script to run) and [`ServiceWorkerRegistry::handle_fetch`]
//! does not intercept anything. This is deliberate: the registry never fakes a
//! script result or a synthesized response.
//!
//! ```
//! use browser_event_loop::ServiceWorkerRegistry;
//!
//! let mut registry = ServiceWorkerRegistry::new();
//! registry.register("/app/", "/app/sw.js").unwrap();
//! registry.activate("/app/").unwrap();
//! // No script handler installed, so fetch passes through untouched.
//! assert_eq!(registry.handle_fetch("/app/data.json"), None);
//! ```

use std::fmt;
use std::sync::Arc;

/// Identifier of one service worker registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegistrationId(pub u64);

impl fmt::Display for RegistrationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sw-{}", self.0)
    }
}

/// Lifecycle state of a registration.
///
/// The normal progression is `Installing` → `Installed` → `Active`. A failed
/// install/activate, an `unregister` or a re-registration for the same scope
/// moves a worker to `Redundant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkerState {
    /// The `install` event is being handled.
    Installing,
    /// Install succeeded; the worker is waiting to control clients.
    Installed,
    /// The `activate` event is being handled.
    Activating,
    /// The worker controls matching clients and may intercept fetches.
    Active,
    /// The worker was replaced, failed or unregistered.
    Redundant,
}

impl WorkerState {
    /// Stable lowercase name of the state.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Installing => "installing",
            Self::Installed => "installed",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Redundant => "redundant",
        }
    }

    /// True only for an active worker.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// A synthesized response returned by a worker's `fetch` handler.
///
/// The field layout mirrors `browser_web_apis::fetch::FetchResponse` so the
/// embedder can convert in both directions without copying logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// True for a 2xx status.
    pub ok: bool,
    /// Numeric HTTP status.
    pub status: u16,
    /// HTTP reason phrase.
    pub status_text: String,
    /// Response headers in insertion order.
    pub headers: Vec<(String, String)>,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

impl Response {
    /// Builds a response, deriving [`Self::ok`] from the status code.
    pub fn new(
        status: u16,
        status_text: impl Into<String>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            ok: (200..300).contains(&status),
            status,
            status_text: status_text.into(),
            headers,
            body,
        }
    }

    /// Builds a `200 OK` text response.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self::new(
            200,
            "OK",
            vec![(
                "content-type".to_string(),
                "text/plain; charset=utf-8".to_string(),
            )],
            body.into(),
        )
    }

    /// Builds a `404 Not Found` response with an empty body.
    pub fn not_found() -> Self {
        Self::new(404, "Not Found", Vec::new(), Vec::new())
    }

    /// First value of a header, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// True when the status is in the 2xx range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Descriptive alias for [`Response`] when both HTTP crates are in scope.
pub type ServiceWorkerResponse = Response;

/// Which lifecycle event a worker script is being asked to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkerEventKind {
    /// Worker installation.
    Install,
    /// Worker activation.
    Activate,
    /// A network request the worker may intercept.
    Fetch,
}

impl WorkerEventKind {
    /// Stable lowercase name of the event kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Activate => "activate",
            Self::Fetch => "fetch",
        }
    }
}

/// An event dispatched to a worker script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceWorkerEvent {
    /// Sent when a registration is created.
    Install {
        /// Path scope the registration controls.
        scope: String,
        /// URL of the worker script.
        script_url: String,
    },
    /// Sent when an installed worker is promoted to active.
    Activate {
        /// Path scope the registration controls.
        scope: String,
        /// URL of the worker script.
        script_url: String,
    },
    /// Sent for each request that matches an active registration's scope.
    Fetch {
        /// Scope of the worker handling the request.
        scope: String,
        /// Request URL being intercepted.
        url: String,
    },
}

impl ServiceWorkerEvent {
    /// The kind of this event.
    pub const fn kind(&self) -> WorkerEventKind {
        match self {
            Self::Install { .. } => WorkerEventKind::Install,
            Self::Activate { .. } => WorkerEventKind::Activate,
            Self::Fetch { .. } => WorkerEventKind::Fetch,
        }
    }

    /// The scope of the registration the event belongs to.
    pub fn scope(&self) -> &str {
        match self {
            Self::Install { scope, .. }
            | Self::Activate { scope, .. }
            | Self::Fetch { scope, .. } => scope,
        }
    }
}

/// The result a worker script reports for one dispatched event.
///
/// The variant must match the event kind; a mismatch is treated as a failed
/// lifecycle step or, for `fetch`, as "no interception".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEventResult {
    /// Result of an `install` event.
    Installed {
        /// Whether installation succeeded.
        success: bool,
    },
    /// Result of an `activate` event.
    Activated {
        /// Whether activation succeeded.
        success: bool,
    },
    /// Result of a `fetch` event.
    Fetched {
        /// A response to use instead of the network, or `None` to pass through.
        response: Option<Response>,
    },
}

impl WorkerEventResult {
    /// Convenience constructor for an install result.
    pub fn install(success: bool) -> Self {
        Self::Installed { success }
    }

    /// Convenience constructor for an activate result.
    pub fn activate(success: bool) -> Self {
        Self::Activated { success }
    }

    /// Convenience constructor for a fetch result.
    pub fn fetch(response: Option<Response>) -> Self {
        Self::Fetched { response }
    }
}

/// Host-supplied callback that executes a worker script for one event.
///
/// The callback is called synchronously and must be safe to call from the thread
/// driving the registry. An embedder typically captures a `Mutex<Runtime>` and
/// evaluates the corresponding event listener inside the lock.
pub type WorkerScriptHandler = Arc<dyn Fn(&ServiceWorkerEvent) -> WorkerEventResult + Send + Sync>;

/// One service worker registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// Unique identifier.
    pub id: RegistrationId,
    /// Path scope this worker controls.
    pub scope: String,
    /// URL of the worker script.
    pub script_url: String,
    /// Current lifecycle state.
    pub state: WorkerState,
}

impl Registration {
    /// True when this worker can intercept fetches.
    pub const fn is_active(&self) -> bool {
        self.state.is_active()
    }

    /// True when `url` lies under this registration's scope.
    pub fn covers(&self, url: &str) -> bool {
        url.starts_with(self.scope.as_str())
    }
}

/// Errors from registration and lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerError {
    /// `register` was called with an empty scope.
    EmptyScope,
    /// `register` was called with an empty script URL.
    EmptyScriptUrl,
    /// No registration exists for the given scope.
    NotFound {
        /// Scope that was looked up.
        scope: String,
    },
    /// The worker script rejected the install event.
    InstallFailed {
        /// Scope of the failed registration.
        scope: String,
    },
    /// The worker script rejected the activate event.
    ActivateFailed {
        /// Scope of the failed registration.
        scope: String,
    },
    /// The operation is not valid in the registration's current state.
    InvalidState {
        /// Scope of the registration.
        scope: String,
        /// State that caused the rejection.
        state: WorkerState,
    },
}

impl fmt::Display for WorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyScope => f.write_str("service worker scope must not be empty"),
            Self::EmptyScriptUrl => f.write_str("service worker script URL must not be empty"),
            Self::NotFound { scope } => write!(f, "no service worker registered for '{scope}'"),
            Self::InstallFailed { scope } => {
                write!(f, "service worker install failed for '{scope}'")
            }
            Self::ActivateFailed { scope } => {
                write!(f, "service worker activation failed for '{scope}'")
            }
            Self::InvalidState { scope, state } => {
                write!(f, "service worker for '{scope}' is {state:?}")
            }
        }
    }
}

impl std::error::Error for WorkerError {}

/// Registry of service worker registrations and their event outbox.
///
/// The registry is fully deterministic: no clock, no I/O and no JavaScript run
/// inside it. The browser shell calls the mutating methods while driving its
/// event loop and forwards [`ServiceWorkerEvent`]s to the embedder callback.
pub struct ServiceWorkerRegistry {
    registrations: Vec<Registration>,
    handler: Option<WorkerScriptHandler>,
    events: Vec<ServiceWorkerEvent>,
    next_id: u64,
}

impl ServiceWorkerRegistry {
    /// Creates an empty registry with no script handler.
    pub fn new() -> Self {
        Self {
            registrations: Vec::new(),
            handler: None,
            events: Vec::new(),
            next_id: 0,
        }
    }

    /// Installs the callback used to execute worker scripts.
    pub fn set_script_handler(&mut self, handler: WorkerScriptHandler) {
        self.handler = Some(handler);
    }

    /// True when a script handler has been installed.
    pub const fn has_script_handler(&self) -> bool {
        self.handler.is_some()
    }

    /// Registers a worker for `scope`, creating it in `Installing` and dispatching
    /// its `install` event.
    ///
    /// Re-registering an existing scope retires the old registration (it becomes
    /// `Redundant`) and returns a fresh identifier. On a failed install the new
    /// registration is also `Redundant` and [`WorkerError::InstallFailed`] is
    /// returned; the registration is still recorded for inspection.
    pub fn register(
        &mut self,
        scope: impl Into<String>,
        script_url: impl Into<String>,
    ) -> Result<RegistrationId, WorkerError> {
        let scope = scope.into();
        let script_url = script_url.into();
        if scope.is_empty() {
            return Err(WorkerError::EmptyScope);
        }
        if script_url.is_empty() {
            return Err(WorkerError::EmptyScriptUrl);
        }

        // The spec allows at most one active worker per scope: retire any old one.
        if let Some(previous) = self
            .registrations
            .iter_mut()
            .find(|registration| registration.scope == scope)
        {
            previous.state = WorkerState::Redundant;
        }

        self.next_id += 1;
        let id = RegistrationId(self.next_id);
        let event = ServiceWorkerEvent::Install {
            scope: scope.clone(),
            script_url: script_url.clone(),
        };
        let result = self.dispatch(&event);
        let success = matches!(result, WorkerEventResult::Installed { success: true });
        self.registrations.push(Registration {
            id,
            scope: scope.clone(),
            script_url,
            state: if success {
                WorkerState::Installed
            } else {
                WorkerState::Redundant
            },
        });

        if success {
            Ok(id)
        } else {
            Err(WorkerError::InstallFailed { scope })
        }
    }

    /// Removes a registration by marking it `Redundant`.
    ///
    /// Returns `true` when a live registration was found. The record is kept so
    /// hosts can still inspect past registrations.
    pub fn unregister(&mut self, scope: &str) -> bool {
        match self.registrations.iter_mut().find(|registration| {
            registration.scope == scope && registration.state != WorkerState::Redundant
        }) {
            Some(registration) => {
                registration.state = WorkerState::Redundant;
                true
            }
            None => false,
        }
    }

    /// Activates an installed worker for `scope`, dispatching its `activate` event.
    ///
    /// Activation is idempotent for an already-active worker and rejects a
    /// missing or redundant registration.
    pub fn activate(&mut self, scope: &str) -> Result<(), WorkerError> {
        let index = self
            .registrations
            .iter()
            .position(|registration| registration.scope == scope)
            .ok_or_else(|| WorkerError::NotFound {
                scope: scope.to_string(),
            })?;
        let state = self.registrations[index].state;
        match state {
            WorkerState::Active => return Ok(()),
            WorkerState::Activating | WorkerState::Redundant => {
                return Err(WorkerError::InvalidState {
                    scope: scope.to_string(),
                    state,
                });
            }
            WorkerState::Installing | WorkerState::Installed => {}
        }

        let script_url = self.registrations[index].script_url.clone();
        let event = ServiceWorkerEvent::Activate {
            scope: scope.to_string(),
            script_url,
        };
        let result = self.dispatch(&event);
        let success = matches!(result, WorkerEventResult::Activated { success: true });
        self.registrations[index].state = if success {
            WorkerState::Active
        } else {
            WorkerState::Redundant
        };

        if success {
            Ok(())
        } else {
            Err(WorkerError::ActivateFailed {
                scope: scope.to_string(),
            })
        }
    }

    /// Intercepts `url` on behalf of the most specific active worker whose scope
    /// is a prefix of `url`.
    ///
    /// Returns `None` when no active worker matches, no script handler is
    /// installed, or the worker script declines to provide a response. Every
    /// invocation records a [`ServiceWorkerEvent::Fetch`] in the outbox.
    pub fn handle_fetch(&mut self, url: &str) -> Option<Response> {
        let index = self
            .registrations
            .iter()
            .enumerate()
            .filter(|(_, registration)| {
                registration.state == WorkerState::Active && registration.covers(url)
            })
            .max_by_key(|(_, registration)| registration.scope.len())
            .map(|(index, _)| index)?;
        let scope = self.registrations[index].scope.clone();
        let event = ServiceWorkerEvent::Fetch {
            scope,
            url: url.to_string(),
        };
        match self.dispatch(&event) {
            WorkerEventResult::Fetched { response } => response,
            _ => None,
        }
    }

    /// Finds the most recent registration for an exact scope, including
    /// redundant ones.
    ///
    /// Re-registering a scope appends a new record and retires the old one, so
    /// the newest record is the one callers normally want.
    pub fn get(&self, scope: &str) -> Option<&Registration> {
        self.registrations
            .iter()
            .rev()
            .find(|registration| registration.scope == scope)
    }

    /// The active registration that would handle `url`, if any.
    pub fn registration_for(&self, url: &str) -> Option<&Registration> {
        self.registrations
            .iter()
            .filter(|registration| {
                registration.state == WorkerState::Active && registration.covers(url)
            })
            .max_by_key(|registration| registration.scope.len())
    }

    /// All registrations, in registration order.
    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    /// Number of registrations, including redundant ones.
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// True when the registry has no registrations.
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    /// Removes and returns the events dispatched since the last call.
    pub fn take_events(&mut self) -> Vec<ServiceWorkerEvent> {
        std::mem::take(&mut self.events)
    }

    /// Number of events currently in the outbox.
    pub fn pending_events(&self) -> usize {
        self.events.len()
    }

    fn dispatch(&mut self, event: &ServiceWorkerEvent) -> WorkerEventResult {
        self.events.push(event.clone());
        match &self.handler {
            Some(handler) => handler(event),
            None => match event.kind() {
                WorkerEventKind::Install => WorkerEventResult::install(true),
                WorkerEventKind::Activate => WorkerEventResult::activate(true),
                WorkerEventKind::Fetch => WorkerEventResult::fetch(None),
            },
        }
    }
}

impl Default for ServiceWorkerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ServiceWorkerRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceWorkerRegistry")
            .field("registrations", &self.registrations.len())
            .field("has_script_handler", &self.has_script_handler())
            .field("pending_events", &self.pending_events())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler_result(
        events: Arc<std::sync::Mutex<Vec<ServiceWorkerEvent>>>,
    ) -> WorkerScriptHandler {
        Arc::new(move |event: &ServiceWorkerEvent| {
            events.lock().unwrap().push(event.clone());
            match event.kind() {
                WorkerEventKind::Install => WorkerEventResult::install(true),
                WorkerEventKind::Activate => WorkerEventResult::activate(true),
                WorkerEventKind::Fetch => {
                    WorkerEventResult::fetch(Some(Response::ok("from worker")))
                }
            }
        })
    }

    #[test]
    fn register_dispatches_install_and_moves_to_installed() {
        let mut registry = ServiceWorkerRegistry::new();
        let id = registry.register("/app/", "/app/sw.js").unwrap();
        assert_eq!(id, RegistrationId(1));
        let registration = registry.get("/app/").unwrap();
        assert_eq!(registration.state, WorkerState::Installed);
        assert_eq!(registration.script_url, "/app/sw.js");
        assert_eq!(
            registry.take_events(),
            [ServiceWorkerEvent::Install {
                scope: "/app/".to_string(),
                script_url: "/app/sw.js".to_string(),
            }]
        );
    }

    #[test]
    fn activate_promotes_installed_worker_to_active() {
        let mut registry = ServiceWorkerRegistry::new();
        registry.register("/app/", "/app/sw.js").unwrap();
        assert_eq!(registry.registration_for("/app/x"), None);
        registry.activate("/app/").unwrap();
        assert_eq!(registry.get("/app/").unwrap().state, WorkerState::Active);
        assert_eq!(
            registry.registration_for("/app/x").unwrap().id,
            RegistrationId(1)
        );
        assert_eq!(registry.take_events().len(), 2);
    }

    #[test]
    fn activate_is_idempotent_but_missing_scope_errors() {
        let mut registry = ServiceWorkerRegistry::new();
        registry.register("/", "/sw.js").unwrap();
        registry.activate("/").unwrap();
        registry.activate("/").unwrap();
        assert_eq!(registry.pending_events(), 2);
        assert_eq!(
            registry.activate("/missing"),
            Err(WorkerError::NotFound {
                scope: "/missing".to_string()
            })
        );
    }

    #[test]
    fn failed_install_and_failed_activate_become_redundant() {
        let handler: WorkerScriptHandler = Arc::new(|event| match event.kind() {
            WorkerEventKind::Install => WorkerEventResult::install(false),
            _ => WorkerEventResult::activate(false),
        });
        let mut registry = ServiceWorkerRegistry::new();
        registry.set_script_handler(handler);
        assert_eq!(
            registry.register("/a/", "/sw.js"),
            Err(WorkerError::InstallFailed {
                scope: "/a/".to_string()
            })
        );
        assert_eq!(registry.get("/a/").unwrap().state, WorkerState::Redundant);

        let ok_install: WorkerScriptHandler = Arc::new(|event| match event.kind() {
            WorkerEventKind::Install => WorkerEventResult::install(true),
            _ => WorkerEventResult::activate(false),
        });
        registry.set_script_handler(ok_install);
        registry.register("/b/", "/sw.js").unwrap();
        assert_eq!(
            registry.activate("/b/"),
            Err(WorkerError::ActivateFailed {
                scope: "/b/".to_string()
            })
        );
        assert_eq!(registry.get("/b/").unwrap().state, WorkerState::Redundant);
    }

    #[test]
    fn fetch_handler_intercepts_only_active_workers() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = ServiceWorkerRegistry::new();
        registry.set_script_handler(handler_result(Arc::clone(&seen)));
        registry.register("/app/", "/sw.js").unwrap();

        // Installed but not active: no interception, no fetch event.
        assert_eq!(registry.handle_fetch("/app/data"), None);
        assert_eq!(registry.pending_events(), 1);

        registry.activate("/app/").unwrap();
        let response = registry.handle_fetch("/app/data").unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"from worker");
        assert!(response.ok);
        assert_eq!(
            registry.take_events().last(),
            Some(&ServiceWorkerEvent::Fetch {
                scope: "/app/".to_string(),
                url: "/app/data".to_string(),
            })
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn longest_matching_scope_wins() {
        let handler: WorkerScriptHandler = Arc::new(|event| match event.kind() {
            WorkerEventKind::Fetch => WorkerEventResult::fetch(Some(Response::ok(event.scope()))),
            WorkerEventKind::Install => WorkerEventResult::install(true),
            WorkerEventKind::Activate => WorkerEventResult::activate(true),
        });
        let mut registry = ServiceWorkerRegistry::new();
        registry.set_script_handler(handler);
        registry.register("/", "/root.js").unwrap();
        registry.activate("/").unwrap();
        registry.register("/app/", "/app.js").unwrap();
        registry.activate("/app/").unwrap();

        assert_eq!(registry.handle_fetch("/app/data").unwrap().body, b"/app/");
        assert_eq!(registry.handle_fetch("/other").unwrap().body, b"/");
    }

    #[test]
    fn unregister_stops_interception() {
        let handler: WorkerScriptHandler = Arc::new(|event| match event.kind() {
            WorkerEventKind::Fetch => WorkerEventResult::fetch(Some(Response::ok("yes"))),
            WorkerEventKind::Install => WorkerEventResult::install(true),
            WorkerEventKind::Activate => WorkerEventResult::activate(true),
        });
        let mut registry = ServiceWorkerRegistry::new();
        registry.set_script_handler(handler);
        registry.register("/app/", "/sw.js").unwrap();
        registry.activate("/app/").unwrap();
        assert!(registry.handle_fetch("/app/x").is_some());
        assert!(registry.unregister("/app/"));
        assert!(!registry.unregister("/app/"));
        assert_eq!(registry.get("/app/").unwrap().state, WorkerState::Redundant);
        assert_eq!(registry.handle_fetch("/app/x"), None);
        assert_eq!(
            registry.activate("/app/"),
            Err(WorkerError::InvalidState {
                scope: "/app/".to_string(),
                state: WorkerState::Redundant,
            })
        );
    }

    #[test]
    fn re_registering_a_scope_retires_the_previous_worker() {
        let mut registry = ServiceWorkerRegistry::new();
        let first = registry.register("/app/", "/v1.js").unwrap();
        let second = registry.register("/app/", "/v2.js").unwrap();
        assert_ne!(first, second);
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.registrations()[0].state, WorkerState::Redundant);
        assert_eq!(registry.registrations()[1].state, WorkerState::Installed);
        assert_eq!(registry.get("/app/").unwrap().id, second);
    }

    #[test]
    fn validates_inputs() {
        let mut registry = ServiceWorkerRegistry::new();
        assert_eq!(
            registry.register("", "/sw.js"),
            Err(WorkerError::EmptyScope)
        );
        assert_eq!(registry.register("/", ""), Err(WorkerError::EmptyScriptUrl));
        assert!(registry.is_empty());
    }

    #[test]
    fn response_helpers_and_mismatched_results() {
        let response = Response::ok("hi");
        assert!(response.is_success());
        assert_eq!(
            response.header("Content-Type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            response.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(response.header("missing"), None);
        assert!(!Response::not_found().is_success());

        // A result whose variant does not match the event is treated as failure.
        let handler: WorkerScriptHandler = Arc::new(|_| WorkerEventResult::activate(true));
        let mut registry = ServiceWorkerRegistry::new();
        registry.set_script_handler(handler);
        assert_eq!(
            registry.register("/app/", "/sw.js"),
            Err(WorkerError::InstallFailed {
                scope: "/app/".to_string()
            })
        );
        assert_eq!(registry.get("/app/").unwrap().state, WorkerState::Redundant);
    }

    #[test]
    fn event_kinds_and_scope_accessors() {
        let install = ServiceWorkerEvent::Install {
            scope: "/a".to_string(),
            script_url: "sw.js".to_string(),
        };
        let activate = ServiceWorkerEvent::Activate {
            scope: "/b".to_string(),
            script_url: "sw.js".to_string(),
        };
        let fetch = ServiceWorkerEvent::Fetch {
            scope: "/c".to_string(),
            url: "/c/x".to_string(),
        };
        assert_eq!(install.kind().as_str(), "install");
        assert_eq!(activate.kind().as_str(), "activate");
        assert_eq!(fetch.kind().as_str(), "fetch");
        assert_eq!(install.scope(), "/a");
        assert_eq!(fetch.scope(), "/c");
        assert_eq!(WorkerState::Active.as_str(), "active");
        assert!(WorkerState::Active.is_active());
    }
}
