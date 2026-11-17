//! Window composition: page image + chrome, rasterized with our own primitives.
//!
//! The output is a single RGBA [`ImageBuffer`] for the whole window. The native
//! backend uploads it as one texture; the self-check writes it to PNG. Both
//! therefore show exactly the same pixels.

use std::sync::OnceLock;

use browser_core::Browser;
use browser_css::Color;
use browser_gpu::GlyphAtlas;
use browser_layout::{Edges, Rect, Size};
use browser_paint::{DisplayCommand, DisplayList, ImageBuffer, ImageData, Rasterizer};
use browser_style::BorderStyle;
use browser_text::Font;

use super::layout::{ADDRESS_INSET, STATUS_HEIGHT};
use super::{
    ACCENT, BACKGROUND, ChromeLayout, ChromePart, FONT_SIZE, HAIRLINE, HOVER, INK, MUTED_INK,
    PADDING, PAGE_SURROUND, STATUS_FONT_SIZE, TAB_ACTIVE, TAB_STRIP, TOOLBAR, UiState,
};
use crate::chrome::CHROME_HEIGHT;

/// Corner radius for UI fields and buttons.
const RADIUS: f32 = 7.0;
/// Corner radius for tabs.
const TAB_RADIUS: f32 = 6.0;

/// A shared glyph atlas; building it is cheap but it never changes.
fn atlas() -> &'static GlyphAtlas {
    static ATLAS: OnceLock<GlyphAtlas> = OnceLock::new();
    ATLAS.get_or_init(GlyphAtlas::new)
}

/// Renders the full window composition.
pub fn render_window(browser: &Browser, ui: &UiState, chrome: &ChromeLayout) -> ImageBuffer {
    let mut list = DisplayList::new();
    let window = Rect::new(0.0, 0.0, chrome.width, chrome.height);

    // Window and chrome surfaces.
    fill(&mut list, window, BACKGROUND);
    fill(&mut list, chrome.tab_strip, TAB_STRIP);
    fill(&mut list, chrome.toolbar, TOOLBAR);
    fill(&mut list, chrome.content, PAGE_SURROUND);

    let (titles, active) = browser.tabs().titles();

    // Tabs: active tab is a white card with a hairline and an accent underline.
    for (index, rect) in chrome.tabs.iter().enumerate() {
        if index == active {
            stroke_rounded(&mut list, *rect, TAB_RADIUS, HAIRLINE, TAB_ACTIVE);
            let underline = Rect::new(
                rect.x + PADDING,
                rect.bottom() - 2.0,
                (rect.width - 2.0 * PADDING).max(0.0),
                2.0,
            );
            fill_rounded(&mut list, underline, 1.0, ACCENT);
        } else if ui.hover == Some(ChromePart::Tab(index)) {
            fill_rounded(&mut list, *rect, TAB_RADIUS, HOVER);
        }
    }

    // Toolbar buttons: a faint rounded chip, darker on hover. Icons are added
    // below with the other primitives.
    for (rect, part) in [
        (chrome.back, ChromePart::Back),
        (chrome.forward, ChromePart::Forward),
        (chrome.reload, ChromePart::Reload),
    ] {
        fill_rounded(
            &mut list,
            inset(rect, 2.0),
            RADIUS,
            Color::rgba(255, 255, 255, 160),
        );
        if ui.hover == Some(part) {
            fill_rounded(&mut list, inset(rect, 2.0), RADIUS, HOVER);
        }
    }
    if ui.hover == Some(ChromePart::NewTab) && chrome.new_tab_visible {
        fill_rounded(&mut list, inset(chrome.new_tab, 2.0), TAB_RADIUS, HOVER);
    }

    // Address field: accent border when focused, hairline otherwise.
    let focused = ui.address.is_focused();
    stroke_rounded(
        &mut list,
        chrome.address,
        RADIUS,
        if focused { ACCENT } else { HAIRLINE },
        Color::rgb(255, 255, 255),
    );

    // Page image, then the content frame on top of it.
    if !chrome.page.is_empty() {
        let page = browser.render();
        if let Ok(image) = ImageData::from_rgba(page.width, page.height, page.pixels) {
            let index = list.add_image(image);
            list.push(DisplayCommand::DrawImage {
                rect: chrome.page,
                image: index,
            });
        }
    }
    list.push(DisplayCommand::DrawBorder {
        rect: chrome.content,
        widths: Edges::uniform(1.0),
        colors: [HAIRLINE; 4],
        styles: [BorderStyle::Solid; 4],
    });

    // Hairlines between the tab strip and toolbar, and below the toolbar.
    fill(
        &mut list,
        Rect::new(0.0, chrome.tab_strip.bottom() - 1.0, chrome.width, 1.0),
        HAIRLINE,
    );
    fill(
        &mut list,
        Rect::new(0.0, CHROME_HEIGHT - 1.0, chrome.width, 1.0),
        HAIRLINE,
    );

    // Icons.
    chevron(&mut list, chrome.back, false, MUTED_INK);
    chevron(&mut list, chrome.forward, true, MUTED_INK);
    reload_icon(&mut list, chrome.reload, MUTED_INK);
    plus_icon(
        &mut list,
        chrome.new_tab,
        ui.hover == Some(ChromePart::NewTab),
    );

    // Status pill.
    stroke_rounded(
        &mut list,
        chrome.status,
        STATUS_HEIGHT / 2.0,
        HAIRLINE,
        Color::rgba(255, 255, 255, 235),
    );

    // Caret occupies a real pixel column so it composes with the display list.
    let caret_x = if focused {
        caret_position(ui, chrome)
    } else {
        None
    };
    if let Some(x) = caret_x {
        fill(
            &mut list,
            Rect::new(
                x,
                chrome.address.y + (chrome.address.height - 14.0) / 2.0,
                1.0,
                14.0,
            ),
            ACCENT,
        );
    }

    let mut image = Rasterizer::new(
        Size {
            width: chrome.width,
            height: chrome.height,
        },
        BACKGROUND,
    )
    .execute(&list);
    paint_text(&mut image, ui, chrome, &titles, active, caret_x);
    image
}

#[allow(clippy::too_many_arguments)]
fn paint_text(
    image: &mut ImageBuffer,
    ui: &UiState,
    chrome: &ChromeLayout,
    titles: &[String],
    active: usize,
    caret_x: Option<f32>,
) {
    let atlas = atlas();
    let tab_chars = chrome.tab_visible_chars();
    for (index, rect) in chrome.tabs.iter().enumerate() {
        let Some(title) = titles.get(index) else {
            continue;
        };
        let color = if index == active { INK } else { MUTED_INK };
        let visible = fit_text(title, tab_chars);
        atlas.paint_text_centered(
            image,
            rect.x + PADDING,
            rect.y,
            rect.height,
            &visible,
            FONT_SIZE,
            color,
            Some(rect.width - 2.0 * PADDING),
        );
    }

    // Address text. When focused, the caret is already drawn by the display
    // list, so reserve one character for it.
    let reserved = if caret_x.is_some() { 1 } else { 0 };
    let max_chars = chrome.address_visible_chars().saturating_sub(reserved);
    let visible = ui.address.visible(max_chars);
    let color = if visible.text.starts_with('\u{2026}') {
        MUTED_INK
    } else {
        INK
    };
    atlas.paint_text_centered(
        image,
        chrome.address.x + ADDRESS_INSET,
        chrome.address.y,
        chrome.address.height,
        &visible.text,
        FONT_SIZE,
        color,
        Some(chrome.address.width - 2.0 * ADDRESS_INSET - 2.0),
    );

    atlas.paint_text_centered(
        image,
        chrome.status.x + PADDING,
        chrome.status.y,
        STATUS_HEIGHT,
        ui.status.text(),
        STATUS_FONT_SIZE,
        MUTED_INK,
        Some(chrome.status.width - 2.0 * PADDING),
    );
}

fn caret_position(ui: &UiState, chrome: &ChromeLayout) -> Option<f32> {
    let max_chars = chrome.address_visible_chars();
    let visible = ui.address.visible(max_chars);
    let font = Font::monospace();
    let x =
        chrome.address.x + ADDRESS_INSET + visible.caret_col as f32 * font.advance('M', FONT_SIZE);
    if x >= chrome.address.right() - 4.0 {
        return None;
    }
    Some(x)
}

/// Truncates `text` to `max_chars`, appending an ellipsis when it does not fit.
pub fn fit_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return "\u{2026}".to_string();
    }
    let mut out: String = text.chars().take(max_chars - 1).collect();
    out.push('\u{2026}');
    out
}

fn fill(list: &mut DisplayList, rect: Rect, color: Color) {
    if rect.is_empty() {
        return;
    }
    list.push(DisplayCommand::FillRect { rect, color });
}

fn inset(rect: Rect, amount: f32) -> Rect {
    Rect::new(
        rect.x + amount,
        rect.y + amount,
        (rect.width - 2.0 * amount).max(0.0),
        (rect.height - 2.0 * amount).max(0.0),
    )
}

/// Fills a rounded rectangle using one span per scanline.
fn fill_rounded(list: &mut DisplayList, rect: Rect, radius: f32, color: Color) {
    if rect.is_empty() {
        return;
    }
    let radius = radius.clamp(0.0, rect.width.min(rect.height) / 2.0);
    let rows = rect.height.ceil() as i32;
    for row in 0..rows {
        let y = rect.y + row as f32;
        let cy = row as f32 + 0.5;
        let distance = if cy < radius {
            radius - cy
        } else if cy > rect.height - radius {
            cy - (rect.height - radius)
        } else {
            0.0
        };
        let inset = if distance <= 0.0 {
            0.0
        } else {
            radius - (radius * radius - distance * distance).max(0.0).sqrt()
        };
        fill(
            list,
            Rect::new(rect.x + inset, y, (rect.width - 2.0 * inset).max(0.0), 1.0),
            color,
        );
    }
}

/// Draws a 1 px rounded outline and fills its interior.
fn stroke_rounded(list: &mut DisplayList, rect: Rect, radius: f32, border: Color, interior: Color) {
    fill_rounded(list, rect, radius, border);
    fill_rounded(list, inset(rect, 1.0), (radius - 1.0).max(0.0), interior);
}

fn chevron(list: &mut DisplayList, rect: Rect, right: bool, color: Color) {
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let direction = if right { 1.0 } else { -1.0 };
    for offset in -5_i32..=5 {
        let depth = 5.0 - offset.abs() as f32;
        let x = cx + direction * (depth - 2.0) - 0.75;
        fill(
            list,
            Rect::new(x, cy + offset as f32 - 0.5, 1.5, 1.25),
            color,
        );
    }
}

fn reload_icon(list: &mut DisplayList, rect: Rect, color: Color) {
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let radius = 5.5;
    for step in 0..14 {
        let angle = 0.55 + step as f32 * std::f32::consts::TAU / 16.0;
        let x = cx + radius * angle.cos() - 0.75;
        let y = cy + radius * angle.sin() - 0.75;
        fill(list, Rect::new(x, y, 1.5, 1.5), color);
    }
    // Arrow head at the open end of the ring.
    let tip_x = cx + radius * 0.55_f32.cos();
    let tip_y = cy + radius * 0.55_f32.sin();
    fill(list, Rect::new(tip_x - 3.0, tip_y - 1.0, 3.0, 1.5), color);
    fill(list, Rect::new(tip_x - 1.5, tip_y + 1.0, 1.5, 1.5), color);
}

fn plus_icon(list: &mut DisplayList, rect: Rect, active: bool) {
    let color = if active { INK } else { MUTED_INK };
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    fill(list, Rect::new(cx - 5.0, cy - 0.75, 10.0, 1.5), color);
    fill(list, Rect::new(cx - 0.75, cy - 5.0, 1.5, 10.0), color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{LoadStatus, TAB_STRIP_HEIGHT};
    use browser_core::{Browser, Settings};

    fn browser() -> Browser {
        let mut browser = Browser::new(Settings::headless());
        browser
            .load_html(
                "<html><head><title>Test page</title></head><body style='background:#ffffff;margin:0'>\
                 <h1>Hello</h1><p>content</p></body></html>",
                None,
            )
            .unwrap();
        browser
    }

    fn chrome() -> ChromeLayout {
        ChromeLayout::new(
            Size {
                width: 800.0,
                height: 600.0,
            },
            1,
            LoadStatus::Ready,
        )
    }

    #[test]
    fn composition_has_chrome_and_page_regions() {
        let browser = browser();
        let mut ui = UiState::new();
        ui.address.set_exact("file:///tmp/page.html");
        let image = render_window(&browser, &ui, &chrome());
        assert_eq!((image.width, image.height), (800, 600));
        // Empty part of the tab strip is the strip colour.
        assert_eq!(image.pixel(700, 10), [0xf0, 0xf0, 0xf3, 255]);
        // Toolbar background away from controls.
        assert_eq!(image.pixel(796, 58), [0xf7, 0xf7, 0xf9, 255]);
        // Content frame hairline.
        assert_eq!(
            image.pixel(400, CHROME_HEIGHT as u32),
            [0xd2, 0xd2, 0xd7, 255]
        );
        // Page background inside the frame.
        assert_eq!(image.pixel(400, 300), [255, 255, 255, 255]);
        // The address field is white and holds text ink.
        assert_eq!(image.pixel(700, 58), [255, 255, 255, 255]);
        let mut ink = 0;
        for x in 115..690 {
            for y in 50..67 {
                let pixel = image.pixel(x, y);
                if pixel[0] < 200 && pixel[1] < 200 && pixel[2] < 200 {
                    ink += 1;
                }
            }
        }
        assert!(ink > 20, "address text should be visible");
    }

    #[test]
    fn active_tab_is_highlighted() {
        let browser = browser();
        let ui = UiState::new();
        let image = render_window(&browser, &ui, &chrome());
        // First tab starts at x=8; its card is white, the strip is grey.
        assert_eq!(image.pixel(20, 8), [255, 255, 255, 255]);
        assert_eq!(image.pixel(250, 8), [0xf0, 0xf0, 0xf3, 255]);
        assert_eq!(
            image.pixel(20, (TAB_STRIP_HEIGHT - 6.0) as u32),
            [0x0a, 0x84, 0xff, 255]
        );
    }

    #[test]
    fn focus_draws_accent_caret_and_ring() {
        let browser = browser();
        let mut ui = UiState::new();
        ui.address.set_exact("https://example.com");
        ui.address.focus();
        let image = render_window(&browser, &ui, &chrome());
        // Field border top-most pixel is accent when focused.
        let mut accent_pixels = 0;
        for x in 0..800 {
            if image.pixel(x, 44) == [0x0a, 0x84, 0xff, 255] {
                accent_pixels += 1;
            }
        }
        assert!(accent_pixels > 0);
        // The caret sits after the text and is accent-coloured.
        let caret = caret_position(&ui, &chrome()).unwrap();
        let mut found = false;
        for y in 51..65 {
            if image.pixel(caret.round() as u32, y) == [0x0a, 0x84, 0xff, 255] {
                found = true;
            }
        }
        assert!(found);
    }

    #[test]
    fn status_line_changes_with_state() {
        let browser = browser();
        let mut ui = UiState::new();
        let ready = render_window(&browser, &ui, &chrome());
        let ready_pixels = status_ink(&ready);
        ui.status = LoadStatus::Error;
        let error = render_window(&browser, &ui, &chrome());
        let error_pixels = status_ink(&error);
        assert!(ready_pixels > 0 && error_pixels > 0);
        assert_ne!(ready_pixels, error_pixels);
    }

    fn status_ink(image: &ImageBuffer) -> usize {
        let chrome = chrome();
        let mut ink = 0;
        for x in chrome.status.x as u32..chrome.status.right() as u32 {
            for y in chrome.status.y as u32..chrome.status.bottom() as u32 {
                let pixel = image.pixel(x, y);
                if pixel[0] < 200 && pixel[1] < 200 && pixel[2] < 200 {
                    ink += 1;
                }
            }
        }
        ink
    }

    #[test]
    fn fitted_text_never_leaves_the_field() {
        assert_eq!(fit_text("short", 10), "short");
        assert_eq!(fit_text("a-very-long-title", 6), "a-ver\u{2026}");
        let fitted = fit_text("a-very-long-title", 6);
        assert!(Font::monospace().measure(&fitted, FONT_SIZE) <= 6.0 * 7.0 + 0.01);
    }
}
