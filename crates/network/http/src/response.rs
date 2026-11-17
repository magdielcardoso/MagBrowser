//! HTTP response representation and content decoding helpers.

use browser_url::Url;

use crate::headers::Headers;

/// A parsed `Set-Cookie` header: name, value and raw attribute list.
pub type SetCookie<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>);

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

    /// The `charset` parameter of `Content-Type`, if present.
    pub fn charset(&self) -> Option<String> {
        let ct = self.headers.get("Content-Type")?;
        for part in ct.split(';').skip(1) {
            let part = part.trim();
            if let Some(value) = part
                .strip_prefix("charset=")
                .or_else(|| part.strip_prefix("CHARSET="))
            {
                return Some(value.trim_matches('"').to_ascii_lowercase());
            }
        }
        None
    }

    /// Decodes the body as text.
    ///
    /// UTF-8 and ASCII are decoded properly; `latin1` maps bytes to code points.
    /// Other charsets fall back to lossy UTF-8 and the limitation is documented
    /// in `docs/architecture.md`.
    pub fn text(&self) -> String {
        match self.charset().as_deref() {
            Some("iso-8859-1" | "latin1" | "windows-1252") => {
                self.body.iter().map(|b| char::from(*b)).collect::<String>()
            }
            _ => String::from_utf8_lossy(&self.body).into_owned(),
        }
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
