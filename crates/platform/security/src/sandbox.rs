//! iframe `sandbox` attribute parsing and capability checks.
//!
//! A sandboxed document starts with every capability denied. Each token in the
//! `sandbox` attribute re-enables exactly one capability. Without
//! `allow-same-origin`, [`Sandbox::origin_for`] returns an opaque origin, forcing
//! the document into an isolated origin (its own storage, no same-origin access to
//! its parent).

use crate::origin::OriginPolicy;

/// The flags enabled by a `sandbox` attribute.
///
/// All fields default to `false`, which is the most restrictive sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SandboxFlags {
    /// `allow-scripts`
    pub allow_scripts: bool,
    /// `allow-same-origin`
    pub allow_same_origin: bool,
    /// `allow-forms`
    pub allow_forms: bool,
    /// `allow-popups`
    pub allow_popups: bool,
    /// `allow-downloads`
    pub allow_downloads: bool,
    /// `allow-modals`
    pub allow_modals: bool,
}

impl SandboxFlags {
    /// A fully permissive set of flags (no sandbox restrictions).
    pub const fn all() -> Self {
        Self {
            allow_scripts: true,
            allow_same_origin: true,
            allow_forms: true,
            allow_popups: true,
            allow_downloads: true,
            allow_modals: true,
        }
    }
}

/// A capability that a sandbox can grant or withhold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SandboxCapability {
    /// Run scripts.
    Scripts,
    /// Keep the document's real origin instead of an opaque one.
    SameOrigin,
    /// Submit forms.
    Forms,
    /// Open popups and auxiliary browsing contexts.
    Popups,
    /// Start downloads.
    Downloads,
    /// Show modal dialogs (`alert`, `confirm`, `prompt`, ...).
    Modals,
}

/// A parsed sandbox policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sandbox {
    flags: SandboxFlags,
}

impl Sandbox {
    /// Creates a sandbox from explicit flags.
    pub const fn new(flags: SandboxFlags) -> Self {
        Self { flags }
    }

    /// Parses a `sandbox` attribute value.
    ///
    /// An empty attribute is the most restrictive sandbox and denies everything.
    pub fn from_attribute(value: &str) -> Self {
        Self::new(parse_sandbox_attribute(value))
    }

    /// The enabled flags.
    pub const fn flags(self) -> SandboxFlags {
        self.flags
    }

    /// True when the sandbox allows `capability`.
    pub const fn can(self, capability: SandboxCapability) -> bool {
        match capability {
            SandboxCapability::Scripts => self.flags.allow_scripts,
            SandboxCapability::SameOrigin => self.flags.allow_same_origin,
            SandboxCapability::Forms => self.flags.allow_forms,
            SandboxCapability::Popups => self.flags.allow_popups,
            SandboxCapability::Downloads => self.flags.allow_downloads,
            SandboxCapability::Modals => self.flags.allow_modals,
        }
    }

    /// The effective origin for a document inside this sandbox.
    ///
    /// Returns an opaque origin unless `allow-same-origin` is present. An already
    /// opaque base origin stays opaque.
    pub fn origin_for(&self, base_origin: impl Into<OriginPolicy>) -> OriginPolicy {
        let base_origin = base_origin.into();
        if self.flags.allow_same_origin && !base_origin.is_opaque() {
            base_origin
        } else {
            OriginPolicy::opaque()
        }
    }
}

/// Parses the tokens of a `sandbox` attribute into flags.
///
/// Recognised tokens are `allow-scripts`, `allow-same-origin`, `allow-forms`,
/// `allow-popups`, `allow-downloads` and `allow-modals`. Unknown tokens are
/// ignored; an empty string enables nothing.
pub fn parse_sandbox_attribute(value: &str) -> SandboxFlags {
    let mut flags = SandboxFlags::default();
    for token in value.split_ascii_whitespace() {
        match token.to_ascii_lowercase().as_str() {
            "allow-scripts" => flags.allow_scripts = true,
            "allow-same-origin" => flags.allow_same_origin = true,
            "allow-forms" => flags.allow_forms = true,
            "allow-popups" => flags.allow_popups = true,
            "allow-downloads" => flags.allow_downloads = true,
            "allow-modals" => flags.allow_modals = true,
            _ => {}
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_url::Url;

    fn base_origin() -> OriginPolicy {
        OriginPolicy::from_url(&Url::parse("https://app.example/page").unwrap()).unwrap()
    }

    #[test]
    fn default_sandbox_denies_every_capability() {
        let sandbox = Sandbox::default();
        for capability in [
            SandboxCapability::Scripts,
            SandboxCapability::SameOrigin,
            SandboxCapability::Forms,
            SandboxCapability::Popups,
            SandboxCapability::Downloads,
            SandboxCapability::Modals,
        ] {
            assert!(!sandbox.can(capability), "{capability:?}");
        }
        assert_eq!(sandbox.flags(), SandboxFlags::default());
        assert_eq!(
            sandbox.flags(),
            SandboxFlags {
                allow_scripts: false,
                allow_same_origin: false,
                allow_forms: false,
                allow_popups: false,
                allow_downloads: false,
                allow_modals: false,
            }
        );
    }

    #[test]
    fn empty_attribute_denies_everything() {
        let sandbox = Sandbox::from_attribute("");
        assert_eq!(sandbox.flags(), SandboxFlags::default());
        assert!(!sandbox.can(SandboxCapability::Scripts));
    }

    #[test]
    fn parses_individual_tokens() {
        let cases = [
            ("allow-scripts", SandboxCapability::Scripts),
            ("allow-same-origin", SandboxCapability::SameOrigin),
            ("allow-forms", SandboxCapability::Forms),
            ("allow-popups", SandboxCapability::Popups),
            ("allow-downloads", SandboxCapability::Downloads),
            ("allow-modals", SandboxCapability::Modals),
        ];
        for (token, capability) in cases {
            let sandbox = Sandbox::from_attribute(token);
            assert!(sandbox.can(capability), "{token}");
        }
    }

    #[test]
    fn parses_multiple_tokens_with_case_and_whitespace() {
        let flags = parse_sandbox_attribute("  allow-scripts\tALLOW-FORMS\nallow-popups ");
        assert!(flags.allow_scripts);
        assert!(flags.allow_forms);
        assert!(flags.allow_popups);
        assert!(!flags.allow_same_origin);
        assert!(!flags.allow_downloads);
        assert!(!flags.allow_modals);
    }

    #[test]
    fn unknown_tokens_are_ignored() {
        let flags = parse_sandbox_attribute(
            "allow-scripts allow-top-navigation allow-popups-to-escape-sandbox nonsense",
        );
        assert!(flags.allow_scripts);
        assert_eq!(
            flags,
            SandboxFlags {
                allow_scripts: true,
                ..SandboxFlags::default()
            }
        );
    }

    #[test]
    fn all_tokens_enable_every_flag() {
        let sandbox = Sandbox::from_attribute(
            "allow-scripts allow-same-origin allow-forms allow-popups allow-downloads allow-modals",
        );
        assert_eq!(sandbox.flags(), SandboxFlags::all());
    }

    #[test]
    fn capabilities_map_to_the_right_flags() {
        let sandbox = Sandbox::from_attribute("allow-scripts allow-downloads");
        assert!(sandbox.can(SandboxCapability::Scripts));
        assert!(sandbox.can(SandboxCapability::Downloads));
        assert!(!sandbox.can(SandboxCapability::SameOrigin));
        assert!(!sandbox.can(SandboxCapability::Forms));
        assert!(!sandbox.can(SandboxCapability::Popups));
        assert!(!sandbox.can(SandboxCapability::Modals));
    }

    #[test]
    fn same_origin_flag_preserves_the_base_origin() {
        let base = base_origin();
        let sandbox = Sandbox::from_attribute("allow-same-origin");
        let effective = sandbox.origin_for(&base);
        assert!(!effective.is_opaque());
        assert!(effective.same_origin(&base));
    }

    #[test]
    fn missing_same_origin_flag_produces_an_opaque_origin() {
        let base = base_origin();
        let sandbox = Sandbox::from_attribute("allow-scripts");
        let effective = sandbox.origin_for(&base);
        assert!(effective.is_opaque());
        assert!(!effective.same_origin(&base));
        assert_eq!(effective.serialize(), "null");
    }

    #[test]
    fn opaque_base_origin_stays_opaque() {
        let sandbox = Sandbox::from_attribute("allow-same-origin");
        let effective = sandbox.origin_for(OriginPolicy::opaque());
        assert!(effective.is_opaque());
    }

    #[test]
    fn scripts_do_not_imply_same_origin() {
        let sandbox = Sandbox::from_attribute("allow-scripts");
        assert!(sandbox.can(SandboxCapability::Scripts));
        assert!(!sandbox.can(SandboxCapability::SameOrigin));
        assert!(sandbox.origin_for(base_origin()).is_opaque());
    }
}
