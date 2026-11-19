//! Non-blocking navigation for hosts with a UI thread.
//!
//! [`Browser`] is deliberately single-threaded: its [`Page`] owns `Rc<RefCell<…>>`
//! handles shared with the JavaScript runtime, so the browser itself is not
//! `Send` and cannot simply be moved onto a worker thread. This module solves
//! that by giving each load its own `Browser` *inside* a worker thread and
//! shipping back a [`PagePayload`] made only of `Send` data.
//!
//! The visible thread never runs the load. It calls [`BackgroundLoader::navigate_async`]
//! and later polls [`BackgroundLoader::try_take`]. Results are tagged with a
//! monotonically increasing request id; when several navigations are queued,
//! only the newest one is ever delivered — stale results are discarded on sight.
//!
//! # Send-ness of the payload
//!
//! Every field of [`PagePayload`] was checked against the engine's public types:
//!
//! * [`DisplayList`] (`browser_paint`) contains only `Vec<DisplayCommand>` and
//!   `Vec<ImageData>`; `DisplayCommand` is built from plain geometry/colour/text
//!   values and `ImageData` is `Vec<u8>`. It is `Send` + `Clone`.
//! * `Page::images` is `HashMap<NodeId, ImageData>`; `NodeId(pub usize)` and
//!   `ImageData` are both `Send`, so the whole map is `Send` + `Clone`.
//! * `Size`, `Color`, `Url` and `String` are all `Send`.
//!
//! Because the actual values are `Send`, no serialization or skipping is
//! required and the payload keeps full fidelity. The only data intentionally
//! *not* carried over are the engine objects that are genuinely not `Send`:
//! the DOM [`Document`], the `StyledTree`, the `LayoutTree` and the JavaScript
//! runtime (all anchored by `Rc`/`RefCell`). Consequently
//! [`PagePayload::apply_to`] performs a *paint-only* hand-off: it installs the
//! display list, decoded images, URL, title (best-effort) and state, but leaves
//! the target page's DOM/layout/style trees untouched. See that method's docs.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use browser_css::Color;
use browser_dom::NodeId;
use browser_layout::Size;
use browser_paint::{DisplayList, ImageData};
use browser_url::Url;

use crate::{Browser, Page, PageState, Settings};

/// A `Send`-able snapshot of a loaded page, extracted from a worker-owned
/// [`Browser`].
///
/// All fields are public so a host can inspect the result directly, or use
/// [`PagePayload::apply_to`] to install the painted content into an existing
/// [`Page`] on the UI thread.
#[derive(Debug, Clone)]
pub struct PagePayload {
    /// Document title (mirrors `Page::title()`).
    pub title: String,
    /// Final URL as a string, or `None` when the page had no URL.
    ///
    /// Kept as a string (rather than [`Url`]) so the payload stays trivially
    /// `Send`, though `Url` itself is `Send` too.
    pub url: Option<String>,
    /// Visible-text snapshot (mirrors `Page::text()`).
    pub text: String,
    /// The complete display list produced by paint.
    pub display_list: DisplayList,
    /// Decoded images keyed by `<img>` node id (a clone of `Page::images`).
    pub images: HashMap<NodeId, ImageData>,
    /// Viewport the page was laid out for.
    pub viewport: Size,
    /// Background colour the page was configured with.
    ///
    /// Derived from [`Settings::background_color`]. `Page::background` is a
    /// private field with no public getter, so the worker cannot read the
    /// page's own value; this is the same value the host supplied.
    pub background: Color,
}

impl PagePayload {
    /// Number of recorded paint commands.
    pub fn command_count(&self) -> usize {
        self.display_list.len()
    }

    /// Best-effort installation of this payload into an existing [`Page`].
    ///
    /// This never panics and touches only fields that can be replaced without
    /// forcing a layout/paint pass (which would discard the very display list
    /// being installed). Specifically it installs:
    ///
    /// * `display_list` and `images` (the painted result),
    /// * `url` (re-parsed; invalid URLs leave the page's URL cleared),
    /// * `title` into `page.document` via `try_borrow_mut` — if the document is
    ///   already borrowed, the title is simply left as-is,
    /// * `state` = [`PageState::Loaded`],
    /// * `background` via the safe [`Page::set_background`] setter.
    ///
    /// # Limitations
    ///
    /// The DOM, `StyledTree`, `LayoutTree` and JS runtime are *not* transferred:
    /// they are not `Send` (they contain `Rc<RefCell<…>>`). The target page keeps
    /// its own document/layout, so DOM queries, hit-testing and script dispatch
    /// still reflect the old page. Only painting (`Page::render`) and the
    /// metadata above reflect the loaded payload. The `viewport` field is
    /// likewise carried for the caller but not applied here: the only public
    /// way to change it is `Page::resize`, which re-runs layout/paint and would
    /// overwrite the installed display list. Callers who need a matching layout
    /// should resize *before* applying the payload.
    pub fn apply_to(&self, page: &mut Page) {
        page.display_list = self.display_list.clone();
        page.images = self.images.clone();
        page.url = self.url.as_ref().and_then(|raw| Url::parse(raw).ok());
        if let Ok(mut document) = page.document.try_borrow_mut() {
            document.title = self.title.clone();
            if let Some(url) = &self.url {
                document.base_url = Some(url.clone());
            }
        }
        page.state = PageState::Loaded;
        page.set_background(self.background);
    }
}

/// One command sent to the worker.
enum Request {
    Navigate {
        id: u64,
        url: String,
        settings: Settings,
    },
    LoadHtml {
        id: u64,
        html: String,
        url: Option<String>,
        settings: Settings,
    },
}

impl Request {
    fn id(&self) -> u64 {
        match self {
            Self::Navigate { id, .. } | Self::LoadHtml { id, .. } => *id,
        }
    }
}

/// One result sent back from the worker.
struct Response {
    id: u64,
    result: Result<PagePayload, String>,
}

/// Pulls a `Send`-able payload out of a freshly loaded browser.
fn extract_payload(browser: &Browser, settings: &Settings) -> PagePayload {
    let snapshot = browser.snapshot();
    let page = browser.page();
    PagePayload {
        title: snapshot.title,
        url: snapshot.url,
        text: snapshot.text,
        display_list: page.display_list.clone(),
        images: page.images.clone(),
        viewport: page.viewport(),
        background: settings.background_color(),
    }
}

/// Turns a panicking worker body into an error string.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "background worker panicked".to_string()
    }
}

/// The worker thread body: creates one `Browser` per request and loads in place.
fn worker_loop(requests: mpsc::Receiver<Request>, responses: mpsc::Sender<Response>) {
    while let Ok(request) = requests.recv() {
        let id = request.id();
        let result = catch_unwind(AssertUnwindSafe(|| match request {
            Request::Navigate { url, settings, .. } => {
                let mut browser = Browser::new(settings.clone());
                browser.navigate(&url).map_err(|err| err.to_string())?;
                Ok(extract_payload(&browser, &settings))
            }
            Request::LoadHtml {
                html,
                url,
                settings,
                ..
            } => {
                let mut browser = Browser::new(settings.clone());
                browser
                    .load_html(&html, url.as_deref())
                    .map_err(|err| err.to_string())?;
                Ok(extract_payload(&browser, &settings))
            }
        }))
        .unwrap_or_else(|payload| Err(panic_message(payload)));
        if responses.send(Response { id, result }).is_err() {
            // The host dropped the loader; nothing left to do.
            break;
        }
    }
}

/// A reusable background worker for non-blocking navigation.
///
/// Constructing one spawns a single named worker thread. Dropping the loader
/// closes the request channel, which lets the worker exit after any in-flight
/// load finishes; the loader never blocks on the thread.
pub struct BackgroundLoader {
    requests: mpsc::Sender<Request>,
    responses: mpsc::Receiver<Response>,
    /// Id of the most recently requested navigation.
    latest_id: u64,
    /// A completed result for `latest_id`, waiting to be taken.
    pending: Option<Result<PagePayload, String>>,
    /// True from a request until its (or a newer) result is taken.
    busy: bool,
}

impl BackgroundLoader {
    /// Spawns a new background loader with its own worker thread.
    pub fn new() -> Self {
        let (requests_tx, requests_rx) = mpsc::channel();
        let (responses_tx, responses_rx) = mpsc::channel();
        thread::Builder::new()
            .name("mag-background-loader".to_string())
            .spawn(move || worker_loop(requests_rx, responses_tx))
            .expect("failed to spawn background loader thread");
        Self {
            requests: requests_tx,
            responses: responses_rx,
            latest_id: 0,
            pending: None,
            busy: false,
        }
    }

    /// Queues a navigation on the worker thread without blocking the caller.
    ///
    /// Any previously queued or completed result is superseded: only this
    /// navigation's result can be returned by a later [`Self::try_take`].
    pub fn navigate_async(&mut self, url: String, settings: Settings) {
        let id = self.next_id();
        self.enqueue(Request::Navigate { id, url, settings });
    }

    /// Queues a direct HTML load on the worker thread without blocking.
    ///
    /// Like [`Self::navigate_async`], this supersedes any earlier result.
    pub fn load_html_async(&mut self, html: String, url: Option<String>, settings: Settings) {
        let id = self.next_id();
        self.enqueue(Request::LoadHtml {
            id,
            html,
            url,
            settings,
        });
    }

    /// Advances the request id and marks the loader busy.
    fn next_id(&mut self) -> u64 {
        self.latest_id += 1;
        self.pending = None;
        self.busy = true;
        self.latest_id
    }

    /// Sends a request, recovering gracefully if the worker is unavailable.
    fn enqueue(&mut self, request: Request) {
        if self.requests.send(request).is_err() {
            self.pending = Some(Err("background worker is unavailable".to_string()));
            self.busy = false;
        }
    }

    /// Non-blocking poll for the newest completed navigation.
    ///
    /// Returns `Some` at most once per navigation. Stale results (from
    /// superseded navigations) are drained and discarded. Returns `None` while
    /// the navigation is still running or after its result has already been
    /// taken.
    pub fn try_take(&mut self) -> Option<Result<PagePayload, String>> {
        loop {
            match self.responses.try_recv() {
                Ok(Response { id, result }) => {
                    if id == self.latest_id {
                        self.pending = Some(result);
                        self.busy = false;
                    }
                    // A different id belongs to a superseded navigation: drop it.
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.busy && self.pending.is_none() {
                        self.pending = Some(Err("background worker disconnected".to_string()));
                        self.busy = false;
                    }
                    break;
                }
            }
        }
        self.pending.take()
    }

    /// True while a requested navigation has not yet been delivered.
    pub fn is_busy(&self) -> bool {
        self.busy
    }
}

impl Default for BackgroundLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// Writes a unique temporary HTML file and returns its `file://` URL.
    fn temp_page(title: &str, body: &str) -> (PathBuf, String) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock works")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mag-bg-test-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("page.html");
        let html = format!(
            "<!DOCTYPE html><html><head><title>{title}</title></head>\
             <body><p>{body}</p></body></html>"
        );
        std::fs::write(&path, html).expect("write temp page");
        let url = format!("file://{}", path.display());
        (dir, url)
    }

    fn settings() -> Settings {
        let mut settings = Settings::headless();
        settings.enable_network = false;
        settings
    }

    /// Polls `try_take` until a result arrives or the deadline passes.
    fn wait_for_payload(loader: &mut BackgroundLoader) -> Result<PagePayload, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = loader.try_take() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for background navigation"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn loads_local_file_in_background_and_applies_payload() {
        let (dir, url) = temp_page("Bg", "hello from worker");
        let mut loader = BackgroundLoader::new();
        assert!(!loader.is_busy());

        loader.navigate_async(url, settings());
        assert!(loader.is_busy());

        let payload = wait_for_payload(&mut loader).expect("navigation succeeds");
        assert!(!loader.is_busy());
        assert_eq!(payload.title, "Bg");
        assert!(payload.text.contains("hello from worker"));
        assert!(payload.command_count() > 0);
        assert!(!payload.images.is_empty() || !payload.display_list.is_empty());

        let mut page = Page::new(Size {
            width: 800.0,
            height: 600.0,
        });
        payload.apply_to(&mut page);
        assert!(!page.display_list.is_empty());
        assert_eq!(page.display_list.len(), payload.display_list.len());
        assert_eq!(page.title(), "Bg");
        assert_eq!(page.state, PageState::Loaded);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_newest_navigation_is_delivered() {
        let (dir_a, url_a) = temp_page("First", "first page");
        let (dir_b, url_b) = temp_page("Second", "second page");

        let mut loader = BackgroundLoader::new();
        loader.navigate_async(url_a, settings());
        loader.navigate_async(url_b, settings());

        // Both responses may already be queued; `try_take` must keep only the
        // result whose id matches the newest request.
        let payload = wait_for_payload(&mut loader).expect("navigation succeeds");
        assert_eq!(payload.title, "Second");
        assert!(payload.text.contains("second page"));
        assert!(payload.command_count() > 0);
        assert!(!loader.is_busy());

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn load_html_async_delivers_a_payload() {
        let mut loader = BackgroundLoader::new();
        loader.load_html_async(
            "<html><head><title>Inline</title></head><body><p>inline</p></body></html>".to_string(),
            None,
            settings(),
        );
        let payload = wait_for_payload(&mut loader).expect("load succeeds");
        assert_eq!(payload.title, "Inline");
        assert!(payload.command_count() > 0);
    }
}
