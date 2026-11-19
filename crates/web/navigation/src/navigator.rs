//! The navigator: history plus the document lifecycle state machine.
//!
//! [`Navigator`] owns a [`SessionHistory`] and the current [`LifecycleState`]. Every
//! state change is recorded as a [`Transition`] so a host can dispatch DOM events
//! (`load`, `pageshow`, ...) without the navigation model depending on the DOM.

use browser_url::Url;

use crate::history::{HistoryEntry, NavError, Navigation, NavigationAction, SessionHistory};
use crate::lifecycle::{LifecycleState, Transition};

/// Drives navigation and the document lifecycle for one browsing context.
#[derive(Debug, Clone, Default)]
pub struct Navigator {
    history: SessionHistory,
    state: LifecycleState,
    transitions: Vec<Transition>,
}

impl Navigator {
    /// Creates a navigator with empty history in the `Unloaded` state.
    pub fn new() -> Self {
        Self::default()
    }

    /// The lifecycle state.
    pub fn state(&self) -> LifecycleState {
        self.state
    }

    /// Read-only access to session history.
    pub fn history(&self) -> &SessionHistory {
        &self.history
    }

    /// Mutable access to session history.
    ///
    /// Prefer [`Self::start_navigation`] for ordinary navigation; direct mutation is for
    /// updating titles and scroll positions.
    pub fn history_mut(&mut self) -> &mut SessionHistory {
        &mut self.history
    }

    /// The current history entry, if any.
    pub fn current(&self) -> Option<&HistoryEntry> {
        self.history.current()
    }

    /// Number of history entries.
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// True when no document has been loaded.
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// True when a back traversal is possible.
    pub fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    /// True when a forward traversal is possible.
    pub fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    /// Starts a navigation and moves the lifecycle to [`LifecycleState::Loading`].
    ///
    /// The URL is ignored for `Back`, `Forward`, `Go` and `Reload` because those
    /// actions take their target from history. On error neither history nor the
    /// lifecycle state changes.
    pub fn start_navigation(
        &mut self,
        url: Url,
        action: NavigationAction,
    ) -> Result<Navigation, NavError> {
        let navigation = self.history.navigate(action, Some(url), "")?;
        let from = self.state;
        self.state = LifecycleState::Loading;
        self.transitions.push(Transition::LifecycleChanged {
            from,
            to: LifecycleState::Loading,
        });
        self.transitions
            .push(Transition::NavigationStarted(navigation.clone()));
        Ok(navigation)
    }

    /// Marks a loading document as interactive (`DOMContentLoaded` time).
    ///
    /// A no-op unless the navigator is currently loading.
    pub fn mark_interactive(&mut self) {
        if self.state != LifecycleState::Loading {
            return;
        }
        self.change_state(LifecycleState::Interactive);
    }

    /// Completes loading and records that the `load` event may fire.
    ///
    /// A no-op outside `Loading` and `Interactive`. `Complete` is terminal until
    /// the next navigation.
    pub fn finish_loading(&mut self) {
        if !matches!(
            self.state,
            LifecycleState::Loading | LifecycleState::Interactive
        ) {
            return;
        }
        self.change_state(LifecycleState::Complete);
        self.transitions.push(Transition::NavigationFinished);
    }

    /// Abandons a load in progress and returns to [`LifecycleState::Unloaded`].
    ///
    /// A no-op outside `Loading` and `Interactive`.
    pub fn fail_loading(&mut self) {
        if !matches!(
            self.state,
            LifecycleState::Loading | LifecycleState::Interactive
        ) {
            return;
        }
        self.change_state(LifecycleState::Unloaded);
        self.transitions.push(Transition::NavigationFailed(
            "document load failed".to_string(),
        ));
    }

    /// Queued transitions not yet handed to the host.
    pub fn transitions(&self) -> &[Transition] {
        &self.transitions
    }

    /// Removes and returns all queued transitions.
    pub fn take_transitions(&mut self) -> Vec<Transition> {
        std::mem::take(&mut self.transitions)
    }

    /// Drops queued transitions.
    pub fn clear_transitions(&mut self) {
        self.transitions.clear();
    }

    fn change_state(&mut self, to: LifecycleState) {
        let from = self.state;
        if from == to {
            return;
        }
        self.state = to;
        self.transitions
            .push(Transition::LifecycleChanged { from, to });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).unwrap()
    }

    #[test]
    fn navigation_enters_loading_and_records_transitions() {
        let mut navigator = Navigator::new();
        assert_eq!(navigator.state(), LifecycleState::Unloaded);
        let nav = navigator
            .start_navigation(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        assert_eq!(nav.url.as_str(), "https://example.com/");
        assert_eq!(navigator.state(), LifecycleState::Loading);
        assert_eq!(navigator.len(), 1);
        let transitions = navigator.take_transitions();
        assert_eq!(transitions.len(), 2);
        assert_eq!(
            transitions[0],
            Transition::LifecycleChanged {
                from: LifecycleState::Unloaded,
                to: LifecycleState::Loading,
            }
        );
        assert!(matches!(
            &transitions[1],
            Transition::NavigationStarted(n) if n.url.as_str() == "https://example.com/"
        ));
        assert!(navigator.transitions().is_empty());
    }

    #[test]
    fn interactive_then_complete_records_each_state() {
        let mut navigator = Navigator::new();
        navigator
            .start_navigation(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        navigator.clear_transitions();
        navigator.mark_interactive();
        assert_eq!(navigator.state(), LifecycleState::Interactive);
        navigator.finish_loading();
        assert_eq!(navigator.state(), LifecycleState::Complete);
        assert_eq!(
            navigator.transitions(),
            [
                Transition::LifecycleChanged {
                    from: LifecycleState::Loading,
                    to: LifecycleState::Interactive,
                },
                Transition::LifecycleChanged {
                    from: LifecycleState::Interactive,
                    to: LifecycleState::Complete,
                },
                Transition::NavigationFinished,
            ]
        );
    }

    #[test]
    fn finish_loading_can_skip_interactive() {
        let mut navigator = Navigator::new();
        navigator
            .start_navigation(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        navigator.finish_loading();
        assert_eq!(navigator.state(), LifecycleState::Complete);
        // Completing twice is a no-op.
        let before = navigator.transitions().to_vec();
        navigator.finish_loading();
        assert_eq!(navigator.transitions(), before.as_slice());
    }

    #[test]
    fn fail_loading_returns_to_unloaded() {
        let mut navigator = Navigator::new();
        navigator
            .start_navigation(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        navigator.clear_transitions();
        navigator.fail_loading();
        assert_eq!(navigator.state(), LifecycleState::Unloaded);
        assert_eq!(
            navigator.transitions()[0],
            Transition::LifecycleChanged {
                from: LifecycleState::Loading,
                to: LifecycleState::Unloaded,
            }
        );
        assert!(
            navigator
                .transitions()
                .iter()
                .any(|t| matches!(t, Transition::NavigationFailed(_)))
        );
        // A second failure is ignored.
        navigator.clear_transitions();
        navigator.fail_loading();
        assert!(navigator.transitions().is_empty());
    }

    #[test]
    fn failed_navigation_does_not_change_state_or_history() {
        let mut navigator = Navigator::new();
        assert_eq!(
            navigator.start_navigation(url("https://example.com/"), NavigationAction::Back),
            Err(NavError::OutOfRange)
        );
        assert_eq!(navigator.state(), LifecycleState::Unloaded);
        assert!(navigator.is_empty());
        assert!(navigator.transitions().is_empty());
    }

    #[test]
    fn back_and_forward_delegate_to_history() {
        let mut navigator = Navigator::new();
        for path in ["/1", "/2", "/3"] {
            let target = Url::parse("https://example.com")
                .unwrap()
                .join(path)
                .unwrap();
            navigator
                .start_navigation(target, NavigationAction::Push)
                .unwrap();
            navigator.finish_loading();
        }
        assert!(navigator.can_go_back());
        assert!(!navigator.can_go_forward());
        navigator
            .start_navigation(url("https://example.com/ignored"), NavigationAction::Back)
            .unwrap();
        assert_eq!(
            navigator.current().unwrap().url.as_str(),
            "https://example.com/2"
        );
        assert_eq!(navigator.state(), LifecycleState::Loading);
        navigator.finish_loading();
        assert!(navigator.can_go_forward());
        navigator
            .start_navigation(
                url("https://example.com/ignored"),
                NavigationAction::Forward,
            )
            .unwrap();
        assert_eq!(
            navigator.current().unwrap().url.as_str(),
            "https://example.com/3"
        );
    }

    #[test]
    fn reload_starts_loading_without_growing_history() {
        let mut navigator = Navigator::new();
        navigator
            .start_navigation(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        navigator.finish_loading();
        navigator
            .start_navigation(url("https://example.com/other"), NavigationAction::Reload)
            .unwrap();
        assert_eq!(navigator.len(), 1);
        assert_eq!(
            navigator.current().unwrap().url.as_str(),
            "https://example.com/"
        );
        assert_eq!(navigator.state(), LifecycleState::Loading);
    }

    #[test]
    fn history_accessors_expose_titles_and_scroll() {
        let mut navigator = Navigator::new();
        navigator
            .start_navigation(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        navigator.history_mut().set_current_title("Example");
        navigator.history_mut().set_current_scroll(5.0, 6.0);
        let entry = navigator.current().unwrap();
        assert_eq!(entry.title, "Example");
        assert_eq!(entry.scroll, (5.0, 6.0));
    }
}
