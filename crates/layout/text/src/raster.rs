//! Anti-aliased rasterizer for the embedded vector face.
//!
//! Glyph outlines are flattened to polygons and sampled with a 4x4 coverage
//! grid using the non-zero winding rule. The result is a small, reusable
//! coverage bitmap that both the display-list rasterizer and the window chrome
//! composite with a text colour, so page and UI text share one implementation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::glyphs::Font;
use crate::truetype::OutlinePoint;

/// Samples per axis when computing glyph coverage.
const SUB: usize = 4;
const SUB_F: f32 = SUB as f32;

/// A coverage bitmap for one glyph at one pixel size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterGlyph {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Offset from the pen position to the bitmap's left edge.
    pub left: i32,
    /// Offset from the baseline to the bitmap's top edge (negative is above).
    pub top: i32,
    /// Row-major coverage in `0..=255`.
    pub coverage: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    ch: char,
    /// Pixel size in quarter pixels.
    size: u32,
    bold: bool,
    italic: bool,
}

static CACHE: OnceLock<Mutex<HashMap<Key, Arc<RasterGlyph>>>> = OnceLock::new();

/// Rasterizes `ch` at `px`, returning `None` for blank and control characters.
///
/// Sizes are quantised to a quarter pixel so the same bitmap is reused across
/// frames; rendering and measurement both use the quantised size.
pub fn quantize(px: f32) -> f32 {
    if px <= 0.0 {
        0.0
    } else {
        (px * 4.0).round().max(1.0) / 4.0
    }
}

/// Rasterizes a glyph and caches it by character, size and style.
pub fn rasterize_cached(
    font: &Font,
    ch: char,
    px: f32,
    bold: bool,
    italic: bool,
) -> Option<Arc<RasterGlyph>> {
    let size = (quantize(px) * 4.0).round() as u32;
    if size == 0 {
        return None;
    }
    let key = Key {
        ch,
        size,
        bold,
        italic,
    };
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(glyph) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Some(Arc::clone(glyph));
    }
    let glyph = Arc::new(rasterize(font, ch, size as f32 / 4.0, bold, italic)?);
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, Arc::clone(&glyph));
    Some(glyph)
}

/// Rasterizes one glyph without using the cache.
pub fn rasterize(font: &Font, ch: char, px: f32, bold: bool, italic: bool) -> Option<RasterGlyph> {
    if px <= 0.0 || ch.is_control() {
        return None;
    }
    let glyph = font.glyph(ch)?;
    let (min_x, min_y, max_x, max_y) = glyph.bounds()?;
    let scale = px / f32::from(font.face().units_per_em);
    if scale <= 0.0 {
        return None;
    }

    let pad = 2.0;
    let italic_slack = if italic {
        (max_y - min_y) as f32 * 0.22 * scale
    } else {
        0.0
    };
    let bold_slack = if bold { 1.0 } else { 0.0 };
    let left = (f32::from(min_x) * scale - pad).floor() as i32;
    let top = (-f32::from(max_y) * scale - pad).floor() as i32;
    let right = (f32::from(max_x) * scale + pad + italic_slack + bold_slack).ceil() as i32;
    let bottom = (-f32::from(min_y) * scale + pad).ceil() as i32;
    let width = (right - left).max(0) as u32;
    let height = (bottom - top).max(0) as u32;
    if width == 0 || height == 0 {
        return None;
    }

    let polygons: Vec<Vec<(f32, f32)>> = glyph
        .contours
        .iter()
        .map(|contour| flatten(contour))
        .filter(|polygon| polygon.len() >= 3)
        .collect();
    if polygons.is_empty() {
        return None;
    }

    let ascent = f32::from(font.face().ascender);
    let bold_step = 0.5 / scale;
    let mut coverage = vec![0_u8; width as usize * height as usize];
    for cy in 0..height {
        for cx in 0..width {
            let mut hits = 0_usize;
            for sy in 0..SUB {
                let py = top as f32 + cy as f32 + (sy as f32 + 0.5) / SUB_F;
                let fy = -py / scale;
                for sx in 0..SUB {
                    let px_at = left as f32 + cx as f32 + (sx as f32 + 0.5) / SUB_F;
                    let mut fx = px_at / scale;
                    if italic {
                        fx -= (ascent - fy) * 0.20;
                    }
                    let mut filled = winding(&polygons, fx, fy) != 0;
                    if bold && !filled {
                        filled = winding(&polygons, fx - bold_step, fy) != 0
                            || winding(&polygons, fx + bold_step, fy) != 0;
                    }
                    if filled {
                        hits += 1;
                    }
                }
            }
            coverage[cy as usize * width as usize + cx as usize] = (hits * 255 / (SUB * SUB)) as u8;
        }
    }
    Some(RasterGlyph {
        width,
        height,
        left,
        top,
        coverage,
    })
}

/// Turns a TrueType contour (linear and quadratic segments) into a polygon in
/// font units. Consecutive off-curve points contribute an implied on-curve
/// midpoint, exactly as the format specifies.
fn flatten(contour: &[OutlinePoint]) -> Vec<(f32, f32)> {
    let n = contour.len();
    if n < 2 {
        return Vec::new();
    }
    let start = if contour[0].on_curve {
        (f32::from(contour[0].x), f32::from(contour[0].y))
    } else {
        let first = contour[0];
        let last = contour[n - 1];
        (
            (f32::from(first.x) + f32::from(last.x)) / 2.0,
            (f32::from(first.y) + f32::from(last.y)) / 2.0,
        )
    };
    let mut polygon = vec![start];
    let mut index = if contour[0].on_curve { 1 } else { 0 };
    let mut guard = 0;
    while index < n && guard <= n + 1 {
        guard += 1;
        let point = contour[index];
        if point.on_curve {
            polygon.push((f32::from(point.x), f32::from(point.y)));
            index += 1;
            continue;
        }
        let next = contour[(index + 1) % n];
        let end = if next.on_curve {
            index += 2;
            (f32::from(next.x), f32::from(next.y))
        } else {
            index += 1;
            (
                (f32::from(point.x) + f32::from(next.x)) / 2.0,
                (f32::from(point.y) + f32::from(next.y)) / 2.0,
            )
        };
        let control = (f32::from(point.x), f32::from(point.y));
        let from = *polygon.last().expect("polygon has a start point");
        let length = hypot(control.0 - from.0, control.1 - from.1)
            + hypot(end.0 - control.0, end.1 - control.1);
        let steps = (length / 12.0).ceil().clamp(2.0, 32.0) as usize;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let u = 1.0 - t;
            polygon.push((
                u * u * from.0 + 2.0 * u * t * control.0 + t * t * end.0,
                u * u * from.1 + 2.0 * u * t * control.1 + t * t * end.1,
            ));
        }
    }
    polygon
}

fn hypot(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

/// Non-zero winding test at a point given in font units.
fn winding(polygons: &[Vec<(f32, f32)>], x: f32, y: f32) -> i32 {
    let mut result = 0;
    for polygon in polygons {
        let count = polygon.len();
        for index in 0..count {
            let (ax, ay) = polygon[index];
            let (bx, by) = polygon[(index + 1) % count];
            if (ay <= y && by > y) || (by <= y && ay > y) {
                let crossing = ax + (y - ay) * (bx - ax) / (by - ay);
                if crossing > x {
                    result += if by > ay { 1 } else { -1 };
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> &'static Font {
        Font::monospace()
    }

    #[test]
    fn glyphs_have_smooth_coverage() {
        let glyph = rasterize(font(), 'A', 16.0, false, false).expect("A rasterizes");
        assert!(glyph.width > 4 && glyph.height > 4);
        let full = glyph.coverage.iter().filter(|&&v| v == 255).count();
        let partial = glyph.coverage.iter().filter(|&&v| v > 0 && v < 255).count();
        assert!(full > 0, "glyph has solid interior");
        assert!(partial > 0, "glyph has anti-aliased edges");
    }

    #[test]
    fn blank_and_control_characters_have_no_glyph() {
        assert!(rasterize(font(), ' ', 16.0, false, false).is_none());
        assert!(rasterize(font(), '\n', 16.0, false, false).is_none());
    }

    #[test]
    fn sizes_scale_the_bitmap() {
        let small = rasterize(font(), 'H', 12.0, false, false).unwrap();
        let large = rasterize(font(), 'H', 48.0, false, false).unwrap();
        assert!(large.width > small.width * 2);
        assert!(large.height > small.height * 2);
    }

    #[test]
    fn cache_returns_the_same_glyph() {
        let a = rasterize_cached(font(), 'a', 14.0, false, false).unwrap();
        let b = rasterize_cached(font(), 'a', 14.0, false, false).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
