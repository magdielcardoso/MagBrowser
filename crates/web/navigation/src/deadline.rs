//! Wall-clock deadline and loop guard for a single navigation pipeline.
//!
//! A [`NavigationBudget`] is created when a navigation starts and is shared by
//! every stage of the pipeline: the main document, stylesheets, images, scripts
//! and the layout/paint passes they trigger. Before doing any work a stage calls
//! [`NavigationBudget::allow`]; once the wall clock passes the deadline (or a
//! chained-navigation loop is detected) the budget denies further work so the
//! caller can stop gracefully and present a usable partial page instead of
//! hanging on a heavy site.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Why a navigation pipeline stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncation {
    /// The wall-clock deadline elapsed.
    Deadline,
    /// Too many chained (`location.href`) navigations were followed.
    TooManyHops,
    /// A chained navigation revisited a URL already seen in this pipeline.
    Loop,
}

impl std::fmt::Display for Truncation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline => f.write_str("navigation deadline exceeded"),
            Self::TooManyHops => f.write_str("too many chained navigations"),
            Self::Loop => f.write_str("navigation loop detected"),
        }
    }
}

impl std::error::Error for Truncation {}

/// A single wall-clock deadline measured from when navigation starts.
///
/// Unlike the HTTP client's per-request timeout, this spans the *whole* load,
/// so a page with many slow subresources cannot extend the load indefinitely.
#[derive(Debug, Clone, Copy)]
pub struct NavigationDeadline {
    start: Instant,
    duration: Duration,
}

impl NavigationDeadline {
    /// Starts a new deadline of `duration` running from now.
    pub fn new(duration: Duration) -> Self {
        Self {
            start: Instant::now(),
            duration,
        }
    }

    /// Builds a deadline from an explicit start instant (useful for tests).
    pub fn from_parts(start: Instant, duration: Duration) -> Self {
        Self { start, duration }
    }

    /// Time elapsed since the deadline started.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// The budget this deadline was created with.
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Time left before the deadline, or zero when it has passed.
    pub fn remaining(&self) -> Duration {
        self.duration.saturating_sub(self.elapsed())
    }

    /// True once the wall clock has reached the deadline.
    pub fn is_expired(&self) -> bool {
        self.elapsed() >= self.duration
    }

    /// `Ok(())` while time remains, otherwise [`Truncation::Deadline`].
    pub fn check(&self) -> Result<(), Truncation> {
        if self.is_expired() {
            Err(Truncation::Deadline)
        } else {
            Ok(())
        }
    }
}

/// Shared budget for one navigation pipeline.
///
/// Tracks the deadline together with the chained-navigation hops already taken.
/// It never performs I/O itself; it only answers whether the caller may proceed.
#[derive(Debug)]
pub struct NavigationBudget {
    deadline: NavigationDeadline,
    max_hops: usize,
    hops: usize,
    visited: HashSet<String>,
    truncation: Option<Truncation>,
}

impl NavigationBudget {
    /// Default cap for chained script navigations inside one pipeline.
    pub const DEFAULT_MAX_HOPS: usize = 16;

    /// Starts a budget with the default hop cap.
    pub fn new(timeout: Duration) -> Self {
        Self::with_max_hops(timeout, Self::DEFAULT_MAX_HOPS)
    }

    /// Starts a budget with an explicit cap on chained navigations.
    pub fn with_max_hops(timeout: Duration, max_hops: usize) -> Self {
        Self {
            deadline: NavigationDeadline::new(timeout),
            max_hops,
            hops: 0,
            visited: HashSet::new(),
            truncation: None,
        }
    }

    /// The underlying wall-clock deadline.
    pub const fn deadline(&self) -> &NavigationDeadline {
        &self.deadline
    }

    /// Whether the deadline has passed.
    pub fn is_expired(&self) -> bool {
        self.deadline.is_expired()
    }

    /// Time elapsed since the budget started.
    pub fn elapsed(&self) -> Duration {
        self.deadline.elapsed()
    }

    /// Time left in the budget.
    pub fn remaining(&self) -> Duration {
        self.deadline.remaining()
    }

    /// The first reason this pipeline was truncated, if any.
    pub const fn truncation(&self) -> Option<Truncation> {
        self.truncation
    }

    /// Whether this pipeline was truncated.
    pub const fn is_truncated(&self) -> bool {
        self.truncation.is_some()
    }

    /// Number of chained navigations reserved so far.
    pub const fn hops(&self) -> usize {
        self.hops
    }

    /// Returns `true` while another stage of work is allowed.
    ///
    /// A `false` result records the truncation reason (only the first one sticks
    /// so callers can report a single, stable cause).
    pub fn allow(&mut self) -> bool {
        match self.deadline.check() {
            Ok(()) => true,
            Err(reason) => {
                self.truncate(reason);
                false
            }
        }
    }

    /// Reserves one chained navigation to `url`.
    ///
    /// Returns the truncation reason and denies the hop when time is up, when
    /// the hop cap is reached, or when the URL was already visited in this
    /// pipeline (a redirect/preload loop).
    pub fn begin_hop(&mut self, url: &str) -> Result<(), Truncation> {
        if !self.allow() {
            return Err(Truncation::Deadline);
        }
        if self.hops >= self.max_hops {
            self.truncate(Truncation::TooManyHops);
            return Err(Truncation::TooManyHops);
        }
        if !self.visited.insert(url.to_string()) {
            self.truncate(Truncation::Loop);
            return Err(Truncation::Loop);
        }
        self.hops += 1;
        Ok(())
    }

    fn truncate(&mut self, reason: Truncation) {
        if self.truncation.is_none() {
            self.truncation = Some(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_reports_elapsed_and_remaining() {
        let start = Instant::now() - Duration::from_millis(40);
        let deadline = NavigationDeadline::from_parts(start, Duration::from_millis(100));
        assert!(!deadline.is_expired());
        assert!(deadline.elapsed() >= Duration::from_millis(40));
        assert!(deadline.remaining() <= Duration::from_millis(60));
        assert_eq!(deadline.duration(), Duration::from_millis(100));
        assert_eq!(deadline.check(), Ok(()));
    }

    #[test]
    fn zero_duration_deadline_is_immediately_expired() {
        let deadline = NavigationDeadline::from_parts(Instant::now(), Duration::ZERO);
        assert!(deadline.is_expired());
        assert_eq!(deadline.remaining(), Duration::ZERO);
        assert_eq!(deadline.check(), Err(Truncation::Deadline));
    }

    #[test]
    fn budget_denies_work_after_deadline_and_records_reason() {
        let start = Instant::now() - Duration::from_secs(5);
        let mut budget = NavigationBudget::with_max_hops(Duration::from_millis(1), 4);
        // Replace the freshly created deadline with an already-expired one.
        budget.deadline = NavigationDeadline::from_parts(start, Duration::from_secs(1));
        assert!(!budget.allow());
        assert!(!budget.allow());
        assert_eq!(budget.truncation(), Some(Truncation::Deadline));
        assert!(budget.is_truncated());
    }

    #[test]
    fn budget_allows_work_while_time_remains() {
        let mut budget = NavigationBudget::new(Duration::from_secs(30));
        assert!(budget.allow());
        assert!(budget.allow());
        assert!(!budget.is_truncated());
        assert_eq!(budget.truncation(), None);
    }

    #[test]
    fn budget_rejects_repeated_urls_as_a_loop() {
        let mut budget = NavigationBudget::new(Duration::from_secs(30));
        assert_eq!(budget.begin_hop("https://example.com/b"), Ok(()));
        assert_eq!(budget.begin_hop("https://example.com/a"), Ok(()));
        assert_eq!(
            budget.begin_hop("https://example.com/b"),
            Err(Truncation::Loop)
        );
        assert_eq!(budget.hops(), 2);
        assert_eq!(budget.truncation(), Some(Truncation::Loop));
    }

    #[test]
    fn budget_caps_chained_navigations() {
        let mut budget = NavigationBudget::with_max_hops(Duration::from_secs(30), 2);
        assert_eq!(budget.begin_hop("https://example.com/1"), Ok(()));
        assert_eq!(budget.begin_hop("https://example.com/2"), Ok(()));
        assert_eq!(
            budget.begin_hop("https://example.com/3"),
            Err(Truncation::TooManyHops)
        );
        assert_eq!(budget.truncation(), Some(Truncation::TooManyHops));
    }

    #[test]
    fn first_truncation_reason_is_kept() {
        let mut budget = NavigationBudget::with_max_hops(Duration::from_secs(30), 1);
        assert_eq!(budget.begin_hop("https://example.com/1"), Ok(()));
        assert_eq!(
            budget.begin_hop("https://example.com/2"),
            Err(Truncation::TooManyHops)
        );
        assert_eq!(
            budget.begin_hop("https://example.com/1"),
            Err(Truncation::TooManyHops)
        );
        assert_eq!(budget.truncation(), Some(Truncation::TooManyHops));
    }
}
