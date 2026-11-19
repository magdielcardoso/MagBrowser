//! Cross-Origin Resource Sharing request and response evaluation.
//!
//! The implementation follows the Fetch CORS protocol for the cases the engine
//! currently performs: simple requests, preflight detection, `Access-Control-
//! Allow-Origin` matching and preflight method/header/credential checks.
//!
//! Response parsing is tolerant of the shapes real servers send: repeated
//! `Access-Control-Allow-Methods` / `Access-Control-Allow-Headers` lines are
//! combined, `Access-Control-Max-Age` is clamped to a browser-sized cache window,
//! and `Access-Control-Expose-Headers` / `Vary: Origin` can be inspected by the
//! caller. It stays strict where ambiguity could leak data: an ambiguous
//! `Access-Control-Allow-Origin`, a wildcard with credentials, and header values
//! containing CORS-unsafe bytes are all rejected.
//!
//! Credentials are inferred from the presence of `Cookie` or `Authorization` on
//! the request. Hosts that track CORS credentials mode separately should call
//! [`evaluate_response_with_credentials`].

use std::fmt;

use crate::origin::OriginHeader;

/// A request that may need CORS approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorsRequest {
    /// Serialised request origin.
    pub origin: String,
    /// Request method (`GET`, `PUT`, `OPTIONS`, ...).
    pub method: String,
    /// Request headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
}

impl CorsRequest {
    /// Creates a request with no headers.
    pub fn new(origin: &str, method: &str) -> Self {
        Self {
            origin: origin.to_string(),
            method: method.to_string(),
            headers: Vec::new(),
        }
    }

    /// Adds a header, returning the updated request.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `Access-Control-Request-Method` value, if present.
    pub fn requested_method(&self) -> Option<&str> {
        self.header("access-control-request-method")
    }

    /// The `Access-Control-Request-Headers` list, split and trimmed.
    pub fn requested_headers(&self) -> Vec<&str> {
        self.header("access-control-request-headers")
            .map_or_else(Vec::new, parse_header_list)
    }

    /// True when the request includes credentials.
    pub fn credentials(&self) -> bool {
        self.header("cookie").is_some() || self.header("authorization").is_some()
    }

    /// True when the method is CORS-safelisted.
    pub fn is_simple_method(&self) -> bool {
        is_simple_method(&self.method)
    }

    /// Classifies the request as simple or preflighted.
    pub fn kind(&self) -> RequestKind {
        if !self.is_simple_method() || self.requested_method().is_some() {
            return RequestKind::Preflight;
        }
        if self
            .headers
            .iter()
            .any(|(name, value)| !is_managed_header(name) && !is_simple_header(name, value))
        {
            return RequestKind::Preflight;
        }
        RequestKind::Simple
    }
}

/// A response to a CORS request or preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorsResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
}

impl CorsResponse {
    /// Creates a response with a status and no headers.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
        }
    }

    /// Adds a header, returning the updated response.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Every value of a header name, in the order the headers were added.
    ///
    /// Real servers sometimes split `Access-Control-Allow-Methods` and
    /// `Access-Control-Allow-Headers` over several header lines; CORS checks use
    /// the union of those lines.
    pub fn headers_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// Parsed `Access-Control-Max-Age` in seconds.
    ///
    /// Multiple values take the largest one, and the result is capped at two
    /// hours, matching common browser preflight caches. Malformed values are
    /// ignored so a bad header cannot poison the cache.
    pub fn max_age(&self) -> Option<u64> {
        const MAX_PREFLIGHT_CACHE_SECONDS: u64 = 7200;
        self.headers_all("access-control-max-age")
            .into_iter()
            .filter_map(|value| value.trim().parse::<u64>().ok())
            .max()
            .map(|age| age.min(MAX_PREFLIGHT_CACHE_SECONDS))
    }

    /// Lowercased names from every `Access-Control-Expose-Headers` header.
    ///
    /// A `*` entry is preserved so callers can implement their own wildcard rule.
    pub fn exposed_headers(&self) -> Vec<String> {
        self.headers_all("access-control-expose-headers")
            .into_iter()
            .flat_map(parse_header_list)
            .map(str::to_ascii_lowercase)
            .collect()
    }

    /// True when any `Vary` header lists `Origin`.
    ///
    /// This is informational: callers should still key their CORS cache by origin.
    pub fn varies_on_origin(&self) -> bool {
        self.headers_all("vary")
            .into_iter()
            .flat_map(parse_header_list)
            .any(|name| name.eq_ignore_ascii_case("origin"))
    }
}

/// Splits a comma-separated header list into trimmed, non-empty entries.
pub fn parse_header_list(value: &str) -> Vec<&str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect()
}

/// Whether a CORS request can be sent without a preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// A CORS-safelisted request.
    Simple,
    /// A request that requires an `OPTIONS` preflight.
    Preflight,
}

/// The result of evaluating a CORS request or response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorsDecision {
    /// The request/response is allowed.
    Allow,
    /// The exchange is blocked for the given reason.
    Deny(CorsError),
}

impl CorsDecision {
    /// True for [`CorsDecision::Allow`].
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Reasons a CORS exchange can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorsError {
    /// The request origin header is malformed.
    InvalidOrigin(String),
    /// The request origin is opaque (`null`).
    OpaqueOrigin,
    /// The request method is empty.
    InvalidMethod(String),
    /// The response has no `Access-Control-Allow-Origin`.
    MissingAllowOrigin,
    /// `Access-Control-Allow-Origin` is malformed.
    InvalidAllowOrigin(String),
    /// The response does not allow the request origin.
    OriginNotAllowed,
    /// `Access-Control-Allow-Origin: *` was used with credentials.
    WildcardWithCredentials,
    /// The preflight response has no `Access-Control-Allow-Methods`.
    MissingAllowMethods,
    /// The preflight response does not allow the requested method.
    MethodNotAllowed(String),
    /// `Access-Control-Allow-Methods: *` was used with credentials.
    WildcardMethodWithCredentials,
    /// The preflight response has no `Access-Control-Allow-Headers`.
    MissingAllowHeaders,
    /// The preflight response does not allow a requested header.
    HeaderNotAllowed(String),
    /// `Access-Control-Allow-Headers: *` was used with credentials.
    WildcardHeaderWithCredentials,
    /// Credentials were used but `Access-Control-Allow-Credentials: true` is missing.
    CredentialsNotAllowed,
    /// The preflight response did not have a 2xx status.
    PreflightStatus(u16),
}

impl fmt::Display for CorsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOrigin(message) => write!(f, "invalid request origin: {message}"),
            Self::OpaqueOrigin => f.write_str("opaque origins are not allowed for CORS"),
            Self::InvalidMethod(method) => write!(f, "invalid request method: {method:?}"),
            Self::MissingAllowOrigin => f.write_str("missing Access-Control-Allow-Origin header"),
            Self::InvalidAllowOrigin(value) => {
                write!(f, "invalid Access-Control-Allow-Origin: {value:?}")
            }
            Self::OriginNotAllowed => f.write_str("request origin is not allowed"),
            Self::WildcardWithCredentials => {
                f.write_str("wildcard Access-Control-Allow-Origin with credentials")
            }
            Self::MissingAllowMethods => f.write_str("missing Access-Control-Allow-Methods header"),
            Self::MethodNotAllowed(method) => write!(f, "method not allowed: {method}"),
            Self::WildcardMethodWithCredentials => {
                f.write_str("wildcard Access-Control-Allow-Methods with credentials")
            }
            Self::MissingAllowHeaders => f.write_str("missing Access-Control-Allow-Headers header"),
            Self::HeaderNotAllowed(name) => write!(f, "header not allowed: {name}"),
            Self::WildcardHeaderWithCredentials => {
                f.write_str("wildcard Access-Control-Allow-Headers with credentials")
            }
            Self::CredentialsNotAllowed => f.write_str("credentials not allowed by the response"),
            Self::PreflightStatus(status) => {
                write!(f, "preflight response status {status} is not successful")
            }
        }
    }
}

impl std::error::Error for CorsError {}

/// True for `GET`, `HEAD` and `POST`.
pub fn is_simple_method(method: &str) -> bool {
    method.eq_ignore_ascii_case("GET")
        || method.eq_ignore_ascii_case("HEAD")
        || method.eq_ignore_ascii_case("POST")
}

/// True for the CORS-safelisted request headers.
///
/// Header values containing CORS-unsafe bytes (control characters and a handful
/// of delimiters) are rejected instead of being treated as safelisted. `Range` is
/// safelisted only for a single, well-formed byte range, as required by Fetch.
pub fn is_simple_header(name: &str, value: &str) -> bool {
    if has_cors_unsafe_bytes(value) {
        return false;
    }
    if name.eq_ignore_ascii_case("accept")
        || name.eq_ignore_ascii_case("accept-language")
        || name.eq_ignore_ascii_case("content-language")
    {
        return true;
    }
    if name.eq_ignore_ascii_case("content-type") {
        let media_type = value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        return matches!(
            media_type.as_str(),
            "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
        );
    }
    if name.eq_ignore_ascii_case("range") {
        return is_simple_range(value);
    }
    false
}

/// Bytes Fetch forbids in a CORS-safelisted request-header value.
fn has_cors_unsafe_bytes(value: &str) -> bool {
    value.bytes().any(|byte| {
        matches!(
            byte,
            0x00..=0x08
                | 0x0A..=0x0C
                | 0x0E..=0x1F
                | 0x7F
                | b'"'
                | b'('
                | b')'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b'\\'
                | b']'
                | b'{'
                | b'}'
        )
    })
}

/// True for a single byte range such as `bytes=0-` or `bytes=100-999`.
fn is_simple_range(value: &str) -> bool {
    let Some(range) = value.strip_prefix("bytes=") else {
        return false;
    };
    let Some((start, end)) = range.split_once('-') else {
        return false;
    };
    if start.is_empty() || !start.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if end.is_empty() {
        return true;
    }
    if !end.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(start) = start.parse::<u64>() else {
        return false;
    };
    let Ok(end) = end.parse::<u64>() else {
        return false;
    };
    end >= start
}

/// Headers set by the user agent that do not count as author request headers and
/// therefore never trigger a preflight.
fn is_managed_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cookie"
            | "origin"
            | "host"
            | "referer"
            | "user-agent"
            | "accept-encoding"
            | "connection"
            | "content-length"
            | "transfer-encoding"
    )
}

/// Validates a request's origin and classifies it.
pub fn evaluate_request(request: &CorsRequest) -> Result<RequestKind, CorsError> {
    if request.method.trim().is_empty() {
        return Err(CorsError::InvalidMethod(request.method.clone()));
    }
    let header = OriginHeader::parse(&request.origin)
        .map_err(|error| CorsError::InvalidOrigin(error.to_string()))?;
    if header.is_opaque() {
        return Err(CorsError::OpaqueOrigin);
    }
    if header.origins().len() != 1 {
        return Err(CorsError::InvalidOrigin(
            "expected exactly one origin".to_string(),
        ));
    }
    Ok(request.kind())
}

/// Evaluates a response against a request, inferring credentials from headers.
pub fn evaluate_response(request: &CorsRequest, response: &CorsResponse) -> CorsDecision {
    evaluate_response_with_credentials(request, response, request.credentials())
}

/// Evaluates a response against a request with an explicit credentials mode.
pub fn evaluate_response_with_credentials(
    request: &CorsRequest,
    response: &CorsResponse,
    credentials: bool,
) -> CorsDecision {
    let kind = match evaluate_request(request) {
        Ok(kind) => kind,
        Err(error) => return CorsDecision::Deny(error),
    };

    if let Err(error) = check_allow_origin(request, response, credentials) {
        return CorsDecision::Deny(error);
    }
    if credentials
        && !response
            .header("access-control-allow-credentials")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
    {
        return CorsDecision::Deny(CorsError::CredentialsNotAllowed);
    }

    if kind == RequestKind::Preflight
        && let Err(error) = check_preflight(request, response, credentials)
    {
        return CorsDecision::Deny(error);
    }

    CorsDecision::Allow
}

fn check_allow_origin(
    request: &CorsRequest,
    response: &CorsResponse,
    credentials: bool,
) -> Result<(), CorsError> {
    let raw = response.headers_all("access-control-allow-origin");
    if raw.is_empty() {
        return Err(CorsError::MissingAllowOrigin);
    }
    let values: Vec<&str> = raw
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    let allowed = match values.as_slice() {
        [] => return Err(CorsError::InvalidAllowOrigin(String::new())),
        // A comma-separated value or multiple header lines are ambiguous: a
        // server may be reflecting more than one origin. Fail closed rather than
        // picking one.
        [value] if !value.contains(',') => *value,
        _ => {
            return Err(CorsError::InvalidAllowOrigin(
                response
                    .headers_all("access-control-allow-origin")
                    .join(", "),
            ));
        }
    };
    if allowed == "*" {
        return if credentials {
            Err(CorsError::WildcardWithCredentials)
        } else {
            Ok(())
        };
    }
    // An `Origin` serialisation never contains whitespace: a value with spaces
    // names several origins and must not be narrowed down to the first one.
    if allowed.split_ascii_whitespace().count() != 1 {
        return Err(CorsError::InvalidAllowOrigin(allowed.to_string()));
    }
    let allowed = OriginHeader::parse(allowed)
        .map_err(|_| CorsError::InvalidAllowOrigin(allowed.to_string()))?;
    let requested = OriginHeader::parse(&request.origin)
        .map_err(|error| CorsError::InvalidOrigin(error.to_string()))?;
    if allowed.same_origin(&requested) {
        Ok(())
    } else {
        Err(CorsError::OriginNotAllowed)
    }
}

fn check_preflight(
    request: &CorsRequest,
    response: &CorsResponse,
    credentials: bool,
) -> Result<(), CorsError> {
    if !(200..300).contains(&response.status) {
        return Err(CorsError::PreflightStatus(response.status));
    }

    let method = request
        .requested_method()
        .unwrap_or(request.method.as_str())
        .trim();
    let allowed_methods: Vec<&str> = response
        .headers_all("access-control-allow-methods")
        .into_iter()
        .flat_map(parse_header_list)
        .collect();
    if allowed_methods.is_empty() {
        return Err(CorsError::MissingAllowMethods);
    }
    let mut method_allowed = false;
    for candidate in allowed_methods {
        if candidate.eq_ignore_ascii_case(method) {
            method_allowed = true;
            break;
        }
        if candidate == "*" {
            if credentials {
                return Err(CorsError::WildcardMethodWithCredentials);
            }
            method_allowed = true;
        }
    }
    if !method_allowed {
        return Err(CorsError::MethodNotAllowed(method.to_string()));
    }

    let requested_headers = request.requested_headers();
    if requested_headers.is_empty() {
        return Ok(());
    }
    let allowed: Vec<String> = response
        .headers_all("access-control-allow-headers")
        .into_iter()
        .flat_map(parse_header_list)
        .map(str::to_ascii_lowercase)
        .collect();
    if allowed.is_empty() {
        return Err(CorsError::MissingAllowHeaders);
    }
    let wildcard = allowed.iter().any(|name| name == "*");
    if wildcard && credentials {
        return Err(CorsError::WildcardHeaderWithCredentials);
    }
    for header in requested_headers {
        let lower = header.to_ascii_lowercase();
        if !allowed.iter().any(|name| name == &lower) && !wildcard {
            return Err(CorsError::HeaderNotAllowed(header.to_string()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://app.example";

    fn simple_request() -> CorsRequest {
        CorsRequest::new(ORIGIN, "GET")
    }

    fn allow_response() -> CorsResponse {
        CorsResponse::new(200).with_header("Access-Control-Allow-Origin", ORIGIN)
    }

    #[test]
    fn simple_request_with_echoed_origin_is_allowed() {
        let request = simple_request();
        assert_eq!(request.kind(), RequestKind::Simple);
        assert_eq!(
            evaluate_response(&request, &allow_response()),
            CorsDecision::Allow
        );
    }

    #[test]
    fn wildcard_allows_request_without_credentials() {
        let response = CorsResponse::new(200).with_header("Access-Control-Allow-Origin", "*");
        assert!(evaluate_response(&simple_request(), &response).is_allowed());
    }

    #[test]
    fn wildcard_is_rejected_with_credentials() {
        let request = simple_request().with_header("Cookie", "session=1");
        let response = CorsResponse::new(200).with_header("Access-Control-Allow-Origin", "*");
        assert_eq!(
            evaluate_response(&request, &response),
            CorsDecision::Deny(CorsError::WildcardWithCredentials)
        );
    }

    #[test]
    fn missing_allow_origin_is_denied() {
        let response = CorsResponse::new(200);
        assert_eq!(
            evaluate_response(&simple_request(), &response),
            CorsDecision::Deny(CorsError::MissingAllowOrigin)
        );
    }

    #[test]
    fn mismatched_allow_origin_is_denied() {
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", "https://other.example");
        assert_eq!(
            evaluate_response(&simple_request(), &response),
            CorsDecision::Deny(CorsError::OriginNotAllowed)
        );
    }

    #[test]
    fn request_origin_must_be_a_valid_tuple_origin() {
        let request = CorsRequest::new("not a url", "GET");
        assert!(matches!(
            evaluate_request(&request),
            Err(CorsError::InvalidOrigin(_))
        ));
        assert!(matches!(
            evaluate_response(&request, &allow_response()),
            CorsDecision::Deny(CorsError::InvalidOrigin(_))
        ));
    }

    #[test]
    fn opaque_null_origin_is_rejected() {
        let request = CorsRequest::new("null", "GET");
        assert_eq!(evaluate_request(&request), Err(CorsError::OpaqueOrigin));
    }

    #[test]
    fn empty_method_is_rejected() {
        let request = CorsRequest::new(ORIGIN, "  ");
        assert!(matches!(
            evaluate_request(&request),
            Err(CorsError::InvalidMethod(_))
        ));
    }

    #[test]
    fn non_simple_method_triggers_preflight() {
        assert_eq!(
            CorsRequest::new(ORIGIN, "PUT").kind(),
            RequestKind::Preflight
        );
        assert_eq!(
            CorsRequest::new(ORIGIN, "DELETE").kind(),
            RequestKind::Preflight
        );
    }

    #[test]
    fn json_content_type_triggers_preflight() {
        let request =
            CorsRequest::new(ORIGIN, "POST").with_header("Content-Type", "application/json");
        assert_eq!(request.kind(), RequestKind::Preflight);
    }

    #[test]
    fn safelisted_content_type_is_simple() {
        let request = CorsRequest::new(ORIGIN, "POST")
            .with_header("Content-Type", "text/plain; charset=utf-8");
        assert_eq!(request.kind(), RequestKind::Simple);
    }

    #[test]
    fn user_agent_headers_do_not_trigger_preflight() {
        let request = simple_request()
            .with_header("Cookie", "a=1")
            .with_header("Origin", ORIGIN)
            .with_header("User-Agent", "Mag/0.1")
            .with_header("Accept-Language", "en");
        assert_eq!(request.kind(), RequestKind::Simple);
    }

    #[test]
    fn preflight_detected_by_request_method_header() {
        let request = CorsRequest::new(ORIGIN, "OPTIONS")
            .with_header("Access-Control-Request-Method", "PATCH");
        assert_eq!(request.kind(), RequestKind::Preflight);
        assert_eq!(request.requested_method(), Some("PATCH"));
    }

    #[test]
    fn successful_preflight_with_method_headers_and_credentials() {
        let request = CorsRequest::new(ORIGIN, "OPTIONS")
            .with_header("Access-Control-Request-Method", "PUT")
            .with_header("Access-Control-Request-Headers", "X-Token, Content-Type");
        let response = CorsResponse::new(204)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "GET, PUT")
            .with_header("Access-Control-Allow-Headers", "x-token, content-type")
            .with_header("Access-Control-Allow-Credentials", "true");
        assert_eq!(
            evaluate_response_with_credentials(&request, &response, true),
            CorsDecision::Allow
        );
        assert_eq!(
            evaluate_response_with_credentials(&request, &response, false),
            CorsDecision::Allow
        );
    }

    #[test]
    fn preflight_requires_successful_status() {
        let request =
            CorsRequest::new(ORIGIN, "PUT").with_header("Access-Control-Request-Method", "PUT");
        let response = CorsResponse::new(500)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "PUT");
        assert_eq!(
            evaluate_response(&request, &response),
            CorsDecision::Deny(CorsError::PreflightStatus(500))
        );
    }

    #[test]
    fn preflight_rejects_unlisted_method() {
        let request =
            CorsRequest::new(ORIGIN, "PUT").with_header("Access-Control-Request-Method", "PUT");
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "GET, POST");
        assert_eq!(
            evaluate_response(&request, &response),
            CorsDecision::Deny(CorsError::MethodNotAllowed("PUT".to_string()))
        );
    }

    #[test]
    fn preflight_requires_allow_methods_header() {
        let request =
            CorsRequest::new(ORIGIN, "PUT").with_header("Access-Control-Request-Method", "PUT");
        let response = CorsResponse::new(200).with_header("Access-Control-Allow-Origin", ORIGIN);
        assert_eq!(
            evaluate_response(&request, &response),
            CorsDecision::Deny(CorsError::MissingAllowMethods)
        );
    }

    #[test]
    fn preflight_rejects_unlisted_header() {
        let request = CorsRequest::new(ORIGIN, "PUT")
            .with_header("Access-Control-Request-Method", "PUT")
            .with_header("Access-Control-Request-Headers", "X-Token");
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "PUT")
            .with_header("Access-Control-Allow-Headers", "Content-Type");
        assert_eq!(
            evaluate_response(&request, &response),
            CorsDecision::Deny(CorsError::HeaderNotAllowed("X-Token".to_string()))
        );
    }

    #[test]
    fn preflight_requires_allow_headers_when_requested() {
        let request = CorsRequest::new(ORIGIN, "PUT")
            .with_header("Access-Control-Request-Method", "PUT")
            .with_header("Access-Control-Request-Headers", "X-Token");
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "PUT");
        assert_eq!(
            evaluate_response(&request, &response),
            CorsDecision::Deny(CorsError::MissingAllowHeaders)
        );
    }

    #[test]
    fn credentials_require_explicit_allow_credentials() {
        let request = simple_request().with_header("Cookie", "session=1");
        let response = allow_response();
        assert_eq!(
            evaluate_response(&request, &response),
            CorsDecision::Deny(CorsError::CredentialsNotAllowed)
        );
        let response = response.with_header("Access-Control-Allow-Credentials", "true");
        assert_eq!(evaluate_response(&request, &response), CorsDecision::Allow);
    }

    #[test]
    fn wildcard_methods_are_rejected_with_credentials() {
        let request =
            CorsRequest::new(ORIGIN, "PUT").with_header("Access-Control-Request-Method", "PUT");
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "*")
            .with_header("Access-Control-Allow-Credentials", "true");
        assert_eq!(
            evaluate_response_with_credentials(&request, &response, true),
            CorsDecision::Deny(CorsError::WildcardMethodWithCredentials)
        );
    }

    #[test]
    fn wildcard_headers_are_rejected_with_credentials() {
        let request = CorsRequest::new(ORIGIN, "PUT")
            .with_header("Access-Control-Request-Method", "PUT")
            .with_header("Access-Control-Request-Headers", "X-Token");
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "PUT")
            .with_header("Access-Control-Allow-Headers", "*")
            .with_header("Access-Control-Allow-Credentials", "true");
        assert_eq!(
            evaluate_response_with_credentials(&request, &response, true),
            CorsDecision::Deny(CorsError::WildcardHeaderWithCredentials)
        );
    }

    #[test]
    fn wildcard_headers_allow_any_header_without_credentials() {
        let request = CorsRequest::new(ORIGIN, "PUT")
            .with_header("Access-Control-Request-Method", "PUT")
            .with_header("Access-Control-Request-Headers", "X-Anything");
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "PUT")
            .with_header("Access-Control-Allow-Headers", "*");
        assert_eq!(evaluate_response(&request, &response), CorsDecision::Allow);
    }

    #[test]
    fn request_header_lookup_is_case_insensitive() {
        let request = CorsRequest::new(ORIGIN, "GET").with_header("X-Test", "value");
        assert_eq!(request.header("x-test"), Some("value"));
        assert_eq!(request.header("missing"), None);
    }

    #[test]
    fn cors_error_display_mentions_the_problem() {
        assert!(CorsError::OriginNotAllowed.to_string().contains("origin"));
        assert!(
            CorsError::MethodNotAllowed("PUT".to_string())
                .to_string()
                .contains("PUT")
        );
        assert!(CorsError::PreflightStatus(403).to_string().contains("403"));
    }

    #[test]
    fn repeated_allow_methods_and_headers_lines_are_combined() {
        // Typical of servers that append CORS headers in more than one layer.
        let request = CorsRequest::new(ORIGIN, "OPTIONS")
            .with_header("Access-Control-Request-Method", "PATCH")
            .with_header(
                "Access-Control-Request-Headers",
                "X-Requested-With, Content-Type",
            );
        let response = CorsResponse::new(204)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Methods", "GET, HEAD")
            .with_header("Access-Control-Allow-Methods", "POST, PATCH")
            .with_header("Access-Control-Allow-Headers", "Content-Type")
            .with_header("Access-Control-Allow-Headers", "X-Requested-With");
        assert_eq!(evaluate_response(&request, &response), CorsDecision::Allow);
    }

    #[test]
    fn ambiguous_allow_origin_is_rejected() {
        for value in [
            "https://a.example, https://b.example",
            "https://a.example https://b.example",
        ] {
            let response = CorsResponse::new(200).with_header("Access-Control-Allow-Origin", value);
            assert_eq!(
                evaluate_response(&simple_request(), &response),
                CorsDecision::Deny(CorsError::InvalidAllowOrigin(value.to_string())),
                "{value}"
            );
        }
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", ORIGIN)
            .with_header("Access-Control-Allow-Origin", ORIGIN);
        assert!(matches!(
            evaluate_response(&simple_request(), &response),
            CorsDecision::Deny(CorsError::InvalidAllowOrigin(_))
        ));
    }

    #[test]
    fn max_age_is_parsed_and_capped() {
        assert_eq!(
            CorsResponse::new(200)
                .with_header("Access-Control-Max-Age", "600")
                .max_age(),
            Some(600)
        );
        assert_eq!(
            CorsResponse::new(200)
                .with_header("Access-Control-Max-Age", "86400")
                .max_age(),
            Some(7200)
        );
        assert_eq!(
            CorsResponse::new(200)
                .with_header("Access-Control-Max-Age", "nonsense")
                .max_age(),
            None
        );
        assert_eq!(CorsResponse::new(200).max_age(), None);
    }

    #[test]
    fn expose_headers_and_vary_origin_are_parsed() {
        let response = CorsResponse::new(200)
            .with_header(
                "Access-Control-Expose-Headers",
                "X-Request-Id, Content-Range",
            )
            .with_header("Access-Control-Expose-Headers", "X-Total-Count")
            .with_header("Vary", "Accept-Encoding, Origin");
        assert_eq!(
            response.exposed_headers(),
            ["x-request-id", "content-range", "x-total-count"]
        );
        assert!(response.varies_on_origin());
        assert!(
            !CorsResponse::new(200)
                .with_header("Vary", "Accept")
                .varies_on_origin()
        );
    }

    #[test]
    fn range_header_is_safelisted_only_for_single_byte_ranges() {
        assert!(is_simple_header("Range", "bytes=0-"));
        assert!(is_simple_header("Range", "bytes=100-999"));
        assert!(!is_simple_header("Range", "bytes=0-100,200-300"));
        assert!(!is_simple_header("Range", "items=0-10"));
        assert!(!is_simple_header("Range", "bytes=500-100"));
    }

    #[test]
    fn headers_with_unsafe_bytes_trigger_preflight() {
        assert!(is_simple_header("Accept", "text/html, application/json"));
        assert!(!is_simple_header("X-Token", "a:b"));
        assert!(!is_simple_header("Accept", "text/html\r\nInjected: yes"));
        let request = CorsRequest::new(ORIGIN, "GET").with_header("Accept", "text/html (x)");
        assert_eq!(request.kind(), RequestKind::Preflight);
    }

    #[test]
    fn common_api_preflight_headers_are_accepted() {
        // A typical XHR/fetch preflight response from a JSON API.
        let request = CorsRequest::new("https://app.example", "OPTIONS")
            .with_header("Origin", "https://app.example")
            .with_header("Access-Control-Request-Method", "POST")
            .with_header(
                "Access-Control-Request-Headers",
                "authorization, content-type, x-requested-with",
            );
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", "https://app.example")
            .with_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
            .with_header(
                "Access-Control-Allow-Headers",
                "Authorization, Content-Type, X-Requested-With",
            )
            .with_header("Access-Control-Allow-Credentials", "true")
            .with_header("Access-Control-Max-Age", "600")
            .with_header("Vary", "Origin");
        assert_eq!(
            evaluate_response_with_credentials(&request, &response, true),
            CorsDecision::Allow
        );
    }

    #[test]
    fn trailing_slash_in_allow_origin_is_tolerated() {
        // Some frameworks serialise the origin with a path; it still names the
        // same tuple origin and is safe to accept.
        let response = CorsResponse::new(200)
            .with_header("Access-Control-Allow-Origin", "https://app.example/");
        assert!(evaluate_response(&simple_request(), &response).is_allowed());
    }
}
