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

/// Browser-emulation defaults applied to every outgoing request.
///
/// These values mirror the headers a current Chrome on macOS sends for a
/// top-level navigation. `ClientConfig` owns one of these; the values are only
/// written when the caller has not already supplied that header on the
/// `Request`, so per-request overrides continue to work.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestConfig {
    /// `Accept` header value.
    pub accept: String,
    /// `Accept-Language` header value.
    pub accept_language: String,
    /// `Accept-Encoding` header value. Only codings decoded in-process are
    /// advertised by default (`gzip, deflate`).
    pub accept_encoding: String,
    /// `Sec-Fetch-Dest` value.
    pub sec_fetch_dest: String,
    /// `Sec-Fetch-Mode` value.
    pub sec_fetch_mode: String,
    /// `Sec-Fetch-Site` value.
    pub sec_fetch_site: String,
    /// `Sec-Fetch-User` is `?1` when true and `?0` when false.
    pub sec_fetch_user: bool,
    /// `Cache-Control` value sent with document requests; `None` omits it.
    /// Chrome sends `max-age=0` on a normal top-level navigation.
    pub cache_control: Option<String>,
    /// Sends `Upgrade-Insecure-Requests: 1` when true.
    pub upgrade_insecure_requests: bool,
    /// `sec-ch-ua` client hint.
    pub sec_ch_ua: String,
    /// `sec-ch-ua-mobile` client hint (`?1` when true, `?0` when false).
    pub sec_ch_ua_mobile: bool,
    /// `sec-ch-ua-platform` client hint.
    pub sec_ch_ua_platform: String,
    /// Viewport width in CSS pixels, reported as `sec-ch-viewport-width`.
    pub viewport_width: u32,
    /// Device memory in gigabytes, reported as `sec-ch-device-memory`.
    pub device_memory: u32,
    /// Device pixel ratio, reported as `sec-ch-dpr`.
    pub dpr: f32,
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            accept: "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
                .to_string(),
            accept_language: "pt-BR,pt;q=0.9,en;q=0.8".to_string(),
            accept_encoding: "gzip, deflate".to_string(),
            sec_fetch_dest: "document".to_string(),
            sec_fetch_mode: "navigate".to_string(),
            sec_fetch_site: "none".to_string(),
            sec_fetch_user: true,
            cache_control: Some("max-age=0".to_string()),
            upgrade_insecure_requests: true,
            sec_ch_ua: "\"Google Chrome\";v=\"131\", \"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\""
                .to_string(),
            sec_ch_ua_mobile: false,
            sec_ch_ua_platform: "\"macOS\"".to_string(),
            viewport_width: 1280,
            device_memory: 8,
            dpr: 1.0,
        }
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

    #[test]
    fn request_config_defaults_match_chrome_navigation() {
        let config = RequestConfig::default();
        assert_eq!(
            config.accept,
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
        );
        assert_eq!(config.accept_language, "pt-BR,pt;q=0.9,en;q=0.8");
        assert_eq!(config.accept_encoding, "gzip, deflate");
        assert_eq!(config.sec_fetch_dest, "document");
        assert_eq!(config.sec_fetch_mode, "navigate");
        assert_eq!(config.sec_fetch_site, "none");
        assert!(config.sec_fetch_user);
        assert_eq!(config.cache_control.as_deref(), Some("max-age=0"));
        assert!(config.upgrade_insecure_requests);
        assert_eq!(
            config.sec_ch_ua,
            "\"Google Chrome\";v=\"131\", \"Chromium\";v=\"131\", \"Not_A Brand\";v=\"24\""
        );
        assert!(!config.sec_ch_ua_mobile);
        assert_eq!(config.sec_ch_ua_platform, "\"macOS\"");
        assert_eq!(config.viewport_width, 1280);
        assert_eq!(config.device_memory, 8);
        assert_eq!(config.dpr, 1.0);
    }
}
