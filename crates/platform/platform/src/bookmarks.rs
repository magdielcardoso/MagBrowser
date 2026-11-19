//! Flat, ordered collection of bookmarks with stable ids.
//!
//! [`Bookmarks`] is a simple list: each [`Bookmark`] has a unique id, a title
//! and a URL. It supports adding, removing and searching by title or URL, and
//! never touches the filesystem.

/// A single bookmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// Stable identifier assigned by the collection.
    pub id: u64,
    /// Human-readable title.
    pub title: String,
    /// Bookmarked URL.
    pub url: String,
}

/// A flat, ordered collection of bookmarks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bookmarks {
    next_id: u64,
    items: Vec<Bookmark>,
}

impl Bookmarks {
    /// Creates an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a bookmark and returns its stable id.
    pub fn add(&mut self, title: impl Into<String>, url: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Bookmark {
            id,
            title: title.into(),
            url: url.into(),
        });
        id
    }

    /// Removes and returns the bookmark with the given id, if present.
    pub fn remove(&mut self, id: u64) -> Option<Bookmark> {
        let index = self.items.iter().position(|bookmark| bookmark.id == id)?;
        Some(self.items.remove(index))
    }

    /// Searches bookmark titles and URLs, case-insensitively.
    ///
    /// An empty query matches every bookmark.
    pub fn search(&self, query: &str) -> Vec<Bookmark> {
        let needle = query.to_lowercase();
        self.items
            .iter()
            .filter(|bookmark| {
                bookmark.title.to_lowercase().contains(&needle)
                    || bookmark.url.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    /// Returns a bookmark by id, if present.
    pub fn get(&self, id: u64) -> Option<&Bookmark> {
        self.items.iter().find(|bookmark| bookmark.id == id)
    }

    /// Whether a bookmark with exactly this URL exists.
    pub fn contains_url(&self, url: &str) -> bool {
        self.items.iter().any(|bookmark| bookmark.url == url)
    }

    /// All bookmarks, in insertion order.
    pub fn all(&self) -> &[Bookmark] {
        &self.items
    }

    /// Number of bookmarks.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Removes every bookmark.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_get() {
        let mut bookmarks = Bookmarks::new();
        let id = bookmarks.add("Rust", "https://rust-lang.org");
        assert_eq!(
            bookmarks.get(id),
            Some(&Bookmark {
                id,
                title: "Rust".to_string(),
                url: "https://rust-lang.org".to_string(),
            })
        );
        assert_eq!(bookmarks.len(), 1);
        assert!(!bookmarks.is_empty());
    }

    #[test]
    fn remove_returns_removed_bookmark() {
        let mut bookmarks = Bookmarks::new();
        let a = bookmarks.add("A", "https://a");
        let b = bookmarks.add("B", "https://b");
        assert_eq!(
            bookmarks.remove(a),
            Some(Bookmark {
                id: a,
                title: "A".to_string(),
                url: "https://a".to_string(),
            })
        );
        assert_eq!(bookmarks.get(a), None);
        assert_eq!(bookmarks.get(b).unwrap().title, "B");
        assert_eq!(bookmarks.remove(a), None);
    }

    #[test]
    fn search_matches_title_and_url_case_insensitively() {
        let mut bookmarks = Bookmarks::new();
        bookmarks.add("Rust Language", "https://rust-lang.org");
        bookmarks.add("Cooking", "https://example.com/recipes");

        let by_title = bookmarks.search("RUST");
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].title, "Rust Language");

        let by_url = bookmarks.search("RECIPES");
        assert_eq!(by_url.len(), 1);
        assert_eq!(by_url[0].url, "https://example.com/recipes");

        assert!(bookmarks.search("nope").is_empty());
        assert_eq!(bookmarks.search("").len(), 2);
    }

    #[test]
    fn contains_url_all_and_clear() {
        let mut bookmarks = Bookmarks::new();
        bookmarks.add("A", "https://a");
        bookmarks.add("B", "https://b");
        assert!(bookmarks.contains_url("https://b"));
        assert!(!bookmarks.contains_url("https://c"));
        assert_eq!(bookmarks.all().len(), 2);
        bookmarks.clear();
        assert!(bookmarks.is_empty());
        assert!(bookmarks.all().is_empty());
    }

    #[test]
    fn ids_are_stable_and_unique() {
        let mut bookmarks = Bookmarks::new();
        let a = bookmarks.add("A", "a");
        let b = bookmarks.add("B", "b");
        assert_ne!(a, b);
        bookmarks.remove(a);
        let c = bookmarks.add("C", "c");
        assert_ne!(c, b);
    }
}
