//! The `DevTools` panel: state, input helpers and deterministic drawing.

use browser_core::Browser;
use browser_css::Color;
use browser_gpu::GlyphAtlas;
use browser_layout::Rect;
use browser_paint::ImageBuffer;

use super::{NetworkRequest, RequestStatus, ResourceType};

/// Fraction of the window height covered by the docked panel.
const PANEL_FRACTION: f32 = 0.40;
/// Height of the tab strip inside the panel, in logical pixels.
const TAB_BAR_HEIGHT: f32 = 34.0;
/// Height of a content header row.
const HEADER_HEIGHT: f32 = 22.0;
/// Height of one log/table row.
const ROW_HEIGHT: f32 = 18.0;
/// Horizontal inset for content text.
const PAD_X: f32 = 10.0;
/// Text size used for panel content.
const FONT_SIZE: f32 = 12.0;
/// Maximum requests retained in the log.
const MAX_REQUESTS: usize = 256;
/// Maximum diagnostics retained in the log.
const MAX_DIAGNOSTICS: usize = 256;
/// Upper bound for a stored scroll offset (rendering clamps to the content).
const MAX_SCROLL_LINES: usize = 4096;
/// Number of source blocks listed above the code view.
const MAX_SOURCE_BLOCKS: usize = 4;

const BG: Color = Color::rgb(0x1e, 0x1e, 0x23);
const BG_ALT: Color = Color::rgb(0x26, 0x26, 0x2d);
const TAB_ACTIVE: Color = Color::rgb(0x30, 0x30, 0x39);
const BORDER: Color = Color::rgb(0x3c, 0x3c, 0x47);
const INK: Color = Color::rgb(0xe8, 0xe8, 0xee);
const MUTED: Color = Color::rgb(0x9b, 0x9b, 0xa8);
const ACCENT: Color = Color::rgb(0x4f, 0xa8, 0xff);
const WARN: Color = Color::rgb(0xf2, 0xc4, 0x4f);
const ERROR: Color = Color::rgb(0xff, 0x6b, 0x6b);
const OK: Color = Color::rgb(0x6b, 0xd5, 0x8f);

/// The three panels exposed by the devtools tab strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DevToolsTab {
    /// Console messages and engine diagnostics.
    #[default]
    Console,
    /// App-layer request log.
    Network,
    /// Document source and inline blocks.
    Sources,
}

impl DevToolsTab {
    /// Tabs in drawing order.
    pub const ALL: [Self; 3] = [Self::Console, Self::Network, Self::Sources];

    /// Label drawn in the tab strip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::Network => "Network",
            Self::Sources => "Sources",
        }
    }
}

/// Severity of a console entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogLevel {
    /// Informational script output.
    #[default]
    Info,
    /// Recoverable problem (navigation truncation, CSP warning).
    Warn,
    /// Failure (navigation error, blocked resource).
    Error,
}

impl LogLevel {
    /// Short tag drawn before the message.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Info => "INF",
            Self::Warn => "WRN",
            Self::Error => "ERR",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Info => MUTED,
            Self::Warn => WARN,
            Self::Error => ERROR,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConsoleEntry {
    level: LogLevel,
    message: String,
}

/// Mutable state of the docked developer tools panel.
///
/// All state is owned by the app layer; nothing here reads the wall clock or
/// any other nondeterministic source, so repeated renders are pixel-identical.
#[derive(Debug, Clone)]
pub struct DevTools {
    /// Whether the panel is currently docked over the page.
    pub open: bool,
    /// Selected tab.
    pub active_tab: DevToolsTab,
    console_scroll: usize,
    network_scroll: usize,
    sources_scroll: usize,
    diagnostics: Vec<ConsoleEntry>,
    requests: Vec<NetworkRequest>,
}

impl Default for DevTools {
    fn default() -> Self {
        Self::new()
    }
}

impl DevTools {
    /// Creates a closed panel showing the Console tab.
    pub fn new() -> Self {
        Self {
            open: false,
            active_tab: DevToolsTab::Console,
            console_scroll: 0,
            network_scroll: 0,
            sources_scroll: 0,
            diagnostics: Vec::new(),
            requests: Vec::new(),
        }
    }

    /// Shows or hides the panel.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Hides the panel (used by Escape).
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Selects a tab.
    pub fn set_tab(&mut self, tab: DevToolsTab) {
        self.active_tab = tab;
    }

    /// Currently selected tab.
    pub fn tab(&self) -> DevToolsTab {
        self.active_tab
    }

    /// Whether the panel is docked.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Records one request in the Network log.
    pub fn record_request(
        &mut self,
        method: &str,
        url: &str,
        status: RequestStatus,
        duration_ms: u32,
        resource_type: ResourceType,
    ) {
        self.requests.push(NetworkRequest {
            method: method.to_string(),
            url: url.to_string(),
            status,
            duration_ms,
            resource_type,
        });
        if self.requests.len() > MAX_REQUESTS {
            let overflow = self.requests.len() - MAX_REQUESTS;
            self.requests.drain(..overflow);
        }
    }

    /// Records a top-level navigation (`GET`, document resource).
    pub fn record_navigation(&mut self, url: &str, status: RequestStatus, duration_ms: u32) {
        self.record_request("GET", url, status, duration_ms, ResourceType::Document);
    }

    /// Adds an engine diagnostic to the Console tab.
    pub fn push_diagnostic(&mut self, level: LogLevel, message: impl Into<String>) {
        self.diagnostics.push(ConsoleEntry {
            level,
            message: message.into(),
        });
        if self.diagnostics.len() > MAX_DIAGNOSTICS {
            let overflow = self.diagnostics.len() - MAX_DIAGNOSTICS;
            self.diagnostics.drain(..overflow);
        }
    }

    /// Recorded requests, oldest first.
    pub fn requests(&self) -> &[NetworkRequest] {
        &self.requests
    }

    /// Number of recorded requests.
    pub fn request_count(&self) -> usize {
        self.requests.len()
    }

    /// Number of app-layer diagnostics.
    pub fn diagnostic_count(&self) -> usize {
        self.diagnostics.len()
    }

    /// Rectangle of the bottom-docked panel for a window of `window` size.
    pub fn panel_rect(&self, window: Rect) -> Rect {
        let height = window.height * PANEL_FRACTION;
        Rect::new(window.x, window.bottom() - height, window.width, height)
    }

    /// True when `(x, y)` falls inside the docked panel.
    pub fn contains(&self, window: Rect, x: f32, y: f32) -> bool {
        self.open && self.panel_rect(window).contains(x, y)
    }

    /// Tab under the point, when the point is in the panel's tab strip.
    pub fn tab_at(&self, rect: Rect, x: f32, y: f32) -> Option<DevToolsTab> {
        if !rect.contains(x, y) || y >= rect.y + TAB_BAR_HEIGHT {
            return None;
        }
        let tab_width = rect.width / DevToolsTab::ALL.len() as f32;
        if tab_width <= 0.0 {
            return None;
        }
        let index = ((x - rect.x) / tab_width) as usize;
        DevToolsTab::ALL.get(index).copied()
    }

    /// Handles a click inside `rect`; returns true when the click was consumed.
    ///
    /// The caller should invoke this before page hit-testing so clicks in the
    /// panel never reach the document.
    pub fn click(&mut self, rect: Rect, x: f32, y: f32) -> bool {
        if !rect.contains(x, y) {
            return false;
        }
        if let Some(tab) = self.tab_at(rect, x, y) {
            self.active_tab = tab;
        }
        true
    }

    /// Scrolls the active tab by a pixel delta (positive scrolls down).
    pub fn scroll(&mut self, delta_y: f32) {
        if delta_y.is_finite() {
            self.scroll_lines((delta_y / ROW_HEIGHT).round() as i32);
        }
    }

    /// Scrolls the active tab by whole rows.
    pub fn scroll_lines(&mut self, lines: i32) {
        let offset = match self.active_tab {
            DevToolsTab::Console => &mut self.console_scroll,
            DevToolsTab::Network => &mut self.network_scroll,
            DevToolsTab::Sources => &mut self.sources_scroll,
        };
        let next = *offset as i64 + i64::from(lines);
        *offset = next.clamp(0, MAX_SCROLL_LINES as i64) as usize;
    }

    /// Draws the panel into `image` at device scale `scale`.
    ///
    /// `rect` is in logical pixels (the same space as the window chrome); the
    /// panel is fully opaque and is painted after the page and chrome.
    pub fn render(&self, image: &mut ImageBuffer, browser: &Browser, rect: Rect, scale: f32) {
        if rect.is_empty() {
            return;
        }
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let atlas = GlyphAtlas::new();

        fill_rect(image, rect, scale, BG);
        fill_rect(
            image,
            Rect::new(rect.x, rect.y, rect.width, 1.0),
            scale,
            BORDER,
        );

        let tab_bar = Rect::new(rect.x, rect.y, rect.width, TAB_BAR_HEIGHT);
        fill_rect(image, tab_bar, scale, BG_ALT);
        fill_rect(
            image,
            Rect::new(rect.x, tab_bar.bottom() - 1.0, rect.width, 1.0),
            scale,
            BORDER,
        );

        let tab_width = rect.width / DevToolsTab::ALL.len() as f32;
        for (index, tab) in DevToolsTab::ALL.iter().enumerate() {
            let tab_rect = Rect::new(
                rect.x + tab_width * index as f32,
                rect.y,
                tab_width,
                TAB_BAR_HEIGHT,
            );
            if *tab == self.active_tab {
                fill_rect(image, tab_rect, scale, TAB_ACTIVE);
                fill_rect(
                    image,
                    Rect::new(tab_rect.x, tab_rect.bottom() - 2.0, tab_rect.width, 2.0),
                    scale,
                    ACCENT,
                );
            }
            let ink = if *tab == self.active_tab { INK } else { MUTED };
            draw_centered(
                image,
                &atlas,
                tab_rect.x,
                tab_rect.y,
                tab_rect.height,
                tab.label(),
                FONT_SIZE,
                ink,
                Some(tab_width - 4.0),
                scale,
            );
        }

        let content = Rect::new(
            rect.x,
            tab_bar.bottom(),
            rect.width,
            (rect.bottom() - tab_bar.bottom()).max(0.0),
        );
        if content.height <= HEADER_HEIGHT {
            return;
        }
        match self.active_tab {
            DevToolsTab::Console => self.render_console(image, &atlas, content, browser, scale),
            DevToolsTab::Network => self.render_network(image, &atlas, content, scale),
            DevToolsTab::Sources => self.render_sources(image, &atlas, content, browser, scale),
        }
    }

    fn render_console(
        &self,
        image: &mut ImageBuffer,
        atlas: &GlyphAtlas,
        content: Rect,
        browser: &Browser,
        scale: f32,
    ) {
        let entries = self.console_entries(browser);
        draw_header(
            image,
            atlas,
            content,
            &format!("Console  —  {} messages", entries.len()),
            scale,
        );

        let visible = rows_in(content);
        let max_scroll = entries.len().saturating_sub(visible);
        let start = self.console_scroll.min(max_scroll);
        for (row, entry) in entries.iter().skip(start).take(visible).enumerate() {
            let y = content.y + HEADER_HEIGHT + ROW_HEIGHT * row as f32;
            if row % 2 == 1 {
                fill_rect(
                    image,
                    Rect::new(content.x, y, content.width, ROW_HEIGHT),
                    scale,
                    BG_ALT,
                );
            }
            let baseline = y + 13.0;
            let time_x = content.x + PAD_X;
            let level_x = time_x + 64.0;
            let message_x = level_x + 34.0;
            draw_text(
                image,
                atlas,
                time_x,
                baseline,
                &timestamp(start + row),
                FONT_SIZE,
                MUTED,
                Some(60.0),
                scale,
            );
            draw_text(
                image,
                atlas,
                level_x,
                baseline,
                entry.level.tag(),
                FONT_SIZE,
                entry.level.color(),
                Some(30.0),
                scale,
            );
            draw_text(
                image,
                atlas,
                message_x,
                baseline,
                &entry.message,
                FONT_SIZE,
                INK,
                Some((content.right() - PAD_X - message_x).max(0.0)),
                scale,
            );
        }
    }

    fn render_network(
        &self,
        image: &mut ImageBuffer,
        atlas: &GlyphAtlas,
        content: Rect,
        scale: f32,
    ) {
        draw_header(
            image,
            atlas,
            content,
            &format!("Network  —  {} requests", self.requests.len()),
            scale,
        );
        let columns = self.network_columns(content);
        let column_header_y = content.y + HEADER_HEIGHT;
        for (label, x, width) in [
            ("METHOD", columns.method, 52.0),
            ("TYPE", columns.resource, 52.0),
            ("URL", columns.url, columns.url_width),
            ("STATUS", columns.status, 52.0),
            ("TIME", columns.time, 48.0),
        ] {
            draw_text(
                image,
                atlas,
                x,
                column_header_y + 13.0,
                label,
                FONT_SIZE,
                MUTED,
                Some(width),
                scale,
            );
        }
        let first_row = content.y + HEADER_HEIGHT + ROW_HEIGHT;

        let visible = rows_in(content).saturating_sub(1);
        let max_scroll = self.requests.len().saturating_sub(visible);
        let start = self.network_scroll.min(max_scroll);
        if self.requests.is_empty() {
            draw_text(
                image,
                atlas,
                content.x + PAD_X,
                first_row + 13.0,
                "No requests recorded yet.",
                FONT_SIZE,
                MUTED,
                Some(content.width - 2.0 * PAD_X),
                scale,
            );
            return;
        }
        for (row, request) in self.requests.iter().skip(start).take(visible).enumerate() {
            let y = first_row + ROW_HEIGHT * row as f32;
            if row % 2 == 1 {
                fill_rect(
                    image,
                    Rect::new(content.x, y, content.width, ROW_HEIGHT),
                    scale,
                    BG_ALT,
                );
            }
            let baseline = y + 13.0;
            let status_color = match request.status {
                RequestStatus::Loading => WARN,
                RequestStatus::Ready => OK,
                RequestStatus::Error => ERROR,
            };
            for (text, x, width, color) in [
                (request.method.as_str(), columns.method, 52.0, INK),
                (request.resource_type.label(), columns.resource, 52.0, MUTED),
                (request.url.as_str(), columns.url, columns.url_width, INK),
                (request.status.label(), columns.status, 52.0, status_color),
            ] {
                draw_text(
                    image,
                    atlas,
                    x,
                    baseline,
                    text,
                    FONT_SIZE,
                    color,
                    Some(width),
                    scale,
                );
            }
            draw_text(
                image,
                atlas,
                columns.time,
                baseline,
                &format!("{}ms", request.duration_ms),
                FONT_SIZE,
                MUTED,
                Some(48.0),
                scale,
            );
        }
    }

    fn render_sources(
        &self,
        image: &mut ImageBuffer,
        atlas: &GlyphAtlas,
        content: Rect,
        browser: &Browser,
        scale: f32,
    ) {
        let document = browser.page().document.borrow();
        let source = browser_dom::serialize(&document, document.root());
        let url = browser.page().url.as_ref().map_or_else(
            || "(no document)".to_string(),
            |url| url.as_str().to_string(),
        );
        let mut blocks = Vec::new();
        for tag in ["script", "style"] {
            for id in document.get_elements_by_tag_name(tag) {
                let chars = document.text_content(id).chars().count();
                blocks.push(format!("<{tag}> inline — {chars} chars"));
            }
        }
        drop(document);

        draw_header(image, atlas, content, &format!("Sources  —  {url}"), scale);
        let mut y = content.y + HEADER_HEIGHT;
        for block in blocks.iter().take(MAX_SOURCE_BLOCKS) {
            draw_text(
                image,
                atlas,
                content.x + PAD_X,
                y + 13.0,
                &format!("• {block}"),
                FONT_SIZE,
                ACCENT,
                Some(content.width - 2.0 * PAD_X),
                scale,
            );
            y += ROW_HEIGHT;
        }
        if blocks.len() > MAX_SOURCE_BLOCKS {
            draw_text(
                image,
                atlas,
                content.x + PAD_X,
                y + 13.0,
                &format!("… +{} more blocks", blocks.len() - MAX_SOURCE_BLOCKS),
                FONT_SIZE,
                MUTED,
                Some(content.width - 2.0 * PAD_X),
                scale,
            );
            y += ROW_HEIGHT;
        }
        // Separator between the block list and the code view.
        fill_rect(
            image,
            Rect::new(content.x, y, content.width, 1.0),
            scale,
            BORDER,
        );
        y += 1.0;

        let lines: Vec<&str> = source.lines().collect();
        let visible = ((content.bottom() - y) / ROW_HEIGHT) as usize;
        let max_scroll = lines.len().saturating_sub(visible);
        let start = self.sources_scroll.min(max_scroll);
        let gutter = 44.0;
        for (row, line) in lines.iter().skip(start).take(visible).enumerate() {
            let row_y = y + ROW_HEIGHT * row as f32;
            if row % 2 == 1 {
                fill_rect(
                    image,
                    Rect::new(content.x, row_y, content.width, ROW_HEIGHT),
                    scale,
                    BG_ALT,
                );
            }
            draw_text(
                image,
                atlas,
                content.x + PAD_X,
                row_y + 13.0,
                &format!("{:>4}", start + row + 1),
                FONT_SIZE,
                MUTED,
                Some(gutter - 2.0 * PAD_X),
                scale,
            );
            draw_text(
                image,
                atlas,
                content.x + gutter,
                row_y + 13.0,
                line,
                FONT_SIZE,
                INK,
                Some((content.right() - PAD_X - content.x - gutter).max(0.0)),
                scale,
            );
        }
    }

    /// Console messages followed by app-layer diagnostics.
    fn console_entries(&self, browser: &Browser) -> Vec<ConsoleEntry> {
        let mut entries: Vec<ConsoleEntry> = browser
            .console()
            .iter()
            .map(|message| ConsoleEntry {
                level: classify_console(message),
                message: message.clone(),
            })
            .collect();
        entries.extend(self.diagnostics.iter().cloned());
        entries
    }

    fn network_columns(&self, content: Rect) -> NetworkColumns {
        let method = content.x + PAD_X;
        let resource = method + 52.0;
        let url = resource + 52.0;
        let time = content.right() - PAD_X - 48.0;
        let status = time - 8.0 - 52.0;
        NetworkColumns {
            method,
            resource,
            url,
            url_width: (status - 8.0 - url).max(0.0),
            status,
            time,
        }
    }
}

struct NetworkColumns {
    method: f32,
    resource: f32,
    url: f32,
    url_width: f32,
    status: f32,
    time: f32,
}

fn classify_console(message: &str) -> LogLevel {
    if message.starts_with("[csp]") || message.starts_with("[image]") {
        LogLevel::Error
    } else if message.starts_with("[nav]") {
        LogLevel::Warn
    } else {
        LogLevel::Info
    }
}

/// Deterministic clock for console rows (no wall-clock reads).
fn timestamp(index: usize) -> String {
    let total_ms = index * 37;
    format!(
        "{:02}:{:02}:{:03}",
        (total_ms / 60_000) % 60,
        (total_ms / 1_000) % 60,
        total_ms % 1_000
    )
}

fn rows_in(content: Rect) -> usize {
    ((content.height - HEADER_HEIGHT) / ROW_HEIGHT).max(0.0) as usize
}

fn draw_header(
    image: &mut ImageBuffer,
    atlas: &GlyphAtlas,
    content: Rect,
    label: &str,
    scale: f32,
) {
    fill_rect(
        image,
        Rect::new(content.x, content.y, content.width, HEADER_HEIGHT),
        scale,
        BG_ALT,
    );
    draw_text(
        image,
        atlas,
        content.x + PAD_X,
        content.y + 15.0,
        label,
        FONT_SIZE,
        INK,
        Some(content.width - 2.0 * PAD_X),
        scale,
    );
}

fn fill_rect(image: &mut ImageBuffer, rect: Rect, scale: f32, color: Color) {
    let x0 = (rect.x * scale).round() as i32;
    let y0 = (rect.y * scale).round() as i32;
    let x1 = ((rect.x + rect.width) * scale).round() as i32;
    let y1 = ((rect.y + rect.height) * scale).round() as i32;
    let width = image.width as i32;
    let height = image.height as i32;
    for y in y0.max(0)..y1.min(height) {
        for x in x0.max(0)..x1.min(width) {
            let index = (y as usize * image.width as usize + x as usize) * 4;
            image.pixels[index] = color.r;
            image.pixels[index + 1] = color.g;
            image.pixels[index + 2] = color.b;
            image.pixels[index + 3] = 255;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    image: &mut ImageBuffer,
    atlas: &GlyphAtlas,
    x: f32,
    baseline: f32,
    text: &str,
    size: f32,
    color: Color,
    max_width: Option<f32>,
    scale: f32,
) {
    atlas.paint_text(
        image,
        x * scale,
        baseline * scale,
        text,
        size * scale,
        color,
        max_width.map(|width| width * scale),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_centered(
    image: &mut ImageBuffer,
    atlas: &GlyphAtlas,
    x: f32,
    rect_y: f32,
    rect_height: f32,
    text: &str,
    size: f32,
    color: Color,
    max_width: Option<f32>,
    scale: f32,
) {
    atlas.paint_text_centered(
        image,
        x * scale,
        rect_y * scale,
        rect_height * scale,
        text,
        size * scale,
        color,
        max_width.map(|width| width * scale),
    );
}

#[cfg(test)]
mod tests {
    use browser_core::{Browser, Settings};

    use super::*;

    fn devtools() -> DevTools {
        DevTools::new()
    }

    fn browser() -> Browser {
        let mut browser = Browser::new(Settings::headless());
        browser
            .load_html(
                "<html><head><style>p{color:red}</style></head><body>\
                 <script>console.log('hi')</script><p>Hello</p></body></html>",
                Some("https://example.test/"),
            )
            .expect("fixture HTML loads");
        browser
    }

    #[test]
    fn toggling_and_tab_switching() {
        let mut devtools = devtools();
        assert!(!devtools.is_open());
        assert_eq!(devtools.tab(), DevToolsTab::Console);

        devtools.toggle();
        assert!(devtools.is_open());

        let panel = Rect::new(0.0, 60.0, 240.0, 40.0);
        assert!(devtools.click(panel, 100.0, 70.0));
        assert_eq!(devtools.tab(), DevToolsTab::Network);
        assert!(devtools.click(panel, 200.0, 70.0));
        assert_eq!(devtools.tab(), DevToolsTab::Sources);
        assert_eq!(devtools.tab_at(panel, 20.0, 96.0), None);
        assert!(!devtools.click(panel, 20.0, 50.0));

        devtools.set_tab(DevToolsTab::Console);
        assert_eq!(devtools.tab(), DevToolsTab::Console);
        devtools.close();
        assert!(!devtools.is_open());
    }

    #[test]
    fn recording_requests_and_diagnostics() {
        let mut devtools = devtools();
        devtools.record_request(
            "GET",
            "https://example.test/a",
            RequestStatus::Loading,
            7,
            ResourceType::Document,
        );
        devtools.record_navigation("https://example.test/b", RequestStatus::Ready, 42);
        devtools.push_diagnostic(LogLevel::Error, "navigation failed");

        assert_eq!(devtools.request_count(), 2);
        assert_eq!(devtools.diagnostic_count(), 1);
        assert_eq!(devtools.requests()[0].method, "GET");
        assert_eq!(devtools.requests()[0].resource_type, ResourceType::Document);
        assert_eq!(devtools.requests()[1].url, "https://example.test/b");
        assert_eq!(devtools.requests()[1].status, RequestStatus::Ready);
        assert_eq!(devtools.requests()[1].duration_ms, 42);
    }

    #[test]
    fn scroll_offsets_are_per_tab_and_clamped() {
        let mut devtools = devtools();
        devtools.scroll_lines(5);
        devtools.scroll_lines(-100);
        assert_eq!(devtools.console_scroll, 0);
        devtools.set_tab(DevToolsTab::Network);
        devtools.scroll(40.0);
        assert_eq!(devtools.network_scroll, 2);
        assert_eq!(devtools.console_scroll, 0);
    }

    #[test]
    fn rendering_is_deterministic_for_all_tabs() {
        let browser = browser();
        let panel = Rect::new(0.0, 60.0, 320.0, 40.0);
        let background = Color::rgb(255, 255, 255);
        for tab in DevToolsTab::ALL {
            let mut devtools = devtools();
            devtools.open = true;
            devtools.set_tab(tab);
            devtools.record_navigation("https://example.test/", RequestStatus::Ready, 11);
            let mut first = ImageBuffer::filled(320, 100, background);
            let mut second = ImageBuffer::filled(320, 100, background);
            devtools.render(&mut first, &browser, panel, 1.0);
            devtools.render(&mut second, &browser, panel, 1.0);
            assert_eq!(
                first.pixels, second.pixels,
                "{tab:?} must render identically"
            );
            assert!(first.coverage_vs(background) > 0.0);
        }
    }
}
