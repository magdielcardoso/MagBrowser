//! Loading resources over HTTP(S) and from `file:` URLs.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use browser_http::{Client, ClientConfig, HttpError, Response};
use browser_url::{Url, UrlError};

use crate::budget::{BudgetLimit, PageStats, ResourceBudget, ResourceKind};
use crate::fetch::Resource;
use crate::mime;

/// Errors raised while loading a resource.
#[derive(Debug)]
pub enum LoadError {
    /// Invalid or unsupported URL.
    Url(UrlError),
    /// Network failure.
    Http(HttpError),
    /// The scheme cannot be loaded.
    UnsupportedScheme(String),
    /// Local file error.
    Io(String),
    /// HTTP status is not successful.
    Status(u16),
    /// The per-page resource budget was exhausted; the resource was not fetched.
    BudgetExceeded(BudgetLimit),
    /// The resource did not load before its per-resource deadline expired.
    Timeout,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(err) => write!(f, "invalid URL: {err}"),
            Self::Http(err) => write!(f, "network error: {err}"),
            Self::UnsupportedScheme(scheme) => write!(f, "unsupported scheme: {scheme}"),
            Self::Io(err) => write!(f, "file error: {err}"),
            Self::Status(status) => write!(f, "HTTP {status}"),
            Self::BudgetExceeded(limit) => {
                write!(f, "per-page resource budget exceeded ({limit})")
            }
            Self::Timeout => f.write_str("resource load timed out"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<UrlError> for LoadError {
    fn from(value: UrlError) -> Self {
        Self::Url(value)
    }
}

impl From<HttpError> for LoadError {
    fn from(value: HttpError) -> Self {
        Self::Http(value)
    }
}

/// Mutable state for the current page: statistics, the duplicate-URL cache and
/// the URL of the last document that triggered an automatic reset.
#[derive(Default)]
struct PageState {
    stats: PageStats,
    cache: HashMap<String, Resource>,
    last_document: Option<String>,
}

impl PageState {
    fn reset(&mut self) {
        self.stats.reset();
        self.cache.clear();
    }
}

/// Loads subresources (stylesheets, scripts, images) for a page.
///
/// Every call to [`Self::load`] is subject to a per-page [`ResourceBudget`], a
/// per-resource deadline derived from the configured timeout, and
/// duplicate-URL suppression. Navigating to a new document resets the budget
/// automatically; embedders loading documents by other means can call
/// [`Self::reset_page`].
pub struct ResourceLoader {
    client: Client,
    allowed_schemes: Vec<String>,
    budget: ResourceBudget,
    page: PageState,
}

impl Default for ResourceLoader {
    fn default() -> Self {
        Self::new(ClientConfig::default())
    }
}

impl ResourceLoader {
    /// Creates a loader with the given HTTP configuration and the default budget.
    pub fn new(config: ClientConfig) -> Self {
        Self {
            client: Client::new(config),
            allowed_schemes: vec!["http".into(), "https".into(), "file".into()],
            budget: ResourceBudget::default(),
            page: PageState::default(),
        }
    }

    /// Restricts the schemes that may be loaded (for example `["https"]`).
    pub fn set_allowed_schemes(&mut self, schemes: &[&str]) {
        self.allowed_schemes = schemes.iter().map(|s| (*s).to_string()).collect();
    }

    /// The underlying HTTP client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The underlying HTTP client, mutably.
    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.client
    }

    /// The per-page resource budget currently in force.
    pub fn budget(&self) -> &ResourceBudget {
        &self.budget
    }

    /// Replaces the per-page resource budget.
    pub fn set_budget(&mut self, budget: ResourceBudget) {
        self.budget = budget;
    }

    /// Statistics for the current page (counts, bytes, dedup hits, timeouts).
    pub fn stats(&self) -> &PageStats {
        &self.page.stats
    }

    /// The configured per-resource timeout.
    pub fn timeout(&self) -> Duration {
        self.client.config().timeout
    }

    /// Clears the budget accounting and duplicate-URL cache for a new page.
    pub fn reset_page(&mut self) {
        self.page.reset();
        self.page.last_document = None;
    }

    /// Sets the connect/read timeout (also used as the per-resource deadline).
    pub fn set_timeout(&mut self, timeout: Duration) {
        let mut config = self.client.config().clone();
        config.timeout = timeout;
        self.client = Client::new(config);
    }

    /// Loads a URL into a [`Resource`].
    ///
    /// The effective deadline is `now + self.timeout()`. A repeated URL is
    /// served from the per-page cache without any I/O; when a budget limit is
    /// reached a [`LoadError::BudgetExceeded`] is returned instead of fetching.
    pub fn load(&mut self, url: &Url) -> Result<Resource, LoadError> {
        let deadline = Instant::now().checked_add(self.timeout());
        self.load_inner(url, deadline)
    }

    /// Loads a URL with an explicit deadline.
    ///
    /// This is useful for embedders that want a tighter, shared deadline than
    /// the loader's configured timeout. An already-expired deadline fails
    /// immediately with [`LoadError::Timeout`] and performs no I/O.
    pub fn load_with_deadline(
        &mut self,
        url: &Url,
        deadline: Instant,
    ) -> Result<Resource, LoadError> {
        self.load_inner(url, Some(deadline))
    }

    /// Loads a URL and decodes it as text.
    pub fn load_text(&mut self, url: &Url) -> Result<String, LoadError> {
        Ok(self.load(url)?.text())
    }

    fn load_inner(&mut self, url: &Url, deadline: Option<Instant>) -> Result<Resource, LoadError> {
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            self.page.stats.timeouts += 1;
            return Err(LoadError::Timeout);
        }

        // 1. Duplicate suppression: return the previously fetched bytes without
        //    touching the network or the budget again.
        let key = crate::budget::dedup_key(url);
        if let Some(cached) = self.page.cache.get(&key).cloned() {
            self.page.stats.dedup_hits += 1;
            return Ok(cached);
        }

        // A document URL starts a new page. Skip the pre-check so a fresh
        // navigation is never blocked by the previous page's exhausted budget.
        let starts_page = crate::budget::looks_like_document(url);
        if !starts_page {
            self.budget
                .check(&self.page.stats, ResourceKind::from_url(url))
                .map_err(LoadError::BudgetExceeded)?;
        }

        // 2. Fetch the resource. The HTTP client already applies the configured
        //    timeout to connect/read/write operations; the deadline below adds
        //    an overall bound across the whole request.
        let resource = self.fetch(url)?;

        if deadline.is_some_and(|limit| Instant::now() > limit) {
            self.page.stats.timeouts += 1;
            return Err(LoadError::Timeout);
        }

        let kind = ResourceKind::from_mime(&resource.mime);

        // A successfully loaded document resets the accounting for its page.
        if starts_page && crate::budget::is_document(&resource) {
            self.page.reset();
            self.page.last_document = Some(key.clone());
        }

        // 3. Cumulative byte budget.
        self.budget
            .check_bytes(self.page.stats.bytes, resource.bytes.len() as u64)
            .map_err(LoadError::BudgetExceeded)?;

        // 4. Account for the fetch and remember it for deduplication.
        self.page.stats.record(kind, resource.bytes.len() as u64);
        self.page.cache.insert(key, resource.clone());
        Ok(resource)
    }

    /// Performs the actual scheme-specific I/O (no budget or dedup logic).
    fn fetch(&mut self, url: &Url) -> Result<Resource, LoadError> {
        if !self
            .allowed_schemes
            .iter()
            .any(|scheme| scheme == url.scheme())
        {
            return Err(LoadError::UnsupportedScheme(url.scheme().to_string()));
        }
        match url.scheme() {
            "http" | "https" => {
                let response: Response = self.client.get(url.clone())?;
                if !response.is_success() {
                    return Err(LoadError::Status(response.status));
                }
                let mime = response
                    .media_type()
                    .unwrap_or_else(|| mime::from_path(&url.path()).to_string());
                let charset = response.charset();
                let status = response.status;
                let mut resource = Resource::new(url.clone(), status, &mime, response.body);
                resource.charset = charset;
                Ok(resource)
            }
            "file" => {
                let path = format!("/{}", url.path_segments().join("/"));
                let bytes =
                    std::fs::read(&path).map_err(|err| LoadError::Io(format!("{path}: {err}")))?;
                let mime = mime::sniff_image(&bytes)
                    .map_or_else(|| mime::from_path(&path).to_string(), str::to_string);
                Ok(Resource::new(url.clone(), 200, &mime, bytes))
            }
            other => Err(LoadError::UnsupportedScheme(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("mag-loader-{}-{}-{}", name, std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &std::path::Path, name: &str, body: &[u8]) -> Url {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        Url::parse(&format!("file://{}", path.display())).unwrap()
    }

    fn loader(budget: ResourceBudget) -> ResourceLoader {
        let mut loader = ResourceLoader::default();
        loader.set_budget(budget);
        loader
    }

    #[test]
    fn loads_local_files() {
        let dir = temp_dir("basic");
        let path = dir.join("style.css");
        std::fs::write(&path, "body { color: red }").unwrap();
        let mut loader = ResourceLoader::default();
        let url = Url::parse(&format!("file://{}", path.display())).unwrap();
        let resource = loader.load(&url).unwrap();
        assert_eq!(resource.mime, "text/css");
        assert!(resource.is_css());
        assert_eq!(resource.text(), "body { color: red }");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_disallowed_schemes() {
        let mut loader = ResourceLoader::default();
        loader.set_allowed_schemes(&["https"]);
        let url = Url::parse("http://example.com/").unwrap();
        assert!(matches!(
            loader.load(&url),
            Err(LoadError::UnsupportedScheme(_))
        ));
    }

    #[test]
    fn enforces_total_and_per_kind_budgets() {
        let dir = temp_dir("counts");
        let budget = ResourceBudget {
            max_total: 5,
            max_css: 2,
            max_images: 1,
            max_scripts: 1,
            max_bytes: u64::MAX,
        };
        let mut loader = loader(budget);
        let css1 = write(&dir, "one.css", b"a{}");
        let css2 = write(&dir, "two.css", b"b{}");
        let css3 = write(&dir, "three.css", b"c{}");
        let img1 = write(&dir, "one.png", b"\x89PNG");
        let img2 = write(&dir, "two.png", b"\x89PNG");
        let js1 = write(&dir, "one.js", b"1");
        let js2 = write(&dir, "two.js", b"2");
        let txt1 = write(&dir, "one.txt", b"t");
        let txt2 = write(&dir, "two.txt", b"t");

        assert!(loader.load(&css1).is_ok());
        assert!(loader.load(&css2).is_ok());
        assert!(matches!(
            loader.load(&css3),
            Err(LoadError::BudgetExceeded(BudgetLimit::Css))
        ));
        assert!(loader.load(&img1).is_ok());
        assert!(matches!(
            loader.load(&img2),
            Err(LoadError::BudgetExceeded(BudgetLimit::Image))
        ));
        assert!(loader.load(&js1).is_ok());
        assert!(matches!(
            loader.load(&js2),
            Err(LoadError::BudgetExceeded(BudgetLimit::Script))
        ));
        assert!(loader.load(&txt1).is_ok());
        assert!(matches!(
            loader.load(&txt2),
            Err(LoadError::BudgetExceeded(BudgetLimit::Total))
        ));

        assert_eq!(loader.stats().resources, 5);
        assert_eq!(loader.stats().css, 2);
        assert_eq!(loader.stats().images, 1);
        assert_eq!(loader.stats().scripts, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enforces_cumulative_byte_budget() {
        let dir = temp_dir("bytes");
        let budget = ResourceBudget {
            max_bytes: 10,
            ..ResourceBudget::default()
        };
        let mut loader = loader(budget);
        let a = write(&dir, "a.txt", b"123456");
        let b = write(&dir, "b.txt", b"123456");
        let c = write(&dir, "c.txt", b"1234");
        let d = write(&dir, "d.txt", b"1");

        assert!(loader.load(&a).is_ok());
        assert!(matches!(
            loader.load(&b),
            Err(LoadError::BudgetExceeded(BudgetLimit::Bytes))
        ));
        assert!(loader.load(&c).is_ok());
        assert_eq!(loader.stats().bytes, 10);
        assert!(matches!(
            loader.load(&d),
            Err(LoadError::BudgetExceeded(BudgetLimit::Bytes))
        ));
        assert_eq!(loader.stats().resources, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_urls_are_served_from_cache() {
        let dir = temp_dir("dedup");
        let mut loader = ResourceLoader::default();
        let url = write(&dir, "sheet.css", b"first");

        let first = loader.load(&url).unwrap();
        assert_eq!(first.text(), "first");
        // Change the file *after* the first fetch: the second load must still
        // return the cached bytes, proving no I/O was performed.
        std::fs::write(url.path(), b"second, much bigger").unwrap();

        let second = loader.load(&url).unwrap();
        assert_eq!(second.text(), "first");
        let with_fragment = Url::parse(&format!("{url}#section")).unwrap();
        assert_eq!(loader.load(&with_fragment).unwrap().text(), "first");

        assert_eq!(loader.stats().dedup_hits, 2);
        assert_eq!(loader.stats().resources, 1);
        assert_eq!(loader.stats().bytes, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_deadline_fails_without_fetching() {
        let dir = temp_dir("deadline");
        let mut loader = ResourceLoader::default();
        let url = write(&dir, "page.css", b"a{}");
        let past = Instant::now() - Duration::from_secs(1);

        assert!(matches!(
            loader.load_with_deadline(&url, past),
            Err(LoadError::Timeout)
        ));
        assert_eq!(loader.stats().resources, 0);
        assert_eq!(loader.stats().timeouts, 1);

        let future = Instant::now() + Duration::from_secs(30);
        assert!(loader.load_with_deadline(&url, future).is_ok());
        assert_eq!(loader.stats().resources, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loading_a_document_resets_the_page_budget() {
        let dir = temp_dir("reset");
        let mut loader = loader(ResourceBudget {
            max_total: 2,
            ..ResourceBudget::default()
        });
        let page1 = write(&dir, "one.html", b"<html></html>");
        let page2 = write(&dir, "two.html", b"<html></html>");
        let css = write(&dir, "site.css", b"a{}");

        assert!(loader.load(&page1).is_ok()); // 1 resource, new page
        assert!(loader.load(&css).is_ok()); // 2 resources
        assert!(matches!(
            loader.load(&write(&dir, "extra.css", b"b{}")),
            Err(LoadError::BudgetExceeded(BudgetLimit::Total))
        ));

        // Navigating to a new document starts a fresh, full budget.
        assert!(loader.load(&page2).is_ok());
        assert_eq!(loader.stats().resources, 1);
        assert!(loader.load(&css).is_ok());
        assert_eq!(loader.stats().resources, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_reset_page_clears_stats_and_cache() {
        let dir = temp_dir("explicit-reset");
        let mut loader = ResourceLoader::default();
        let url = write(&dir, "site.css", b"a{}");

        assert!(loader.load(&url).is_ok());
        assert_eq!(loader.load(&url).unwrap().text(), "a{}");
        assert_eq!(loader.stats().dedup_hits, 1);

        loader.reset_page();
        assert_eq!(loader.stats(), &PageStats::default());
        assert!(loader.load(&url).is_ok());
        assert_eq!(loader.stats().resources, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_error_is_reported_through_display() {
        let error = LoadError::BudgetExceeded(BudgetLimit::Image);
        assert_eq!(
            error.to_string(),
            "per-page resource budget exceeded (images)"
        );
        assert_eq!(LoadError::Timeout.to_string(), "resource load timed out");
    }
}
