//! Browsing history store with a bounded number of visits.
//!
//! [`History`] keeps [`Visit`] records in insertion order. [`History::recent`]
//! and [`History::search`] return results most-recent-first. Timestamps are
//! supplied by the caller as Unix milliseconds; the store itself never reads
//! the clock, which keeps it deterministic and headless-friendly.

/// Maximum number of visits kept by [`History::new`].
pub const DEFAULT_HISTORY_CAP: usize = 10_000;

/// A single page visit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    /// Visited URL.
    pub url: String,
    /// Page title at the time of the visit.
    pub title: String,
    /// Unix timestamp in milliseconds.
    pub timestamp: u64,
}

/// A bounded, in-memory browsing history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    visits: Vec<Visit>,
    cap: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::with_cap(DEFAULT_HISTORY_CAP)
    }
}

impl History {
    /// Creates an empty history with the default cap of 10,000 visits.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty history that keeps at most `cap` visits.
    ///
    /// A `cap` of `0` means nothing is ever retained.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            visits: Vec::new(),
            cap,
        }
    }

    /// Maximum number of visits retained.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Records a visit, evicting the oldest visits when the cap is exceeded.
    pub fn add(&mut self, url: impl Into<String>, title: impl Into<String>, timestamp: u64) {
        self.visits.push(Visit {
            url: url.into(),
            title: title.into(),
            timestamp,
        });
        if self.visits.len() > self.cap {
            let excess = self.visits.len() - self.cap;
            self.visits.drain(..excess);
        }
    }

    /// Returns up to `limit` most recently added visits, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Visit> {
        self.visits.iter().rev().take(limit).cloned().collect()
    }

    /// Searches visit URLs and titles, case-insensitively, newest first.
    ///
    /// An empty query matches every visit.
    pub fn search(&self, query: &str) -> Vec<Visit> {
        let needle = query.to_lowercase();
        self.visits
            .iter()
            .rev()
            .filter(|visit| {
                visit.url.to_lowercase().contains(&needle)
                    || visit.title.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    /// Removes every visit.
    pub fn clear(&mut self) {
        self.visits.clear();
    }

    /// All retained visits, in insertion order.
    pub fn visits(&self) -> &[Visit] {
        &self.visits
    }

    /// Number of retained visits.
    pub fn len(&self) -> usize {
        self.visits.len()
    }

    /// Whether the history is empty.
    pub fn is_empty(&self) -> bool {
        self.visits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_recent_are_most_recent_first() {
        let mut history = History::new();
        history.add("https://a", "A", 100);
        history.add("https://b", "B", 200);
        history.add("https://c", "C", 300);
        assert_eq!(history.len(), 3);
        assert_eq!(history.visits()[0].timestamp, 100);

        let recent = history.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].url, "https://c");
        assert_eq!(recent[1].title, "B");
        assert!(history.recent(0).is_empty());
        assert_eq!(history.recent(10).len(), 3);
    }

    #[test]
    fn search_matches_url_and_title_case_insensitively() {
        let mut history = History::new();
        history.add("https://example.com/Rust", "Rust News", 1);
        history.add("https://other.org", "C++ Weekly", 2);

        let by_url = history.search("EXAMPLE");
        assert_eq!(by_url.len(), 1);
        assert_eq!(by_url[0].url, "https://example.com/Rust");

        let by_title = history.search("c++");
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].title, "C++ Weekly");

        assert!(history.search("missing").is_empty());
        assert_eq!(history.search("").len(), 2);
    }

    #[test]
    fn search_results_are_newest_first() {
        let mut history = History::new();
        history.add("https://rust/a", "first", 1);
        history.add("https://rust/b", "second", 2);
        let results = history.search("rust");
        assert_eq!(results[0].url, "https://rust/b");
        assert_eq!(results[1].url, "https://rust/a");
    }

    #[test]
    fn cap_evicts_oldest_visits() {
        let mut history = History::with_cap(3);
        for i in 0..5 {
            history.add(format!("https://site/{i}"), format!("Site {i}"), i);
        }
        assert_eq!(history.len(), 3);
        let urls: Vec<&str> = history
            .visits()
            .iter()
            .map(|visit| visit.url.as_str())
            .collect();
        assert_eq!(urls, ["https://site/2", "https://site/3", "https://site/4"]);
        assert_eq!(history.recent(1)[0].url, "https://site/4");
    }

    #[test]
    fn zero_cap_stores_nothing() {
        let mut history = History::with_cap(0);
        history.add("url", "title", 1);
        assert!(history.is_empty());
        assert_eq!(history.cap(), 0);
        assert!(history.recent(5).is_empty());
    }

    #[test]
    fn clear_removes_all_visits() {
        let mut history = History::new();
        history.add("a", "A", 1);
        history.add("b", "B", 2);
        history.clear();
        assert!(history.is_empty());
        assert!(history.recent(5).is_empty());
        assert!(history.search("A").is_empty());
    }

    #[test]
    fn default_cap_is_ten_thousand() {
        assert_eq!(History::new().cap(), DEFAULT_HISTORY_CAP);
        assert_eq!(History::new().cap(), 10_000);
    }
}
