//! Software rasterizer: executes the display list into an RGBA buffer.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use browser_css::Color;
use browser_layout::Size;
use browser_layout::box_model::Rect;
use browser_style::BorderStyle;
use browser_text::RasterGlyph;

use crate::display_list::{DisplayCommand, DisplayFilter, DisplayList};
use crate::effects::{
    BlendMode, BorderImageData, BorderImageRepeat, ClipPath, ConicGradient, MaskData,
    RadialGradient, rasterize_clip_path, sample_conic, sample_radial,
};
use crate::images::{ImageData, clamp_dimension};
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
    ///
    /// Dimensions larger than [`crate::images::MAX_IMAGE_DIMENSION`] are clamped
    /// before allocating, matching [`ImageData::new`].
    pub fn new(width: u32, height: u32) -> Self {
        let width = clamp_dimension(width);
        let height = clamp_dimension(height);
        Self {
            width,
            height,
            pixels: vec![
                0;
                (width as usize)
                    .saturating_mul(height as usize)
                    .saturating_mul(4)
            ],
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

/// Number of sub-samples per axis used for anti-aliased coverage (4x4 = 16 taps).
const AA_SUBSAMPLES: usize = 4;

/// Global per-frame budget of expensive samples across the shadow, transform and
/// filter paths. Once it is exhausted those effects fall back to cheaper drawing
/// so a pathological page can never hang the rasterizer. Temp rasterizers share
/// the same counter, making the cap truly per-frame rather than per-effect.
const FRAME_WORK_BUDGET: u64 = 20_000_000;

/// A shared, atomic per-frame work budget. `Arc` keeps `Rasterizer` `Send` while
/// allowing nested effect rasterizers to draw from the same pool.
#[derive(Clone)]
struct WorkBudget(Arc<AtomicU64>);

impl WorkBudget {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(FRAME_WORK_BUDGET)))
    }

    /// Atomically reserves `cost` units, returning `false` (without consuming
    /// anything) when fewer units remain.
    fn try_consume(&self, cost: u64) -> bool {
        let mut remaining = self.0.load(Ordering::Relaxed);
        loop {
            if cost > remaining {
                return false;
            }
            match self.0.compare_exchange_weak(
                remaining,
                remaining - cost,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => remaining = actual,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Optional per-command profiling, enabled by `MAG_PAINT_PROFILE=1`.
//
// The profiler is deliberately self-contained: a global table of per-kind
// statistics plus a boolean carried on each rasterizer (so temporary
// rasterizers used by transforms/filters inherit it). Enabling it costs a
// couple of `Instant::now()` calls per command; when the flag is unset every
// hot loop and the normal render path are untouched.
// ---------------------------------------------------------------------------

/// Command categories reported by the profiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum ProfileKind {
    PushClip,
    PopClip,
    FillRect,
    DrawBorder,
    DrawText,
    DrawImage,
    DrawRoundedRect,
    DrawShadow,
    DrawLinearGradient,
    DrawTransformed,
    DrawFiltered,
    DrawMasked,
    DrawClipped,
    DrawBlended,
    DrawRadialGradient,
    DrawConicGradient,
    DrawImageWithRendering,
    DrawBorderImage,
}

impl ProfileKind {
    const COUNT: usize = 18;

    const NAMES: [&'static str; Self::COUNT] = [
        "PushClip",
        "PopClip",
        "FillRect",
        "DrawBorder",
        "DrawText",
        "DrawImage",
        "DrawRoundedRect",
        "DrawShadow",
        "DrawLinearGradient",
        "DrawTransformed",
        "DrawFiltered",
        "DrawMasked",
        "DrawClipped",
        "DrawBlended",
        "DrawRadialGradient",
        "DrawConicGradient",
        "DrawImageWithRendering",
        "DrawBorderImage",
    ];

    fn of(command: &DisplayCommand) -> Self {
        match command {
            DisplayCommand::PushClip(_) => Self::PushClip,
            DisplayCommand::PopClip => Self::PopClip,
            DisplayCommand::FillRect { .. } => Self::FillRect,
            DisplayCommand::DrawBorder { .. } => Self::DrawBorder,
            DisplayCommand::DrawText { .. } => Self::DrawText,
            DisplayCommand::DrawImage { .. } => Self::DrawImage,
            DisplayCommand::DrawRoundedRect { .. } => Self::DrawRoundedRect,
            DisplayCommand::DrawShadow { .. } => Self::DrawShadow,
            DisplayCommand::DrawLinearGradient { .. } => Self::DrawLinearGradient,
            DisplayCommand::DrawTransformed { .. } => Self::DrawTransformed,
            DisplayCommand::DrawFiltered { .. } => Self::DrawFiltered,
            DisplayCommand::DrawMasked { .. } => Self::DrawMasked,
            DisplayCommand::DrawClipped { .. } => Self::DrawClipped,
            DisplayCommand::DrawBlended { .. } => Self::DrawBlended,
            DisplayCommand::DrawRadialGradient { .. } => Self::DrawRadialGradient,
            DisplayCommand::DrawConicGradient { .. } => Self::DrawConicGradient,
            DisplayCommand::DrawImageWithRendering { .. } => Self::DrawImageWithRendering,
            DisplayCommand::DrawBorderImage { .. } => Self::DrawBorderImage,
        }
    }
}

/// Accumulated statistics for one command kind.
#[derive(Debug, Default, Clone, Copy)]
struct CommandStats {
    calls: u64,
    /// Pixels written or visited (inclusive of nested commands).
    pixels: u64,
    /// Wall-clock time in nanoseconds (inclusive of nested commands).
    nanos: u64,
}

static PAINT_PROFILE: OnceLock<Mutex<[CommandStats; ProfileKind::COUNT]>> = OnceLock::new();

fn paint_profile() -> &'static Mutex<[CommandStats; ProfileKind::COUNT]> {
    PAINT_PROFILE.get_or_init(|| Mutex::new([CommandStats::default(); ProfileKind::COUNT]))
}

// Text sub-phase timings, reset at the start of every profiled frame.
thread_local! {
    static TEXT_PHASES: Cell<[u64; 4]> = const { Cell::new([0; 4]) };
}

/// Process-wide key for the rasterizer's local glyph cache.
type GlyphKey = (char, u32, bool, bool);

static GLYPH_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static GLYPH_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static SLOW_GLYPHS: std::cell::RefCell<Vec<(char, f32, f64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Clamps per-corner radii so opposite corners never overlap, following the
/// CSS `border-radius` correction: every radius is scaled down by the same
/// factor until all radius pairs fit inside the box.
fn normalized_radii(rect: Rect, radius: [f32; 4]) -> [f32; 4] {
    let mut r = [
        radius[0].max(0.0),
        radius[1].max(0.0),
        radius[2].max(0.0),
        radius[3].max(0.0),
    ];
    let pairs = [
        (r[0] + r[1], rect.width),
        (r[2] + r[3], rect.width),
        (r[0] + r[3], rect.height),
        (r[1] + r[2], rect.height),
    ];
    let mut factor = 1.0_f32;
    for (sum, length) in pairs {
        if sum > length && length > 0.0 {
            factor = factor.min(length / sum);
        }
    }
    if factor < 1.0 {
        for value in &mut r {
            *value *= factor;
        }
    }
    r
}

/// Exact inside/outside test for a rounded rectangle at a single point.
fn rounded_rect_contains(rect: Rect, radius: [f32; 4], px: f32, py: f32) -> bool {
    let x = px - rect.x;
    let y = py - rect.y;
    if x < 0.0 || y < 0.0 || x > rect.width || y > rect.height {
        return false;
    }
    let [tl, tr, br, bl] = radius;
    if x < tl && y < tl {
        return (x - tl) * (x - tl) + (y - tl) * (y - tl) <= tl * tl;
    }
    if x > rect.width - tr && y < tr {
        let dx = x - (rect.width - tr);
        let dy = y - tr;
        return dx * dx + dy * dy <= tr * tr;
    }
    if x > rect.width - br && y > rect.height - br {
        let dx = x - (rect.width - br);
        let dy = y - (rect.height - br);
        return dx * dx + dy * dy <= br * br;
    }
    if x < bl && y > rect.height - bl {
        let dx = x - bl;
        let dy = y - (rect.height - bl);
        return dx * dx + dy * dy <= bl * bl;
    }
    true
}

/// 4x4 supersampled coverage of a rounded rectangle for one pixel.
///
/// The two fast paths return exactly the same value as the 16-tap loop: a
/// pixel whose sample box lies wholly outside the bounding box has coverage
/// zero, and one whose sample box is inside the box and in the "straight" band
/// of every corner has coverage one. Only pixels straddling a corner pay for
/// supersampling, which makes large mostly-flat rounded rectangles cheap.
fn rounded_rect_coverage(rect: Rect, radius: [f32; 4], x: i32, y: i32) -> f32 {
    let [tl, tr, br, bl] = radius;
    let px0 = x as f32;
    let py0 = y as f32;
    let px1 = px0 + 1.0;
    let py1 = py0 + 1.0;
    if px1 <= rect.x || px0 >= rect.right() || py1 <= rect.y || py0 >= rect.bottom() {
        return 0.0;
    }
    if px0 >= rect.x
        && py0 >= rect.y
        && px1 <= rect.right()
        && py1 <= rect.bottom()
        && (px0 >= rect.x + tl || py0 >= rect.y + tl)
        && (px1 <= rect.right() - tr || py0 >= rect.y + tr)
        && (px1 <= rect.right() - br || py1 <= rect.bottom() - br)
        && (px0 >= rect.x + bl || py1 <= rect.bottom() - bl)
    {
        return 1.0;
    }

    let mut inside = 0usize;
    for sy in 0..AA_SUBSAMPLES {
        let py = py0 + (sy as f32 + 0.5) / AA_SUBSAMPLES as f32;
        let y = py - rect.y;
        for sx in 0..AA_SUBSAMPLES {
            let px = px0 + (sx as f32 + 0.5) / AA_SUBSAMPLES as f32;
            let x = px - rect.x;
            // Same tests as `rounded_rect_contains`, with only the corner that
            // can possibly apply to this sample instead of all four.
            let covered = if x < 0.0 || y < 0.0 || x > rect.width || y > rect.height {
                false
            } else if x < tl && y < tl {
                (x - tl) * (x - tl) + (y - tl) * (y - tl) <= tl * tl
            } else if x > rect.width - tr && y < tr {
                let dx = x - (rect.width - tr);
                let dy = y - tr;
                dx * dx + dy * dy <= tr * tr
            } else if x > rect.width - br && y > rect.height - br {
                let dx = x - (rect.width - br);
                let dy = y - (rect.height - br);
                dx * dx + dy * dy <= br * br
            } else if x < bl && y > rect.height - bl {
                let dx = x - bl;
                let dy = y - (rect.height - bl);
                dx * dx + dy * dy <= bl * bl
            } else {
                true
            };
            if covered {
                inside += 1;
            }
        }
    }
    inside as f32 / (AA_SUBSAMPLES * AA_SUBSAMPLES) as f32
}

// ---------------------------------------------------------------------------
// Glyph coverage rasterization.
//
// `browser_text::rasterize_cached` computes coverage with a per-sample winding
// test: for every pixel it evaluates 16 sub-samples, each scanning every edge
// of every outline polygon. For a 100px heading that is hundreds of millions
// of edge tests and dominates real-page paint time.
//
// The implementation below produces *byte-identical* coverage but evaluates
// winding with a scanline: for each of the four sub-sample rows of a pixel row
// the (four or, for synthetic bold, twelve) x positions whose winding is needed
// are sorted once, and each crossing is folded into a difference array with one
// binary search. Crossings, sub-sample coordinates and the integer winding
// accumulation use exactly the same f32 expressions as the reference
// rasterizer, so `coverage` is identical for every glyph/size/style.
// ---------------------------------------------------------------------------

/// One outline edge in font units, with its winding direction.
#[derive(Debug, Clone, Copy)]
struct GlyphEdge {
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    direction: i32,
}

/// Flattened outline of one character, shared across all pixel sizes.
struct GlyphOutline {
    bounds: (i16, i16, i16, i16),
    edges: Vec<GlyphEdge>,
}

static GLYPH_OUTLINES: OnceLock<Mutex<HashMap<char, Option<Arc<GlyphOutline>>>>> = OnceLock::new();

/// Matches the reference rasterizer's `hypot` exactly (`sqrt(x² + y²)`).
#[inline]
fn glyph_hypot(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

/// Flattens one TrueType contour exactly as the text rasterizer does.
fn flatten_outline(contour: &[browser_text::OutlinePoint]) -> Vec<(f32, f32)> {
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
        let length = glyph_hypot(control.0 - from.0, control.1 - from.1)
            + glyph_hypot(end.0 - control.0, end.1 - control.1);
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

/// Returns the cached flattened outline for `ch`, or `None` for a blank glyph.
///
/// `browser_text` exposes a single embedded face (`Font::monospace` is an alias
/// for `Font::sans`), so keying only on the character is sufficient.
fn glyph_outline(font: &browser_text::Font, ch: char) -> Option<Arc<GlyphOutline>> {
    let cache = GLYPH_OUTLINES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(outline) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&ch)
    {
        return outline.clone();
    }
    let glyph = font.glyph(ch)?;
    let mut min_x = i16::MAX;
    let mut min_y = i16::MAX;
    let mut max_x = i16::MIN;
    let mut max_y = i16::MIN;
    for point in glyph.contours.iter().flatten() {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    let outline = (min_x <= max_x && min_y <= max_y).then(|| {
        let polygons: Vec<Vec<(f32, f32)>> = glyph
            .contours
            .iter()
            .map(|contour| flatten_outline(contour))
            .filter(|polygon| polygon.len() >= 3)
            .collect();
        let mut edges = Vec::new();
        for polygon in &polygons {
            let count = polygon.len();
            for index in 0..count {
                let (ax, ay) = polygon[index];
                let (bx, by) = polygon[(index + 1) % count];
                edges.push(GlyphEdge {
                    ax,
                    ay,
                    bx,
                    by,
                    direction: if by > ay { 1 } else { -1 },
                });
            }
        }
        Arc::new(GlyphOutline {
            bounds: (min_x, min_y, max_x, max_y),
            edges,
        })
    });
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(ch, outline.clone());
    outline
}

/// Coverage for one glyph, byte-identical to `browser_text::rasterize_cached`.
fn fast_rasterize_glyph(
    font: &browser_text::Font,
    ch: char,
    px: f32,
    bold: bool,
    italic: bool,
) -> Option<Arc<RasterGlyph>> {
    if px <= 0.0 || ch.is_control() {
        return None;
    }
    let outline = glyph_outline(font, ch)?;
    if outline.edges.is_empty() {
        return None;
    }
    let (min_x, min_y, max_x, max_y) = outline.bounds;
    let scale = px / font.units_per_em();
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

    let ascent = f32::from(font.face().ascender);
    let bold_step = 0.5 / scale;
    let samples: f32 = AA_SUBSAMPLES as f32;
    let mut coverage = vec![0_u8; width as usize * height as usize];
    // Bucket edges by the font-unit y of the sample rows so each scanline only
    // examines the handful of edges that can cross it, not every outline edge.
    const Y_BUCKETS: usize = 256;
    let fy_lowest = -(top as f32 + height as f32 - 0.125) / scale;
    let fy_highest = -(top as f32 + 0.125) / scale;
    let fy_span = fy_highest - fy_lowest;
    let bucket_of = |y: f32| -> usize {
        if fy_span <= 0.0 {
            0
        } else {
            (((y - fy_lowest) / fy_span) * (Y_BUCKETS - 1) as f32)
                .floor()
                .clamp(0.0, (Y_BUCKETS - 1) as f32) as usize
        }
    };
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); Y_BUCKETS];
    for (index, edge) in outline.edges.iter().enumerate() {
        let low = bucket_of(edge.ay.min(edge.by));
        let high = (bucket_of(edge.ay.max(edge.by)) + 1).min(Y_BUCKETS - 1);
        for bucket in buckets.iter_mut().take(high + 1).skip(low) {
            bucket.push(index);
        }
    }
    let mut fx_samples = [0.0_f32; AA_SUBSAMPLES];
    // Thresholds are the x positions whose winding the sub-samples need: one
    // per sub-sample normally, three per sub-sample for synthetic bold. They
    // are sorted once per sub-sample row; each polygon crossing then needs only
    // a binary search instead of being sorted with all other crossings.
    let mut thresholds: Vec<(f32, usize, u8)> = Vec::with_capacity(3 * AA_SUBSAMPLES);
    let mut differences: Vec<i32> = Vec::with_capacity(3 * AA_SUBSAMPLES + 1);
    let mut windings: Vec<i32> = Vec::with_capacity(3 * AA_SUBSAMPLES);

    for cy in 0..height {
        for cx in 0..width {
            let mut hits_total = 0_usize;
            for sy in 0..AA_SUBSAMPLES {
                let py = top as f32 + cy as f32 + (sy as f32 + 0.5) / samples;
                let fy = -py / scale;
                let italic_shift = if italic { (ascent - fy) * 0.20 } else { 0.0 };
                for (sx, fx) in fx_samples.iter_mut().enumerate() {
                    let px_at = left as f32 + cx as f32 + (sx as f32 + 0.5) / samples;
                    *fx = px_at / scale - italic_shift;
                }
                thresholds.clear();
                for (sx, &fx) in fx_samples.iter().enumerate() {
                    if bold {
                        thresholds.push((fx - bold_step, sx, 0));
                        thresholds.push((fx, sx, 1));
                        thresholds.push((fx + bold_step, sx, 2));
                    } else {
                        thresholds.push((fx, sx, 0));
                    }
                }
                thresholds.sort_by(|a, b| a.0.total_cmp(&b.0));
                differences.clear();
                differences.resize(thresholds.len() + 1, 0);
                // A crossing contributes its direction to every winding at a
                // threshold strictly smaller than it. Record that with a
                // difference array and take a prefix sum below. Only edges
                // bucketed at this scanline's y can possibly cross it.
                for &index in &buckets[bucket_of(fy)] {
                    let edge = outline.edges[index];
                    let (ax, ay, bx, by) = (edge.ax, edge.ay, edge.bx, edge.by);
                    if (ay <= fy && by > fy) || (by <= fy && ay > fy) {
                        let crossing = ax + (fy - ay) * (bx - ax) / (by - ay);
                        let direction = edge.direction;
                        let beyond = thresholds.partition_point(|t| t.0 < crossing);
                        differences[0] += direction;
                        differences[beyond] -= direction;
                    }
                }
                windings.clear();
                let mut winding = 0_i32;
                for delta in differences.iter().take(thresholds.len()) {
                    winding += *delta;
                    windings.push(winding);
                }
                let mut filled = [false; AA_SUBSAMPLES];
                for (index, &(_, sx, kind)) in thresholds.iter().enumerate() {
                    if windings[index] != 0 && (kind == 0 || !filled[sx]) {
                        filled[sx] = true;
                    }
                }
                hits_total += filled.iter().filter(|hit| **hit).count();
            }
            coverage[cy as usize * width as usize + cx as usize] =
                (hits_total * 255 / (AA_SUBSAMPLES * AA_SUBSAMPLES)) as u8;
        }
    }

    Some(Arc::new(RasterGlyph {
        width,
        height,
        left,
        top,
        coverage,
    }))
}

/// Interpolates a sorted list of gradient stops at parameter `t` (0.0 ..= 1.0).
fn sample_gradient(stops: &[(f32, Color)], t: f32) -> Color {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    if t >= stops[stops.len() - 1].0 {
        return stops[stops.len() - 1].1;
    }
    for i in 1..stops.len() {
        if t <= stops[i].0 {
            let (p0, c0) = stops[i - 1];
            let (p1, c1) = stops[i];
            let span = (p1 - p0).max(f32::EPSILON);
            let f = ((t - p0) / span).clamp(0.0, 1.0);
            let mix = |a: u8, b: u8| (a as f32 * (1.0 - f) + b as f32 * f).round() as u8;
            return Color::rgba(
                mix(c0.r, c1.r),
                mix(c0.g, c1.g),
                mix(c0.b, c1.b),
                mix(c0.a, c1.a),
            );
        }
    }
    stops[stops.len() - 1].1
}

/// Executes display lists into an [`ImageBuffer`].
///
/// Coordinates in the display list are CSS pixels. A device scale factor can
/// be applied so the same layout renders crisply at HiDPI resolutions without
/// changing the page's CSS viewport.
pub struct Rasterizer {
    canvas: ImageBuffer,
    clips: Vec<Rect>,
    scale: f32,
    /// Canvas intersected with every active clip, in device pixels.
    ///
    /// Because all clips are axis-aligned rectangles, their intersection is a
    /// single rectangle; recomputing it when clips change (instead of walking
    /// the clip stack for every pixel) removes clip iteration from inner loops.
    visible: Rect,
    /// Integer pixel bounds equivalent to `visible`: `[x0, x1) x [y0, y1)`.
    /// An integer pixel `(x, y)` is visible iff it lies in this range.
    visible_i: (i32, i32, i32, i32),
    /// Shared cap on expensive samples for this frame; also held by every temp
    /// rasterizer created for transforms and filters.
    work_budget: WorkBudget,
    /// True when `MAG_PAINT_PROFILE=1` was set for the current frame.
    profile_enabled: bool,
    /// Pixels counted for the command currently being drawn.
    profile_pixels: u64,
    /// Frame-local glyph cache, shared with effect rasterizers so the
    /// expensive outline rasterizer runs at most once per distinct glyph.
    glyph_cache: Arc<Mutex<HashMap<GlyphKey, Option<Arc<RasterGlyph>>>>>,
}

/// Integer pixel bounds equivalent to `rect`'s half-open area.
fn integer_visible_bounds(rect: Rect) -> (i32, i32, i32, i32) {
    if rect.is_empty() {
        return (0, 0, 0, 0);
    }
    let x0 = rect.x.ceil() as i32;
    let y0 = rect.y.ceil() as i32;
    let x1 = rect.right().ceil() as i32;
    let y1 = rect.bottom().ceil() as i32;
    (x0, y0, x1.max(x0), y1.max(y0))
}

impl Rasterizer {
    /// Creates a rasterizer with a filled background.
    pub fn new(viewport: Size, background: Color) -> Self {
        let width = viewport.width.max(1.0) as u32;
        let height = viewport.height.max(1.0) as u32;
        let canvas = ImageBuffer::filled(width, height, background);
        let visible = Rect::new(0.0, 0.0, canvas.width as f32, canvas.height as f32);
        let visible_i = integer_visible_bounds(visible);
        Self {
            canvas,
            clips: Vec::new(),
            scale: 1.0,
            visible,
            visible_i,
            work_budget: WorkBudget::new(),
            profile_enabled: false,
            profile_pixels: 0,
            glyph_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Scales every display-list coordinate and font size by `scale`.
    #[must_use]
    pub fn scaled(mut self, scale: f32) -> Self {
        self.scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self
    }

    /// Executes a display list.
    pub fn execute(mut self, list: &DisplayList) -> ImageBuffer {
        let profiling = std::env::var_os("MAG_PAINT_PROFILE").is_some();
        if profiling {
            for stats in paint_profile().lock().unwrap().iter_mut() {
                *stats = CommandStats::default();
            }
            TEXT_PHASES.with(|p| p.set([0; 4]));
            GLYPH_CACHE_HITS.store(0, Ordering::Relaxed);
            GLYPH_CACHE_MISSES.store(0, Ordering::Relaxed);
            SLOW_GLYPHS.with(|s| s.borrow_mut().clear());
            self.profile_enabled = true;
            self.profile_pixels = 0;
        }
        let started = profiling.then(Instant::now);
        self.execute_commands(&list.commands, &list.images);
        if let Some(started) = started {
            let total = started.elapsed();
            eprintln!(
                "[paint-profile] total raster: {:.3} ms, {} top-level commands",
                total.as_secs_f64() * 1000.0,
                list.commands.len()
            );
            let stats = *paint_profile().lock().unwrap();
            for (kind, stats) in stats.iter().enumerate() {
                eprintln!(
                    "[paint-profile] {:>18}: calls={:>5} pixels={:>10} time={:>9.3} ms",
                    ProfileKind::NAMES[kind],
                    stats.calls,
                    stats.pixels,
                    stats.nanos as f64 / 1_000_000.0
                );
            }
            let phases = TEXT_PHASES.with(|p| p.get());
            eprintln!(
                "[paint-profile]   text: measure={:.3} advance={:.3} rasterize={:.3} blend={:.3} ms",
                phases[0] as f64 / 1_000_000.0,
                phases[1] as f64 / 1_000_000.0,
                phases[2] as f64 / 1_000_000.0,
                phases[3] as f64 / 1_000_000.0
            );
            eprintln!(
                "[paint-profile]   glyph local cache: hits={} misses={}",
                GLYPH_CACHE_HITS.load(Ordering::Relaxed),
                GLYPH_CACHE_MISSES.load(Ordering::Relaxed)
            );
            SLOW_GLYPHS.with(|slow| {
                let mut slow = slow.borrow_mut();
                slow.sort_by(|a, b| b.2.total_cmp(&a.2));
                eprintln!("[paint-profile]   slowest glyph rasterizations:");
                for (ch, px, ms) in slow.iter().take(15) {
                    eprintln!("[paint-profile]     {:?} @ {}px: {:.3} ms", ch, px, ms);
                }
                let sum: f64 = slow.iter().map(|g| g.2).sum();
                eprintln!(
                    "[paint-profile]     total {} rasterizations: {:.3} ms",
                    slow.len(),
                    sum
                );
            });
        }
        self.canvas
    }

    /// Executes a slice of commands against the supplied images.
    ///
    /// This is used by nested effects (transforms and filters) which render
    /// their children into a temporary rasterizer before compositing.
    pub fn execute_commands(&mut self, commands: &[DisplayCommand], images: &[ImageData]) {
        for command in commands {
            self.draw(command, images);
        }
    }

    /// Executes one command.
    pub fn draw(&mut self, command: &DisplayCommand, images: &[ImageData]) {
        let timer = if self.profile_enabled {
            Some(Instant::now())
        } else {
            None
        };
        let previous_pixels = self.profile_pixels;
        self.profile_pixels = 0;
        match command {
            DisplayCommand::PushClip(rect) => {
                self.clips.push(self.scaled_rect(*rect));
                self.refresh_visible();
            }
            DisplayCommand::PopClip => {
                self.clips.pop();
                self.refresh_visible();
            }
            DisplayCommand::FillRect { rect, color } => {
                self.fill_rect(self.scaled_rect(*rect), *color);
            }
            DisplayCommand::DrawBorder {
                rect,
                widths,
                colors,
                styles,
            } => {
                let widths = browser_layout::Edges {
                    top: widths.top * self.scale,
                    right: widths.right * self.scale,
                    bottom: widths.bottom * self.scale,
                    left: widths.left * self.scale,
                };
                self.draw_border(self.scaled_rect(*rect), &widths, colors, styles);
            }
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
                x * self.scale,
                baseline * self.scale,
                text,
                font_size * self.scale,
                *color,
                *underline,
                *line_through,
                *bold,
                *italic,
            ),
            DisplayCommand::DrawImage { rect, image } => {
                if let Some(image) = images.get(*image) {
                    // Legacy DrawImage is nearest-neighbour; keep its bytes identical.
                    self.draw_image(self.scaled_rect(*rect), image, false);
                }
            }
            DisplayCommand::DrawRoundedRect {
                rect,
                radius,
                background,
                border,
            } => {
                let radius = [
                    radius[0] * self.scale,
                    radius[1] * self.scale,
                    radius[2] * self.scale,
                    radius[3] * self.scale,
                ];
                let border = border.map(|(color, width)| (color, width * self.scale));
                self.draw_rounded_rect(self.scaled_rect(*rect), radius, *background, border);
            }
            DisplayCommand::DrawShadow {
                rect,
                radius,
                offset,
                blur,
                color,
            } => {
                let radius = [
                    radius[0] * self.scale,
                    radius[1] * self.scale,
                    radius[2] * self.scale,
                    radius[3] * self.scale,
                ];
                self.draw_shadow(
                    self.scaled_rect(*rect),
                    radius,
                    (offset.0 * self.scale, offset.1 * self.scale),
                    blur * self.scale,
                    *color,
                );
            }
            DisplayCommand::DrawLinearGradient {
                rect,
                stops,
                angle_deg,
                radius,
            } => {
                let radius = [
                    radius[0] * self.scale,
                    radius[1] * self.scale,
                    radius[2] * self.scale,
                    radius[3] * self.scale,
                ];
                self.draw_linear_gradient(self.scaled_rect(*rect), stops, *angle_deg, radius);
            }
            DisplayCommand::DrawTransformed {
                child_commands,
                matrix,
                opacity,
            } => self.draw_transformed(child_commands, *matrix, *opacity, images),
            DisplayCommand::DrawFiltered {
                child_commands,
                filters,
            } => self.draw_filtered(child_commands, filters, images),
            DisplayCommand::DrawMasked {
                rect,
                child_commands,
                mask,
            } => self.draw_masked(self.scaled_rect(*rect), child_commands, mask, images),
            DisplayCommand::DrawClipped {
                child_commands,
                clip,
                rect,
            } => self.draw_clipped(child_commands, clip, *rect, images),
            DisplayCommand::DrawBlended {
                child_commands,
                blend_mode,
                backdrop,
            } => self.draw_blended(child_commands, *blend_mode, backdrop, images),
            DisplayCommand::DrawRadialGradient {
                rect,
                gradient,
                radius,
            } => {
                let radius = [
                    radius[0] * self.scale,
                    radius[1] * self.scale,
                    radius[2] * self.scale,
                    radius[3] * self.scale,
                ];
                self.draw_radial_gradient(self.scaled_rect(*rect), gradient, radius);
            }
            DisplayCommand::DrawConicGradient {
                rect,
                gradient,
                radius,
            } => {
                let radius = [
                    radius[0] * self.scale,
                    radius[1] * self.scale,
                    radius[2] * self.scale,
                    radius[3] * self.scale,
                ];
                self.draw_conic_gradient(self.scaled_rect(*rect), gradient, radius);
            }
            DisplayCommand::DrawImageWithRendering {
                rect,
                image,
                rendering,
            } => {
                if let Some(image) = images.get(*image) {
                    self.draw_image(self.scaled_rect(*rect), image, !rendering.is_pixelated());
                }
            }
            DisplayCommand::DrawBorderImage {
                rect,
                border_image,
                image,
            } => {
                if let Some(source) = images.get(*image) {
                    self.draw_border_image(self.scaled_rect(*rect), border_image, source);
                }
            }
        }
        if let Some(started) = timer {
            let elapsed = started.elapsed().as_nanos() as u64;
            let pixels = self.profile_pixels;
            self.profile_pixels = previous_pixels + pixels;
            let stats = &mut paint_profile().lock().unwrap()[ProfileKind::of(command) as usize];
            stats.calls += 1;
            stats.pixels += pixels;
            stats.nanos += elapsed;
        }
    }

    /// Adds `count` to the current command's pixel tally (profiling only).
    #[inline]
    fn count_pixels(&mut self, count: u64) {
        if self.profile_enabled {
            self.profile_pixels += count;
        }
    }

    fn scaled_rect(&self, rect: Rect) -> Rect {
        Rect::new(
            rect.x * self.scale,
            rect.y * self.scale,
            rect.width * self.scale,
            rect.height * self.scale,
        )
    }

    /// Recomputes the cached visible region after the clip stack changes.
    fn refresh_visible(&mut self) {
        let mut bounds = Rect::new(
            0.0,
            0.0,
            self.canvas.width as f32,
            self.canvas.height as f32,
        );
        for clip in &self.clips {
            if clip.is_empty() {
                bounds = Rect::ZERO;
                break;
            }
            bounds = bounds.intersect(clip);
        }
        self.visible = bounds;
        self.visible_i = integer_visible_bounds(bounds);
    }

    /// The region, in device pixels, where painting can have an effect: the
    /// canvas intersected with every active clip.
    fn visible_bounds(&self) -> Rect {
        self.visible
    }

    /// Integer pixel loop bounds for `rect`, clamped to the visible region.
    ///
    /// Returns `None` when `rect` lies entirely outside the canvas or clips, so
    /// callers can skip all per-pixel work. The bounds are deliberately a
    /// conservative superset (floored lows, ceiled highs) of the pixels that
    /// anti-aliased shapes can touch — the per-pixel integer bounds check in
    /// [`Self::blend_components`] remains authoritative, so clipping behaviour
    /// is unchanged while huge off-screen rects no longer drive wasted work.
    fn clipped_pixel_bounds(&self, rect: Rect) -> Option<(i32, i32, i32, i32)> {
        if rect.is_empty() || self.visible.is_empty() {
            return None;
        }
        let clipped = rect.intersect(&self.visible);
        let x0 = clipped.x.floor() as i32;
        let y0 = clipped.y.floor() as i32;
        let x1 = clipped.right().ceil() as i32;
        let y1 = clipped.bottom().ceil() as i32;
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// Like [`Self::clipped_pixel_bounds`], but tightened to the integer visible
    /// bounds so every pixel in the returned range is guaranteed visible.
    /// Solid fills can therefore skip the per-pixel bounds check entirely.
    fn solid_pixel_bounds(&self, rect: Rect) -> Option<(i32, i32, i32, i32)> {
        let (x0, y0, x1, y1) = self.clipped_pixel_bounds(rect)?;
        let (vx0, vy0, vx1, vy1) = self.visible_i;
        let x0 = x0.max(vx0);
        let y0 = y0.max(vy0);
        let x1 = x1.min(vx1);
        let y1 = y1.min(vy1);
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// Blends a straight-RGBA colour with effective alpha `alpha` (0.0 ..= 1.0)
    /// into a single device pixel.
    ///
    /// The clip/canvas test is a pair of integer comparisons against the cached
    /// [`Self::visible_i`] bounds instead of a walk over the clip stack, and the
    /// canvas is written through a slice so no per-pixel `Rect` work is needed.
    #[inline]
    fn blend_components(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let (vx0, vy0, vx1, vy1) = self.visible_i;
        if x < vx0 || x >= vx1 || y < vy0 || y >= vy1 {
            return;
        }
        let index = ((y as usize) * (self.canvas.width as usize) + (x as usize)) * 4;
        let pixels = &mut self.canvas.pixels;
        if alpha >= 1.0 {
            pixels[index] = r;
            pixels[index + 1] = g;
            pixels[index + 2] = b;
        } else {
            let inverse = 1.0 - alpha;
            pixels[index] = (r as f32 * alpha + pixels[index] as f32 * inverse).round() as u8;
            pixels[index + 1] =
                (g as f32 * alpha + pixels[index + 1] as f32 * inverse).round() as u8;
            pixels[index + 2] =
                (b as f32 * alpha + pixels[index + 2] as f32 * inverse).round() as u8;
        }
        pixels[index + 3] = 255;
    }

    /// Blends `color` into a pixel with fractional `coverage` (0.0 ..= 1.0).
    ///
    /// This is the basis of anti-aliasing: a pixel straddling a shape edge is
    /// composited only partially, producing intermediate values instead of a
    /// hard staircase.
    #[inline]
    fn blend_covered(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if color.a == 0 || coverage <= 0.0 {
            return;
        }
        let alpha = (color.a as f32 / 255.0) * coverage.min(1.0);
        self.blend_components(x, y, color.r, color.g, color.b, alpha);
    }

    fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.a == 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.solid_pixel_bounds(rect) else {
            return;
        };
        self.count_pixels(((x1 - x0) as u64) * ((y1 - y0) as u64));
        // Alpha and the colour premultiplication are constant across the whole
        // rectangle, so compute them once and write the pixel slice directly.
        let alpha = color.a as f32 / 255.0;
        let inverse = 1.0 - alpha;
        let rr = color.r as f32 * alpha;
        let gg = color.g as f32 * alpha;
        let bb = color.b as f32 * alpha;
        let width = self.canvas.width as usize;
        let pixels = &mut self.canvas.pixels;
        for y in y0..y1 {
            let mut index = (y as usize) * width * 4 + (x0 as usize) * 4;
            for _ in x0..x1 {
                pixels[index] = (rr + pixels[index] as f32 * inverse).round() as u8;
                pixels[index + 1] = (gg + pixels[index + 1] as f32 * inverse).round() as u8;
                pixels[index + 2] = (bb + pixels[index + 2] as f32 * inverse).round() as u8;
                pixels[index + 3] = 255;
                index += 4;
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
        let visible = self.visible_bounds();
        if visible.is_empty() {
            return;
        }
        // A very long dashed edge would otherwise loop once per dash even though
        // only the dashes crossing the canvas/clip are visible. Start at the
        // phase-aligned dash nearest the visible span and stop just past it.
        let period = dash + gap;
        let (v_start, v_end) = if horizontal {
            (visible.x, visible.right())
        } else {
            (visible.y, visible.bottom())
        };
        let first = if v_start > 0.0 {
            (v_start / period).floor() as usize
        } else {
            0
        };
        let mut pos = (first as f32) * period;
        let limit = (v_end.max(0.0) + period).min(length);
        while pos < limit {
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
        let font = browser_text::Font::sans();
        let px = browser_text::quantize(font_size);
        if px <= 0.0 {
            return;
        }
        let fine = self.profile_enabled;
        let mut measure_ns = 0_u64;
        let mut advance_ns = 0_u64;
        let mut rasterize_ns = 0_u64;
        let mut blend_ns = 0_u64;
        let started = fine.then(Instant::now);
        let width = font.measure(text, px);
        // Skip text runs that cannot touch the canvas or the active clips before
        // rasterizing any glyphs. The generous margin covers glyph bearings and
        // the underline / strike-through strokes drawn below.
        let bounds = Rect::new(x - px, baseline - 2.0 * px, width + 2.0 * px, 4.0 * px);
        if self.clipped_pixel_bounds(bounds).is_none() {
            if let Some(started) = started {
                TEXT_PHASES.with(|p| {
                    let mut v = p.get();
                    v[0] += started.elapsed().as_nanos() as u64;
                    p.set(v);
                });
            }
            return;
        }
        if let Some(started) = started {
            measure_ns = started.elapsed().as_nanos() as u64;
        }
        // Glyph bitmaps come from `browser_text::rasterize_cached`, a
        // process-wide Arc cache keyed by (char, quantized px, bold, italic), so
        // the 4x4 outline sampling happens at most once per distinct glyph.
        // Only the cheap blend of the cached coverage remains per frame.
        let base_alpha = color.a as f32 / 255.0;
        let mut pen_x = x;
        let mut covered = 0_u64;
        let count = self.profile_enabled;
        for ch in text.chars() {
            let lookup_started = fine.then(Instant::now);
            let advance = font.advance(ch, px);
            let advance_done = fine.then(Instant::now);
            let key: GlyphKey = (ch, (px * 4.0).round() as u32, bold, italic);
            let mut cache = self
                .glyph_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let glyph = if let Some(glyph) = cache.get(&key) {
                if fine {
                    GLYPH_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
                }
                glyph.clone()
            } else {
                if fine {
                    GLYPH_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
                }
                let glyph_started = fine.then(Instant::now);
                let glyph = fast_rasterize_glyph(font, ch, px, bold, italic);
                if let Some(glyph_started) = glyph_started {
                    SLOW_GLYPHS.with(|slow| {
                        slow.borrow_mut().push((
                            ch,
                            px,
                            glyph_started.elapsed().as_secs_f64() * 1000.0,
                        ));
                    });
                }
                cache.insert(key, glyph.clone());
                glyph
            };
            drop(cache);
            if let (Some(lookup_started), Some(advance_done)) = (lookup_started, advance_done) {
                advance_ns += advance_done.duration_since(lookup_started).as_nanos() as u64;
                rasterize_ns += advance_done.elapsed().as_nanos() as u64;
            }
            let Some(glyph) = glyph else {
                pen_x += advance;
                continue;
            };
            let blend_started = fine.then(Instant::now);
            let glyph_x = pen_x.round() as i32 + glyph.left;
            let glyph_y = baseline.round() as i32 + glyph.top;
            for row in 0..glyph.height {
                let row_start = row as usize * glyph.width as usize;
                for col in 0..glyph.width {
                    let coverage = glyph.coverage[row_start + col as usize];
                    if coverage == 0 {
                        continue;
                    }
                    if count {
                        covered += 1;
                    }
                    let alpha = base_alpha * (coverage as f32 / 255.0);
                    self.blend_components(
                        glyph_x + col as i32,
                        glyph_y + row as i32,
                        color.r,
                        color.g,
                        color.b,
                        alpha,
                    );
                }
            }
            if let Some(blend_started) = blend_started {
                blend_ns += blend_started.elapsed().as_nanos() as u64;
            }
            pen_x += advance;
        }
        if fine {
            TEXT_PHASES.with(|p| {
                let mut v = p.get();
                v[0] += measure_ns;
                v[1] += advance_ns;
                v[2] += rasterize_ns;
                v[3] += blend_ns;
                p.set(v);
            });
        }
        self.count_pixels(covered);
        let thickness = self.scale.max(1.0);
        if underline {
            self.fill_rect(Rect::new(x, baseline + thickness, width, thickness), color);
        }
        if line_through {
            self.fill_rect(Rect::new(x, baseline - px * 0.3, width, thickness), color);
        }
    }

    /// Draws an image scaled into `rect`.
    ///
    /// `bilinear = false` uses [`ImageData::sample`] (nearest neighbour) and is
    /// byte-identical to the legacy [`DisplayCommand::DrawImage`] path. `true`
    /// bilinearly interpolates the four neighbouring source texels, which is the
    /// `image-rendering: auto`/`smooth` behaviour of
    /// [`DisplayCommand::DrawImageWithRendering`].
    fn draw_image(&mut self, rect: Rect, image: &ImageData, bilinear: bool) {
        if image.width == 0 || image.height == 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.solid_pixel_bounds(rect) else {
            return;
        };
        self.count_pixels(((x1 - x0) as u64) * ((y1 - y0) as u64));
        let inv_width = 1.0 / rect.width;
        let inv_height = 1.0 / rect.height;
        for y in y0..y1 {
            let v = (y as f32 - rect.y) * inv_height;
            for x in x0..x1 {
                let u = (x as f32 - rect.x) * inv_width;
                let [r, g, b, a] = if bilinear {
                    sample_image_bilinear(image, u, v)
                } else {
                    image.sample(u, v)
                };
                if a == 0 {
                    continue;
                }
                self.blend_components(x, y, r, g, b, a as f32 / 255.0);
            }
        }
    }

    /// Fills a rounded rectangle with anti-aliased edges.
    fn fill_rounded(&mut self, rect: Rect, radius: [f32; 4], color: Color) {
        if color.a == 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(rect) else {
            return;
        };
        let mut covered = 0_u64;
        let count = self.profile_enabled;
        for y in y0..y1 {
            for x in x0..x1 {
                let coverage = rounded_rect_coverage(rect, radius, x, y);
                if coverage > 0.0 {
                    if count {
                        covered += 1;
                    }
                    self.blend_covered(x, y, color, coverage);
                }
            }
        }
        self.count_pixels(covered);
    }

    /// Fills the ring between an outer and an inner rounded rectangle.
    ///
    /// Per-pixel coverage is the difference of the two coverages, so both the
    /// outer and inner edges of a border are anti-aliased.
    fn fill_rounded_ring(
        &mut self,
        outer: Rect,
        outer_radius: [f32; 4],
        inner: Rect,
        inner_radius: [f32; 4],
        color: Color,
    ) {
        if color.a == 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(outer) else {
            return;
        };
        let mut covered = 0_u64;
        let count = self.profile_enabled;
        for y in y0..y1 {
            for x in x0..x1 {
                let outside = rounded_rect_coverage(outer, outer_radius, x, y);
                if outside <= 0.0 {
                    continue;
                }
                let inside = if inner.is_empty() {
                    0.0
                } else {
                    rounded_rect_coverage(inner, inner_radius, x, y)
                };
                let coverage = (outside - inside).max(0.0);
                if coverage > 0.0 {
                    if count {
                        covered += 1;
                    }
                    self.blend_covered(x, y, color, coverage);
                }
            }
        }
        self.count_pixels(covered);
    }

    fn draw_rounded_rect(
        &mut self,
        rect: Rect,
        radius: [f32; 4],
        background: Color,
        border: Option<(Color, f32)>,
    ) {
        if rect.is_empty() {
            return;
        }
        let radius = normalized_radii(rect, radius);
        self.fill_rounded(rect, radius, background);
        if let Some((color, width)) = border
            && color.a > 0
            && width > 0.0
        {
            let inner = Rect::new(
                rect.x + width,
                rect.y + width,
                (rect.width - 2.0 * width).max(0.0),
                (rect.height - 2.0 * width).max(0.0),
            );
            let inner_radius = [
                (radius[0] - width).max(0.0),
                (radius[1] - width).max(0.0),
                (radius[2] - width).max(0.0),
                (radius[3] - width).max(0.0),
            ];
            let inner_radius = normalized_radii(inner, inner_radius);
            self.fill_rounded_ring(rect, radius, inner, inner_radius, color);
        }
    }

    /// Draws a hard (or near-hard) shadow as a single rounded rect with its
    /// radius expanded by the blur spread. This is also the graceful fallback
    /// when the frame work budget is exhausted.
    fn draw_shadow_simple(&mut self, base: Rect, radius: [f32; 4], blur: f32, color: Color) {
        let shape = base.inflate(blur);
        let radii = [
            radius[0] + blur,
            radius[1] + blur,
            radius[2] + blur,
            radius[3] + blur,
        ];
        self.fill_rounded(shape, normalized_radii(shape, radii), color);
    }

    fn draw_shadow(
        &mut self,
        rect: Rect,
        radius: [f32; 4],
        offset: (f32, f32),
        blur: f32,
        color: Color,
    ) {
        if rect.is_empty() || color.a == 0 {
            return;
        }
        let base = rect.translate(offset.0, offset.1);
        // A non-finite blur would produce NaN/infinite shapes; clamp it to zero
        // so a pathological input degrades to a simple hard-edged shadow.
        let blur = if blur.is_finite() { blur.max(0.0) } else { 0.0 };

        // Skip the whole effect when even the blur-expanded shape cannot touch
        // the canvas or the active clips.
        if self.clipped_pixel_bounds(base.inflate(blur)).is_none() {
            return;
        }

        // Tiny/zero blur: one expanded rounded rect is visually equivalent to a
        // stack of concentric fills and avoids building any mask.
        if blur <= SHADOW_SINGLE_BLUR {
            self.draw_shadow_simple(base, radius, blur, color);
            return;
        }

        // Build a low-res alpha mask over the shadow's region of influence,
        // extended by the blur so the kernel has proper support near the clip.
        let margin = blur + SHADOW_MASK_CELL as f32 * 2.0;
        let region = base.inflate(blur).intersect(&self.visible.inflate(margin));
        let (rx0, ry0, rx1, ry1) = integer_visible_bounds(region);
        if rx0 >= rx1 || ry0 >= ry1 {
            return;
        }
        let region_w = (rx1 - rx0) as u32;
        let region_h = (ry1 - ry0) as u32;

        // Scale the cell size up so a huge caster still yields a capped mask.
        let cell = SHADOW_MASK_CELL
            .max(region_w.div_ceil(SHADOW_MASK_MAX_DIM))
            .max(region_h.div_ceil(SHADOW_MASK_MAX_DIM))
            .max(1);
        let mask_w = region_w.div_ceil(cell).max(1) as usize;
        let mask_h = region_h.div_ceil(cell).max(1) as usize;

        // Composite area is the part of the mask region that is actually visible.
        let (vx0, vy0, vx1, vy1) = self.visible_i;
        let cx0 = rx0.max(vx0);
        let cy0 = ry0.max(vy0);
        let cx1 = rx1.min(vx1);
        let cy1 = ry1.min(vy1);
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }

        // Charge for mask generation, the two blur passes and the bilinear
        // composite. If the frame budget cannot cover it, degrade to one fill.
        self.count_pixels(((cx1 - cx0) as u64) * ((cy1 - cy0) as u64));
        let composite_px = (cx1 - cx0) as u64 * (cy1 - cy0) as u64;
        let mask_px = mask_w as u64 * mask_h as u64;
        let cost = mask_px * 2 + composite_px;
        if !self.work_budget.try_consume(cost) {
            self.draw_shadow_simple(base, radius, blur, color);
            return;
        }

        let norm_radius = normalized_radii(base, radius);
        let mut mask = vec![0_u8; mask_w * mask_h];
        for my in 0..mask_h {
            let dy = ry0 as f32 + (my as f32 + 0.5) * cell as f32;
            for mx in 0..mask_w {
                let dx = rx0 as f32 + (mx as f32 + 0.5) * cell as f32;
                if rounded_rect_contains(base, norm_radius, dx, dy) {
                    mask[my * mask_w + mx] = 255;
                }
            }
        }
        let blur_cells = (blur / cell as f32).ceil().max(1.0) as usize;
        blur_alpha(&mut mask, mask_w, mask_h, blur_cells);
        blur_alpha(&mut mask, mask_w, mask_h, blur_cells);

        // Bilinearly composite the blurred mask into the visible region.
        let cell_f = cell as f32;
        for y in cy0..cy1 {
            let fy = (y as f32 + 0.5 - ry0 as f32) / cell_f - 0.5;
            for x in cx0..cx1 {
                let fx = (x as f32 + 0.5 - rx0 as f32) / cell_f - 0.5;
                let coverage = sample_alpha_bilinear(&mask, mask_w, mask_h, fx, fy) as f32 / 255.0;
                if coverage > 0.0 {
                    self.blend_covered(x, y, color, coverage);
                }
            }
        }
    }

    fn draw_linear_gradient(
        &mut self,
        rect: Rect,
        stops: &[(f32, Color)],
        angle_deg: f32,
        radius: [f32; 4],
    ) {
        if rect.is_empty() || stops.is_empty() {
            return;
        }
        // A gradient spanning a huge rect only touches the canvas over a small
        // window; clamp the loop bounds up front instead of sampling every pixel
        // of the (possibly enormous) gradient box.
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(rect) else {
            return;
        };
        let radius = normalized_radii(rect, radius);
        let mut sorted: Vec<(f32, Color)> = stops
            .iter()
            .map(|(position, color)| (position.clamp(0.0, 1.0), *color))
            .collect();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        if sorted.len() == 1 {
            self.fill_rounded(rect, radius, sorted[0].1);
            return;
        }

        let angle = angle_deg.to_radians();
        let dx = angle.sin();
        let dy = -angle.cos();
        let length = (rect.width * dx.abs() + rect.height * dy.abs()).max(f32::EPSILON);
        let cx = rect.x + rect.width * 0.5;
        let cy = rect.y + rect.height * 0.5;

        let mut covered = 0_u64;
        let count = self.profile_enabled;
        for y in y0..y1 {
            for x in x0..x1 {
                let coverage = rounded_rect_coverage(rect, radius, x, y);
                if coverage <= 0.0 {
                    continue;
                }
                if count {
                    covered += 1;
                }
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let t = (((px - cx) * dx + (py - cy) * dy) / length + 0.5).clamp(0.0, 1.0);
                self.blend_covered(x, y, sample_gradient(&sorted, t), coverage);
            }
        }
        self.count_pixels(covered);
    }

    /// Creates a transparent rasterizer for painting effect children.
    ///
    /// `factor` scales both the canvas dimensions and the paint scale relative
    /// to this rasterizer, so a `0.5` buffer covers the same page at half the
    /// resolution (used for large blurs). The work budget is shared so nested
    /// effects draw from the same per-frame pool.
    fn temp_rasterizer_scaled(&self, factor: f32) -> Rasterizer {
        let width = ((self.canvas.width as f32 * factor).ceil().max(1.0)) as u32;
        let height = ((self.canvas.height as f32 * factor).ceil().max(1.0)) as u32;
        let viewport = Size {
            width: width as f32,
            height: height as f32,
        };
        let mut temp = Rasterizer::new(viewport, Color::transparent()).scaled(self.scale * factor);
        // `ImageBuffer::filled` always writes alpha = 255, so start from a
        // genuinely transparent buffer for correct effect compositing.
        temp.canvas.pixels.fill(0);
        temp.work_budget = self.work_budget.clone();
        temp.glyph_cache = self.glyph_cache.clone();
        temp.profile_enabled = self.profile_enabled;
        temp.profile_pixels = 0;
        temp
    }

    /// Full-resolution effect buffer (the common case).
    fn temp_rasterizer(&self) -> Rasterizer {
        self.temp_rasterizer_scaled(1.0)
    }

    /// Paints child commands through an affine transform.
    ///
    /// Children are painted into a transparent buffer, then destination pixels
    /// are inverse-mapped into child space. Rather than sampling the entire
    /// canvas, only the transformed bounding box of the child content (clipped
    /// to the visible region) is sampled. Identity transforms replay the child
    /// commands directly, and the 4x4 sample grid degrades to a single sample
    /// when the per-frame work budget is tight.
    fn draw_transformed(
        &mut self,
        commands: &[DisplayCommand],
        matrix: [f32; 6],
        opacity: f32,
        images: &[ImageData],
    ) {
        if commands.is_empty() || opacity <= 0.0 || !matrix.iter().all(|v| v.is_finite()) {
            return;
        }
        let alpha_scale = opacity.clamp(0.0, 1.0);

        // Identity transform at full opacity: an exact replay needs no temporary
        // buffer or inverse sampling at all.
        if is_identity_matrix(matrix) && alpha_scale >= 1.0 {
            self.execute_commands(commands, images);
            return;
        }

        let Some(inverse) = invert_matrix(matrix) else {
            return;
        };

        // Only the transformed ink bounds can receive pixels. Padding by one
        // device pixel covers anti-aliasing at the edges of the source content.
        let Some(child_bounds) = command_bounds(commands) else {
            return;
        };
        let dst_bounds = transform_rect(child_bounds, matrix).inflate(1.0);
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(self.scaled_rect(dst_bounds)) else {
            return;
        };
        let bbox_px = (x1 - x0) as u64 * (y1 - y0) as u64;
        if bbox_px == 0 {
            return;
        }
        self.count_pixels(bbox_px);

        // Decide how many sub-samples each destination pixel gets. An identity
        // matrix is exact with one sample; otherwise prefer 4x4 AA and fall back
        // to nearest sampling, then to skipping, as the budget drains.
        let subsamples = if is_identity_matrix(matrix) {
            if !self.work_budget.try_consume(bbox_px) {
                return;
            }
            1
        } else {
            let aa_cost = bbox_px * (AA_SUBSAMPLES * AA_SUBSAMPLES) as u64;
            if self.work_budget.try_consume(aa_cost) {
                AA_SUBSAMPLES
            } else if self.work_budget.try_consume(bbox_px) {
                1
            } else {
                return;
            }
        };

        let mut temp = self.temp_rasterizer();
        temp.execute_commands(commands, images);
        let source = &temp.canvas;
        if source.width == 0 || source.height == 0 {
            return;
        }

        let scale = self.scale;
        let samples = (subsamples * subsamples) as f32;
        let src_w = source.width as i32;
        let src_h = source.height as i32;

        for y in y0..y1 {
            for x in x0..x1 {
                let mut sum_r = 0_u32;
                let mut sum_g = 0_u32;
                let mut sum_b = 0_u32;
                let mut sum_a = 0_u32;
                for sy in 0..subsamples {
                    for sx in 0..subsamples {
                        // Destination sub-sample in device pixels, then CSS px.
                        let (dx, dy) = if subsamples == 1 {
                            (x as f32 + 0.5, y as f32 + 0.5)
                        } else {
                            (
                                x as f32 + (sx as f32 + 0.5) / subsamples as f32,
                                y as f32 + (sy as f32 + 0.5) / subsamples as f32,
                            )
                        };
                        let px = dx / scale;
                        let py = dy / scale;
                        // Inverse-map into child CSS space, then to device px.
                        let child_x = inverse[0] * px + inverse[2] * py + inverse[4];
                        let child_y = inverse[1] * px + inverse[3] * py + inverse[5];
                        let sample_x = (child_x * scale).floor() as i32;
                        let sample_y = (child_y * scale).floor() as i32;
                        if sample_x < 0 || sample_y < 0 || sample_x >= src_w || sample_y >= src_h {
                            continue;
                        }
                        let index =
                            ((sample_y as usize * source.width as usize) + sample_x as usize) * 4;
                        let a = u32::from(source.pixels[index + 3]);
                        if a == 0 {
                            continue;
                        }
                        sum_r += u32::from(source.pixels[index]) * a;
                        sum_g += u32::from(source.pixels[index + 1]) * a;
                        sum_b += u32::from(source.pixels[index + 2]) * a;
                        sum_a += a;
                    }
                }
                if sum_a == 0 {
                    continue;
                }
                let color = Color::rgba(
                    (sum_r / sum_a) as u8,
                    (sum_g / sum_a) as u8,
                    (sum_b / sum_a) as u8,
                    ((sum_a as f32 / samples) * alpha_scale).round() as u8,
                );
                self.blend_covered(x, y, color, 1.0);
            }
        }
    }

    /// Composites an effect buffer over the visible part of the canvas.
    ///
    /// `factor` is the buffer's scale relative to this rasterizer (1.0 for a
    /// full-res buffer, 0.5 for a downscaled blur). `matrix`, when present, is
    /// applied per-pixel during the blend so colour-only filters need no extra
    /// full-buffer pass.
    fn composite_temp(
        &mut self,
        temp: &ImageBuffer,
        factor: f32,
        bounds: (i32, i32, i32, i32),
        matrix: Option<&ColorMatrix>,
    ) {
        let (x0, y0, x1, y1) = bounds;
        let tw = temp.width as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let (r, g, b, a) = if factor == 1.0 {
                    let index = ((y as usize) * tw + x as usize) * 4;
                    (
                        temp.pixels[index],
                        temp.pixels[index + 1],
                        temp.pixels[index + 2],
                        temp.pixels[index + 3],
                    )
                } else {
                    let sample = sample_rgba_bilinear(
                        temp,
                        (x as f32 + 0.5) * factor - 0.5,
                        (y as f32 + 0.5) * factor - 0.5,
                    );
                    (sample[0], sample[1], sample[2], sample[3])
                };
                if a == 0 {
                    continue;
                }
                let (r, g, b, a) = match matrix {
                    Some(m) => {
                        let out = apply_matrix_pixel(r, g, b, a, m);
                        (out[0], out[1], out[2], out[3])
                    }
                    None => (r, g, b, a),
                };
                if a == 0 {
                    continue;
                }
                self.blend_covered(x, y, Color::rgba(r, g, b, a), 1.0);
            }
        }
    }

    /// Paints child commands into a temporary buffer, applies the filters, then
    /// composites the result over the canvas.
    fn draw_filtered(
        &mut self,
        commands: &[DisplayCommand],
        filters: &[DisplayFilter],
        images: &[ImageData],
    ) {
        if commands.is_empty() || filters.is_empty() {
            return;
        }

        // Collapse the colour filters into one matrix and collect blur radii.
        let mut matrix = IDENTITY_MATRIX;
        let mut blur = 0.0_f32;
        for filter in filters {
            match filter {
                DisplayFilter::Blur(radius) => {
                    if radius.is_finite() {
                        blur = blur.max(radius.max(0.0));
                    }
                }
                other => matrix = multiply_matrices(&filter_matrix(other), &matrix),
            }
        }
        if matrix == IDENTITY_MATRIX && blur <= 0.0 {
            self.execute_commands(commands, images);
            return;
        }

        // Fast path: a single solid fill with colour-only filters needs no temp
        // buffer at all — applying the matrix to the constant colour and
        // painting directly yields byte-identical pixels.
        if blur <= 0.0 {
            match &commands[0] {
                DisplayCommand::FillRect { rect, color } if commands.len() == 1 => {
                    let filtered = apply_matrix_pixel(color.r, color.g, color.b, color.a, &matrix);
                    self.fill_rect(
                        self.scaled_rect(*rect),
                        Color::rgba(filtered[0], filtered[1], filtered[2], filtered[3]),
                    );
                    return;
                }
                DisplayCommand::DrawRoundedRect {
                    rect,
                    radius,
                    background,
                    border: None,
                } if commands.len() == 1 => {
                    let filtered = apply_matrix_pixel(
                        background.r,
                        background.g,
                        background.b,
                        background.a,
                        &matrix,
                    );
                    let radius = [
                        radius[0] * self.scale,
                        radius[1] * self.scale,
                        radius[2] * self.scale,
                        radius[3] * self.scale,
                    ];
                    self.draw_rounded_rect(
                        self.scaled_rect(*rect),
                        radius,
                        Color::rgba(filtered[0], filtered[1], filtered[2], filtered[3]),
                        None,
                    );
                    return;
                }
                _ => {}
            }
        }

        // Offscreen groups do no work at all (the blur may extend their ink).
        let Some(bounds) = command_bounds(commands) else {
            return;
        };
        let Some((x0, y0, x1, y1)) =
            self.clipped_pixel_bounds(self.scaled_rect(bounds.inflate(blur + 1.0)))
        else {
            return;
        };
        let bbox_px = (x1 - x0) as u64 * (y1 - y0) as u64;
        self.count_pixels(bbox_px);

        // Downscale large blurs: both the rasterization and the blur cost drop
        // by 4x at half resolution, and the blur hides the resolution loss.
        let blur_dev = blur * self.scale;
        let downscale = blur_dev >= 8.0;
        let factor = if downscale { 0.5 } else { 1.0 };
        let mut temp = self.temp_rasterizer_scaled(factor);
        temp.execute_commands(commands, images);

        // Charge for the buffer work and composite; degrade by dropping the blur
        // first, then by replaying the children unfiltered, as the budget drains.
        let temp_px = temp.canvas.width as u64 * temp.canvas.height as u64;
        let work = temp_px + bbox_px;
        if !self.work_budget.try_consume(work) {
            if self.work_budget.try_consume(bbox_px) && matrix != IDENTITY_MATRIX {
                self.composite_temp(&temp.canvas, factor, (x0, y0, x1, y1), Some(&matrix));
            } else {
                self.execute_commands(commands, images);
            }
            return;
        }

        if blur > 0.0 {
            apply_filters(
                &mut temp.canvas.pixels,
                temp.canvas.width,
                temp.canvas.height,
                filters,
                factor,
            );
            // The matrix has already been folded into the buffer by apply_filters.
            self.composite_temp(&temp.canvas, factor, (x0, y0, x1, y1), None);
        } else {
            // Colour-only: fuse the matrix into the composite, skipping a full
            // extra pass over the buffer.
            self.composite_temp(&temp.canvas, factor, (x0, y0, x1, y1), Some(&matrix));
        }
    }

    /// Fills an anti-aliased rounded rectangle with a radial gradient, sampling
    /// [`sample_radial`] at each covered pixel's centre.
    fn draw_radial_gradient(&mut self, rect: Rect, gradient: &RadialGradient, radius: [f32; 4]) {
        if rect.is_empty() || gradient.stops.is_empty() {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(rect) else {
            return;
        };
        let radius = normalized_radii(rect, radius);
        let mut covered = 0_u64;
        let count = self.profile_enabled;
        for y in y0..y1 {
            for x in x0..x1 {
                let coverage = rounded_rect_coverage(rect, radius, x, y);
                if coverage <= 0.0 {
                    continue;
                }
                if count {
                    covered += 1;
                }
                let color = sample_radial(gradient, rect, x as f32 + 0.5, y as f32 + 0.5);
                self.blend_covered(x, y, color, coverage);
            }
        }
        self.count_pixels(covered);
    }

    /// Fills an anti-aliased rounded rectangle with a conic gradient, sampling
    /// [`sample_conic`] at each covered pixel's centre.
    fn draw_conic_gradient(&mut self, rect: Rect, gradient: &ConicGradient, radius: [f32; 4]) {
        if rect.is_empty() || gradient.stops.is_empty() {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(rect) else {
            return;
        };
        let radius = normalized_radii(rect, radius);
        let mut covered = 0_u64;
        let count = self.profile_enabled;
        for y in y0..y1 {
            for x in x0..x1 {
                let coverage = rounded_rect_coverage(rect, radius, x, y);
                if coverage <= 0.0 {
                    continue;
                }
                if count {
                    covered += 1;
                }
                let color = sample_conic(gradient, rect, x as f32 + 0.5, y as f32 + 0.5);
                self.blend_covered(x, y, color, coverage);
            }
        }
        self.count_pixels(covered);
    }

    /// Paints children into a transparent buffer and multiplies each pixel by a
    /// coverage mask. Alpha and luminance masks share the same single-channel
    /// path (the style layer pre-reduces both into a [`CoverageMask`]).
    fn draw_masked(
        &mut self,
        rect: Rect,
        commands: &[DisplayCommand],
        mask: &MaskData,
        images: &[ImageData],
    ) {
        if commands.is_empty() {
            return;
        }
        match mask {
            MaskData::None => {
                self.execute_commands(commands, images);
                return;
            }
            MaskData::Url(_) => {
                // A URL mask needs a decoded image resolved by the compositor;
                // the software rasterizer has no URL -> image lookup, so nothing
                // is painted. This is the one remaining data-only mask gap.
                return;
            }
            MaskData::Alpha(_) | MaskData::Luminance(_) => {}
        }
        if rect.is_empty() {
            return;
        }
        let Some(bounds) = command_bounds(commands) else {
            return;
        };
        let bounds = self.scaled_rect(bounds).intersect(&rect);
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(bounds) else {
            return;
        };
        let region_px = (x1 - x0) as u64 * (y1 - y0) as u64;
        if region_px == 0 {
            return;
        }
        self.count_pixels(region_px);
        if !self.work_budget.try_consume(region_px) {
            return;
        }

        let mut temp = self.temp_rasterizer();
        temp.execute_commands(commands, images);
        let source = &temp.canvas;
        let coverage = match mask {
            MaskData::Alpha(m) | MaskData::Luminance(m) => m,
            _ => unreachable!("None/Url masks returned above"),
        };
        let inv_w = if rect.width > 0.0 {
            1.0 / rect.width
        } else {
            0.0
        };
        let inv_h = if rect.height > 0.0 {
            1.0 / rect.height
        } else {
            0.0
        };
        for y in y0..y1 {
            let my = ((y as f32 + 0.5 - rect.y) * inv_h * coverage.height as f32) as u32;
            for x in x0..x1 {
                let mx = ((x as f32 + 0.5 - rect.x) * inv_w * coverage.width as f32) as u32;
                let opacity = coverage.coverage_at(mx, my);
                if opacity <= 0.0 {
                    continue;
                }
                let index = ((y as usize) * source.width as usize + x as usize) * 4;
                let a = source.pixels[index + 3];
                if a == 0 {
                    continue;
                }
                self.blend_components(
                    x,
                    y,
                    source.pixels[index],
                    source.pixels[index + 1],
                    source.pixels[index + 2],
                    (a as f32 / 255.0) * opacity,
                );
            }
        }
    }

    /// Paints children into a transparent buffer and clips them with the 4x4
    /// anti-aliased coverage mask produced by [`rasterize_clip_path`].
    fn draw_clipped(
        &mut self,
        commands: &[DisplayCommand],
        clip: &ClipPath,
        rect: Rect,
        images: &[ImageData],
    ) {
        if commands.is_empty() {
            return;
        }
        let rect = self.scaled_rect(rect);
        if rect.is_empty() {
            return;
        }
        let Some(bounds) = command_bounds(commands) else {
            return;
        };
        let bounds = self.scaled_rect(bounds).intersect(&rect);
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(bounds) else {
            return;
        };
        let region_px = (x1 - x0) as u64 * (y1 - y0) as u64;
        if region_px == 0 {
            return;
        }
        self.count_pixels(region_px);
        if !self.work_budget.try_consume(region_px) {
            return;
        }

        // Rasterize the clip in local (origin-zero) device-pixel coordinates.
        let mask_w = rect.width.ceil().max(1.0) as u32;
        let mask_h = rect.height.ceil().max(1.0) as u32;
        let local = Rect::new(0.0, 0.0, rect.width, rect.height);
        let coverage = rasterize_clip_path(clip, local, mask_w, mask_h);

        let mut temp = self.temp_rasterizer();
        temp.execute_commands(commands, images);
        let source = &temp.canvas;
        let inv_w = if rect.width > 0.0 {
            1.0 / rect.width
        } else {
            0.0
        };
        let inv_h = if rect.height > 0.0 {
            1.0 / rect.height
        } else {
            0.0
        };
        for y in y0..y1 {
            let my = ((y as f32 + 0.5 - rect.y) * inv_h * coverage.height as f32) as u32;
            for x in x0..x1 {
                let mx = ((x as f32 + 0.5 - rect.x) * inv_w * coverage.width as f32) as u32;
                let clip_coverage = coverage.coverage_at(mx, my);
                if clip_coverage <= 0.0 {
                    continue;
                }
                let index = ((y as usize) * source.width as usize + x as usize) * 4;
                let a = source.pixels[index + 3];
                if a == 0 {
                    continue;
                }
                self.blend_components(
                    x,
                    y,
                    source.pixels[index],
                    source.pixels[index + 1],
                    source.pixels[index + 2],
                    (a as f32 / 255.0) * clip_coverage,
                );
            }
        }
    }

    /// Blends a group with its backdrop using the W3C compositing formula on
    /// premultiplied channels, optionally filtering the backdrop first.
    fn draw_blended(
        &mut self,
        commands: &[DisplayCommand],
        mode: BlendMode,
        backdrop: &crate::effects::BackdropFilters,
        images: &[ImageData],
    ) {
        if commands.is_empty() {
            return;
        }
        let Some(bounds) = command_bounds(commands) else {
            return;
        };
        let Some((x0, y0, x1, y1)) = self.clipped_pixel_bounds(self.scaled_rect(bounds)) else {
            return;
        };
        let region_px = (x1 - x0) as u64 * (y1 - y0) as u64;
        if region_px == 0 {
            return;
        }
        self.count_pixels(region_px);
        if !self.work_budget.try_consume(region_px) {
            // Budget exhaustion degrades to a plain source-over replay.
            self.execute_commands(commands, images);
            return;
        }

        let mut temp = self.temp_rasterizer();
        temp.execute_commands(commands, images);
        let source = &temp.canvas;

        // Snapshot the backdrop (the current canvas) over the affected region.
        let bw = (x1 - x0) as u32;
        let bh = (y1 - y0) as u32;
        let mut backdrop_buf = ImageBuffer::new(bw, bh);
        for y in y0..y1 {
            for x in x0..x1 {
                let src = ((y as usize) * self.canvas.width as usize + x as usize) * 4;
                let dst = (((y - y0) as usize) * bw as usize + (x - x0) as usize) * 4;
                backdrop_buf.pixels[dst..dst + 4]
                    .copy_from_slice(&self.canvas.pixels[src..src + 4]);
            }
        }
        if !backdrop.is_empty() {
            apply_filters(&mut backdrop_buf.pixels, bw, bh, &backdrop.0, 1.0);
        }

        for y in y0..y1 {
            for x in x0..x1 {
                let s = ((y as usize) * source.width as usize + x as usize) * 4;
                let sa = source.pixels[s + 3];
                if sa == 0 {
                    continue;
                }
                let b = (((y - y0) as usize) * bw as usize + (x - x0) as usize) * 4;
                let (sr, sg, sb) = (source.pixels[s], source.pixels[s + 1], source.pixels[s + 2]);
                let (br, bg, bb, ba) = (
                    backdrop_buf.pixels[b],
                    backdrop_buf.pixels[b + 1],
                    backdrop_buf.pixels[b + 2],
                    backdrop_buf.pixels[b + 3],
                );
                let alpha_s = sa as f32 / 255.0;
                let alpha_b = ba as f32 / 255.0;
                let alpha_out = alpha_s + alpha_b * (1.0 - alpha_s);
                if alpha_out <= 0.0 {
                    continue;
                }
                // W3C: Co = as*(1-ab)*Cs + as*ab*B(Cb,Cs) + (1-as)*ab*Cb, all over ao.
                let composite = |cb: u8, cs: u8| -> f32 {
                    let cb = cb as f32 / 255.0;
                    let cs = cs as f32 / 255.0;
                    (alpha_s * (1.0 - alpha_b) * cs
                        + alpha_s * alpha_b * w3c_blend_channel(mode, cb, cs)
                        + (1.0 - alpha_s) * alpha_b * cb)
                        / alpha_out
                };
                let index = ((y as usize) * self.canvas.width as usize + x as usize) * 4;
                self.canvas.pixels[index] =
                    (composite(br, sr).clamp(0.0, 1.0) * 255.0).round() as u8;
                self.canvas.pixels[index + 1] =
                    (composite(bg, sg).clamp(0.0, 1.0) * 255.0).round() as u8;
                self.canvas.pixels[index + 2] =
                    (composite(bb, sb).clamp(0.0, 1.0) * 255.0).round() as u8;
                self.canvas.pixels[index + 3] = (alpha_out.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }

    /// Draws a nine-slice (`border-image`) scaling of `source` into `rect`.
    ///
    /// The source is split into nine regions by `border_image.slice`; the four
    /// corners keep their size, the four edges and the centre are stretched (or
    /// tiled for `repeat`/`round`) into the corresponding destination cells.
    fn draw_border_image(&mut self, rect: Rect, data: &BorderImageData, source: &ImageData) {
        if rect.is_empty() || source.width == 0 || source.height == 0 {
            return;
        }
        // border-image-outset expands the area the nine slices are painted into.
        let (out_top, out_right, out_bottom, out_left) = (
            data.outset[0].resolve(rect.height).max(0.0),
            data.outset[1].resolve(rect.width).max(0.0),
            data.outset[2].resolve(rect.height).max(0.0),
            data.outset[3].resolve(rect.width).max(0.0),
        );
        let area = Rect::new(
            rect.x - out_left,
            rect.y - out_top,
            rect.width + out_left + out_right,
            rect.height + out_top + out_bottom,
        );
        let Some((x0, y0, x1, y1)) = self.solid_pixel_bounds(area) else {
            return;
        };
        let region_px = (x1 - x0) as u64 * (y1 - y0) as u64;
        self.count_pixels(region_px);
        if !self.work_budget.try_consume(region_px) {
            return;
        }

        let image_w = source.width as f32;
        let image_h = source.height as f32;
        // Source slice widths in source pixels; destination slice widths in device px.
        let mut src_slice = [
            data.slice[0].resolve(image_h).max(0.0),
            data.slice[1].resolve(image_w).max(0.0),
            data.slice[2].resolve(image_h).max(0.0),
            data.slice[3].resolve(image_w).max(0.0),
        ];
        let mut dest_slice = [
            data.width[0].resolve(rect.height).max(0.0),
            data.width[1].resolve(rect.width).max(0.0),
            data.width[2].resolve(rect.height).max(0.0),
            data.width[3].resolve(rect.width).max(0.0),
        ];
        // Opposite slices must not overlap; scale both down if they do.
        let src_y = src_slice[0] + src_slice[2];
        if src_y > image_h {
            let factor = image_h / src_y;
            src_slice[0] *= factor;
            src_slice[2] *= factor;
        }
        let src_x = src_slice[1] + src_slice[3];
        if src_x > image_w {
            let factor = image_w / src_x;
            src_slice[1] *= factor;
            src_slice[3] *= factor;
        }
        let dest_y = dest_slice[0] + dest_slice[2];
        if dest_y > rect.height && dest_y > 0.0 {
            let factor = rect.height / dest_y;
            dest_slice[0] *= factor;
            dest_slice[2] *= factor;
        }
        let dest_x = dest_slice[1] + dest_slice[3];
        if dest_x > rect.width && dest_x > 0.0 {
            let factor = rect.width / dest_x;
            dest_slice[1] *= factor;
            dest_slice[3] *= factor;
        }

        let repeat_x = data.repeat[0];
        let repeat_y = data.repeat[1];
        for y in y0..y1 {
            let py = y as f32 + 0.5 - area.y;
            let sy = nine_slice_axis(
                py,
                area.height,
                (dest_slice[0], dest_slice[2]),
                (src_slice[0], src_slice[2], image_h),
                repeat_y,
            );
            for x in x0..x1 {
                let px = x as f32 + 0.5 - area.x;
                let sx = nine_slice_axis(
                    px,
                    area.width,
                    (dest_slice[3], dest_slice[1]),
                    (src_slice[3], src_slice[1], image_w),
                    repeat_x,
                );
                let tx = sx.clamp(0.0, image_w - 1.0) as u32;
                let ty = sy.clamp(0.0, image_h - 1.0) as u32;
                let [r, g, b, a] = source.get(tx, ty);
                if a == 0 {
                    continue;
                }
                self.blend_components(x, y, r, g, b, a as f32 / 255.0);
            }
        }
    }
}

/// Maps a position along one axis of a nine-slice to a source-image coordinate.
///
/// `dest` is `(leading, trailing)` destination slice thickness; `src` is
/// `(leading, trailing, total)` source sizes. The middle band stretches or
/// tiles according to `mode`.
fn nine_slice_axis(
    position: f32,
    length: f32,
    dest: (f32, f32),
    src: (f32, f32, f32),
    mode: BorderImageRepeat,
) -> f32 {
    let (before, after) = dest;
    let (src_before, src_after, src_length) = src;
    let (offset, dst_span, src_start, src_span) = if position < before {
        (position, before, 0.0, src_before)
    } else if position >= length - after {
        (
            position - (length - after),
            after,
            src_length - src_after,
            src_after,
        )
    } else {
        (
            position - before,
            length - before - after,
            src_before,
            src_length - src_before - src_after,
        )
    };
    if dst_span <= 0.0 || src_span <= 0.0 {
        return src_start;
    }
    match mode {
        BorderImageRepeat::Stretch | BorderImageRepeat::Space => {
            src_start + (offset / dst_span) * src_span
        }
        BorderImageRepeat::Repeat => src_start + (offset % src_span),
        BorderImageRepeat::Round => {
            let count = (dst_span / src_span).round().max(1.0);
            src_start + ((offset / dst_span * count).fract()) * src_span
        }
    }
}

/// W3C separable blend function for premultiplied compositing. Non-separable
/// modes fall back to the channel-wise implementation in [`BlendMode::blend`].
fn w3c_blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => {
            if cb <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        other => {
            f32::from(other.blend((cb * 255.0).round() as u8, (cs * 255.0).round() as u8)) / 255.0
        }
    }
}

/// A 4x5 colour matrix. Each row maps `[r, g, b, a, 1]` to an output channel.
type ColorMatrix = [f32; 20];

const IDENTITY_MATRIX: ColorMatrix = [
    1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    0.0,
];

/// Inverts a 2D affine matrix, returning `None` when it is degenerate.
fn invert_matrix(m: [f32; 6]) -> Option<[f32; 6]> {
    let [a, b, c, d, e, f] = m;
    let determinant = a * d - b * c;
    if !determinant.is_finite() || determinant.abs() < 1e-6 {
        return None;
    }
    let ia = d / determinant;
    let ib = -b / determinant;
    let ic = -c / determinant;
    let id = a / determinant;
    let ie = -(ia * e + ic * f);
    let iff = -(ib * e + id * f);
    Some([ia, ib, ic, id, ie, iff])
}

/// True for the exact identity affine matrix.
fn is_identity_matrix(m: [f32; 6]) -> bool {
    m == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

/// Axis-aligned bounding box (CSS px) enclosing `rect` after `m` is applied to
/// its four corners. Used to clip inverse-mapped effects to their real extent.
fn transform_rect(rect: Rect, m: [f32; 6]) -> Rect {
    let corners = [
        (rect.x, rect.y),
        (rect.right(), rect.y),
        (rect.x, rect.bottom()),
        (rect.right(), rect.bottom()),
    ];
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        let tx = m[0] * x + m[2] * y + m[4];
        let ty = m[1] * x + m[3] * y + m[5];
        min_x = min_x.min(tx);
        max_x = max_x.max(tx);
        min_y = min_y.min(ty);
        max_y = max_y.max(ty);
    }
    Rect::new(
        min_x,
        min_y,
        (max_x - min_x).max(0.0),
        (max_y - min_y).max(0.0),
    )
}

/// Conservative bounding box (CSS px) of everything a command list may paint,
/// or `None` when it paints nothing. This is the "per-command bounds" used to
/// keep transform/filter sampling away from empty areas of the canvas.
fn command_bounds(commands: &[DisplayCommand]) -> Option<Rect> {
    fn union(bounds: &mut Option<Rect>, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        *bounds = Some(match *bounds {
            Some(existing) => existing.union(&rect),
            None => rect,
        });
    }

    let mut bounds: Option<Rect> = None;
    for command in commands {
        match command {
            DisplayCommand::PushClip(_) | DisplayCommand::PopClip => {}
            DisplayCommand::FillRect { rect, .. }
            | DisplayCommand::DrawBorder { rect, .. }
            | DisplayCommand::DrawImage { rect, .. }
            | DisplayCommand::DrawRoundedRect { rect, .. } => union(&mut bounds, *rect),
            DisplayCommand::DrawShadow {
                rect, offset, blur, ..
            } => {
                let spread = if blur.is_finite() { blur.max(0.0) } else { 0.0 };
                union(
                    &mut bounds,
                    rect.translate(offset.0, offset.1).inflate(spread),
                );
            }
            DisplayCommand::DrawText {
                x,
                baseline,
                text,
                font_size,
                ..
            } => {
                let px = browser_text::quantize(*font_size);
                let width = browser_text::Font::sans().measure(text, px);
                union(
                    &mut bounds,
                    Rect::new(*x - px, *baseline - 2.0 * px, width + 2.0 * px, 4.0 * px),
                );
            }
            DisplayCommand::DrawLinearGradient { rect, .. }
            | DisplayCommand::DrawRadialGradient { rect, .. }
            | DisplayCommand::DrawConicGradient { rect, .. }
            | DisplayCommand::DrawImageWithRendering { rect, .. }
            | DisplayCommand::DrawBorderImage { rect, .. } => union(&mut bounds, *rect),
            DisplayCommand::DrawMasked {
                rect,
                child_commands,
                ..
            }
            | DisplayCommand::DrawClipped {
                rect,
                child_commands,
                ..
            } => {
                if let Some(child) = command_bounds(child_commands) {
                    union(&mut bounds, child.intersect(rect));
                }
            }
            DisplayCommand::DrawBlended { child_commands, .. } => {
                if let Some(child) = command_bounds(child_commands) {
                    union(&mut bounds, child);
                }
            }
            DisplayCommand::DrawTransformed {
                child_commands,
                matrix,
                ..
            } => {
                if let Some(child) = command_bounds(child_commands)
                    && matrix.iter().all(|v| v.is_finite())
                {
                    union(&mut bounds, transform_rect(child, *matrix));
                }
            }
            DisplayCommand::DrawFiltered {
                child_commands,
                filters,
            } => {
                if let Some(child) = command_bounds(child_commands) {
                    let blur = filters.iter().fold(0.0_f32, |acc, filter| match filter {
                        DisplayFilter::Blur(radius) if radius.is_finite() => {
                            acc.max(radius.max(0.0))
                        }
                        _ => acc,
                    });
                    union(&mut bounds, child.inflate(blur));
                }
            }
        }
    }
    bounds
}

/// Composes two colour matrices: the result applies `b` first, then `a`.
fn multiply_matrices(a: &ColorMatrix, b: &ColorMatrix) -> ColorMatrix {
    let mut out = [0.0_f32; 20];
    for row in 0..4 {
        for col in 0..5 {
            let mut value = if col == 4 { a[row * 5 + 4] } else { 0.0 };
            for k in 0..4 {
                value += a[row * 5 + k] * b[k * 5 + col];
            }
            out[row * 5 + col] = value;
        }
    }
    out
}

/// Builds the colour matrix for one non-spatial filter.
fn filter_matrix(filter: &DisplayFilter) -> ColorMatrix {
    match *filter {
        DisplayFilter::Brightness(value) => {
            let v = value.max(0.0);
            [
                v, 0.0, 0.0, 0.0, 0.0, 0.0, v, 0.0, 0.0, 0.0, 0.0, 0.0, v, 0.0, 0.0, 0.0, 0.0, 0.0,
                1.0, 0.0,
            ]
        }
        DisplayFilter::Contrast(value) => {
            let offset = 0.5 - 0.5 * value;
            [
                value, 0.0, 0.0, 0.0, offset, 0.0, value, 0.0, 0.0, offset, 0.0, 0.0, value, 0.0,
                offset, 0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        }
        DisplayFilter::Grayscale(value) => saturate_matrix(1.0 - value.clamp(0.0, 1.0)),
        DisplayFilter::Saturate(value) => saturate_matrix(value.max(0.0)),
        DisplayFilter::Sepia(value) => sepia_matrix(value.clamp(0.0, 1.0)),
        DisplayFilter::Invert(value) => {
            let a = value.clamp(0.0, 1.0);
            let inverse = 1.0 - a;
            [
                inverse, 0.0, 0.0, 0.0, a, 0.0, inverse, 0.0, 0.0, a, 0.0, 0.0, inverse, 0.0, a,
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        }
        DisplayFilter::Opacity(value) => {
            let v = value.clamp(0.0, 1.0);
            [
                1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0,
                0.0, 0.0, v, 0.0,
            ]
        }
        DisplayFilter::HueRotate(degrees) => hue_rotate_matrix(degrees),
        DisplayFilter::Blur(_) => IDENTITY_MATRIX,
    }
}

fn saturate_matrix(amount: f32) -> ColorMatrix {
    [
        0.213 + 0.787 * amount,
        0.715 - 0.715 * amount,
        0.072 - 0.072 * amount,
        0.0,
        0.0,
        0.213 - 0.213 * amount,
        0.715 + 0.285 * amount,
        0.072 - 0.072 * amount,
        0.0,
        0.0,
        0.213 - 0.213 * amount,
        0.715 - 0.715 * amount,
        0.072 + 0.928 * amount,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ]
}

fn sepia_matrix(amount: f32) -> ColorMatrix {
    let sepia = [
        [0.393_f32, 0.769, 0.189],
        [0.349, 0.686, 0.168],
        [0.272, 0.534, 0.131],
    ];
    let mut out = IDENTITY_MATRIX;
    for row in 0..3 {
        for col in 0..3 {
            let base = if row == col { 1.0 } else { 0.0 };
            out[row * 5 + col] = base * (1.0 - amount) + sepia[row][col] * amount;
        }
    }
    out
}

fn hue_rotate_matrix(degrees: f32) -> ColorMatrix {
    let radians = degrees.to_radians();
    let cosine = radians.cos();
    let sine = radians.sin();
    [
        0.213 + cosine * 0.787 - sine * 0.213,
        0.715 - cosine * 0.715 - sine * 0.715,
        0.072 - cosine * 0.072 + sine * 0.928,
        0.0,
        0.0,
        0.213 - cosine * 0.213 + sine * 0.143,
        0.715 + cosine * 0.285 + sine * 0.140,
        0.072 - cosine * 0.072 - sine * 0.283,
        0.0,
        0.0,
        0.213 - cosine * 0.213 - sine * 0.787,
        0.715 - cosine * 0.715 + sine * 0.715,
        0.072 + cosine * 0.928 + sine * 0.072,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ]
}

/// Applies a colour matrix to one straight-RGBA pixel. The arithmetic matches
/// [`apply_matrix`] exactly so fused and buffered paths produce identical bytes.
#[inline]
fn apply_matrix_pixel(r: u8, g: u8, b: u8, a: u8, matrix: &ColorMatrix) -> [u8; 4] {
    let rf = f32::from(r) / 255.0;
    let gf = f32::from(g) / 255.0;
    let bf = f32::from(b) / 255.0;
    let af = f32::from(a) / 255.0;
    let mut out = [0_u8; 4];
    for (channel, component) in out.iter_mut().enumerate() {
        let base = channel * 5;
        let value = matrix[base] * rf
            + matrix[base + 1] * gf
            + matrix[base + 2] * bf
            + matrix[base + 3] * af
            + matrix[base + 4];
        *component = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    out
}

/// Applies a colour matrix to every straight-RGBA pixel of a buffer.
fn apply_matrix(pixels: &mut [u8], matrix: &ColorMatrix) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let [r, g, b, a] = apply_matrix_pixel(pixel[0], pixel[1], pixel[2], pixel[3], matrix);
        pixel.copy_from_slice(&[r, g, b, a]);
    }
}

/// Applies a filter chain, collapsing consecutive colour filters into a single
/// matrix pass and using spatial box blurs for `blur()`. `radius_scale` shrinks
/// blur radii for downscaled buffers so a half-resolution filter blurs half as
/// many pixels; at 1.0 the behaviour is unchanged.
fn apply_filters(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    filters: &[DisplayFilter],
    radius_scale: f32,
) {
    let mut matrix = IDENTITY_MATRIX;
    for filter in filters {
        match filter {
            DisplayFilter::Blur(radius) => {
                apply_matrix(pixels, &matrix);
                matrix = IDENTITY_MATRIX;
                box_blur(
                    pixels,
                    width as usize,
                    height as usize,
                    *radius * radius_scale,
                );
            }
            other => matrix = multiply_matrices(&filter_matrix(other), &matrix),
        }
    }
    apply_matrix(pixels, &matrix);
}

/// Deterministic sliding-window average of an RGBA line. Produces byte-for-byte
/// the same result as summing the naive clamped window `[i-radius, i+radius]`,
/// but updates the running sum in O(1) per pixel instead of O(radius).
fn sliding_box_rgba(src: &[u8], dst: &mut [u8], count: usize, radius: usize) {
    if count == 0 {
        return;
    }
    let mut lo = 0_usize;
    let mut hi = radius.min(count - 1);
    let mut sums = [0_u32; 4];
    for i in 0..=hi {
        for channel in 0..4 {
            sums[channel] += u32::from(src[i * 4 + channel]);
        }
    }
    for i in 0..count {
        while lo < i.saturating_sub(radius) {
            for channel in 0..4 {
                sums[channel] -= u32::from(src[lo * 4 + channel]);
            }
            lo += 1;
        }
        while hi < (i + radius).min(count - 1) {
            hi += 1;
            for channel in 0..4 {
                sums[channel] += u32::from(src[hi * 4 + channel]);
            }
        }
        let divisor = (hi - lo + 1) as u32;
        for channel in 0..4 {
            dst[i * 4 + channel] = (sums[channel] / divisor) as u8;
        }
    }
}

/// A small, deterministic separable box blur used to approximate CSS `blur()`.
fn box_blur(pixels: &mut [u8], width: usize, height: usize, radius: f32) {
    if width == 0 || height == 0 || !radius.is_finite() {
        return;
    }
    let radius = (radius.ceil().max(0.0) as usize).min(32);
    if radius == 0 {
        return;
    }
    // Two iterations approximate a Gaussian falloff while staying cheap.
    for _ in 0..2 {
        blur_pass(pixels, width, height, radius, true);
        blur_pass(pixels, width, height, radius, false);
    }
}

fn blur_pass(pixels: &mut [u8], width: usize, height: usize, radius: usize, horizontal: bool) {
    let outer = if horizontal { height } else { width };
    let inner = if horizontal { width } else { height };
    if inner == 0 || outer == 0 {
        return;
    }
    let mut line = vec![0_u8; inner * 4];
    let mut out = vec![0_u8; inner * 4];

    for l in 0..outer {
        for i in 0..inner {
            let index = if horizontal {
                (l * inner + i) * 4
            } else {
                (i * outer + l) * 4
            };
            line[i * 4..i * 4 + 4].copy_from_slice(&pixels[index..index + 4]);
        }
        sliding_box_rgba(&line, &mut out, inner, radius);
        for i in 0..inner {
            let index = if horizontal {
                (l * inner + i) * 4
            } else {
                (i * outer + l) * 4
            };
            pixels[index..index + 4].copy_from_slice(&out[i * 4..i * 4 + 4]);
        }
    }
}

/// Device pixels per low-resolution shadow-mask cell. Evaluating coverage on a
/// coarse grid and blurring it is dramatically cheaper than 4x4 supersampling
/// five full-size concentric rounded rectangles.
const SHADOW_MASK_CELL: u32 = 4;
/// Upper bound on mask cells per axis, capping the work done by one shadow even
/// for an enormous (but partially visible) caster box.
const SHADOW_MASK_MAX_DIM: u32 = 256;
/// Blurs at or below this radius (device px) use a single expanded rounded rect
/// instead of building a mask — visually equivalent and much cheaper.
const SHADOW_SINGLE_BLUR: f32 = 1.0;

/// Deterministic sliding-window average of a single-channel line.
fn sliding_box_alpha(src: &[u8], dst: &mut [u8], count: usize, radius: usize) {
    if count == 0 {
        return;
    }
    let mut lo = 0_usize;
    let mut hi = radius.min(count - 1);
    let mut sum: u32 = src[..=hi].iter().map(|&v| u32::from(v)).sum();
    for (i, out) in dst.iter_mut().enumerate().take(count) {
        while lo < i.saturating_sub(radius) {
            sum -= u32::from(src[lo]);
            lo += 1;
        }
        while hi < (i + radius).min(count - 1) {
            hi += 1;
            sum += u32::from(src[hi]);
        }
        *out = (sum / (hi - lo + 1) as u32) as u8;
    }
}

/// Two-pass separable box blur of a single-channel alpha mask. The mask is tiny
/// (at most [`SHADOW_MASK_MAX_DIM`] squared), so a full scratch buffer is fine.
fn blur_alpha(mask: &mut [u8], width: usize, height: usize, radius: usize) {
    if width == 0 || height == 0 || radius == 0 {
        return;
    }
    let mut scratch = vec![0_u8; width * height];
    for y in 0..height {
        let start = y * width;
        sliding_box_alpha(
            &mask[start..start + width],
            &mut scratch[start..start + width],
            width,
            radius,
        );
    }
    let mut column = vec![0_u8; height];
    let mut out = vec![0_u8; height];
    for x in 0..width {
        for y in 0..height {
            column[y] = scratch[y * width + x];
        }
        sliding_box_alpha(&column, &mut out, height, radius);
        for y in 0..height {
            mask[y * width + x] = out[y];
        }
    }
}

/// Bilinearly samples a single-channel mask at float cell coordinates, clamping
/// outside samples to the edge cells.
fn sample_alpha_bilinear(mask: &[u8], width: usize, height: usize, fx: f32, fy: f32) -> u8 {
    let x = fx.clamp(0.0, width.saturating_sub(1) as f32);
    let y = fy.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let top = mask[y0 * width + x0] as f32 * (1.0 - tx) + mask[y0 * width + x1] as f32 * tx;
    let bottom = mask[y1 * width + x0] as f32 * (1.0 - tx) + mask[y1 * width + x1] as f32 * tx;
    (top * (1.0 - ty) + bottom * ty).round() as u8
}

/// Bilinearly samples a straight-RGBA buffer at float pixel coordinates,
/// clamping outside samples to the edge pixels. Used when upscaling a
/// downscaled filter result.
fn sample_rgba_bilinear(image: &ImageBuffer, fx: f32, fy: f32) -> [u8; 4] {
    let width = image.width as usize;
    let height = image.height as usize;
    if width == 0 || height == 0 {
        return [0, 0, 0, 0];
    }
    let x = fx.clamp(0.0, (width - 1) as f32);
    let y = fy.clamp(0.0, (height - 1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let mut out = [0_u8; 4];
    for channel in 0..4 {
        let top = image.pixels[(y0 * width + x0) * 4 + channel] as f32 * (1.0 - tx)
            + image.pixels[(y0 * width + x1) * 4 + channel] as f32 * tx;
        let bottom = image.pixels[(y1 * width + x0) * 4 + channel] as f32 * (1.0 - tx)
            + image.pixels[(y1 * width + x1) * 4 + channel] as f32 * tx;
        out[channel] = (top * (1.0 - ty) + bottom * ty).round() as u8;
    }
    out
}

/// Bilinearly samples a decoded image at normalized `(u, v)` coordinates.
///
/// Texel centres sit at `(i + 0.5) / dimension`, so the continuous source
/// coordinate is `u * width - 0.5` (clamped to the edge texels). This mirrors
/// [`sample_rgba_bilinear`] but works on [`ImageData`] directly.
fn sample_image_bilinear(image: &ImageData, u: f32, v: f32) -> [u8; 4] {
    let width = image.width as usize;
    let height = image.height as usize;
    if width == 0 || height == 0 {
        return [0, 0, 0, 0];
    }
    let x = (u.clamp(0.0, 1.0) * image.width as f32 - 0.5).clamp(0.0, (width - 1) as f32);
    let y = (v.clamp(0.0, 1.0) * image.height as f32 - 0.5).clamp(0.0, (height - 1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let mut out = [0_u8; 4];
    for channel in 0..4 {
        let top = f32::from(image.pixels[(y0 * width + x0) * 4 + channel]) * (1.0 - tx)
            + f32::from(image.pixels[(y0 * width + x1) * 4 + channel]) * tx;
        let bottom = f32::from(image.pixels[(y1 * width + x0) * 4 + channel]) * (1.0 - tx)
            + f32::from(image.pixels[(y1 * width + x1) * 4 + channel]) * tx;
        out[channel] = (top * (1.0 - ty) + bottom * ty).round() as u8;
    }
    out
}

/// Renders a display list into an RGBA buffer.
pub fn render(list: &DisplayList, viewport: Size, background: Color) -> ImageBuffer {
    Rasterizer::new(viewport, background).execute(list)
}

/// Identifies one rendered frame for the [`RenderCache`].
///
/// The key combines a structural hash of the display list (commands *and*
/// image contents) with the viewport, scale and background. Two frames with
/// the same key are guaranteed to produce identical pixels, so a hit can simply
/// clone the previous [`ImageBuffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderKey {
    list_hash: u64,
    list_len: usize,
    image_count: usize,
    width: u32,
    height: u32,
    scale_bits: u32,
    background: u32,
}

impl RenderKey {
    /// Builds a key from a display list and the rasterization parameters.
    pub fn new(list: &DisplayList, viewport: Size, background: Color, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let mut packed = u32::from(background.r) << 24;
        packed |= u32::from(background.g) << 16;
        packed |= u32::from(background.b) << 8;
        packed |= u32::from(background.a);
        Self {
            list_hash: list.identity_hash(),
            list_len: list.commands.len(),
            image_count: list.images.len(),
            width: clamp_dimension(viewport.width.max(1.0) as u32),
            height: clamp_dimension(viewport.height.max(1.0) as u32),
            scale_bits: scale.to_bits(),
            background: packed,
        }
    }
}

/// A bounded LRU cache of rendered frames.
///
/// Reuse an instance across frames (e.g. in a render pipeline) with
/// [`Rasterizer::render_cached`]. For callers that cannot thread a cache through
/// their pipeline, the free function [`render_cached`] uses a process-wide
/// instance instead.
#[derive(Debug)]
pub struct RenderCache {
    entries: HashMap<RenderKey, ImageBuffer>,
    order: VecDeque<RenderKey>,
    capacity: usize,
    hits: u64,
    misses: u64,
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new(4)
    }
}

impl RenderCache {
    /// Creates a cache holding at most `capacity` frames (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
            hits: 0,
            misses: 0,
        }
    }

    /// Looks up `key`, recording a hit or miss in the cache statistics and
    /// refreshing its position in the eviction order.
    pub fn get(&mut self, key: &RenderKey) -> Option<&ImageBuffer> {
        if self.entries.contains_key(key) {
            self.hits += 1;
            self.order.retain(|old| old != key);
            self.order.push_back(*key);
        } else {
            self.misses += 1;
        }
        self.entries.get(key)
    }

    /// Stores `image` under `key`, evicting the oldest entry when full.
    pub fn insert(&mut self, key: RenderKey, image: ImageBuffer) {
        if self.entries.insert(key, image).is_none() {
            self.order.push_back(key);
            while self.order.len() > self.capacity {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
    }

    /// Number of cache hits since construction or the last [`Self::clear`].
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Number of cache misses since construction or the last [`Self::clear`].
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Number of cached frames.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no frames are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Removes all cached frames (statistics are left intact).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

impl Rasterizer {
    /// Renders `list`, or clones a cached frame when the display list,
    /// viewport, background and scale are unchanged.
    ///
    /// `render()` is left untouched; this is an explicit opt-in for callers
    /// that redraw the same page repeatedly.
    pub fn render_cached(
        list: &DisplayList,
        viewport: Size,
        background: Color,
        scale: f32,
        cache: &mut RenderCache,
    ) -> ImageBuffer {
        let key = RenderKey::new(list, viewport, background, scale);
        if let Some(image) = cache.get(&key) {
            return image.clone();
        }
        let image = Rasterizer::new(viewport, background)
            .scaled(scale)
            .execute(list);
        cache.insert(key, image.clone());
        image
    }
}

static GLOBAL_RENDER_CACHE: OnceLock<Mutex<RenderCache>> = OnceLock::new();

fn global_render_cache() -> &'static Mutex<RenderCache> {
    GLOBAL_RENDER_CACHE.get_or_init(|| Mutex::new(RenderCache::new(4)))
}

/// Renders through a process-wide [`RenderCache`].
///
/// The lock is released while rasterizing so concurrent callers do not block
/// each other for the duration of a full paint.
pub fn render_cached(
    list: &DisplayList,
    viewport: Size,
    background: Color,
    scale: f32,
) -> ImageBuffer {
    let key = RenderKey::new(list, viewport, background, scale);
    {
        let mut cache = global_render_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(image) = cache.get(&key) {
            return image.clone();
        }
    }
    let image = Rasterizer::new(viewport, background)
        .scaled(scale)
        .execute(list);
    let mut cache = global_render_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(key, image.clone());
    image
}

/// Clears the process-wide render cache.
pub fn clear_render_cache() {
    let mut cache = global_render_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{CoverageMask, ImageRendering};
    use std::time::{Duration, Instant};

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
    fn fast_glyph_coverage_matches_reference() {
        let font = browser_text::Font::sans();
        let characters = "AZéçã0O3 gprsujtodW—!?".chars();
        let sizes = [8.0, 11.0, 13.25, 16.0, 21.0, 48.0, 99.25];
        for ch in characters {
            for px in sizes {
                for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                    let fast = fast_rasterize_glyph(font, ch, px, bold, italic);
                    let reference = browser_text::rasterize_cached(font, ch, px, bold, italic);
                    assert_eq!(
                        fast.as_deref(),
                        reference.as_deref(),
                        "coverage mismatch for {ch:?} @ {px}px bold={bold} italic={italic}"
                    );
                }
            }
        }
    }

    #[test]
    fn glyph_cache_reuses_bitmaps_across_runs_and_effects() {
        let text = "Hello world";
        let run = |x, baseline| DisplayCommand::DrawText {
            x,
            baseline,
            text: text.into(),
            font_size: 16.0,
            color: Color::rgb(0, 0, 0),
            underline: false,
            line_through: false,
            bold: false,
            italic: false,
        };
        let mut rasterizer = Rasterizer::new(viewport(), Color::WHITE);
        let images = [];
        rasterizer.draw(&run(4.0, 20.0), &images);
        let distinct = rasterizer.glyph_cache.lock().unwrap().len();
        assert!(distinct > 0 && distinct < text.chars().count());

        // Identical runs hit the same cache entries instead of rasterizing again.
        rasterizer.draw(&run(4.0, 40.0), &images);
        assert_eq!(rasterizer.glyph_cache.lock().unwrap().len(), distinct);

        // Effect rasterizers share the parent's cache.
        let transformed = DisplayCommand::DrawTransformed {
            child_commands: vec![run(4.0, 60.0)],
            matrix: [1.0, 0.0, 0.0, 1.0, 2.0, 0.0],
            opacity: 0.5,
        };
        rasterizer.draw(&transformed, &images);
        assert_eq!(rasterizer.glyph_cache.lock().unwrap().len(), distinct);
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

    #[test]
    fn rounded_rect_coverage_fast_paths_match_supersampling() {
        let rects = [
            Rect::new(10.0, 10.0, 40.0, 30.0),
            Rect::new(8.5, 8.25, 24.0, 24.0),
            Rect::new(0.0, 0.0, 64.0, 64.0),
            Rect::new(-5.0, -5.0, 20.0, 20.0),
        ];
        let radii = [[0.0; 4], [8.0; 4], [4.0, 8.0, 12.0, 2.0], [20.0; 4]];
        for rect in rects {
            for radius in radii {
                let radius = normalized_radii(rect, radius);
                for y in -2..40 {
                    for x in -2..40 {
                        let mut hits = 0_usize;
                        for sy in 0..AA_SUBSAMPLES {
                            for sx in 0..AA_SUBSAMPLES {
                                let px = x as f32 + (sx as f32 + 0.5) / AA_SUBSAMPLES as f32;
                                let py = y as f32 + (sy as f32 + 0.5) / AA_SUBSAMPLES as f32;
                                if rounded_rect_contains(rect, radius, px, py) {
                                    hits += 1;
                                }
                            }
                        }
                        let expected = hits as f32 / (AA_SUBSAMPLES * AA_SUBSAMPLES) as f32;
                        assert_eq!(
                            rounded_rect_coverage(rect, radius, x, y),
                            expected,
                            "coverage mismatch at ({x}, {y}) for {rect:?} {radius:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rounded_rect_has_partially_covered_edge_pixels() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawRoundedRect {
            rect: Rect::new(10.0, 10.0, 40.0, 40.0),
            radius: [8.0; 4],
            background: Color::rgb(255, 0, 0),
            border: None,
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));

        // The interior is fully covered, the far corner is untouched.
        assert_eq!(image.pixel(30, 30), [255, 0, 0, 255]);
        assert_eq!(image.pixel(10, 10), [255, 255, 255, 255]);

        // Pixels along the arc are blended: their green/blue channels lie
        // strictly between the backdrop (255) and the fill (0). That is only
        // possible with fractional coverage, i.e. anti-aliasing.
        let mut partial = 0;
        for y in 10..22 {
            for x in 10..22 {
                let p = image.pixel(x, y);
                if p[1] > 0 && p[1] < 255 {
                    partial += 1;
                    assert_eq!(p[1], p[2], "blended pixel must be neutral");
                    assert_eq!(p[3], 255, "blended pixel stays opaque");
                }
            }
        }
        assert!(partial > 0, "expected partially covered corner pixels");
    }

    #[test]
    fn rounded_rect_edge_coverage_is_graded_not_binary() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawRoundedRect {
            rect: Rect::new(8.0, 8.0, 24.0, 24.0),
            radius: [10.0; 4],
            background: Color::rgb(0, 0, 255),
            border: None,
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));

        // Collect the red channel of the pixels straddling the top-left arc;
        // a hard staircase would only ever produce 0 or 255.
        let mut distinct = std::collections::BTreeSet::new();
        for y in 8..20 {
            for x in 8..20 {
                let p = image.pixel(x, y);
                if p[0] > 0 && p[0] < 255 {
                    distinct.insert(p[0]);
                }
            }
        }
        assert!(
            distinct.len() >= 2,
            "expected multiple intermediate coverage levels, got {distinct:?}"
        );
    }

    #[test]
    fn rounded_rect_border_is_anti_aliased() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawRoundedRect {
            rect: Rect::new(10.0, 10.0, 40.0, 40.0),
            radius: [10.0; 4],
            background: Color::rgb(255, 255, 255),
            border: Some((Color::rgb(0, 0, 0), 3.0)),
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));

        assert_eq!(image.pixel(30, 30), [255, 255, 255, 255]);
        assert_eq!(image.pixel(10, 30), [0, 0, 0, 255]);
        let mut partial = 0;
        for y in 10..22 {
            for x in 10..22 {
                let p = image.pixel(x, y);
                if p[0] > 0 && p[0] < 255 {
                    partial += 1;
                }
            }
        }
        assert!(partial > 0, "expected partially covered border pixels");
    }

    #[test]
    fn shadow_falls_off_outward() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawShadow {
            rect: Rect::new(20.0, 20.0, 20.0, 20.0),
            radius: [4.0; 4],
            offset: (2.0, 2.0),
            blur: 8.0,
            color: Color::rgba(0, 0, 0, 200),
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));
        assert!(image.pixel(30, 30)[0] < 255, "shadow core is visible");
        assert!(
            image.pixel(30, 30)[0] <= image.pixel(18, 30)[0],
            "core is darkest"
        );
        assert_eq!(image.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn linear_gradient_interpolates_across_the_box() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawLinearGradient {
            rect: Rect::new(4.0, 4.0, 40.0, 40.0),
            stops: vec![(0.0, Color::rgb(255, 0, 0)), (1.0, Color::rgb(0, 0, 255))],
            angle_deg: 90.0,
            radius: [6.0; 4],
        });
        let image = render(&list, viewport(), Color::rgb(255, 255, 255));
        assert!(
            image.pixel(6, 24)[0] > image.pixel(42, 24)[0],
            "red at start"
        );
        assert!(
            image.pixel(42, 24)[2] > image.pixel(6, 24)[2],
            "blue at end"
        );
        assert_eq!(image.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn image_buffer_clamps_oversized_dimensions() {
        let cap = crate::images::MAX_IMAGE_DIMENSION;
        let buffer = ImageBuffer::new(cap + 500, 10);
        assert_eq!(buffer.width, cap);
        assert_eq!(buffer.height, 10);
        assert_eq!(buffer.pixels.len(), cap as usize * 10 * 4);

        // A giant viewport must not try to allocate a giant canvas.
        let image = render(
            &DisplayList::new(),
            Size {
                width: 1e7,
                height: 1e7,
            },
            Color::WHITE,
        );
        assert_eq!(image.width, cap);
        assert_eq!(image.height, cap);
    }

    #[test]
    fn commands_fully_outside_viewport_are_skipped() {
        let mut list = DisplayList::new();
        // Without clip-aware skipping these two huge primitives would drive
        // billions of per-pixel iterations for an empty result.
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(100_000.0, 0.0, 100_000.0, 100_000.0),
            color: Color::rgb(255, 0, 0),
        });
        list.push(DisplayCommand::DrawLinearGradient {
            rect: Rect::new(0.0, 100_000.0, 100_000.0, 100_000.0),
            stops: vec![(0.0, Color::WHITE), (1.0, Color::BLACK)],
            angle_deg: 90.0,
            radius: [0.0; 4],
        });
        list.push(DisplayCommand::DrawShadow {
            rect: Rect::new(-200_000.0, -200_000.0, 100.0, 100.0),
            radius: [4.0; 4],
            offset: (0.0, 0.0),
            blur: 64.0,
            color: Color::rgba(0, 0, 0, 200),
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(image.pixel(63, 63), [255, 255, 255, 255]);
    }

    #[test]
    fn commands_outside_a_clip_are_skipped() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::PushClip(Rect::new(0.0, 0.0, 10.0, 10.0)));
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(100.0, 100.0, 500.0, 500.0),
            color: Color::rgb(255, 0, 0),
        });
        list.push(DisplayCommand::PopClip);
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(5, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn huge_half_visible_primitives_paint_only_the_viewport() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(-1000.0, -1000.0, 2000.0, 2000.0),
            color: Color::rgb(255, 0, 0),
        });
        list.push(DisplayCommand::DrawShadow {
            rect: Rect::new(-500.0, -500.0, 1000.0, 1000.0),
            radius: [4.0; 4],
            offset: (0.0, 0.0),
            blur: 100.0,
            color: Color::rgba(0, 0, 0, 200),
        });
        let image = render(&list, viewport(), Color::WHITE);
        let near = image.pixel(0, 0);
        let far = image.pixel(63, 63);
        assert_eq!(near, far, "the whole viewport is uniformly covered");
        assert!(near[0] < 255 && near[0] > 0, "shadow darkens the red fill");
        assert_eq!(near[3], 255);
    }

    #[test]
    fn transformed_rect_uses_inverse_mapping() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawTransformed {
            child_commands: vec![DisplayCommand::FillRect {
                rect: Rect::new(8.0, 8.0, 16.0, 16.0),
                color: Color::rgb(255, 0, 0),
            }],
            // translate(10, 6): the child fill moves from 8..24 to 18..34 (x).
            matrix: [1.0, 0.0, 0.0, 1.0, 10.0, 6.0],
            opacity: 1.0,
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(27, 20), [255, 0, 0, 255], "translated fill");
        assert_eq!(
            image.pixel(12, 20),
            [255, 255, 255, 255],
            "original position is empty"
        );
    }

    #[test]
    fn transformed_rect_scales_through_inverse_mapping() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawTransformed {
            child_commands: vec![DisplayCommand::FillRect {
                rect: Rect::new(20.0, 20.0, 20.0, 20.0),
                color: Color::rgb(0, 0, 255),
            }],
            // scale(2) about the origin: 20..40 becomes 40..80.
            matrix: [2.0, 0.0, 0.0, 2.0, 0.0, 0.0],
            opacity: 1.0,
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(50, 50), [0, 0, 255, 255], "scaled fill");
        assert_eq!(image.pixel(20, 20), [255, 255, 255, 255], "outside fill");
    }

    #[test]
    fn filter_brightness_scales_rgb_channels() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawFiltered {
            child_commands: vec![DisplayCommand::FillRect {
                rect: Rect::new(8.0, 8.0, 16.0, 16.0),
                color: Color::rgb(200, 100, 50),
            }],
            filters: vec![DisplayFilter::Brightness(0.5)],
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(16, 16), [100, 50, 25, 255]);
        assert_eq!(image.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn render_cache_reuses_unchanged_display_list() {
        let mut cache = RenderCache::new(2);
        let mut list = DisplayList::new();
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(8.0, 8.0, 16.0, 16.0),
            color: Color::rgb(255, 0, 0),
        });

        let first = Rasterizer::render_cached(&list, viewport(), Color::WHITE, 1.0, &mut cache);
        let second = Rasterizer::render_cached(&list, viewport(), Color::WHITE, 1.0, &mut cache);

        assert_eq!(cache.misses(), 1, "the first render is a miss");
        assert_eq!(cache.hits(), 1, "the identical second render is a hit");
        assert_eq!(cache.len(), 1);
        assert_eq!(first, second, "a hit returns an equivalent image");
    }

    #[test]
    fn render_cache_invalidates_on_content_viewport_and_scale_changes() {
        let mut cache = RenderCache::new(4);
        let mut list = DisplayList::new();
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(8.0, 8.0, 16.0, 16.0),
            color: Color::rgb(255, 0, 0),
        });

        let _ = Rasterizer::render_cached(&list, viewport(), Color::WHITE, 1.0, &mut cache);
        assert_eq!(cache.misses(), 1);

        // A freshly built but structurally identical list still hits.
        let cloned = list.clone();
        let _ = Rasterizer::render_cached(&cloned, viewport(), Color::WHITE, 1.0, &mut cache);
        assert_eq!(cache.hits(), 1);

        // Changed content: miss.
        let mut changed = list.clone();
        changed.push(DisplayCommand::FillRect {
            rect: Rect::new(0.0, 0.0, 4.0, 4.0),
            color: Color::rgb(0, 0, 255),
        });
        let _ = Rasterizer::render_cached(&changed, viewport(), Color::WHITE, 1.0, &mut cache);
        assert_eq!(cache.misses(), 2);

        // Changed scale: miss.
        let _ = Rasterizer::render_cached(&changed, viewport(), Color::WHITE, 2.0, &mut cache);
        assert_eq!(cache.misses(), 3);

        // Changed viewport: miss.
        let _ = Rasterizer::render_cached(
            &changed,
            Size {
                width: 128.0,
                height: 64.0,
            },
            Color::WHITE,
            2.0,
            &mut cache,
        );
        assert_eq!(cache.misses(), 4);
    }

    #[test]
    fn render_cache_clone_is_independent() {
        let mut cache = RenderCache::new(2);
        let list = DisplayList::new();
        let mut first = Rasterizer::render_cached(&list, viewport(), Color::WHITE, 1.0, &mut cache);
        let second = Rasterizer::render_cached(&list, viewport(), Color::WHITE, 1.0, &mut cache);
        first.pixels[0] = 123;
        assert_ne!(
            first.pixels[0], second.pixels[0],
            "callers get their own buffer"
        );
    }

    #[test]
    fn identity_transform_replays_children_directly() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawTransformed {
            child_commands: vec![DisplayCommand::FillRect {
                rect: Rect::new(8.0, 8.0, 16.0, 16.0),
                color: Color::rgb(255, 0, 0),
            }],
            matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            opacity: 1.0,
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(10, 10), [255, 0, 0, 255]);
        assert_eq!(image.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn no_op_filter_replays_children_directly() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawFiltered {
            child_commands: vec![DisplayCommand::FillRect {
                rect: Rect::new(8.0, 8.0, 16.0, 16.0),
                color: Color::rgb(0, 200, 0),
            }],
            filters: vec![DisplayFilter::Brightness(1.0)],
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(10, 10), [0, 200, 0, 255]);
    }

    #[test]
    fn filter_opacity_on_solid_fill_is_fused() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawFiltered {
            child_commands: vec![DisplayCommand::FillRect {
                rect: Rect::new(8.0, 8.0, 16.0, 16.0),
                color: Color::rgba(200, 100, 50, 255),
            }],
            filters: vec![DisplayFilter::Opacity(0.5)],
        });
        let image = render(&list, viewport(), Color::WHITE);
        let pixel = image.pixel(16, 16);
        assert!(pixel[0] > 200 && pixel[0] < 255, "half-strength red");
        assert!(pixel[1] > 100 && pixel[1] < 255, "half-strength green");
        assert!(pixel[2] > 50 && pixel[2] < 255, "half-strength blue");
        assert_eq!(pixel[3], 255);
    }

    #[test]
    fn large_blur_filter_uses_a_downscaled_buffer() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawFiltered {
            child_commands: vec![DisplayCommand::FillRect {
                rect: Rect::new(20.0, 20.0, 24.0, 24.0),
                color: Color::rgb(0, 0, 0),
            }],
            filters: vec![DisplayFilter::Blur(16.0)],
        });
        let image = render(&list, viewport(), Color::WHITE);
        // A blurred black rect composited over white has partial alpha, so its
        // core is light grey rather than black — but it must still be darker
        // than the surrounding falloff and leave the far corner untouched.
        assert!(image.pixel(32, 32)[0] < 240, "blur core darkens the canvas");
        assert!(
            image.pixel(32, 32)[0] < image.pixel(12, 32)[0],
            "blur falls off outward"
        );
        assert_eq!(image.pixel(0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn sliding_box_blur_matches_naive_window() {
        let src: Vec<u8> = (0..8_u32)
            .flat_map(|i| {
                let base = (i * 37) as u8;
                [base, base.wrapping_mul(3), base.wrapping_mul(5), 255]
            })
            .collect();
        let radius: usize = 3;
        let mut fast = vec![0_u8; src.len()];
        sliding_box_rgba(&src, &mut fast, 8, radius);

        let mut naive = vec![0_u8; src.len()];
        for i in 0..8_usize {
            let start = i.saturating_sub(radius);
            let end = (i + radius).min(7);
            let mut sums = [0_u32; 4];
            for tap in start..=end {
                for channel in 0..4 {
                    sums[channel] += u32::from(src[tap * 4 + channel]);
                }
            }
            let count = (end - start + 1) as u32;
            for channel in 0..4 {
                naive[i * 4 + channel] = (sums[channel] / count) as u8;
            }
        }
        assert_eq!(fast, naive, "sliding window must be byte-identical");
    }

    #[test]
    fn exhausted_work_budget_degrades_without_hanging() {
        let mut list = DisplayList::new();
        for i in 0..8 {
            list.push(DisplayCommand::DrawShadow {
                rect: Rect::new(4.0 + i as f32, 4.0, 40.0, 40.0),
                radius: [8.0; 4],
                offset: (2.0, 2.0),
                blur: 12.0,
                color: Color::rgba(0, 0, 0, 120),
            });
            list.push(DisplayCommand::DrawTransformed {
                child_commands: vec![DisplayCommand::FillRect {
                    rect: Rect::new(4.0, 4.0, 30.0, 30.0),
                    color: Color::rgb(20, 40, 60),
                }],
                matrix: [1.0, 0.0, 0.0, 1.0, 8.0, 8.0],
                opacity: 0.5,
            });
            list.push(DisplayCommand::DrawFiltered {
                child_commands: vec![DisplayCommand::FillRect {
                    rect: Rect::new(4.0, 4.0, 30.0, 30.0),
                    color: Color::rgb(10, 20, 30),
                }],
                filters: vec![DisplayFilter::Blur(8.0)],
            });
        }
        let mut rasterizer = Rasterizer::new(viewport(), Color::WHITE);
        rasterizer.work_budget = WorkBudget(Arc::new(AtomicU64::new(0)));
        let image = rasterizer.execute(&list);
        assert_eq!((image.width, image.height), (64, 64));
        // Degraded shadows still paint (as a single rounded rect), so the
        // corner may be darkened; what matters is a finite, opaque, repeatable
        // result instead of a hang or NaN output.
        assert_eq!(image.pixel(0, 0)[3], 255);
        let mut again = Rasterizer::new(viewport(), Color::WHITE);
        again.work_budget = WorkBudget(Arc::new(AtomicU64::new(0)));
        assert_eq!(
            image,
            again.execute(&list),
            "degradation stays deterministic"
        );
    }

    /// Benchmark-style guard for the effect-heavy paths (shadows, transforms,
    /// filters) that dominate real pages. Mirrors the profile of a busy landing
    /// page (many box shadows, a handful of transformed groups and filters) on a
    /// 1280x800 canvas. Must stay well under half a second even in debug builds.
    #[test]
    fn effect_heavy_page_stays_under_budget() {
        let mut list = DisplayList::new();

        // 80 soft, offset box shadows spread across the canvas.
        for i in 0..80 {
            let col = i % 10;
            let row = i / 10;
            list.push(DisplayCommand::DrawShadow {
                rect: Rect::new(
                    12.0 + col as f32 * 126.0,
                    12.0 + row as f32 * 98.0,
                    92.0,
                    58.0,
                ),
                radius: [8.0; 4],
                offset: (2.0, 4.0),
                blur: 12.0,
                color: Color::rgba(0, 0, 0, 90),
            });
        }

        // 50 small, rotated child groups requiring inverse mapping.
        for i in 0..50 {
            let tx = 40.0 + (i % 10) as f32 * 124.0;
            let ty = 40.0 + (i / 10) as f32 * 150.0;
            let angle = (i as f32) * 0.17;
            let (cos, sin) = (angle.cos(), angle.sin());
            list.push(DisplayCommand::DrawTransformed {
                child_commands: vec![
                    DisplayCommand::FillRect {
                        rect: Rect::new(-14.0, -14.0, 28.0, 28.0),
                        color: Color::rgb(40, 90, 200),
                    },
                    DisplayCommand::DrawRoundedRect {
                        rect: Rect::new(-8.0, -8.0, 16.0, 16.0),
                        radius: [4.0; 4],
                        background: Color::rgb(230, 60, 40),
                        border: None,
                    },
                ],
                matrix: [cos, sin, -sin, cos, tx, ty],
                opacity: 1.0,
            });
        }

        // 20 colour-filtered regions.
        for i in 0..20 {
            let col = i % 5;
            let row = i / 5;
            list.push(DisplayCommand::DrawFiltered {
                child_commands: vec![DisplayCommand::FillRect {
                    rect: Rect::new(
                        30.0 + col as f32 * 250.0,
                        30.0 + row as f32 * 190.0,
                        180.0,
                        120.0,
                    ),
                    color: Color::rgb(200, 120, 60),
                }],
                filters: vec![
                    DisplayFilter::Brightness(0.85),
                    DisplayFilter::Contrast(1.1),
                ],
            });
        }

        let vp = Size {
            width: 1280.0,
            height: 800.0,
        };
        let start = Instant::now();
        let image = render(&list, vp, Color::WHITE);
        let elapsed = start.elapsed();
        println!("effect-heavy paint: {elapsed:?}");
        assert_eq!((image.width, image.height), (1280, 800));
        assert!(
            elapsed < Duration::from_millis(500),
            "effect-heavy paint took {elapsed:?}, expected < 500ms"
        );
    }

    /// Manual performance measurement: `cargo test -p browser-paint
    /// render_cache_performance -- --ignored --nocapture`.
    #[test]
    #[ignore = "manual performance measurement"]
    fn render_cache_performance() {
        // A page-shaped workload: many text runs over light background bands.
        let mut list = DisplayList::new();
        for i in 0..300 {
            let y = 6.0 + i as f32 * 2.0;
            list.push(DisplayCommand::DrawText {
                x: 4.0,
                baseline: y,
                text: "The quick brown fox jumps over the lazy dog".into(),
                font_size: 14.0,
                color: Color::rgb(20, 20, 20),
                underline: false,
                line_through: false,
                bold: false,
                italic: false,
            });
            list.push(DisplayCommand::FillRect {
                rect: Rect::new(0.0, y + 1.0, 600.0, 0.5),
                color: Color::rgb(235, 235, 235),
            });
        }
        let vp = Size {
            width: 800.0,
            height: 600.0,
        };
        const ITERS: u32 = 20;

        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let _ = render(&list, vp, Color::WHITE);
        }
        let cold = start.elapsed();

        let mut cache = RenderCache::new(2);
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            let _ = Rasterizer::render_cached(&list, vp, Color::WHITE, 1.0, &mut cache);
        }
        let warm = start.elapsed();

        println!(
            "cold avg {:?}, cached avg {:?}, speedup {:.1}x (hits {}, misses {})",
            cold / ITERS,
            warm / ITERS,
            cold.as_secs_f64() / warm.as_secs_f64().max(1e-9),
            cache.hits(),
            cache.misses()
        );
    }

    // --- executable paint effects -------------------------------------------

    fn filled(rect: Rect, color: Color) -> DisplayCommand {
        DisplayCommand::FillRect { rect, color }
    }

    #[test]
    fn masks_change_pixels() {
        let mut mask = CoverageMask::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                mask.samples[y * 16 + x] = if x < 8 { 255 } else { 0 };
            }
        }
        let build = |mask| {
            let mut list = DisplayList::new();
            list.push(DisplayCommand::DrawMasked {
                rect: Rect::new(0.0, 0.0, 16.0, 16.0),
                child_commands: vec![filled(
                    Rect::new(0.0, 0.0, 16.0, 16.0),
                    Color::rgb(255, 0, 0),
                )],
                mask,
            });
            render(&list, viewport(), Color::WHITE)
        };
        let alpha = build(MaskData::Alpha(mask.clone()));
        assert_eq!(alpha.pixel(4, 8), [255, 0, 0, 255], "unmasked half paints");
        assert_eq!(
            alpha.pixel(12, 8),
            [255, 255, 255, 255],
            "masked half stays blank"
        );
        // The pre-reduced luminance channel uses the same coverage path.
        let luminance = build(MaskData::Luminance(mask));
        assert_eq!(luminance.pixel(4, 8), alpha.pixel(4, 8));
        assert_eq!(luminance.pixel(12, 8), alpha.pixel(12, 8));
        // A URL mask cannot be resolved by the software rasterizer yet.
        let url = build(MaskData::Url("mask.png".into()));
        assert_eq!(url, render(&DisplayList::new(), viewport(), Color::WHITE));
    }

    #[test]
    fn clipped_group_respects_shape_coverage() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawClipped {
            child_commands: vec![filled(
                Rect::new(0.0, 0.0, 32.0, 32.0),
                Color::rgb(255, 0, 0),
            )],
            clip: ClipPath::parse("circle(50% at 50% 50%)").expect("circle clip"),
            rect: Rect::new(0.0, 0.0, 32.0, 32.0),
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(16, 16), [255, 0, 0, 255], "centre survives");
        assert_eq!(
            image.pixel(1, 1),
            [255, 255, 255, 255],
            "corner is clipped away"
        );
    }

    #[test]
    fn blend_mode_differs_from_normal() {
        let build = |mode, filters: crate::effects::BackdropFilters| {
            let mut list = DisplayList::new();
            list.push(filled(
                Rect::new(0.0, 0.0, 40.0, 40.0),
                Color::rgb(200, 100, 50),
            ));
            list.push(DisplayCommand::DrawBlended {
                child_commands: vec![filled(
                    Rect::new(16.0, 16.0, 16.0, 16.0),
                    Color::rgb(100, 200, 255),
                )],
                blend_mode: mode,
                backdrop: filters,
            });
            render(&list, viewport(), Color::WHITE)
        };
        let normal = build(BlendMode::Normal, Default::default());
        let multiply = build(BlendMode::Multiply, Default::default());
        let overlay = build(BlendMode::Overlay, Default::default());
        assert_eq!(normal.pixel(24, 24), [100, 200, 255, 255]);
        assert_ne!(
            multiply.pixel(24, 24),
            normal.pixel(24, 24),
            "multiply blends"
        );
        assert_ne!(
            overlay.pixel(24, 24),
            normal.pixel(24, 24),
            "overlay blends"
        );
        assert!(
            multiply.pixel(24, 24)[0] < normal.pixel(24, 24)[0],
            "multiply darkens red over a bright backdrop"
        );

        // Backdrop filters change the pixels the blend operates on.
        let filtered = build(
            BlendMode::Multiply,
            crate::effects::BackdropFilters::parse("brightness(0.5)"),
        );
        assert_ne!(filtered.pixel(24, 24), multiply.pixel(24, 24));
    }

    #[test]
    fn radial_gradient_hits_center_and_last_stop() {
        let gradient =
            RadialGradient::parse("radial-gradient(circle at 50% 50%, red 0%, blue 100%)")
                .expect("radial gradient");
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawRadialGradient {
            // Pixel (16, 16) has centre (16.5, 16.5), exactly the gradient centre.
            rect: Rect::new(0.5, 0.5, 32.0, 32.0),
            gradient,
            radius: [0.0; 4],
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(16, 16), [255, 0, 0, 255], "centre is stop 0");
        assert!(
            image.pixel(1, 1)[2] > 200,
            "far corner approaches the last stop"
        );
        assert_eq!(image.pixel(63, 63), [255, 255, 255, 255], "outside the box");
    }

    #[test]
    fn conic_gradient_hits_stops() {
        let gradient =
            ConicGradient::parse("conic-gradient(from 0deg at 50% 50%, red 0%, blue 100%)")
                .expect("conic gradient");
        let mut list = DisplayList::new();
        list.push(DisplayCommand::DrawConicGradient {
            rect: Rect::new(0.5, 0.5, 32.0, 32.0),
            gradient,
            radius: [0.0; 4],
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(16, 1), [255, 0, 0, 255], "12 o'clock is t=0");
        assert!(
            image.pixel(32, 16)[2] > 0,
            "3 o'clock is a quarter of the way to blue"
        );
        assert_eq!(image.pixel(63, 63), [255, 255, 255, 255], "outside the box");
    }

    #[test]
    fn border_image_scales_nine_slices() {
        use crate::effects::{BorderImageData, CssLength};

        let mut source = ImageData::new(3, 3);
        // Row-major: three corners, three edge centres, three corners.
        let colors: [[u8; 3]; 9] = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 0],
            [0, 0, 0],
            [255, 0, 255],
            [0, 255, 255],
            [128, 128, 128],
            [255, 255, 255],
        ];
        for (index, rgb) in colors.iter().enumerate() {
            source.set(
                (index % 3) as u32,
                (index / 3) as u32,
                rgb[0],
                rgb[1],
                rgb[2],
                255,
            );
        }
        let border_image = BorderImageData {
            source: "url(border.png)".into(),
            slice: [CssLength::Px(1.0); 4],
            width: [CssLength::Px(4.0); 4],
            outset: [CssLength::Px(0.0); 4],
            repeat: [BorderImageRepeat::Stretch; 2],
        };
        let mut list = DisplayList::new();
        let index = list.add_image(source.clone());
        list.push(DisplayCommand::DrawBorderImage {
            rect: Rect::new(0.0, 0.0, 12.0, 12.0),
            border_image: border_image.clone(),
            image: index,
        });
        let image = render(&list, viewport(), Color::WHITE);
        assert_eq!(image.pixel(2, 2), [255, 0, 0, 255], "top-left corner");
        assert_eq!(image.pixel(10, 2), [0, 0, 255, 255], "top-right corner");
        assert_eq!(image.pixel(2, 10), [0, 255, 255, 255], "bottom-left corner");
        assert_eq!(image.pixel(6, 6), [0, 0, 0, 255], "stretched centre");

        // border-image-outset paints outside the original border box.
        let mut outset = border_image;
        outset.outset = [CssLength::Px(2.0); 4];
        let mut list = DisplayList::new();
        let index = list.add_image(source.clone());
        list.push(DisplayCommand::DrawBorderImage {
            rect: Rect::new(0.0, 0.0, 12.0, 12.0),
            border_image: outset,
            image: index,
        });
        let painted = render(&list, viewport(), Color::WHITE);
        assert_eq!(
            painted.pixel(0, 0),
            [255, 0, 0, 255],
            "outset corner is painted outside the box"
        );
    }

    #[test]
    fn image_rendering_nearest_matches_legacy_and_smooth_interpolates() {
        let mut source = ImageData::new(2, 2);
        source.set(0, 0, 255, 0, 0, 255);
        source.set(1, 0, 0, 255, 0, 255);
        source.set(0, 1, 0, 0, 255, 255);
        source.set(1, 1, 255, 255, 255, 255);
        let build = |command: fn(usize) -> DisplayCommand| {
            let mut list = DisplayList::new();
            let index = list.add_image(source.clone());
            list.push(command(index));
            render(&list, viewport(), Color::WHITE)
        };
        let legacy = build(|image| DisplayCommand::DrawImage {
            rect: Rect::new(0.0, 0.0, 20.0, 20.0),
            image,
        });
        let nearest = build(|image| DisplayCommand::DrawImageWithRendering {
            rect: Rect::new(0.0, 0.0, 20.0, 20.0),
            image,
            rendering: ImageRendering::Pixelated,
        });
        assert_eq!(legacy, nearest, "pixelated is byte-identical to DrawImage");
        assert_eq!(nearest.pixel(5, 5), [255, 0, 0, 255], "crisp texel");

        let smooth = build(|image| DisplayCommand::DrawImageWithRendering {
            rect: Rect::new(0.0, 0.0, 20.0, 20.0),
            image,
            rendering: ImageRendering::Smooth,
        });
        assert_ne!(
            smooth.pixel(8, 8),
            nearest.pixel(8, 8),
            "bilinear blends neighbouring texels"
        );
    }
}
