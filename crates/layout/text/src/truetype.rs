//! Minimal TrueType/OpenType reader for the embedded Inter face.
//!
//! The tables needed to turn a Unicode scalar into a set of contours are read:
//! `head`, `hhea`, `hmtx`, `maxp`, `cmap`, `loca` and `glyf`. Composite glyphs
//! (accents such as `ã` and `ç`) are expanded recursively. No hinting is
//! applied — the anti-aliased rasterizer works directly from the font units.
//!
//! Kerning is read from a legacy `kern` table when one exists, otherwise from
//! the `GPOS` table's PairPos lookups (format 1 and 2, including lookups wrapped
//! in an Extension subtable — the Inter face uses the latter). Standard
//! ligatures are collected from the `GSUB` table's LigatureSubst (type 4)
//! lookups. Both are resolved to glyph ids once, when the face is parsed.
//!
//! The font itself is embedded in [`crates/layout/text/fonts`] under the SIL
//! Open Font License; see `Inter-LICENSE.txt` next to it.

use std::collections::HashMap;
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
    /// Pair kerning adjustments, keyed by (first glyph, second glyph), in font
    /// units. The value is added to the first glyph's advance.
    kern: HashMap<(u16, u16), i16>,
    /// Ligature substitutions, keyed by the ordered component glyph ids.
    ligatures: HashMap<Vec<u16>, u16>,
}

impl Face {
    /// Returns the shared, lazily parsed embedded face.
    pub fn embedded() -> &'static Face {
        static FACE: OnceLock<Face> = OnceLock::new();
        FACE.get_or_init(|| {
            let head = table(FONT_BYTES, b"head").expect("embedded font has a head table");
            let hhea = table(FONT_BYTES, b"hhea").expect("embedded font has an hhea table");
            let maxp = table(FONT_BYTES, b"maxp").expect("embedded font has a maxp table");
            let num_glyphs = u16_at(maxp, 4);
            // Prefer the legacy `kern` table; Inter has none, so its kerning is
            // read from GPOS PairPos lookups instead.
            let kern = table(FONT_BYTES, b"kern")
                .map(parse_kern_table)
                .filter(|pairs| !pairs.is_empty())
                .unwrap_or_else(|| {
                    table(FONT_BYTES, b"GPOS")
                        .map(|gpos| parse_gpos_kern(gpos, num_glyphs))
                        .unwrap_or_default()
                });
            let ligatures = table(FONT_BYTES, b"GSUB")
                .map(parse_gsub_ligatures)
                .unwrap_or_default();
            Face {
                cmap: cmap_subtable(
                    table(FONT_BYTES, b"cmap").expect("embedded font has a cmap table"),
                ),
                glyf: table(FONT_BYTES, b"glyf").expect("embedded font has a glyf table"),
                loca: table(FONT_BYTES, b"loca").expect("embedded font has a loca table"),
                hmtx: table(FONT_BYTES, b"hmtx").expect("embedded font has an hmtx table"),
                num_glyphs,
                num_h_metrics: u16_at(hhea, 34),
                loca_long: i16_at(head, 50) == 1,
                units_per_em: u16_at(head, 18),
                ascender: i16_at(hhea, 4),
                descender: i16_at(hhea, 6),
                line_gap: i16_at(hhea, 8),
                kern,
                ligatures,
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

    /// Kerning adjustment, in font units, applied to the advance of `a` when
    /// followed by `b`. Both arguments are glyph ids; 0 means no adjustment.
    pub fn kerning_units(&self, a: u16, b: u16) -> i16 {
        self.kern.get(&(a, b)).copied().unwrap_or(0)
    }

    /// Ligature glyph for an ordered sequence of component glyph ids.
    pub fn ligature(&self, gids: &[u16]) -> Option<u16> {
        self.ligatures.get(gids).copied()
    }

    /// Resolves a character into contours, expanding composite glyphs.
    pub fn outline(&self, c: char) -> OutlineGlyph {
        let glyph = self.glyph_index(c);
        self.outline_by_index(glyph, 0)
            .unwrap_or_else(|| OutlineGlyph::blank(self.advance(glyph)))
    }

    /// Resolves a glyph id directly into contours (used to verify that a
    /// fallback ligature glyph actually has an outline).
    pub fn outline_gid(&self, gid: u16) -> Option<OutlineGlyph> {
        self.outline_by_index(gid, 0)
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

// --- Kerning (legacy `kern` + GPOS) and ligature (GSUB) parsing ------------

/// Reads a `kern` table (version 0, format 0 horizontal pairs). Returns an
/// empty map for any other format; callers fall back to GPOS in that case.
fn parse_kern_table(kern: &[u8]) -> HashMap<(u16, u16), i16> {
    let mut out = HashMap::new();
    if u16_at(kern, 0) != 0 {
        return out;
    }
    let tables = u16_at(kern, 2) as usize;
    let mut at = 4;
    for _ in 0..tables {
        if at + 6 > kern.len() {
            break;
        }
        let length = u16_at(kern, at + 2) as usize;
        let coverage = u16_at(kern, at + 4);
        if coverage & 0x0001 != 0 && coverage >> 8 == 0 && length >= 14 {
            let pairs = u16_at(kern, at + 6) as usize;
            let mut p = at + 14; // 6-byte header + searchRange/entrySelector/rangeShift
            for _ in 0..pairs {
                if p + 6 > kern.len() {
                    break;
                }
                let value = i16_at(kern, p + 4);
                if value != 0 {
                    out.insert((u16_at(kern, p), u16_at(kern, p + 2)), value);
                }
                p += 6;
            }
        }
        if length == 0 {
            break;
        }
        at += length;
    }
    out
}

/// Reads all GPOS PairPos (type 2) lookups, including those wrapped in an
/// Extension (type 9) subtable, into a (first glyph, second glyph) -> XAdvance
/// adjustment map. Only the first value record's XAdvance is used, which is
/// what harfbuzz/Blink apply as kerning to the first glyph of the pair.
fn parse_gpos_kern(gpos: &[u8], num_glyphs: u16) -> HashMap<(u16, u16), i16> {
    let mut out = HashMap::new();
    // The header's lookupListOffset is relative to the table start.
    let list = u16_at(gpos, 8) as usize;
    let count = u16_at(gpos, list) as usize;
    for i in 0..count {
        let Some(lookup) = rel_offset(gpos, list, list + 2 + i * 2) else {
            continue;
        };
        let kind = u16_at(gpos, lookup);
        let sub_count = u16_at(gpos, lookup + 4) as usize;
        for j in 0..sub_count {
            let Some(sub) = rel_offset(gpos, lookup, lookup + 6 + j * 2) else {
                continue;
            };
            match kind {
                2 => parse_pairpos(gpos, sub, num_glyphs, &mut out),
                9 if u16_at(gpos, sub) == 1 && u16_at(gpos, sub + 2) == 2 => {
                    // Extension lookup: extensionLookupType == 2 (PairPos),
                    // extensionOffset is measured from the extension subtable.
                    let offset = u32_at(gpos, sub + 4) as usize;
                    if let Some(actual) = sub.checked_add(offset) {
                        parse_pairpos(gpos, actual, num_glyphs, &mut out);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Reads one PairPos subtable (format 1 or 2) into `out`.
fn parse_pairpos(data: &[u8], st: usize, num_glyphs: u16, out: &mut HashMap<(u16, u16), i16>) {
    let format = u16_at(data, st);
    let Some(coverage) = rel_offset(data, st, st + 2) else {
        return;
    };
    let vf1 = u16_at(data, st + 4);
    let vf2 = u16_at(data, st + 6);
    let len1 = value_record_len(vf1);
    let len2 = value_record_len(vf2);
    let first_glyphs = coverage_gids(data, coverage);

    if format == 1 {
        let sets = u16_at(data, st + 8) as usize;
        for (ci, &first) in first_glyphs.iter().enumerate() {
            if ci >= sets {
                break;
            }
            let Some(set) = rel_offset(data, st, st + 10 + ci * 2) else {
                continue;
            };
            let count = u16_at(data, set) as usize;
            let mut at = set + 2;
            for _ in 0..count {
                if at + 2 + len1 + len2 > data.len() {
                    break;
                }
                let second = u16_at(data, at);
                let kern = value_x_advance(data, at + 2, vf1);
                if kern != 0 {
                    out.insert((first, second), kern);
                }
                at += 2 + len1 + len2;
            }
        }
    } else if format == 2 {
        let Some(class_def1) = rel_offset(data, st, st + 8) else {
            return;
        };
        let Some(class_def2) = rel_offset(data, st, st + 10) else {
            return;
        };
        let class1_count = u16_at(data, st + 12) as usize;
        let class2_count = u16_at(data, st + 14) as usize;
        let record_len = len1 + len2;
        if record_len == 0 || class2_count == 0 {
            return;
        }
        // ClassDef2 maps a glyph to a class; invert it so each class-2 column
        // can be expanded back into concrete glyph ids.
        let mut class2_glyphs: HashMap<u16, Vec<u16>> = HashMap::new();
        for gid in 0..num_glyphs {
            let class = class_value(data, class_def2, gid);
            class2_glyphs.entry(class).or_default().push(gid);
        }
        let base = st + 16;
        for &first in &first_glyphs {
            let class1 = class_value(data, class_def1, first) as usize;
            if class1 >= class1_count {
                continue;
            }
            for class2 in 0..class2_count {
                let at = base + (class1 * class2_count + class2) * record_len;
                if at + len1 > data.len() {
                    break;
                }
                let kern = value_x_advance(data, at, vf1);
                if kern == 0 {
                    continue;
                }
                if let Some(seconds) = class2_glyphs.get(&(class2 as u16)) {
                    for &second in seconds {
                        out.insert((first, second), kern);
                    }
                }
            }
        }
    }
}

/// Reads GSUB LigatureSubst (type 4) lookups into a component-sequence map.
fn parse_gsub_ligatures(gsub: &[u8]) -> HashMap<Vec<u16>, u16> {
    let mut out = HashMap::new();
    // Header offset is relative to the table start, not the offset field.
    let list = u16_at(gsub, 8) as usize;
    let count = u16_at(gsub, list) as usize;
    for i in 0..count {
        let Some(lookup) = rel_offset(gsub, list, list + 2 + i * 2) else {
            continue;
        };
        if u16_at(gsub, lookup) != 4 {
            continue;
        }
        let sub_count = u16_at(gsub, lookup + 4) as usize;
        for j in 0..sub_count {
            let Some(sub) = rel_offset(gsub, lookup, lookup + 6 + j * 2) else {
                continue;
            };
            if u16_at(gsub, sub) != 1 {
                continue;
            }
            let Some(coverage) = rel_offset(gsub, sub, sub + 2) else {
                continue;
            };
            let set_count = u16_at(gsub, sub + 4) as usize;
            let first_glyphs = coverage_gids(gsub, coverage);
            for (ci, &first) in first_glyphs.iter().enumerate() {
                if ci >= set_count {
                    break;
                }
                let Some(set) = rel_offset(gsub, sub, sub + 6 + ci * 2) else {
                    continue;
                };
                let ligatures = u16_at(gsub, set) as usize;
                for k in 0..ligatures {
                    let Some(lig) = rel_offset(gsub, set, set + 2 + k * 2) else {
                        continue;
                    };
                    let lig_glyph = u16_at(gsub, lig);
                    let components = u16_at(gsub, lig + 2) as usize;
                    if components < 2 {
                        continue;
                    }
                    let mut sequence = vec![first];
                    for m in 0..components - 1 {
                        sequence.push(u16_at(gsub, lig + 4 + m * 2));
                    }
                    out.insert(sequence, lig_glyph);
                }
            }
        }
    }
    out
}

/// Resolves a u16 offset stored at `field` relative to the start of the
/// containing structure `base` (OpenType offsets are structure-relative).
fn rel_offset(data: &[u8], base: usize, field: usize) -> Option<usize> {
    let offset = u16_at(data, field) as usize;
    base.checked_add(offset).filter(|&end| end <= data.len())
}

/// Expands an OpenType Coverage table into the list of glyph ids it covers.
fn coverage_gids(data: &[u8], cov: usize) -> Vec<u16> {
    let mut gids = Vec::new();
    match u16_at(data, cov) {
        1 => {
            let count = u16_at(data, cov + 2) as usize;
            for i in 0..count {
                gids.push(u16_at(data, cov + 4 + i * 2));
            }
        }
        2 => {
            let ranges = u16_at(data, cov + 2) as usize;
            for i in 0..ranges {
                let at = cov + 4 + i * 6;
                let start = u16_at(data, at);
                let end = u16_at(data, at + 2);
                for gid in start..=end {
                    gids.push(gid);
                }
            }
        }
        _ => {}
    }
    gids
}

/// Returns the class assigned to `gid` by a ClassDef table.
fn class_value(data: &[u8], class_def: usize, gid: u16) -> u16 {
    match u16_at(data, class_def) {
        1 => {
            let start = u16_at(data, class_def + 2);
            let count = u16_at(data, class_def + 4) as usize;
            if gid >= start && ((gid - start) as usize) < count {
                u16_at(data, class_def + 6 + (gid - start) as usize * 2)
            } else {
                0
            }
        }
        2 => {
            let ranges = u16_at(data, class_def + 2) as usize;
            for i in 0..ranges {
                let at = class_def + 4 + i * 6;
                if gid >= u16_at(data, at) && gid <= u16_at(data, at + 2) {
                    return u16_at(data, at + 4);
                }
            }
            0
        }
        _ => 0,
    }
}

/// Size in bytes of a ValueRecord with the given format bitmask.
fn value_record_len(format: u16) -> usize {
    (0..8).filter(|&bit| format & (1 << bit) != 0).count() * 2
}

/// Reads the XAdvance field of a ValueRecord (the third optional field).
fn value_x_advance(data: &[u8], at: usize, format: u16) -> i16 {
    let mut pos = at;
    if format & 0x0001 != 0 {
        pos += 2; // XPlacement
    }
    if format & 0x0002 != 0 {
        pos += 2; // YPlacement
    }
    if format & 0x0004 != 0 {
        i16_at(data, pos) // XAdvance
    } else {
        0
    }
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

    #[test]
    fn gpos_kerning_is_parsed() {
        let face = Face::embedded();
        let a = face.glyph_index('A');
        let v = face.glyph_index('V');
        let t = face.glyph_index('T');
        let o = face.glyph_index('o');
        // Cross-checked against harfbuzz on this exact font (font units):
        // "AV" -> -192, "To" -> -224 (both tighten the first glyph's advance).
        assert_eq!(face.kerning_units(a, v), -192);
        assert_eq!(face.kerning_units(t, o), -224);
        assert_eq!(face.kerning_units(a, face.glyph_index('a')), 0);
        assert_eq!(face.kerning_units(0, 0), 0);
    }

    #[test]
    fn gsub_ligatures_are_parsed() {
        let face = Face::embedded();
        // This Inter build has no `liga` feature, so the classic f-ligatures
        // are absent; the parser must still handle the many ligatures it does
        // contain (e.g. the Arabic lam-alef forms are stored as type-4 ligs).
        let f = face.glyph_index('f');
        let i = face.glyph_index('i');
        assert_eq!(face.ligature(&[f, i]), None);
        // A ligature the face does define: the "No." ordinal abbreviation.
        let sequence: Vec<u16> = "No.".chars().map(|ch| face.glyph_index(ch)).collect();
        assert!(face.ligature(&sequence).is_some());
    }
}
