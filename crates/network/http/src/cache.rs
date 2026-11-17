//! A small in-memory HTTP response cache driven by `Cache-Control`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::response::Response;

#[derive(Debug)]
struct Entry {
    response: Response,
    stored_at: Instant,
    ttl: Duration,
    size: usize,
}

/// In-memory HTTP cache with `max-age` and `no-store` support.
#[derive(Debug)]
pub struct HttpCache {
    entries: HashMap<String, Entry>,
    bytes: usize,
    max_bytes: usize,
}

impl Default for HttpCache {
    fn default() -> Self {
        Self::new(8 * 1024 * 1024)
    }
}

impl HttpCache {
    /// Creates a cache with a byte budget.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            max_bytes,
        }
    }

    /// Returns a fresh cached response for `url`.
    pub fn get(&mut self, url: &str) -> Option<Response> {
        let entry = self.entries.get(url)?;
        if entry.stored_at.elapsed() >= entry.ttl {
            let removed = self.entries.remove(url)?;
            self.bytes = self.bytes.saturating_sub(removed.size);
            return None;
        }
        Some(entry.response.clone())
    }

    /// Stores `response` if its `Cache-Control` allows caching.
    pub fn store(&mut self, url: &str, response: &Response) {
        let Some(ttl) = cache_ttl(response) else {
            return;
        };
        if ttl.is_zero() {
            return;
        }
        let size = response.body.len() + 256;
        if size > self.max_bytes {
            return;
        }
        if let Some(old) = self.entries.remove(url) {
            self.bytes = self.bytes.saturating_sub(old.size);
        }
        self.bytes += size;
        self.entries.insert(
            url.to_string(),
            Entry {
                response: response.clone(),
                stored_at: Instant::now(),
                ttl,
                size,
            },
        );
        self.evict();
    }

    /// Number of cached responses.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clears all cached responses.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn evict(&mut self) {
        while self.bytes > self.max_bytes && !self.entries.is_empty() {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.stored_at)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest
                && let Some(removed) = self.entries.remove(&key)
            {
                self.bytes = self.bytes.saturating_sub(removed.size);
            }
        }
    }
}

/// Computes the freshness lifetime of a response, or `None` when it must not be cached.
pub fn cache_ttl(response: &Response) -> Option<Duration> {
    if !response.is_success() {
        return None;
    }
    let cc = response.headers.get("Cache-Control")?.to_ascii_lowercase();
    if cc.contains("no-store") {
        return None;
    }
    let mut ttl = Duration::ZERO;
    for directive in cc.split(',') {
        let directive = directive.trim();
        if directive == "no-cache" {
            return None;
        }
        if let Some(value) = directive.strip_prefix("max-age=")
            && let Ok(secs) = value.trim().trim_matches('"').parse::<u64>()
        {
            ttl = Duration::from_secs(secs);
        }
    }
    Some(ttl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headers::Headers;
    use browser_url::Url;

    fn response(cc: &str) -> Response {
        let mut headers = Headers::new();
        headers.insert("Cache-Control", cc);
        Response::new(
            Url::parse("http://a.test/").unwrap(),
            200,
            "OK",
            headers,
            b"body".to_vec(),
        )
    }

    #[test]
    fn caches_only_max_age_responses() {
        let mut cache = HttpCache::default();
        cache.store("http://a.test/", &response("max-age=60"));
        assert!(cache.get("http://a.test/").is_some());
        cache.store("http://a.test/none", &response("no-store"));
        assert!(cache.get("http://a.test/none").is_none());
        cache.store("http://a.test/zero", &response("max-age=0"));
        assert!(cache.get("http://a.test/zero").is_none());
    }

    #[test]
    fn evicts_when_over_budget() {
        let mut cache = HttpCache::new(600);
        for i in 0..10 {
            cache.store(&format!("http://a.test/{i}"), &response("max-age=60"));
        }
        assert!(cache.len() < 10);
    }
}
