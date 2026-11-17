//! Redirect handling.

use browser_url::Url;

use crate::request::Method;
use crate::response::Response;

/// Policy controlling how redirects are followed.
#[derive(Debug, Clone, Copy)]
pub struct RedirectPolicy {
    /// Maximum number of redirects before giving up.
    pub max_redirects: usize,
    /// Whether `http -> https` (and the reverse) is allowed.
    pub allow_cross_scheme: bool,
}

impl Default for RedirectPolicy {
    fn default() -> Self {
        Self {
            max_redirects: 10,
            allow_cross_scheme: true,
        }
    }
}

/// What to do with a redirect response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectOutcome {
    /// Follow this URL with this method.
    Follow(Url, Method),
    /// The redirect cannot be followed.
    Reject(String),
}

impl RedirectPolicy {
    /// Computes the next hop for a redirect response.
    ///
    /// `hops` is the number of redirects already followed.
    pub fn resolve(
        &self,
        current: &Url,
        response: &Response,
        method: Method,
        hops: usize,
    ) -> Result<RedirectOutcome, crate::HttpError> {
        if hops >= self.max_redirects {
            return Err(crate::HttpError::TooManyRedirects(hops));
        }
        let Some(location) = response.headers.get("Location") else {
            return Ok(RedirectOutcome::Reject("missing Location header".into()));
        };
        let next = match current.join(location) {
            Ok(url) => url,
            Err(err) => return Ok(RedirectOutcome::Reject(err.to_string())),
        };
        if !next.is_network() {
            return Ok(RedirectOutcome::Reject(format!(
                "unsupported redirect scheme: {}",
                next.scheme()
            )));
        }
        if !self.allow_cross_scheme && next.scheme() != current.scheme() {
            return Ok(RedirectOutcome::Reject(
                "cross-scheme redirect rejected".into(),
            ));
        }
        let next_method = match response.status {
            303 => Method::Get,
            301 | 302 if method == Method::Post => Method::Get,
            _ => method,
        };
        Ok(RedirectOutcome::Follow(next, next_method))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headers::Headers;

    fn redirect(status: u16, from: &str, location: &str) -> RedirectOutcome {
        let current = Url::parse(from).unwrap();
        let mut headers = Headers::new();
        headers.insert("Location", location);
        let response = Response::new(current.clone(), status, "", headers, Vec::new());
        RedirectPolicy::default()
            .resolve(&current, &response, Method::Get, 0)
            .unwrap()
    }

    #[test]
    fn resolves_relative_location() {
        match redirect(302, "https://a.test/dir/page", "../next") {
            RedirectOutcome::Follow(url, method) => {
                assert_eq!(url.as_str(), "https://a.test/next");
                assert_eq!(method, Method::Get);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn see_other_rewrites_post_to_get() {
        match redirect(303, "https://a.test/", "/done") {
            RedirectOutcome::Follow(_, method) => assert_eq!(method, Method::Get),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn stops_after_max_redirects() {
        let current = Url::parse("https://a.test/").unwrap();
        let mut headers = Headers::new();
        headers.insert("Location", "/x");
        let response = Response::new(current.clone(), 302, "", headers, Vec::new());
        let err = RedirectPolicy::default()
            .resolve(&current, &response, Method::Get, 10)
            .unwrap_err();
        assert!(matches!(err, crate::HttpError::TooManyRedirects(10)));
    }

    #[test]
    fn rejects_non_network_schemes() {
        match redirect(302, "https://a.test/", "javascript:alert(1)") {
            RedirectOutcome::Reject(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
