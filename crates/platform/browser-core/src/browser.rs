//! The browser facade: navigation, tabs, rendering and interaction.

#[path = "imports.rs"]
mod imports;

#[path = "module_scripts.rs"]
mod module_scripts;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use browser_dom::{Document, NodeId, escape_text};
use browser_event_loop::{Registration, ServiceWorkerEvent};
use browser_http::{Client, ClientConfig, HttpError, Method, Request};
use browser_navigation::{IdbDatabase, NavigationAction, NavigationBudget, Truncation};
use browser_paint::ImageBuffer;
use browser_resource_loader::ResourceLoader;
use browser_security::CspList;
use browser_url::{Url, UrlError};

use crate::page::{Page, PageState};
use crate::settings::Settings;
use crate::tab::{TabId, TabSet};

/// Errors raised by browser operations.
#[derive(Debug)]
pub enum BrowserError {
    /// URL could not be parsed.
    Url(UrlError),
    /// Network request failed.
    Http(HttpError),
    /// Local file could not be read.
    Io(String),
    /// Network access is disabled in settings.
    NetworkDisabled,
    /// No page is loaded.
    NoPage,
    /// The supplied node is not a form in the active document.
    InvalidForm,
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(err) => write!(f, "invalid URL: {err}"),
            Self::Http(err) => write!(f, "network error: {err}"),
            Self::Io(err) => write!(f, "file error: {err}"),
            Self::NetworkDisabled => f.write_str("network access is disabled"),
            Self::NoPage => f.write_str("no page is loaded"),
            Self::InvalidForm => f.write_str("node is not a form element"),
        }
    }
}

impl std::error::Error for BrowserError {}

impl From<UrlError> for BrowserError {
    fn from(value: UrlError) -> Self {
        Self::Url(value)
    }
}

impl From<browser_resource_loader::LoadError> for BrowserError {
    fn from(value: browser_resource_loader::LoadError) -> Self {
        match value {
            browser_resource_loader::LoadError::Url(err) => Self::Url(err),
            browser_resource_loader::LoadError::Http(err) => Self::Http(err),
            browser_resource_loader::LoadError::Io(err) => Self::Io(err),
            other => Self::Io(other.to_string()),
        }
    }
}

impl From<HttpError> for BrowserError {
    fn from(value: HttpError) -> Self {
        Self::Http(value)
    }
}

/// Summary of the active page, useful for CLIs and tests.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Final URL.
    pub url: Option<String>,
    /// Document title.
    pub title: String,
    /// Body text.
    pub text: String,
    /// Number of DOM nodes.
    pub node_count: usize,
    /// Rendered document height.
    pub content_height: f32,
    /// Number of display list commands.
    pub command_count: usize,
    /// Page lifecycle state.
    pub state: PageState,
    /// True when the navigation stopped early because the wall-clock deadline
    /// elapsed or a chained-navigation loop was detected. The page still holds
    /// whatever was loaded before that point (a usable partial page).
    pub truncated: bool,
    /// The reason the navigation was truncated, when `truncated` is true.
    pub truncation_reason: Option<Truncation>,
}

impl Snapshot {
    /// True when the navigation was cut short by the deadline or a loop guard.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// A browser instance with tabs.
pub struct Browser {
    settings: Settings,
    loader: ResourceLoader,
    tabs: TabSet,
    console: Vec<String>,
    csp: CspList,
    /// Budget for the navigation currently in progress. `None` outside a
    /// navigation; nested (`location.href`) navigations reuse the same budget.
    nav: Option<NavigationBudget>,
    /// Truncation reason of the most recently completed navigation.
    last_truncation: Option<Truncation>,
    /// Body for a POST document fetch, consumed once by `fetch_document`.
    pending_form_post: Option<Vec<u8>>,
    /// Name/value of the submit button used by the last form submission.
    last_submitter: Option<(String, String)>,
}

impl Browser {
    /// Creates a browser with one empty tab.
    pub fn new(settings: Settings) -> Self {
        // A single request may never outlast the whole navigation budget;
        // otherwise one slow subresource could hang the pipeline on its own.
        let request_timeout = Duration::from_secs(settings.timeout_seconds.max(1))
            .min(settings.navigation_deadline)
            .max(Duration::from_millis(1));
        let config = ClientConfig {
            user_agent: settings.user_agent.clone(),
            timeout: request_timeout,
            max_body_bytes: settings.max_body_bytes,
            request: browser_http::RequestConfig {
                viewport_width: settings.viewport_width.max(1.0).round() as u32,
                ..browser_http::RequestConfig::default()
            },
            ..ClientConfig::default()
        };
        let size = settings.viewport();
        Self {
            settings,
            loader: ResourceLoader::new(config),
            tabs: TabSet::new(size),
            console: Vec::new(),
            csp: CspList::new(),
            nav: None,
            last_truncation: None,
            pending_form_post: None,
            last_submitter: None,
        }
    }

    /// Active settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Mutable settings (dimensions apply on the next navigation or resize).
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Access to the HTTP client (cookies, cache).
    pub fn client(&self) -> &Client {
        self.loader.client()
    }

    /// Mutable access to the HTTP client.
    pub fn client_mut(&mut self) -> &mut Client {
        self.loader.client_mut()
    }

    /// The active tab.
    pub fn tab(&self) -> &crate::tab::Tab {
        self.tabs.active()
    }

    /// The active tab, mutably.
    pub fn tab_mut(&mut self) -> &mut crate::tab::Tab {
        self.tabs.active_mut()
    }

    /// The active page.
    pub fn page(&self) -> &Page {
        &self.tabs.active().page
    }

    /// The tab set.
    pub fn tabs(&self) -> &TabSet {
        &self.tabs
    }

    /// The tab set, mutably.
    pub fn tabs_mut(&mut self) -> &mut TabSet {
        &mut self.tabs
    }

    /// Opens a new tab.
    pub fn new_tab(&mut self) -> TabId {
        self.tabs.open_empty()
    }

    /// Console output collected from executed scripts.
    pub fn console(&self) -> &[String] {
        &self.console
    }

    /// Clears console output.
    pub fn clear_console(&mut self) {
        self.console.clear();
    }

    /// Resizes the viewport and relayouts every tab.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.settings.viewport_width = width.max(1.0);
        self.settings.viewport_height = height.max(1.0);
        let size = self.settings.viewport();
        self.tabs.resize(size);
    }

    /// Renders the active page.
    pub fn render(&self) -> ImageBuffer {
        self.page().render()
    }

    /// Renders the active page at a device scale factor.
    pub fn render_scaled(&self, scale: f32) -> ImageBuffer {
        self.page().render_scaled(scale)
    }

    /// Loads HTML directly into the active tab (tests, `view-source`, fixtures).
    ///
    /// Like [`Self::navigate_url`] this runs the whole subresource pipeline under
    /// a single wall-clock deadline.
    pub fn load_html(&mut self, html: &str, url: Option<&str>) -> Result<Snapshot, BrowserError> {
        let parsed = match url {
            Some(raw) => Some(Url::parse(raw)?),
            None => None,
        };
        let top = self.begin_navigation();
        let result = self.load_html_inner(html, parsed);
        if top {
            self.end_navigation();
        }
        result?;
        Ok(self.snapshot())
    }

    fn load_html_inner(&mut self, html: &str, url: Option<Url>) -> Result<(), BrowserError> {
        // Parsing the main document and its first layout/paint are essential: even
        // a truncated load must leave a usable page behind.
        let tab = self.tabs.active_mut();
        tab.page.load_html(html, url.clone());
        if let Some(url) = url {
            tab.record_navigation(NavigationAction::Push, Some(url));
        }
        self.refresh_policy();
        self.load_linked_stylesheets();
        if self.budget_ok() {
            self.load_images_inner();
        }
        if self.budget_ok() {
            self.execute_scripts_inner();
        }
        Ok(())
    }

    /// Navigates the active tab to `input` (a URL or a bare domain).
    pub fn navigate(&mut self, input: &str) -> Result<Snapshot, BrowserError> {
        let url = normalize_url(input)?;
        self.navigate_url(url, NavigationAction::Push)
    }

    /// Navigates to an already parsed URL.
    ///
    /// One wall-clock deadline covers the document and every subresource. If it
    /// elapses the load stops and `Snapshot::truncated` reports the partial page.
    pub fn navigate_url(
        &mut self,
        url: Url,
        action: NavigationAction,
    ) -> Result<Snapshot, BrowserError> {
        let top = self.begin_navigation();
        let result = self.navigate_url_inner(url, action);
        if top {
            self.end_navigation();
        }
        result?;
        Ok(self.snapshot())
    }

    fn navigate_url_inner(
        &mut self,
        url: Url,
        action: NavigationAction,
    ) -> Result<(), BrowserError> {
        // A chained navigation that arrives after the budget is spent stops here,
        // leaving the partial page from the previous hop in place.
        if !self.budget_ok() {
            return Ok(());
        }
        let html = self.fetch_document(&url)?;
        let viewport = self.settings.viewport();
        {
            let tab = self.tabs.active_mut();
            tab.page.resize(viewport);
            tab.page.load_html(&html, Some(url.clone()));
            tab.record_navigation(action, Some(url.clone()));
        }
        self.refresh_policy();
        self.fetch_stylesheets_for(&url);
        if self.budget_ok() {
            self.load_images_inner();
        }
        if self.budget_ok() {
            self.execute_scripts_inner();
        }
        Ok(())
    }

    /// Starts a navigation budget, returning `true` when this call owns it.
    ///
    /// A `false` return means a script-triggered navigation is reusing the budget
    /// started higher up the stack, so the deadline still spans the whole chain.
    fn begin_navigation(&mut self) -> bool {
        if self.nav.is_some() {
            return false;
        }
        self.last_truncation = None;
        self.nav = Some(NavigationBudget::new(self.settings.navigation_deadline));
        true
    }

    fn end_navigation(&mut self) {
        if let Some(budget) = self.nav.take() {
            self.last_truncation = budget.truncation();
        }
    }

    /// Checks the shared budget before a stage of work, logging the first
    /// truncation once so callers can see why the page is incomplete.
    fn budget_ok(&mut self) -> bool {
        let was_truncated = self
            .nav
            .as_ref()
            .is_some_and(NavigationBudget::is_truncated);
        let (allowed, reason) = match &mut self.nav {
            Some(budget) => (budget.allow(), budget.truncation()),
            None => (true, None),
        };
        if !allowed
            && !was_truncated
            && let Some(reason) = reason
        {
            self.note_truncation(reason);
        }
        allowed
    }

    fn note_truncation(&mut self, reason: Truncation) {
        self.console
            .push(format!("[nav] {reason}; showing partial page"));
    }

    /// Absolute instant shared by subresource fetches of the current navigation.
    ///
    /// Module and classic scripts are fetched with this deadline so a slow
    /// script cannot outlive the budget that covers the document itself.
    fn subresource_deadline(&self) -> Instant {
        let remaining = self
            .nav
            .as_ref()
            .map_or(self.settings.navigation_deadline, |nav| {
                nav.deadline().remaining()
            });
        Instant::now() + remaining
    }

    /// Reserves one chained-navigation hop, guarding against loops and deadline
    /// overruns. The same shared budget is reused by the nested navigation.
    fn reserve_hop(&mut self, key: &str) -> bool {
        let was_truncated = self
            .nav
            .as_ref()
            .is_some_and(NavigationBudget::is_truncated);
        let hop = match &mut self.nav {
            Some(budget) => budget.begin_hop(key),
            None => Ok(()),
        };
        match hop {
            Ok(()) => true,
            Err(reason) => {
                if !was_truncated {
                    self.note_truncation(reason);
                }
                false
            }
        }
    }

    /// Goes back in history.
    pub fn back(&mut self) -> Result<Snapshot, BrowserError> {
        let tab = self.tabs.active_mut();
        let navigation = tab
            .history
            .navigate(NavigationAction::Back, None, "")
            .map_err(|_| BrowserError::NoPage)?;
        let target = navigation.url.clone();
        self.navigate_url(target, NavigationAction::Replace)
    }

    /// Goes forward in history.
    pub fn forward(&mut self) -> Result<Snapshot, BrowserError> {
        let tab = self.tabs.active_mut();
        let navigation = tab
            .history
            .navigate(NavigationAction::Forward, None, "")
            .map_err(|_| BrowserError::NoPage)?;
        let target = navigation.url.clone();
        self.navigate_url(target, NavigationAction::Replace)
    }

    /// Reloads the current URL.
    pub fn reload(&mut self) -> Result<Snapshot, BrowserError> {
        let Some(url) = self.page().url.clone() else {
            return Err(BrowserError::NoPage);
        };
        self.navigate_url(url, NavigationAction::Reload)
    }

    /// Activates a tab, returning its snapshot.
    pub fn select_tab(&mut self, id: TabId) -> Result<Snapshot, BrowserError> {
        self.tabs.select(id);
        Ok(self.snapshot())
    }

    /// Follows the link under a point, if any.
    pub fn click(&mut self, x: f32, y: f32) -> Option<Result<Snapshot, BrowserError>> {
        let url = self.page().link_at(x, y)?;
        Some(self.navigate_url(url, NavigationAction::Push))
    }

    /// Submits a form with no explicit submitter, using `GET` by default.
    ///
    /// Successful controls are collected from the form's descendants, encoded as
    /// `application/x-www-form-urlencoded` and sent to the form's `action` (the
    /// current URL when omitted). The resulting navigation uses the same
    /// deadline and budget as [`Self::navigate_url`].
    pub fn submit_form(&mut self, form_node: NodeId) -> Result<Snapshot, BrowserError> {
        self.submit_form_with(form_node, None)
    }

    /// Submits a form, optionally activating a submit button.
    ///
    /// When `submitter` is a submit button with a `name`, its name/value pair is
    /// included in the submission (and remembered by [`Self::last_submitter`]).
    /// Submit buttons that were not activated are skipped, as are disabled and
    /// unnamed controls; `type=hidden` controls are included. Checkboxes and
    /// radio buttons contribute only when checked.
    pub fn submit_form_with(
        &mut self,
        form_node: NodeId,
        submitter: Option<NodeId>,
    ) -> Result<Snapshot, BrowserError> {
        let (action, post, fields, submitter_pair) = {
            let page = self.page();
            let document = page.document.borrow();
            if !document.contains(form_node)
                || !document
                    .element_name(form_node)
                    .is_some_and(|name| name.eq_ignore_ascii_case("form"))
            {
                return Err(BrowserError::InvalidForm);
            }
            let current = page.url.clone().ok_or(BrowserError::NoPage)?;
            let raw_action = document.attr(form_node, "action").unwrap_or("").trim();
            let action = if raw_action.is_empty() {
                current
            } else {
                current.join(raw_action)?
            };
            let method = document
                .attr(form_node, "method")
                .unwrap_or("get")
                .trim()
                .to_ascii_lowercase();
            let mut fields = form_fields(&document, form_node, submitter);
            let submitter_pair = submitter.and_then(|node| submitter_field(&document, node));
            // The submitter's own pair is collected by `form_fields`; store it
            // separately so embedders can inspect which button was used.
            if let Some((name, value)) = &submitter_pair
                && !fields
                    .iter()
                    .any(|field| field.0 == *name && field.1 == *value)
            {
                fields.push((name.clone(), value.clone()));
            }
            (action, method == "post", fields, submitter_pair)
        };

        let encoded = encode_form_fields(&fields);
        self.last_submitter = submitter_pair;
        let url = if post {
            action
        } else {
            append_query(&action, &encoded)?
        };
        if post {
            self.pending_form_post = Some(encoded.into_bytes());
        }
        let result = self.navigate_url(url, NavigationAction::Push);
        self.pending_form_post = None;
        result
    }

    /// The submit button name/value used by the most recent form submission.
    pub fn last_submitter(&self) -> Option<(&str, &str)> {
        self.last_submitter
            .as_ref()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Submits the form containing the focused element when it has exactly one
    /// text control (the implicit-submission rule for the Enter key).
    ///
    /// Returns `Ok(None)` when no element is focused, the element is not in a
    /// form, or the form does not have exactly one enabled text control.
    pub fn submit_focused_input(&mut self) -> Result<Option<Snapshot>, BrowserError> {
        let focused = {
            let Some(host) = self.page().host.clone() else {
                return Ok(None);
            };
            host.borrow().focused
        };
        let Some(focused) = focused else {
            return Ok(None);
        };
        let (form, submitter) = {
            let document = self.page().document.borrow();
            let Some(form) = browser_web_apis::form_for_input(&document, focused) else {
                return Ok(None);
            };
            if browser_web_apis::single_text_control(&document, form) != Some(focused) {
                return Ok(None);
            }
            (
                form,
                browser_web_apis::default_submit_button(&document, form),
            )
        };
        self.submit_form_with(form, submitter).map(Some)
    }

    /// Consumes a form submission recorded by script (`submit()` /
    /// `requestSubmit()`) or by an Enter keydown, and navigates.
    ///
    /// Returns `Ok(None)` when no submission is pending. Embedders that dispatch
    /// keyboard events themselves should call this afterwards so implicit
    /// submission takes effect.
    pub fn take_form_submission(&mut self) -> Result<Option<Snapshot>, BrowserError> {
        let submission = {
            let Some(host) = self.page().host.clone() else {
                return Ok(None);
            };
            host.borrow_mut().take_form_submission()
        };
        match submission {
            Some(submission) => self
                .submit_form_with(submission.form, submission.submitter)
                .map(Some),
            None => Ok(None),
        }
    }

    /// Scrolls the container under a point by `delta` pixels.
    pub fn scroll_at(&mut self, x: f32, y: f32, delta: f32) -> bool {
        let scrolled = self.tab_mut().page.scroll_at(x, y, 0.0, delta);
        if scrolled {
            let host = self.page().host.clone();
            if let Some(host) = host {
                crate::script::notify_observers(self.page(), &host);
                if let Some(runtime) = self.tab_mut().page.runtime.as_mut() {
                    crate::script::deliver_observers(runtime, &host);
                }
            }
        }
        scrolled
    }

    /// Current page snapshot.
    pub fn snapshot(&self) -> Snapshot {
        let page = self.page();
        Snapshot {
            url: page.url.as_ref().map(Url::as_str),
            title: page.title().to_string(),
            text: page.text(),
            node_count: page.node_count(),
            content_height: page.layout.document_height,
            command_count: page.display_list.len(),
            state: page.state.clone(),
            truncated: self.last_truncation.is_some(),
            truncation_reason: self.last_truncation,
        }
    }

    fn fetch_document(&mut self, url: &Url) -> Result<String, BrowserError> {
        match url.scheme() {
            "http" | "https" => {
                if !self.settings.enable_network {
                    return Err(BrowserError::NetworkDisabled);
                }
                if let Some(body) = self.pending_form_post.take() {
                    // A POST form submission is the document fetch itself: the
                    // form body goes on the wire and the response becomes the
                    // page, with the same subresource pipeline as a GET.
                    let request = Request::with_body(Method::Post, url.clone(), body)
                        .header("Content-Type", "application/x-www-form-urlencoded");
                    let response = self.client_mut().fetch(&request)?;
                    if !response.is_success() {
                        return Ok(error_page(url, response.status));
                    }
                    let media = response
                        .media_type()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    return text_document(url, media, response.text());
                }
                let resource = match self.loader.load(url) {
                    Ok(resource) => resource,
                    Err(browser_resource_loader::LoadError::Status(status)) => {
                        return Ok(error_page(url, status));
                    }
                    Err(error) => return Err(error.into()),
                };
                let media = resource.mime.to_ascii_lowercase();
                let body = resource.text();
                text_document(url, media, body)
            }
            "file" => {
                let path = format!("/{}", url.path_segments().join("/"));
                let bytes = std::fs::read(&path)
                    .map_err(|err| BrowserError::Io(format!("{path}: {err}")))?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            "about" => Ok(if url.path() == "/blank" || url.path() == "blank" {
                "<!DOCTYPE html><html><head><title>about:blank</title></head><body></body></html>"
                    .to_string()
            } else {
                "<!DOCTYPE html><html><head><title>about</title></head><body></body></html>"
                    .to_string()
            }),
            other => Err(BrowserError::Url(UrlError::NotSpecial)).map(|_: ()| {
                let _ = other;
                String::new()
            }),
        }
    }

    /// Re-reads `<meta http-equiv="Content-Security-Policy">` policies.
    ///
    /// Response-header policies are recorded by the network layer when available;
    /// this method merges the document-level policies of the active page.
    pub fn refresh_policy(&mut self) {
        let mut list = CspList::new();
        {
            let page = self.page();
            let document = page.document.borrow();
            for node in document.get_elements_by_tag_name("meta") {
                let equiv = document
                    .attr(node, "http-equiv")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if equiv != "content-security-policy" {
                    continue;
                }
                if let Some(content) = document.attr(node, "content") {
                    list.push(browser_security::csp::Policy::parse(content));
                }
            }
        }
        self.csp = list;
    }

    /// The active content security policy.
    pub fn policy(&self) -> &CspList {
        &self.csp
    }

    /// True when an inline script is allowed by the active policy.
    pub fn inline_scripts_allowed(&self) -> bool {
        self.csp.is_empty() || self.csp.allows_inline("script-src")
    }

    /// The page origin, if the page has one.
    fn page_origin(&self) -> browser_url::Origin {
        self.page()
            .url
            .as_ref()
            .map_or_else(browser_url::Origin::opaque, browser_url::Url::origin)
    }

    /// Fetches every `<link rel="stylesheet">` in the active page.
    fn load_linked_stylesheets(&mut self) {
        if let Some(url) = self.page().url.clone() {
            self.fetch_stylesheets_for(&url);
        }
    }

    fn fetch_stylesheets_for(&mut self, base: &Url) {
        // Inline `<style>` blocks were collected by `Page::load_html` and already
        // live in the page's author CSS; they are the first seeds for @import.
        let inline: Vec<String> = self.page().author_css().to_vec();

        let links = {
            let page = self.page();
            let document = page.document.borrow();
            let mut out = Vec::new();
            for node in document.get_elements_by_tag_name("link") {
                let rel = document.attr(node, "rel").unwrap_or_default();
                if !rel.to_ascii_lowercase().contains("stylesheet") {
                    continue;
                }
                let Some(href) = document.attr(node, "href") else {
                    continue;
                };
                if let Ok(url) = base.join(href) {
                    out.push(url);
                }
            }
            out
        };
        let origin = self.page_origin();
        let mut linked = Vec::new();
        let mut linked_seeds = Vec::new();
        for url in links {
            // Stop before issuing the next request once the wall clock is up.
            if !self.budget_ok() {
                break;
            }
            if !self.settings.enable_network && matches!(url.scheme(), "http" | "https") {
                self.console
                    .push(format!("[net] network disabled, skipping stylesheet {url}"));
                continue;
            }
            if !self.csp.is_empty() && !self.csp.allows("style-src", &url, &origin) {
                self.console.push(format!("[csp] blocked stylesheet {url}"));
                continue;
            }
            if let Ok(resource) = self.loader.load(&url) {
                let css = resource.text();
                linked_seeds.push((css.clone(), Some(url.as_str())));
                linked.push(css);
            }
        }

        // Resolve and fetch every @import reachable from inline + linked CSS.
        let mut seeds: Vec<(String, Option<String>)> =
            inline.into_iter().map(|css| (css, None)).collect();
        seeds.extend(linked_seeds);
        let imported = self.fetch_imports(base, &origin, seeds);

        let changed = !linked.is_empty() || !imported.is_empty();
        if !changed || !self.budget_ok() {
            return;
        }
        // Batch every new sheet into one style/layout/paint pass, which the
        // deadline can gate. Previously each sheet triggered a full recompute.
        let tab = self.tabs.active_mut();
        for css in linked {
            tab.page.add_author_css_no_recompute(&css);
        }
        for css in imported {
            tab.page.add_author_css_no_recompute(&css);
        }
        tab.page.recompute();
    }

    /// Resolves and fetches `@import`ed stylesheets for the active page.
    ///
    /// `seeds` pairs each source sheet's CSS with its absolute URL (`None` for
    /// inline styles). The style engine's depth/duplicate guards are applied by
    /// [`imports::ImportCollector`], and browser-side caps bound the number and
    /// total size of imported sheets. Returns the imported CSS in discovery
    /// order for the caller to append and recompute in a single pass.
    fn fetch_imports(
        &mut self,
        base: &Url,
        origin: &browser_url::Origin,
        seeds: Vec<(String, Option<String>)>,
    ) -> Vec<String> {
        let mut collector = imports::ImportCollector::new();
        // Never refetch a sheet we already obtained through <link> or inline.
        for (_, url) in &seeds {
            if let Some(url) = url {
                collector.seed(url);
            }
        }

        // One absolute deadline for the whole chain, derived from the shared
        // navigation budget so a slow import cannot outlive the page load.
        let deadline = match &self.nav {
            Some(budget) => Instant::now() + budget.deadline().remaining(),
            None => Instant::now() + self.loader.timeout(),
        };

        let mut queue: VecDeque<(String, Option<String>, usize)> =
            seeds.into_iter().map(|(css, url)| (css, url, 0)).collect();
        let mut out = Vec::new();

        while let Some((css, url, depth)) = queue.pop_front() {
            if !self.budget_ok() {
                break;
            }
            let requests = collector.add_sheet(&css, url.as_deref(), depth);
            for request in requests {
                if !self.budget_ok() {
                    break;
                }
                let Ok(import_url) = base.join(&request.url) else {
                    self.console
                        .push(format!("[import] invalid URL '{}'", request.url));
                    continue;
                };
                if !self.settings.enable_network && matches!(import_url.scheme(), "http" | "https")
                {
                    self.console.push(format!(
                        "[net] network disabled, skipping import {import_url}"
                    ));
                    continue;
                }
                if !self.csp.is_empty() && !self.csp.allows("style-src", &import_url, origin) {
                    self.console
                        .push(format!("[csp] blocked import {import_url}"));
                    continue;
                }
                // Reserve by absolute URL *before* fetching so a cycle can
                // never issue a second request for the same sheet.
                if !collector.reserve(&import_url.as_str()) {
                    continue;
                }
                match self.loader.load_with_deadline(&import_url, deadline) {
                    Ok(resource) => {
                        let css = resource.text();
                        if !collector.accept_bytes(css.len()) {
                            self.console
                                .push(format!("[import] byte cap reached, skipping {import_url}"));
                            continue;
                        }
                        out.push(css.clone());
                        queue.push_back((css, Some(import_url.as_str()), request.depth));
                    }
                    Err(error) => self
                        .console
                        .push(format!("[import] failed to load {import_url}: {error}")),
                }
            }
        }
        out
    }

    /// Fetches and decodes `<img src>` resources of the active page.
    ///
    /// Images are checked against `img-src` when a CSP is active and skipped when
    /// they exceed the configured body limit or cannot be decoded. Failures are
    /// reported on the console and the placeholder continues to be painted.
    pub fn load_images(&mut self) {
        self.load_images_inner();
    }

    fn load_images_inner(&mut self) {
        let targets: Vec<(browser_dom::NodeId, String)> = {
            let page = self.page();
            let document = page.document.borrow();
            document
                .get_elements_by_tag_name("img")
                .into_iter()
                .filter_map(|node| {
                    document
                        .attr(node, "src")
                        .map(|src| (node, src.to_string()))
                })
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        let Some(base) = self.page().url.clone() else {
            return;
        };
        let origin = self.page_origin();
        for (node, src) in targets {
            if !self.budget_ok() {
                break;
            }
            let Ok(url) = base.join(&src) else { continue };
            if !self.csp.is_empty() && !self.csp.allows("img-src", &url, &origin) {
                self.console.push(format!("[csp] blocked image {url}"));
                continue;
            }
            match self.loader.load(&url) {
                Ok(resource) => match browser_paint::ImageData::decode(&resource.bytes) {
                    Ok(image) => {
                        self.tab_mut().page.images.insert(node, image);
                    }
                    Err(error) => self.console.push(format!("[image] {url}: {}", error)),
                },
                Err(error) => self.console.push(format!("[image] {url}: {error}")),
            }
        }
        // Layout/paint after image decode is the last, potentially expensive
        // pass; skip it when the budget is already spent.
        if self.budget_ok() {
            self.tab_mut().page.recompute();
        }
    }

    /// Collects the page's scripts in document order, fetching `src` scripts.
    ///
    /// Both classic scripts and ES modules are included. External scripts are
    /// resolved against the document URL and fetched with the shared navigation
    /// deadline; inline scripts use their text content. Inline scripts are
    /// filtered by the active CSP and external ones are checked against
    /// `script-src` before any request is made. The returned scripts are already
    /// sorted into browser execution order (blocking classics, then deferred
    /// classics and modules, then `async` scripts).
    fn collect_scripts(&mut self) -> Vec<module_scripts::FetchedScript> {
        use module_scripts::{FetchedScript, PageScript, ScriptKind};

        let origin = self.page_origin();
        let page_scripts: Vec<PageScript> = {
            let page = self.page();
            let document = page.document.borrow();
            module_scripts::classify_scripts(&document)
        };

        let base = self.page().url.clone();
        let deadline = self.subresource_deadline();
        let mut scripts = Vec::new();
        for (order, page_script) in page_scripts.into_iter().enumerate() {
            if !self.budget_ok() {
                break;
            }
            let PageScript {
                kind,
                src,
                inline,
                ready_group,
            } = page_script;
            match src {
                Some(src) => {
                    let Some(base) = &base else { continue };
                    let Ok(url) = base.join(&src) else { continue };
                    if !self.csp.is_empty() && !self.csp.allows("script-src", &url, &origin) {
                        self.console.push(format!("[csp] blocked script {url}"));
                        continue;
                    }
                    let resource = match self.loader.load_with_deadline(&url, deadline) {
                        Ok(resource) => resource,
                        Err(error) => {
                            self.console
                                .push(format!("[script] failed to load {url}: {error}"));
                            continue;
                        }
                    };
                    let name = match kind {
                        ScriptKind::Module => format!("module:{url}"),
                        ScriptKind::Classic => format!("external:{url}"),
                    };
                    scripts.push(FetchedScript {
                        kind,
                        source: resource.text(),
                        url: Some(url.as_str().to_string()),
                        name,
                        order,
                        ready_group,
                    });
                }
                None => {
                    if !self.inline_scripts_allowed() {
                        self.console.push("[csp] blocked inline script".to_string());
                        continue;
                    }
                    let name = match kind {
                        ScriptKind::Module => format!("inline-module-{order}"),
                        ScriptKind::Classic => format!("inline-{order}"),
                    };
                    scripts.push(FetchedScript {
                        kind,
                        source: inline,
                        url: None,
                        name,
                        order,
                        ready_group,
                    });
                }
            }
        }
        module_scripts::execution_order(scripts)
    }

    /// Executes page scripts (JavaScript) and applies DOM mutations.
    ///
    /// Scripts may request navigation through `location.href`; up to eight
    /// chained navigations are followed, which is enough for router-style pages
    /// without allowing an infinite redirect loop inside one call.
    ///
    /// Returns the number of scripts that ran.
    pub fn execute_scripts(&mut self) -> usize {
        self.execute_scripts_inner()
    }

    fn execute_scripts_inner(&mut self) -> usize {
        if !self.settings.enable_scripting {
            return 0;
        }
        if !self.budget_ok() {
            return 0;
        }
        let scripts = self.collect_scripts();
        if scripts.is_empty() {
            return 0;
        }
        // Static imports discovered while linking a module are fetched through
        // the runtime's host hook with the same budget and HTTP settings as the
        // scripts we collected here.
        let module_context = module_scripts::ModuleFetchContext {
            base_url: self.page().url.as_ref().map(ToString::to_string),
            deadline: self.subresource_deadline(),
            client_config: self.loader.client().config().clone(),
        };
        let (count, console) = {
            let tab = self.tabs.active_mut();
            module_scripts::execute_scripts_with_loader(
                &mut tab.page,
                &scripts,
                Some(module_context),
            )
        };
        self.console.extend(console);
        if count > 0 && self.budget_ok() {
            self.tab_mut().page.recompute();
        }

        loop {
            if !self.budget_ok() {
                break;
            }
            // Scripts can request a form submission (`form.submit()` /
            // `requestSubmit()`) instead of a plain URL navigation.
            let submission = {
                let Some(host) = self.page().host.clone() else {
                    break;
                };
                host.borrow_mut().take_form_submission()
            };
            if let Some(submission) = submission {
                if !self.reserve_hop(&format!("form:{}", submission.form.0)) {
                    break;
                }
                if self
                    .submit_form_with(submission.form, submission.submitter)
                    .is_err()
                {
                    break;
                }
                continue;
            }
            let Some(request) = self.tab_mut().page.take_pending_navigation() else {
                break;
            };
            // Reserve the hop in the shared budget so chained script navigations
            // cannot recurse forever or revisit a URL in a loop. The budget is
            // reused by the nested `navigate`, so the deadline spans the chain.
            if !self.reserve_hop(&request) {
                break;
            }
            if self.navigate(&request).is_err() {
                break;
            }
        }
        count
    }

    // ---- Service workers -------------------------------------------------

    /// The page's host-side service worker mirror.
    pub fn service_workers(&self) -> &crate::service_workers::ServiceWorkerMirror {
        &self.page().service_workers
    }

    /// Registers a service worker for `scope`.
    ///
    /// `script_url` and the resolved scope must be secure contexts (HTTPS, or
    /// loopback HTTP for local tests). `source`, when supplied, is the worker
    /// script body evaluated for `install`, `activate` and `fetch` listeners.
    pub fn register_service_worker(
        &mut self,
        scope: &str,
        script_url: &str,
        source: Option<&str>,
    ) -> Result<(), String> {
        let base = self.page().url.clone();
        self.tab_mut()
            .page
            .service_workers
            .register(scope, script_url, source, base.as_ref())
    }

    /// Activates the worker registered for `scope`.
    pub fn activate_service_worker(&mut self, scope: &str) -> Result<(), String> {
        self.tab_mut().page.service_workers.activate(scope)
    }

    /// Removes the worker registered for `scope`.
    pub fn unregister_service_worker(&mut self, scope: &str) -> bool {
        self.tab_mut().page.service_workers.unregister(scope)
    }

    /// Snapshot of current registrations (including redundant ones).
    pub fn service_worker_registrations(&self) -> Vec<Registration> {
        self.page().service_workers.registrations()
    }

    /// Events recorded by the worker script handler, awaiting JS dispatch.
    pub fn take_service_worker_events(&mut self) -> Vec<ServiceWorkerEvent> {
        self.tab_mut().page.service_workers.take_events()
    }

    /// Delivers recorded worker events to the page's JS listeners.
    pub fn run_service_worker_events(&mut self) -> usize {
        let Some(host) = self.page().host.clone() else {
            return 0;
        };
        let Some(runtime) = self.tab_mut().page.runtime.as_mut() else {
            return 0;
        };
        let delivered = browser_web_apis::run_service_worker_events(runtime, &host);
        let _ = runtime.run_microtasks();
        delivered
    }

    // ---- WebSocket -------------------------------------------------------

    /// Polls the TCP WebSocket transport and dispatches events to JS handlers.
    pub fn pump_websockets(&mut self) -> usize {
        let Some(host) = self.page().host.clone() else {
            return 0;
        };
        let Some(runtime) = self.tab_mut().page.runtime.as_mut() else {
            return 0;
        };
        let applied = browser_web_apis::pump_websockets(runtime, &host);
        let _ = runtime.run_microtasks();
        applied
    }

    // ---- IndexedDB -------------------------------------------------------

    /// True when a database with `name` has been opened on this page.
    pub fn has_indexed_db(&self, name: &str) -> bool {
        self.page()
            .host
            .as_ref()
            .is_some_and(|host| host.borrow().indexed_db.databases.contains_key(name))
    }

    /// Names of every database opened through the page's `indexedDB` global.
    pub fn indexed_db_names(&self) -> Vec<String> {
        self.page().host.as_ref().map_or_else(Vec::new, |host| {
            host.borrow().indexed_db.databases.keys().cloned().collect()
        })
    }

    /// Runs `f` with read-only access to a named database.
    pub fn with_indexed_db<T>(&self, name: &str, f: impl FnOnce(&IdbDatabase) -> T) -> Option<T> {
        let host = self.page().host.as_ref()?;
        let state = host.borrow();
        state.indexed_db.databases.get(name).map(f)
    }

    /// Ensures a named database exists in the page's HostState.
    ///
    /// The `indexedDB` JavaScript global reads and writes these same
    /// [`browser_navigation::IdbDatabase`] handles, so data persists across
    /// every script execution and microtask drain on the page.
    pub fn ensure_indexed_db(&mut self, name: &str, version: u32) {
        crate::script::ensure_runtime(&mut self.tab_mut().page);
        let host = self.page().host.clone().expect("runtime installed");
        let mut state = host.borrow_mut();
        if !state.indexed_db.databases.contains_key(name) {
            state
                .indexed_db
                .databases
                .insert(name.to_string(), IdbDatabase::open(name, version));
        }
    }

    /// Dispatches a DOM event at the node under a point (used by click handling).
    pub fn dispatch_pointer_event(&mut self, x: f32, y: f32, event_type: &str) -> bool {
        let Some(node) = self.page().hit_test_node(x, y) else {
            return false;
        };
        let handled = self.tab_mut().page.dispatch_event(node, event_type);
        if handled {
            self.tab_mut().page.recompute();
        }
        handled
    }
}

/// Renders a fetched body as a document, or a placeholder for binary media.
fn text_document(url: &Url, media: String, body: String) -> Result<String, BrowserError> {
    if media.is_empty()
        || media.contains("html")
        || media.contains("xml")
        || media.starts_with("text/plain")
    {
        Ok(body)
    } else {
        Ok(format!(
            "<!DOCTYPE html><html><head><title>{}</title></head>\
             <body><h1>{}</h1><pre>Conteúdo não textual: {}</pre></body></html>",
            escape_text(&url.as_str()),
            escape_text(&url.as_str()),
            escape_text(&media)
        ))
    }
}

const FORM_HEX: &[u8; 16] = b"0123456789ABCDEF";

/// `application/x-www-form-urlencoded` percent-encoding of a single string.
///
/// Unlike URL query encoding, spaces become `+`, `*` is left alone and every
/// `%` is escaped (form values are always literal text, never existing escapes).
fn form_urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(FORM_HEX[(byte >> 4) as usize] as char);
                out.push(FORM_HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Encodes an ordered list of successful controls as a form body.
fn encode_form_fields(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", form_urlencode(name), form_urlencode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Appends an encoded form query to `url`, before any fragment.
fn append_query(url: &Url, query: &str) -> Result<Url, BrowserError> {
    let serialised = url.as_str();
    let (base, fragment) = match serialised.split_once('#') {
        Some((base, fragment)) => (base.to_string(), Some(fragment.to_string())),
        None => (serialised, None),
    };
    let mut combined = if base.contains('?') {
        format!("{base}&{query}")
    } else {
        format!("{base}?{query}")
    };
    if let Some(fragment) = fragment {
        combined.push('#');
        combined.push_str(&fragment);
    }
    Ok(Url::parse(&combined)?)
}

/// Lowercased tag name of a node, or an empty string for non-elements.
fn node_tag(document: &Document, node: NodeId) -> String {
    document
        .element_name(node)
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// True when a control is disabled, honouring the `fieldset`/`legend` rule.
fn is_disabled_control(document: &Document, node: NodeId, form: NodeId) -> bool {
    let mut current = node;
    let mut from_first_legend = false;
    loop {
        if current == form {
            return false;
        }
        if !from_first_legend && document.has_attr(current, "disabled") {
            return true;
        }
        let Some(parent) = document.parent(current) else {
            return false;
        };
        from_first_legend = node_tag(document, parent) == "fieldset"
            && node_tag(document, current) == "legend"
            && document
                .children(parent)
                .iter()
                .copied()
                .find(|child| node_tag(document, *child) == "legend")
                == Some(current);
        current = parent;
    }
}

/// Nodes below `root` in document order.
fn descendants(document: &Document, root: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = document.children(root).iter().rev().copied().collect();
    while let Some(node) = stack.pop() {
        out.push(node);
        stack.extend(document.children(node).iter().rev().copied());
    }
    out
}

/// The submitted value of an `<option>`: its `value` attribute or its text.
fn option_value(document: &Document, option: NodeId) -> String {
    document
        .attr(option, "value")
        .map(str::to_string)
        .unwrap_or_else(|| document.text_content(option).trim().to_string())
}

/// Name/value pair contributed by an activated submit button, if any.
fn submitter_field(document: &Document, node: NodeId) -> Option<(String, String)> {
    let name = document.attr(node, "name").unwrap_or("");
    if name.is_empty() {
        return None;
    }
    let kind = document
        .attr(node, "type")
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_submit = match node_tag(document, node).as_str() {
        "button" => kind.is_empty() || kind == "submit",
        "input" => kind == "submit" || kind == "image",
        _ => false,
    };
    is_submit.then(|| {
        (
            name.to_string(),
            document.attr(node, "value").unwrap_or("").to_string(),
        )
    })
}

/// Collects successful controls below `node` into `out`, in document order.
fn collect_form_fields(
    document: &Document,
    form: NodeId,
    node: NodeId,
    submitter: Option<NodeId>,
    out: &mut Vec<(String, String)>,
) {
    for &child in document.children(node) {
        let tag = node_tag(document, child);
        // A disabled control contributes nothing, but we still descend: a
        // control in the first <legend> of a disabled <fieldset> is exempt.
        let disabled = !tag.is_empty() && is_disabled_control(document, child, form);
        let name = document.attr(child, "name").unwrap_or("");
        match (tag.as_str(), name.is_empty(), disabled) {
            ("input", false, false) => {
                let kind = document
                    .attr(child, "type")
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match kind.as_str() {
                    "submit" | "button" | "reset" | "image" => {
                        if Some(child) == submitter {
                            out.push((
                                name.to_string(),
                                document.attr(child, "value").unwrap_or("").to_string(),
                            ));
                        }
                    }
                    "checkbox" | "radio" => {
                        if document.has_attr(child, "checked") {
                            out.push((
                                name.to_string(),
                                document.attr(child, "value").unwrap_or("on").to_string(),
                            ));
                        }
                    }
                    "file" => {} // binary uploads are not represented as form text
                    _ => {
                        // `text`, `hidden`, `search`, ... and missing `type`.
                        out.push((
                            name.to_string(),
                            document.attr(child, "value").unwrap_or("").to_string(),
                        ));
                    }
                }
            }
            ("textarea", false, false) => {
                out.push((name.to_string(), document.text_content(child)));
            }
            ("select", false, false) => {
                let options: Vec<NodeId> = descendants(document, child)
                    .into_iter()
                    .filter(|option| {
                        node_tag(document, *option) == "option"
                            && !is_disabled_control(document, *option, form)
                    })
                    .collect();
                let selected: Vec<NodeId> = options
                    .iter()
                    .copied()
                    .filter(|option| document.has_attr(*option, "selected"))
                    .collect();
                if document.has_attr(child, "multiple") {
                    for option in selected {
                        out.push((name.to_string(), option_value(document, option)));
                    }
                } else if let Some(option) = selected
                    .first()
                    .copied()
                    .or_else(|| options.first().copied())
                {
                    out.push((name.to_string(), option_value(document, option)));
                } else {
                    out.push((name.to_string(), String::new()));
                }
            }
            ("button", false, false) if Some(child) == submitter => {
                let kind = document
                    .attr(child, "type")
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if kind.is_empty() || kind == "submit" {
                    out.push((
                        name.to_string(),
                        document.attr(child, "value").unwrap_or("").to_string(),
                    ));
                }
            }
            _ => {}
        }
        collect_form_fields(document, form, child, submitter, out);
    }
}

/// All successful controls of a form, in document order.
fn form_fields(
    document: &Document,
    form: NodeId,
    submitter: Option<NodeId>,
) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    collect_form_fields(document, form, form, submitter, &mut fields);
    fields
}

/// A minimal error page, rendered by the engine like any other document.
fn error_page(url: &Url, status: u16) -> String {
    format!(
        "<!DOCTYPE html><html><head><title>{status}</title></head>\
         <body style='font-family: monospace; padding: 24px'>\
         <h1>HTTP {status}</h1><p>{}</p>\
         <p>O pedido falhou. Verifique o endereço e tente novamente.</p>\
         </body></html>",
        escape_text(&url.as_str())
    )
}

fn normalize_url(input: &str) -> Result<Url, BrowserError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(BrowserError::Url(UrlError::MissingScheme));
    }
    if let Ok(url) = Url::parse(input) {
        return Ok(url);
    }
    if input.contains(' ') || input.contains('/') && !input.contains('.') {
        // Treat as a search-free plain path relative to https?
        return Url::parse(&format!("https://{input}")).map_err(BrowserError::Url);
    }
    Url::parse(&format!("https://{input}")).map_err(BrowserError::Url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browser() -> Browser {
        let mut settings = Settings::headless();
        settings.enable_network = false;
        Browser::new(settings)
    }

    #[test]
    fn loads_inline_html_and_snapshots() {
        let mut b = browser();
        let snapshot = b
            .load_html(
                "<!DOCTYPE html><html><head><title>Hi</title></head><body><p>Hello world</p></body></html>",
                None,
            )
            .unwrap();
        assert_eq!(snapshot.title, "Hi");
        assert!(snapshot.text.contains("Hello world"));
        assert!(snapshot.node_count >= 4);
        assert!(snapshot.command_count > 0);
        assert!(
            b.render()
                .coverage_vs(browser_css::Color::rgb(255, 255, 255))
                > 0.0
        );
    }

    #[test]
    fn navigation_is_disabled_without_network() {
        let mut b = browser();
        let err = b.navigate("https://example.com").unwrap_err();
        assert!(matches!(err, BrowserError::NetworkDisabled));
    }

    #[test]
    fn file_urls_load_from_disk() {
        let dir = std::env::temp_dir().join(format!("mag-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("page.html");
        std::fs::write(
            &path,
            "<html><head><title>File</title></head><body><p>from disk</p></body></html>",
        )
        .unwrap();
        let mut b = browser();
        let url = format!("file://{}", path.display());
        let snapshot = b.navigate(&url).unwrap();
        assert_eq!(snapshot.title, "File");
        assert!(snapshot.text.contains("from disk"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tabs_are_independent() {
        let mut b = browser();
        b.load_html(
            "<html><head><title>One</title></head><body></body></html>",
            None,
        )
        .unwrap();
        let first = b.tab().id;
        b.new_tab();
        b.load_html(
            "<html><head><title>Two</title></head><body></body></html>",
            None,
        )
        .unwrap();
        assert_eq!(b.snapshot().title, "Two");
        b.select_tab(first).unwrap();
        assert_eq!(b.snapshot().title, "One");
    }

    #[test]
    fn normalizes_bare_domains() {
        let url = normalize_url("example.com/path").unwrap();
        assert_eq!(url.as_str(), "https://example.com/path");
    }

    #[test]
    fn ordinary_load_is_not_truncated() {
        let mut b = browser();
        let snapshot = b
            .load_html("<html><body><p>fine</p></body></html>", None)
            .unwrap();
        assert!(!snapshot.is_truncated());
        assert_eq!(snapshot.truncation_reason, None);
    }

    #[test]
    fn zero_deadline_keeps_the_parsed_document_and_reports_truncation() {
        let mut settings = Settings::headless();
        settings.navigation_deadline = Duration::ZERO;
        let mut b = Browser::new(settings);
        let snapshot = b
            .load_html("<html><body><p>partial</p></body></html>", None)
            .unwrap();
        assert!(snapshot.is_truncated());
        assert_eq!(snapshot.truncation_reason, Some(Truncation::Deadline));
        // The main document is always parsed and laid out: the page is usable.
        assert!(snapshot.text.contains("partial"));
        assert!(snapshot.command_count > 0);
        assert!(
            b.console()
                .iter()
                .any(|line| line.contains("deadline exceeded"))
        );
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Instant;

    /// Minimal HTTP/1.1 server. `handler` maps a request path to a response body
    /// and an artificial delay, so slow subresources can be simulated locally.
    fn serve<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> (String, Duration) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0_u8; 4096];
                        let read = stream.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..read]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        let (body, delay) = handler(&path);
                        if !delay.is_zero() {
                            thread::sleep(delay);
                        }
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (addr, stop, handle)
    }

    #[test]
    fn slow_subresources_are_cut_off_by_the_wall_clock_deadline() {
        let (addr, stop, handle) = serve(|path| {
            if path == "/" {
                (
                    "<html><head><title>Heavy</title>\
                     <link rel='stylesheet' href='/slow.css'></head>\
                     <body><h1>Partial page</h1><img src='/slow.png'></body></html>"
                        .to_string(),
                    Duration::ZERO,
                )
            } else {
                ("body{}".to_string(), Duration::from_secs(2))
            }
        });
        let mut settings = Settings::headless();
        settings.enable_network = true;
        settings.navigation_deadline = Duration::from_millis(400);
        let mut browser = Browser::new(settings);
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.title, "Heavy");
        assert!(snapshot.text.contains("Partial page"));
        assert!(snapshot.is_truncated());
        assert_eq!(snapshot.truncation_reason, Some(Truncation::Deadline));
        // No image was decoded after the deadline elapsed.
        assert!(browser.page().images.is_empty());
    }

    #[test]
    fn chained_navigation_loop_stops_quickly() {
        let (addr, stop, handle) = serve(|_path| {
            (
                "<html><head><title>Loop</title></head><body>\
                 <script>location.href='loop'</script></body></html>"
                    .to_string(),
                Duration::ZERO,
            )
        });
        let mut settings = Settings::headless();
        settings.enable_network = true;
        settings.navigation_deadline = Duration::from_secs(5);
        let mut browser = Browser::new(settings);
        let started = Instant::now();
        let snapshot = browser.navigate(&format!("http://{addr}/loop")).unwrap();
        let elapsed = started.elapsed();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.truncation_reason, Some(Truncation::Loop));
        assert!(
            elapsed < Duration::from_secs(4),
            "loop guard should stop promptly, took {elapsed:?}"
        );
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    /// Minimal HTTP/1.1 server mapping a request path to (content type, body).
    fn serve<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> (&'static str, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut buf = [0_u8; 4096];
                        let read = stream.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..read]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        let (mime, body) = handler(&path);
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (addr, stop, handle)
    }

    #[test]
    fn imported_stylesheets_apply_rules() {
        let (addr, stop, handle) = serve(|path| match path {
            "/" => (
                "text/html",
                "<!DOCTYPE html><html><head><title>Imports</title>\
                 <style>h1 { color: rgb(1, 2, 3); }</style>\
                 <link rel='stylesheet' href='/linked.css'></head>\
                 <body><h1>Heading</h1><p>Paragraph</p><span>Span</span></body></html>"
                    .to_string(),
            ),
            "/linked.css" => (
                "text/css",
                "@import 'imported.css';\np { color: rgb(0, 128, 0); }".to_string(),
            ),
            "/imported.css" => ("text/css", "span { color: rgb(220, 20, 60); }".to_string()),
            _ => ("text/plain", String::new()),
        });

        let mut settings = Settings::headless();
        settings.enable_network = true;
        settings.navigation_deadline = Duration::from_secs(5);
        let mut browser = Browser::new(settings);
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.title, "Imports");
        assert!(snapshot.text.contains("Heading"));

        let page = browser.page();
        let document = page.document.borrow();
        let h1 = document.get_elements_by_tag_name("h1")[0];
        let p = document.get_elements_by_tag_name("p")[0];
        let span = document.get_elements_by_tag_name("span")[0];
        // Rule from an inline <style> block (seed CSS).
        assert_eq!(
            page.styles.get(h1).unwrap().color(),
            browser_css::Color::rgb(1, 2, 3)
        );
        // Rule from the importing (linked) stylesheet.
        assert_eq!(
            page.styles.get(p).unwrap().color(),
            browser_css::Color::rgb(0, 128, 0)
        );
        // Rule from the stylesheet fetched through @import.
        assert_eq!(
            page.styles.get(span).unwrap().color(),
            browser_css::Color::rgb(220, 20, 60)
        );
    }
}

#[cfg(test)]
mod form_tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    const RESULTS: &str = "<!DOCTYPE html><html><head><title>Results</title></head>\
                           <body><p>results page</p></body></html>";

    /// One recorded HTTP request, used to assert what the form actually sent.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Recorded {
        method: String,
        target: String,
        body: String,
    }

    type TestServer = (
        String,
        Arc<Mutex<Vec<Recorded>>>,
        Arc<AtomicBool>,
        thread::JoinHandle<()>,
    );

    /// Minimal HTTP/1.1 server that records method, target and body and serves
    /// `form_html` at `/` and a result page for any `/search...` request.
    fn serve(form_html: String) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let log_flag = Arc::clone(&log);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop_flag.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(_) => {
                        // Non-blocking accept also fails with transient
                        // interruptions; never abandon a client mid-request.
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                };
                let mut buffer = Vec::new();
                let mut chunk = [0_u8; 4096];
                let header_end = loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 || buffer.len() > 64 * 1024 {
                        break None;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(position + 4);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or("");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("GET").to_string();
                let target = parts.next().unwrap_or("/").to_string();
                let content_length = lines
                    .filter_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .next()
                    .unwrap_or(0);
                let mut body = buffer[header_end..].to_vec();
                while body.len() < content_length {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..read]);
                }
                log_flag.lock().unwrap().push(Recorded {
                    method,
                    target,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let html = if buffer[..header_end].starts_with(b"GET /search")
                    || buffer[..header_end].starts_with(b"POST /search")
                {
                    RESULTS.to_string()
                } else {
                    form_html.clone()
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    html.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(html.as_bytes());
                let _ = stream.flush();
            }
        });
        (addr, log, stop, handle)
    }

    fn settings() -> Settings {
        let mut settings = Settings::headless();
        settings.enable_network = true;
        settings.navigation_deadline = Duration::from_secs(5);
        settings
    }

    fn stop_server(stop: Arc<AtomicBool>, handle: thread::JoinHandle<()>) {
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }

    #[test]
    fn successful_controls_are_collected_and_encoded() {
        let document = browser_html_parser::parse_document(
            "<html><body><form id='f' action='/search'>\
               <input name='q' value='hello world'>\
               <input type='hidden' name='token' value='a%b'>\
               <input name='ignored' value='x' disabled>\
               <fieldset disabled><legend><input name='in-legend' value='L'></legend>\
                 <input name='in-fieldset' value='F'></fieldset>\
               <input type='checkbox' name='yes' value='1' checked>\
               <input type='checkbox' name='no' value='2'>\
               <input type='radio' name='pick' value='a'>\
               <input type='radio' name='pick' value='b' checked>\
               <input type='submit' name='go' value='Search'>\
               <input type='submit' name='other' value='Other'>\
               <textarea name='note'>café & more</textarea>\
               <select name='color'>\
                 <option>red</option><option value='g' selected>green</option>\
               </select>\
               <select name='tags' multiple>\
                 <option value='x' selected>X</option><option value='y'>Y</option>\
                 <option value='z' selected>Z</option>\
               </select>\
               <input value='nameless'>\
             </form></body></html>",
        );
        let form = document.get_element_by_id("f").unwrap();
        let go = document
            .get_elements_by_tag_name("input")
            .into_iter()
            .find(|node| document.attr(*node, "value") == Some("Search"))
            .unwrap();

        let fields = form_fields(&document, form, Some(go));
        assert_eq!(
            fields,
            vec![
                ("q".into(), "hello world".into()),
                ("token".into(), "a%b".into()),
                ("in-legend".into(), "L".into()),
                ("yes".into(), "1".into()),
                ("pick".into(), "b".into()),
                ("go".into(), "Search".into()),
                ("note".into(), "café & more".into()),
                ("color".into(), "g".into()),
                ("tags".into(), "x".into()),
                ("tags".into(), "z".into()),
            ]
        );
        let encoded = encode_form_fields(&fields);
        assert!(encoded.starts_with("q=hello+world&token=a%25b&in-legend=L"));
        assert!(encoded.contains("note=caf%C3%A9+%26+more"));
        assert!(encoded.contains("go=Search"));
        assert!(!encoded.contains("other"));
        assert!(!encoded.contains("ignored"));
        assert!(!encoded.contains("in-fieldset"));
    }

    #[test]
    fn query_is_appended_before_the_fragment() {
        let url = Url::parse("http://x.test/search?existing=1#frag").unwrap();
        let joined = append_query(&url, "q=test").unwrap();
        assert_eq!(
            joined.as_str(),
            "http://x.test/search?existing=1&q=test#frag"
        );
    }

    #[test]
    fn get_form_navigates_to_action_with_query() {
        let (addr, log, stop, handle) = serve(
            "<html><head><title>Form</title></head><body>\
             <form action='/search' method='get'><input name='q' value='test'></form>\
             </body></html>"
                .to_string(),
        );
        let mut browser = Browser::new(settings());
        browser.navigate(&format!("http://{addr}/")).unwrap();
        let form = browser
            .page()
            .document
            .borrow()
            .get_elements_by_tag_name("form")[0];
        let snapshot = browser.submit_form(form).unwrap();

        let expected = format!("http://{addr}/search?q=test");
        assert_eq!(snapshot.url.as_deref(), Some(expected.as_str()));
        assert_eq!(snapshot.title, "Results");
        let requests = log.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].target, "/search?q=test");
        stop_server(stop, handle);
    }

    #[test]
    fn post_form_sends_urlencoded_body() {
        let (addr, log, stop, handle) = serve(
            "<html><head><title>Form</title></head><body>\
             <form action='/search' method='POST'>\
               <input name='q' value='hello world'>\
               <input type='hidden' name='token' value='a%b'>\
               <input name='off' value='nope' disabled>\
               <input type='checkbox' name='c' value='1'>\
             </form></body></html>"
                .to_string(),
        );
        let mut browser = Browser::new(settings());
        browser.navigate(&format!("http://{addr}/")).unwrap();
        let form = browser
            .page()
            .document
            .borrow()
            .get_elements_by_tag_name("form")[0];
        let snapshot = browser.submit_form(form).unwrap();

        assert_eq!(
            snapshot.url.as_deref(),
            Some(format!("http://{addr}/search").as_str())
        );
        assert_eq!(snapshot.title, "Results");
        let requests = log.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].target, "/search");
        assert_eq!(requests[1].body, "q=hello+world&token=a%25b");
        stop_server(stop, handle);
    }

    #[test]
    fn request_submit_includes_the_activated_button() {
        let (addr, log, stop, handle) = serve(
            "<html><head><title>Form</title></head><body>\
             <form action='/search'>\
               <input name='q' value='test'>\
               <button type='submit' name='go' value='yes'>Go</button>\
             </form></body></html>"
                .to_string(),
        );
        let mut browser = Browser::new(settings());
        browser.navigate(&format!("http://{addr}/")).unwrap();
        let (form, button) = {
            let document = browser.page().document.borrow();
            (
                document.get_elements_by_tag_name("form")[0],
                document.get_elements_by_tag_name("button")[0],
            )
        };
        let snapshot = browser.submit_form_with(form, Some(button)).unwrap();

        let expected = format!("http://{addr}/search?q=test&go=yes");
        assert_eq!(snapshot.url.as_deref(), Some(expected.as_str()));
        assert_eq!(browser.last_submitter(), Some(("go", "yes")));
        assert_eq!(log.lock().unwrap()[1].target, "/search?q=test&go=yes");
        stop_server(stop, handle);
    }

    #[test]
    fn script_can_submit_a_form() {
        let (addr, log, stop, handle) = serve(
            "<html><head><title>Form</title></head><body>\
             <form action='/search'><input name='q' value='fromscript'></form>\
             <script>document.getElementsByTagName('form')[0].submit();</script>\
             </body></html>"
                .to_string(),
        );
        let mut browser = Browser::new(settings());
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();

        let expected = format!("http://{addr}/search?q=fromscript");
        assert_eq!(snapshot.url.as_deref(), Some(expected.as_str()));
        assert_eq!(log.lock().unwrap().len(), 2);
        stop_server(stop, handle);
    }

    #[test]
    fn enter_in_focused_input_submits_through_the_host() {
        let (addr, log, stop, handle) = serve(
            "<html><head><title>Form</title></head><body>\
             <form action='/search'><input id='q' name='q' value='enter'>\
             <button type='submit' name='go' value='1'>Go</button></form>\
             <script>var ready = true;</script></body></html>"
                .to_string(),
        );
        let mut browser = Browser::new(settings());
        browser.navigate(&format!("http://{addr}/")).unwrap();
        let input = browser
            .page()
            .document
            .borrow()
            .get_element_by_id("q")
            .unwrap();
        {
            let page = &mut browser.tab_mut().page;
            let runtime = page.runtime.as_mut().expect("script runtime");
            let host = page.host.clone().expect("host state");
            browser_web_apis::dispatch_keyboard_event(
                runtime, &host, input, "keydown", "Enter", "Enter",
            );
        }
        let snapshot = browser.take_form_submission().unwrap().unwrap();
        let expected = format!("http://{addr}/search?q=enter&go=1");
        assert_eq!(snapshot.url.as_deref(), Some(expected.as_str()));
        assert_eq!(log.lock().unwrap().len(), 2);
        stop_server(stop, handle);
    }

    #[test]
    fn submitting_a_non_form_node_is_rejected() {
        let mut browser = Browser::new(settings());
        browser
            .load_html("<html><body><div id='x'></div></body></html>", None)
            .unwrap();
        let div = browser
            .page()
            .document
            .borrow()
            .get_element_by_id("x")
            .unwrap();
        assert!(matches!(
            browser.submit_form(div),
            Err(BrowserError::InvalidForm)
        ));
    }
}

#[cfg(test)]
mod module_script_tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    /// Minimal HTTP/1.1 server: maps a request path to (content type, body).
    fn serve<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> (&'static str, String) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut request = String::new();
                        let mut buf = [0_u8; 4096];
                        // A request may arrive in several TCP segments; wait for
                        // the full header block before choosing a response.
                        while !request.contains("\r\n\r\n") {
                            match stream.read(&mut buf) {
                                Ok(0) => break,
                                Ok(read) => {
                                    request.push_str(&String::from_utf8_lossy(&buf[..read]));
                                }
                                Err(ref err)
                                    if matches!(
                                        err.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::Interrupted
                                            | std::io::ErrorKind::TimedOut
                                    ) => {}
                                Err(_) => break,
                            }
                        }
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        let (mime, body) = handler(&path);
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(body.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (addr, stop, handle)
    }

    fn browser() -> Browser {
        let mut settings = Settings::headless();
        settings.enable_network = true;
        Browser::new(settings)
    }

    #[test]
    fn external_module_script_executes_and_sets_the_title() {
        let (addr, stop, handle) = serve(|path| match path {
            "/a.js" => (
                "text/javascript",
                "document.title = 'module ran';".to_string(),
            ),
            _ => (
                "text/html",
                "<!DOCTYPE html><html><head><title>before</title></head>\
                 <body><script type='module' src='/a.js'></script></body></html>"
                    .to_string(),
            ),
        });
        let mut browser = browser();
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.title, "module ran");
        assert!(!snapshot.is_truncated());
    }

    #[test]
    fn inline_module_script_executes() {
        let (addr, stop, handle) = serve(|_| {
            (
                "text/html",
                "<!DOCTYPE html><html><head><title>before</title></head>\
                 <body><script type='module'>document.title = 'inline module';</script></body></html>"
                    .to_string(),
            )
        });
        let mut browser = browser();
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.title, "inline module");
    }

    #[test]
    fn multiple_inline_modules_are_each_evaluated() {
        let (addr, stop, handle) = serve(|_| {
            (
                "text/html",
                "<!DOCTYPE html><html><head><title>start</title></head>\
                 <body>\
                 <script type='module'>document.title = document.title + '|one';</script>\
                 <script type='module'>document.title = document.title + '|two';</script>\
                 </body></html>"
                    .to_string(),
            )
        });
        let mut browser = browser();
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.title, "start|one|two");
    }

    #[test]
    fn module_static_import_is_resolved_relative_to_the_module_url() {
        let (addr, stop, handle) = serve(|path| match path {
            "/a.js" => (
                "text/javascript",
                "import { setTitle } from './b.js'; setTitle();".to_string(),
            ),
            "/b.js" => (
                "text/javascript",
                "export function setTitle() { document.title = 'import ran'; }".to_string(),
            ),
            _ => (
                "text/html",
                "<!DOCTYPE html><html><head><title>before</title></head>\
                 <body><script type='module' src='/a.js'></script></body></html>"
                    .to_string(),
            ),
        });
        let mut browser = browser();
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.title, "import ran");
    }

    #[test]
    fn classic_external_script_still_executes() {
        let (addr, stop, handle) = serve(|path| match path {
            "/classic.js" => (
                "text/javascript",
                "document.title = 'classic ran';".to_string(),
            ),
            _ => (
                "text/html",
                "<!DOCTYPE html><html><head><title>before</title></head>\
                 <body><script src='/classic.js'></script></body></html>"
                    .to_string(),
            ),
        });
        let mut browser = browser();
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(snapshot.title, "classic ran");
    }

    #[test]
    fn modules_are_deferred_behind_blocking_classics() {
        let (addr, stop, handle) = serve(|path| match path {
            "/m.js" => (
                "text/javascript",
                "document.title = document.title + '|module';".to_string(),
            ),
            _ => (
                "text/html",
                "<!DOCTYPE html><html><head><title>start</title></head>\
                 <body><script>document.title = document.title + '|classic';</script>\
                 <script type='module' src='/m.js'></script></body></html>"
                    .to_string(),
            ),
        });
        let mut browser = browser();
        let snapshot = browser.navigate(&format!("http://{addr}/")).unwrap();
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();

        // The blocking classic script runs first, the deferred module second.
        assert_eq!(snapshot.title, "start|classic|module");
    }
}
