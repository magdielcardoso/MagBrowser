//! Cookie storage and `Cookie` header generation.

use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// When the cookie expires (`None` for a session cookie).
    pub expires_at: Option<Instant>,
    /// Insertion order, used to keep `Cookie` headers deterministic.
    pub created_at: Instant,
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
        let name = name.trim();
        if name.is_empty() || !valid_name(name) {
            return None;
        }
        let value = unquote(value.trim());
        if !valid_value(&value) {
            return None;
        }

        let now = Instant::now();
        let mut cookie = Self {
            name: name.to_string(),
            value,
            domain: host.clone(),
            path: default_path(request_url),
            secure: false,
            http_only: false,
            host_only: true,
            expires_at: None,
            created_at: now,
        };

        let mut max_age = None;
        let mut expires = None;
        for (key, val) in attrs {
            match key.trim().to_ascii_lowercase().as_str() {
                "domain" => {
                    let domain = val.trim().trim_start_matches('.').to_ascii_lowercase();
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
                // A server cannot meaningfully set a Secure cookie over http.
                "secure" if request_url.scheme() == "https" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                "max-age" => {
                    if let Ok(seconds) = val.trim().parse::<i64>() {
                        max_age = Some(seconds);
                    }
                }
                "expires" => expires = parse_http_date(val.trim()),
                _ => {}
            }
        }

        // Max-Age takes precedence over Expires regardless of attribute order.
        cookie.expires_at = match max_age {
            Some(seconds) if seconds <= 0 => Some(now - Duration::from_secs(1)),
            Some(seconds) => now.checked_add(Duration::from_secs(seconds as u64)),
            None => expires.map(|time| instant_from_system_time(time, now)),
        };
        Some(cookie)
    }

    /// True when this cookie may be sent with a request to `url`.
    pub fn matches(&self, url: &Url) -> bool {
        let now = Instant::now();
        if self.is_expired(now) {
            return false;
        }
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

    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|expiry| expiry <= now)
    }

    fn same_key(&self, other: &Self) -> bool {
        self.name == other.name
            && self.domain == other.domain
            && self.path == other.path
            && self.host_only == other.host_only
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
        let now = Instant::now();
        // A new cookie replaces the old one with the same name/domain/path, and
        // an already-expired cookie deletes it (Max-Age=0 or a past Expires).
        self.cookies.retain(|existing| !existing.same_key(&cookie));
        if cookie.is_expired(now) {
            return;
        }
        self.cookies.push(cookie);
        self.cookies.retain(|existing| !existing.is_expired(now));
    }

    /// Stores every `Set-Cookie` from a response.
    pub fn store_response(&mut self, url: &Url, response: &crate::response::Response) {
        for (name, value, attrs) in response.set_cookies() {
            self.store(url, name, value, &attrs);
        }
    }

    /// Builds the `Cookie` header value for `url`, if any cookie applies.
    pub fn cookie_header(&self, url: &Url) -> Option<String> {
        let mut matching: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|cookie| cookie.matches(url))
            .collect();
        if matching.is_empty() {
            return None;
        }
        // RFC 6265: longer paths first, then older cookies first.
        matching.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.created_at.cmp(&b.created_at))
        });
        let mut seen = HashSet::new();
        let pairs: Vec<String> = matching
            .into_iter()
            .filter(|cookie| seen.insert(cookie.name.as_str()))
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect();
        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
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

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn valid_name(name: &str) -> bool {
    name.bytes().all(|byte| {
        byte.is_ascii_graphic()
            && !matches!(
                byte,
                b'(' | b')'
                    | b'<'
                    | b'>'
                    | b'@'
                    | b','
                    | b';'
                    | b':'
                    | b'\\'
                    | b'"'
                    | b'/'
                    | b'['
                    | b']'
                    | b'?'
                    | b'='
                    | b'{'
                    | b'}'
            )
    })
}

fn valid_value(value: &str) -> bool {
    !value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
}

fn instant_from_system_time(time: SystemTime, now: Instant) -> Instant {
    match time.duration_since(SystemTime::now()) {
        Ok(remaining) => now.checked_add(remaining).unwrap_or(now),
        // Instants cannot reach back before boot; returning `now` still marks
        // the cookie expired because `is_expired` uses `<=`.
        Err(elapsed) => now.checked_sub(elapsed.duration()).unwrap_or(now),
    }
}

/// Parses the IMF-fixdate form used by `Expires` (with a couple of tolerant
/// fallbacks), returning a `SystemTime`.
fn parse_http_date(input: &str) -> Option<SystemTime> {
    let (year, month, day, time) = parse_date_parts(input)?;
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) {
        return None;
    }
    let mut clock = time.split(':');
    let hour = clock.next()?.parse::<u64>().ok()?;
    let minute = clock.next()?.parse::<u64>().ok()?;
    let second = clock.next()?.parse::<u64>().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64;
    let seconds = u64::try_from(seconds).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// Returns `(year, month, day, "HH:MM:SS")`.
fn parse_date_parts(input: &str) -> Option<(i64, u32, u32, &str)> {
    // IMF-fixdate: "Sun, 06 Nov 1994 08:49:37 GMT"
    if let Some((_, rest)) = input.split_once(',') {
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        if tokens.len() >= 5 {
            let day = tokens.first()?.parse::<u32>().ok()?;
            let month = month_number(tokens.get(1)?)?;
            let year = tokens.get(2)?.parse::<i64>().ok()?;
            return Some((year, month, day, tokens[3]));
        }
        // RFC 850: "Sunday, 06-Nov-94 08:49:37 GMT"
        let date = tokens.first()?;
        let time = tokens.get(1)?;
        let pieces: Vec<&str> = date.split('-').collect();
        let day = pieces.first()?.parse::<u32>().ok()?;
        let month = month_number(pieces.get(1)?)?;
        let year = expand_year(pieces.get(2)?.parse::<i64>().ok()?);
        return Some((year, month, day, time));
    }
    // asctime: "Sun Nov  6 08:49:37 1994"
    let tokens: Vec<&str> = input.split_whitespace().collect();
    if tokens.len() >= 5 {
        let month = month_number(tokens.get(1)?)?;
        let day = tokens.get(2)?.parse::<u32>().ok()?;
        let year = tokens.get(4)?.parse::<i64>().ok()?;
        return Some((year, month, day, tokens[3]));
    }
    None
}

fn month_number(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

fn expand_year(year: i64) -> i64 {
    if (0..70).contains(&year) {
        2000 + year
    } else if (70..100).contains(&year) {
        1900 + year
    } else {
        year
    }
}

/// Days since 1970-01-01 (Howard Hinnant's civil-from-days inverse).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * i64::from(shifted_month) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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

    #[test]
    fn strips_quoted_values_and_trims_whitespace() {
        let url = Url::parse("https://example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, " token ", "\"quoted value\"", &[("Path", "/")]);
        assert_eq!(jar.cookie_header(&url).unwrap(), "token=quoted value");
    }

    #[test]
    fn max_age_deletes_cookies() {
        let url = Url::parse("https://example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, "k", "v", &[("Max-Age", "60")]);
        assert!(jar.cookie_header(&url).is_some());
        jar.store(&url, "k", "v", &[("Max-Age", "0")]);
        assert!(jar.is_empty());
        assert!(jar.cookie_header(&url).is_none());
    }

    #[test]
    fn past_and_future_expires_dates() {
        let url = Url::parse("https://example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(
            &url,
            "old",
            "v",
            &[("Expires", "Wed, 21 Oct 2015 07:28:00 GMT")],
        );
        assert!(jar.is_empty());
        jar.store(
            &url,
            "new",
            "v",
            &[("Expires", "Wed, 21 Oct 2099 07:28:00 GMT")],
        );
        assert!(jar.cookie_header(&url).is_some());
    }

    #[test]
    fn max_age_wins_over_expires() {
        let url = Url::parse("https://example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(
            &url,
            "k",
            "v",
            &[
                ("Expires", "Wed, 21 Oct 2099 07:28:00 GMT"),
                ("Max-Age", "0"),
            ],
        );
        assert!(jar.is_empty());
    }

    #[test]
    fn secure_attribute_from_http_is_ignored() {
        let url = Url::parse("http://example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, "k", "v", &[("Secure", "")]);
        assert!(jar.cookie_header(&url).is_some());
    }

    #[test]
    fn longer_paths_are_sent_first() {
        let url = Url::parse("https://example.com/app/page").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, "root", "1", &[("Path", "/")]);
        jar.store(&url, "app", "2", &[("Path", "/app")]);
        assert_eq!(jar.cookie_header(&url).unwrap(), "app=2; root=1");
    }

    #[test]
    fn rejects_invalid_cookie_names() {
        let url = Url::parse("https://example.com/").unwrap();
        let mut jar = CookieJar::new();
        jar.store(&url, "", "v", &[]);
        jar.store(&url, "bad name", "v", &[]);
        assert!(jar.is_empty());
    }
}
