//! Chrome-style developer tools for the Mag native window.
//!
//! The panel is an engine-drawn overlay: it shares the window's single RGBA
//! frame, so the same pixels are produced with or without a GPU presenter.
//! No native widgets are involved, which keeps `--window-selfcheck` stable.
//!
//! The module is compiled in headless builds too (it has no `winit`
//! dependency); only [`crate::window_app`] drives it from user input.

#![allow(dead_code)]

pub mod panel;

#[allow(unused_imports)] // Re-exports are driven by the optional window build.
pub use panel::{DevTools, DevToolsTab, LogLevel};

/// Lifecycle state of a recorded request, as observed by the app layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    /// The navigation started but has not finished.
    Loading,
    /// The navigation completed successfully.
    Ready,
    /// The navigation failed.
    Error,
}

impl RequestStatus {
    /// Short label drawn in the Network table.
    pub fn label(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }
}

/// Coarse resource classification for the Network table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// Top-level document navigation.
    Document,
    /// Script resource.
    Script,
    /// Stylesheet resource.
    Style,
    /// Image resource.
    Image,
    /// Anything else.
    Other,
}

impl ResourceType {
    /// Short label drawn in the Network table.
    pub fn label(self) -> &'static str {
        match self {
            Self::Document => "doc",
            Self::Script => "script",
            Self::Style => "style",
            Self::Image => "image",
            Self::Other => "other",
        }
    }
}

/// One row of the app-layer request log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRequest {
    /// HTTP method (navigations use `GET`).
    pub method: String,
    /// Requested URL.
    pub url: String,
    /// Lifecycle state at the time it was recorded.
    pub status: RequestStatus,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u32,
    /// Resource classification.
    pub resource_type: ResourceType,
}
