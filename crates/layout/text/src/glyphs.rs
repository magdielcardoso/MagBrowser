//! Font metrics and glyph access for the embedded Inter face.
//!
//! The engine ships one open-licensed typeface (SIL OFL 1.1). It is a real
//! outline font with proportional advances, so text no longer has the chunky
//! 8x8 terminal look: every glyph is rasterised with anti-aliasing at the
//! requested size by [`crate::raster`].
//!
//! [`Font::monospace`] is kept as an alias for callers that used the old
//! built-in face; it now returns this proportional face.

use std::sync::OnceLock;

use crate::truetype::{Face, OutlineGlyph, OutlinePoint};

/// A resolved outline glyph together with the character it renders.
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    /// The character this glyph renders.
    pub code: char,
    /// Horizontal advance in font units.
    pub advance: u16,
    /// Closed contours in font units (y up).
    pub contours: Vec<Vec<OutlinePoint>>,
}

impl Glyph {
    /// Bounding box of the outline in font units.
    pub fn bounds(&self) -> Option<(i16, i16, i16, i16)> {
        OutlineGlyph {
            advance: self.advance,
            contours: self.contours.clone(),
        }
        .bounds()
    }
}

/// The engine's embedded proportional typeface.
#[derive(Debug)]
pub struct Font {
    face: &'static Face,
}

impl Font {
    /// Returns the shared embedded face.
    pub fn sans() -> &'static Font {
        static FONT: OnceLock<Font> = OnceLock::new();
        FONT.get_or_init(|| Font {
            face: Face::embedded(),
        })
    }

    /// Compatibility alias for [`Self::sans`].
    pub fn monospace() -> &'static Font {
        Self::sans()
    }

    /// Parsed TrueType face backing this font.
    pub fn face(&self) -> &'static Face {
        self.face
    }

    /// Design units per em.
    pub fn units_per_em(&self) -> f32 {
        f32::from(self.face.units_per_em)
    }

    /// Nominal advance of an `M` in font units (kept for legacy callers).
    pub fn cell_width(&self) -> f32 {
        f32::from(self.face.outline('M').advance)
    }

    /// Em size in font units (kept for legacy callers).
    pub fn cell_height(&self) -> f32 {
        f32::from(self.face.units_per_em)
    }

    /// Looks up the outline for `c`.
    ///
    /// Control characters have no drawable glyph. Every other character maps
    /// to a real outline; unsupported code points use the font's `.notdef`.
    pub fn glyph(&self, c: char) -> Option<Glyph> {
        if c.is_control() {
            return None;
        }
        let outline = self.face.outline(c);
        Some(Glyph {
            code: c,
            advance: outline.advance,
            contours: outline.contours,
        })
    }

    /// Horizontal advance of `c` at pixel size `px`.
    pub fn advance(&self, c: char, px: f32) -> f32 {
        if px <= 0.0 {
            return 0.0;
        }
        if c == '\t' {
            return px;
        }
        if c.is_control() {
            return 0.0;
        }
        f32::from(self.face.advance(self.face.glyph_index(c))) * px / self.units_per_em()
    }

    /// Line height in pixels (ascender + descender + line gap).
    pub fn line_height(&self, px: f32) -> f32 {
        if px <= 0.0 {
            return 0.0;
        }
        let units = f32::from(self.face.ascender - self.face.descender + self.face.line_gap);
        units.max(0.0) * px / self.units_per_em()
    }

    /// Distance from the baseline to the top of the line box, in pixels.
    pub fn ascent(&self, px: f32) -> f32 {
        if px <= 0.0 {
            return 0.0;
        }
        f32::from(self.face.ascender.max(0)) * px / self.units_per_em()
    }

    /// Distance from the baseline to the bottom of the line box, in pixels.
    pub fn descent(&self, px: f32) -> f32 {
        if px <= 0.0 {
            return 0.0;
        }
        f32::from((-self.face.descender).max(0)) * px / self.units_per_em()
    }

    /// Sum of advances for every character in `text`, in pixels.
    pub fn measure(&self, text: &str, px: f32) -> f32 {
        text.chars().map(|c| self.advance(c, px)).sum()
    }

    /// Sum of advances for `text[start..end]` in pixels.
    ///
    /// Byte offsets are clamped to the nearest UTF-8 boundary so the function
    /// never panics on malformed ranges.
    pub fn measure_range(&self, text: &str, px: f32, start: usize, end: usize) -> f32 {
        let start = clamp_boundary(text, start);
        let end = clamp_boundary(text, end);
        if end <= start {
            return 0.0;
        }
        self.measure(&text[start..end], px)
    }
}

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
    fn font_is_shared_and_aliased() {
        assert!(std::ptr::eq(Font::sans(), Font::sans()));
        assert!(std::ptr::eq(Font::monospace(), Font::sans()));
    }

    #[test]
    fn metrics_follow_the_embedded_face() {
        let font = font();
        assert_eq!(font.cell_height(), font.face().units_per_em as f32);
        assert!((font.ascent(16.0) / 16.0 - 0.96875).abs() < 0.01);
        assert!((font.line_height(16.0) - font.ascent(16.0) - font.descent(16.0)).abs() < 1e-4);
    }

    #[test]
    fn advances_are_proportional() {
        let font = font();
        assert!(font.advance('i', 16.0) < font.advance('M', 16.0));
        assert!(font.measure("iii", 16.0) < font.measure("MMM", 16.0));
    }

    #[test]
    fn non_ascii_has_real_outlines() {
        for ch in ['ç', 'ã', 'ó', 'é', '—', '…'] {
            let glyph = font().glyph(ch).unwrap();
            assert!(!glyph.contours.is_empty(), "missing outline for {ch:?}");
        }
    }

    #[test]
    fn control_characters_have_no_glyph() {
        assert!(font().glyph('\n').is_none());
        assert!(font().glyph('\0').is_none());
        assert_eq!(font().advance('\n', 16.0), 0.0);
    }

    #[test]
    fn tab_advances_one_em() {
        assert_eq!(font().advance('\t', 16.0), 16.0);
        assert_eq!(font().advance('\t', 32.0), 32.0);
    }

    #[test]
    fn measure_scales_and_clamps() {
        let font = font();
        let wide = font.measure("abc", 32.0);
        let narrow = font.measure("abc", 16.0);
        assert!((wide - 2.0 * narrow).abs() < 0.01);
        assert_eq!(font.measure("abc", 0.0), 0.0);
        assert_eq!(font.measure("", 16.0), 0.0);
    }

    #[test]
    fn measure_range_uses_byte_offsets() {
        let text = "héllo";
        assert!(font().measure_range(text, 16.0, 0, 2) > 0.0);
        // Byte 1 is a character boundary (start of `é`); byte 3 is the start of
        // the following `l`, so both ranges cover different characters.
        assert_eq!(
            font().measure_range(text, 16.0, 1, 3),
            font().measure("é", 16.0)
        );
        assert_eq!(font().measure_range(text, 16.0, 99, 200), 0.0);
        assert_eq!(font().measure_range(text, 16.0, 4, 2), 0.0);
    }

    #[test]
    fn negative_pixel_sizes_are_floored() {
        assert_eq!(font().line_height(-4.0), 0.0);
        assert_eq!(font().ascent(-4.0), 0.0);
        assert_eq!(font().descent(-4.0), 0.0);
        assert_eq!(font().advance('a', -4.0), 0.0);
    }
}
