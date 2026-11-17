//! URL parsing, resolution and origin computation.
//!
//! This crate implements the subset of the WHATWG URL standard needed by the rest
//! of the engine: a full parser for absolute URLs, relative reference resolution
//! against a base URL, path normalisation (dot segments), percent-encoding helpers
//! and origin derivation.
//!
//! ```
//! use browser_url::Url;
//! let base = Url::parse("https://example.com/a/b?x=1").unwrap();
//! let link = Url::parse_with_base("../c d.txt#frag", &base).unwrap();
//! assert_eq!(link.as_str(), "https://example.com/c%20d.txt#frag");
//! assert_eq!(link.origin().to_string(), "https://example.com:443");
//! ```

mod origin;
mod parser;

pub use origin::Origin;

use std::fmt;

/// Errors produced while parsing URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlError {
    /// The input had no scheme and no base URL was available.
    MissingScheme,
    /// The scheme contains characters outside `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
    InvalidScheme,
    /// A special scheme URL had no host.
    MissingHost,
    /// The host contains a forbidden character or is otherwise malformed.
    InvalidHost,
    /// The port is not a valid 16-bit number.
    InvalidPort,
    /// The URL is serialisable but cannot be used to make a network request.
    NotSpecial,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::MissingScheme => "missing scheme",
            Self::InvalidScheme => "invalid scheme",
            Self::MissingHost => "missing host",
            Self::InvalidHost => "invalid host",
            Self::InvalidPort => "invalid port",
            Self::NotSpecial => "not a special (network) URL",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for UrlError {}

/// A parsed absolute URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    scheme: String,
    username: String,
    password: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    path: Vec<String>,
    query: Option<String>,
    fragment: Option<String>,
    special: bool,
}

impl Url {
    /// Parses an absolute URL string.
    pub fn parse(input: &str) -> Result<Self, UrlError> {
        parser::parse(input)
    }

    /// Parses `input` relative to `base`, following the reference resolution steps
    /// of the WHATWG URL standard.
    pub fn parse_with_base(input: &str, base: &Self) -> Result<Self, UrlError> {
        parser::parse_with_base(input, base)
    }

    /// Resolves a relative reference against this URL.
    pub fn join(&self, input: &str) -> Result<Self, UrlError> {
        Self::parse_with_base(input, self)
    }

    /// The URL scheme, lowercased, without the trailing colon.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Username component (already percent-decoded).
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Password component, if present.
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// Host component (domain, IPv4/IPv6 literal), if any.
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// Explicit port, if the URL contained one.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// The port that a connection should use, honouring scheme defaults.
    pub fn port_or_default(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    /// Path segments, without slashes.
    pub fn path_segments(&self) -> &[String] {
        &self.path
    }

    /// Serialised path, always starting with `/` for hierarchical URLs.
    pub fn path(&self) -> String {
        let mut out = String::new();
        for seg in &self.path {
            out.push('/');
            out.push_str(seg);
        }
        if out.is_empty() {
            out.push('/');
        }
        out
    }

    /// Raw query string, without the leading `?`.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Raw fragment, without the leading `#`.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// True when the scheme is one of the special schemes.
    pub fn is_special(&self) -> bool {
        self.special
    }

    /// True when the URL can be requested over the network by this engine.
    pub fn is_network(&self) -> bool {
        matches!(self.scheme.as_str(), "http" | "https")
    }

    /// The origin of this URL, or an opaque origin.
    pub fn origin(&self) -> Origin {
        match (&self.host, self.port_or_default()) {
            (Some(host), port) if self.is_special() => {
                Origin::tuple(&self.scheme, host, port.unwrap_or(0))
            }
            _ => Origin::opaque(),
        }
    }

    /// The request target used in an HTTP request line: `path[?query]`.
    pub fn request_target(&self) -> String {
        let mut target = self.path();
        if let Some(q) = &self.query {
            target.push('?');
            target.push_str(q);
        }
        target
    }

    /// The `Host` header value, including a non-default port.
    pub fn host_header(&self) -> Option<String> {
        let host = self.host.as_ref()?;
        let mut out = host.clone();
        if let (Some(port), Some(default)) = (self.port, default_port(&self.scheme)) {
            if port != default {
                out.push(':');
                out.push_str(&port.to_string());
            }
        }
        Some(out)
    }

    /// The serialised URL.
    pub fn as_str(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.scheme);
        out.push(':');
        if self.host.is_some() || self.special {
            out.push_str("//");
        }
        if !self.username.is_empty() {
            out.push_str(&self.username);
            if let Some(pw) = &self.password {
                out.push(':');
                out.push_str(pw);
            }
            out.push('@');
        }
        if let Some(host) = &self.host {
            out.push_str(host);
        }
        if let (Some(port), Some(default)) = (self.port, default_port(&self.scheme)) {
            if port != default {
                out.push(':');
                out.push_str(&port.to_string());
            }
        }
        out.push_str(&self.path());
        if let Some(q) = &self.query {
            out.push('?');
            out.push_str(q);
        }
        if let Some(f) = &self.fragment {
            out.push('#');
            out.push_str(f);
        }
        out
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl Url {
    pub(crate) fn empty(scheme: &str, special: bool) -> Self {
        Self {
            scheme: scheme.to_string(),
            username: String::new(),
            password: None,
            host: None,
            port: None,
            path: Vec::new(),
            query: None,
            fragment: None,
            special,
        }
    }

    pub(crate) fn set_username(&mut self, value: &str) {
        self.username = value.to_string();
    }

    pub(crate) fn set_password(&mut self, value: Option<String>) {
        self.password = value;
    }

    pub(crate) fn set_host(&mut self, value: Option<&str>) {
        self.host = value.map(ToOwned::to_owned);
    }

    pub(crate) fn set_port(&mut self, value: Option<u16>) {
        self.port = value;
    }

    pub(crate) fn set_path(&mut self, value: Vec<String>) {
        self.path = value;
    }

    pub(crate) fn set_query(&mut self, value: Option<String>) {
        self.query = value;
    }

    pub(crate) fn set_fragment(&mut self, value: Option<String>) {
        self.fragment = value;
    }

    pub(crate) fn is_empty_path(&self) -> bool {
        self.path.is_empty()
    }
}

/// Default port for a scheme, if the scheme has one.
pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

/// Percent-encodes a string for use in a URL path, leaving unreserved
/// characters and existing `%XX` escapes untouched.
pub fn encode_path(input: &str) -> String {
    percent_encode(input, |b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'/'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            )
    })
}

/// Percent-encodes a string for use in a URL query or fragment.
pub fn encode_query(input: &str) -> String {
    percent_encode(input, |b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
                    | b'/'
                    | b'?'
            )
    })
}

/// Percent-encodes bytes that fail `keep`, leaving `%` untouched when it starts a
/// valid escape so already-encoded input round-trips.
pub fn percent_encode(input: &str, keep: impl Fn(u8) -> bool) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let valid_escape = b == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit();
        if valid_escape {
            out.push('%');
            i += 1;
        } else if keep(b) {
            out.push(b as char);
            i += 1;
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0xf));
            i += 1;
        }
    }
    out
}

const fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

/// Decodes `%XX` escapes. Invalid escapes are preserved verbatim.
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_http_url() {
        let url = Url::parse("HTTP://user:pw@example.com:8080/a/b?q=1#frag").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.username(), "user");
        assert_eq!(url.password(), Some("pw"));
        assert_eq!(url.host(), Some("example.com"));
        assert_eq!(url.port(), Some(8080));
        assert_eq!(url.path_segments(), ["a", "b"]);
        assert_eq!(url.query(), Some("q=1"));
        assert_eq!(url.fragment(), Some("frag"));
        assert_eq!(url.request_target(), "/a/b?q=1");
        assert_eq!(url.host_header().as_deref(), Some("example.com:8080"));
    }

    #[test]
    fn omits_default_port_when_serialising() {
        let url = Url::parse("https://example.com:443/").unwrap();
        assert_eq!(url.as_str(), "https://example.com/");
        assert_eq!(url.port_or_default(), Some(443));
    }

    #[test]
    fn normalises_dot_segments() {
        let url = Url::parse("https://example.com/a/./b/../c/").unwrap();
        assert_eq!(url.path(), "/a/c/");
    }

    #[test]
    fn resolves_relative_references() {
        let base = Url::parse("https://example.com/a/b/c.html").unwrap();
        assert_eq!(
            base.join("d.html").unwrap().as_str(),
            "https://example.com/a/b/d.html"
        );
        assert_eq!(
            base.join("../x/./y").unwrap().as_str(),
            "https://example.com/a/x/y"
        );
        assert_eq!(
            base.join("/root").unwrap().as_str(),
            "https://example.com/root"
        );
        assert_eq!(
            base.join("//other.test/p").unwrap().as_str(),
            "https://other.test/p"
        );
        assert_eq!(base.join("?q=2").unwrap().query(), Some("q=2"));
        assert_eq!(base.join("#x").unwrap().fragment(), Some("x"));
        assert_eq!(
            base.join("http://plain.test/").unwrap().as_str(),
            "http://plain.test/"
        );
    }

    #[test]
    fn percent_encodes_spaces_and_keeps_escapes() {
        assert_eq!(encode_path("/a b%20c"), "/a%20b%20c");
        assert_eq!(percent_decode("a%20b%zz"), "a b%zz");
    }

    #[test]
    fn builds_origins() {
        let a = Url::parse("https://example.com/x").unwrap();
        let b = Url::parse("https://example.com:443/y").unwrap();
        let c = Url::parse("http://example.com/x").unwrap();
        assert_eq!(a.origin(), b.origin());
        assert_ne!(a.origin(), c.origin());
        assert_eq!(a.origin().to_string(), "https://example.com:443");
        assert_eq!(
            Url::parse("file:///tmp/x").unwrap().origin().is_opaque(),
            true
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(Url::parse("no-scheme"), Err(UrlError::MissingScheme));
        assert_eq!(Url::parse("1bad://x"), Err(UrlError::InvalidScheme));
        assert_eq!(Url::parse("http://"), Err(UrlError::MissingHost));
        assert_eq!(Url::parse("http://host:99999/"), Err(UrlError::InvalidPort));
    }

    #[test]
    fn file_urls_keep_empty_host() {
        let url = Url::parse("file:///tmp/test.html").unwrap();
        assert_eq!(url.host(), None);
        assert_eq!(url.path(), "/tmp/test.html");
        assert!(!url.is_network());
    }
}
