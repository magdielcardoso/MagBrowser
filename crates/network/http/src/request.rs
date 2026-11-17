//! HTTP request representation.

use browser_url::Url;

use crate::headers::Headers;

/// HTTP methods understood by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`
    Get,
    /// `HEAD`
    Head,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `DELETE`
    Delete,
    /// `PATCH`
    Patch,
    /// `OPTIONS`
    Options,
    /// `CONNECT`
    Connect,
    /// `TRACE`
    Trace,
}

impl Method {
    /// The wire representation of the method.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Options => "OPTIONS",
            Self::Connect => "CONNECT",
            Self::Trace => "TRACE",
        }
    }

    /// Parses a method token, falling back to `GET` for unknown tokens.
    pub fn parse(input: &str) -> Self {
        match input.to_ascii_uppercase().as_str() {
            "HEAD" => Self::Head,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "DELETE" => Self::Delete,
            "PATCH" => Self::Patch,
            "OPTIONS" => Self::Options,
            "CONNECT" => Self::Connect,
            "TRACE" => Self::Trace,
            _ => Self::Get,
        }
    }

    /// True when the method is considered safe (no side effects).
    pub fn is_safe(self) -> bool {
        matches!(self, Self::Get | Self::Head | Self::Options | Self::Trace)
    }
}

/// An HTTP request.
#[derive(Debug, Clone)]
pub struct Request {
    /// Request method.
    pub method: Method,
    /// Absolute request URL.
    pub url: Url,
    /// Request headers.
    pub headers: Headers,
    /// Request body.
    pub body: Vec<u8>,
}

impl Request {
    /// Builds a `GET` request for `url`.
    pub fn get(url: Url) -> Self {
        Self {
            method: Method::Get,
            url,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    /// Builds a `GET` request from a URL string.
    pub fn get_str(url: &str) -> Result<Self, browser_url::UrlError> {
        Ok(Self::get(Url::parse(url)?))
    }

    /// Builds a request with an explicit method and body.
    pub fn with_body(method: Method, url: Url, body: Vec<u8>) -> Self {
        Self {
            method,
            url,
            headers: Headers::new(),
            body,
        }
    }

    /// Adds a header, replacing existing values.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Serialises the request head (request line plus headers).
    pub fn to_wire(&self) -> String {
        let mut out = String::new();
        out.push_str(self.method.as_str());
        out.push(' ');
        out.push_str(&self.url.request_target());
        out.push_str(" HTTP/1.1\r\n");
        for (name, value) in self.headers.iter() {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_request_head() {
        let req = Request::get_str("http://example.com/a?b=1")
            .unwrap()
            .header("Host", "example.com")
            .header("Accept", "*/*");
        let wire = req.to_wire();
        assert!(wire.starts_with("GET /a?b=1 HTTP/1.1\r\n"));
        assert!(wire.contains("Host: example.com\r\n"));
        assert!(wire.ends_with("\r\n\r\n"));
    }

    #[test]
    fn parses_methods() {
        assert_eq!(Method::parse("post"), Method::Post);
        assert_eq!(Method::parse("weird"), Method::Get);
        assert!(Method::Get.is_safe());
        assert!(!Method::Put.is_safe());
    }
}
