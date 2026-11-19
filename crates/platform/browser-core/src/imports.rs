//! Browser-side bookkeeping for real `@import` fetching.
//!
//! The style engine already parses `@import` rules and applies depth/duplicate
//! guards through [`StyleEngine::add_css_collect_imports`]. This module drives
//! that API for an entire sheet chain while adding the limits that only the
//! *fetching* side can enforce: a cap on how many imported sheets are accepted,
//! a cap on their cumulative byte size, and a set of already-fetched absolute
//! URLs so cycles (`a.css -> b.css -> a.css`) can never trigger a refetch.
//!
//! The browser owns the network I/O and the navigation deadline; this type is
//! deliberately IO-free so it can be unit-tested on its own.

use std::collections::HashSet;

use browser_css::Origin;
use browser_style::{ImportRequest, StyleEngine};

/// Maximum number of `@import`ed stylesheets accepted during one load.
///
/// Mirrors the resource loader's default CSS budget but applies specifically to
/// the import chain so a single page cannot pull in an unbounded cascade.
pub const MAX_IMPORT_SHEETS: usize = 8;

/// Maximum cumulative bytes of `@import`ed CSS accepted during one load.
pub const MAX_IMPORT_BYTES: usize = 256 * 1024;

/// Drives [`StyleEngine::add_css_collect_imports`] across a chain of sheets.
#[derive(Debug)]
pub struct ImportCollector {
    /// One engine for the whole chain so its depth/duplicate guards persist.
    engine: StyleEngine,
    /// Absolute URLs already fetched or seeded, for cycle/duplicate safety.
    seen_urls: HashSet<String>,
    /// Number of imported sheets reserved so far.
    sheets: usize,
    /// Cumulative bytes of accepted imported CSS.
    bytes: usize,
}

impl Default for ImportCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportCollector {
    /// Creates an empty collector with a fresh style engine.
    pub fn new() -> Self {
        Self {
            engine: StyleEngine::new(browser_style::Viewport::default()),
            seen_urls: HashSet::new(),
            sheets: 0,
            bytes: 0,
        }
    }

    /// Records a URL that has already been fetched through another path (an
    /// inline sheet or a `<link>`), so a later `@import` of it is ignored.
    pub fn seed(&mut self, absolute_url: &str) {
        self.seen_urls.insert(absolute_url.to_string());
    }

    /// Registers a sheet's CSS with the engine and returns its import requests.
    ///
    /// `url` is the absolute URL the sheet was fetched from (`None` for inline
    /// styles) and `depth` is the number of `@import` hops taken to reach it.
    pub fn add_sheet(&mut self, css: &str, url: Option<&str>, depth: usize) -> Vec<ImportRequest> {
        self.engine
            .add_css_collect_imports(css, url, Origin::Author, depth)
    }

    /// Reserves one imported sheet identified by its absolute URL.
    ///
    /// Returns `false` when the URL was already fetched (a duplicate or cycle)
    /// or when the sheet cap has been reached. On success the URL is remembered
    /// so any later request for it is rejected.
    pub fn reserve(&mut self, absolute_url: &str) -> bool {
        if self.sheets >= MAX_IMPORT_SHEETS || !self.seen_urls.insert(absolute_url.to_string()) {
            return false;
        }
        self.sheets += 1;
        true
    }

    /// Accepts the bytes of a fetched sheet, enforcing the cumulative byte cap.
    ///
    /// Returns `false` (and accounts for nothing) when adding `len` bytes would
    /// exceed [`MAX_IMPORT_BYTES`].
    pub fn accept_bytes(&mut self, len: usize) -> bool {
        let next = self.bytes.saturating_add(len);
        if next > MAX_IMPORT_BYTES {
            return false;
        }
        self.bytes = next;
        true
    }

    /// Number of imported sheets reserved so far (test introspection).
    #[cfg(test)]
    pub fn sheet_count(&self) -> usize {
        self.sheets
    }

    /// Cumulative bytes of imported CSS accepted so far (test introspection).
    #[cfg(test)]
    pub fn byte_count(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_import_requests_with_depth() {
        let mut collector = ImportCollector::new();
        let requests =
            collector.add_sheet("@import \"a.css\"; @import url(b.css) screen;", None, 0);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url, "a.css");
        assert_eq!(requests[0].depth, 1);
        assert_eq!(requests[1].url, "b.css");
        assert_eq!(requests[1].media.as_deref(), Some("screen"));
    }

    #[test]
    fn reserve_rejects_duplicates_and_enforces_sheet_cap() {
        let mut collector = ImportCollector::new();
        assert!(collector.reserve("http://x/a.css"));
        assert!(!collector.reserve("http://x/a.css")); // duplicate / cycle
        for n in 1..MAX_IMPORT_SHEETS {
            assert!(collector.reserve(&format!("http://x/{n}.css")));
        }
        assert_eq!(collector.sheet_count(), MAX_IMPORT_SHEETS);
        assert!(!collector.reserve("http://x/overflow.css"));
    }

    #[test]
    fn accept_bytes_enforces_byte_cap() {
        let mut collector = ImportCollector::new();
        assert!(collector.accept_bytes(MAX_IMPORT_BYTES));
        assert_eq!(collector.byte_count(), MAX_IMPORT_BYTES);
        assert!(!collector.accept_bytes(1));
        assert_eq!(collector.byte_count(), MAX_IMPORT_BYTES);
    }
}
