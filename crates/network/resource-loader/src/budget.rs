//! Per-page resource budgets, duplicate-URL suppression and deadline support.
//!
//! These types are used by [`crate::ResourceLoader`] to bound the number and
//! size of subresources fetched while rendering a single page. The limits are
//! deliberately conservative: real pages often reference hundreds of images,
//! stylesheets and scripts, and fetching all of them synchronously stalls the
//! UI. Once a limit is reached the loader returns a
//! [`crate::LoadError::BudgetExceeded`] instead of performing more I/O.

use browser_url::Url;

/// The category a resource belongs to for budget accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A CSS stylesheet (`text/css`).
    Css,
    /// An image (`image/*`).
    Image,
    /// A JavaScript file (`text/javascript`, `application/javascript`, ...).
    Script,
    /// Anything else: documents, fonts, JSON, media, ...
    Other,
}

impl ResourceKind {
    /// Classifies a resource from its MIME type.
    ///
    /// The match is intentionally loose because servers frequently send
    /// vendor-prefixed or parameterised media types.
    pub fn from_mime(mime: &str) -> Self {
        let lower = mime.to_ascii_lowercase();
        if lower.contains("css") {
            Self::Css
        } else if lower.starts_with("image/") {
            Self::Image
        } else if lower.contains("javascript") || lower.contains("ecmascript") {
            Self::Script
        } else {
            Self::Other
        }
    }

    /// Guesses the kind of a URL before it is fetched, from its path extension.
    ///
    /// This lets the loader reject a resource *before* spending time on I/O.
    /// The actual kind is re-checked from the response MIME type after fetching.
    pub fn from_url(url: &Url) -> Self {
        Self::from_mime(crate::mime::from_path(&url.path()))
    }
}

/// Which specific limit was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetLimit {
    /// The total number of resources for the page was reached.
    Total,
    /// The CSS stylesheet limit was reached.
    Css,
    /// The image limit was reached.
    Image,
    /// The script limit was reached.
    Script,
    /// The cumulative transferred-byte limit was reached.
    Bytes,
}

impl std::fmt::Display for BudgetLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Total => "total resources",
            Self::Css => "stylesheets",
            Self::Image => "images",
            Self::Script => "scripts",
            Self::Bytes => "transferred bytes",
        };
        f.write_str(name)
    }
}

/// Limits applied to the resources fetched for one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceBudget {
    /// Maximum number of resources of any kind (default 32).
    pub max_total: usize,
    /// Maximum number of CSS stylesheets (default 8).
    pub max_css: usize,
    /// Maximum number of images (default 8).
    pub max_images: usize,
    /// Maximum number of scripts (default 8).
    pub max_scripts: usize,
    /// Maximum cumulative body size, in bytes (default 8 MiB).
    pub max_bytes: u64,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            max_total: 96,
            max_css: 24,
            max_images: 48,
            max_scripts: 24,
            max_bytes: 32 * 1024 * 1024,
        }
    }
}

impl ResourceBudget {
    /// Checks whether one more resource of `kind` may be fetched.
    pub fn check(self, stats: &PageStats, kind: ResourceKind) -> Result<(), BudgetLimit> {
        if stats.resources >= self.max_total {
            return Err(BudgetLimit::Total);
        }
        match kind {
            ResourceKind::Css if stats.css >= self.max_css => Err(BudgetLimit::Css),
            ResourceKind::Image if stats.images >= self.max_images => Err(BudgetLimit::Image),
            ResourceKind::Script if stats.scripts >= self.max_scripts => Err(BudgetLimit::Script),
            _ => Ok(()),
        }
    }

    /// Checks whether `additional` more bytes fit in the budget.
    pub fn check_bytes(self, used: u64, additional: u64) -> Result<(), BudgetLimit> {
        if used.saturating_add(additional) > self.max_bytes {
            Err(BudgetLimit::Bytes)
        } else {
            Ok(())
        }
    }
}

/// Live accounting for the current page, also exposed as public statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageStats {
    /// Total resources successfully fetched on this page.
    pub resources: usize,
    /// CSS stylesheets fetched on this page.
    pub css: usize,
    /// Images fetched on this page.
    pub images: usize,
    /// Scripts fetched on this page.
    pub scripts: usize,
    /// Cumulative body bytes fetched on this page.
    pub bytes: u64,
    /// Requests served from the duplicate-URL cache instead of the network.
    pub dedup_hits: u64,
    /// Requests rejected because their per-resource deadline expired.
    pub timeouts: u64,
}

impl PageStats {
    /// Records a successfully fetched resource.
    pub(crate) fn record(&mut self, kind: ResourceKind, bytes: u64) {
        self.resources += 1;
        self.bytes += bytes;
        match kind {
            ResourceKind::Css => self.css += 1,
            ResourceKind::Image => self.images += 1,
            ResourceKind::Script => self.scripts += 1,
            ResourceKind::Other => {}
        }
    }

    /// Clears all counters (used when a new page starts).
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Key used for duplicate-URL suppression.
///
/// Fragments are stripped because `#section` never changes the resource that
/// is fetched, but queries are kept because they may select different content.
pub fn dedup_key(url: &Url) -> String {
    let raw = url.as_str();
    match raw.find('#') {
        Some(index) => raw[..index].to_string(),
        None => raw.to_string(),
    }
}

/// True when a URL looks like a top-level document rather than a subresource.
///
/// Loading such a URL resets the per-page accounting so that navigating (or
/// reloading) starts with a fresh budget. Embedders that load documents through
/// other means can call [`crate::ResourceLoader::reset_page`] explicitly.
pub fn looks_like_document(url: &Url) -> bool {
    let path = url.path();
    if path.len() <= 1 {
        return true;
    }
    if matches!(
        crate::mime::from_path(&path),
        "text/html" | "application/xml" | "text/xml"
    ) {
        return true;
    }
    // Extensionless paths are typically top-level document routes (SPAs), e.g.
    // `https://example.com/watch`. Subresources almost always carry an
    // extension, and a false positive here cannot cause a spurious reset
    // because `is_document` checks the actual MIME type after fetching.
    let last_segment = path.rsplit('/').next().unwrap_or("");
    !last_segment.contains('.')
}

/// True when a fetched resource is a document (as opposed to a subresource).
pub fn is_document(resource: &crate::Resource) -> bool {
    let mime = resource.mime.to_ascii_lowercase();
    mime.contains("html") || mime.contains("xml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    #[test]
    fn classifies_kinds_from_mime_and_url() {
        assert_eq!(ResourceKind::from_mime("text/css"), ResourceKind::Css);
        assert_eq!(ResourceKind::from_mime("image/WebP"), ResourceKind::Image);
        assert_eq!(
            ResourceKind::from_mime("application/javascript; charset=utf-8"),
            ResourceKind::Script
        );
        assert_eq!(ResourceKind::from_mime("font/woff2"), ResourceKind::Other);
        assert_eq!(
            ResourceKind::from_url(&url("https://a.test/s/site.css?v=2")),
            ResourceKind::Css
        );
        assert_eq!(
            ResourceKind::from_url(&url("https://a.test/app.js")),
            ResourceKind::Script
        );
        assert_eq!(
            ResourceKind::from_url(&url("https://a.test/pic.PNG")),
            ResourceKind::Image
        );
    }

    #[test]
    fn enforces_every_budget_limit() {
        let budget = ResourceBudget {
            max_total: 3,
            max_css: 1,
            max_images: 1,
            max_scripts: 1,
            max_bytes: 11,
        };
        let mut stats = PageStats::default();
        assert_eq!(budget.check(&stats, ResourceKind::Css), Ok(()));
        stats.record(ResourceKind::Css, 4);
        assert_eq!(
            budget.check(&stats, ResourceKind::Css),
            Err(BudgetLimit::Css)
        );
        assert_eq!(budget.check(&stats, ResourceKind::Image), Ok(()));
        stats.record(ResourceKind::Image, 4);
        assert_eq!(budget.check(&stats, ResourceKind::Script), Ok(()));
        stats.record(ResourceKind::Script, 2);
        assert_eq!(
            budget.check(&stats, ResourceKind::Other),
            Err(BudgetLimit::Total)
        );
        assert_eq!(budget.check_bytes(stats.bytes, 1), Ok(()));
        assert_eq!(budget.check_bytes(stats.bytes, 2), Err(BudgetLimit::Bytes));
    }

    #[test]
    fn dedup_key_ignores_fragments_but_keeps_queries() {
        assert_eq!(
            dedup_key(&url("https://a.test/a.css#one")),
            "https://a.test/a.css"
        );
        assert_ne!(
            dedup_key(&url("https://a.test/a.css?v=1")),
            dedup_key(&url("https://a.test/a.css?v=2"))
        );
    }

    #[test]
    fn recognises_documents() {
        assert!(looks_like_document(&url("https://a.test/")));
        assert!(looks_like_document(&url("https://a.test/index.html")));
        assert!(looks_like_document(&url("https://a.test/watch?v=1")));
        assert!(!looks_like_document(&url("https://a.test/a.css")));
        assert!(!looks_like_document(&url("https://a.test/pic.png")));
        assert!(!looks_like_document(&url("https://a.test/data.json")));
    }
}
