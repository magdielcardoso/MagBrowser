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

/// One glyph produced by the shared layout engine, with its source byte range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LaidOutGlyph {
    pub ch: char,
    pub glyph_id: u16,
    /// Byte offset of the first source character in the run's text.
    pub byte_start: usize,
    /// Byte offset just past the last source character (exclusive).
    pub byte_end: usize,
    pub x: f32,
    /// Advance including kerning and letter/word spacing.
    pub advance: f32,
}

/// A cluster of one or more consecutive source characters forming one glyph.
struct Cluster {
    start: usize,
    end: usize,
    glyph_id: u16,
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

    /// Pair kerning to apply to the advance of `a` when it is followed by `b`,
    /// in pixels at size `px`. A negative value tightens the pair (e.g. `AV`).
    ///
    /// Values come from the font's GPOS `kern` data (or a legacy `kern` table).
    pub fn kerning(&self, a: char, b: char, px: f32) -> f32 {
        if px <= 0.0 {
            return 0.0;
        }
        let ga = self.face.glyph_index(a);
        let gb = self.face.glyph_index(b);
        f32::from(self.face.kerning_units(ga, gb)) * px / self.units_per_em()
    }

    /// Glyph to draw for a character sequence, or `None` when no ligature
    /// substitution applies. GSUB ligatures are preferred; the classic
    /// `fi`/`fl`/`ff`/`ffi`/`ffl` forms fall back to their Unicode compatibility
    /// code points, but only when this face resolves them to a distinct glyph
    /// with an outline (this Inter build has no such outlines, so f-ligatures
    /// are intentionally left unligated to match harfbuzz/Blink).
    pub(crate) fn ligature_glyph(&self, chars: &[char]) -> Option<u16> {
        let gids: Vec<u16> = chars.iter().map(|&c| self.face.glyph_index(c)).collect();
        if let Some(gid) = self.face.ligature(&gids) {
            return Some(gid);
        }
        let sequence: String = chars.iter().collect();
        let cp = match sequence.as_str() {
            "ffi" => 0xFB03,
            "ffl" => 0xFB04,
            "ff" => 0xFB00,
            "fi" => 0xFB01,
            "fl" => 0xFB02,
            _ => return None,
        };
        let gid = self.face.glyph_index(char::from_u32(cp)?);
        if gid == 0 || gids.contains(&gid) {
            return None;
        }
        self.face
            .outline_gid(gid)
            .filter(|outline| !outline.is_empty())
            .map(|_| gid)
    }

    /// Lays out `text` into positioned glyphs with byte ranges, applying
    /// ligature substitution, pair kerning and letter/word spacing. This is the
    /// single source of truth shared by [`Self::measure`] and
    /// [`crate::shape`] so their widths always agree.
    pub(crate) fn layout(
        &self,
        text: &str,
        px: f32,
        letter_spacing: f32,
        word_spacing: f32,
    ) -> Vec<LaidOutGlyph> {
        if px <= 0.0 {
            return Vec::new();
        }
        let upem = self.units_per_em();
        let chars: Vec<(usize, char)> = text
            .char_indices()
            .filter(|&(_, ch)| !ch.is_control())
            .collect();

        // Group drawable characters into clusters, substituting the longest
        // available ligature at each position.
        let mut clusters: Vec<Cluster> = Vec::with_capacity(chars.len());
        let mut i = 0;
        while i < chars.len() {
            let max = (chars.len() - i).min(4);
            let mut matched = None;
            for len in (2..=max).rev() {
                let sequence: Vec<char> = chars[i..i + len].iter().map(|&(_, c)| c).collect();
                if let Some(gid) = self.ligature_glyph(&sequence) {
                    matched = Some((len, gid));
                    break;
                }
            }
            if let Some((len, gid)) = matched {
                clusters.push(Cluster {
                    start: i,
                    end: i + len,
                    glyph_id: gid,
                });
                i += len;
            } else {
                clusters.push(Cluster {
                    start: i,
                    end: i + 1,
                    glyph_id: self.face.glyph_index(chars[i].1),
                });
                i += 1;
            }
        }

        let mut x = 0.0;
        let mut positioned = Vec::with_capacity(clusters.len());
        for (index, cluster) in clusters.iter().enumerate() {
            let first = chars[cluster.start];
            let last = chars[cluster.end - 1];
            let base = f32::from(self.face.advance(cluster.glyph_id)) * px / upem;
            let kern = clusters.get(index + 1).map_or(0.0, |next| {
                f32::from(self.face.kerning_units(cluster.glyph_id, next.glyph_id)) * px / upem
            });
            let spacing = letter_spacing + if first.1 == ' ' { word_spacing } else { 0.0 };
            let advance = base + kern + spacing;
            positioned.push(LaidOutGlyph {
                ch: first.1,
                glyph_id: cluster.glyph_id,
                byte_start: first.0,
                byte_end: last.0 + last.1.len_utf8(),
                x,
                advance,
            });
            x += advance;
        }
        positioned
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

    /// Width of `text` in pixels, including pair kerning and ligatures.
    ///
    /// This is the same value as [`crate::shape`]'s `width`, so layout and
    /// painting agree on how wide a run is.
    pub fn measure(&self, text: &str, px: f32) -> f32 {
        self.measure_with_spacing(text, px, 0.0, 0.0)
    }

    /// Width of `text` in pixels with CSS-style `letter_spacing` and
    /// `word_spacing` applied (both in pixels).
    pub fn measure_with_spacing(
        &self,
        text: &str,
        px: f32,
        letter_spacing: f32,
        word_spacing: f32,
    ) -> f32 {
        self.layout(text, px, letter_spacing, word_spacing)
            .iter()
            .map(|glyph| glyph.advance)
            .sum()
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
