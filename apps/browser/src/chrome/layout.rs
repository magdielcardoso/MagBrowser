#![cfg_attr(not(feature = "window"), allow(dead_code))]

//! Chrome geometry: rectangles, hit testing and text budgets.
//!
//! Everything here is pure maths over logical pixels, so it is shared by the
//! native window and the self-check renderer and is covered by unit tests.

use browser_layout::{Rect, Size};
use browser_text::Font;

use super::{
    BUTTON_SIZE, CHROME_HEIGHT, FONT_SIZE, LoadStatus, PADDING, STATUS_FONT_SIZE, TAB_STRIP_HEIGHT,
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
    /// New-tab `+` button.
    NewTab,
    /// Page content area.
    Page,
    /// Status pill.
    Status,
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
        let mut x = PADDING;
        for _ in 0..tab_count {
            if x + per_tab > right_limit + 0.01 {
                break;
            }
            tabs.push(Rect::new(x, tab_y, per_tab, TAB_HEIGHT));
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
            new_tab,
            new_tab_visible,
            content,
            page,
            status,
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
        if contains(self.page, x, y) {
            return Some(ChromePart::Page);
        }
        None
    }

    /// Number of characters that fit in the address field.
    pub fn address_visible_chars(&self) -> usize {
        let font = Font::monospace();
        let available = (self.address.width - 2.0 * ADDRESS_INSET - 2.0).max(0.0);
        let cell = font.advance('M', FONT_SIZE).max(1.0);
        (available / cell) as usize
    }

    /// Number of characters that fit inside a tab.
    pub fn tab_visible_chars(&self) -> usize {
        let font = Font::monospace();
        let available =
            (self.tabs.first().map_or(TAB_MIN_WIDTH, |tab| tab.width) - 2.0 * PADDING).max(0.0);
        let cell = font.advance('M', FONT_SIZE).max(1.0);
        (available / cell) as usize
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
        assert_eq!(widths[2], widths[0] - 2.0 * 6.0);
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
        assert!(chrome.address_visible_chars() > 20);
    }
}
