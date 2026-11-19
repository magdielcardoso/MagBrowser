//! Mixed-content decisions for subresource loads.
//!
//! A document served over HTTPS must not fetch active resources (scripts,
//! styles, iframes, `fetch`/XHR, WebSocket connections) over HTTP: an attacker
//! on the network could replace them and run code in the secure page. Passive
//! content such as images is auto-upgraded to HTTPS by default, which keeps most
//! real sites working without opening an active-content hole.
//!
//! The checks are pure: callers pass the document URL (or an explicit secure
//! context flag, for `about:blank` and sandboxed documents) and the resolved
//! subresource URL, then act on the returned [`MixedContentDecision`].

use browser_url::Url;

/// How a subresource will be used by the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubresourceKind {
    /// Content that can execute script or read page data: scripts, stylesheets,
    /// iframes, fonts, form actions, `fetch`/XHR and WebSockets.
    Active,
    /// Content that is rendered but not executed: images, audio and video.
    Passive,
}

impl SubresourceKind {
    /// True for content that must be blocked rather than auto-upgraded.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// How strictly mixed content is handled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MixedContentMode {
    /// Block active mixed content, upgrade passive content. Browser default.
    #[default]
    UpgradePassive,
    /// Block every insecure subresource, including images.
    BlockAll,
    /// Rewrite every insecure subresource to HTTPS (CSP `upgrade-insecure-
    /// requests`).
    UpgradeAll,
}

/// The action a caller should take for a subresource request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixedContentDecision {
    /// The document is not a secure context, or the target does not use the
    /// network (for example a `data:` URL). Nothing to do.
    NotApplicable,
    /// The target is already trustworthy; load it as-is.
    Secure,
    /// Retry the load with [`upgraded_url`].
    Upgrade,
    /// Refuse the load.
    Block,
}

impl MixedContentDecision {
    /// True when the load may proceed (possibly after an upgrade).
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::NotApplicable | Self::Secure | Self::Upgrade)
    }

    /// True when the caller should retry with the upgraded URL.
    pub const fn requires_upgrade(self) -> bool {
        matches!(self, Self::Upgrade)
    }
}

/// Checks a subresource using the default browser-compatible mode.
pub fn check(document: &Url, target: &Url, kind: SubresourceKind) -> MixedContentDecision {
    check_with_mode(document, target, kind, MixedContentMode::default())
}

/// Checks a subresource with an explicit handling mode.
pub fn check_with_mode(
    document: &Url,
    target: &Url,
    kind: SubresourceKind,
    mode: MixedContentMode,
) -> MixedContentDecision {
    check_with_context(is_secure_context(document), target, kind, mode)
}

/// Checks a subresource when the caller already knows whether the document is a
/// secure context.
///
/// Use this for documents whose URL does not carry that information, such as
/// `about:blank`, srcdoc frames and sandboxed documents that inherited a secure
/// parent.
pub fn check_with_context(
    document_is_secure: bool,
    target: &Url,
    kind: SubresourceKind,
    mode: MixedContentMode,
) -> MixedContentDecision {
    if !document_is_secure {
        return MixedContentDecision::NotApplicable;
    }
    match target_class(target) {
        TargetClass::NotNetwork | TargetClass::Local => MixedContentDecision::NotApplicable,
        TargetClass::Trustworthy => MixedContentDecision::Secure,
        TargetClass::Upgradeable => match mode {
            MixedContentMode::UpgradeAll => MixedContentDecision::Upgrade,
            MixedContentMode::BlockAll => MixedContentDecision::Block,
            MixedContentMode::UpgradePassive if kind.is_active() => MixedContentDecision::Block,
            MixedContentMode::UpgradePassive => MixedContentDecision::Upgrade,
        },
        TargetClass::Insecure => MixedContentDecision::Block,
    }
}

/// True when a subresource load from `document` is not blocked by mixed content.
pub fn allows(document: &Url, target: &Url, kind: SubresourceKind) -> bool {
    check(document, target, kind).is_allowed()
}

/// Rewrites an `http:` or `ws:` URL to its secure counterpart.
///
/// An explicit default port (`:80`) is dropped and the path, query and fragment
/// are preserved. Returns `None` for URLs that cannot be upgraded.
pub fn upgraded_url(target: &Url) -> Option<Url> {
    let scheme = match target.scheme() {
        "http" => "https",
        "ws" => "wss",
        _ => return None,
    };
    // `host_header` omits the port when it is the default for the original
    // scheme, so `http://example.com:80/x` becomes `https://example.com/x`.
    let authority = target.host_header()?;
    let mut upgraded = format!("{scheme}://{authority}{}", target.path());
    if let Some(query) = target.query() {
        upgraded.push('?');
        upgraded.push_str(query);
    }
    if let Some(fragment) = target.fragment() {
        upgraded.push('#');
        upgraded.push_str(fragment);
    }
    Url::parse(&upgraded).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetClass {
    /// `data:`, `blob:`, `file:`, ... — no network fetch to block or upgrade.
    NotNetwork,
    /// `https:` / `wss:` — already secure.
    Trustworthy,
    /// Loopback names are not a network threat even over plain HTTP.
    Local,
    /// `http:` / `ws:` — can be retried over TLS.
    Upgradeable,
    /// Other host-bearing schemes (for example `ftp:`) cannot be upgraded.
    Insecure,
}

fn target_class(target: &Url) -> TargetClass {
    match target.scheme() {
        "https" | "wss" => TargetClass::Trustworthy,
        "http" | "ws" if is_localhost(target) => TargetClass::Local,
        "http" | "ws" => TargetClass::Upgradeable,
        "file" | "data" | "blob" | "about" | "javascript" | "mailto" => TargetClass::NotNetwork,
        _ if target.is_special() && target.host().is_some() => TargetClass::Insecure,
        _ => TargetClass::NotNetwork,
    }
}

fn is_secure_context(document: &Url) -> bool {
    match document.scheme() {
        "https" | "wss" => true,
        "http" | "ws" => is_localhost(document),
        _ => false,
    }
}

fn is_localhost(url: &Url) -> bool {
    let Some(host) = url.host() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("127.0.0.1")
        || host.eq_ignore_ascii_case("::1")
        || host.to_ascii_lowercase().ends_with(".localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).unwrap()
    }

    const PAGE: &str = "https://app.example/page";

    #[test]
    fn active_mixed_content_is_blocked() {
        for resource in ["http://cdn.example/app.js", "ws://chat.example/socket"] {
            assert_eq!(
                check(&url(PAGE), &url(resource), SubresourceKind::Active),
                MixedContentDecision::Block,
                "{resource}"
            );
        }
    }

    #[test]
    fn passive_mixed_content_is_upgraded() {
        let decision = check(
            &url(PAGE),
            &url("http://images.example/a.png?v=1#frag"),
            SubresourceKind::Passive,
        );
        assert_eq!(decision, MixedContentDecision::Upgrade);
        assert!(decision.is_allowed());
        assert!(decision.requires_upgrade());
        assert_eq!(
            upgraded_url(&url("http://images.example/a.png?v=1#frag"))
                .unwrap()
                .as_str(),
            "https://images.example/a.png?v=1#frag"
        );
    }

    #[test]
    fn insecure_pages_are_not_restricted() {
        for resource in ["http://cdn.example/app.js", "http://images.example/a.png"] {
            assert_eq!(
                check(
                    &url("http://app.example/page"),
                    &url(resource),
                    SubresourceKind::Active
                ),
                MixedContentDecision::NotApplicable,
                "{resource}"
            );
        }
    }

    #[test]
    fn secure_targets_load_as_is() {
        for resource in ["https://cdn.example/a.js", "wss://chat.example/socket"] {
            assert_eq!(
                check(&url(PAGE), &url(resource), SubresourceKind::Active),
                MixedContentDecision::Secure,
                "{resource}"
            );
        }
    }

    #[test]
    fn non_network_targets_load_as_is() {
        for resource in [
            "data:image/png;base64,AAAA",
            "blob:https://app.example/uuid",
            "about:blank",
        ] {
            assert_eq!(
                check(&url(PAGE), &url(resource), SubresourceKind::Passive),
                MixedContentDecision::NotApplicable,
                "{resource}"
            );
        }
    }

    #[test]
    fn localhost_is_treated_as_secure() {
        // Localhost pages are trustworthy contexts, so they still block mixed
        // content from remote hosts.
        for page in [
            "http://localhost:8080/",
            "http://127.0.0.1/",
            "http://dev.localhost/",
        ] {
            assert_eq!(
                check(
                    &url(page),
                    &url("http://cdn.example/app.js"),
                    SubresourceKind::Active
                ),
                MixedContentDecision::Block,
                "{page}"
            );
        }
        // Loopback targets from a secure page are not mixed content either.
        assert_eq!(
            check(
                &url(PAGE),
                &url("http://localhost:3000/api"),
                SubresourceKind::Active
            ),
            MixedContentDecision::NotApplicable
        );
    }

    #[test]
    fn strict_mode_blocks_passive_content() {
        assert_eq!(
            check_with_mode(
                &url(PAGE),
                &url("http://images.example/a.png"),
                SubresourceKind::Passive,
                MixedContentMode::BlockAll
            ),
            MixedContentDecision::Block
        );
    }

    #[test]
    fn upgrade_all_mode_upgrades_active_content() {
        assert_eq!(
            check_with_mode(
                &url(PAGE),
                &url("http://cdn.example/app.js"),
                SubresourceKind::Active,
                MixedContentMode::UpgradeAll
            ),
            MixedContentDecision::Upgrade
        );
    }

    #[test]
    fn explicit_secure_context_handles_blank_documents() {
        let target = url("http://cdn.example/app.js");
        assert_eq!(
            check_with_context(
                true,
                &target,
                SubresourceKind::Active,
                MixedContentMode::UpgradePassive
            ),
            MixedContentDecision::Block
        );
        assert_eq!(
            check_with_context(
                false,
                &target,
                SubresourceKind::Active,
                MixedContentMode::UpgradePassive
            ),
            MixedContentDecision::NotApplicable
        );
    }

    #[test]
    fn upgradable_and_non_upgradable_schemes_are_distinguished() {
        assert_eq!(
            check(
                &url(PAGE),
                &url("ftp://files.example/a.png"),
                SubresourceKind::Passive
            ),
            MixedContentDecision::Block
        );
        assert_eq!(
            upgraded_url(&url("ws://chat.example/socket"))
                .unwrap()
                .as_str(),
            "wss://chat.example/socket"
        );
        assert_eq!(upgraded_url(&url("https://cdn.example/a.js")), None);
    }

    #[test]
    fn explicit_default_port_is_dropped_when_upgrading() {
        assert_eq!(
            upgraded_url(&url("http://cdn.example:80/app.js"))
                .unwrap()
                .as_str(),
            "https://cdn.example/app.js"
        );
        assert_eq!(
            upgraded_url(&url("http://cdn.example:8443/app.js"))
                .unwrap()
                .as_str(),
            "https://cdn.example:8443/app.js"
        );
    }

    #[test]
    fn allows_matches_decision() {
        assert!(allows(
            &url(PAGE),
            &url("https://cdn.example/app.js"),
            SubresourceKind::Active
        ));
        assert!(!allows(
            &url(PAGE),
            &url("http://cdn.example/app.js"),
            SubresourceKind::Active
        ));
        assert!(allows(
            &url(PAGE),
            &url("http://images.example/a.png"),
            SubresourceKind::Passive
        ));
    }
}
