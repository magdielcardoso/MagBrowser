//! Content Security Policy parsing and matching.
//!
//! This is a working subset of CSP Level 3: the common fetch directives
//! (`default-src`, `script-src`, `style-src`, `img-src`, `connect-src`, ...) with
//! `'self'`, `'none'`, `'unsafe-inline'`, `*`, scheme sources and host sources
//! (including wildcard subdomains and explicit ports).
//!
//! Multiple policies are combined with [`CspList`], which enforces the most
//! restrictive result: a resource is allowed only when every policy allows it.
//!
//! `'nonce-...'` and `'sha256-...'`/`'sha384-...'`/`'sha512-...'` sources are
//! parsed and can authorise inline and external content through
//! [`Policy::allows_inline_with`] and [`Policy::allows_with_check`]. The caller
//! supplies the element's nonce and the already computed content digests; this
//! crate is I/O free and does not hash content itself. [`Policy::allows_inline`]
//! keeps its conservative, nonce-free behaviour for existing callers.
//!
//! `'strict-dynamic'` is recognised so that it can invalidate `'unsafe-inline'`
//! and host allowlists when a nonce or hash is present, as CSP Level 3 requires.
//! Reports, `report-uri`, `upgrade-insecure-requests` and `frame-ancestors` are
//! parsed as inert directives; mixed-content handling lives in
//! [`crate::mixed_content`].

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
    /// A `'nonce-...'` source. Authorises content carrying the same nonce.
    Nonce(String),
    /// A `'sha256-...'`, `'sha384-...'` or `'sha512-...'` source.
    Hash(HashSource),
    /// `'strict-dynamic'`: with a nonce or hash, host allowlists are ignored.
    StrictDynamic,
    /// Any other quoted keyword; never grants access.
    Keyword(String),
}

/// The hash algorithm named by a CSP hash source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    /// `sha256`
    Sha256,
    /// `sha384`
    Sha384,
    /// `sha512`
    Sha512,
}

impl HashAlgorithm {
    /// The algorithm name used in a hash source expression.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }

    /// The digest length in bytes for this algorithm.
    pub const fn digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }
}

/// A parsed CSP hash source such as `'sha256-X48a...'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashSource {
    /// The named hash algorithm.
    pub algorithm: HashAlgorithm,
    /// The base64 digest from the policy, preserved verbatim.
    pub value: String,
}

impl HashSource {
    /// True when the supplied digest matches this source.
    ///
    /// Both standard and URL-safe base64 alphabets are accepted when decoding,
    /// and a missing padding suffix is tolerated. This keeps policies produced
    /// by common build tools working without weakening the comparison: only
    /// decoded bytes are compared.
    pub fn matches(&self, content: &ContentHash) -> bool {
        self.algorithm == content.algorithm
            && decode_base64(&self.value).is_some_and(|bytes| bytes == content.digest)
    }
}

/// A content digest computed by the caller, for CSP hash-source matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHash {
    /// The algorithm used to compute `digest`.
    pub algorithm: HashAlgorithm,
    /// Raw digest bytes (32 for SHA-256, 48 for SHA-384, 64 for SHA-512).
    pub digest: Vec<u8>,
}

impl ContentHash {
    /// Creates a hash, returning `None` when the digest length is wrong.
    pub fn new(algorithm: HashAlgorithm, digest: impl Into<Vec<u8>>) -> Option<Self> {
        let digest = digest.into();
        (digest.len() == algorithm.digest_len()).then_some(Self { algorithm, digest })
    }
}

/// Nonce and hash metadata for an inline or external piece of content.
///
/// A nonce is compared verbatim. Hashes are compared by decoded bytes, so the
/// caller only needs to provide the raw digest for the algorithm it used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentCheck<'a> {
    /// The element's `nonce` attribute.
    pub nonce: Option<&'a str>,
    /// Digests of the content, usually from an `integrity` attribute.
    pub hashes: &'a [ContentHash],
}

impl<'a> ContentCheck<'a> {
    /// A check carrying only a nonce.
    pub fn with_nonce(nonce: &'a str) -> Self {
        Self {
            nonce: Some(nonce),
            hashes: &[],
        }
    }

    /// A check carrying only content digests.
    pub fn with_hashes(hashes: &'a [ContentHash]) -> Self {
        Self {
            nonce: None,
            hashes,
        }
    }
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
            Self::None
            | Self::UnsafeInline
            | Self::Nonce(_)
            | Self::Hash(_)
            | Self::StrictDynamic
            | Self::Keyword(_) => false,
            Self::Wildcard => url.is_network(),
            Self::Scheme(scheme) => url.scheme().eq_ignore_ascii_case(scheme),
            Self::Host(host) => host.matches(url),
        }
    }

    /// True when this source authorises the supplied nonce or content digest.
    pub fn matches_content(&self, check: &ContentCheck<'_>) -> bool {
        match self {
            Self::Nonce(value) => check.nonce.is_some_and(|nonce| nonce == value),
            Self::Hash(source) => check.hashes.iter().any(|hash| source.matches(hash)),
            _ => false,
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
        if sources_allow_nothing(sources) {
            return false;
        }
        sources.iter().any(|source| source.matches(url, origin))
    }

    /// True when inline content is permitted for `directive`.
    ///
    /// This conservative check requires `'unsafe-inline'`; it deliberately ignores
    /// nonces and hashes because no element metadata is available. Use
    /// [`Policy::allows_inline_with`] to authorise nonce/hash-protected content.
    pub fn allows_inline(&self, directive: &str) -> bool {
        let Some(sources) = self.sources_for(directive) else {
            return true;
        };
        sources.contains(&SourceExpression::UnsafeInline)
    }

    /// True when inline content is permitted for `directive`, considering the
    /// element's nonce and content digests.
    ///
    /// `'unsafe-inline'` is ignored when the policy also contains a nonce, hash
    /// or `'strict-dynamic'` source, as CSP Level 3 requires.
    pub fn allows_inline_with(&self, directive: &str, check: &ContentCheck<'_>) -> bool {
        let Some(sources) = self.sources_for(directive) else {
            return true;
        };
        if sources_allow_nothing(sources) {
            return false;
        }
        let has_crypto_source = sources.iter().any(|source| {
            matches!(
                source,
                SourceExpression::Nonce(_)
                    | SourceExpression::Hash(_)
                    | SourceExpression::StrictDynamic
            )
        });
        if !has_crypto_source && sources.contains(&SourceExpression::UnsafeInline) {
            return true;
        }
        sources.iter().any(|source| source.matches_content(check))
    }

    /// True when `url` is allowed for `directive`, honouring a nonce or hash
    /// attached to the element (for example `<script nonce="..." src="...">`).
    ///
    /// With `'strict-dynamic'` and a nonce or hash, host and scheme sources are
    /// ignored for `script-src`: only content carrying the nonce/hash is allowed.
    pub fn allows_with_check(
        &self,
        directive: &str,
        url: &Url,
        origin: &Origin,
        check: &ContentCheck<'_>,
    ) -> bool {
        let Some(sources) = self.sources_for(directive) else {
            return true;
        };
        if sources_allow_nothing(sources) {
            return false;
        }
        if sources.iter().any(|source| source.matches_content(check)) {
            return true;
        }
        let strict_dynamic = directive.eq_ignore_ascii_case("script-src")
            && sources.contains(&SourceExpression::StrictDynamic)
            && sources.iter().any(|source| {
                matches!(
                    source,
                    SourceExpression::Nonce(_) | SourceExpression::Hash(_)
                )
            });
        if strict_dynamic {
            return false;
        }
        sources.iter().any(|source| source.matches(url, origin))
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

    /// True when every policy allows inline content carrying this nonce/digest.
    pub fn allows_inline_with(&self, directive: &str, check: &ContentCheck<'_>) -> bool {
        self.policies
            .iter()
            .all(|policy| policy.allows_inline_with(directive, check))
    }

    /// True when every policy allows `url`, also honouring the element's
    /// nonce/hash metadata.
    pub fn allows_with_check(
        &self,
        directive: &str,
        url: &Url,
        origin: &Origin,
        check: &ContentCheck<'_>,
    ) -> bool {
        self.policies
            .iter()
            .all(|policy| policy.allows_with_check(directive, url, origin, check))
    }
}

fn sources_allow_nothing(sources: &[SourceExpression]) -> bool {
    sources.is_empty()
        || sources
            .iter()
            .all(|source| *source == SourceExpression::None)
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
        "'strict-dynamic'" => Some(SourceExpression::StrictDynamic),
        "*" => Some(SourceExpression::Wildcard),
        // Nonce values are case-sensitive base64, so slice them from the original
        // token rather than the lowercased keyword.
        _ if lower.starts_with("'nonce-") && token.ends_with('\'') => {
            let value = &token["'nonce-".len()..token.len() - 1];
            (!value.is_empty()).then(|| SourceExpression::Nonce(value.to_string()))
        }
        _ if lower.starts_with("'sha") && token.ends_with('\'') => {
            Some(parse_hash_source(token).unwrap_or(SourceExpression::Keyword(lower)))
        }
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

fn parse_hash_source(token: &str) -> Option<SourceExpression> {
    let inner = token.strip_prefix('\'')?.strip_suffix('\'')?;
    let (name, value) = inner.split_once('-')?;
    let algorithm = match name.to_ascii_lowercase().as_str() {
        "sha256" => HashAlgorithm::Sha256,
        "sha384" => HashAlgorithm::Sha384,
        "sha512" => HashAlgorithm::Sha512,
        _ => return None,
    };
    if value.is_empty() || decode_base64(value).is_none() {
        return None;
    }
    Some(SourceExpression::Hash(HashSource {
        algorithm,
        value: value.to_string(),
    }))
}

/// Encodes bytes as standard padded base64 (the form used in CSP hash sources).
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[((word >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((word >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((word >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes standard or URL-safe base64, with or without padding.
///
/// Returns `None` on invalid characters or impossible lengths so a malformed
/// hash source can never be treated as a match.
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'=' {
        end -= 1;
    }
    if bytes.len() - end > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(end / 4 * 3 + 2);
    let mut word = 0u32;
    let mut bits = 0u32;
    for &byte in &bytes[..end] {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        word = (word << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((word >> bits) as u8);
            word &= (1u32 << bits) - 1;
        }
    }
    (bits < 6).then_some(out)
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

    fn digest(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn nonce_and_hash_sources_are_parsed() {
        let policy = Policy::parse(
            "script-src 'nonce-rAnd0m==' 'sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI=' 'strict-dynamic'",
        );
        let sources = &policy.directives()["script-src"];
        assert!(sources.contains(&SourceExpression::Nonce("rAnd0m==".to_string())));
        assert!(sources.iter().any(|source| matches!(
            source,
            SourceExpression::Hash(HashSource {
                algorithm: HashAlgorithm::Sha256,
                ..
            })
        )));
        assert!(sources.contains(&SourceExpression::StrictDynamic));
    }

    #[test]
    fn nonce_authorises_inline_content_without_unsafe_inline() {
        let policy = Policy::parse(
            "default-src 'self'; script-src 'self' 'nonce-rAnd0m'; style-src 'unsafe-inline'",
        );
        assert!(policy.allows_inline_with("script-src", &ContentCheck::with_nonce("rAnd0m")));
        assert!(!policy.allows_inline_with("script-src", &ContentCheck::with_nonce("wrong")));
        assert!(!policy.allows_inline_with("script-src", &ContentCheck::default()));
        // The conservative API keeps its old answer.
        assert!(!policy.allows_inline("script-src"));
        assert!(policy.allows_inline_with("style-src", &ContentCheck::default()));
    }

    #[test]
    fn hash_authorises_matching_inline_content() {
        let policy =
            Policy::parse("script-src 'sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI='");
        // Independently derived from SHA-256("alert(1)").
        let hash = ContentHash::new(
            HashAlgorithm::Sha256,
            digest("6e11c72f7cf6bc383152dd16ddd5903aba6bb1c99d6b6639a4bb0b838185fa92"),
        )
        .unwrap();
        let other = ContentHash::new(
            HashAlgorithm::Sha256,
            digest("46289932de1604479260f0178bba3a5f7019d133b263efc139c9a18d856bebf1"),
        )
        .unwrap();
        assert!(policy.allows_inline_with("script-src", &ContentCheck::with_hashes(&[hash])));
        assert!(!policy.allows_inline_with("script-src", &ContentCheck::with_hashes(&[other])));
        assert!(!policy.allows_inline_with("script-src", &ContentCheck::default()));
        // Hash sources never authorise a URL on their own.
        assert!(!allows(&policy, "script-src", "https://app.example/a.js"));
    }

    #[test]
    fn hash_algorithm_must_match() {
        let source = HashSource {
            algorithm: HashAlgorithm::Sha256,
            value: "bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI=".to_string(),
        };
        assert!(
            source.matches(
                &ContentHash::new(
                    HashAlgorithm::Sha256,
                    digest("6e11c72f7cf6bc383152dd16ddd5903aba6bb1c99d6b6639a4bb0b838185fa92"),
                )
                .unwrap()
            )
        );
        assert!(!source.matches(&ContentHash {
            algorithm: HashAlgorithm::Sha384,
            digest: digest("6e11c72f7cf6bc383152dd16ddd5903aba6bb1c99d6b6639a4bb0b838185fa92"),
        }));
    }

    #[test]
    fn base64_helpers_round_trip_and_accept_url_safe_forms() {
        let bytes = digest("6e11c72f7cf6bc383152dd16ddd5903aba6bb1c99d6b6639a4bb0b838185fa92");
        let encoded = encode_base64(&bytes);
        assert_eq!(encoded, "bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI=");
        assert_eq!(decode_base64(&encoded), Some(bytes.clone()));
        assert_eq!(decode_base64(encoded.trim_end_matches('=')), Some(bytes));
        assert_eq!(decode_base64("not!base64"), None);
    }

    #[test]
    fn strict_dynamic_ignores_unsafe_inline_and_host_allowlists() {
        let policy = Policy::parse(
            "script-src 'nonce-abc' 'strict-dynamic' 'unsafe-inline' https://cdn.example",
        );
        assert!(!policy.allows_inline_with("script-src", &ContentCheck::default()));
        assert!(policy.allows_inline_with("script-src", &ContentCheck::with_nonce("abc")));
        assert!(allows(&policy, "script-src", "https://cdn.example/a.js"));
        assert!(!policy.allows_with_check(
            "script-src",
            &url("https://cdn.example/a.js"),
            &origin(),
            &ContentCheck::default()
        ));
        assert!(policy.allows_with_check(
            "script-src",
            &url("https://cdn.example/a.js"),
            &origin(),
            &ContentCheck::with_nonce("abc")
        ));
    }

    #[test]
    fn nonce_allows_an_external_script_from_an_unlisted_host() {
        let policy = Policy::parse("script-src 'self' 'nonce-abc'");
        assert!(!allows(&policy, "script-src", "https://cdn.example/a.js"));
        assert!(policy.allows_with_check(
            "script-src",
            &url("https://cdn.example/a.js"),
            &origin(),
            &ContentCheck::with_nonce("abc")
        ));
        let list = CspList::parse("script-src 'self' 'nonce-abc'");
        assert!(list.allows_with_check(
            "script-src",
            &url("https://cdn.example/a.js"),
            &origin(),
            &ContentCheck::with_nonce("abc")
        ));
        assert!(!list.allows_inline_with("script-src", &ContentCheck::with_nonce("wrong")));
    }

    #[test]
    fn common_real_site_policy_with_nonce_and_data_images() {
        // A shape used by many frameworks: nonced scripts, a CDN fallback,
        // inline styles and data: images.
        let policy = Policy::parse(concat!(
            "default-src 'self'; ",
            "script-src 'self' 'nonce-rAnd0m' https://cdnjs.cloudflare.com; ",
            "style-src 'self' 'unsafe-inline'; ",
            "img-src 'self' data: https:; ",
            "connect-src 'self' https://api.example"
        ));
        assert!(policy.allows_inline_with("script-src", &ContentCheck::with_nonce("rAnd0m")));
        assert!(policy.allows_with_check(
            "script-src",
            &url("https://cdnjs.cloudflare.com/ajax/libs/x.js"),
            &origin(),
            &ContentCheck::with_nonce("rAnd0m")
        ));
        assert!(allows(&policy, "style-src", "https://app.example/a.css"));
        assert!(policy.allows_inline_with("style-src", &ContentCheck::default()));
        assert!(allows(&policy, "img-src", "data:image/png;base64,AAAA"));
        assert!(allows(&policy, "img-src", "https://images.example/a.png"));
        assert!(!allows(&policy, "img-src", "http://images.example/a.png"));
        assert!(allows(&policy, "connect-src", "https://api.example/v1"));
    }

    #[test]
    fn malformed_hash_sources_do_not_grant_access() {
        let policy = Policy::parse("script-src 'sha256-!!!not-base64!!!'");
        assert!(!policy.allows_inline_with("script-src", &ContentCheck::default()));
        assert!(!allows(&policy, "script-src", "https://app.example/a.js"));
    }
}
