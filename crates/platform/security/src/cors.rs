//! Cross-Origin Resource Sharing request and response evaluation.
//!
//! The implementation follows the Fetch CORS protocol for the cases the engine
//! currently performs: simple requests, preflight detection, `Access-Control-
//! Allow-Origin` matching and preflight method/header/credential checks.
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
            .map_or_else(Vec::new, |value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .collect()
            })
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
pub fn is_simple_header(name: &str, value: &str) -> bool {
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
    false
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
    let allowed = response
        .header("access-control-allow-origin")
        .ok_or(CorsError::MissingAllowOrigin)?
        .trim();
    if allowed.is_empty() {
        return Err(CorsError::InvalidAllowOrigin(allowed.to_string()));
    }
    if allowed == "*" {
        return if credentials {
            Err(CorsError::WildcardWithCredentials)
        } else {
            Ok(())
        };
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
    let allowed_methods = response
        .header("access-control-allow-methods")
        .ok_or(CorsError::MissingAllowMethods)?;
    let mut method_allowed = false;
    for candidate in allowed_methods.split(',') {
        let candidate = candidate.trim();
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
    let allowed_headers = response
        .header("access-control-allow-headers")
        .ok_or(CorsError::MissingAllowHeaders)?;
    let allowed: Vec<String> = allowed_headers
        .split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
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
}
