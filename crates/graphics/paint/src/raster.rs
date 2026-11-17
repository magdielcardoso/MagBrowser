//! Software rasterizer: executes the display list into an RGBA buffer.

use std::path::Path;

use browser_css::Color;
use browser_layout::Size;
use browser_layout::box_model::Rect;
use browser_style::BorderStyle;

use crate::display_list::{DisplayCommand, DisplayList};
use crate::images::ImageData;
use crate::png;

/// An RGBA8 image buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageBuffer {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA pixels, row-major.
    pub pixels: Vec<u8>,
}

impl ImageBuffer {
    /// Creates a fully transparent buffer.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    /// Creates a buffer filled with `color`.
    pub fn filled(width: u32, height: u32, color: Color) -> Self {
        let mut buffer = Self::new(width, height);
        for index in (0..buffer.pixels.len()).step_by(4) {
            buffer.pixels[index] = color.r;
            buffer.pixels[index + 1] = color.g;
            buffer.pixels[index + 2] = color.b;
            buffer.pixels[index + 3] = 255;
        }
        buffer
    }

    /// Reads a pixel as RGBA.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
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

    /// Encodes the buffer as PNG bytes.
    pub fn to_png(&self) -> Vec<u8> {
        png::encode(self.width, self.height, &self.pixels)
    }

    /// Writes the buffer to disk as a PNG.
    pub fn save_png(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        std::fs::write(path, self.to_png())
    }

    /// Fraction of pixels that differ from `background` (used by render tests).
    pub fn coverage_vs(&self, background: Color) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let mut different = 0_usize;
        for y in 0..self.height {
            for x in 0..self.width {
                let p = self.pixel(x, y);
                if p[0] != background.r || p[1] != background.g || p[2] != background.b {
                    different += 1;
                }
            }
        }
        different as f32 / (self.width as f32 * self.height as f32)
    }
}

/// Executes display lists into an [`ImageBuffer`].
pub struct Rasterizer {
    canvas: ImageBuffer,
    clips: Vec<Rect>,
}

impl Rasterizer {
    /// Creates a rasterizer with a filled background.
    pub fn new(viewport: Size, background: Color) -> Self {
        let width = viewport.width.max(1.0) as u32;
        let height = viewport.height.max(1.0) as u32;
        Self {
            canvas: ImageBuffer::filled(width, height, background),
            clips: Vec::new(),
        }
    }

    /// Executes a display list.
    pub fn execute(mut self, list: &DisplayList) -> ImageBuffer {
        for command in &list.commands {
            self.draw(command, &list.images);
        }
        self.canvas
    }

    /// Executes one command.
    pub fn draw(&mut self, command: &DisplayCommand, images: &[ImageData]) {
        match command {
            DisplayCommand::PushClip(rect) => self.clips.push(*rect),
            DisplayCommand::PopClip => {
                self.clips.pop();
            }
            DisplayCommand::FillRect { rect, color } => self.fill_rect(*rect, *color),
            DisplayCommand::DrawBorder {
                rect,
                widths,
                colors,
                styles,
            } => self.draw_border(*rect, widths, colors, styles),
            DisplayCommand::DrawText {
                x,
                baseline,
                text,
                font_size,
                color,
                underline,
                line_through,
                bold,
                italic,
            } => self.draw_text(
                *x,
                *baseline,
                text,
                *font_size,
                *color,
                *underline,
                *line_through,
                *bold,
                *italic,
            ),
            DisplayCommand::DrawImage { rect, image } => {
                if let Some(image) = images.get(*image) {
                    self.draw_image(*rect, image);
                }
            }
        }
    }

    fn visible(&self, x: f32, y: f32) -> bool {
        let px = x.floor();
        let py = y.floor();
        if px < 0.0 || py < 0.0 || px >= self.canvas.width as f32 || py >= self.canvas.height as f32
        {
            return false;
        }
        self.clips.iter().all(|clip| clip.contains(px, py))
    }

    fn blend(&mut self, x: i32, y: i32, color: Color) {
        if color.a == 0 {
            return;
        }
        let fx = x as f32;
        let fy = y as f32;
        if !self.visible(fx, fy) {
            return;
        }
        let width = self.canvas.width as i32;
        let index = ((y * width + x) * 4) as usize;
        let pixels = &mut self.canvas.pixels;
        let alpha = color.a as f32 / 255.0;
        let inverse = 1.0 - alpha;
        pixels[index] = (color.r as f32 * alpha + pixels[index] as f32 * inverse).round() as u8;
        pixels[index + 1] =
            (color.g as f32 * alpha + pixels[index + 1] as f32 * inverse).round() as u8;
        pixels[index + 2] =
            (color.b as f32 * alpha + pixels[index + 2] as f32 * inverse).round() as u8;
        pixels[index + 3] = 255;
    }

    fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.a == 0 || rect.is_empty() {
            return;
        }
        let x0 = rect.x.floor() as i32;
        let y0 = rect.y.floor() as i32;
        let x1 = (rect.x + rect.width).ceil() as i32;
        let y1 = (rect.y + rect.height).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend(x, y, color);
            }
        }
    }

    fn draw_border(
        &mut self,
        rect: Rect,
        widths: &browser_layout::Edges,
        colors: &[Color; 4],
        styles: &[BorderStyle; 4],
    ) {
        let sides = [
            Rect::new(rect.x, rect.y, rect.width, widths.top),
            Rect::new(
                rect.x + rect.width - widths.right,
                rect.y,
                widths.right,
                rect.height,
            ),
            Rect::new(
                rect.x,
                rect.y + rect.height - widths.bottom,
                rect.width,
                widths.bottom,
            ),
            Rect::new(rect.x, rect.y, widths.left, rect.height),
        ];
        for (i, side) in sides.iter().enumerate() {
            let style = styles[i];
            if !style.is_visible() || side.is_empty() {
                continue;
            }
            match style {
                BorderStyle::Dashed | BorderStyle::Dotted => {
                    self.draw_dashed(*side, colors[i], style == BorderStyle::Dotted, i);
                }
                _ => self.fill_rect(*side, colors[i]),
            }
        }
    }

    fn draw_dashed(&mut self, rect: Rect, color: Color, dotted: bool, side: usize) {
        let (dash, gap): (f32, f32) = if dotted { (1.0, 1.0) } else { (3.0, 3.0) };
        let horizontal = matches!(side, 0 | 2);
        let length = if horizontal { rect.width } else { rect.height };
        let thickness = if horizontal { rect.height } else { rect.width };
        let mut pos = 0.0;
        while pos < length {
            let span = dash.min(length - pos);
            let piece = if horizontal {
                Rect::new(rect.x + pos, rect.y, span, thickness)
            } else {
                Rect::new(rect.x, rect.y + pos, thickness, span)
            };
            self.fill_rect(piece, color);
            pos += dash + gap;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        x: f32,
        baseline: f32,
        text: &str,
        font_size: f32,
        color: Color,
        underline: bool,
        line_through: bool,
        bold: bool,
        italic: bool,
    ) {
        let font = browser_text::Font::monospace();
        let scale = (font_size / 16.0).max(0.1);
        let mut pen_x = x;
        let top = baseline - font.ascent(font_size);
        for ch in text.chars() {
            let Some(glyph) = font.glyph(ch) else {
                pen_x += font.advance(ch, font_size);
                continue;
            };
            for row in 0..glyph.height {
                let Some(bits) = glyph.rows.get(row as usize) else {
                    break;
                };
                for col in 0..glyph.width {
                    if bits & (0x80 >> col) == 0 {
                        continue;
                    }
                    let shear = if italic {
                        (glyph.height as f32 - f32::from(row)) * 0.18 * scale
                    } else {
                        0.0
                    };
                    let px = pen_x + f32::from(col) * scale + shear;
                    let py = top + f32::from(row) * scale;
                    let size = scale.ceil() as i32;
                    for dy in 0..size {
                        for dx in 0..size {
                            self.blend(px as i32 + dx, py as i32 + dy, color);
                            if bold {
                                self.blend(px as i32 + dx + size, py as i32 + dy, color);
                            }
                        }
                    }
                }
            }
            pen_x += font.advance(ch, font_size);
        }
        let width = font.measure(text, font_size);
        if underline {
            self.fill_rect(Rect::new(x, baseline + 1.0, width, 1.0), color);
        }
        if line_through {
            self.fill_rect(Rect::new(x, baseline - font_size * 0.3, width, 1.0), color);
        }
    }

    fn draw_image(&mut self, rect: Rect, image: &ImageData) {
        if rect.is_empty() || image.width == 0 || image.height == 0 {
            return;
        }
        let x0 = rect.x.floor() as i32;
        let y0 = rect.y.floor() as i32;
        let x1 = (rect.x + rect.width).ceil() as i32;
        let y1 = (rect.y + rect.height).ceil() as i32;
        for y in y0..y1 {
            let v = (y as f32 - rect.y) / rect.height;
            for x in x0..x1 {
                let u = (x as f32 - rect.x) / rect.width;
                let [r, g, b, a] = image.sample(u, v);
                self.blend(x, y, Color::rgba(r, g, b, a));
            }
        }
    }
}

/// Renders a display list into an RGBA buffer.
pub fn render(list: &DisplayList, viewport: Size, background: Color) -> ImageBuffer {
    Rasterizer::new(viewport, background).execute(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Size {
        Size {
            width: 64.0,
            height: 64.0,
        }
    }

    #[test]
    fn fills_rectangles() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(8.0, 8.0, 16.0, 16.0),
            color: Color::rgb(255, 0, 0),
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));
        assert_eq!(image.pixel(10, 10), [255, 0, 0, 255]);
        assert_eq!(image.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn clipping_restricts_painting() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::PushClip(Rect::new(0.0, 0.0, 10.0, 10.0)));
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(0.0, 0.0, 64.0, 64.0),
            color: Color::rgb(0, 0, 255),
        });
        list.push(DisplayCommand::PopClip);
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));
        assert_eq!(image.pixel(5, 5), [0, 0, 255, 255]);
        assert_eq!(image.pixel(20, 20), [255, 255, 255, 255]);
    }

    #[test]
    fn draws_text_glyphs() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawText {
            x: 4.0,
            baseline: 20.0,
            text: "A".into(),
            font_size: 16.0,
            color: Color::rgb(0, 0, 0),
            underline: false,
            line_through: false,
            bold: false,
            italic: false,
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));
        assert!(image.coverage_vs(Color::rgb(255, 255, 255)) > 0.001);
    }

    #[test]
    fn borders_paint_each_side() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawBorder {
            rect: Rect::new(4.0, 4.0, 20.0, 20.0),
            widths: browser_layout::Edges::uniform(2.0),
            colors: [
                Color::rgb(255, 0, 0),
                Color::rgb(0, 255, 0),
                Color::rgb(0, 0, 255),
                Color::rgb(255, 255, 0),
            ],
            styles: [BorderStyle::Solid; 4],
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));
        assert_eq!(image.pixel(10, 4), [255, 0, 0, 255]);
        assert_eq!(image.pixel(4, 10), [255, 255, 0, 255]);
        assert_eq!(image.pixel(10, 10), [255, 255, 255, 255]);
    }

    #[test]
    fn images_are_scaled() {
        let mut image = ImageData::new(2, 2);
        image.set(0, 0, 10, 20, 30, 255);
        image.set(1, 1, 40, 50, 60, 255);
        let mut list = DisplayList::new();
        let index = list.add_image(image);
        list.push(DisplayCommand::DrawImage {
            rect: Rect::new(0.0, 0.0, 20.0, 20.0),
            image: index,
        });
        let rendered = render(&list, viewport(), Color::rgb(255, 255, 255));
        assert_eq!(rendered.pixel(2, 2), [10, 20, 30, 255]);
        assert_eq!(rendered.pixel(18, 18), [40, 50, 60, 255]);
    }
}
