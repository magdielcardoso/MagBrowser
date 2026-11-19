//! Text shaping: turning a string into positioned glyphs.
//!
//! Shaping is pure advance layout over the embedded proportional face: no
//! kerning, ligatures or bidirectional reordering yet. The API is shaped like a
//! real shaper's so those features can be added without changing callers.

use crate::glyphs::Font;

/// A glyph placed at a horizontal pen position within a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    /// Source character (the fallback box keeps the requested character).
    pub ch: char,
    /// Pen position in pixels from the start of the run.
    pub x: f32,
    /// Advance of this glyph in pixels.
    pub advance: f32,
}

/// The result of shaping one string at one pixel size.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedRun {
    /// The source text, unchanged.
    pub text: String,
    /// Total advance of the run in pixels.
    pub width: f32,
    /// One entry per drawable character, in visual order.
    pub glyphs: Vec<PositionedGlyph>,
}

/// Shapes `text` into a run of horizontally positioned glyphs.
///
/// Characters without a glyph (controls such as `\n`) advance nothing and are
/// omitted from `glyphs`; every drawable character produces exactly one entry.
pub fn shape(font: &Font, text: &str, px: f32) -> ShapedRun {
    let mut x = 0.0;
    let mut glyphs = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        if font.glyph(ch).is_none() {
            continue;
        }
        let advance = font.advance(ch, px);
        glyphs.push(PositionedGlyph { ch, x, advance });
        x += advance;
    }
    ShapedRun {
        text: text.to_owned(),
        width: x,
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
}
