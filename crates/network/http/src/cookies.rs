//! Cookie storage and `Cookie` header generation.

use browser_url::Url;

/// A single stored cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// Domain the cookie is scoped to (lowercase, without leading dot).
    pub domain: String,
    /// Path prefix scope.
    pub path: String,
    /// Only send over HTTPS.
    pub secure: bool,
    /// Not exposed to scripts.
    pub http_only: bool,
    /// Host-only cookie (no `Domain` attribute): must match the host exactly.
    pub host_only: bool,
}

impl Cookie {
    /// Builds a cookie from a `Set-Cookie` header for `request_url`.
    pub fn from_set_cookie(
        request_url: &Url,
        name: &str,
        value: &str,
        attrs: &[(&str, &str)],
    ) -> Option<Self> {
        let host = request_url.host()?.to_ascii_lowercase();
        let mut cookie = Self {
            name: name.to_string(),
            value: value.to_string(),
            domain: host.clone(),
            path: default_path(request_url),
            secure: false,
            http_only: false,
            host_only: true,
        };
        for (key, val) in attrs {
            match key.to_ascii_lowercase().as_str() {
                "domain" => {
                    let domain = val.trim_start_matches('.').to_ascii_lowercase();
                    if !domain.is_empty() && domain_matches(&host, &domain) {
                        cookie.domain = domain;
                        cookie.host_only = false;
                    }
                }
                "path" => {
                    if val.starts_with('/') {
                        cookie.path = (*val).to_string();
                    }
                }
                "secure" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                _ => {}
            }
        }
        if cookie.name.is_empty() {
            return None;
        }
        Some(cookie)
    }

    /// True when this cookie may be sent with a request to `url`.
    pub fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host() else { return false };
        let host = host.to_ascii_lowercase();
        if self.host_only {
            if host != self.domain {
                return false;
            }
        } else if !domain_matches(&host, &self.domain) {
            return false;
        }
        if self.secure && url.scheme() != "https" {
            return false;
        }
        path_matches(&url.path(), &self.path)
    }
}

/// A cookie jar scoped to a browser session.
#[derive(Debug, Default, Clone)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    /// Creates an empty jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores a cookie parsed from a `Set-Cookie` header.
    pub fn store(&mut self, request_url: &Url, name: &str, value: &str, attrs: &[(&str, &str)]) {
        let Some(cookie) = Cookie::from_set_cookie(request_url, name, value, attrs) else {
            return;
        };
        if let Some(existing) = self
            .cookies
            .iter_mut()
            .find(|c| c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        {
            *existing = cookie;
        } else {
            self.cookies.push(cookie);
        }
    }

    /// Stores every `Set-Cookie` from a response.
    pub fn store_response(&mut self, url: &Url, response: &crate::response::Response) {
        for (name, value, attrs) in response.set_cookies() {
            self.store(url, name, value, &attrs);
        }
    }

    /// Builds the `Cookie` header value for `url`, if any cookie applies.
    pub fn cookie_header(&self, url: &Url) -> Option<String> {
        let matching: Vec<&Cookie> = self.cookies.iter().filter(|c| c.matches(url)).collect();
        if matching.is_empty() {
            return None;
        }
        let mut pairs: Vec<&Cookie> = matching;
        pairs.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        Some(
            pairs
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Number of stored cookies.
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    /// True when the jar holds no cookies.
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Removes all cookies.
    pub fn clear(&mut self) {
        self.cookies.clear();
    }
}

fn default_path(url: &Url) -> String {
    let path = url.path();
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => path[..i].to_string(),
    }
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    request_path.starts_with(cookie_path)
        && (cookie_path.ends_with('/')
            || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_sends_cookies() {
        let url = Url::parse("https://example.com/app/page").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, "a", "1", &[("Path", "/app")]);
        jar.store(&url, "b", "2", &[("Path", "/"), ("Secure", "")]);
        let header = jar.cookie_header(&url).unwrap();
        assert!(header.contains("a=1"));
        assert!(header.contains("b=2"));
        assert_eq!(jar.len(), 2);
    }

    #[test]
    fn secure_cookies_are_https_only() {
        let https = Url::parse("https://example.com/").unwrap();
        let http = Url::parse("http://example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&https, "s", "v", &[("Secure", "")]);
        assert!(jar.cookie_header(&https).is_some());
        assert!(jar.cookie_header(&http).is_none());
    }

    #[test]
    fn domain_cookies_apply_to_subdomains() {
        let url = Url::parse("https://a.example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, "d", "v", &[("Domain", "example.com")]);
        let sub = Url::parse("https://b.example.com/x").unwrap();
        assert!(jar.cookie_header(&sub).is_some());
        let evil = Url::parse("https://notexample.com/").unwrap();
        assert!(jar.cookie_header(&evil).is_none());
    }

    #[test]
    fn replacing_same_cookie_keeps_single_entry() {
        let url = Url::parse("https://example.com/x").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, "k", "1", &[]);
        jar.store(&url, "k", "2", &[]);
        assert_eq!(jar.len(), 1);
        assert_eq!(jar.cookie_header(&url).unwrap(), "k=2");
    }
}
