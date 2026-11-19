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
    ///
    /// Expired entries are kept (rather than dropped) so a stale response with
    /// a validator can be revalidated with `If-None-Match`.
    pub fn get(&mut self, url: &str) -> Option<Response> {
        let entry = self.entries.get(url)?;
        if entry.stored_at.elapsed() >= entry.ttl {
            return None;
        }
        Some(entry.response.clone())
    }

    /// Returns any stored response for `url`, fresh or stale.
    pub fn get_stored(&self, url: &str) -> Option<Response> {
        self.entries.get(url).map(|entry| entry.response.clone())
    }

    /// Returns the `ETag` of a stored response for conditional revalidation.
    pub fn revalidation_etag(&self, url: &str) -> Option<String> {
        self.entries
            .get(url)
            .and_then(|entry| entry.response.etag().map(str::to_string))
    }

    /// Stores `response` if its `Cache-Control` allows caching.
    pub fn store(&mut self, url: &str, response: &Response) {
        let Some(ttl) = cache_ttl(response) else {
            return;
        };
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
///
/// A zero lifetime means the entry is stored but always revalidated: this is
/// how `no-cache`, `max-age=0` and validator-only responses are handled.
pub fn cache_ttl(response: &Response) -> Option<Duration> {
    if !response.is_success() {
        return None;
    }
    let cc = response
        .headers
        .get("Cache-Control")
        .map(str::to_ascii_lowercase);
    if cc.as_deref().is_some_and(|cc| cc.contains("no-store")) {
        return None;
    }
    let Some(cc) = cc else {
        // ETag-only responses are still worth keeping for revalidation.
        return response.etag().map(|_| Duration::ZERO);
    };
    let mut ttl = None;
    for directive in cc.split(',') {
        let directive = directive.trim();
        if directive == "no-cache" {
            return Some(Duration::ZERO);
        }
        if let Some(value) = directive.strip_prefix("max-age=")
            && let Ok(secs) = value.trim().trim_matches('"').parse::<u64>()
        {
            ttl = Some(Duration::from_secs(secs));
        }
    }
    Some(ttl.unwrap_or(Duration::ZERO))
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
    fn stores_no_cache_responses_for_revalidation() {
        let mut response = response("no-cache, max-age=60");
        response.headers.insert("ETag", "\"v1\"");
        let mut cache = HttpCache::default();
        cache.store("http://a.test/", &response);
        assert!(cache.get("http://a.test/").is_none());
        assert_eq!(
            cache.revalidation_etag("http://a.test/").as_deref(),
            Some("\"v1\"")
        );
        assert_eq!(cache.get_stored("http://a.test/").unwrap().body, b"body");
    }

    #[test]
    fn etag_only_responses_are_revalidated() {
        let mut response = response("no-store");
        response.headers.remove("Cache-Control");
        response.headers.insert("ETag", "\"x\"");
        let mut cache = HttpCache::default();
        cache.store("http://a.test/", &response);
        assert!(cache.get_stored("http://a.test/").is_some());
        assert!(cache.get("http://a.test/").is_none());
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
