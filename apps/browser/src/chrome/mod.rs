//! The browser chrome: palette, layout and composition.
//!
//! Chrome is drawn with the engine's own primitives — rounded rectangles are
//! emitted as scanline spans on a display list, and text comes from the
//! `browser-gpu` glyph atlas built from `browser_text::Font`. There are no
//! native widgets and no HTML involved, so the same composition renders to a
//! real window and to the `--window-selfcheck` PNG.

pub mod layout;
pub mod render;

use browser_core::Browser;
use browser_css::Color;

pub use layout::{ChromeLayout, ChromePart};

use crate::address_bar::AddressBar;
use crate::ui_extra::{ContextAction, FindBar, FindHighlight};

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
    /// Find-in-page state (query, matches and current match).
    pub find: FindBar,
    /// Whether the find bar overlay is visible.
    pub find_open: bool,
    /// Whether typed characters go to the find query field.
    pub find_focused: bool,
    /// Approximate rectangles for every current match, in page-local CSS pixels.
    pub find_highlights: Vec<FindHighlight>,
    /// Maps display slots in the tab strip to browser tab indices.
    ///
    /// `browser_core::TabSet` has no reorder API, so drag-to-reorder is kept
    /// here and translated at every hit test.
    pub tab_order: Vec<usize>,
    /// Display slot of the tab being dragged, for visual feedback.
    pub tab_drag_source: Option<usize>,
    /// Target slot of the tab being dragged, for visual feedback.
    pub tab_drag_target: Option<usize>,
    /// Context-menu row under the pointer, for hover feedback.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub context_hover: Option<ContextAction>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            address: AddressBar::new(),
            status: LoadStatus::Ready,
            hover: None,
            pressed: None,
            error: None,
            find: FindBar::new(),
            find_open: false,
            find_focused: false,
            find_highlights: Vec::new(),
            tab_order: Vec::new(),
            tab_drag_source: None,
            tab_drag_target: None,
            context_hover: None,
        }
    }
}

impl UiState {
    /// Creates empty UI state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Titles in display order and the display slot of the active tab.
    ///
    /// The browser's own `titles()` order is only used when no custom tab order
    /// is set (the default and the self-check path).
    pub fn tab_titles(&self, browser: &Browser) -> (Vec<String>, usize) {
        let (titles, active) = browser.tabs().titles();
        if self.tab_order.len() == titles.len() && is_permutation(&self.tab_order, titles.len()) {
            let active_slot = self.tab_order.iter().position(|&index| index == active);
            let reordered: Vec<String> = self
                .tab_order
                .iter()
                .map(|&index| {
                    titles
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| "?".to_string())
                })
                .collect();
            return (reordered, active_slot.unwrap_or(active));
        }
        (titles, active)
    }

    /// Converts a display slot into a browser tab index.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn tab_index(&self, display_slot: usize) -> usize {
        self.tab_order
            .get(display_slot)
            .copied()
            .unwrap_or(display_slot)
    }
}

fn is_permutation(order: &[usize], len: usize) -> bool {
    order.len() == len && {
        let mut seen = vec![false; len];
        order.iter().all(|&index| {
            if index >= len || seen[index] {
                return false;
            }
            seen[index] = true;
            true
        })
    }
}
