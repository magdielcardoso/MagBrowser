//! Content Security Policy parsing and matching.
//!
//! This is a working subset of CSP Level 3: the common fetch directives
//! (`default-src`, `script-src`, `style-src`, `img-src`, `connect-src`, ...) with
//! `'self'`, `'none'`, `'unsafe-inline'`, `*`, scheme sources and host sources
//! (including wildcard subdomains and explicit ports).
//!
//! Multiple policies are combined with [`CspList`], which enforces the most
//! restrictive result: a resource is allowed only when every policy allows it.
//! Nonces, hashes, `'strict-dynamic'`, reports and `frame-ancestors` are parsed as
//! inert keywords and do not yet grant access.

use std::collections::HashMap;

use browser_url::{Origin, Url, default_port};

/// One source expression in a CSP directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceExpression {
    /// `'self'`: the document's own origin.
    SelfOrigin,
    /// `'none'`: matches nothing.
    None,
    /// `'unsafe-inline'`: permits inline script/style for the directive.
    UnsafeInline,
    /// `*`: any network URL.
    Wildcard,
    /// A scheme source such as `https:`.
    Scheme(String),
    /// A host source, optionally with scheme, port and wildcard subdomain.
    Host(HostSource),
    /// An unsupported quoted keyword such as `'nonce-...'`; never matches a URL.
    Keyword(String),
}

/// A host source expression such as `*.example.com:8443` or
/// `https://cdn.example.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSource {
    /// Optional scheme prefix. When `None`, any scheme with a matching host is
    /// accepted.
    pub scheme: Option<String>,
    /// Host without a leading `*.`; lowercase.
    pub host: String,
    /// Optional explicit port.
    pub port: Option<u16>,
    /// True when the source was `*.host` and matches subdomains only.
    pub wildcard_subdomain: bool,
}

impl HostSource {
    fn matches(&self, url: &Url) -> bool {
        if let Some(scheme) = &self.scheme
            && !url.scheme().eq_ignore_ascii_case(scheme)
        {
            return false;
        }
        let Some(url_host) = url.host() else {
            return false;
        };
        let host_matches = if self.wildcard_subdomain {
            let suffix = format!(".{}", self.host);
            url_host != self.host && url_host.ends_with(&suffix)
        } else {
            url_host.eq_ignore_ascii_case(&self.host)
        };
        if !host_matches {
            return false;
        }
        match self.port {
            Some(port) => url.port_or_default() == Some(port),
            None => match url.port() {
                None => true,
                Some(port) => default_port(url.scheme()) == Some(port),
            },
        }
    }
}

impl SourceExpression {
    /// True when this expression permits `url` for a document with `origin`.
    pub fn matches(&self, url: &Url, origin: &Origin) -> bool {
        match self {
            Self::SelfOrigin => {
                !origin.is_opaque() && !url.origin().is_opaque() && url.origin().same_origin(origin)
            }
            Self::None | Self::UnsafeInline | Self::Keyword(_) => false,
            Self::Wildcard => url.is_network(),
            Self::Scheme(scheme) => url.scheme().eq_ignore_ascii_case(scheme),
            Self::Host(host) => host.matches(url),
        }
    }
}

/// A parsed CSP policy, mapping directive names to source lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Directive name (lowercase) to source expressions.
    pub directives: HashMap<String, Vec<SourceExpression>>,
}

impl Policy {
    /// An empty policy. It restricts nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses one CSP header value (for example the value of `Content-Security-
    /// Policy`). Directive names are lowercased; repeated directives keep their
    /// first occurrence.
    pub fn parse(header: &str) -> Self {
        let mut directives: HashMap<String, Vec<SourceExpression>> = HashMap::new();
        for directive in header.split(';') {
            let mut tokens = directive.split_ascii_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };
            let name = name.to_ascii_lowercase();
            let sources = tokens.filter_map(parse_source).collect::<Vec<_>>();
            directives.entry(name).or_insert(sources);
        }
        Self { directives }
    }

    /// All directives with their source lists.
    pub fn directives(&self) -> &HashMap<String, Vec<SourceExpression>> {
        &self.directives
    }

    /// True when `url` is allowed by `directive`, falling back to `default-src`.
    ///
    /// A missing directive and missing `default-src` mean the policy does not
    /// restrict this resource type. A directive with an empty source list blocks
    /// everything of that type.
    pub fn allows(&self, directive: &str, url: &Url, origin: &Origin) -> bool {
        let Some(sources) = self.sources_for(directive) else {
            return true;
        };
        if sources.is_empty() {
            return false;
        }
        if sources
            .iter()
            .all(|source| *source == SourceExpression::None)
        {
            return false;
        }
        sources.iter().any(|source| source.matches(url, origin))
    }

    /// True when inline content is permitted for `directive`.
    ///
    /// Requires `'unsafe-inline'`; nonces and hashes are not yet supported.
    pub fn allows_inline(&self, directive: &str) -> bool {
        let Some(sources) = self.sources_for(directive) else {
            return true;
        };
        sources.contains(&SourceExpression::UnsafeInline)
    }

    fn sources_for(&self, directive: &str) -> Option<&Vec<SourceExpression>> {
        let name = directive.to_ascii_lowercase();
        self.directives
            .get(&name)
            .or_else(|| self.directives.get("default-src"))
    }
}

/// An ordered list of policies that must all allow a resource.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CspList {
    policies: Vec<Policy>,
}

impl CspList {
    /// Creates an empty list. An empty list permits everything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a list from already parsed policies.
    pub fn from_policies(policies: Vec<Policy>) -> Self {
        Self { policies }
    }

    /// Parses a header value into a policy list.
    ///
    /// A single header may contain comma-separated policies; each is enforced
    /// independently.
    pub fn parse(header: &str) -> Self {
        Self::from_policies(split_policies(header).map(Policy::parse).collect())
    }

    /// Parses several header values. Each header is a separate policy, and they
    /// combine most-restrictively.
    pub fn parse_headers<'a>(headers: impl IntoIterator<Item = &'a str>) -> Self {
        Self::from_policies(
            headers
                .into_iter()
                .flat_map(split_policies)
                .map(Policy::parse)
                .collect(),
        )
    }

    /// Appends a policy.
    pub fn push(&mut self, policy: Policy) {
        self.policies.push(policy);
    }

    /// The individual policies.
    pub fn policies(&self) -> &[Policy] {
        &self.policies
    }

    /// True when there are no policies.
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// True when every policy allows `url` for `directive`.
    pub fn allows(&self, directive: &str, url: &Url, origin: &Origin) -> bool {
        self.policies
            .iter()
            .all(|policy| policy.allows(directive, url, origin))
    }

    /// True when every policy allows inline content for `directive`.
    pub fn allows_inline(&self, directive: &str) -> bool {
        self.policies
            .iter()
            .all(|policy| policy.allows_inline(directive))
    }
}

fn split_policies(header: &str) -> impl Iterator<Item = &str> {
    header
        .split(',')
        .map(str::trim)
        .filter(|policy| !policy.is_empty())
}

fn parse_source(token: &str) -> Option<SourceExpression> {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "'self'" => Some(SourceExpression::SelfOrigin),
        "'none'" => Some(SourceExpression::None),
        "'unsafe-inline'" => Some(SourceExpression::UnsafeInline),
        "*" => Some(SourceExpression::Wildcard),
        _ if lower.starts_with('\'') => Some(SourceExpression::Keyword(lower)),
        _ => {
            if let Some(scheme) = lower.strip_suffix(':') {
                if scheme.is_empty() || scheme.contains('/') {
                    return None;
                }
                return Some(SourceExpression::Scheme(scheme.to_string()));
            }
            parse_host_source(&lower).map(SourceExpression::Host)
        }
    }
}

fn parse_host_source(token: &str) -> Option<HostSource> {
    if token.is_empty()
        || token.contains('@')
        || token.contains('?')
        || token.contains('#')
        || token.contains('\'')
    {
        return None;
    }
    let (scheme, rest) = token
        .split_once("://")
        .map_or((None, token), |(scheme, rest)| {
            (Some(scheme.to_string()), rest)
        });
    if scheme.as_ref().is_some_and(|scheme| {
        scheme.is_empty()
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    }) {
        return None;
    }
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    let (host, port) = split_host_port(rest)?;
    let wildcard_subdomain = host.starts_with("*.");
    let host = if wildcard_subdomain {
        host.strip_prefix("*.")?.to_string()
    } else {
        host
    };
    if host.is_empty() || host.contains('*') {
        return None;
    }
    Some(HostSource {
        scheme,
        host,
        port,
        wildcard_subdomain,
    })
}

fn split_host_port(rest: &str) -> Option<(String, Option<u16>)> {
    if rest.starts_with('[') {
        let end = rest.find(']')?;
        let host = rest.get(..=end)?.to_string();
        let tail = rest.get(end + 1..)?;
        if tail.is_empty() {
            return Some((host, None));
        }
        let port = tail.strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host, Some(port)));
    }
    if let Some((host, port)) = rest.rsplit_once(':') {
        if host.is_empty() {
            return None;
        }
        let port = port.parse::<u16>().ok()?;
        return Some((host.to_string(), Some(port)));
    }
    Some((rest.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "https://app.example/page";

    fn url(input: &str) -> Url {
        Url::parse(input).unwrap()
    }

    fn origin() -> Origin {
        url(PAGE).origin()
    }

    fn allows(policy: &Policy, directive: &str, resource: &str) -> bool {
        policy.allows(directive, &url(resource), &origin())
    }

    #[test]
    fn self_allows_same_origin_resources() {
        let policy = Policy::parse("default-src 'self'");
        assert!(allows(
            &policy,
            "script-src",
            "https://app.example/js/app.js"
        ));
        assert!(allows(
            &policy,
            "script-src",
            "https://app.example:443/x.js"
        ));
        assert!(!allows(&policy, "script-src", "https://cdn.example/x.js"));
    }

    #[test]
    fn self_rejects_other_scheme_and_port() {
        let policy = Policy::parse("script-src 'self'");
        assert!(!allows(&policy, "script-src", "http://app.example/x.js"));
        assert!(!allows(
            &policy,
            "script-src",
            "https://app.example:8443/x.js"
        ));
    }

    #[test]
    fn none_denies_everything() {
        let policy = Policy::parse("default-src 'none'");
        assert!(!allows(&policy, "img-src", "https://app.example/a.png"));
        assert!(!policy.allows_inline("script-src"));
    }

    #[test]
    fn none_mixed_with_other_sources_is_ignored() {
        let policy = Policy::parse("script-src 'none' 'self'");
        assert!(allows(&policy, "script-src", "https://app.example/a.js"));
    }

    #[test]
    fn wildcard_allows_network_urls_but_not_data() {
        let policy = Policy::parse("img-src *");
        assert!(allows(&policy, "img-src", "https://cdn.example/a.png"));
        assert!(allows(&policy, "img-src", "http://cdn.example/a.png"));
        assert!(!allows(&policy, "img-src", "data:image/png;base64,AAAA"));
    }

    #[test]
    fn scheme_sources_match_by_scheme() {
        let policy = Policy::parse("script-src https:");
        assert!(allows(&policy, "script-src", "https://cdn.example/a.js"));
        assert!(!allows(&policy, "script-src", "http://cdn.example/a.js"));
    }

    #[test]
    fn host_source_matches_exact_host() {
        let policy = Policy::parse("script-src cdn.example");
        assert!(allows(&policy, "script-src", "https://cdn.example/a.js"));
        assert!(!allows(&policy, "script-src", "https://evil.example/a.js"));
    }

    #[test]
    fn wildcard_host_matches_subdomains_only() {
        let policy = Policy::parse("script-src *.cdn.example");
        assert!(allows(
            &policy,
            "script-src",
            "https://static.cdn.example/a.js"
        ));
        assert!(allows(
            &policy,
            "script-src",
            "https://a.b.cdn.example/a.js"
        ));
        assert!(!allows(&policy, "script-src", "https://cdn.example/a.js"));
        assert!(!allows(
            &policy,
            "script-src",
            "https://badcdn.example/a.js"
        ));
    }

    #[test]
    fn host_source_honours_explicit_port() {
        let policy = Policy::parse("connect-src api.example:8443");
        assert!(allows(
            &policy,
            "connect-src",
            "https://api.example:8443/v1"
        ));
        assert!(!allows(&policy, "connect-src", "https://api.example/v1"));
    }

    #[test]
    fn host_source_without_port_rejects_non_default_port() {
        let policy = Policy::parse("connect-src api.example");
        assert!(allows(&policy, "connect-src", "https://api.example/v1"));
        assert!(allows(&policy, "connect-src", "https://api.example:443/v1"));
        assert!(!allows(
            &policy,
            "connect-src",
            "https://api.example:8443/v1"
        ));
    }

    #[test]
    fn host_source_with_scheme_requires_that_scheme() {
        let policy = Policy::parse("script-src https://cdn.example");
        assert!(allows(&policy, "script-src", "https://cdn.example/a.js"));
        assert!(!allows(&policy, "script-src", "http://cdn.example/a.js"));
    }

    #[test]
    fn directive_falls_back_to_default_src() {
        let policy = Policy::parse("default-src https://cdn.example");
        assert!(allows(&policy, "img-src", "https://cdn.example/a.png"));
        assert!(!allows(&policy, "img-src", "https://other.example/a.png"));
    }

    #[test]
    fn explicit_directive_overrides_default_src() {
        let policy = Policy::parse("default-src 'self'; script-src https://cdn.example");
        assert!(allows(&policy, "script-src", "https://cdn.example/a.js"));
        assert!(!allows(&policy, "script-src", "https://app.example/a.js"));
        assert!(allows(&policy, "img-src", "https://app.example/a.png"));
    }

    #[test]
    fn empty_directive_blocks_that_resource_type() {
        let policy = Policy::parse("script-src; img-src 'self'");
        assert!(!allows(&policy, "script-src", "https://app.example/a.js"));
        assert!(allows(&policy, "img-src", "https://app.example/a.png"));
    }

    #[test]
    fn missing_directive_is_unrestricted() {
        let policy = Policy::parse("img-src 'none'");
        assert!(allows(
            &policy,
            "script-src",
            "https://anywhere.example/a.js"
        ));
        assert!(policy.allows_inline("script-src"));
    }

    #[test]
    fn unsafe_inline_controls_inline_content() {
        let blocked = Policy::parse("script-src 'self'");
        let allowed = Policy::parse("script-src 'unsafe-inline' 'self'");
        assert!(!blocked.allows_inline("script-src"));
        assert!(allowed.allows_inline("script-src"));
        assert!(!Policy::parse("style-src 'self'").allows_inline("style-src"));
    }

    #[test]
    fn unsupported_keywords_never_match_urls() {
        let policy = Policy::parse("script-src 'nonce-abc123'");
        assert!(!allows(&policy, "script-src", "https://app.example/a.js"));
        assert!(!policy.allows_inline("script-src"));
    }

    #[test]
    fn multiple_policies_combine_most_restrictively() {
        let list = CspList::parse_headers(["script-src 'self'", "script-src https://cdn.example"]);
        assert_eq!(list.policies().len(), 2);
        assert!(!list.allows("script-src", &url("https://cdn.example/a.js"), &origin()));
        assert!(!list.allows("script-src", &url("https://app.example/a.js"), &origin()));
        let list = CspList::parse_headers([
            "script-src 'self' https://cdn.example",
            "script-src https://cdn.example 'self'",
        ]);
        assert!(list.allows("script-src", &url("https://cdn.example/a.js"), &origin()));
    }

    #[test]
    fn a_none_policy_blocks_across_the_list() {
        let list = CspList::parse_headers(["default-src *", "default-src 'none'"]);
        assert!(!list.allows("img-src", &url("https://cdn.example/a.png"), &origin()));
        assert!(!list.allows_inline("script-src"));
    }

    #[test]
    fn comma_separated_policies_are_all_enforced() {
        let list = CspList::parse("default-src * , default-src 'none'");
        assert_eq!(list.policies().len(), 2);
        assert!(!list.allows("img-src", &url("https://cdn.example/a.png"), &origin()));
    }

    #[test]
    fn empty_csp_list_allows_everything() {
        let list = CspList::new();
        assert!(list.is_empty());
        assert!(list.allows(
            "script-src",
            &url("https://anywhere.example/a.js"),
            &origin()
        ));
        assert!(list.allows_inline("script-src"));
    }

    #[test]
    fn policy_parsing_is_case_insensitive_and_skips_blank_parts() {
        let policy = Policy::parse("  DEFAULT-SRC   'SELF' ;; IMG-SRC *.Example.COM ");
        assert!(allows(&policy, "script-src", "https://app.example/a.js"));
        assert!(allows(
            &policy,
            "img-src",
            "https://static.example.com/a.png"
        ));
    }

    #[test]
    fn documented_directives_are_preserved() {
        let policy = Policy::parse(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src *; connect-src https:",
        );
        let names: Vec<_> = policy.directives().keys().cloned().collect();
        for expected in [
            "default-src",
            "script-src",
            "style-src",
            "img-src",
            "connect-src",
        ] {
            assert!(names.iter().any(|name| name == expected), "{expected}");
        }
    }

    #[test]
    fn self_does_not_allow_opaque_document_origins() {
        let opaque = Origin::opaque();
        let policy = Policy::parse("script-src 'self'");
        assert!(!policy.allows("script-src", &url("data:text/javascript,alert(1)"), &opaque));
    }
}
