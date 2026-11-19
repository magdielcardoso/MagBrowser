//! HTTP response representation and content decoding helpers.

use browser_url::Url;

use crate::headers::Headers;

/// A parsed `Set-Cookie` header: name, value and raw attribute list.
pub type SetCookie<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>);

/// Character encodings understood when turning a response body into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// UTF-8 (also the fallback when no supported charset is declared).
    Utf8,
    /// ISO-8859-1 / Latin-1.
    Iso88591,
    /// Windows-1252 (CP1252).
    Windows1252,
}

impl Charset {
    /// Parses a `charset` parameter value, returning `None` for unknown names.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
        match name.as_str() {
            "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some(Self::Utf8),
            "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "latin1" | "latin-1" | "l1" | "cp819"
            | "ibm819" => Some(Self::Iso88591),
            "windows-1252" | "windows_1252" | "cp1252" | "x-cp1252" => Some(Self::Windows1252),
            _ => None,
        }
    }

    /// Canonical name of the encoding.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Iso88591 => "iso-8859-1",
            Self::Windows1252 => "windows-1252",
        }
    }
}

/// Undefined Windows-1252 bytes map to the Unicode replacement character.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{0192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02c6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{fffd}', '\u{017d}', '\u{fffd}',
    '\u{fffd}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02dc}', '\u{2122}', '\u{0161}', '\u{203a}', '\u{0153}', '\u{fffd}', '\u{017e}', '\u{0178}',
];

/// A fully read HTTP response.
#[derive(Debug, Clone)]
pub struct Response {
    /// Final URL after redirects.
    pub url: Url,
    /// Numeric status code.
    pub status: u16,
    /// Reason phrase from the status line (may be empty for HTTP/2 style responses).
    pub reason: String,
    /// Response headers.
    pub headers: Headers,
    /// Decoded body bytes (chunked transfer encoding already removed).
    pub body: Vec<u8>,
}

impl Response {
    /// Builds a response.
    pub fn new(
        url: Url,
        status: u16,
        reason: impl Into<String>,
        headers: Headers,
        body: Vec<u8>,
    ) -> Self {
        Self {
            url,
            status,
            reason: reason.into(),
            headers,
            body,
        }
    }

    /// Builds an empty error response used when a request fails before headers arrive.
    pub fn network_error(url: Url, status: u16, message: &str) -> Self {
        let mut headers = Headers::new();
        headers.insert("Content-Type", "text/plain; charset=utf-8");
        Self {
            url,
            status,
            reason: message.to_string(),
            headers,
            body: message.as_bytes().to_vec(),
        }
    }

    /// True for 2xx status codes.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// True when the response asks the client to follow a redirect.
    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308) && self.headers.contains("Location")
    }

    /// The `Content-Type` media type without parameters, lowercased.
    pub fn media_type(&self) -> Option<String> {
        self.headers.get("Content-Type").map(|ct| {
            ct.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
    }

    /// The `charset` parameter of `Content-Type`, if present (lower-cased).
    pub fn charset(&self) -> Option<String> {
        let ct = self.headers.get("Content-Type")?;
        for part in ct.split(';').skip(1) {
            let part = part.trim();
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("charset") {
                return Some(value.trim().trim_matches('"').to_ascii_lowercase());
            }
        }
        None
    }

    /// Detects the body charset from `Content-Type`.
    ///
    /// Recognises UTF-8, ISO-8859-1 and Windows-1252 (including common alias
    /// names and mixed casing). Unknown or missing charsets fall back to UTF-8,
    /// which is the HTML default and the safest choice for modern sites.
    pub fn detected_charset(&self) -> Charset {
        self.charset()
            .as_deref()
            .and_then(Charset::from_name)
            .unwrap_or(Charset::Utf8)
    }

    /// Decodes the body as text using an explicit charset.
    pub fn decode_text(&self, charset: Charset) -> String {
        match charset {
            Charset::Utf8 => String::from_utf8_lossy(&self.body).into_owned(),
            Charset::Iso88591 => self.body.iter().map(|byte| char::from(*byte)).collect(),
            Charset::Windows1252 => self
                .body
                .iter()
                .map(|byte| match byte {
                    0x80..=0x9f => WINDOWS_1252_HIGH[(byte - 0x80) as usize],
                    other => char::from(*other),
                })
                .collect(),
        }
    }

    /// Decodes the body as text using the detected charset.
    pub fn text(&self) -> String {
        self.decode_text(self.detected_charset())
    }

    /// The `ETag` validator, trimmed for use in `If-None-Match`.
    pub fn etag(&self) -> Option<&str> {
        self.headers.get("ETag").map(str::trim)
    }

    /// The `Content-Encoding` header, if present.
    pub fn content_encoding(&self) -> Option<&str> {
        self.headers.get("Content-Encoding")
    }

    /// Parses `Set-Cookie` headers into `(name, value, attributes)` triples.
    pub fn set_cookies(&self) -> Vec<SetCookie<'_>> {
        let mut out = Vec::new();
        for raw in self.headers.get_all("Set-Cookie") {
            let mut parts = raw.split(';');
            let Some(first) = parts.next() else { continue };
            let Some((name, value)) = first.split_once('=') else {
                continue;
            };
            let mut attrs = Vec::new();
            for attr in parts {
                let attr = attr.trim();
                if attr.is_empty() {
                    continue;
                }
                match attr.split_once('=') {
                    Some((k, v)) => attrs.push((k.trim(), v.trim())),
                    None => attrs.push((attr, "")),
                }
            }
            out.push((name.trim(), value.trim(), attrs));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with(headers: Headers, body: &[u8]) -> Response {
        Response::new(
            Url::parse("http://example.com/").unwrap(),
            200,
            "OK",
            headers,
            body.to_vec(),
        )
    }

    #[test]
    fn parses_content_type_and_charset() {
        let mut h = Headers::new();
        h.insert("Content-Type", "Text/HTML; charset=UTF-8");
        let r = response_with(h, b"<p>hi</p>");
        assert_eq!(r.media_type().as_deref(), Some("text/html"));
        assert_eq!(r.charset().as_deref(), Some("utf-8"));
        assert_eq!(r.text(), "<p>hi</p>");
    }

    #[test]
    fn decodes_latin1() {
        let mut h = Headers::new();
        h.insert("Content-Type", "text/plain; charset=iso-8859-1");
        let r = response_with(h, &[0x63, 0x61, 0x66, 0xe9]);
        assert_eq!(r.text(), "café");
    }

    #[test]
    fn detects_redirects() {
        let mut h = Headers::new();
        h.insert("Location", "/next");
        let mut r = response_with(h, b"");
        r.status = 302;
        assert!(r.is_redirect());
        r.status = 200;
        assert!(!r.is_redirect());
    }

    #[test]
    fn detects_charset_aliases_and_casing() {
        let mut h = Headers::new();
        h.insert("Content-Type", "text/plain; Charset=\"Windows-1252\"");
        let r = response_with(h, b"");
        assert_eq!(r.charset().as_deref(), Some("windows-1252"));
        assert_eq!(r.detected_charset(), Charset::Windows1252);

        let mut h = Headers::new();
        h.insert("Content-Type", "application/json");
        let r = response_with(h, b"");
        assert_eq!(r.detected_charset(), Charset::Utf8);
    }

    #[test]
    fn decodes_windows_1252_special_bytes() {
        let mut h = Headers::new();
        h.insert("Content-Type", "text/html; charset=cp1252");
        let r = response_with(h, &[0x93, 0x48, 0x69, 0x94, 0x20, 0x80, 0x81]);
        assert_eq!(r.text(), "\u{201c}Hi\u{201d} \u{20ac}\u{fffd}");
    }

    #[test]
    fn exposes_etag_and_content_encoding() {
        let mut h = Headers::new();
        h.insert("ETag", "  \"v1\"  ");
        h.insert("Content-Encoding", "gzip");
        let r = response_with(h, b"");
        assert_eq!(r.etag(), Some("\"v1\""));
        assert_eq!(r.content_encoding(), Some("gzip"));
    }

    #[test]
    fn parses_set_cookie() {
        let mut h = Headers::new();
        h.append("Set-Cookie", "session=abc123; Path=/; HttpOnly; Secure");
        let r = response_with(h, b"");
        let cookies = r.set_cookies();
        assert_eq!(cookies[0].0, "session");
        assert_eq!(cookies[0].1, "abc123");
        assert!(cookies[0].2.contains(&("HttpOnly", "")));
    }
}
