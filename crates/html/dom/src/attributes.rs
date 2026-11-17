//! Element attributes: ordered, case-insensitive for HTML elements.

/// An ordered attribute list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attributes {
    entries: Vec<(String, String)>,
}

impl Attributes {
    /// Creates an empty attribute list.
    pub fn new() -> Self {
        Self::default()
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
    pub fn set(&mut self, name: &str, value: &str) {
        if let Some((_, v)) = self
            .entries
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            *v = value.to_string();
            return;
        }
        self.entries.push((name.to_string(), value.to_string()));
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
}
