#![cfg_attr(not(feature = "window"), allow(dead_code))]

//! Chrome geometry: rectangles, hit testing and text budgets.
//!
//! Everything here is pure maths over logical pixels, so it is shared by the
//! native window and the self-check renderer and is covered by unit tests.

use browser_layout::{Rect, Size};
use browser_text::Font;

use super::{
    BUTTON_SIZE, CHROME_HEIGHT, LoadStatus, PADDING, STATUS_FONT_SIZE, TAB_STRIP_HEIGHT,
    TOOLBAR_HEIGHT,
};

/// Minimum width of a tab before it is dropped from the strip.
pub const TAB_MIN_WIDTH: f32 = 96.0;
/// Maximum width of a tab.
pub const TAB_MAX_WIDTH: f32 = 220.0;
/// Gap between tabs.
pub const TAB_GAP: f32 = 4.0;
/// Tab height.
pub const TAB_HEIGHT: f32 = 28.0;
/// Distance from the content frame to the status pill.
pub const STATUS_MARGIN: f32 = 8.0;
/// Height of the status pill.
pub const STATUS_HEIGHT: f32 = 20.0;
/// Left inset of address field text.
pub const ADDRESS_INSET: f32 = 10.0;
/// Side of the square site-security icon drawn inside the address field.
pub const ADDRESS_ICON_SIZE: f32 = 12.0;
/// Gap between the security icon and the address text.
pub const ADDRESS_ICON_GAP: f32 = 6.0;
/// Size of the close button on each tab.
pub const TAB_CLOSE_SIZE: f32 = 14.0;
/// Gap between the close button and the tab title.
const TAB_CLOSE_GAP: f32 = 6.0;
/// Preferred width of the find bar overlay.
pub const FIND_BAR_WIDTH: f32 = 420.0;
/// Minimum width of the find bar overlay.
pub const FIND_BAR_MIN_WIDTH: f32 = 180.0;
/// Height of the find bar overlay.
pub const FIND_BAR_HEIGHT: f32 = 40.0;
/// Inset of the find bar from the top/right of the page.
pub const FIND_BAR_MARGIN: f32 = 8.0;

/// A region of the window chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromePart {
    /// Back button.
    Back,
    /// Forward button.
    Forward,
    /// Reload button.
    Reload,
    /// Address field.
    Address,
    /// Tab at an index in [`ChromeLayout::tabs`].
    Tab(usize),
    /// Close button of the tab at an index in [`ChromeLayout::tabs`].
    CloseTab(usize),
    /// New-tab `+` button.
    NewTab,
    /// Page content area.
    Page,
    /// Status pill.
    Status,
    /// Find bar text field.
    FindInput,
    /// Find bar close button.
    FindClose,
    /// Find bar "previous match" button.
    FindPrev,
    /// Find bar "next match" button.
    FindNext,
}

/// Computed rectangles for one window size.
#[derive(Debug, Clone, PartialEq)]
pub struct ChromeLayout {
    /// Window width in logical pixels.
    pub width: f32,
    /// Window height in logical pixels.
    pub height: f32,
    /// Tab strip rectangle.
    pub tab_strip: Rect,
    /// Toolbar rectangle.
    pub toolbar: Rect,
    /// Back button.
    pub back: Rect,
    /// Forward button.
    pub forward: Rect,
    /// Reload button.
    pub reload: Rect,
    /// Address field.
    pub address: Rect,
    /// Visible tab rectangles, left to right.
    pub tabs: Vec<Rect>,
    /// Close-button rectangles, one per visible tab.
    pub tab_closes: Vec<Rect>,
    /// New-tab button.
    pub new_tab: Rect,
    /// Whether the new-tab button fits in the strip.
    pub new_tab_visible: bool,
    /// Content frame (the 1 px border sits on its edges).
    pub content: Rect,
    /// Page image rectangle (content inset by the 1 px border).
    pub page: Rect,
    /// Status pill at the bottom right of the content.
    pub status: Rect,
    /// Find bar card, overlaid on the top-right of the page when open.
    pub find_bar: Rect,
    /// Find query field inside [`Self::find_bar`].
    pub find_input: Rect,
    /// Find bar close button.
    pub find_close: Rect,
    /// Find bar previous-match button.
    pub find_prev: Rect,
    /// Find bar next-match button.
    pub find_next: Rect,
}

impl ChromeLayout {
    /// Computes the layout for a window with `tab_count` tabs.
    pub fn new(size: Size, tab_count: usize, status: LoadStatus) -> Self {
        let width = size.width.max(1.0);
        let height = size.height.max(1.0);
        let tab_strip = Rect::new(0.0, 0.0, width, TAB_STRIP_HEIGHT);
        let toolbar = Rect::new(0.0, TAB_STRIP_HEIGHT, width, TOOLBAR_HEIGHT);

        let button_y = TAB_STRIP_HEIGHT + (TOOLBAR_HEIGHT - BUTTON_SIZE) / 2.0;
        let back = Rect::new(PADDING, button_y, BUTTON_SIZE, BUTTON_SIZE);
        let forward = Rect::new(back.right() + 4.0, button_y, BUTTON_SIZE, BUTTON_SIZE);
        let reload = Rect::new(forward.right() + 4.0, button_y, BUTTON_SIZE, BUTTON_SIZE);
        let address_x = reload.right() + PADDING;
        let address = Rect::new(
            address_x,
            button_y,
            (width - address_x - PADDING).max(40.0),
            BUTTON_SIZE,
        );

        let tab_count = tab_count.max(1);
        let right_reserved = PADDING + BUTTON_SIZE + PADDING;
        let available = (width - 2.0 * PADDING - BUTTON_SIZE - PADDING).max(0.0);
        let count = tab_count as f32;
        let per_tab = ((available - TAB_GAP * (count - 1.0).max(0.0)) / count)
            .clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH);
        let tab_y = (TAB_STRIP_HEIGHT - TAB_HEIGHT) / 2.0;
        let right_limit = width - right_reserved;
        let mut tabs = Vec::new();
        let mut tab_closes = Vec::new();
        let mut x = PADDING;
        for _ in 0..tab_count {
            if x + per_tab > right_limit + 0.01 {
                break;
            }
            tabs.push(Rect::new(x, tab_y, per_tab, TAB_HEIGHT));
            let close = TAB_CLOSE_SIZE.min(per_tab / 3.0);
            tab_closes.push(Rect::new(
                x + per_tab - PADDING - close,
                tab_y + (TAB_HEIGHT - close) / 2.0,
                close,
                close,
            ));
            x += per_tab + TAB_GAP;
        }
        let new_tab = Rect::new(x, tab_y, BUTTON_SIZE, TAB_HEIGHT);
        let new_tab_visible = new_tab.right() <= width - PADDING + 0.01;

        let content = Rect::new(0.0, CHROME_HEIGHT, width, (height - CHROME_HEIGHT).max(0.0));
        let page = Rect::new(
            content.x + 1.0,
            content.y + 1.0,
            (content.width - 2.0).max(0.0),
            (content.height - 2.0).max(0.0),
        );

        let font = Font::monospace();
        let status_width = font.measure(status.text(), STATUS_FONT_SIZE) + 2.0 * PADDING;
        let status = Rect::new(
            (content.right() - STATUS_MARGIN - status_width).max(content.x),
            (content.bottom() - STATUS_MARGIN - STATUS_HEIGHT).max(content.y),
            status_width.min(content.width),
            STATUS_HEIGHT,
        );

        // The find bar is an overlay, not a viewport change: it never resizes
        // the page (and therefore never disturbs coordinates shared with the
        // engine). The window layer hides it until Ctrl+F opens it.
        let find_width = FIND_BAR_WIDTH
            .min(page.width - 2.0 * FIND_BAR_MARGIN)
            .max(FIND_BAR_MIN_WIDTH)
            .min(page.width.max(1.0));
        let find_bar = Rect::new(
            (page.right() - FIND_BAR_MARGIN - find_width).max(page.x),
            page.y + FIND_BAR_MARGIN,
            find_width,
            FIND_BAR_HEIGHT.min(page.height.max(1.0)),
        );
        let find_button = (FIND_BAR_HEIGHT - 12.0).max(8.0);
        let find_button_y = find_bar.y + (find_bar.height - find_button) / 2.0;
        let find_close = Rect::new(
            find_bar.right() - 6.0 - find_button,
            find_button_y,
            find_button,
            find_button,
        );
        let find_next = Rect::new(
            find_close.x - 4.0 - find_button,
            find_button_y,
            find_button,
            find_button,
        );
        let find_prev = Rect::new(
            find_next.x - 4.0 - find_button,
            find_button_y,
            find_button,
            find_button,
        );
        let find_input = Rect::new(
            find_bar.x + 6.0,
            find_button_y,
            (find_prev.x - find_bar.x - 10.0).max(1.0),
            find_button,
        );

        Self {
            width,
            height,
            tab_strip,
            toolbar,
            back,
            forward,
            reload,
            address,
            tabs,
            tab_closes,
            new_tab,
            new_tab_visible,
            content,
            page,
            status,
            find_bar,
            find_input,
            find_close,
            find_prev,
            find_next,
        }
    }

    /// Width available for a page viewport.
    pub fn page_size(&self) -> Size {
        Size {
            width: self.page.width,
            height: self.page.height,
        }
    }

    /// Maps a window coordinate to page-local coordinates.
    pub fn page_local(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        if x >= self.page.x && y >= self.page.y && x < self.page.right() && y < self.page.bottom() {
            Some((x - self.page.x, y - self.page.y))
        } else {
            None
        }
    }

    /// Hit-tests a window coordinate.
    pub fn hit(&self, x: f32, y: f32) -> Option<ChromePart> {
        if contains(self.back, x, y) {
            return Some(ChromePart::Back);
        }
        if contains(self.forward, x, y) {
            return Some(ChromePart::Forward);
        }
        if contains(self.reload, x, y) {
            return Some(ChromePart::Reload);
        }
        if contains(self.address, x, y) {
            return Some(ChromePart::Address);
        }
        for (index, close) in self.tab_closes.iter().enumerate() {
            if contains(*close, x, y) {
                return Some(ChromePart::CloseTab(index));
            }
        }
        for (index, tab) in self.tabs.iter().enumerate() {
            if contains(*tab, x, y) {
                return Some(ChromePart::Tab(index));
            }
        }
        if self.new_tab_visible && contains(self.new_tab, x, y) {
            return Some(ChromePart::NewTab);
        }
        if contains(self.status, x, y) {
            return Some(ChromePart::Status);
        }
        if contains(self.find_input, x, y) {
            return Some(ChromePart::FindInput);
        }
        if contains(self.find_prev, x, y) {
            return Some(ChromePart::FindPrev);
        }
        if contains(self.find_next, x, y) {
            return Some(ChromePart::FindNext);
        }
        if contains(self.find_close, x, y) {
            return Some(ChromePart::FindClose);
        }
        if contains(self.page, x, y) {
            return Some(ChromePart::Page);
        }
        None
    }

    /// Left inset for address text, leaving room for the security icon.
    pub fn address_text_inset() -> f32 {
        ADDRESS_INSET + ADDRESS_ICON_SIZE + ADDRESS_ICON_GAP
    }

    /// Measured width available for address text, in logical pixels.
    pub fn address_text_width(&self) -> f32 {
        (self.address.width - Self::address_text_inset() - ADDRESS_INSET - 2.0).max(0.0)
    }

    /// Measured width available for a tab title, in logical pixels.
    pub fn tab_text_width(&self) -> f32 {
        let width = self.tabs.first().map_or(TAB_MIN_WIDTH, |tab| tab.width);
        (width - 2.0 * PADDING - TAB_CLOSE_SIZE - TAB_CLOSE_GAP).max(0.0)
    }
}

fn contains(rect: Rect, x: f32, y: f32) -> bool {
    !rect.is_empty() && x >= rect.x && y >= rect.y && x < rect.right() && y < rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(width: f32, height: f32, tabs: usize) -> ChromeLayout {
        ChromeLayout::new(Size { width, height }, tabs, LoadStatus::Ready)
    }

    #[test]
    fn chrome_buttons_have_stable_hit_rects() {
        let chrome = layout(1200.0, 800.0, 1);
        assert_eq!(chrome.hit(22.0, 58.0), Some(ChromePart::Back));
        assert_eq!(chrome.hit(54.0, 58.0), Some(ChromePart::Forward));
        assert_eq!(chrome.hit(86.0, 58.0), Some(ChromePart::Reload));
        assert_eq!(chrome.hit(700.0, 58.0), Some(ChromePart::Address));
        assert_eq!(chrome.hit(100.0, 20.0), Some(ChromePart::Tab(0)));
        assert_eq!(chrome.hit(600.0, 400.0), Some(ChromePart::Page));
        assert_eq!(chrome.hit(600.0, 5.0), None);
        // Buttons never overlap and live inside the 44 px toolbar.
        assert!(chrome.back.right() < chrome.forward.x);
        assert!(chrome.reload.bottom() <= CHROME_HEIGHT);
        assert_eq!(chrome.address.y, chrome.back.y);
        assert!(chrome.address.right() <= 1200.0);
    }

    #[test]
    fn tab_close_buttons_are_hittable_and_reserve_title_space() {
        let chrome = layout(1200.0, 800.0, 2);
        assert_eq!(chrome.tab_closes.len(), chrome.tabs.len());
        for (tab, close) in chrome.tabs.iter().zip(&chrome.tab_closes) {
            assert!(close.x > tab.x && close.right() <= tab.right());
            assert!(chrome.hit(close.x + 2.0, close.y + 2.0).is_some());
            assert!(matches!(
                chrome.hit(close.x + 2.0, close.y + 2.0),
                Some(ChromePart::CloseTab(_))
            ));
        }
        // Title no longer runs under the close button.
        let title_width = chrome.tab_text_width();
        assert!(title_width > 0.0);
        assert!(chrome.tabs[0].width > title_width + TAB_CLOSE_SIZE + 2.0 * PADDING);
    }

    #[test]
    fn tab_strip_widths_are_bounded_and_ordered() {
        let wide = layout(1600.0, 800.0, 3);
        assert_eq!(wide.tabs.len(), 3);
        for tab in &wide.tabs {
            assert!((tab.width - TAB_MAX_WIDTH).abs() < 0.01);
        }
        for pair in wide.tabs.windows(2) {
            assert!((pair[1].x - (pair[0].right() + TAB_GAP)).abs() < 0.01);
        }
        assert!(wide.new_tab.x >= wide.tabs.last().unwrap().right());
        assert!(wide.new_tab_visible);

        let narrow = layout(500.0, 600.0, 8);
        assert!(narrow.tabs.len() < 8);
        assert!(
            narrow
                .tabs
                .iter()
                .all(|tab| tab.width >= TAB_MIN_WIDTH - 0.01)
        );
        assert!(narrow.tabs.last().unwrap().right() <= 500.0);
    }

    #[test]
    fn status_text_and_rect_follow_load_state() {
        assert_eq!(LoadStatus::Ready.text(), "Pronto");
        assert_eq!(LoadStatus::Loading.text(), "Carregando\u{2026}");
        assert_eq!(LoadStatus::Error.text(), "Erro");

        let mut widths = Vec::new();
        for status in [LoadStatus::Ready, LoadStatus::Loading, LoadStatus::Error] {
            let chrome = ChromeLayout::new(
                Size {
                    width: 800.0,
                    height: 600.0,
                },
                1,
                status,
            );
            assert!(chrome.status.right() <= chrome.content.right());
            assert!(chrome.status.bottom() <= chrome.content.bottom());
            assert!(
                (chrome.status.right() - (chrome.content.right() - STATUS_MARGIN)).abs() < 0.01
            );
            widths.push(chrome.status.width);
        }
        assert!(widths[1] > widths[0], "longer text needs a wider pill");
        assert!(
            widths[2] < widths[0],
            "\"Erro\" is narrower than \"Pronto\""
        );
    }

    #[test]
    fn address_text_budget_leaves_room_for_security_icon() {
        let chrome = layout(1000.0, 700.0, 1);
        assert_eq!(
            ChromeLayout::address_text_inset(),
            ADDRESS_INSET + ADDRESS_ICON_SIZE + ADDRESS_ICON_GAP
        );
        assert_eq!(
            chrome.address_text_width(),
            chrome.address.width - ChromeLayout::address_text_inset() - ADDRESS_INSET - 2.0
        );
        assert!(chrome.address_text_width() > 140.0);
    }

    #[test]
    fn new_tab_button_has_a_stable_hit_region() {
        let chrome = layout(1200.0, 800.0, 1);
        assert!(chrome.new_tab_visible);
        let cx = chrome.new_tab.x + chrome.new_tab.width / 2.0;
        let cy = chrome.new_tab.y + chrome.new_tab.height / 2.0;
        assert_eq!(chrome.hit(cx, cy), Some(ChromePart::NewTab));
        assert!(chrome.new_tab.x >= chrome.tabs.last().unwrap().right());
    }

    #[test]
    fn find_bar_overlays_the_page_with_stable_hit_regions() {
        let chrome = layout(1000.0, 700.0, 1);
        assert!(chrome.find_bar.x > chrome.page.x);
        assert!(chrome.find_bar.right() <= chrome.page.right());
        assert!(chrome.find_bar.y > chrome.page.y);
        assert!(chrome.find_bar.bottom() < chrome.page.bottom());
        assert!(
            chrome.find_input.right() <= chrome.find_prev.x,
            "query field must not run under the navigation buttons"
        );
        assert!(chrome.find_prev.right() <= chrome.find_next.x);
        assert!(chrome.find_next.right() <= chrome.find_close.x);
        assert_eq!(
            chrome.hit(chrome.find_input.x + 2.0, chrome.find_input.y + 2.0),
            Some(ChromePart::FindInput)
        );
        assert_eq!(
            chrome.hit(chrome.find_prev.x + 2.0, chrome.find_prev.y + 2.0),
            Some(ChromePart::FindPrev)
        );
        assert_eq!(
            chrome.hit(chrome.find_next.x + 2.0, chrome.find_next.y + 2.0),
            Some(ChromePart::FindNext)
        );
        assert_eq!(
            chrome.hit(chrome.find_close.x + 2.0, chrome.find_close.y + 2.0),
            Some(ChromePart::FindClose)
        );
        // The overlay does not change the page viewport.
        assert_eq!(chrome.page_size().width, 998.0);
    }

    #[test]
    fn page_mapping_excludes_chrome_and_border() {
        let chrome = layout(1000.0, 700.0, 2);
        assert_eq!(chrome.page_size().width, 998.0);
        assert_eq!(chrome.page_size().height, 618.0);
        assert_eq!(
            chrome.page_local(1.0, CHROME_HEIGHT + 1.0),
            Some((0.0, 0.0))
        );
        assert_eq!(chrome.page_local(500.0, 400.0), Some((499.0, 319.0)));
        assert_eq!(chrome.page_local(500.0, 20.0), None);
        assert_eq!(chrome.page_local(0.0, 400.0), None);
        assert!(chrome.address_text_width() > 140.0);
    }
}
