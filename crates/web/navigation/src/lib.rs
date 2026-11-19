//! Session history and page navigation.
//!
//! The navigation model separates three concerns:
//!
//! * [`SessionHistory`] is the list of committed entries for one browsing context,
//!   including the current index and per-entry scroll restoration data.
//! * [`Navigator`] adds the document [`LifecycleState`] machine and records
//!   [`Transition`] events for the host.
//! * [`Frame`] wires the navigator to the engine [`Scheduler`], delivering
//!   lifecycle transitions as DOM-manipulation tasks.
//!
//! A history operation that cannot produce a valid destination returns a
//! [`NavError`] and leaves history untouched. In particular the list length alone
//! never produces a navigation: `back` at the first entry, `forward` at the last
//! and any out-of-range `Go` are rejected.
//!
//! [`NavigationBudget`] caps a whole load pipeline with a single wall-clock
//! deadline and guards against chained-navigation loops; the browser layer
//! consults it before each subresource and before each layout/paint pass.
//!
//! ```
//! use browser_navigation::{NavigationAction, SessionHistory};
//! use browser_url::Url;
//!
//! let mut history = SessionHistory::new();
//! let url = Url::parse("https://example.com/").unwrap();
//! history
//!     .navigate(NavigationAction::Push, Some(url), "Example")
//!     .unwrap();
//! assert!(!history.can_go_back());
//! assert!(history.current().is_some());
//! ```

#![forbid(unsafe_code)]

mod deadline;
mod frame;
mod history;
mod lifecycle;
mod navigator;

pub use browser_event_loop::Scheduler;
pub use deadline::{NavigationBudget, NavigationDeadline, Truncation};
pub use frame::Frame;
pub use history::{HistoryEntry, NavError, Navigation, NavigationAction, SessionHistory};
pub use lifecycle::{LifecycleState, Transition};
pub use navigator::Navigator;
