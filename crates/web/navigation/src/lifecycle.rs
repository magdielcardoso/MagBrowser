//! Document lifecycle states and transition events.

use crate::history::Navigation;

/// The document lifecycle state of a browsing context.
///
/// Mirrors the HTML `document.readyState` progression, with an explicit
/// `Unloaded` state for a context that has no document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LifecycleState {
    /// No document is loaded.
    #[default]
    Unloaded,
    /// The navigation is in flight and the document is being fetched.
    Loading,
    /// The document is parsed and deferred scripts may run.
    Interactive,
    /// Load events have fired and subresources are complete.
    Complete,
}

impl LifecycleState {
    /// The string exposed as `document.readyState`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::Loading => "loading",
            Self::Interactive => "interactive",
            Self::Complete => "complete",
        }
    }

    /// True once the document is at least interactive.
    pub const fn is_loaded(self) -> bool {
        matches!(self, Self::Interactive | Self::Complete)
    }
}

/// An observable lifecycle or navigation transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// The lifecycle state changed.
    LifecycleChanged {
        /// State before the change.
        from: LifecycleState,
        /// State after the change.
        to: LifecycleState,
    },
    /// A navigation started and the target URL is now loading.
    NavigationStarted(Navigation),
    /// The document finished loading and `load` may be dispatched.
    NavigationFinished,
    /// Loading failed and the document was discarded.
    NavigationFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_defaults_to_unloaded() {
        assert_eq!(LifecycleState::default(), LifecycleState::Unloaded);
        assert!(!LifecycleState::Unloaded.is_loaded());
        assert!(!LifecycleState::Loading.is_loaded());
        assert!(LifecycleState::Interactive.is_loaded());
        assert!(LifecycleState::Complete.is_loaded());
    }

    #[test]
    fn ready_state_strings_match_dom_values() {
        assert_eq!(LifecycleState::Unloaded.as_str(), "unloaded");
        assert_eq!(LifecycleState::Loading.as_str(), "loading");
        assert_eq!(LifecycleState::Interactive.as_str(), "interactive");
        assert_eq!(LifecycleState::Complete.as_str(), "complete");
    }
}
