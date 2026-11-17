//! Glyph access and font metrics for the built-in bitmap face.

use std::sync::OnceLock;

use crate::fonts;

/// A single rasterised glyph from the built-in monospace face.
///
/// `rows` holds one byte per scanline, top row first. Bit 7 of each byte is the
/// leftmost pixel and only the low `width` bits are meaningful (the face is
/// 8 pixels wide, so all eight bits are used). `advance` is the horizontal
/// advance in font units at the design size (16 pixels), i.e. 8.0 for every
/// covered character.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    /// The character this glyph renders. For the fallback box this is the
    /// requested character, not `U+FFFD`.
    pub code: char,
    /// Bitmap width in pixels.
    pub width: u8,
    /// Bitmap height in pixels.
    pub height: u8,
    /// Horizontal advance in font units at the design size.
    pub advance: f32,
    /// Bitmap rows, one byte per row, top row first, bit 7 leftmost.
    pub rows: &'static [u8],
}

/// The engine's built-in monospace font.
///
/// This is the only font the engine currently has: an 8x16 bitmap cell with an
/// 8x8 printable-ASCII glyph set (see the `fonts` module). Instances are
/// obtained from [`Font::monospace`]; the type deliberately has no constructor
/// that could describe a font the renderer cannot draw.
#[derive(Debug)]
pub struct Font {
    cell_width: f32,
    cell_height: f32,
    ascent_ratio: f32,
}

impl Font {
    /// Returns the shared monospace face.
    ///
    /// The font is initialised once, lazily, and is identical for every caller.
    pub fn monospace() -> &'static Font {
        static FONT: OnceLock<Font> = OnceLock::new();
        FONT.get_or_init(|| Font {
            cell_width: fonts::CELL_WIDTH,
            cell_height: fonts::CELL_HEIGHT,
            ascent_ratio: 0.75,
        })
    }

    /// Width of one character cell in font units (8.0).
    pub fn cell_width(&self) -> f32 {
        self.cell_width
    }

    /// Height of one character cell in font units (16.0).
    pub fn cell_height(&self) -> f32 {
        self.cell_height
    }

    /// Looks up the glyph for `c`.
    ///
    /// Returns `None` for control characters (including `\n` and `\t`, which are
    /// handled by layout rather than rasterised). Every other character is
    /// drawable: printable ASCII has its own bitmap, and any other (non-ASCII)
    /// code point falls back to the hollow box glyph in the `fonts` module,
    /// whose `code` is the requested character.
    pub fn glyph(&self, c: char) -> Option<Glyph> {
        if let Some(rows) = fonts::ascii_rows(c) {
            return Some(Glyph {
                code: c,
                width: fonts::GLYPH_WIDTH,
                height: fonts::GLYPH_HEIGHT,
                advance: self.cell_width,
                rows,
            });
        }
        if c.is_control() {
            return None;
        }
        Some(Glyph {
            code: c,
            width: fonts::GLYPH_WIDTH,
            height: fonts::GLYPH_HEIGHT,
            advance: self.cell_width,
            rows: fonts::FALLBACK_GLYPH.as_slice(),
        })
    }

    /// Horizontal advance of `c` at pixel size `px`.
    ///
    /// The face is monospace, so this is the scaled cell width for every
    /// drawable character and 0.0 for control characters. A tab advances one
    /// cell so that measurement is stable (layout still owns real tab stops).
    pub fn advance(&self, c: char, px: f32) -> f32 {
        if c == '\t' {
            return self.scaled_cell_width(px);
        }
        if self.glyph(c).is_some() {
            self.scaled_cell_width(px)
        } else {
            0.0
        }
    }

    /// Line height in pixels at `px`: the full em cell (16 pixels at 16px).
    pub fn line_height(&self, px: f32) -> f32 {
        px.max(0.0)
    }

    /// Distance from the baseline to the top of the line box in pixels.
    ///
    /// Always `0.75 * px`, so `ascent + descent == line_height`.
    pub fn ascent(&self, px: f32) -> f32 {
        px.max(0.0) * self.ascent_ratio
    }

    /// Distance from the baseline to the bottom of the line box in pixels.
    ///
    /// Always `0.25 * px`, so `ascent + descent == line_height`.
    pub fn descent(&self, px: f32) -> f32 {
        px.max(0.0) * (1.0 - self.ascent_ratio)
    }

    /// Advances the whole string, in pixels.
    pub fn measure(&self, text: &str, px: f32) -> f32 {
        text.chars().map(|c| self.advance(c, px)).sum()
    }

    /// Advances `text[start..end]` in pixels.
    ///
    /// Indices are byte offsets. Out-of-range or non-boundary indices are
    /// clamped to the nearest character boundary so the function never panics.
    pub fn measure_range(&self, text: &str, px: f32, start: usize, end: usize) -> f32 {
        let start = clamp_boundary(text, start);
        let end = clamp_boundary(text, end);
        if end <= start {
            return 0.0;
        }
        text[start..end].chars().map(|c| self.advance(c, px)).sum()
    }

    fn scaled_cell_width(&self, px: f32) -> f32 {
        if px <= 0.0 {
            return 0.0;
        }
        self.cell_width * (px / self.cell_height)
    }
}

/// Clamps `index` into `0..=text.len()` and back to a UTF-8 boundary.
fn clamp_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> &'static Font {
        Font::monospace()
    }

    #[test]
    fn font_is_shared() {
        assert!(std::ptr::eq(Font::monospace(), Font::monospace()));
    }

    #[test]
    fn cell_metrics_are_8x16() {
        assert_eq!(font().cell_width(), 8.0);
        assert_eq!(font().cell_height(), 16.0);
        assert_eq!(font().line_height(16.0), 16.0);
    }

    #[test]
    fn glyph_metrics_for_uppercase_a() {
        let glyph = font().glyph('A').expect("A is supported");
        assert_eq!(glyph.code, 'A');
        assert_eq!(glyph.width, 8);
        assert_eq!(glyph.height, 8);
        assert_eq!(glyph.advance, 8.0);
        assert_eq!(glyph.rows.len(), 8);
        // Row 1 of 'A' is `.####...`: 0x78 with bit 7 leftmost.
        assert_eq!(glyph.rows[1], 0x78);
    }

    #[test]
    fn space_has_width_and_no_pixels() {
        let glyph = font().glyph(' ').expect("space is supported");
        assert_eq!(glyph.width, 8);
        assert!(glyph.rows.iter().all(|&row| row == 0));
        assert_eq!(font().advance(' ', 16.0), 8.0);
    }

    #[test]
    fn ascii_is_fully_covered() {
        for code in 0x20u32..=0x7E {
            let c = char::from_u32(code).unwrap();
            let glyph = font().glyph(c).unwrap_or_else(|| panic!("missing {c:?}"));
            assert_eq!(glyph.rows.len(), 8);
        }
    }

    #[test]
    fn control_characters_have_no_glyph() {
        assert!(font().glyph('\n').is_none());
        assert!(font().glyph('\0').is_none());
        assert!(font().glyph('\u{7f}').is_none());
        assert_eq!(font().advance('\n', 16.0), 0.0);
    }

    #[test]
    fn non_ascii_falls_back_to_box_glyph() {
        let glyph = font().glyph('→').expect("fallback glyph");
        assert_eq!(glyph.code, '→');
        assert_eq!(glyph.rows, fonts::FALLBACK_GLYPH.as_slice());
        assert_eq!(glyph.advance, 8.0);
        assert_eq!(font().advance('→', 16.0), 8.0);
    }

    #[test]
    fn tab_advances_one_cell() {
        assert_eq!(font().advance('\t', 16.0), 8.0);
        assert_eq!(font().advance('\t', 32.0), 16.0);
    }

    #[test]
    fn monospace_advance_invariant() {
        for c in "abcXYZ019!?~{}".chars() {
            assert_eq!(font().advance(c, 16.0), 8.0);
        }
    }

    #[test]
    fn measure_counts_characters_not_bytes() {
        assert_eq!(font().measure("", 16.0), 0.0);
        assert_eq!(font().measure("abc", 16.0), 24.0);
        assert_eq!(font().measure("héllo", 16.0), 40.0);
    }

    #[test]
    fn measure_scales_with_pixel_size() {
        assert_eq!(font().measure("abc", 8.0), 12.0);
        assert_eq!(font().measure("abc", 32.0), 48.0);
        assert_eq!(font().measure("abc", 0.0), 0.0);
    }

    #[test]
    fn measure_range_uses_byte_offsets() {
        let text = "héllo";
        // 'é' encodes as two bytes, so bytes 0..2 are one character.
        assert_eq!(font().measure_range(text, 16.0, 0, 2), 8.0);
        assert_eq!(font().measure_range(text, 16.0, 0, text.len()), 40.0);
    }

    #[test]
    fn measure_range_clamps_odd_indices() {
        let text = "héllo";
        // Byte 1 is inside 'é' and snaps back to 0.
        assert_eq!(font().measure_range(text, 16.0, 1, 3), 8.0);
        assert_eq!(font().measure_range(text, 16.0, 99, 200), 0.0);
        assert_eq!(font().measure_range(text, 16.0, 4, 2), 0.0);
    }

    #[test]
    fn ascent_plus_descent_equals_line_height() {
        for px in [8.0, 12.0, 16.0, 20.0, 33.5] {
            let font = font();
            assert!((font.ascent(px) + font.descent(px) - font.line_height(px)).abs() < 1e-6);
        }
        assert_eq!(font().ascent(16.0), 12.0);
        assert_eq!(font().descent(16.0), 4.0);
    }

    #[test]
    fn negative_pixel_sizes_are_floored() {
        assert_eq!(font().line_height(-4.0), 0.0);
        assert_eq!(font().ascent(-4.0), 0.0);
        assert_eq!(font().descent(-4.0), 0.0);
        assert_eq!(font().advance('a', -4.0), 0.0);
    }
}
