//! Window composition: page image + chrome, rasterized with our own primitives.
//!
//! The output is a single RGBA [`ImageBuffer`] for the whole window. The native
//! backend uploads it as one texture; the self-check writes it to PNG. Both
//! therefore show exactly the same pixels.
//!
//! Quality strategy:
//! - chrome shapes (rounded cards, borders, hairlines) are composed at 2x the
//!   device resolution and reduced with a box filter, so their edges are
//!   anti-aliased instead of staircase-stepped;
//! - icons are filled polygons rasterised with 4x4 coverage, so they stay solid
//!   and smooth instead of being a row of little squares;
//! - text and the page are drawn at the final device resolution, after the
//!   reduction, so neither gets blurred by a second resampling pass.

#[path = "../frame_pipeline.rs"]
#[allow(dead_code)] // Public API wired in by the native-window integration.
pub mod frame_pipeline;

// Public API re-exported for the native window wiring in `window_app.rs`.
#[allow(unused_imports)]
pub use frame_pipeline::{FramePipeline, LayerView, PageRenderJob, as_layer_frames, plan_layers};

use std::sync::{Mutex, OnceLock};

use browser_core::Browser;
use browser_css::Color;
use browser_gpu::GlyphAtlas;
use browser_layout::{Edges, Rect, Size};
use browser_paint::{DisplayCommand, DisplayList, ImageBuffer, Rasterizer};
use browser_style::BorderStyle;
use browser_text::Font;

use super::layout::{ADDRESS_ICON_SIZE, ADDRESS_INSET, STATUS_HEIGHT};
use super::{
    ACCENT, BACKGROUND, ChromeLayout, ChromePart, FONT_SIZE, HAIRLINE, HOVER, INK, LoadStatus,
    MUTED_INK, PADDING, PAGE_SURROUND, PRESSED, STATUS_FONT_SIZE, TAB_ACTIVE, TAB_STRIP, TOOLBAR,
    UiState,
};
use crate::chrome::CHROME_HEIGHT;
use crate::ui_extra::{ContextAction, ContextMenu};

/// Corner radius for UI fields and buttons.
const RADIUS: f32 = 7.0;
/// Corner radius for tabs.
const TAB_RADIUS: f32 = 6.0;
/// Chrome shapes are composed at twice the device resolution.
const SUPERSAMPLE: u32 = 2;
/// Samples per axis for icon coverage.
const ICON_SUB: usize = 4;
/// Translucent yellow used for the active find-in-page match.
const FIND_CURRENT: Color = Color::rgba(255, 193, 7, 190);
/// Translucent yellow used for the other find-in-page matches.
const FIND_MATCH: Color = Color::rgba(255, 224, 94, 130);
/// Track (gutter) of a painted page scrollbar.
const SCROLLBAR_TRACK: Color = Color::rgba(120, 120, 130, 42);
/// Draggable thumb of a painted page scrollbar.
const SCROLLBAR_THUMB: Color = Color::rgba(88, 88, 98, 170);
/// Shortest useful thumb, in CSS pixels, before it fills the whole track.
const MIN_THUMB: f32 = 24.0;

/// Which axis of a page scrollbar a hit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarAxis {
    /// Vertical bar on the right edge of a scroll container.
    Vertical,
    /// Horizontal bar on the bottom edge of a scroll container.
    Horizontal,
}

/// Geometry of one scrollbar, in document (CSS) coordinates.
///
/// Produced from the active page's layout tree so both painting and pointer
/// hit testing read exactly the same rectangles. `max_scroll` is the vertical
/// scroll range; the engine has no horizontal scrolling yet, so it is zero for
/// [`ScrollbarAxis::Horizontal`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarGeometry {
    /// Layout box that owns the bar.
    pub box_index: usize,
    /// Orientation of the bar.
    pub axis: ScrollbarAxis,
    /// Full gutter rectangle (document coordinates).
    pub track: Rect,
    /// Draggable portion of the track (document coordinates).
    pub thumb: Rect,
    /// Largest scroll offset the thumb can reach, in CSS pixels.
    pub max_scroll: f32,
}

/// Computes every scrollbar of the active page from its layout tree.
///
/// The layout engine stores the track inside the scroller's padding box and
/// exposes it as [`browser_layout::LayoutBox::scrollbar`] /
/// [`browser_layout::LayoutBox::horizontal_scrollbar`]. The thumb is derived
/// from the visible fraction of the scrollable content; both the painter and
/// the window's drag handler call this function, so they cannot disagree.
pub fn scrollbars(browser: &Browser) -> Vec<ScrollbarGeometry> {
    let mut bars = Vec::new();
    for (box_index, b) in browser.page().layout.boxes.iter().enumerate() {
        if let Some(track) = b.scrollbar
            && !track.is_empty()
        {
            let viewport = b.content.height.max(0.0);
            let total = b.scroll_height.max(viewport);
            let thumb_len = if total > 0.0 {
                (track.height * viewport / total).clamp(MIN_THUMB.min(track.height), track.height)
            } else {
                track.height
            };
            let max_scroll = (total - viewport).max(0.0);
            let progress = if max_scroll > 0.0 {
                b.scroll_offset.1.clamp(0.0, max_scroll) / max_scroll
            } else {
                0.0
            };
            let thumb = Rect::new(
                track.x,
                track.y + progress * (track.height - thumb_len),
                track.width,
                thumb_len,
            );
            bars.push(ScrollbarGeometry {
                box_index,
                axis: ScrollbarAxis::Vertical,
                track,
                thumb,
                max_scroll,
            });
        }
        if let Some(track) = b.horizontal_scrollbar
            && !track.is_empty()
        {
            // The engine currently scrolls only the vertical axis, so the
            // horizontal thumb spans its whole track.
            bars.push(ScrollbarGeometry {
                box_index,
                axis: ScrollbarAxis::Horizontal,
                track,
                thumb: track,
                max_scroll: 0.0,
            });
        }
    }
    bars
}

/// Finds the scrollbar under a document-coordinate point, thumb first.
///
/// When the point lands on the track but not the thumb, the geometry is still
/// returned so the window can jump the thumb to the click, like a browser does.
#[allow(dead_code)] // Wired into `window_app.rs` by the native-window integration.
pub fn scrollbar_at(browser: &Browser, doc_x: f32, doc_y: f32) -> Option<ScrollbarGeometry> {
    let bars = scrollbars(browser);
    bars.iter()
        .find(|bar| bar.thumb.contains(doc_x, doc_y))
        .copied()
        .or_else(|| {
            bars.iter()
                .find(|bar| bar.track.contains(doc_x, doc_y))
                .copied()
        })
}

/// A shared text painter; glyph coverage is cached inside `browser-text`.
fn atlas() -> &'static GlyphAtlas {
    static ATLAS: OnceLock<GlyphAtlas> = OnceLock::new();
    ATLAS.get_or_init(GlyphAtlas::new)
}

fn device_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn scaled_rect(rect: Rect, scale: f32) -> Rect {
    Rect::new(
        rect.x * scale,
        rect.y * scale,
        rect.width * scale,
        rect.height * scale,
    )
}

/// Everything that changes the rasterized chrome shapes. Text, icons and the
/// page are painted per frame, so address text is represented here only by the
/// caret's position (its pixel column is part of the supersampled base).
#[derive(Clone, PartialEq, Eq)]
struct ChromeKey {
    width_bits: u32,
    height_bits: u32,
    scale_bits: u32,
    hover: Option<ChromePart>,
    pressed: Option<ChromePart>,
    focus: bool,
    status: LoadStatus,
    tab_count: usize,
    active: usize,
    caret_x_bits: Option<u32>,
    tab_drag_source: Option<usize>,
    tab_drag_target: Option<usize>,
}

impl ChromeKey {
    fn new(browser: &Browser, ui: &UiState, chrome: &ChromeLayout, scale: f32) -> Self {
        let shape_scale = device_scale(scale) * SUPERSAMPLE as f32;
        let (_titles, active) = ui.tab_titles(browser);
        let focus = ui.address.is_focused();
        Self {
            width_bits: chrome.width.to_bits(),
            height_bits: chrome.height.to_bits(),
            scale_bits: scale.to_bits(),
            hover: ui.hover,
            pressed: ui.pressed,
            focus,
            status: ui.status,
            tab_count: browser.tabs().len(),
            active,
            caret_x_bits: if focus {
                caret_position(ui, chrome, shape_scale).map(f32::to_bits)
            } else {
                None
            },
            tab_drag_source: ui.tab_drag_source,
            tab_drag_target: ui.tab_drag_target,
        }
    }
}

/// Rasterizes the chrome surfaces at `SUPERSAMPLE`x and reduces them once.
///
/// This is the expensive part of a redraw; the page, icons and text are added
/// afterwards at the final device resolution.
fn build_chrome_base(
    browser: &Browser,
    ui: &UiState,
    chrome: &ChromeLayout,
    scale: f32,
) -> ImageBuffer {
    let shape_scale = device_scale(scale) * SUPERSAMPLE as f32;
    let mut list = DisplayList::new();
    let window = Rect::new(
        0.0,
        0.0,
        chrome.width * shape_scale,
        chrome.height * shape_scale,
    );

    // Window and chrome surfaces.
    fill(&mut list, window, BACKGROUND);
    fill(
        &mut list,
        scaled_rect(chrome.tab_strip, shape_scale),
        TAB_STRIP,
    );
    fill(&mut list, scaled_rect(chrome.toolbar, shape_scale), TOOLBAR);
    fill(
        &mut list,
        scaled_rect(chrome.content, shape_scale),
        PAGE_SURROUND,
    );

    let (_titles, active) = ui.tab_titles(browser);
    let pad = PADDING * shape_scale;

    // Tabs: active tab is a white card with a hairline and an accent underline.
    for (index, rect) in chrome.tabs.iter().enumerate() {
        let rect = scaled_rect(*rect, shape_scale);
        if index == active {
            stroke_rounded(
                &mut list,
                rect,
                TAB_RADIUS * shape_scale,
                shape_scale,
                HAIRLINE,
                TAB_ACTIVE,
            );
            tab_underline(
                &mut list,
                rect,
                pad,
                shape_scale,
                ui.status == LoadStatus::Loading,
            );
        } else if ui.hover == Some(ChromePart::Tab(index)) {
            fill_rounded(&mut list, rect, TAB_RADIUS * shape_scale, HOVER);
        }
        if Some(index) == ui.tab_drag_source {
            fill_rounded(
                &mut list,
                rect,
                TAB_RADIUS * shape_scale,
                Color::rgba(100, 100, 110, 70),
            );
        }
        if Some(index) == ui.tab_drag_target {
            stroke_rounded(
                &mut list,
                rect,
                TAB_RADIUS * shape_scale,
                shape_scale,
                ACCENT,
                Color::rgba(255, 255, 255, 0),
            );
        }
        if (ui.hover == Some(ChromePart::CloseTab(index))
            || ui.pressed == Some(ChromePart::CloseTab(index)))
            && let Some(close) = chrome.tab_closes.get(index)
        {
            let color = if ui.pressed == Some(ChromePart::CloseTab(index)) {
                PRESSED
            } else {
                HOVER
            };
            fill_rounded(
                &mut list,
                inset(scaled_rect(*close, shape_scale), shape_scale),
                3.0 * shape_scale,
                color,
            );
        }
    }

    // Toolbar buttons: a faint rounded chip, darker on hover, darkest pressed.
    for (rect, part) in [
        (chrome.back, ChromePart::Back),
        (chrome.forward, ChromePart::Forward),
        (chrome.reload, ChromePart::Reload),
    ] {
        let rect = inset(scaled_rect(rect, shape_scale), 2.0 * shape_scale);
        let radius = RADIUS * shape_scale;
        fill_rounded(&mut list, rect, radius, Color::rgba(255, 255, 255, 160));
        if ui.hover == Some(part) {
            fill_rounded(&mut list, rect, radius, HOVER);
        }
        if ui.pressed == Some(part) {
            fill_rounded(&mut list, rect, radius, PRESSED);
        }
    }
    // The new-tab chip appears on hover/press only, so idle tab-strip pixels
    // stay untouched.
    if chrome.new_tab_visible
        && (ui.hover == Some(ChromePart::NewTab) || ui.pressed == Some(ChromePart::NewTab))
    {
        let rect = inset(scaled_rect(chrome.new_tab, shape_scale), 2.0 * shape_scale);
        let color = if ui.pressed == Some(ChromePart::NewTab) {
            PRESSED
        } else {
            HOVER
        };
        fill_rounded(&mut list, rect, TAB_RADIUS * shape_scale, color);
    }

    // Address field: accent border when focused, hairline otherwise.
    let focused = ui.address.is_focused();
    stroke_rounded(
        &mut list,
        scaled_rect(chrome.address, shape_scale),
        RADIUS * shape_scale,
        shape_scale,
        if focused { ACCENT } else { HAIRLINE },
        Color::rgb(255, 255, 255),
    );

    // Content frame on top of the (later composited) page.
    list.push(DisplayCommand::DrawBorder {
        rect: scaled_rect(chrome.content, shape_scale),
        widths: Edges::uniform(shape_scale),
        colors: [HAIRLINE; 4],
        styles: [BorderStyle::Solid; 4],
    });

    // Hairlines between the tab strip and toolbar, and below the toolbar.
    fill(
        &mut list,
        Rect::new(
            0.0,
            chrome.tab_strip.bottom() * shape_scale - shape_scale,
            chrome.width * shape_scale,
            shape_scale,
        ),
        HAIRLINE,
    );
    fill(
        &mut list,
        Rect::new(
            0.0,
            CHROME_HEIGHT * shape_scale - shape_scale,
            chrome.width * shape_scale,
            shape_scale,
        ),
        HAIRLINE,
    );

    // Status pill.
    stroke_rounded(
        &mut list,
        scaled_rect(chrome.status, shape_scale),
        STATUS_HEIGHT * shape_scale / 2.0,
        shape_scale,
        HAIRLINE,
        Color::rgba(255, 255, 255, 235),
    );

    // Caret occupies a real pixel column so it composes with the display list.
    let caret_x = if focused {
        caret_position(ui, chrome, shape_scale)
    } else {
        None
    };
    if let Some(x) = caret_x {
        fill(
            &mut list,
            Rect::new(
                x,
                (chrome.address.y + (chrome.address.height - 14.0) / 2.0) * shape_scale,
                shape_scale,
                14.0 * shape_scale,
            ),
            ACCENT,
        );
    }

    let base_width = (chrome.width * shape_scale).round() as u32;
    let base_height = (chrome.height * shape_scale).round() as u32;
    let width = base_width.max(SUPERSAMPLE) + (base_width.max(SUPERSAMPLE) % SUPERSAMPLE);
    let height = base_height.max(SUPERSAMPLE) + (base_height.max(SUPERSAMPLE) % SUPERSAMPLE);
    let image = Rasterizer::new(
        Size {
            width: width as f32,
            height: height as f32,
        },
        BACKGROUND,
    )
    .execute(&list);
    downsample(&image, SUPERSAMPLE as usize)
}

/// Reusable cache for the expensive, mostly-static chrome background.
///
/// Holds a single entry keyed by window geometry, scale and interaction state.
/// A redraw only re-rasterizes the chrome when one of those inputs changes;
/// otherwise it reuses the cached image. Interior mutability lets `render_window`
/// keep its `&`-only signature.
pub struct ChromeCache {
    cached: Mutex<Option<(ChromeKey, ImageBuffer)>>,
}

impl ChromeCache {
    /// Creates an empty cache.
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }

    /// Returns the cached chrome base image for the current state.
    pub fn chrome_base(
        &self,
        browser: &Browser,
        ui: &UiState,
        chrome: &ChromeLayout,
        scale: f32,
    ) -> ImageBuffer {
        let key = ChromeKey::new(browser, ui, chrome, scale);
        let mut guard = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached_key, image)) = guard.as_ref()
            && *cached_key == key
        {
            return image.clone();
        }
        let image = build_chrome_base(browser, ui, chrome, scale);
        *guard = Some((key, image.clone()));
        image
    }

    /// Composes a full frame from the cached chrome base plus the per-frame
    /// layers (page, icons and text).
    ///
    /// `page` may be `None` to draw chrome only, which keeps the UI responsive
    /// while an off-thread [`FramePipeline`] render is still in flight.
    pub fn render_frame(
        &self,
        browser: &Browser,
        ui: &UiState,
        chrome: &ChromeLayout,
        page: Option<&ImageBuffer>,
        scale: f32,
    ) -> ImageBuffer {
        self.render_frame_scaled(browser, ui, chrome, page, scale, scale)
    }

    /// Composes a frame with separate scales for chrome and page content.
    ///
    /// `scale` stays the device pixel ratio so chrome (and the output image
    /// size) remain readable and exactly match the window. `page_scale` is the
    /// device ratio multiplied by the logical zoom level; the page image is
    /// rasterized at that scale and composited 1:1 into the page rect, so zoom
    /// magnifies content without magnifying the UI. `composite_page` clips the
    /// magnified image to the page rect so it never bleeds over the chrome.
    pub fn render_frame_scaled(
        &self,
        browser: &Browser,
        ui: &UiState,
        chrome: &ChromeLayout,
        page: Option<&ImageBuffer>,
        scale: f32,
        page_scale: f32,
    ) -> ImageBuffer {
        let device = device_scale(scale);
        let page_device = device_scale(page_scale);
        let zoom = if device > 0.0 {
            page_device / device
        } else {
            1.0
        };
        let mut image = self.chrome_base(browser, ui, chrome, scale);
        if let Some(page) = page
            && !chrome.page.is_empty()
        {
            composite_page(&mut image, page, chrome.page, device);
        }
        paint_scrollbars(&mut image, browser, chrome, device, zoom);
        paint_find_highlights(&mut image, ui, chrome, device, zoom);
        paint_icons(&mut image, ui, chrome, device);
        let (titles, active) = ui.tab_titles(browser);
        paint_text(&mut image, ui, chrome, &titles, active, device);
        if ui.find_open {
            paint_find_bar(&mut image, ui, chrome, device);
        }
        image
    }
}

impl Default for ChromeCache {
    fn default() -> Self {
        Self::new()
    }
}

fn global_cache() -> &'static ChromeCache {
    static CACHE: OnceLock<ChromeCache> = OnceLock::new();
    CACHE.get_or_init(ChromeCache::new)
}

/// Renders the full window composition at `scale` device pixels per CSS pixel.
///
/// The page is still rasterized synchronously here for callers that have no
/// [`FramePipeline`]; the chrome base comes from the process-wide cache.
pub fn render_window(
    browser: &Browser,
    ui: &UiState,
    chrome: &ChromeLayout,
    scale: f32,
) -> ImageBuffer {
    let device = device_scale(scale);
    let page = (!chrome.page.is_empty()).then(|| browser.render_scaled(device));
    global_cache().render_frame(browser, ui, chrome, page.as_ref(), scale)
}

/// Like [`render_window`], but composites a page image supplied by the caller
/// (typically [`FramePipeline::latest`]) instead of rasterizing one inline.
#[allow(dead_code)] // Wired into `window_app.rs` by the native-window integration.
pub fn render_window_with_page(
    browser: &Browser,
    ui: &UiState,
    chrome: &ChromeLayout,
    scale: f32,
    page: Option<&ImageBuffer>,
) -> ImageBuffer {
    global_cache().render_frame(browser, ui, chrome, page, scale)
}

/// Variant of [`render_window_with_page`] with an independent page render scale.
///
/// The native window uses this for `Ctrl+Plus` / `Ctrl+Minus` / `Ctrl+0`: the
/// page is rasterized at `page_scale` (device ratio x logical zoom) while the
/// chrome and the output image stay at `scale` (plain device ratio).
#[allow(dead_code)] // Wired into `window_app.rs` by the native-window integration.
pub fn render_window_with_page_scaled(
    browser: &Browser,
    ui: &UiState,
    chrome: &ChromeLayout,
    scale: f32,
    page: Option<&ImageBuffer>,
    page_scale: f32,
) -> ImageBuffer {
    global_cache().render_frame_scaled(browser, ui, chrome, page, scale, page_scale)
}

/// Reduces a supersampled image by averaging `factor x factor` blocks.
fn downsample(source: &ImageBuffer, factor: usize) -> ImageBuffer {
    let width = source.width / factor as u32;
    let height = source.height / factor as u32;
    let mut output = ImageBuffer::new(width, height);
    let count = (factor * factor) as u32;
    for y in 0..height {
        for x in 0..width {
            let mut r = 0_u32;
            let mut g = 0_u32;
            let mut b = 0_u32;
            let mut a = 0_u32;
            for dy in 0..factor as u32 {
                for dx in 0..factor as u32 {
                    let pixel = source.pixel(x * factor as u32 + dx, y * factor as u32 + dy);
                    r += u32::from(pixel[0]);
                    g += u32::from(pixel[1]);
                    b += u32::from(pixel[2]);
                    a += u32::from(pixel[3]);
                }
            }
            let index = (y as usize * width as usize + x as usize) * 4;
            output.pixels[index] = (r / count) as u8;
            output.pixels[index + 1] = (g / count) as u8;
            output.pixels[index + 2] = (b / count) as u8;
            output.pixels[index + 3] = (a / count) as u8;
        }
    }
    output
}

fn composite_page(image: &mut ImageBuffer, page: &ImageBuffer, rect: Rect, scale: f32) {
    let x = (rect.x * scale).round() as i32;
    let y = (rect.y * scale).round() as i32;
    // A zoomed page image is larger than the viewport. Clip to the page rect so
    // magnified content never paints over the tab strip, toolbar or status pill.
    let right = ((rect.x + rect.width) * scale).round() as i32;
    let bottom = ((rect.y + rect.height) * scale).round() as i32;
    for py in 0..page.height as i32 {
        let dy = y + py;
        if dy < 0 || dy >= bottom || dy >= image.height as i32 {
            continue;
        }
        for px in 0..page.width as i32 {
            let dx = x + px;
            if dx < 0 || dx >= right || dx >= image.width as i32 {
                continue;
            }
            let src = (py as usize * page.width as usize + px as usize) * 4;
            let dst = (dy as usize * image.width as usize + dx as usize) * 4;
            image.pixels[dst..dst + 4].copy_from_slice(&page.pixels[src..src + 4]);
        }
    }
}

fn paint_icons(image: &mut ImageBuffer, ui: &UiState, chrome: &ChromeLayout, scale: f32) {
    let back = chevron(chrome.back, false, scale);
    let forward = chevron(chrome.forward, true, scale);
    let reload = reload_icon(chrome.reload, scale);
    fill_polygons(image, &back, MUTED_INK);
    fill_polygons(image, &forward, MUTED_INK);
    fill_polygons(image, &reload, MUTED_INK);

    // Site-identity marker: a lock for secure (https) pages, a globe otherwise.
    let secure = is_secure(ui.address.text());
    let icon = security_icon(ui.address.text(), chrome.address, scale);
    fill_polygons(image, &icon, if secure { ACCENT } else { MUTED_INK });

    // New-tab plus: boldens on hover, turns accent while pressed.
    if chrome.new_tab_visible {
        let plus = plus_icon(chrome.new_tab, scale, icon_weight(ui, ChromePart::NewTab));
        let color = match (ui.pressed, ui.hover) {
            (Some(ChromePart::NewTab), _) => ACCENT,
            (_, Some(ChromePart::NewTab)) => INK,
            _ => MUTED_INK,
        };
        fill_polygons(image, &plus, color);
    }

    // Per-tab close buttons use the same feedback language.
    for (index, rect) in chrome.tab_closes.iter().enumerate() {
        let part = ChromePart::CloseTab(index);
        let color = match (ui.pressed, ui.hover) {
            (Some(pressed), _) if pressed == part => ACCENT,
            (_, Some(hover)) if hover == part => INK,
            _ => MUTED_INK,
        };
        fill_polygons(
            image,
            &close_icon(*rect, scale, icon_weight(ui, part)),
            color,
        );
    }
}

/// How much thicker an icon stroke becomes on hover / press.
fn icon_weight(ui: &UiState, part: ChromePart) -> f32 {
    if ui.pressed == Some(part) {
        1.4
    } else if ui.hover == Some(part) {
        1.2
    } else {
        1.0
    }
}

/// True when the address text names a secure (https) resource.
fn is_secure(text: &str) -> bool {
    text.trim_start()
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
}

/// Builds the polygon set for the site-security marker inside the address field.
fn security_icon(text: &str, address: Rect, scale: f32) -> Vec<Vec<(f32, f32)>> {
    let size = ADDRESS_ICON_SIZE * scale;
    let x = (address.x + ADDRESS_INSET) * scale;
    let y = address.y * scale + (address.height * scale - size) / 2.0;
    let rect = Rect::new(x, y, size, size);
    if is_secure(text) {
        lock_icon(rect, scale)
    } else {
        globe_icon(rect, scale)
    }
}

/// A closed padlock: a solid body plus a half-ring shackle.
fn lock_icon(rect: Rect, scale: f32) -> Vec<Vec<(f32, f32)>> {
    let s = rect.width.max(scale);
    let cx = rect.x + s / 2.0;
    let cy = rect.y + s / 2.0;
    // Body.
    let left = cx - 0.34 * s;
    let right = cx + 0.34 * s;
    let top = cy - 0.02 * s;
    let bottom = cy + 0.48 * s;
    let body = vec![(left, top), (right, top), (right, bottom), (left, bottom)];
    // Shackle: the upper half of an annulus, resting on the body.
    let outer = 0.24 * s;
    let inner = 0.13 * s;
    let steps = 12;
    let mut shackle = Vec::new();
    for step in 0..=steps {
        let angle = std::f32::consts::PI + std::f32::consts::PI * step as f32 / steps as f32;
        shackle.push((cx + outer * angle.cos(), top + outer * angle.sin()));
    }
    for step in (0..=steps).rev() {
        let angle = std::f32::consts::PI + std::f32::consts::PI * step as f32 / steps as f32;
        shackle.push((cx + inner * angle.cos(), top + inner * angle.sin()));
    }
    vec![body, shackle]
}

/// A wireframe globe: an outer ring plus crossed meridians.
fn globe_icon(rect: Rect, scale: f32) -> Vec<Vec<(f32, f32)>> {
    let s = rect.width.max(scale);
    let cx = rect.x + s / 2.0;
    let cy = rect.y + s / 2.0;
    let outer = 0.46 * s;
    let line = 0.09 * s;
    vec![
        ring(cx, cy, outer, outer - line, 20),
        ellipse_ring(cx, cy, 0.17 * s, outer, line, 16),
        ellipse_ring(cx, cy, outer, 0.17 * s, line, 16),
    ]
}

/// An annulus polygon (outer loop forward, inner loop reversed for even-odd fill).
fn ring(cx: f32, cy: f32, outer: f32, inner: f32, steps: usize) -> Vec<(f32, f32)> {
    ellipse_ring(cx, cy, outer, outer, (outer - inner).max(0.0), steps)
}

/// An elliptical ring polygon; `thickness` is subtracted from both radii.
fn ellipse_ring(
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    thickness: f32,
    steps: usize,
) -> Vec<(f32, f32)> {
    let mut points = Vec::new();
    for step in 0..=steps {
        let angle = std::f32::consts::TAU * step as f32 / steps as f32;
        points.push((cx + rx * angle.cos(), cy + ry * angle.sin()));
    }
    let (ix, iy) = ((rx - thickness).max(0.0), (ry - thickness).max(0.0));
    for step in (0..=steps).rev() {
        let angle = std::f32::consts::TAU * step as f32 / steps as f32;
        points.push((cx + ix * angle.cos(), cy + iy * angle.sin()));
    }
    points
}

/// A small crossed pair of strokes for closing a tab.
fn close_icon(rect: Rect, scale: f32, weight: f32) -> Vec<Vec<(f32, f32)>> {
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let radius = rect.width * 0.28;
    let thickness = (rect.width * 0.11 * weight).max(scale);
    let a = (cx - radius, cy - radius);
    let b = (cx + radius, cy + radius);
    let c = (cx + radius, cy - radius);
    let d = (cx - radius, cy + radius);
    vec![
        stroke_polygon(a, b, thickness),
        stroke_polygon(c, d, thickness),
    ]
}

fn stroke_polygon(a: (f32, f32), b: (f32, f32), thickness: f32) -> Vec<(f32, f32)> {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let length = (dx * dx + dy * dy).sqrt().max(0.001);
    let nx = -dy / length * thickness;
    let ny = dx / length * thickness;
    vec![
        (a.0 + nx, a.1 + ny),
        (b.0 + nx, b.1 + ny),
        (b.0 - nx, b.1 - ny),
        (a.0 - nx, a.1 - ny),
    ]
}

/// A solid chevron built from two overlapping quadrilaterals.
fn chevron(rect: Rect, right: bool, scale: f32) -> Vec<Vec<(f32, f32)>> {
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let direction = if right { -1.0 } else { 1.0 };
    let p = |x: f32, y: f32| (cx + direction * x * scale, cy + y * scale);
    vec![
        vec![p(4.0, -5.5), p(-2.0, 0.0), p(-2.0, -2.6), p(4.0, 1.8)],
        vec![p(-2.0, 0.0), p(4.0, 5.5), p(4.0, 2.6), p(-2.0, -1.8)],
    ]
}

/// A circular reload arrow: an annulus segment plus a solid arrowhead.
fn reload_icon(rect: Rect, scale: f32) -> Vec<Vec<(f32, f32)>> {
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let outer = 6.0 * scale;
    let inner = 3.6 * scale;
    let start = 0.6_f32;
    let end = std::f32::consts::TAU - 0.5;
    let steps = 24;
    let mut ring = Vec::new();
    for step in 0..=steps {
        let angle = start + (end - start) * step as f32 / steps as f32;
        ring.push((cx + outer * angle.cos(), cy + outer * angle.sin()));
    }
    for step in (0..=steps).rev() {
        let angle = start + (end - start) * step as f32 / steps as f32;
        ring.push((cx + inner * angle.cos(), cy + inner * angle.sin()));
    }
    let tip = (cx + outer * start.cos(), cy + outer * start.sin());
    let head = vec![
        (tip.0 - 0.5 * scale, tip.1 - 3.4 * scale),
        (tip.0 + 3.4 * scale, tip.1 + 0.4 * scale),
        (tip.0 - 3.2 * scale, tip.1 + 1.4 * scale),
    ];
    vec![ring, head]
}

fn plus_icon(rect: Rect, scale: f32, weight: f32) -> Vec<Vec<(f32, f32)>> {
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let h = 1.0 * scale * weight;
    let l = 5.5 * scale;
    vec![
        vec![
            (cx - l, cy - h),
            (cx + l, cy - h),
            (cx + l, cy + h),
            (cx - l, cy + h),
        ],
        vec![
            (cx - h, cy - l),
            (cx + h, cy - l),
            (cx + h, cy + l),
            (cx - h, cy + l),
        ],
    ]
}

/// Rasterises filled polygons with 4x4 coverage sampling and source-over ink.
fn fill_polygons(image: &mut ImageBuffer, polygons: &[Vec<(f32, f32)>], color: Color) {
    let Some(((mut min_x, mut min_y), (mut max_x, mut max_y))) = polygons
        .iter()
        .flat_map(|polygon| polygon.iter().copied())
        .map(|point| (point, point))
        .reduce(|a, b| {
            (
                (a.0.0.min(b.0.0), a.0.1.min(b.0.1)),
                (a.1.0.max(b.1.0), a.1.1.max(b.1.1)),
            )
        })
    else {
        return;
    };
    min_x -= 1.0;
    min_y -= 1.0;
    max_x += 1.0;
    max_y += 1.0;
    let sub = ICON_SUB as f32;
    for y in min_y.floor() as i32..=max_y.ceil() as i32 {
        for x in min_x.floor() as i32..=max_x.ceil() as i32 {
            let mut hits = 0_usize;
            for sy in 0..ICON_SUB {
                let py = y as f32 + (sy as f32 + 0.5) / sub;
                for sx in 0..ICON_SUB {
                    let px = x as f32 + (sx as f32 + 0.5) / sub;
                    if polygons.iter().any(|polygon| inside(polygon, px, py)) {
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let coverage = (hits * 255 / (ICON_SUB * ICON_SUB)) as u8;
            let alpha = (u16::from(coverage) * u16::from(color.a) / 255) as u8;
            blend(image, x, y, color, alpha);
        }
    }
}

fn inside(polygon: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut previous = polygon.len().saturating_sub(1);
    for current in 0..polygon.len() {
        let (ax, ay) = polygon[current];
        let (bx, by) = polygon[previous];
        if ((ay > y) != (by > y)) && (x < (bx - ax) * (y - ay) / (by - ay) + ax) {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn blend(image: &mut ImageBuffer, x: i32, y: i32, color: Color, alpha: u8) {
    if x < 0 || y < 0 || x >= image.width as i32 || y >= image.height as i32 || alpha == 0 {
        return;
    }
    let index = ((y as u32 * image.width + x as u32) * 4) as usize;
    let a = f32::from(alpha) / 255.0;
    let inverse = 1.0 - a;
    image.pixels[index] =
        (f32::from(color.r) * a + f32::from(image.pixels[index]) * inverse).round() as u8;
    image.pixels[index + 1] =
        (f32::from(color.g) * a + f32::from(image.pixels[index + 1]) * inverse).round() as u8;
    image.pixels[index + 2] =
        (f32::from(color.b) * a + f32::from(image.pixels[index + 2]) * inverse).round() as u8;
    image.pixels[index + 3] = 255;
}

fn paint_text(
    image: &mut ImageBuffer,
    ui: &UiState,
    chrome: &ChromeLayout,
    titles: &[String],
    active: usize,
    scale: f32,
) {
    let atlas = atlas();
    let font = Font::sans();
    for (index, rect) in chrome.tabs.iter().enumerate() {
        let Some(title) = titles.get(index) else {
            continue;
        };
        let color = if index == active { INK } else { MUTED_INK };
        let visible = fit_text(title, chrome.tab_text_width(), font, FONT_SIZE);
        atlas.paint_text_centered(
            image,
            (rect.x + PADDING) * scale,
            rect.y * scale,
            rect.height * scale,
            &visible,
            FONT_SIZE * scale,
            color,
            Some((rect.width - 2.0 * PADDING) * scale),
        );
    }

    let visible = ui
        .address
        .visible_width(chrome.address_text_width(), font, FONT_SIZE);
    let color = if visible.text.starts_with('\u{2026}') {
        MUTED_INK
    } else {
        INK
    };
    atlas.paint_text_centered(
        image,
        (chrome.address.x + ChromeLayout::address_text_inset()) * scale,
        chrome.address.y * scale,
        chrome.address.height * scale,
        &visible.text,
        FONT_SIZE * scale,
        color,
        Some(chrome.address_text_width() * scale),
    );

    atlas.paint_text_centered(
        image,
        (chrome.status.x + PADDING) * scale,
        chrome.status.y * scale,
        STATUS_HEIGHT * scale,
        ui.status.text(),
        STATUS_FONT_SIZE * scale,
        MUTED_INK,
        Some((chrome.status.width - 2.0 * PADDING) * scale),
    );
}

/// Paints the page's scrollbar gutters and thumbs over the composed page.
///
/// Scrollbars are a per-frame layer (not part of the cached chrome base) because
/// they move with the page's scroll offset. They use the same document→page
/// transform as the find highlights, so they stay registered with the zoomed
/// content. Both the track and the thumb are anti-aliased polygons.
fn paint_scrollbars(
    image: &mut ImageBuffer,
    browser: &Browser,
    chrome: &ChromeLayout,
    device: f32,
    zoom: f32,
) {
    if zoom <= 0.0 || chrome.page.is_empty() {
        return;
    }
    let clip = scaled_rect(chrome.page, device);
    for bar in scrollbars(browser) {
        let to_device = |rect: Rect| {
            Rect::new(
                (chrome.page.x + rect.x * zoom) * device,
                (chrome.page.y + rect.y * zoom) * device,
                (rect.width * zoom * device).max(0.0),
                (rect.height * zoom * device).max(0.0),
            )
        };
        let track = clamp_rect(to_device(bar.track), clip);
        let thumb = clamp_rect(to_device(bar.thumb), clip);
        if track.is_empty() || thumb.is_empty() {
            continue;
        }
        let radius = (track.width.min(track.height) / 2.0).min(6.0 * device);
        fill_polygons(
            image,
            &[rounded_rect_polygon(track, radius, 4)],
            SCROLLBAR_TRACK,
        );
        fill_polygons(
            image,
            &[rounded_rect_polygon(thumb, radius, 4)],
            SCROLLBAR_THUMB,
        );
    }
}

/// Clips `rect` to `clip`, returning an empty rectangle when they do not overlap.
fn clamp_rect(rect: Rect, clip: Rect) -> Rect {
    let x = rect.x.max(clip.x);
    let y = rect.y.max(clip.y);
    let right = rect.right().min(clip.right());
    let bottom = rect.bottom().min(clip.bottom());
    Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
}

/// Paints approximate rectangles for every find-in-page match over the page.
///
/// Highlights are a per-frame layer so changing the query never invalidates the
/// cached chrome base. They are clipped to the page rectangle and use the same
/// zoom transform as the page image, keeping them registered with the text.
fn paint_find_highlights(
    image: &mut ImageBuffer,
    ui: &UiState,
    chrome: &ChromeLayout,
    device: f32,
    zoom: f32,
) {
    if !ui.find_open || ui.find_highlights.is_empty() || zoom <= 0.0 {
        return;
    }
    let clip = scaled_rect(chrome.page, device);
    for highlight in &ui.find_highlights {
        if highlight.is_empty() {
            continue;
        }
        let rect = Rect::new(
            (chrome.page.x + highlight.x * zoom) * device,
            (chrome.page.y + highlight.y * zoom) * device,
            highlight.width * zoom * device,
            highlight.height * zoom * device,
        );
        let color = if highlight.current {
            FIND_CURRENT
        } else {
            FIND_MATCH
        };
        fill_rect_alpha(image, rect, clip, color);
    }
}

/// Paints the find bar card, query field, match counter and control icons.
fn paint_find_bar(image: &mut ImageBuffer, ui: &UiState, chrome: &ChromeLayout, device: f32) {
    let bar = scaled_rect(chrome.find_bar, device);
    let radius = 8.0 * device;
    fill_polygons(image, &[rounded_rect_polygon(bar, radius, 5)], HAIRLINE);
    fill_polygons(
        image,
        &[rounded_rect_polygon(inset(bar, device), radius - device, 5)],
        Color::rgb(255, 255, 255),
    );

    let field = scaled_rect(chrome.find_input, device);
    let field_border = if ui.find_focused { ACCENT } else { HAIRLINE };
    fill_polygons(
        image,
        &[rounded_rect_polygon(field, 5.0 * device, 4)],
        field_border,
    );
    fill_polygons(
        image,
        &[rounded_rect_polygon(inset(field, device), 4.0 * device, 4)],
        Color::rgb(255, 255, 255),
    );

    let font = Font::sans();
    let counter = match (ui.find.current_index(), ui.find.match_count()) {
        (_, 0) => "0/0".to_string(),
        (Some(index), total) => format!("{}/{}", index + 1, total),
        (None, total) => format!("0/{total}"),
    };
    let counter_width = font.measure(&counter, FONT_SIZE) + 8.0;
    let query_width = (chrome.find_input.width - counter_width - 12.0).max(0.0);
    let query = fit_text(ui.find.query(), query_width, font, FONT_SIZE);
    atlas().paint_text_centered(
        image,
        (chrome.find_input.x + 6.0) * device,
        field.y,
        field.height,
        &query,
        FONT_SIZE * device,
        INK,
        Some(query_width * device),
    );
    atlas().paint_text_centered(
        image,
        (chrome.find_input.right() - counter_width) * device,
        field.y,
        field.height,
        &counter,
        FONT_SIZE * device,
        MUTED_INK,
        Some(counter_width * device),
    );

    fill_polygons(image, &chevron(chrome.find_prev, false, device), MUTED_INK);
    fill_polygons(image, &chevron(chrome.find_next, true, device), MUTED_INK);
    fill_polygons(
        image,
        &close_icon(chrome.find_close, device, 1.0),
        MUTED_INK,
    );
}

/// Paints a context menu over the composed window.
///
/// The model owns the item rectangles and labels; this function only turns them
/// into pixels. It is called by the native window after [`render_window_with_page`].
#[allow(dead_code)] // Wired into `window_app.rs` by the native-window integration.
pub fn render_context_menu(
    image: &mut ImageBuffer,
    menu: &ContextMenu,
    hover: Option<ContextAction>,
    scale: f32,
) {
    let Some(bounds) = menu.bounds() else {
        return;
    };
    let rect = Rect::new(
        bounds.x * scale,
        bounds.y * scale,
        bounds.width * scale,
        bounds.height * scale,
    );
    let radius = 8.0 * scale;
    // A faint offset shadow, then a hairline-bordered white card.
    let shadow = Rect::new(
        rect.x + 2.0 * scale,
        rect.y + 3.0 * scale,
        rect.width,
        rect.height,
    );
    fill_polygons(
        image,
        &[rounded_rect_polygon(shadow, radius, 5)],
        Color::rgba(0, 0, 0, 28),
    );
    fill_polygons(image, &[rounded_rect_polygon(rect, radius, 5)], HAIRLINE);
    fill_polygons(
        image,
        &[rounded_rect_polygon(inset(rect, scale), radius - scale, 5)],
        Color::rgb(255, 255, 255),
    );

    for item in &menu.items {
        if item.enabled && hover == Some(item.action) {
            let row = Rect::new(
                item.bounds.x * scale,
                item.bounds.y * scale,
                item.bounds.width * scale,
                item.bounds.height * scale,
            );
            fill_rect_alpha(image, inset(row, scale), rect, HOVER);
        }
        let color = if item.enabled { INK } else { MUTED_INK };
        atlas().paint_text_centered(
            image,
            (item.bounds.x + 12.0) * scale,
            item.bounds.y * scale,
            item.bounds.height * scale,
            &item.label,
            FONT_SIZE * scale,
            color,
            Some((item.bounds.width - 20.0) * scale),
        );
    }
}

/// Fills a rectangle with a translucent colour, clipped to `clip`.
fn fill_rect_alpha(image: &mut ImageBuffer, rect: Rect, clip: Rect, color: Color) {
    if color.a == 0 || rect.is_empty() {
        return;
    }
    let x0 = rect.x.max(clip.x).max(0.0).ceil() as i32;
    let y0 = rect.y.max(clip.y).max(0.0).ceil() as i32;
    let x1 = rect
        .right()
        .min(clip.right())
        .min(image.width as f32)
        .floor() as i32;
    let y1 = rect
        .bottom()
        .min(clip.bottom())
        .min(image.height as f32)
        .floor() as i32;
    for y in y0..y1 {
        for x in x0..x1 {
            blend(image, x, y, color, color.a);
        }
    }
}

/// Builds a rounded rectangle as a polygon with `steps` samples per corner.
fn rounded_rect_polygon(rect: Rect, radius: f32, steps: usize) -> Vec<(f32, f32)> {
    let radius = radius.clamp(0.0, rect.width.min(rect.height) / 2.0);
    let quarter = std::f32::consts::FRAC_PI_2;
    let corners = [
        (rect.right() - radius, rect.y + radius, -quarter),
        (rect.right() - radius, rect.bottom() - radius, 0.0),
        (rect.x + radius, rect.bottom() - radius, quarter),
        (rect.x + radius, rect.y + radius, std::f32::consts::PI),
    ];
    let mut points = Vec::with_capacity(4 * (steps + 1));
    for (cx, cy, start) in corners {
        for step in 0..=steps {
            let angle = start + quarter * step as f32 / steps as f32;
            points.push((cx + radius * angle.cos(), cy + radius * angle.sin()));
        }
    }
    points
}

fn caret_position(ui: &UiState, chrome: &ChromeLayout, scale: f32) -> Option<f32> {
    let font = Font::sans();
    let visible = ui
        .address
        .visible_width(chrome.address_text_width(), font, FONT_SIZE);
    let before: String = visible.text.chars().take(visible.caret_col).collect();
    let x =
        (chrome.address.x + ChromeLayout::address_text_inset() + font.measure(&before, FONT_SIZE))
            * scale;
    if x >= (chrome.address.right() - 4.0) * scale {
        None
    } else {
        Some(x)
    }
}

/// Truncates `text` to `max_width` measured pixels, appending an ellipsis.
pub fn fit_text(text: &str, max_width: f32, font: &Font, px: f32) -> String {
    if max_width <= 0.0 || px <= 0.0 {
        return String::new();
    }
    if font.measure(text, px) <= max_width {
        return text.to_string();
    }
    let ellipsis = "\u{2026}";
    let available = (max_width - font.measure(ellipsis, px)).max(0.0);
    let mut out = String::new();
    let mut width = 0.0;
    for ch in text.chars() {
        let advance = font.advance(ch, px);
        if width + advance > available {
            break;
        }
        out.push(ch);
        width += advance;
    }
    out.push_str(ellipsis);
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

/// Draws a rounded outline of `thickness` pixels and fills its interior.
fn stroke_rounded(
    list: &mut DisplayList,
    rect: Rect,
    radius: f32,
    thickness: f32,
    border: Color,
    interior: Color,
) {
    fill_rounded(list, rect, radius, border);
    fill_rounded(
        list,
        inset(rect, thickness),
        (radius - thickness).max(0.0),
        interior,
    );
}

/// Draws the active tab's underline.
///
/// When a load is in flight the solid accent bar becomes a short, deterministic
/// progress bar: a faint full-width track with a solid accent segment. Keeping
/// the pattern a pure function of the tab rectangle means the self-check PNG is
/// byte-for-byte stable — no wall-clock animation is involved.
fn tab_underline(list: &mut DisplayList, rect: Rect, pad: f32, shape_scale: f32, loading: bool) {
    let thickness = 2.0 * shape_scale;
    let x = rect.x + pad;
    let width = (rect.width - 2.0 * pad).max(0.0);
    let y = rect.bottom() - thickness;
    if !loading {
        fill(list, Rect::new(x, y, width, thickness), ACCENT);
        return;
    }
    // Faint track across the whole underline...
    fill(
        list,
        Rect::new(x, y, width, thickness),
        Color::rgba(ACCENT.r, ACCENT.g, ACCENT.b, 64),
    );
    // ...plus a shorter, solid segment that reads as progress.
    let progress = (width * 0.42).max(thickness).min(width);
    fill(list, Rect::new(x, y, progress, thickness), ACCENT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::TAB_STRIP_HEIGHT;
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
    fn chrome_cache_reuses_base_and_tracks_state() {
        let browser = browser();
        let ui = UiState::new();
        let chrome = chrome();
        let cache = ChromeCache::new();

        let first = cache.chrome_base(&browser, &ui, &chrome, 1.0);
        let second = cache.chrome_base(&browser, &ui, &chrome, 1.0);
        assert_eq!(first, second, "identical state should hit the cache");

        let mut hovered = UiState::new();
        hovered.hover = Some(ChromePart::Back);
        let hover_base = cache.chrome_base(&browser, &hovered, &chrome, 1.0);
        assert_ne!(first, hover_base, "hover must invalidate the cached base");

        let hidpi = cache.chrome_base(&browser, &ui, &chrome, 2.0);
        assert_ne!(first, hidpi, "scale must invalidate the cached base");
    }

    #[test]
    fn supplied_page_composes_like_render_window() {
        let browser = browser();
        let ui = UiState::new();
        let chrome = chrome();
        let expected = render_window(&browser, &ui, &chrome, 1.0);
        let page = browser.render_scaled(1.0);
        let composed = render_window_with_page(&browser, &ui, &chrome, 1.0, Some(&page));
        assert_eq!(composed, expected);
    }

    #[test]
    fn composition_has_chrome_and_page_regions() {
        let browser = browser();
        let mut ui = UiState::new();
        ui.address.set_exact("file:///tmp/page.html");
        let image = render_window(&browser, &ui, &chrome(), 1.0);
        assert_eq!((image.width, image.height), (800, 600));
        assert_eq!(image.pixel(700, 10), [0xf0, 0xf0, 0xf3, 255]);
        assert_eq!(image.pixel(796, 58), [0xf7, 0xf7, 0xf9, 255]);
        assert_eq!(
            image.pixel(400, CHROME_HEIGHT as u32),
            [0xd2, 0xd2, 0xd7, 255]
        );
        assert_eq!(image.pixel(400, 300), [255, 255, 255, 255]);
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
        let image = render_window(&browser, &ui, &chrome(), 1.0);
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
        let image = render_window(&browser, &ui, &chrome(), 1.0);
        let is_accent = |p: [u8; 4]| p[0] < 120 && p[1] > 90 && p[1] < 190 && p[2] > 200;
        let mut accent_pixels = 0;
        for x in 0..800 {
            if is_accent(image.pixel(x, 44)) {
                accent_pixels += 1;
            }
        }
        assert!(accent_pixels > 0);
        let caret = caret_position(&ui, &chrome(), 1.0).unwrap();
        let mut found = false;
        for y in 51..65 {
            if is_accent(image.pixel(caret.round() as u32, y)) {
                found = true;
            }
        }
        assert!(found);
    }

    #[test]
    fn status_line_changes_with_state() {
        let browser = browser();
        let mut ui = UiState::new();
        let ready = render_window(&browser, &ui, &chrome(), 1.0);
        let ready_pixels = status_ink(&ready);
        ui.status = LoadStatus::Error;
        let error = render_window(&browser, &ui, &chrome(), 1.0);
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
    fn hidpi_renders_at_device_resolution() {
        let browser = browser();
        let ui = UiState::new();
        let image = render_window(&browser, &ui, &chrome(), 2.0);
        assert_eq!((image.width, image.height), (1600, 1200));
        assert_eq!(image.pixel(1400, 20), [0xf0, 0xf0, 0xf3, 255]);
    }

    #[test]
    fn icons_have_anti_aliased_edges() {
        let chrome = chrome();
        let icon = chevron(chrome.back, false, 1.0);
        let blended = {
            let mut image = ImageBuffer::filled(100, 100, Color::rgb(255, 255, 255));
            fill_polygons(&mut image, &icon, Color::rgb(0, 0, 0));
            let mut partial = 0;
            for y in 0..100 {
                for x in 0..100 {
                    let value = image.pixel(x, y)[0];
                    if value > 0 && value < 255 {
                        partial += 1;
                    }
                }
            }
            partial
        };
        assert!(blended > 0, "icon edges should be anti-aliased");
        assert!(!icon.is_empty());
    }

    #[test]
    fn fitted_text_never_leaves_the_field() {
        let font = Font::sans();
        assert_eq!(fit_text("short", 200.0, font, FONT_SIZE), "short");
        let fitted = fit_text("a-very-long-title", 30.0, font, FONT_SIZE);
        assert!(fitted.ends_with('\u{2026}'));
        assert!(font.measure(&fitted, FONT_SIZE) <= 30.0 + 0.01);
    }

    fn scrollable_browser() -> Browser {
        let mut browser = browser();
        browser
            .load_html(
                "<html><body style='margin:0'><div id='s' \
                 style='width:200px;height:100px;overflow-y:scroll'>\
                 <div style='height:400px'></div></div></body></html>",
                None,
            )
            .unwrap();
        browser
    }

    fn vertical_bar(browser: &Browser) -> ScrollbarGeometry {
        scrollbars(browser)
            .into_iter()
            .find(|bar| bar.axis == ScrollbarAxis::Vertical)
            .expect("the overflowing div needs a vertical scrollbar")
    }

    #[test]
    fn scrollbar_thumb_tracks_the_scroll_offset() {
        let mut browser = scrollable_browser();
        let initial = vertical_bar(&browser);
        assert!(initial.max_scroll > 0.0);
        assert!(initial.thumb.height < initial.track.height);
        assert!(initial.thumb.y >= initial.track.y);
        // Both the thumb and the track are hittable in document coordinates.
        assert!(scrollbar_at(&browser, initial.thumb.x + 2.0, initial.thumb.y + 2.0).is_some());

        // The gutter is outside the scroller's border box, so scroll from a
        // point just inside the container, not from the track itself.
        browser.scroll_at(initial.track.x - 2.0, initial.track.y + 2.0, 100.0);
        let scrolled = vertical_bar(&browser);
        assert!(
            scrolled.thumb.y > initial.thumb.y,
            "thumb must move down as the content scrolls"
        );
    }

    #[test]
    fn scrollbars_are_painted_over_the_page() {
        let browser = scrollable_browser();
        let bar = vertical_bar(&browser);
        let chrome = chrome();
        let image = render_window(&browser, &UiState::new(), &chrome, 1.0);
        let x = (chrome.page.x + bar.thumb.x + bar.thumb.width / 2.0) as u32;
        let y = (chrome.page.y + bar.thumb.y + bar.thumb.height / 2.0) as u32;
        let pixel = image.pixel(x, y);
        assert!(
            pixel[0] < 220 && pixel[3] == 255,
            "the thumb should be a visible grey over the page: {pixel:?}"
        );
    }

    fn chrome_for(status: LoadStatus) -> ChromeLayout {
        ChromeLayout::new(
            Size {
                width: 800.0,
                height: 600.0,
            },
            1,
            status,
        )
    }

    #[test]
    fn address_security_marker_depends_on_scheme() {
        let chrome = chrome();
        let lock = security_icon("https://example.com", chrome.address, 1.0);
        let globe = security_icon("http://example.com", chrome.address, 1.0);
        assert!(!lock.is_empty() && !globe.is_empty());
        assert_ne!(lock[0], globe[0], "lock and globe are distinct shapes");

        // Both markers stay inside the reserved slot, left of the text inset.
        let inset = ChromeLayout::address_text_inset();
        for polygon in lock.iter().chain(globe.iter()) {
            for &(x, y) in polygon {
                assert!(x >= chrome.address.x + ADDRESS_INSET - 1.0);
                assert!(x <= chrome.address.x + inset + 1.0);
                assert!(y >= chrome.address.y && y <= chrome.address.bottom());
            }
        }
        assert!(is_secure("  HTTPS://example.com"));
        assert!(!is_secure("http://example.com"));
        assert!(!is_secure("mag.dev"));
    }

    #[test]
    fn address_renders_a_security_marker() {
        let browser = browser();
        let mut secure = UiState::new();
        secure.address.set_exact("https://example.com");
        let mut plain = UiState::new();
        plain.address.set_exact("http://example.com");
        let secure_image = render_window(&browser, &secure, &chrome(), 1.0);
        let plain_image = render_window(&browser, &plain, &chrome(), 1.0);

        let chrome = chrome();
        let x0 = (chrome.address.x + ADDRESS_INSET) as u32;
        let x1 = (chrome.address.x + ChromeLayout::address_text_inset()) as u32;
        let y0 = chrome.address.y as u32;
        let y1 = chrome.address.bottom() as u32;
        let mut differences = 0;
        let mut accent_pixels = 0;
        for x in x0..x1 {
            for y in y0..y1 {
                let a = secure_image.pixel(x, y);
                if a != plain_image.pixel(x, y) {
                    differences += 1;
                }
                if a[2] > 200 && a[0] < 120 {
                    accent_pixels += 1;
                }
            }
        }
        assert!(differences > 10, "lock and globe should look different");
        assert!(
            accent_pixels > 0,
            "secure marker should use the accent colour"
        );
    }

    #[test]
    fn loading_tab_underline_is_a_shorter_progress_bar() {
        let browser = browser();
        let idle = UiState::new();
        let ready = render_window(&browser, &idle, &chrome_for(LoadStatus::Ready), 1.0);
        let mut busy = UiState::new();
        busy.status = LoadStatus::Loading;
        let loading = render_window(&browser, &busy, &chrome_for(LoadStatus::Loading), 1.0);

        let tab = chrome().tabs[0];
        let y = (tab.bottom() - 2.0) as u32;
        let x0 = (tab.x + PADDING) as u32;
        let x1 = (tab.right() - PADDING) as u32;
        let is_accent = |p: [u8; 4]| p[0] < 120 && p[1] > 90 && p[1] < 190 && p[2] > 200;
        let ready_count = (x0..x1).filter(|&x| is_accent(ready.pixel(x, y))).count();
        let loading_count = (x0..x1).filter(|&x| is_accent(loading.pixel(x, y))).count();
        assert!(ready_count > 10, "ready underline should be solid accent");
        assert!(
            loading_count > 0 && loading_count < ready_count,
            "loading underline should be a shorter accent segment"
        );
    }

    #[test]
    fn find_bar_and_highlights_render_over_the_page() {
        use crate::ui_extra::find_overlay::highlights;

        let browser = browser();
        let chrome = chrome();
        let mut ui = UiState::new();
        let plain = render_window(&browser, &ui, &chrome, 1.0);

        ui.find_open = true;
        ui.find_focused = true;
        ui.find.set_query("Hello", &browser.page().text());
        ui.find_highlights = highlights(browser.page(), &ui.find);
        assert!(!ui.find_highlights.is_empty());
        let with_find = render_window(&browser, &ui, &chrome, 1.0);

        let changed = |rect: Rect| {
            (rect.x as u32..rect.right() as u32)
                .flat_map(|x| (rect.y as u32..rect.bottom() as u32).map(move |y| (x, y)))
                .filter(|&(x, y)| plain.pixel(x, y) != with_find.pixel(x, y))
                .count()
        };
        assert!(
            changed(chrome.find_bar) > 100,
            "the find bar must visibly change its overlay area"
        );
        let highlight = ui.find_highlights[0];
        let mark = Rect::new(
            chrome.page.x + highlight.x,
            chrome.page.y + highlight.y,
            highlight.width,
            highlight.height,
        );
        assert!(changed(mark) > 0, "match rectangles must tint the page");
    }

    #[test]
    fn context_menu_renders_its_rows() {
        use crate::ui_extra::{ContextMenu, Point};

        let browser = browser();
        let chrome = chrome();
        let ui = UiState::new();
        let plain = render_window(&browser, &ui, &chrome, 1.0);
        let menu = ContextMenu::new(Point::new(60.0, 120.0));

        let mut with_menu = plain.clone();
        render_context_menu(&mut with_menu, &menu, Some(ContextAction::Reload), 1.0);

        let bounds = menu.bounds().unwrap();
        let mut changed = 0;
        for x in bounds.x as u32..(bounds.x + bounds.width) as u32 {
            for y in bounds.y as u32..(bounds.y + bounds.height) as u32 {
                if plain.pixel(x, y) != with_menu.pixel(x, y) {
                    changed += 1;
                }
            }
        }
        assert!(changed > 100, "menu rows should be visible");
    }

    #[test]
    fn separate_page_scale_keeps_chrome_size_and_clips_the_page() {
        let browser = browser();
        let ui = UiState::new();
        let chrome = chrome();
        let page = browser.render_scaled(2.0);
        let image = render_window_with_page_scaled(&browser, &ui, &chrome, 1.0, Some(&page), 2.0);

        // Chrome and the output image stay at the device scale, even though the
        // page image carries twice the resolution.
        assert_eq!((image.width, image.height), (800, 600));
        assert_eq!(image.pixel(700, 10), [0xf0, 0xf0, 0xf3, 255]);
        // Magnified page content is clipped to the page rectangle.
        assert_eq!(
            image.pixel(400, CHROME_HEIGHT as u32),
            [0xd2, 0xd2, 0xd7, 255]
        );
    }

    #[test]
    fn pressed_new_tab_and_close_change_their_buttons() {
        let browser = browser();
        let chrome = chrome();
        let idle = UiState::new();
        let base = render_window(&browser, &idle, &chrome, 1.0);
        let count_changed = |image: &ImageBuffer, rect: Rect| {
            let mut changed = 0;
            for x in rect.x as u32..rect.right() as u32 {
                for y in rect.y as u32..rect.bottom() as u32 {
                    if base.pixel(x, y) != image.pixel(x, y) {
                        changed += 1;
                    }
                }
            }
            changed
        };

        let mut pressed = UiState::new();
        pressed.pressed = Some(ChromePart::NewTab);
        let image = render_window(&browser, &pressed, &chrome, 1.0);
        assert!(
            count_changed(&image, chrome.new_tab) > 10,
            "pressing + should visibly change its button"
        );

        let mut pressed = UiState::new();
        pressed.pressed = Some(ChromePart::CloseTab(0));
        let image = render_window(&browser, &pressed, &chrome, 1.0);
        assert!(
            count_changed(&image, chrome.tab_closes[0]) > 0,
            "pressing close should visibly change its button"
        );
    }
}
