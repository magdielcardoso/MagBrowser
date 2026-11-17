//! Mag browser core: wires the engine subsystems into a navigable browser.
//!
//! The core owns the network client, tab set, page pipeline (HTML parse → style →
//! layout → paint) and JavaScript execution. It is the only crate that knows about
//! all of the others; individual subsystems never depend on the core.
//!
//! ```
//! use browser_core::{Browser, Settings};
//!
//! let mut browser = Browser::new(Settings::headless());
//! let snapshot = browser
//!     .load_html("<html><head><title>Demo</title></head><body><p>hello</p></body></html>", None)
//!     .unwrap();
//! assert_eq!(snapshot.title, "Demo");
//! let png = browser.render().to_png();
//! assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
//! ```

#![forbid(unsafe_code)]

pub mod browser;
pub mod page;
pub mod profile;
pub mod script;
pub mod settings;
pub mod tab;

pub use browser::{Browser, BrowserError, Snapshot};
pub use page::{Page, PageState};
pub use profile::{Profile, ProfileError};
pub use settings::Settings;
pub use tab::{Tab, TabId, TabSet};
