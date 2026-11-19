//! Element attributes: ordered, case-insensitive for HTML elements.

use crate::limits::Limits;

/// An ordered attribute list with a bounded number of entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attributes {
    entries: Vec<(String, String)>,
    limit: usize,
}

impl Default for Attributes {
    fn default() -> Self {
        Self::new()
    }
}

impl Attributes {
    /// Creates an empty attribute list using the default attribute cap.
    pub fn new() -> Self {
        Self::with_limit(Limits::default().max_attributes_per_element)
    }

    /// Creates an empty attribute list that holds at most `limit` attributes.
    pub fn with_limit(limit: usize) -> Self {
        Self {
            entries: Vec::new(),
            limit,
        }
    }

    /// Maximum number of attributes this list will hold.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the value of `name` (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// True when `name` is present.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Sets `name`, replacing the existing value while keeping its position.
    ///
    /// A new attribute is silently ignored once [`Self::limit`] is reached;
    /// use [`Self::try_set`] to detect that case. Existing attributes can
    /// always be updated.
    pub fn set(&mut self, name: &str, value: &str) {
        let _ = self.try_set(name, value);
    }

    /// Sets `name`, returning `false` (without changing anything) when the
    /// attribute is new and the cap is full.
    pub fn try_set(&mut self, name: &str, value: &str) -> bool {
        if let Some((_, v)) = self
            .entries
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            *v = value.to_string();
            return true;
        }
        if self.entries.len() >= self.limit {
            return false;
        }
        self.entries.push((name.to_string(), value.to_string()));
        true
    }

    /// Removes `name`, returning its previous value.
    pub fn remove(&mut self, name: &str) -> Option<String> {
        let index = self
            .entries
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(name))?;
        Some(self.entries.remove(index).1)
    }

    /// Iterates over `(name, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Number of attributes.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when there are no attributes.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All `class` tokens.
    pub fn class_list(&self) -> Vec<&str> {
        self.get("class")
            .map(|v| v.split_ascii_whitespace().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_are_case_insensitive() {
        let mut attrs = Attributes::new();
        attrs.set("HREF", "/a");
        assert_eq!(attrs.get("href"), Some("/a"));
        attrs.set("href", "/b");
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs.get("href"), Some("/b"));
        assert_eq!(attrs.remove("Href").as_deref(), Some("/b"));
        assert!(attrs.is_empty());
    }

    #[test]
    fn class_list_splits_whitespace() {
        let mut attrs = Attributes::new();
        attrs.set("class", "  a  b\tc ");
        assert_eq!(attrs.class_list(), ["a", "b", "c"]);
    }

    #[test]
    fn attribute_cap_rejects_new_entries_but_allows_updates() {
        let mut attrs = Attributes::with_limit(2);
        assert_eq!(attrs.limit(), 2);
        assert!(attrs.try_set("a", "1"));
        assert!(attrs.try_set("b", "2"));
        assert!(!attrs.try_set("c", "3"));
        // The infallible API ignores the failure.
        attrs.set("d", "4");
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs.get("c"), None);
        assert_eq!(attrs.get("d"), None);

        // Updating an existing key still works at the cap.
        assert!(attrs.try_set("A", "updated"));
        assert_eq!(attrs.get("a"), Some("updated"));
        assert_eq!(attrs.len(), 2);

        // Freeing a slot permits a new attribute again.
        assert_eq!(attrs.remove("b").as_deref(), Some("2"));
        assert!(attrs.try_set("c", "3"));
        assert_eq!(attrs.len(), 2);
    }
}
