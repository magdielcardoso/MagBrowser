//! Text shaping: turning a string into positioned glyphs.
//!
//! Shaping lays out the embedded proportional face with GPOS pair kerning,
//! GSUB ligatures and CSS-style letter/word spacing. Every glyph carries the
//! byte range of the source characters it covers, so hit testing keeps working
//! even when several characters collapse into one ligature glyph.

use crate::glyphs::Font;

/// A glyph placed at a horizontal pen position within a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    /// Source character (for a ligature, the first character of the cluster).
    pub ch: char,
    /// Pen position in pixels from the start of the run.
    pub x: f32,
    /// Advance of this glyph in pixels, including kerning and spacing.
    pub advance: f32,
    /// Byte offset of the first source character covered by this glyph.
    pub byte_start: usize,
    /// Byte offset just past the last source character (exclusive).
    pub byte_end: usize,
    /// TrueType glyph id used to draw this glyph (a ligature's single id).
    pub glyph_id: u16,
}

impl PositionedGlyph {
    /// Whether this glyph's source range covers `byte`.
    pub fn contains_byte(&self, byte: usize) -> bool {
        (self.byte_start..self.byte_end).contains(&byte)
    }
}

/// The result of shaping one string at one pixel size.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedRun {
    /// The source text, unchanged.
    pub text: String,
    /// Total advance of the run in pixels.
    pub width: f32,
    /// One entry per drawable glyph, in visual order. A ligature occupies one
    /// entry whose byte range spans every source character it substituted.
    pub glyphs: Vec<PositionedGlyph>,
}

impl ShapedRun {
    /// Finds the glyph under a byte offset in the source text, for hit testing.
    /// A byte inside a ligature cluster resolves to that single glyph.
    pub fn glyph_at_byte(&self, byte: usize) -> Option<&PositionedGlyph> {
        self.glyphs.iter().find(|glyph| glyph.contains_byte(byte))
    }
}

/// Shapes `text` into a run of horizontally positioned glyphs.
///
/// Characters without a glyph (controls such as `\n`) advance nothing and are
/// omitted from `glyphs`; every drawable character produces an entry, except
/// ligated sequences which share one entry spanning their byte range.
pub fn shape(font: &Font, text: &str, px: f32) -> ShapedRun {
    shape_with_spacing(font, text, px, 0.0, 0.0)
}

/// Shapes `text` with CSS-style `letter_spacing` and `word_spacing` (in pixels).
///
/// `letter_spacing` is added after every glyph; `word_spacing` is added
/// additionally after spaces. The resulting width matches
/// [`Font::measure_with_spacing`].
pub fn shape_with_spacing(
    font: &Font,
    text: &str,
    px: f32,
    letter_spacing: f32,
    word_spacing: f32,
) -> ShapedRun {
    let laid_out = font.layout(text, px, letter_spacing, word_spacing);
    let width = laid_out.iter().map(|glyph| glyph.advance).sum();
    let glyphs = laid_out
        .into_iter()
        .map(|glyph| PositionedGlyph {
            ch: glyph.ch,
            x: glyph.x,
            advance: glyph.advance,
            byte_start: glyph.byte_start,
            byte_end: glyph.byte_end,
            glyph_id: glyph.glyph_id,
        })
        .collect();
    ShapedRun {
        text: text.to_owned(),
        width,
        glyphs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_proportional_run() {
        let font = Font::sans();
        let run = shape(font, "abc", 16.0);
        assert_eq!(run.text, "abc");
        assert!((run.width - font.measure("abc", 16.0)).abs() < 1e-4);
        assert_eq!(run.glyphs.len(), 3);
        assert_eq!(
            run.glyphs[0],
            PositionedGlyph {
                ch: 'a',
                x: 0.0,
                advance: font.advance('a', 16.0),
                byte_start: 0,
                byte_end: 1,
                glyph_id: font.face().glyph_index('a'),
            }
        );
        assert!((run.glyphs[2].x - font.measure("ab", 16.0)).abs() < 1e-4);
    }

    #[test]
    fn shaping_skips_control_characters() {
        let font = Font::sans();
        let run = shape(font, "a\nb", 16.0);
        assert_eq!(run.glyphs.len(), 2);
        assert!((run.width - font.measure("ab", 16.0)).abs() < 1e-4);
        assert_eq!(run.glyphs[1].ch, 'b');
        assert!((run.glyphs[1].x - font.advance('a', 16.0)).abs() < 1e-4);
    }

    #[test]
    fn shaping_keeps_unmapped_glyph() {
        let font = Font::sans();
        let run = shape(font, "✓", 16.0);
        assert_eq!(run.glyphs.len(), 1);
        assert_eq!(run.glyphs[0].ch, '✓');
        assert!((run.width - font.advance('✓', 16.0)).abs() < 1e-4);
    }

    #[test]
    fn empty_run_has_zero_width() {
        let run = shape(Font::monospace(), "", 16.0);
        assert!(run.glyphs.is_empty());
        assert_eq!(run.width, 0.0);
        assert_eq!(run.text, "");
    }

    #[test]
    fn run_width_matches_measure() {
        let font = Font::monospace();
        let text = "Hello, world! 123";
        assert_eq!(shape(font, text, 20.0).width, font.measure(text, 20.0));
    }

    #[test]
    fn shaping_scales_positions() {
        let font = Font::sans();
        let run = shape(font, "ab", 32.0);
        assert!((run.glyphs[1].x - font.advance('a', 32.0)).abs() < 1e-4);
        assert!((run.glyphs[1].advance - font.advance('b', 32.0)).abs() < 1e-4);
        assert!((run.width - font.measure("ab", 32.0)).abs() < 1e-4);
    }

    #[test]
    fn kerning_tightens_av_and_to() {
        let font = Font::sans();
        for (text, first, second) in [("AV", 'A', 'V'), ("To", 'T', 'o')] {
            let unkerned = font.advance(first, 16.0) + font.advance(second, 16.0);
            let kern = font.kerning(first, second, 16.0);
            assert!(kern < 0.0, "expected negative kerning for {text}");
            assert!((font.measure(text, 16.0) - (unkerned + kern)).abs() < 1e-4);
            assert!(font.measure(text, 16.0) < unkerned);
            // The run's width uses the very same kerning, so layout and the
            // painted measurement cannot disagree.
            let run = shape(font, text, 16.0);
            assert!((run.width - font.measure(text, 16.0)).abs() < 1e-5);
            assert!((run.glyphs[0].advance - (font.advance(first, 16.0) + kern)).abs() < 1e-5);
            assert!((run.glyphs[1].x - run.glyphs[0].advance).abs() < 1e-5);
        }
    }

    #[test]
    fn measure_equals_shaped_run_width() {
        let font = Font::sans();
        for text in ["", "abc", "AVATAR", "To be, or not to be", "héllo — 世界"] {
            for px in [8.0, 13.0, 16.0, 33.5] {
                let run = shape(font, text, px);
                assert!(
                    (run.width - font.measure(text, px)).abs() < 1e-4,
                    "width mismatch for {text:?} at {px}px"
                );
                let sum: f32 = run.glyphs.iter().map(|glyph| glyph.advance).sum();
                assert!((run.width - sum).abs() < 1e-4);
                // Positions are the running sum of advances.
                let mut expected = 0.0;
                for glyph in &run.glyphs {
                    assert!((glyph.x - expected).abs() < 1e-4);
                    expected += glyph.advance;
                }
            }
        }
    }

    #[test]
    fn letter_and_word_spacing_apply() {
        let font = Font::sans();
        let run = shape(font, "a b", 16.0);
        let spaced = shape_with_spacing(font, "a b", 16.0, 2.0, 3.0);
        // Three glyphs, each gets 2px letter spacing; the space also gets 3px.
        assert!((spaced.width - (run.width + 2.0 * 3.0 + 3.0)).abs() < 1e-4);
        assert!((spaced.width - font.measure_with_spacing("a b", 16.0, 2.0, 3.0)).abs() < 1e-4);
        assert!(
            (shape_with_spacing(font, "hi", 16.0, 1.0, 5.0).width
                - (font.measure("hi", 16.0) + 2.0))
                .abs()
                < 1e-4
        );
    }

    #[test]
    fn byte_ranges_cover_the_source_text() {
        let font = Font::sans();
        let run = shape(font, "aé中z", 16.0);
        assert_eq!(run.glyphs.len(), 4);
        let mut covered = String::new();
        for glyph in &run.glyphs {
            covered.push_str(&run.text[glyph.byte_start..glyph.byte_end]);
            assert!(glyph.contains_byte(glyph.byte_start));
        }
        assert_eq!(covered, run.text);
        // Hit testing maps each byte to exactly the glyph covering it.
        for (index, glyph) in run.glyphs.iter().enumerate() {
            for byte in glyph.byte_start..glyph.byte_end {
                assert_eq!(run.glyph_at_byte(byte), Some(glyph));
            }
            assert!(index > 0 || glyph.byte_start == 0);
        }
    }
}
