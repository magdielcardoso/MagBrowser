//! Reusable, UI-framework-agnostic helpers for the browser window shell.
//!
//! Everything in this module depends only on `std` and plain `f32` geometry so
//! it can be wired into the winit/wgpu window layer later without either
//! framework leaking into these modules. Each submodule carries its own unit
//! tests and can also be compiled on its own with `rustc --test`.
//!
//! The window layer (`window_app.rs`) owns the instances: `ZoomController`
//! drives the page render scale, `TabDrag` plus [`tab_drag::reorder_tabs`]
//! maintain the display order shown by the strip, `FindBar` plus
//! [`find_overlay::highlights`] provide find-in-page, and `ContextMenu`
//! describes the right-click menu that `chrome::render` paints.
//!
//! # Not wired (engine gap)
//!
//! Cross-session history, bookmarks and downloads live in the separate
//! `browser-platform` crate, which this binary does not link. The engine only
//! exposes per-tab `SessionHistory` (used here to enable/disable the context
//! menu's Back row) and the clipboard action has no backend in this process.
//! Those features remain models/data sources for a future integration; the
//! five GUI behaviours above are fully wired in `window_app.rs`.

pub mod context_menu;
pub mod find;
pub mod find_overlay;
pub mod tab_drag;
pub mod zoom;

// These re-exports make the models discoverable to the window layer and to
// future callers. A binary crate still warns about the ones it does not use.
#[allow(unused_imports)]
pub use context_menu::{ContextAction, ContextMenu, MenuItem, Point, Rect};
#[allow(unused_imports)]
pub use find::{FindBar, TextRange, find_all};
pub use find_overlay::FindHighlight;
#[allow(unused_imports)]
pub use tab_drag::{TabDrag, push_tab, remove_tab, reorder, reorder_tabs};
#[allow(unused_imports)]
pub use zoom::{ZOOM_LEVELS, ZoomController};
