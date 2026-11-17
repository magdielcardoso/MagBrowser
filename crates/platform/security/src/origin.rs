//! Same-origin policy and the `Origin` header.
//!
//! [`OriginPolicy`] wraps the URL crate's origin tuple and adds the security
//! decisions the rest of the engine needs. Opaque origins (`data:`, `javascript:`,
//! `file:` and sandboxed documents) never match any origin and are denied as
//! request origins by [`OriginPolicy::from_url`] and [`validate`].

use std::fmt;

use browser_url::{Origin, Url};

/// A document or request origin together with same-origin decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicy {
    origin: Origin,
}

impl OriginPolicy {
    /// Wraps an origin. Use [`validate`] before trusting externally supplied data.
    pub fn new(origin: Origin) -> Self {
        Self { origin }
    }
    /// Creates an opaque origin (equivalent to the `Origin: null` serialisation).
    pub fn opaque() -> Self {
        Self::new(Origin::opaque())
    }

    /// Derives a tuple origin from a URL, rejecting opaque and unsupported schemes.
    ///
    /// `data:`, `javascript:`, `file:` and `about:` URLs are rejected.
    pub fn from_url(url: &Url) -> Result<Self, OriginError> {
        let origin = url.origin();
        if origin.is_opaque() {
            return Err(OriginError::Opaque);
        }
        let scheme = origin.scheme().unwrap_or_default();
        if !is_supported_scheme(scheme) {
            return Err(OriginError::UnsupportedScheme(scheme.to_string()));
        }
        Ok(Self::new(origin))
    }

    /// The wrapped URL origin.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Consumes the policy and returns the wrapped origin.
    pub fn into_origin(self) -> Origin {
        self.origin
    }

    /// True when this origin is opaque.
    pub fn is_opaque(&self) -> bool {
        self.origin.is_opaque()
    }

    /// Serialised origin such as `https://example.com:443` or `null`.
    pub fn serialize(&self) -> String {
        self.origin.serialize()
    }

    /// True when the two origins share scheme, host and port.
    ///
    /// Opaque origins never match, including themselves.
    pub fn same_origin(&self, other: &Self) -> bool {
        !self.origin.is_opaque() && self.origin.same_origin(&other.origin)
    }

    /// True when `reader` may read resources belonging to `self`.
    ///
    /// This engine does not implement `document.domain` relaxation, so the check
    /// is exactly same-origin.
    pub fn allows_read(&self, reader: &Self) -> bool {
        self.same_origin(reader)
    }

    /// True when the origin is non-opaque and uses an allowed scheme.
    pub fn validate(&self) -> Result<(), OriginError> {
        validate(self)
    }
}

impl fmt::Display for OriginPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialize())
    }
}

impl From<Origin> for OriginPolicy {
    fn from(origin: Origin) -> Self {
        Self::new(origin)
    }
}

impl From<&Origin> for OriginPolicy {
    fn from(origin: &Origin) -> Self {
        Self::new(origin.clone())
    }
}

impl From<&OriginPolicy> for OriginPolicy {
    fn from(policy: &OriginPolicy) -> Self {
        policy.clone()
    }
}

fn is_supported_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss")
}

/// True when `a` and `b` are the same non-opaque origin.
pub fn same_origin(a: &OriginPolicy, b: &OriginPolicy) -> bool {
    a.same_origin(b)
}

/// True when `reader` may read resources belonging to `target`.
pub fn allows_read(target: &OriginPolicy, reader: &OriginPolicy) -> bool {
    target.allows_read(reader)
}

/// Validates that an origin may be used for a security decision.
///
/// Opaque origins cannot be compared or granted capabilities and are rejected, as
/// are tuple origins with schemes outside `http`, `https`, `ws` and `wss`.
pub fn validate(origin: &OriginPolicy) -> Result<(), OriginError> {
    if origin.is_opaque() {
        return Err(OriginError::Opaque);
    }
    let scheme = origin.origin.scheme().unwrap_or_default();
    if is_supported_scheme(scheme) {
        Ok(())
    } else {
        Err(OriginError::UnsupportedScheme(scheme.to_string()))
    }
}

/// Errors from origin validation and `Origin` header parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginError {
    /// Opaque origins have no host tuple and cannot be compared.
    Opaque,
    /// The scheme is not a network origin scheme supported by the engine.
    UnsupportedScheme(String),
    /// The `Origin` header value was malformed.
    InvalidHeader(String),
}

impl fmt::Display for OriginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Opaque => f.write_str("opaque origins cannot be used for origin checks"),
            Self::UnsupportedScheme(scheme) => write!(f, "unsupported origin scheme: {scheme}"),
            Self::InvalidHeader(value) => write!(f, "invalid Origin header: {value}"),
        }
    }
}

impl std::error::Error for OriginError {}

/// A parsed `Origin` request header.
///
/// The serialisation is either `null` for an opaque origin or one or more
/// space-separated serialised tuple origins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginHeader {
    origins: Vec<OriginPolicy>,
}

impl OriginHeader {
    /// Parses an `Origin` header value.
    pub fn parse(input: &str) -> Result<Self, OriginError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(OriginError::InvalidHeader("empty value".to_string()));
        }
        let tokens: Vec<&str> = input.split_ascii_whitespace().collect();
        if tokens == ["null"] {
            return Ok(Self {
                origins: Vec::new(),
            });
        }
        if tokens.contains(&"null") {
            return Err(OriginError::InvalidHeader(
                "null cannot be combined with tuple origins".to_string(),
            ));
        }
        let mut origins = Vec::with_capacity(tokens.len());
        for token in tokens {
            let url = Url::parse(token)
                .map_err(|_| OriginError::InvalidHeader(format!("invalid origin {token:?}")))?;
            origins.push(OriginPolicy::from_url(&url)?);
        }
        Ok(Self { origins })
    }

    /// Builds a header from an already validated origin.
    pub fn from_origin(origin: OriginPolicy) -> Self {
        if origin.is_opaque() {
            Self {
                origins: Vec::new(),
            }
        } else {
            Self {
                origins: vec![origin],
            }
        }
    }

    /// The origins in the header. Empty means `null`.
    pub fn origins(&self) -> &[OriginPolicy] {
        &self.origins
    }

    /// The single origin in the header, if it is not opaque.
    pub fn origin(&self) -> Option<&OriginPolicy> {
        self.origins.first()
    }

    /// True for `Origin: null`.
    pub fn is_opaque(&self) -> bool {
        self.origins.is_empty()
    }

    /// True when both headers carry the same single non-opaque origin.
    pub fn same_origin(&self, other: &Self) -> bool {
        match (self.origin(), other.origin()) {
            (Some(a), Some(b)) => a.same_origin(b),
            _ => false,
        }
    }

    /// Serialises the header value.
    pub fn serialize(&self) -> String {
        if self.origins.is_empty() {
            return "null".to_string();
        }
        self.origins
            .iter()
            .map(OriginPolicy::serialize)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl fmt::Display for OriginHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(input: &str) -> OriginPolicy {
        OriginPolicy::from_url(&Url::parse(input).unwrap()).unwrap()
    }

    #[test]
    fn same_origin_requires_scheme_host_and_port() {
        assert!(policy("https://example.com/a").same_origin(&policy("https://example.com/b")));
        assert!(!policy("https://example.com/").same_origin(&policy("http://example.com/")));
        assert!(!policy("https://example.com/").same_origin(&policy("https://other.com/")));
        assert!(!policy("https://example.com/").same_origin(&policy("https://example.com:8443/")));
    }

    #[test]
    fn default_and_explicit_ports_match() {
        assert!(policy("https://example.com/").same_origin(&policy("https://example.com:443/")));
        assert!(policy("http://example.com/").same_origin(&policy("http://example.com:80/")));
    }

    #[test]
    fn free_functions_match_methods() {
        let a = policy("https://example.com/");
        let b = policy("https://example.com/x");
        assert!(same_origin(&a, &b));
        assert!(allows_read(&a, &b));
        assert!(!allows_read(&a, &policy("https://evil.com/")));
    }

    #[test]
    fn reading_is_not_symmetric() {
        let a = policy("https://a.example/");
        let b = policy("https://b.example/");
        assert!(!a.allows_read(&b));
        assert!(!b.allows_read(&a));
    }

    #[test]
    fn opaque_origins_never_match() {
        let opaque = OriginPolicy::opaque();
        assert!(opaque.is_opaque());
        assert!(!opaque.same_origin(&OriginPolicy::opaque()));
        assert!(!opaque.same_origin(&policy("https://example.com/")));
        assert!(!policy("https://example.com/").allows_read(&opaque));
        assert_eq!(opaque.serialize(), "null");
    }

    #[test]
    fn from_url_rejects_data_and_javascript() {
        for input in ["data:text/html,hi", "javascript:alert(1)", "file:///tmp/x"] {
            let url = Url::parse(input).unwrap();
            assert_eq!(
                OriginPolicy::from_url(&url),
                Err(OriginError::Opaque),
                "{input}"
            );
        }
    }

    #[test]
    fn validation_rejects_opaque_and_unsupported_schemes() {
        assert_eq!(validate(&OriginPolicy::opaque()), Err(OriginError::Opaque));
        let custom = OriginPolicy::new(Origin::tuple("chrome-extension", "abc", 0));
        assert_eq!(
            validate(&custom),
            Err(OriginError::UnsupportedScheme(
                "chrome-extension".to_string()
            ))
        );
    }

    #[test]
    fn valid_network_origins_pass_validation() {
        for input in [
            "https://example.com/",
            "http://example.com/",
            "wss://example.com/socket",
        ] {
            assert!(policy(input).validate().is_ok(), "{input}");
        }
    }

    #[test]
    fn header_round_trips_a_tuple_origin() {
        let header = OriginHeader::parse("https://example.com:443").unwrap();
        assert!(!header.is_opaque());
        assert_eq!(
            header.origin().unwrap().serialize(),
            "https://example.com:443"
        );
        assert_eq!(header.serialize(), "https://example.com:443");
    }

    #[test]
    fn header_applies_default_ports() {
        let header = OriginHeader::parse("https://example.com").unwrap();
        assert_eq!(header.serialize(), "https://example.com:443");
    }

    #[test]
    fn header_null_is_opaque() {
        let header = OriginHeader::parse(" null ").unwrap();
        assert!(header.is_opaque());
        assert_eq!(header.origin(), None);
        assert_eq!(header.serialize(), "null");
        assert!(!header.same_origin(&OriginHeader::parse("https://example.com").unwrap()));
    }

    #[test]
    fn header_supports_space_separated_origins() {
        let header = OriginHeader::parse("https://a.example https://b.example").unwrap();
        assert_eq!(header.origins().len(), 2);
        assert_eq!(
            header.serialize(),
            "https://a.example:443 https://b.example:443"
        );
    }

    #[test]
    fn header_rejects_malformed_and_mixed_values() {
        assert!(OriginHeader::parse("").is_err());
        assert!(OriginHeader::parse("not a url").is_err());
        assert!(OriginHeader::parse("null https://example.com").is_err());
        assert!(OriginHeader::parse("data:text/html,x").is_err());
    }

    #[test]
    fn header_equality_uses_origin_semantics() {
        let a = OriginHeader::parse("https://example.com").unwrap();
        let b = OriginHeader::parse("https://example.com:443/path").unwrap();
        assert!(a.same_origin(&b));
        assert_eq!(a.serialize(), b.serialize());
        assert!(!a.same_origin(&OriginHeader::parse("http://example.com").unwrap()));
    }

    #[test]
    fn header_from_opaque_origin_serialises_as_null() {
        let header = OriginHeader::from_origin(OriginPolicy::opaque());
        assert!(header.is_opaque());
        assert_eq!(header.to_string(), "null");
    }

    #[test]
    fn header_display_matches_serialize() {
        let header = OriginHeader::parse("https://example.com").unwrap();
        assert_eq!(header.to_string(), header.serialize());
    }

    #[test]
    fn origin_error_messages_are_specific() {
        assert!(OriginError::Opaque.to_string().contains("opaque"));
        assert!(
            OriginError::UnsupportedScheme("ftp".to_string())
                .to_string()
                .contains("ftp")
        );
        assert!(
            OriginError::InvalidHeader("x".to_string())
                .to_string()
                .contains('x')
        );
    }
}
