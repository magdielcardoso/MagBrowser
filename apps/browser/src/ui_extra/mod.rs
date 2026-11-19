//! Reusable, UI-framework-agnostic helpers for the browser window shell.
//!
//! Everything in this module depends only on `std` and plain `f32` geometry so
//! it can be wired into the winit/wgpu window layer later without either
//! framework leaking into these modules. Each submodule carries its own unit
//! tests and can also be compiled on its own with `rustc --test`.

pub mod context_menu;
pub mod find;
pub mod tab_drag;
pub mod zoom;

pub use context_menu::{ContextAction, ContextMenu, MenuItem, Point, Rect};
pub use find::{FindBar, TextRange, find_all};
pub use tab_drag::{TabDrag, reorder};
pub use zoom::{ZOOM_LEVELS, ZoomController};
