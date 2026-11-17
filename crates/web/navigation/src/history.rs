//! Session history: bounded navigation, forward truncation and reloads.
//!
//! A [`SessionHistory`] models one browsing context's list of entries plus the
//! current index. All mutations happen through [`SessionHistory::navigate`], which
//! returns the [`Navigation`] that a renderer should actually perform. History
//! operations that cannot move (back at the start, forward at the end, an
//! out-of-range `go`) return [`NavError::OutOfRange`] and leave the history
//! unchanged.

use std::fmt;

use browser_url::Url;

/// One entry in session history.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// The document URL for this entry.
    pub url: Url,
    /// The document title, initially empty and updated as the page loads.
    pub title: String,
    /// Restored scroll position as `(x, y)` in CSS pixels.
    pub scroll: (f32, f32),
}

/// The kind of history traversal being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationAction {
    /// Create a new entry, truncating any forward entries.
    Push,
    /// Overwrite the current entry.
    Replace,
    /// Reload the current entry without changing history.
    Reload,
    /// Move one entry back.
    Back,
    /// Move one entry forward.
    Forward,
    /// Move by a relative offset; `Go(0)` reloads the current entry.
    Go(i64),
}

/// A navigation the engine must perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Navigation {
    /// The URL to load.
    pub url: Url,
    /// The action that produced this navigation.
    pub action: NavigationAction,
    /// True when the current history entry is updated in place rather than a new
    /// entry being created.
    pub replace: bool,
}

/// Errors from history and navigation operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavError {
    /// The operation needs a current entry, but history is empty.
    NoCurrentEntry,
    /// The requested traversal lies outside the history list.
    OutOfRange,
    /// A URL was missing or is not a valid navigation target.
    InvalidUrl(String),
}

impl fmt::Display for NavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCurrentEntry => f.write_str("no current history entry"),
            Self::OutOfRange => f.write_str("history traversal is out of range"),
            Self::InvalidUrl(message) => write!(f, "invalid navigation URL: {message}"),
        }
    }
}

impl std::error::Error for NavError {}

/// The session history of one browsing context.
#[derive(Debug, Clone, Default)]
pub struct SessionHistory {
    entries: Vec<HistoryEntry>,
    index: usize,
}

impl SessionHistory {
    /// Creates an empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// The entry the browsing context is currently showing.
    pub fn current(&self) -> Option<&HistoryEntry> {
        self.entries.get(self.index)
    }

    /// The current entry index, or zero when history is empty.
    pub fn current_index(&self) -> usize {
        self.index
    }

    /// Number of entries in history.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when history has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, oldest first.
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// True when a back traversal is possible.
    pub fn can_go_back(&self) -> bool {
        self.index > 0 && !self.entries.is_empty()
    }

    /// True when a forward traversal is possible.
    pub fn can_go_forward(&self) -> bool {
        !self.entries.is_empty() && self.index + 1 < self.entries.len()
    }

    /// Applies a history operation and returns the navigation to perform.
    ///
    /// * `Push` requires `url`, truncates forward entries and appends a new entry.
    /// * `Replace` requires `url` and a current entry, and overwrites it.
    /// * `Reload` reuses the current entry's URL; any supplied `url` is ignored.
    /// * `Back`/`Forward`/`Go` move the index and return the destination entry's
    ///   URL; `Go(0)` reloads the current entry.
    ///
    /// On error the history is left untouched.
    pub fn navigate(
        &mut self,
        action: NavigationAction,
        url: Option<Url>,
        title: &str,
    ) -> Result<Navigation, NavError> {
        match action {
            NavigationAction::Push => {
                let url = url.ok_or_else(|| {
                    NavError::InvalidUrl("push navigation requires a URL".to_string())
                })?;
                ensure_navigable(&url)?;
                self.entries.truncate(self.index + 1);
                self.entries.push(HistoryEntry {
                    url: url.clone(),
                    title: title.to_string(),
                    scroll: (0.0, 0.0),
                });
                self.index = self.entries.len() - 1;
                Ok(Navigation {
                    url,
                    action,
                    replace: false,
                })
            }
            NavigationAction::Replace => {
                let url = url.ok_or_else(|| {
                    NavError::InvalidUrl("replace navigation requires a URL".to_string())
                })?;
                ensure_navigable(&url)?;
                if self.entries.is_empty() {
                    return Err(NavError::NoCurrentEntry);
                }
                let entry = &mut self.entries[self.index];
                entry.url = url.clone();
                entry.title = title.to_string();
                entry.scroll = (0.0, 0.0);
                Ok(Navigation {
                    url,
                    action,
                    replace: true,
                })
            }
            NavigationAction::Reload => {
                let current = self.current().ok_or(NavError::NoCurrentEntry)?;
                Ok(Navigation {
                    url: current.url.clone(),
                    action,
                    replace: false,
                })
            }
            NavigationAction::Back => {
                if !self.can_go_back() {
                    return Err(NavError::OutOfRange);
                }
                self.index -= 1;
                Ok(self.navigation_to_current(action))
            }
            NavigationAction::Forward => {
                if !self.can_go_forward() {
                    return Err(NavError::OutOfRange);
                }
                self.index += 1;
                Ok(self.navigation_to_current(action))
            }
            NavigationAction::Go(offset) => {
                if self.entries.is_empty() {
                    return Err(NavError::NoCurrentEntry);
                }
                let target = self.index as i64 + offset;
                if target < 0 || target >= self.entries.len() as i64 {
                    return Err(NavError::OutOfRange);
                }
                self.index = target as usize;
                Ok(self.navigation_to_current(action))
            }
        }
    }

    /// Replaces the current entry's title.
    pub fn set_current_title(&mut self, title: &str) {
        if let Some(entry) = self.entries.get_mut(self.index) {
            entry.title = title.to_string();
        }
    }

    /// Records the current entry's scroll position.
    pub fn set_current_scroll(&mut self, x: f32, y: f32) {
        if let Some(entry) = self.entries.get_mut(self.index) {
            entry.scroll = (x, y);
        }
    }

    fn navigation_to_current(&self, action: NavigationAction) -> Navigation {
        let url = self.entries.get(self.index).map_or_else(
            || Url::parse("about:blank").expect("about:blank is valid"),
            |e| e.url.clone(),
        );
        Navigation {
            url,
            action,
            replace: false,
        }
    }
}

fn ensure_navigable(url: &Url) -> Result<(), NavError> {
    if url.scheme() == "javascript" {
        return Err(NavError::InvalidUrl(
            "javascript: URLs cannot be navigated to directly".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).unwrap()
    }

    fn history_with(urls: &[&str]) -> SessionHistory {
        let mut history = SessionHistory::new();
        for (i, input) in urls.iter().enumerate() {
            let nav = history
                .navigate(
                    NavigationAction::Push,
                    Some(url(input)),
                    &format!("page {i}"),
                )
                .unwrap();
            assert!(!nav.replace);
        }
        history
    }

    #[test]
    fn push_appends_entries_and_updates_current() {
        let mut history = SessionHistory::new();
        let nav = history
            .navigate(
                NavigationAction::Push,
                Some(url("https://example.com/a")),
                "A",
            )
            .unwrap();
        assert_eq!(nav.url, url("https://example.com/a"));
        assert_eq!(nav.action, NavigationAction::Push);
        assert_eq!(history.len(), 1);
        assert_eq!(history.current_index(), 0);
        assert_eq!(history.current().unwrap().title, "A");
        assert_eq!(history.current().unwrap().scroll, (0.0, 0.0));
    }

    #[test]
    fn push_truncates_forward_entries() {
        let mut history = history_with(&[
            "https://example.com/1",
            "https://example.com/2",
            "https://example.com/3",
        ]);
        history.navigate(NavigationAction::Back, None, "").unwrap();
        assert!(history.can_go_forward());
        history
            .navigate(
                NavigationAction::Push,
                Some(url("https://example.com/new")),
                "new",
            )
            .unwrap();
        let urls: Vec<_> = history.entries().iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://example.com/1",
                "https://example.com/2",
                "https://example.com/new"
            ]
        );
        assert!(!history.can_go_forward());
        assert_eq!(history.current_index(), 2);
    }

    #[test]
    fn replace_overwrites_the_current_entry() {
        let mut history = history_with(&["https://example.com/1", "https://example.com/2"]);
        let nav = history
            .navigate(
                NavigationAction::Replace,
                Some(url("https://example.com/replaced")),
                "replaced",
            )
            .unwrap();
        assert!(nav.replace);
        assert_eq!(history.len(), 2);
        assert_eq!(history.current_index(), 1);
        assert_eq!(
            history.current().unwrap().url.as_str(),
            "https://example.com/replaced"
        );
        assert_eq!(history.current().unwrap().title, "replaced");
    }

    #[test]
    fn reload_reuses_the_current_url() {
        let mut history = history_with(&["https://example.com/a?x=1"]);
        let nav = history
            .navigate(
                NavigationAction::Reload,
                Some(url("https://example.com/ignored")),
                "",
            )
            .unwrap();
        assert_eq!(nav.action, NavigationAction::Reload);
        assert!(!nav.replace);
        assert_eq!(nav.url.as_str(), "https://example.com/a?x=1");
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn back_and_forward_move_the_index() {
        let mut history = history_with(&[
            "https://example.com/1",
            "https://example.com/2",
            "https://example.com/3",
        ]);
        let back = history.navigate(NavigationAction::Back, None, "").unwrap();
        assert_eq!(back.url.as_str(), "https://example.com/2");
        assert_eq!(history.current_index(), 1);
        assert!(history.can_go_back());
        assert!(history.can_go_forward());
        let forward = history
            .navigate(NavigationAction::Forward, None, "")
            .unwrap();
        assert_eq!(forward.url.as_str(), "https://example.com/3");
        assert_eq!(history.current_index(), 2);
        assert!(!history.can_go_forward());
    }

    #[test]
    fn go_uses_relative_offsets() {
        let mut history = history_with(&[
            "https://example.com/1",
            "https://example.com/2",
            "https://example.com/3",
        ]);
        let nav = history
            .navigate(NavigationAction::Go(-2), None, "")
            .unwrap();
        assert_eq!(nav.url.as_str(), "https://example.com/1");
        assert_eq!(history.current_index(), 0);
        let nav = history.navigate(NavigationAction::Go(2), None, "").unwrap();
        assert_eq!(nav.url.as_str(), "https://example.com/3");
        assert_eq!(history.current_index(), 2);
    }

    #[test]
    fn go_zero_reloads_the_current_entry() {
        let mut history = history_with(&["https://example.com/1", "https://example.com/2"]);
        let nav = history.navigate(NavigationAction::Go(0), None, "").unwrap();
        assert_eq!(nav.action, NavigationAction::Go(0));
        assert_eq!(nav.url.as_str(), "https://example.com/2");
        assert_eq!(history.current_index(), 1);
    }

    #[test]
    fn out_of_range_traversal_leaves_history_unchanged() {
        let mut history = history_with(&["https://example.com/1"]);
        assert_eq!(
            history.navigate(NavigationAction::Back, None, ""),
            Err(NavError::OutOfRange)
        );
        assert_eq!(
            history.navigate(NavigationAction::Forward, None, ""),
            Err(NavError::OutOfRange)
        );
        assert_eq!(
            history.navigate(NavigationAction::Go(1), None, ""),
            Err(NavError::OutOfRange)
        );
        assert_eq!(
            history.navigate(NavigationAction::Go(-1), None, ""),
            Err(NavError::OutOfRange)
        );
        assert_eq!(history.len(), 1);
        assert_eq!(history.current_index(), 0);
    }

    #[test]
    fn history_length_never_causes_a_navigation() {
        let mut history = history_with(&[
            "https://example.com/1",
            "https://example.com/2",
            "https://example.com/3",
        ]);
        let before = history.current().unwrap().clone();
        assert_eq!(
            history.navigate(NavigationAction::Go(3), None, ""),
            Err(NavError::OutOfRange)
        );
        assert_eq!(history.current().unwrap(), &before);
        assert_eq!(
            history.navigate(NavigationAction::Go(-4), None, ""),
            Err(NavError::OutOfRange)
        );
        assert_eq!(history.current().unwrap(), &before);
    }

    #[test]
    fn empty_history_rejects_traversal_and_reload() {
        let mut history = SessionHistory::new();
        assert_eq!(
            history.navigate(NavigationAction::Back, None, ""),
            Err(NavError::OutOfRange)
        );
        assert_eq!(
            history.navigate(NavigationAction::Reload, None, ""),
            Err(NavError::NoCurrentEntry)
        );
        assert_eq!(
            history.navigate(
                NavigationAction::Replace,
                Some(url("https://example.com/")),
                ""
            ),
            Err(NavError::NoCurrentEntry)
        );
        assert_eq!(
            history.navigate(NavigationAction::Go(0), None, ""),
            Err(NavError::NoCurrentEntry)
        );
        assert!(history.is_empty());
    }

    #[test]
    fn push_without_url_is_rejected() {
        let mut history = SessionHistory::new();
        let error = history
            .navigate(NavigationAction::Push, None, "")
            .unwrap_err();
        assert!(matches!(error, NavError::InvalidUrl(_)));
        assert!(history.is_empty());
    }

    #[test]
    fn javascript_urls_are_not_navigable() {
        let mut history = SessionHistory::new();
        let error = history
            .navigate(NavigationAction::Push, Some(url("javascript:alert(1)")), "")
            .unwrap_err();
        assert_eq!(
            error,
            NavError::InvalidUrl("javascript: URLs cannot be navigated to directly".to_string())
        );
        assert!(history.is_empty());
    }

    #[test]
    fn title_and_scroll_updates_preserve_entry_identity() {
        let mut history = history_with(&["https://example.com/"]);
        history.set_current_title("Loaded");
        history.set_current_scroll(12.0, 34.0);
        let entry = history.current().unwrap();
        assert_eq!(entry.title, "Loaded");
        assert_eq!(entry.scroll, (12.0, 34.0));
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn nav_error_display_is_informative() {
        assert_eq!(
            NavError::OutOfRange.to_string(),
            "history traversal is out of range"
        );
        assert!(
            NavError::InvalidUrl("x".to_string())
                .to_string()
                .contains('x')
        );
    }
}
