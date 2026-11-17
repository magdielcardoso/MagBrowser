//! Per-origin permission decisions.
//!
//! [`PermissionStore`] mirrors the Permissions API: every permission defaults to
//! [`PermissionState::Prompt`] for every origin, and grants or denials are stored
//! per origin so one site's decision cannot affect another. Decisions are keyed by
//! the serialised origin, which is why opaque origins should be normalised by the
//! caller before reaching this store.

use std::collections::HashMap;

use crate::origin::OriginPolicy;

/// A capability a page can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Permission {
    /// Access to the device location.
    Geolocation,
    /// Display of system notifications.
    Notifications,
    /// Reading from the clipboard.
    ClipboardRead,
    /// Writing to the clipboard.
    ClipboardWrite,
    /// Camera capture.
    Camera,
    /// Microphone capture.
    Microphone,
    /// Fullscreen presentation.
    Fullscreen,
    /// Persistent storage (cookies, IndexedDB, ...).
    Storage,
}

impl Permission {
    /// The name used by the Permissions API query interface.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Geolocation => "geolocation",
            Self::Notifications => "notifications",
            Self::ClipboardRead => "clipboard-read",
            Self::ClipboardWrite => "clipboard-write",
            Self::Camera => "camera",
            Self::Microphone => "microphone",
            Self::Fullscreen => "fullscreen",
            Self::Storage => "persistent-storage",
        }
    }

    /// All permissions the engine currently models.
    pub const fn all() -> [Self; 8] {
        [
            Self::Geolocation,
            Self::Notifications,
            Self::ClipboardRead,
            Self::ClipboardWrite,
            Self::Camera,
            Self::Microphone,
            Self::Fullscreen,
            Self::Storage,
        ]
    }
}

/// The stored decision for a permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionState {
    /// Access is allowed without asking.
    Granted,
    /// Access is blocked.
    Denied,
    /// The user must be asked.
    #[default]
    Prompt,
}

/// Per-origin permission decisions.
#[derive(Debug, Clone, Default)]
pub struct PermissionStore {
    decisions: HashMap<String, HashMap<Permission, PermissionState>>,
}

impl PermissionStore {
    /// Creates an empty store; every permission defaults to `Prompt`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up a decision for an origin.
    pub fn check(&self, origin: &str, permission: Permission) -> PermissionState {
        self.decisions
            .get(origin)
            .and_then(|decisions| decisions.get(&permission))
            .copied()
            .unwrap_or_default()
    }

    /// Looks up a decision for a validated origin policy.
    pub fn check_origin(&self, origin: &OriginPolicy, permission: Permission) -> PermissionState {
        self.check(&origin.serialize(), permission)
    }

    /// Grants a permission for an origin.
    pub fn grant(&mut self, origin: &str, permission: Permission) {
        self.set(origin, permission, PermissionState::Granted);
    }

    /// Denies a permission for an origin.
    pub fn deny(&mut self, origin: &str, permission: Permission) {
        self.set(origin, permission, PermissionState::Denied);
    }

    /// Removes one stored decision, returning that origin's permission to `Prompt`.
    pub fn reset(&mut self, origin: &str, permission: Permission) {
        if let Some(decisions) = self.decisions.get_mut(origin) {
            decisions.remove(&permission);
            if decisions.is_empty() {
                self.decisions.remove(origin);
            }
        }
    }

    /// Removes every decision for an origin.
    pub fn reset_origin(&mut self, origin: &str) {
        self.decisions.remove(origin);
    }

    /// Removes every decision for every origin.
    pub fn clear(&mut self) {
        self.decisions.clear();
    }

    /// True when the store has at least one stored decision.
    pub fn is_empty(&self) -> bool {
        self.decisions.is_empty()
    }

    /// Number of origins with stored decisions.
    pub fn origin_count(&self) -> usize {
        self.decisions.len()
    }

    /// The raw decisions for one origin, for diagnostics and tests.
    pub fn decisions_for(&self, origin: &str) -> Option<&HashMap<Permission, PermissionState>> {
        self.decisions.get(origin)
    }

    fn set(&mut self, origin: &str, permission: Permission, state: PermissionState) {
        self.decisions
            .entry(origin.to_string())
            .or_default()
            .insert(permission, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_url::Url;

    const ORIGIN_A: &str = "https://a.example:443";
    const ORIGIN_B: &str = "https://b.example:443";

    #[test]
    fn every_permission_defaults_to_prompt() {
        let store = PermissionStore::new();
        for permission in Permission::all() {
            assert_eq!(
                store.check(ORIGIN_A, permission),
                PermissionState::Prompt,
                "{}",
                permission.name()
            );
        }
        assert!(store.is_empty());
    }

    #[test]
    fn grant_and_deny_are_remembered() {
        let mut store = PermissionStore::new();
        store.grant(ORIGIN_A, Permission::Camera);
        store.deny(ORIGIN_A, Permission::Microphone);
        assert_eq!(
            store.check(ORIGIN_A, Permission::Camera),
            PermissionState::Granted
        );
        assert_eq!(
            store.check(ORIGIN_A, Permission::Microphone),
            PermissionState::Denied
        );
        assert_eq!(store.origin_count(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn decisions_are_isolated_per_origin() {
        let mut store = PermissionStore::new();
        store.grant(ORIGIN_A, Permission::Geolocation);
        assert_eq!(
            store.check(ORIGIN_A, Permission::Geolocation),
            PermissionState::Granted
        );
        assert_eq!(
            store.check(ORIGIN_B, Permission::Geolocation),
            PermissionState::Prompt
        );
    }

    #[test]
    fn decisions_are_isolated_per_permission() {
        let mut store = PermissionStore::new();
        store.grant(ORIGIN_A, Permission::Camera);
        assert_eq!(
            store.check(ORIGIN_A, Permission::Camera),
            PermissionState::Granted
        );
        assert_eq!(
            store.check(ORIGIN_A, Permission::Microphone),
            PermissionState::Prompt
        );
    }

    #[test]
    fn reset_returns_to_prompt() {
        let mut store = PermissionStore::new();
        store.grant(ORIGIN_A, Permission::Notifications);
        store.reset(ORIGIN_A, Permission::Notifications);
        assert_eq!(
            store.check(ORIGIN_A, Permission::Notifications),
            PermissionState::Prompt
        );
        assert!(store.is_empty());
    }

    #[test]
    fn reset_origin_does_not_touch_other_origins() {
        let mut store = PermissionStore::new();
        store.grant(ORIGIN_A, Permission::Storage);
        store.deny(ORIGIN_B, Permission::Storage);
        store.reset_origin(ORIGIN_A);
        assert_eq!(
            store.check(ORIGIN_A, Permission::Storage),
            PermissionState::Prompt
        );
        assert_eq!(
            store.check(ORIGIN_B, Permission::Storage),
            PermissionState::Denied
        );
    }

    #[test]
    fn clear_removes_every_decision() {
        let mut store = PermissionStore::new();
        store.grant(ORIGIN_A, Permission::ClipboardRead);
        store.grant(ORIGIN_B, Permission::ClipboardWrite);
        store.clear();
        assert!(store.is_empty());
        assert_eq!(
            store.check(ORIGIN_A, Permission::ClipboardRead),
            PermissionState::Prompt
        );
    }

    #[test]
    fn origin_policy_lookup_uses_serialised_key() {
        let mut store = PermissionStore::new();
        let origin = OriginPolicy::from_url(&Url::parse("https://a.example/x").unwrap()).unwrap();
        store.grant(&origin.serialize(), Permission::Fullscreen);
        assert_eq!(
            store.check_origin(&origin, Permission::Fullscreen),
            PermissionState::Granted
        );
        assert_eq!(
            store.check_origin(&origin, Permission::Camera),
            PermissionState::Prompt
        );
    }

    #[test]
    fn decisions_for_exposes_stored_map() {
        let mut store = PermissionStore::new();
        store.grant(ORIGIN_A, Permission::Fullscreen);
        let decisions = store.decisions_for(ORIGIN_A).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions.get(&Permission::Fullscreen),
            Some(&PermissionState::Granted)
        );
        assert!(store.decisions_for(ORIGIN_B).is_none());
    }

    #[test]
    fn permission_names_are_stable() {
        assert_eq!(Permission::Geolocation.name(), "geolocation");
        assert_eq!(Permission::ClipboardRead.name(), "clipboard-read");
        assert_eq!(Permission::Storage.name(), "persistent-storage");
        assert_eq!(Permission::all().len(), 8);
    }
}
