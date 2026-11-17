//! A glyph atlas built from the engine's bitmap font.
//!
//! Chrome text is drawn from a single RGBA texture instead of asking the
//! rasterizer for one glyph at a time. The atlas is generated from
//! [`browser_text::Font`] glyph bitmaps, so the window UI uses exactly the same
//! face as the page, and it is sampled with nearest-neighbour filtering so the
//! pixels stay crisp at every scale.
//!
//! The atlas is plain data (no GPU types), which keeps it usable by the
//! deterministic self-check renderer and testable without a window.

use browser_css::Color;
use browser_paint::ImageBuffer;
use browser_text::Font;

/// Source bitmap width of one glyph in the built-in face.
pub const GLYPH_WIDTH: u32 = 8;
/// Source bitmap height of one glyph in the built-in face.
pub const GLYPH_HEIGHT: u32 = 8;
/// Columns in the atlas grid.
const COLUMNS: u32 = 16;
/// Printable ASCII (`0x20..=0x7e`) plus one synthetic ellipsis slot.
const SLOTS: u32 = 96;
/// Index of the synthetic ellipsis glyph (`U+2026`).
const ELLIPSIS_SLOT: usize = 95;

/// A nearest-neighbour sampled RGBA glyph atlas.
#[derive(Debug, Clone)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    /// RGBA texels: white with the glyph coverage in the alpha channel.
    pixels: Vec<u8>,
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}

impl GlyphAtlas {
    /// Builds the atlas from the built-in monospace face.
    pub fn new() -> Self {
        let rows = SLOTS.div_ceil(COLUMNS);
        let width = COLUMNS * GLYPH_WIDTH;
        let height = rows * GLYPH_HEIGHT;
        let mut pixels = vec![0; (width as usize) * (height as usize) * 4];
        let font = Font::monospace();
        for slot in 0..SLOTS {
            let Some(ch) = char_for_slot(slot as usize) else {
                continue;
            };
            let cell_x = (slot % COLUMNS) * GLYPH_WIDTH;
            let cell_y = (slot / COLUMNS) * GLYPH_HEIGHT;
            let coverage = coverage_for(font, ch);
            for row in 0..GLYPH_HEIGHT {
                let bits = coverage[row as usize];
                for col in 0..GLYPH_WIDTH {
                    if bits & (0x80 >> col) == 0 {
                        continue;
                    }
                    let index = (((cell_y + row) * width + cell_x + col) as usize) * 4;
                    pixels[index] = 255;
                    pixels[index + 1] = 255;
                    pixels[index + 2] = 255;
                    pixels[index + 3] = 255;
                }
            }
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Atlas width in texels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Atlas height in texels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Raw RGBA texels, row-major.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The atlas slot for a character, if it has one.
    pub fn slot_for(&self, ch: char) -> Option<usize> {
        if ch == '\u{2026}' {
            return Some(ELLIPSIS_SLOT);
        }
        let code = ch as u32;
        if (0x20..=0x7e).contains(&code) {
            Some((code - 0x20) as usize)
        } else {
            None
        }
    }

    /// The source rectangle of a glyph slot, in texels.
    pub fn glyph_rect(&self, slot: usize) -> (u32, u32, u32, u32) {
        let slot = slot as u32;
        (
            (slot % COLUMNS) * GLYPH_WIDTH,
            (slot / COLUMNS) * GLYPH_HEIGHT,
            GLYPH_WIDTH,
            GLYPH_HEIGHT,
        )
    }

    /// Nearest-neighbour texel lookup.
    pub fn texel(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0, 0, 0, 0];
        }
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        [
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ]
    }

    #[allow(clippy::too_many_arguments)]
    /// Paints a single line of text into an image buffer.
    ///
    /// `baseline` matches [`browser_paint::DisplayCommand::DrawText`], so page
    /// and UI text share the same vertical convention. `max_width` clips on the
    /// right without stretching the glyphs.
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
        let font = Font::monospace();
        let scale = font_size / font.cell_height();
        let glyph_size = (GLYPH_WIDTH as f32 * scale).round().max(1.0);
        let top = baseline - font.ascent(font_size);
        let limit = max_width.map(|width| x + width.max(0.0));
        let mut pen_x = x;
        for ch in text.chars() {
            let advance = font.advance(ch, font_size);
            if let Some(limit) = limit
                && pen_x >= limit
            {
                break;
            }
            let Some(slot) = self.slot_for(ch) else {
                pen_x += advance;
                continue;
            };
            let (src_x, src_y, src_w, src_h) = self.glyph_rect(slot);
            let draw_w = glyph_size as i32;
            let draw_h = glyph_size as i32;
            for dy in 0..draw_h {
                // Nearest-neighbour: map the destination row back to a texel.
                let sy = src_y as i32 + ((dy as f32 + 0.5) / glyph_size * src_h as f32) as i32;
                for dx in 0..draw_w {
                    let sx = src_x as i32 + ((dx as f32 + 0.5) / glyph_size * src_w as f32) as i32;
                    let texel = self.texel(sx, sy);
                    if texel[3] == 0 {
                        continue;
                    }
                    let px = pen_x.round() as i32 + dx;
                    let py = top.round() as i32 + dy;
                    if let Some(limit) = limit
                        && px as f32 >= limit
                    {
                        continue;
                    }
                    blend_pixel(target, px, py, color);
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
        let font = Font::monospace();
        let baseline =
            rect_y + (rect_height - font.line_height(font_size)) / 2.0 + font.ascent(font_size);
        self.paint_text(target, x, baseline, text, font_size, color, max_width);
    }
}

fn char_for_slot(slot: usize) -> Option<char> {
    if slot == ELLIPSIS_SLOT {
        return Some('\u{2026}');
    }
    char::from_u32(0x20 + slot as u32)
}

/// Coverage rows for a slot. The ellipsis is synthesised as three dots so the
/// Portuguese status text renders instead of falling back to the box glyph.
fn coverage_for(font: &Font, ch: char) -> [u8; 8] {
    if ch == '\u{2026}' {
        let mut rows = [0_u8; 8];
        rows[6] = 0b1001_0010;
        return rows;
    }
    let mut rows = [0_u8; 8];
    if let Some(glyph) = font.glyph(ch) {
        for (row, bits) in rows.iter_mut().zip(glyph.rows.iter()) {
            *row = *bits;
        }
    }
    rows
}

fn blend_pixel(target: &mut ImageBuffer, x: i32, y: i32, color: Color) {
    if x < 0 || y < 0 || x >= target.width as i32 || y >= target.height as i32 {
        return;
    }
    let index = ((y as u32 * target.width + x as u32) * 4) as usize;
    if color.a == 0 {
        return;
    }
    let a = f32::from(color.a) / 255.0;
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
    fn atlas_covers_ascii_and_ellipsis() {
        let atlas = GlyphAtlas::new();
        assert_eq!(atlas.slot_for(' '), Some(0));
        assert_eq!(atlas.slot_for('A'), Some(33));
        assert_eq!(atlas.slot_for('~'), Some(94));
        assert_eq!(atlas.slot_for('\u{2026}'), Some(95));
        assert_eq!(atlas.slot_for('\n'), None);
        assert!(atlas.width() >= 8 && atlas.height() >= 8);
    }

    #[test]
    fn glyph_slots_have_visible_texels() {
        let atlas = GlyphAtlas::new();
        let (x, y, w, h) = atlas.glyph_rect(atlas.slot_for('A').unwrap());
        let mut visible = 0;
        for row in y..y + h {
            for col in x..x + w {
                if atlas.texel(col as i32, row as i32)[3] != 0 {
                    visible += 1;
                }
            }
        }
        assert!(visible > 4, "letter A should have coverage");
    }

    #[test]
    fn text_paint_changes_pixels_and_clips() {
        let atlas = GlyphAtlas::new();
        let mut image = ImageBuffer::filled(120, 24, Color::rgb(255, 255, 255));
        atlas.paint_text(
            &mut image,
            2.0,
            18.0,
            "Pronto",
            14.0,
            Color::rgb(0, 0, 0),
            Some(40.0),
        );
        assert!(image.coverage_vs(Color::rgb(255, 255, 255)) > 0.0);
        for x in 42..120 {
            for y in 0..24 {
                assert_eq!(image.pixel(x, y), [255, 255, 255, 255]);
            }
        }
    }

    #[test]
    fn ellipsis_has_ink() {
        let atlas = GlyphAtlas::new();
        let slot = atlas.slot_for('\u{2026}').unwrap();
        let (x, y, _, _) = atlas.glyph_rect(slot);
        let ink = (x..x + 8)
            .flat_map(|col| (y..y + 8).map(move |row| (col, row)))
            .filter(|(col, row)| atlas.texel(*col as i32, *row as i32)[3] != 0)
            .count();
        assert!(ink >= 3);
    }
}
