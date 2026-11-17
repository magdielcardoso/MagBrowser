//! The browser facade: navigation, tabs, rendering and interaction.

use std::time::Duration;

use browser_dom::escape_text;
use browser_http::{Client, ClientConfig, HttpError};
use browser_navigation::NavigationAction;
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
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(err) => write!(f, "invalid URL: {err}"),
            Self::Http(err) => write!(f, "network error: {err}"),
            Self::Io(err) => write!(f, "file error: {err}"),
            Self::NetworkDisabled => f.write_str("network access is disabled"),
            Self::NoPage => f.write_str("no page is loaded"),
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
}

/// A browser instance with tabs.
pub struct Browser {
    settings: Settings,
    loader: ResourceLoader,
    tabs: TabSet,
    console: Vec<String>,
    csp: CspList,
}

impl Browser {
    /// Creates a browser with one empty tab.
    pub fn new(settings: Settings) -> Self {
        let config = ClientConfig {
            user_agent: settings.user_agent.clone(),
            timeout: Duration::from_secs(settings.timeout_seconds.max(1)),
            max_body_bytes: settings.max_body_bytes,
            ..ClientConfig::default()
        };
        let size = settings.viewport();
        Self {
            settings,
            loader: ResourceLoader::new(config),
            tabs: TabSet::new(size),
            console: Vec::new(),
            csp: CspList::new(),
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

    /// Loads HTML directly into the active tab (tests, `view-source`, fixtures).
    pub fn load_html(&mut self, html: &str, url: Option<&str>) -> Result<Snapshot, BrowserError> {
        let parsed = match url {
            Some(raw) => Some(Url::parse(raw)?),
            None => None,
        };
        let tab = self.tabs.active_mut();
        tab.page.load_html(html, parsed.clone());
        if let Some(url) = parsed {
            tab.record_navigation(NavigationAction::Push, Some(url));
        }
        self.refresh_policy();
        self.load_linked_stylesheets();
        self.load_images();
        self.execute_scripts();
        Ok(self.snapshot())
    }

    /// Navigates the active tab to `input` (a URL or a bare domain).
    pub fn navigate(&mut self, input: &str) -> Result<Snapshot, BrowserError> {
        let url = normalize_url(input)?;
        self.navigate_url(url, NavigationAction::Push)
    }

    /// Navigates to an already parsed URL.
    pub fn navigate_url(
        &mut self,
        url: Url,
        action: NavigationAction,
    ) -> Result<Snapshot, BrowserError> {
        let html = self.fetch_document(&url)?;
        let viewport = self.settings.viewport();
        let tab = self.tabs.active_mut();
        tab.page.resize(viewport);
        tab.page.load_html(&html, Some(url.clone()));
        tab.record_navigation(action, Some(url.clone()));
        self.refresh_policy();
        self.fetch_stylesheets_for(&url);
        self.load_images();
        self.execute_scripts();
        Ok(self.snapshot())
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

    /// Scrolls the container under a point by `delta` pixels.
    pub fn scroll_at(&mut self, x: f32, y: f32, delta: f32) -> bool {
        self.tab_mut().page.scroll_at(x, y, 0.0, delta)
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
        }
    }

    fn fetch_document(&mut self, url: &Url) -> Result<String, BrowserError> {
        match url.scheme() {
            "http" | "https" => {
                if !self.settings.enable_network {
                    return Err(BrowserError::NetworkDisabled);
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
        if links.is_empty() || !self.settings.enable_network {
            return;
        }
        let origin = self.page_origin();
        let mut sheets = Vec::new();
        for url in links {
            if !self.csp.is_empty() && !self.csp.allows("style-src", &url, &origin) {
                self.console.push(format!("[csp] blocked stylesheet {url}"));
                continue;
            }
            if let Ok(resource) = self.loader.load(&url) {
                sheets.push(resource.text());
            }
        }
        if sheets.is_empty() {
            return;
        }
        let tab = self.tabs.active_mut();
        for css in sheets {
            tab.page.add_author_css(&css);
        }
    }

    /// Fetches and decodes `<img src>` resources of the active page.
    ///
    /// Images are checked against `img-src` when a CSP is active and skipped when
    /// they exceed the configured body limit or cannot be decoded. Failures are
    /// reported on the console and the placeholder continues to be painted.
    pub fn load_images(&mut self) {
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
        self.tab_mut().page.recompute();
    }

    /// Collects the page's scripts in document order, fetching `src` scripts.
    ///
    /// Inline scripts are filtered by the active CSP; external ones are checked
    /// against `script-src` before any request is made.
    fn collect_scripts(&mut self) -> Vec<String> {
        let origin = self.page_origin();
        let sources: Vec<(Option<String>, String)> = {
            let page = self.page();
            let document = page.document.borrow();
            let mut out = Vec::new();
            for node in document.get_elements_by_tag_name("script") {
                let kind = document
                    .attr(node, "type")
                    .unwrap_or("text/javascript")
                    .to_ascii_lowercase();
                if !kind.contains("javascript") && !kind.is_empty() {
                    continue;
                }
                let src = document.attr(node, "src").map(ToString::to_string);
                let inline = document.text_content(node);
                out.push((src, inline));
            }
            out
        };

        let base = self.page().url.clone();
        let mut scripts = Vec::new();
        for (src, inline) in sources {
            match src {
                Some(src) => {
                    let Some(base) = &base else { continue };
                    let Ok(url) = base.join(&src) else { continue };
                    if !self.csp.is_empty() && !self.csp.allows("script-src", &url, &origin) {
                        self.console.push(format!("[csp] blocked script {url}"));
                        continue;
                    }
                    match self.loader.load(&url) {
                        Ok(resource) => scripts.push(resource.text()),
                        Err(error) => self
                            .console
                            .push(format!("[script] failed to load {url}: {error}")),
                    }
                }
                None => {
                    if !self.inline_scripts_allowed() {
                        self.console.push("[csp] blocked inline script".to_string());
                        continue;
                    }
                    scripts.push(inline);
                }
            }
        }
        scripts
    }

    /// Executes page scripts (JavaScript) and applies DOM mutations.
    ///
    /// Scripts may request navigation through `location.href`; up to eight
    /// chained navigations are followed, which is enough for router-style pages
    /// without allowing an infinite redirect loop inside one call.
    ///
    /// Returns the number of scripts that ran.
    pub fn execute_scripts(&mut self) -> usize {
        if !self.settings.enable_scripting {
            return 0;
        }
        let scripts = self.collect_scripts();
        if scripts.is_empty() {
            return 0;
        }
        let (count, console) = {
            let tab = self.tabs.active_mut();
            crate::script::execute_scripts(&mut tab.page, &scripts)
        };
        self.console.extend(console);
        if count > 0 {
            self.tab_mut().page.recompute();
        }

        let mut hops = 0;
        while hops < 8 {
            let Some(request) = self.tab_mut().page.take_pending_navigation() else {
                break;
            };
            if self.navigate(&request).is_err() {
                break;
            }
            hops += 1;
        }
        count
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
}
