//! Origin model: scheme/host/port tuples plus opaque origins.

use std::fmt;

/// A document origin.
///
/// Two origins compare equal when they share scheme, host and port. Non-special
/// URLs (for example `file:`) get an opaque origin that never compares equal to
/// any other origin, including another opaque origin.
#[derive(Debug, Clone)]
pub struct Origin {
    inner: Option<(String, String, u16)>,
}

impl Origin {
    /// Creates a tuple origin.
    pub fn tuple(scheme: &str, host: &str, port: u16) -> Self {
        Self {
            inner: Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase(), port)),
        }
    }

    /// Creates an opaque origin.
    pub fn opaque() -> Self {
        Self { inner: None }
    }

    /// True when this origin is opaque.
    pub fn is_opaque(&self) -> bool {
        self.inner.is_none()
    }

    /// True when `self` and `other` are the same origin.
    pub fn same_origin(&self, other: &Self) -> bool {
        match (&self.inner, &other.inner) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// True when `other` may read resources of `self` (same origin or
    /// `document.domain` irrelevant, scheme/host/port must match).
    pub fn is_same_origin_domain(&self, other: &Self) -> bool {
        self.same_origin(other)
    }

    /// Scheme of the origin, if it is not opaque.
    pub fn scheme(&self) -> Option<&str> {
        self.inner.as_ref().map(|(s, _, _)| s.as_str())
    }

    /// Host of the origin, if it is not opaque.
    pub fn host(&self) -> Option<&str> {
        self.inner.as_ref().map(|(_, h, _)| h.as_str())
    }

    /// Port of the origin, if it is not opaque.
    pub fn port(&self) -> Option<u16> {
        self.inner.as_ref().map(|(_, _, p)| *p)
    }

    /// Serialisation used by `postMessage` and the `Origin` header.
    pub fn serialize(&self) -> String {
        match &self.inner {
            Some((s, h, p)) => format!("{s}://{h}:{p}"),
            None => "null".to_string(),
        }
    }
}

impl PartialEq for Origin {
    fn eq(&self, other: &Self) -> bool {
        self.same_origin(other)
    }
}

impl Eq for Origin {}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialize())
    }
}
