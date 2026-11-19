//! Find-in-page model: query state, byte-range matches and navigation.
//!
//! [`find_all`] is a pure function: it scans a page's plain text and returns
//! half-open byte ranges into the original `&str`, so callers can map matches
//! straight onto the text/layout representation used by the window layer.
//! [`FindBar`] wraps those matches with navigation state (next / previous /
//! currently highlighted match).

// A framework-agnostic model: not every accessor is used by the window shell.
#![allow(dead_code)]

/// A half-open byte range `[start, end)` into some text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextRange {
    /// Byte offset of the first matched byte.
    pub start: usize,
    /// Byte offset just past the last matched byte.
    pub end: usize,
}

impl TextRange {
    /// Creates a byte range, normalising inverted endpoints.
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start.min(end),
            end: start.max(end),
        }
    }

    /// Length of the range in bytes.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// True when the range covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Converts to a standard [`std::ops::Range`] for slicing.
    pub fn as_range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }
}

/// Finds every non-overlapping occurrence of `query` in `text`.
///
/// Offsets are byte positions into `text` (not character indices), which makes
/// them directly usable for slicing and for mapping onto layout runs. When
/// `case_sensitive` is `false`, matching is done per character using Unicode
/// lowercasing, so accented Latin text works while byte offsets stay correct.
/// An empty query or empty text produces no matches.
///
/// Matches do not overlap: after a hit, scanning resumes at the end byte of
/// that hit (matching the behaviour of [`str::match_indices`]).
pub fn find_all(text: &str, query: &str, case_sensitive: bool) -> Vec<TextRange> {
    let mut found = Vec::new();
    if query.is_empty() || text.is_empty() {
        return found;
    }

    let query_chars: Vec<char> = query.chars().collect();
    let query_len = query_chars.len();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let mut probe = cursor;
        let mut end = cursor;
        let mut matched = 0usize;

        for &qch in &query_chars {
            // `probe` is always a char boundary: it starts at 0, advances by a
            // whole char on a miss, and jumps to a match's end byte on a hit.
            let Some(tch) = text[probe..].chars().next() else {
                break;
            };
            let equal = if case_sensitive {
                tch == qch
            } else {
                tch.to_lowercase().eq(qch.to_lowercase())
            };
            if !equal {
                break;
            }
            end = probe + tch.len_utf8();
            probe = end;
            matched += 1;
        }

        if matched == query_len {
            found.push(TextRange::new(cursor, end));
            cursor = end;
        } else {
            cursor += text[cursor..].chars().next().map_or(1, char::len_utf8);
        }
    }

    found
}

/// State for a find-in-page bar.
///
/// Call [`Self::set_query`] when the user types a query and
/// [`Self::set_page_text`] when the page text changes; both recompute the match
/// list and reset navigation to the first match.
#[derive(Debug, Clone, Default)]
pub struct FindBar {
    query: String,
    page_text: String,
    case_sensitive: bool,
    matches: Vec<TextRange>,
    current: Option<usize>,
}

impl FindBar {
    /// Creates an empty find bar (case-insensitive by default).
    pub fn new() -> Self {
        Self::default()
    }

    /// The current search query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether matching is case-sensitive.
    pub fn is_case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// The page text the matches were computed from.
    pub fn page_text(&self) -> &str {
        &self.page_text
    }

    /// Sets a new query and recomputes matches against the stored page text.
    pub fn set_query(&mut self, query: &str, page_text: &str) {
        self.query = query.to_string();
        self.page_text = page_text.to_string();
        self.recompute();
    }

    /// Replaces the page text and recomputes matches for the current query.
    pub fn set_page_text(&mut self, page_text: &str) {
        self.page_text = page_text.to_string();
        self.recompute();
    }

    /// Toggles case sensitivity and recomputes matches.
    pub fn set_case_sensitive(&mut self, case_sensitive: bool) {
        self.case_sensitive = case_sensitive;
        self.recompute();
    }

    /// All current matches, in document order.
    pub fn matches(&self) -> &[TextRange] {
        &self.matches
    }

    /// Number of matches for the current query.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Index of the currently highlighted match, if any.
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// The currently highlighted match, if any.
    pub fn current_match(&self) -> Option<TextRange> {
        self.current.and_then(|i| self.matches.get(i).copied())
    }

    /// Advances to the next match with wrap-around, returning it.
    ///
    /// This is navigation state, not an [`Iterator`], so the name is kept for
    /// parity with find-in-page UIs rather than implementing the trait.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<TextRange> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let n = self.matches.len();
        self.current = Some(match self.current {
            Some(i) => (i + 1) % n,
            None => 0,
        });
        self.current_match()
    }

    /// Moves to the previous match with wrap-around, returning it.
    pub fn previous(&mut self) -> Option<TextRange> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let n = self.matches.len();
        self.current = Some(match self.current {
            Some(i) => (i + n - 1) % n,
            None => n - 1,
        });
        self.current_match()
    }

    /// Clears the query and matches. Case sensitivity is preserved.
    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current = None;
    }

    fn recompute(&mut self) {
        self.matches = find_all(&self.page_text, &self.query, self.case_sensitive);
        self.current = if self.matches.is_empty() {
            None
        } else {
            Some(0)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_range_helpers() {
        let r = TextRange::new(4, 2); // normalised
        assert_eq!(r.start, 2);
        assert_eq!(r.end, 4);
        assert_eq!(r.len(), 2);
        assert!(!r.is_empty());
        assert_eq!(r.as_range(), 2..4);
        assert!(TextRange::new(3, 3).is_empty());
    }

    #[test]
    fn finds_case_sensitive_matches() {
        let text = "Rust rust RUST rusty";
        let matches = find_all(text, "rust", true);
        assert_eq!(matches, vec![TextRange::new(5, 9), TextRange::new(15, 19)]);
        for m in &matches {
            assert_eq!(&text[m.as_range()], "rust");
        }
    }

    #[test]
    fn finds_case_insensitive_matches() {
        let text = "Rust rust RUST rusty";
        let matches = find_all(text, "rust", false);
        assert_eq!(
            matches,
            vec![
                TextRange::new(0, 4),
                TextRange::new(5, 9),
                TextRange::new(10, 14),
                TextRange::new(15, 19),
            ]
        );
    }

    #[test]
    fn byte_ranges_are_correct_for_multibyte_text() {
        let text = "café CAFÉ";
        let matches = find_all(text, "café", false);
        assert_eq!(matches, vec![TextRange::new(0, 5), TextRange::new(6, 11)]);
        for m in &matches {
            let slice = &text[m.as_range()];
            assert_eq!(slice.to_lowercase(), "café");
        }
    }

    #[test]
    fn empty_inputs_and_missing_queries() {
        assert!(find_all("anything", "", true).is_empty());
        assert!(find_all("", "x", false).is_empty());
        assert!(find_all("abcdef", "zz", false).is_empty());
    }

    #[test]
    fn matches_do_not_overlap() {
        let text = "aaaa";
        let matches = find_all(text, "aa", true);
        assert_eq!(matches, vec![TextRange::new(0, 2), TextRange::new(2, 4)]);
        // Matches the std library's non-overlapping match_indices behaviour.
        let std_matches: Vec<TextRange> = text
            .match_indices("aa")
            .map(|(i, s)| TextRange::new(i, i + s.len()))
            .collect();
        assert_eq!(matches, std_matches);
    }

    #[test]
    fn find_bar_navigates_with_wraparound() {
        let mut bar = FindBar::new();
        bar.set_query("rust", "Rust rust RUST");
        assert_eq!(bar.query(), "rust");
        assert!(!bar.is_case_sensitive());
        assert_eq!(bar.match_count(), 3);
        assert_eq!(bar.current_index(), Some(0));
        assert_eq!(bar.current_match(), Some(TextRange::new(0, 4)));
        assert_eq!(bar.next(), Some(TextRange::new(5, 9)));
        assert_eq!(bar.next(), Some(TextRange::new(10, 14)));
        assert_eq!(bar.next(), Some(TextRange::new(0, 4))); // wraps
        assert_eq!(bar.previous(), Some(TextRange::new(10, 14)));
    }

    #[test]
    fn find_bar_recomputes_on_toggles_and_text_changes() {
        let mut bar = FindBar::new();
        bar.set_query("rust", "Rust rust RUST");
        assert_eq!(bar.match_count(), 3);
        bar.set_case_sensitive(true);
        assert!(bar.is_case_sensitive());
        assert_eq!(bar.match_count(), 1);
        assert_eq!(bar.current_match(), Some(TextRange::new(5, 9)));
        bar.set_page_text("nothing here");
        assert_eq!(bar.match_count(), 0);
        assert_eq!(bar.current_index(), None);
        assert_eq!(bar.current_match(), None);
        assert_eq!(bar.next(), None);
        assert_eq!(bar.previous(), None);
        bar.clear();
        assert_eq!(bar.query(), "");
        assert!(bar.is_case_sensitive()); // preserved
    }
}
