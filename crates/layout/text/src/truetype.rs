//! Minimal TrueType outline reader for the embedded Inter face.
//!
//! Only the tables needed to turn a Unicode scalar into a set of contours are
//! read: `head`, `hhea`, `hmtx`, `maxp`, `cmap`, `loca` and `glyf`. Composite
//! glyphs (accents such as `ã` and `ç`) are expanded recursively. No hinting is
//! applied — the anti-aliased rasterizer works directly from the font units.
//!
//! The font itself is embedded in [`crates/layout/text/fonts`] under the SIL
//! Open Font License; see `Inter-LICENSE.txt` next to it.

use std::sync::OnceLock;

const FONT_BYTES: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");

/// A point on a TrueType outline. `on_curve` is false for a quadratic control
/// point lying between two on-curve points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutlinePoint {
    /// Horizontal coordinate in font units (y up).
    pub x: i16,
    /// Vertical coordinate in font units (y up).
    pub y: i16,
    /// Whether this is an on-curve point.
    pub on_curve: bool,
}

/// A resolved glyph: an advance width and one contour per closed outline.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineGlyph {
    /// Horizontal advance in font units.
    pub advance: u16,
    /// Closed contours in font units.
    pub contours: Vec<Vec<OutlinePoint>>,
}

impl OutlineGlyph {
    /// An empty glyph with no drawable outline (space, or unknown character).
    pub fn blank(advance: u16) -> Self {
        Self {
            advance,
            contours: Vec::new(),
        }
    }

    /// Whether this glyph draws anything.
    pub fn is_empty(&self) -> bool {
        self.contours.iter().all(|contour| contour.len() < 2)
    }

    /// Bounding box of every on-curve and control point, in font units.
    pub fn bounds(&self) -> Option<(i16, i16, i16, i16)> {
        let mut min_x = i16::MAX;
        let mut min_y = i16::MAX;
        let mut max_x = i16::MIN;
        let mut max_y = i16::MIN;
        for point in self.contours.iter().flatten() {
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }
        (min_x <= max_x && min_y <= max_y).then_some((min_x, min_y, max_x, max_y))
    }
}

/// The parsed embedded face.
#[derive(Debug)]
pub struct Face {
    cmap: &'static [u8],
    glyf: &'static [u8],
    loca: &'static [u8],
    hmtx: &'static [u8],
    num_glyphs: u16,
    num_h_metrics: u16,
    loca_long: bool,
    /// Font design units per em.
    pub units_per_em: u16,
    /// Distance from baseline to the top of the ascender, in font units.
    pub ascender: i16,
    /// Distance from baseline to the bottom of the descender, in font units.
    pub descender: i16,
    /// Recommended extra line gap, in font units.
    pub line_gap: i16,
}

impl Face {
    /// Returns the shared, lazily parsed embedded face.
    pub fn embedded() -> &'static Face {
        static FACE: OnceLock<Face> = OnceLock::new();
        FACE.get_or_init(|| {
            let head = table(FONT_BYTES, b"head").expect("embedded font has a head table");
            let hhea = table(FONT_BYTES, b"hhea").expect("embedded font has an hhea table");
            let maxp = table(FONT_BYTES, b"maxp").expect("embedded font has a maxp table");
            Face {
                cmap: cmap_subtable(
                    table(FONT_BYTES, b"cmap").expect("embedded font has a cmap table"),
                ),
                glyf: table(FONT_BYTES, b"glyf").expect("embedded font has a glyf table"),
                loca: table(FONT_BYTES, b"loca").expect("embedded font has a loca table"),
                hmtx: table(FONT_BYTES, b"hmtx").expect("embedded font has an hmtx table"),
                num_glyphs: u16_at(maxp, 4),
                num_h_metrics: u16_at(hhea, 34),
                loca_long: i16_at(head, 50) == 1,
                units_per_em: u16_at(head, 18),
                ascender: i16_at(hhea, 4),
                descender: i16_at(hhea, 6),
                line_gap: i16_at(hhea, 8),
            }
        })
    }

    /// Glyph id for a Unicode scalar, or 0 (`.notdef`) when unsupported.
    pub fn glyph_index(&self, c: char) -> u16 {
        let cp = c as u32;
        let format = u16_at(self.cmap, 0);
        match format {
            12 => self.glyph_index_format12(cp),
            4 => self.glyph_index_format4(cp),
            _ => 0,
        }
    }

    fn glyph_index_format12(&self, cp: u32) -> u16 {
        let groups = u32_at(self.cmap, 12) as usize;
        for index in 0..groups {
            let at = 16 + index * 12;
            let start = u32_at(self.cmap, at);
            let end = u32_at(self.cmap, at + 4);
            if (start..=end).contains(&cp) {
                let glyph = u32_at(self.cmap, at + 8) + (cp - start);
                return u16::try_from(glyph).unwrap_or(0);
            }
        }
        0
    }

    fn glyph_index_format4(&self, cp: u32) -> u16 {
        if cp > 0xffff {
            return 0;
        }
        let code = cp as u16;
        let segments = (u16_at(self.cmap, 6) as usize) / 2;
        let ends = 14;
        let starts = ends + segments * 2 + 2;
        let deltas = starts + segments * 2;
        let ranges = deltas + segments * 2;
        for index in 0..segments {
            let end = u16_at(self.cmap, ends + index * 2);
            if code > end {
                continue;
            }
            let start = u16_at(self.cmap, starts + index * 2);
            if code < start {
                return 0;
            }
            let delta = u16_at(self.cmap, deltas + index * 2);
            let range = u16_at(self.cmap, ranges + index * 2) as usize;
            if range == 0 {
                return code.wrapping_add(delta);
            }
            let address = ranges + index * 2 + range + (code as usize - start as usize) * 2;
            let glyph = u16_at(self.cmap, address);
            return if glyph == 0 {
                0
            } else {
                glyph.wrapping_add(delta)
            };
        }
        0
    }

    /// Advance width of a glyph id in font units.
    pub fn advance(&self, glyph: u16) -> u16 {
        let index = glyph as usize;
        if index < self.num_h_metrics as usize {
            u16_at(self.hmtx, index * 4)
        } else if self.num_h_metrics > 0 {
            u16_at(self.hmtx, (self.num_h_metrics as usize - 1) * 4)
        } else {
            0
        }
    }

    /// Resolves a character into contours, expanding composite glyphs.
    pub fn outline(&self, c: char) -> OutlineGlyph {
        let glyph = self.glyph_index(c);
        self.outline_by_index(glyph, 0)
            .unwrap_or_else(|| OutlineGlyph::blank(self.advance(glyph)))
    }

    fn outline_by_index(&self, glyph: u16, depth: u8) -> Option<OutlineGlyph> {
        if glyph >= self.num_glyphs || depth > 8 {
            return None;
        }
        let start = self.loca_offset(glyph);
        let end = self.loca_offset(glyph + 1);
        if start >= end || end > self.glyf.len() {
            return None;
        }
        let data = &self.glyf[start..end];
        let contours = i16_at(data, 0);
        let advance = self.advance(glyph);
        if contours >= 0 {
            Some(parse_simple(data, contours as usize, advance))
        } else {
            Some(self.parse_composite(data, advance, depth))
        }
    }

    fn loca_offset(&self, glyph: u16) -> usize {
        if self.loca_long {
            u32_at(self.loca, glyph as usize * 4) as usize
        } else {
            u16_at(self.loca, glyph as usize * 2) as usize * 2
        }
    }

    fn parse_composite(&self, data: &[u8], advance: u16, depth: u8) -> OutlineGlyph {
        let mut at = 10; // numberOfContours + xMin/yMin/xMax/yMax
        let mut contours = Vec::new();
        let mut more = true;
        while more && at + 4 <= data.len() {
            let flags = u16_at(data, at);
            let child = u16_at(data, at + 2);
            at += 4;
            let (arg1, arg2) = if flags & 0x0001 != 0 {
                let pair = (i16_at(data, at), i16_at(data, at + 2));
                at += 4;
                (pair.0 as f32, pair.1 as f32)
            } else {
                let pair = (
                    data.get(at).copied().unwrap_or(0),
                    data.get(at + 1).copied().unwrap_or(0),
                );
                at += 2;
                (pair.0 as i8 as f32, pair.1 as i8 as f32)
            };
            // Point-matched components (flag 0x0002 clear) are rare in this
            // face; treating them as unshifted keeps the outline well-formed.
            let (tx, ty) = if flags & 0x0002 != 0 {
                (arg1, arg2)
            } else {
                (0.0, 0.0)
            };
            let (a, b, c, d) = if flags & 0x0008 != 0 {
                let scale = f2dot14(data, at);
                at += 2;
                (scale, 0.0, 0.0, scale)
            } else if flags & 0x0040 != 0 {
                let sx = f2dot14(data, at);
                let sy = f2dot14(data, at + 2);
                at += 4;
                (sx, 0.0, 0.0, sy)
            } else if flags & 0x0080 != 0 {
                let values = (
                    f2dot14(data, at),
                    f2dot14(data, at + 2),
                    f2dot14(data, at + 4),
                    f2dot14(data, at + 6),
                );
                at += 8;
                values
            } else {
                (1.0, 0.0, 0.0, 1.0)
            };
            if let Some(child_glyph) = self.outline_by_index(child, depth + 1) {
                contours.extend(child_glyph.contours.into_iter().map(|contour| {
                    contour
                        .into_iter()
                        .map(|point| {
                            let x = a * f32::from(point.x) + c * f32::from(point.y) + tx;
                            let y = b * f32::from(point.x) + d * f32::from(point.y) + ty;
                            OutlinePoint {
                                x: x.round() as i16,
                                y: y.round() as i16,
                                on_curve: point.on_curve,
                            }
                        })
                        .collect()
                }));
            }
            more = flags & 0x0020 != 0;
        }
        OutlineGlyph { advance, contours }
    }
}

fn parse_simple(data: &[u8], count: usize, advance: u16) -> OutlineGlyph {
    let mut at = 10;
    let mut ends = Vec::with_capacity(count);
    for _ in 0..count {
        ends.push(u16_at(data, at) as usize);
        at += 2;
    }
    let instructions = u16_at(data, at) as usize;
    at += 2 + instructions;
    let total = ends.last().map_or(0, |last| last + 1);
    let mut flags = Vec::with_capacity(total);
    while flags.len() < total && at < data.len() {
        let flag = data[at];
        at += 1;
        let repeat = if flag & 0x08 != 0 {
            let count = data.get(at).copied().unwrap_or(0) as usize;
            at += 1;
            count
        } else {
            0
        };
        flags.extend(std::iter::repeat_n(flag, repeat + 1));
    }
    flags.truncate(total);

    let mut points = Vec::with_capacity(total);
    let mut x = 0_i16;
    for &flag in &flags {
        if flag & 0x02 != 0 {
            let delta = data.get(at).copied().unwrap_or(0) as i16;
            at += 1;
            x = x.wrapping_add(if flag & 0x10 != 0 { delta } else { -delta });
        } else if flag & 0x10 == 0 {
            x = x.wrapping_add(i16_at(data, at));
            at += 2;
        }
        points.push((x, 0_i16, flag & 0x01 != 0));
    }
    let mut y = 0_i16;
    for (index, &flag) in flags.iter().enumerate() {
        if flag & 0x04 != 0 {
            let delta = data.get(at).copied().unwrap_or(0) as i16;
            at += 1;
            y = y.wrapping_add(if flag & 0x20 != 0 { delta } else { -delta });
        } else if flag & 0x20 == 0 {
            y = y.wrapping_add(i16_at(data, at));
            at += 2;
        }
        points[index].1 = y;
    }

    let mut contours = Vec::with_capacity(count);
    let mut from = 0;
    for to in ends {
        if to >= total {
            break;
        }
        contours.push(
            points[from..=to]
                .iter()
                .map(|&(x, y, on_curve)| OutlinePoint { x, y, on_curve })
                .collect(),
        );
        from = to + 1;
    }
    OutlineGlyph { advance, contours }
}

/// Selects the Unicode subtable from the `cmap` table. Format 12 is preferred
/// because it maps the full scalar range with simple start/end groups; format 4
/// is kept as a fallback.
fn cmap_subtable(cmap: &'static [u8]) -> &'static [u8] {
    let count = u16_at(cmap, 2) as usize;
    let mut fallback: &'static [u8] = &[];
    for index in 0..count {
        let at = 4 + index * 8;
        let platform = u16_at(cmap, at);
        let encoding = u16_at(cmap, at + 2);
        let offset = u32_at(cmap, at + 4) as usize;
        let Some(sub) = cmap.get(offset..) else {
            continue;
        };
        match u16_at(sub, 0) {
            12 if (platform == 3 && encoding == 10) || platform == 0 => return sub,
            4 if (platform == 3 && encoding == 1) || platform == 0 => fallback = sub,
            _ => {}
        }
    }
    fallback
}

fn table<'a>(data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let count = u16_at(data, 4) as usize;
    for index in 0..count {
        let at = 12 + index * 16;
        if data.get(at..at + 4) == Some(tag.as_slice()) {
            let start = u32_at(data, at + 8) as usize;
            let len = u32_at(data, at + 12) as usize;
            return data.get(start..start.saturating_add(len));
        }
    }
    None
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    data.get(at..at + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_be_bytes)
        .unwrap_or(0)
}

fn i16_at(data: &[u8], at: usize) -> i16 {
    u16_at(data, at) as i16
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    data.get(at..at + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
        .unwrap_or(0)
}

fn f2dot14(data: &[u8], at: usize) -> f32 {
    f32::from(i16_at(data, at)) / 16384.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_face_has_sane_metrics() {
        let face = Face::embedded();
        assert_eq!(face.units_per_em, 2816);
        assert!(face.ascender > 0 && face.descender < 0);
        assert!(face.num_glyphs > 1000);
    }

    #[test]
    fn ascii_and_accents_have_outlines() {
        for ch in ['A', 'g', '1', 'ç', 'ã', 'ó', '—', '…'] {
            let glyph = Face::embedded().outline(ch);
            assert!(!glyph.is_empty(), "missing outline for {ch:?}");
        }
    }

    #[test]
    fn space_is_blank_but_advances() {
        let face = Face::embedded();
        let space = face.outline(' ');
        assert!(space.is_empty());
        assert!(space.advance > 0);
    }

    #[test]
    fn proportional_advances_differ() {
        let face = Face::embedded();
        assert!(face.outline('i').advance < face.outline('M').advance);
    }
}
