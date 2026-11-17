//! A browsing-context frame: navigation plus event-loop delivery.
//!
//! Navigation transitions are queued as DOM-manipulation tasks rather than being
//! delivered synchronously. This matches the async nature of DOM event dispatch
//! and lets a host batch lifecycle events with other work. [`Frame::pump`] runs
//! the scheduler and collects the delivered transitions.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use browser_event_loop::{RunReport, Scheduler, TaskSource};
use browser_url::Url;

use crate::history::{NavError, Navigation, NavigationAction};
use crate::lifecycle::{LifecycleState, Transition};
use crate::navigator::Navigator;

/// One browsing context: a [`Navigator`], its scheduler and an event outbox.
#[derive(Debug)]
pub struct Frame {
    navigator: Navigator,
    scheduler: Scheduler,
    outbox: Arc<Mutex<Vec<Transition>>>,
}

impl Frame {
    /// Creates a frame with empty history and an empty event loop.
    pub fn new() -> Self {
        Self {
            navigator: Navigator::new(),
            scheduler: Scheduler::new(),
            outbox: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The navigator for this frame.
    pub fn navigator(&self) -> &Navigator {
        &self.navigator
    }

    /// Mutable access to the navigator, for updating titles and scroll positions.
    pub fn navigator_mut(&mut self) -> &mut Navigator {
        &mut self.navigator
    }

    /// The frame's scheduler.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Mutable access to the frame's scheduler for timers and tasks.
    pub fn scheduler_mut(&mut self) -> &mut Scheduler {
        &mut self.scheduler
    }

    /// The current lifecycle state.
    pub fn state(&self) -> LifecycleState {
        self.navigator.state()
    }

    /// Starts a navigation and schedules its transitions for delivery.
    pub fn navigate(&mut self, url: Url, action: NavigationAction) -> Result<Navigation, NavError> {
        let navigation = self.navigator.start_navigation(url, action)?;
        self.dispatch_transitions();
        Ok(navigation)
    }

    /// Marks the loading document interactive and schedules the transition.
    pub fn mark_interactive(&mut self) {
        self.navigator.mark_interactive();
        self.dispatch_transitions();
    }

    /// Finishes loading and schedules the completion transitions.
    pub fn finish_loading(&mut self) {
        self.navigator.finish_loading();
        self.dispatch_transitions();
    }

    /// Fails the current load and schedules the failure transitions.
    pub fn fail_loading(&mut self) {
        self.navigator.fail_loading();
        self.dispatch_transitions();
    }

    /// Runs ready event-loop work at `now` and returns the run counts.
    pub fn pump(&mut self, now: Instant) -> RunReport {
        self.scheduler.run_until_idle(now)
    }

    /// Runs ready work at `now` and returns the transitions it delivered.
    pub fn poll_events(&mut self, now: Instant) -> Vec<Transition> {
        self.pump(now);
        self.take_events()
    }

    /// Removes any delivered transitions without running the scheduler.
    pub fn take_events(&self) -> Vec<Transition> {
        let mut guard = self
            .outbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *guard)
    }

    fn dispatch_transitions(&mut self) {
        let transitions = self.navigator.take_transitions();
        if transitions.is_empty() {
            return;
        }
        let outbox = Arc::clone(&self.outbox);
        self.scheduler.queue_task(
            TaskSource::DomManipulation,
            Box::new(move || {
                let mut guard = outbox
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.extend(transitions);
            }),
        );
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: &str) -> Url {
        Url::parse(input).unwrap()
    }

    #[test]
    fn transitions_are_delivered_by_the_event_loop() {
        let mut frame = Frame::new();
        frame
            .navigate(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        // Queued, not delivered synchronously.
        assert!(frame.take_events().is_empty());
        assert_eq!(frame.scheduler().pending_tasks(), 1);
        let events = frame.poll_events(Instant::now());
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            Transition::LifecycleChanged {
                from: LifecycleState::Unloaded,
                to: LifecycleState::Loading,
            }
        ));
        assert!(matches!(events[1], Transition::NavigationStarted(_)));
        assert_eq!(frame.state(), LifecycleState::Loading);
        assert_eq!(frame.scheduler().pending_tasks(), 0);
    }

    #[test]
    fn lifecycle_events_keep_order_across_one_task() {
        let mut frame = Frame::new();
        frame
            .navigate(url("https://example.com/"), NavigationAction::Push)
            .unwrap();
        frame.mark_interactive();
        frame.finish_loading();
        let events = frame.poll_events(Instant::now());
        assert_eq!(events.len(), 5);
        assert_eq!(
            events[0],
            Transition::LifecycleChanged {
                from: LifecycleState::Unloaded,
                to: LifecycleState::Loading,
            }
        );
        assert_eq!(
            events[2],
            Transition::LifecycleChanged {
                from: LifecycleState::Loading,
                to: LifecycleState::Interactive,
            }
        );
        assert_eq!(
            events[3],
            Transition::LifecycleChanged {
                from: LifecycleState::Interactive,
                to: LifecycleState::Complete,
            }
        );
        assert_eq!(events[4], Transition::NavigationFinished);
        assert_eq!(frame.state(), LifecycleState::Complete);
    }

    #[test]
    fn failed_navigation_delivers_nothing() {
        let mut frame = Frame::new();
        assert_eq!(
            frame.navigate(url("https://example.com/"), NavigationAction::Back),
            Err(NavError::OutOfRange)
        );
        assert!(frame.poll_events(Instant::now()).is_empty());
        assert_eq!(frame.state(), LifecycleState::Unloaded);
    }

    #[test]
    fn frame_exposes_its_scheduler() {
        let mut frame = Frame::new();
        let id = frame
            .scheduler_mut()
            .set_timeout(std::time::Duration::ZERO, Box::new(|| {}));
        assert_eq!(frame.scheduler().pending_timers(), 1);
        let report = frame.pump(Instant::now());
        assert_eq!(report.timers, 1);
        assert!(!frame.scheduler_mut().clear_timer(id));
    }
}
