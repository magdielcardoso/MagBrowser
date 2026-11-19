//! The browser chrome: palette, layout and composition.
//!
//! Chrome is drawn with the engine's own primitives — rounded rectangles are
//! emitted as scanline spans on a display list, and text comes from the
//! `browser-gpu` glyph atlas built from `browser_text::Font`. There are no
//! native widgets and no HTML involved, so the same composition renders to a
//! real window and to the `--window-selfcheck` PNG.

pub mod layout;
pub mod render;

use browser_css::Color;

pub use layout::{ChromeLayout, ChromePart};

use crate::address_bar::AddressBar;

/// Tab strip height in logical pixels.
pub const TAB_STRIP_HEIGHT: f32 = 36.0;
/// Toolbar (back/forward/reload + address) height in logical pixels.
pub const TOOLBAR_HEIGHT: f32 = 44.0;
/// Total chrome height above the page.
pub const CHROME_HEIGHT: f32 = TAB_STRIP_HEIGHT + TOOLBAR_HEIGHT;
/// Standard padding unit.
pub const PADDING: f32 = 8.0;
/// Square button size.
pub const BUTTON_SIZE: f32 = 28.0;
/// UI text size in logical pixels.
pub const FONT_SIZE: f32 = 14.0;
/// Status line text size.
pub const STATUS_FONT_SIZE: f32 = 12.0;

/// Near-white window background.
pub const BACKGROUND: Color = Color::rgb(0xfb, 0xfb, 0xfd);
/// Primary ink.
pub const INK: Color = Color::rgb(0x1d, 0x1d, 0x1f);
/// Secondary ink for inactive tabs and status text.
pub const MUTED_INK: Color = Color::rgb(0x6e, 0x6e, 0x73);
/// System-blue accent.
pub const ACCENT: Color = Color::rgb(0x0a, 0x84, 0xff);
/// 1 px separators and field borders.
pub const HAIRLINE: Color = Color::rgb(0xd2, 0xd2, 0xd7);
/// Tab strip background.
pub const TAB_STRIP: Color = Color::rgb(0xf0, 0xf0, 0xf3);
/// Toolbar background.
pub const TOOLBAR: Color = Color::rgb(0xf7, 0xf7, 0xf9);
/// Active tab background.
pub const TAB_ACTIVE: Color = Color::rgb(0xff, 0xff, 0xff);
/// Hover background for chrome controls.
pub const HOVER: Color = Color::rgb(0xe4, 0xe4, 0xe9);
/// Pressed background for chrome controls.
pub const PRESSED: Color = Color::rgb(0xd2, 0xd2, 0xda);
/// Page surround inside the content frame.
pub const PAGE_SURROUND: Color = Color::rgb(0xff, 0xff, 0xff);

/// Load state shown by the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub enum LoadStatus {
    /// The page finished loading.
    #[default]
    Ready,
    /// A navigation is in flight.
    Loading,
    /// The last navigation failed.
    Error,
}

impl LoadStatus {
    /// The text drawn in the status line.
    pub fn text(self) -> &'static str {
        match self {
            Self::Ready => "Pronto",
            Self::Loading => "Carregando\u{2026}",
            Self::Error => "Erro",
        }
    }
}

/// Mutable state owned by the window layer and read by [`render`].
#[derive(Debug, Clone)]
pub struct UiState {
    /// Address bar text, caret and focus.
    pub address: AddressBar,
    /// Current load state.
    pub status: LoadStatus,
    /// Chrome control currently under the pointer.
    pub hover: Option<ChromePart>,
    /// Chrome control currently held down by the pointer.
    pub pressed: Option<ChromePart>,
    /// Last navigation error, when any.
    pub error: Option<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            address: AddressBar::new(),
            status: LoadStatus::Ready,
            hover: None,
            pressed: None,
            error: None,
        }
    }
}

impl UiState {
    /// Creates empty UI state.
    pub fn new() -> Self {
        Self::default()
    }
}
