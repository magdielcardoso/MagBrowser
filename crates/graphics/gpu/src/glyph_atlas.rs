//! Text painting for window chrome.
//!
//! Chrome text uses the same embedded Inter outlines and the same anti-aliased
//! glyph cache as page text, so the native window and the deterministic
//! `--window-selfcheck` PNG show identical, smooth pixels. The type used to be
//! an 8x8 bitmap atlas; the name is kept because it is the chrome text painter.

use browser_css::Color;
use browser_paint::ImageBuffer;
use browser_text::{Font, rasterize_cached};

/// A text painter backed by the shared glyph cache.
#[derive(Debug, Clone, Default)]
pub struct GlyphAtlas;

impl GlyphAtlas {
    /// Creates a text painter. Glyphs are rasterised lazily and cached.
    pub fn new() -> Self {
        Self
    }

    #[allow(clippy::too_many_arguments)]
    /// Paints one line of text into an image buffer.
    ///
    /// `baseline` matches [`browser_paint::DisplayCommand::DrawText`], so page
    /// and UI text share the same vertical convention. `max_width` clips on the
    /// right without stretching glyphs.
    pub fn paint_text(
        &self,
        target: &mut ImageBuffer,
        x: f32,
        baseline: f32,
        text: &str,
        font_size: f32,
        color: Color,
        max_width: Option<f32>,
    ) {
        if font_size <= 0.0 {
            return;
        }
        let font = Font::sans();
        let px = browser_text::quantize(font_size);
        let limit = max_width.map(|width| x + width.max(0.0));
        let mut pen_x = x;
        for ch in text.chars() {
            let advance = font.advance(ch, px);
            if limit.is_some_and(|limit| pen_x >= limit) {
                break;
            }
            let Some(glyph) = rasterize_cached(font, ch, px, false, false) else {
                pen_x += advance;
                continue;
            };
            let glyph_x = pen_x.round() as i32 + glyph.left;
            let glyph_y = baseline.round() as i32 + glyph.top;
            for row in 0..glyph.height {
                for col in 0..glyph.width {
                    let coverage =
                        glyph.coverage[row as usize * glyph.width as usize + col as usize];
                    if coverage == 0 {
                        continue;
                    }
                    let px_at = glyph_x + col as i32;
                    if limit.is_some_and(|limit| px_at as f32 >= limit) {
                        continue;
                    }
                    let alpha = (u16::from(coverage) * u16::from(color.a) / 255) as u8;
                    blend_pixel(target, px_at, glyph_y + row as i32, color, alpha);
                }
            }
            pen_x += advance;
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// Paints text centred vertically inside a rectangle.
    pub fn paint_text_centered(
        &self,
        target: &mut ImageBuffer,
        x: f32,
        rect_y: f32,
        rect_height: f32,
        text: &str,
        font_size: f32,
        color: Color,
        max_width: Option<f32>,
    ) {
        let font = Font::sans();
        let baseline =
            rect_y + (rect_height - font.line_height(font_size)) / 2.0 + font.ascent(font_size);
        self.paint_text(target, x, baseline, text, font_size, color, max_width);
    }
}

fn blend_pixel(target: &mut ImageBuffer, x: i32, y: i32, color: Color, alpha: u8) {
    if x < 0 || y < 0 || x >= target.width as i32 || y >= target.height as i32 || alpha == 0 {
        return;
    }
    let index = ((y as u32 * target.width + x as u32) * 4) as usize;
    let a = f32::from(alpha) / 255.0;
    let inverse = 1.0 - a;
    target.pixels[index] =
        (f32::from(color.r) * a + f32::from(target.pixels[index]) * inverse).round() as u8;
    target.pixels[index + 1] =
        (f32::from(color.g) * a + f32::from(target.pixels[index + 1]) * inverse).round() as u8;
    target.pixels[index + 2] =
        (f32::from(color.b) * a + f32::from(target.pixels[index + 2]) * inverse).round() as u8;
    target.pixels[index + 3] = 255;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_have_anti_aliased_coverage() {
        let glyph = browser_text::rasterize_cached(Font::sans(), 'A', 14.0, false, false).unwrap();
        let partial = glyph
            .coverage
            .iter()
            .filter(|&&value| value > 0 && value < 255)
            .count();
        assert!(partial > 0, "outline edges should be anti-aliased");
    }

    #[test]
    fn ellipsis_and_accents_paint() {
        for ch in ['…', 'ç', 'ã', 'ó'] {
            let glyph = browser_text::rasterize_cached(Font::sans(), ch, 12.0, false, false);
            assert!(glyph.is_some(), "missing glyph for {ch:?}");
        }
    }

    #[test]
    fn text_paint_changes_pixels_and_clips() {
        let atlas = GlyphAtlas::new();
        let mut image = ImageBuffer::filled(160, 24, Color::rgb(255, 255, 255));
        atlas.paint_text(
            &mut image,
            2.0,
            18.0,
            "Pronto",
            14.0,
            Color::rgb(0, 0, 0),
            Some(60.0),
        );
        assert!(image.coverage_vs(Color::rgb(255, 255, 255)) > 0.0);
        for x in 70..160 {
            for y in 0..24 {
                assert_eq!(image.pixel(x, y), [255, 255, 255, 255]);
            }
        }
    }

    #[test]
    fn centered_text_has_ink() {
        let atlas = GlyphAtlas::new();
        let mut image = ImageBuffer::filled(120, 28, Color::rgb(255, 255, 255));
        atlas.paint_text_centered(
            &mut image,
            4.0,
            0.0,
            28.0,
            "StackLab",
            14.0,
            Color::rgb(0, 0, 0),
            None,
        );
        assert!(image.coverage_vs(Color::rgb(255, 255, 255)) > 0.0);
    }
}
