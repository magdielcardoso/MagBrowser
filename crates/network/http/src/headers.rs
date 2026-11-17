//! Case-insensitive HTTP header map with order preservation.

use std::fmt;

/// An ordered, case-insensitive multimap of HTTP headers.
#[derive(Debug, Clone, Default)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// Creates an empty header map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a header without replacing existing values.
    pub fn append(&mut self, name: &str, value: &str) {
        self.entries
            .push((name.to_string(), value.trim().to_string()));
    }

    /// Sets a header, replacing every existing value for that name.
    pub fn insert(&mut self, name: &str, value: &str) {
        self.remove(name);
        self.append(name, value);
    }

    /// Returns the first value for `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Returns every value for `name`.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Removes every value for `name`, returning how many were removed.
    pub fn remove(&mut self, name: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        before - self.entries.len()
    }

    /// True when at least one value exists for `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Iterates over `(name, value)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Number of stored header lines.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no headers are stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Splits a comma separated header list (for example `Accept`).
    pub fn comma_list(&self, name: &str) -> Vec<String> {
        self.get_all(name)
            .into_iter()
            .flat_map(|v| v.split(','))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

impl fmt::Display for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, value) in &self.entries {
            writeln!(f, "{name}: {value}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        let mut h = Headers::new();
        h.append("Content-Type", "text/html");
        h.append("Set-Cookie", "a=1");
        h.append("Set-Cookie", "b=2");
        assert_eq!(h.get("content-type"), Some("text/html"));
        assert_eq!(h.get_all("SET-COOKIE"), ["a=1", "b=2"]);
        h.insert("set-cookie", "c=3");
        assert_eq!(h.get_all("Set-Cookie"), ["c=3"]);
    }

    #[test]
    fn comma_lists_are_split() {
        let mut h = Headers::new();
        h.insert("Accept", "text/html, application/json");
        assert_eq!(h.comma_list("accept"), ["text/html", "application/json"]);
    }
}
