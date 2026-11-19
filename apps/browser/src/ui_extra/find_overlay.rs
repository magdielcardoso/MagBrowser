//! Maps [`FindBar`] byte matches onto laid-out page text fragments.
//!
//! The page's `text()` joins every text fragment with a single space. By
//! rebuilding that same concatenation while remembering each fragment's byte
//! span and layout rectangle, a match from [`crate::ui_extra::FindBar`] can be
//! turned into one or more approximate highlight rectangles in page-local CSS
//! pixels. The rectangles are intentionally approximate: the engine exposes
//! fragment boxes, not per-glyph boxes, so a match spanning a line break or a
//! fragment boundary is split into proportionally sized pieces.

// Used by the native window; kept warning-free in the headless build too.
#![allow(dead_code)]

use browser_core::Page;
use browser_text::Font;

use super::find::{FindBar, find_all};

/// One approximate highlight rectangle in page-local CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FindHighlight {
    /// Left edge in page-local CSS pixels.
    pub x: f32,
    /// Top edge in page-local CSS pixels.
    pub y: f32,
    /// Width in page-local CSS pixels.
    pub width: f32,
    /// Height in page-local CSS pixels.
    pub height: f32,
    /// True for the match the find bar currently points at.
    pub current: bool,
}

impl FindHighlight {
    /// Creates a highlight rectangle.
    pub fn new(x: f32, y: f32, width: f32, height: f32, current: bool) -> Self {
        Self {
            x,
            y,
            width,
            height,
            current,
        }
    }

    /// True when the rectangle has no visible area.
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// Byte span and layout geometry of one text fragment.
struct FragmentSpan {
    start: usize,
    end: usize,
    x: f32,
    baseline: f32,
    width: f32,
    font_size: f32,
}

/// Computes highlight rectangles for the current find query.
///
/// An empty query, an empty page or a page whose text no longer matches the
/// find bar's query produces no rectangles.
pub fn highlights(page: &Page, find: &FindBar) -> Vec<FindHighlight> {
    if find.query().is_empty() {
        return Vec::new();
    }

    // Rebuild `Page::text()` exactly: non-empty fragments are joined by a space.
    let mut text = String::new();
    let mut spans: Vec<FragmentSpan> = Vec::new();
    for run in page.layout.text_runs() {
        let fragment = run.fragment;
        if fragment.text.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        let start = text.len();
        text.push_str(&fragment.text);
        spans.push(FragmentSpan {
            start,
            end: text.len(),
            x: fragment.x,
            baseline: fragment.baseline,
            width: fragment.width,
            font_size: fragment.font_size,
        });
    }

    let matches = find_all(&text, find.query(), find.is_case_sensitive());
    let current = find.current_index();
    let font = Font::sans();
    let mut out = Vec::new();

    for (index, range) in matches.iter().enumerate() {
        let is_current = Some(index) == current;
        for span in &spans {
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);
            if end <= start || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                continue;
            }
            let before = font.measure(&text[span.start..start], span.font_size);
            let matched = font.measure(&text[start..end], span.font_size);
            let width = matched.min((span.width - before).max(0.0));
            if width <= 0.0 {
                continue;
            }
            let height = span.font_size.max(1.0) * 1.25;
            out.push(FindHighlight::new(
                span.x + before,
                span.baseline - span.font_size,
                width,
                height,
                is_current,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::Settings;

    fn browser() -> browser_core::Browser {
        let mut browser = browser_core::Browser::new(Settings::headless());
        browser
            .load_html(
                "<html><body style='margin:0'><p>alpha beta alpha</p></body></html>",
                None,
            )
            .unwrap();
        browser
    }

    #[test]
    fn query_maps_to_laid_out_fragments() {
        let browser = browser();
        let mut find = FindBar::new();
        find.set_query("alpha", &browser.page().text());
        assert_eq!(find.match_count(), 2);
        let rects = highlights(browser.page(), &find);
        assert_eq!(rects.len(), 2, "one highlight per match");
        assert!(rects.iter().any(|rect| rect.current));
        for rect in &rects {
            assert!(!rect.is_empty());
            assert!(rect.x >= 0.0);
            assert!(rect.y >= 0.0);
            assert!(rect.width > 0.0 && rect.width < 200.0);
        }
        // The two matches sit at different x positions in the same text run.
        assert!((rects[0].x - rects[1].x).abs() > 1.0);
    }

    #[test]
    fn empty_query_has_no_highlights() {
        let browser = browser();
        let find = FindBar::new();
        assert!(highlights(browser.page(), &find).is_empty());
    }

    #[test]
    fn stale_page_text_has_no_highlights() {
        let browser = browser();
        let mut find = FindBar::new();
        find.set_query("missing", &browser.page().text());
        assert!(highlights(browser.page(), &find).is_empty());
    }
}
