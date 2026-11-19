//! Extended paint effects: command data, CSS parsers and a coverage-mask rasterizer.
//!
//! This module is deliberately self-contained. `crate::display_list::DisplayCommand` and
//! `crate::raster::Rasterizer` are owned by other agents, so the effects below are exposed
//! as *command data* ([`ExtendedCommand`]) plus a reusable coverage rasterizer
//! ([`CoverageMask`], [`rasterize_clip_path`]) instead of being added as `DisplayCommand`
//! variants here. Everything in this module compiles and is unit-tested on its own.

//! ## Integration point for the raster / display-list owner
//!
//! To execute these effects, the owner of `display_list.rs` and `raster.rs` must:
//!
//! 1. Add matching variants to `DisplayCommand` (in `display_list.rs`) using the payloads
//!    defined here — e.g. `DrawRadialGradient { rect, gradient, radius }`,
//!    `DrawConicGradient { rect, gradient, radius }`, `DrawMasked { child_commands, mask }`,
//!    `DrawBlended { child_commands, blend_mode, backdrop }`. Add the corresponding arms to
//!    `DisplayCommand::hash_into` so `identity_hash` stays stable.
//! 2. Add arms to `raster::Rasterizer::draw` (and `ProfileKind::of` / `NAMES` / `COUNT`):
//!    - **radial/conic**: for each covered pixel call [`sample_radial`] / [`sample_conic`],
//!      which interpolate [`GradientStop`]s in the same way as the linear path.
//!    - **mask / clip-path**: paint `child_commands` into a transparent temporary
//!      `ImageBuffer`, multiply each pixel by [`CoverageMask::coverage_at`] (or use
//!      [`rasterize_clip_path`] for a clip), then composite the result onto the parent.
//!      Rectangular `clip-path: inset(...)` can instead be lowered to the existing
//!      `PushClip`/`PopClip` pair (see [`clip_path_to_rect`]) and needs no raster change.
//!    - **blend / backdrop**: paint the backdrop and the group into two buffers, apply
//!      [`BlendMode::blend`] per channel and the [`BackdropFilters`] to the backdrop before
//!      blending. This is compositor work; the paint crate only supplies the data.
//! 3. Add a `rendering: ImageRendering` field to `DrawImage` and choose the sampler:
//!    `Pixelated` keeps `ImageData::sample` (nearest neighbour); `Auto`/`Smooth` should use
//!    a bilinear sampler in `raster::draw_image`.
//! 4. `border-image` (nine-slice scaling) has no existing primitive — execute
//!    [`BorderImageData`] with a dedicated nine-slice blit. `outline` is already executable
//!    via [`crate::borders::outline_command`] (a `DrawBorder` on an inflated rect).
//!
//! The style engine (`browser_style`) also needs new `Property` variants for these
//! declarations; until then [`RawEffects`] carries the raw declaration strings that the
//! parsers in this module consume ([`build_effects_with`]). Rectangular clip paths and
//! radial/conic `background-image` values are already reachable through computed style.

use browser_css::{Color, Value};
use browser_dom::NodeId;
use browser_layout::Rect;
use browser_style::{BorderStyle, ComputedValue};

use crate::display_list::{DisplayCommand, DisplayFilter};

use std::collections::HashMap;

/// Number of sub-samples per axis used by the coverage rasterizer (4x4 = 16 taps).
const AA_SUBSAMPLES: u32 = 4;

// ---------------------------------------------------------------------------
// Shared value types
// ---------------------------------------------------------------------------

/// A CSS `<length-percentage>` used by gradient geometry and clipping shapes.
///
/// Percentages are stored as fractions (`0.5` means `50%`) so a single value can be
/// resolved against a box dimension at rasterization time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CssLength {
    /// A percentage of the reference length, stored as a fraction.
    Percent(f32),
    /// An absolute length in CSS pixels.
    Px(f32),
}

impl Default for CssLength {
    fn default() -> Self {
        Self::Percent(0.5)
    }
}

impl CssLength {
    /// Resolves the value against `reference`.
    pub fn resolve(self, reference: f32) -> f32 {
        match self {
            Self::Percent(fraction) => fraction * reference,
            Self::Px(length) => length,
        }
    }

    /// Parses a length or percentage token.
    ///
    /// Percentages are stored as fractions (`50%` becomes `Percent(0.5)`).
    /// Unitless numbers are accepted as pixels (CSS allows them for
    /// `border-image-slice` and gradient sizes).
    pub fn parse(input: &str) -> Option<Self> {
        let text = input.trim().to_ascii_lowercase();
        if let Some(number) = text.strip_suffix('%') {
            let value: f32 = number.trim().parse().ok()?;
            return Some(Self::Percent(value / 100.0));
        }
        if let Some(number) = text.strip_suffix("px") {
            return number.trim().parse::<f32>().ok().map(Self::Px);
        }
        text.parse::<f32>().ok().map(Self::Px)
    }
}

/// A position inside a box, expressed as a pair of `<length-percentage>` values.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GradientCenter {
    /// Horizontal coordinate (`0.5` = centre by default).
    pub x: CssLength,
    /// Vertical coordinate (`0.5` = centre by default).
    pub y: CssLength,
}

impl GradientCenter {
    /// The default centre (`50% 50%`).
    pub const CENTER: Self = Self {
        x: CssLength::Percent(0.5),
        y: CssLength::Percent(0.5),
    };

    /// Resolves the centre to CSS pixels inside `rect`.
    pub fn resolve(self, rect: Rect) -> (f32, f32) {
        (
            rect.x + self.x.resolve(rect.width),
            rect.y + self.y.resolve(rect.height),
        )
    }
}

/// A gradient colour stop with a position in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// Position along the gradient line.
    pub position: f32,
    /// Stop colour.
    pub color: Color,
}

/// Interpolates sorted [`GradientStop`]s at parameter `t` (`0.0..=1.0`).
pub fn sample_gradient(stops: &[GradientStop], t: f32) -> Color {
    if stops.is_empty() {
        return Color::transparent();
    }
    let first = stops[0];
    let last = stops[stops.len() - 1];
    if t <= first.position {
        return first.color;
    }
    if t >= last.position {
        return last.color;
    }
    for index in 1..stops.len() {
        let current = stops[index];
        if t <= current.position {
            let previous = stops[index - 1];
            let span = (current.position - previous.position).max(f32::EPSILON);
            let fraction = ((t - previous.position) / span).clamp(0.0, 1.0);
            let mix = |a: u8, b: u8| {
                (f32::from(a) * (1.0 - fraction) + f32::from(b) * fraction).round() as u8
            };
            return Color::rgba(
                mix(previous.color.r, current.color.r),
                mix(previous.color.g, current.color.g),
                mix(previous.color.b, current.color.b),
                mix(previous.color.a, current.color.a),
            );
        }
    }
    last.color
}

/// Splits a string on commas that are not nested inside parentheses.
fn split_top_level(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0_usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&input[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

/// Returns the inner text of `name(...)`, or `None` when `input` is not that function.
fn function_arguments<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = input.trim();
    let prefix = format!("{name}(");
    if !trimmed.to_ascii_lowercase().starts_with(prefix.as_str()) {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut depth = 1_i32;
    for (index, &byte) in bytes.iter().enumerate().skip(prefix.len()) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&trimmed[prefix.len()..index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits a colour-stop token into colour text and optional position (`0.0..=1.0`).
fn split_stop(token: &str) -> (&str, Option<f32>) {
    let trimmed = token.trim();
    if trimmed.ends_with('%')
        && let Some(index) = trimmed.rfind(char::is_whitespace)
    {
        let color = trimmed[..index].trim();
        if let Ok(position) = trimmed[index..].trim().trim_end_matches('%').parse::<f32>() {
            return (color, Some((position / 100.0).clamp(0.0, 1.0)));
        }
    }
    (trimmed, None)
}

/// Parses gradient stops, spacing unpositioned stops evenly like CSS does.
fn parse_gradient_stops(parts: &[&str]) -> Option<Vec<GradientStop>> {
    let mut stops: Vec<(Option<f32>, Color)> = Vec::new();
    for part in parts {
        let (color_text, position) = split_stop(part);
        let color = Value::parse(color_text).as_color()?;
        stops.push((position, color));
    }
    if stops.len() < 2 {
        return None;
    }
    let count = stops.len();
    let mut resolved: Vec<GradientStop> = stops
        .into_iter()
        .enumerate()
        .map(|(index, (position, color))| GradientStop {
            position: position.unwrap_or(index as f32 / (count - 1) as f32),
            color,
        })
        .collect();
    resolved.sort_by(|a, b| a.position.total_cmp(&b.position));
    Some(resolved)
}

// ---------------------------------------------------------------------------
// Blend modes and backdrop filters (compositor data)
// ---------------------------------------------------------------------------

/// A CSS `mix-blend-mode` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// No blending (default).
    #[default]
    Normal,
    /// `multiply`.
    Multiply,
    /// `screen`.
    Screen,
    /// `overlay`.
    Overlay,
    /// `darken`.
    Darken,
    /// `lighten`.
    Lighten,
    /// `color-dodge`.
    ColorDodge,
    /// `color-burn`.
    ColorBurn,
    /// `hard-light`.
    HardLight,
    /// `soft-light`.
    SoftLight,
    /// `difference`.
    Difference,
    /// `exclusion`.
    Exclusion,
    /// `hue`.
    Hue,
    /// `saturation`.
    Saturation,
    /// `color`.
    Color,
    /// `luminosity`.
    Luminosity,
    /// `plus-lighter`.
    PlusLighter,
}

impl BlendMode {
    /// Parses a keyword, returning `None` for unknown values.
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "multiply" => Some(Self::Multiply),
            "screen" => Some(Self::Screen),
            "overlay" => Some(Self::Overlay),
            "darken" => Some(Self::Darken),
            "lighten" => Some(Self::Lighten),
            "color-dodge" => Some(Self::ColorDodge),
            "color-burn" => Some(Self::ColorBurn),
            "hard-light" => Some(Self::HardLight),
            "soft-light" => Some(Self::SoftLight),
            "difference" => Some(Self::Difference),
            "exclusion" => Some(Self::Exclusion),
            "hue" => Some(Self::Hue),
            "saturation" => Some(Self::Saturation),
            "color" => Some(Self::Color),
            "luminosity" => Some(Self::Luminosity),
            "plus-lighter" => Some(Self::PlusLighter),
            _ => None,
        }
    }

    /// Blends two 8-bit channel values (0 = backdrop, 1 = source).
    ///
    /// Separable modes use the standard CSS blend formulas on normalised channels;
    /// non-separable modes (`hue`, `saturation`, `color`, `luminosity`) fall back to
    /// `Normal` here because they need HSL conversion in the compositor.
    pub fn blend(self, backdrop: u8, source: u8) -> u8 {
        let b = f32::from(backdrop) / 255.0;
        let s = f32::from(source) / 255.0;
        let result = match self {
            Self::Normal | Self::Hue | Self::Saturation | Self::Color | Self::Luminosity => s,
            Self::Multiply => b * s,
            Self::Screen => b + s - b * s,
            Self::Overlay => hard_light(s, b),
            Self::Darken => b.min(s),
            Self::Lighten => b.max(s),
            Self::ColorDodge => {
                if s >= 1.0 {
                    1.0
                } else if b <= 0.0 {
                    0.0
                } else {
                    (b / (1.0 - s)).min(1.0)
                }
            }
            Self::ColorBurn => {
                if s <= 0.0 || b >= 1.0 {
                    1.0
                } else {
                    (1.0 - (1.0 - b) / s).max(0.0)
                }
            }
            Self::HardLight => hard_light(b, s),
            Self::SoftLight => {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                if s <= 0.5 {
                    b - (1.0 - 2.0 * s) * b * (1.0 - b)
                } else {
                    b + (2.0 * s - 1.0) * (d - b)
                }
            }
            Self::Difference => (b - s).abs(),
            Self::Exclusion => b + s - 2.0 * b * s,
            Self::PlusLighter => (b + s).min(1.0),
        };
        (result.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

fn hard_light(source: f32, backdrop: f32) -> f32 {
    if source <= 0.5 {
        2.0 * source * backdrop
    } else {
        1.0 - 2.0 * (1.0 - source) * (1.0 - backdrop)
    }
}

/// An ordered list of backdrop-filter operations for the compositor.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BackdropFilters(pub Vec<DisplayFilter>);

impl BackdropFilters {
    /// True when no filter is applied.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of filters in the chain.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Parses a space-separated `backdrop-filter` list.
    ///
    /// Unrecognised functions (for example `url(...)`) are ignored, matching how
    /// `crate::commands::display_filters` treats raw CSS filters.
    pub fn parse(input: &str) -> Self {
        let text = input.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("none") {
            return Self::default();
        }
        let mut filters = Vec::new();
        for token in split_whitespace_top_level(text) {
            let lower = token.to_ascii_lowercase();
            let number = |name: &str| parse_function_number(&lower, name);
            let filter = if lower.starts_with("blur(") {
                number("blur").map(DisplayFilter::Blur)
            } else if lower.starts_with("brightness(") {
                number("brightness").map(DisplayFilter::Brightness)
            } else if lower.starts_with("contrast(") {
                number("contrast").map(DisplayFilter::Contrast)
            } else if lower.starts_with("grayscale(") {
                number("grayscale").map(DisplayFilter::Grayscale)
            } else if lower.starts_with("hue-rotate(") {
                number("hue-rotate").map(DisplayFilter::HueRotate)
            } else if lower.starts_with("invert(") {
                number("invert").map(DisplayFilter::Invert)
            } else if lower.starts_with("opacity(") {
                number("opacity").map(DisplayFilter::Opacity)
            } else if lower.starts_with("saturate(") {
                number("saturate").map(DisplayFilter::Saturate)
            } else if lower.starts_with("sepia(") {
                number("sepia").map(DisplayFilter::Sepia)
            } else {
                None
            };
            if let Some(filter) = filter {
                filters.push(filter);
            }
        }
        Self(filters)
    }
}

fn parse_function_number(text: &str, name: &str) -> Option<f32> {
    let prefix = format!("{name}(");
    let inside = text
        .strip_prefix(prefix.as_str())?
        .strip_suffix(')')?
        .trim();
    let percentage = inside.ends_with('%');
    let number = inside
        .trim_end_matches('%')
        .trim_end_matches("px")
        .trim_end_matches("deg")
        .trim();
    let value: f32 = number.parse().ok()?;
    Some(if percentage { value / 100.0 } else { value })
}

/// Splits on whitespace that is not nested inside parentheses.
fn split_whitespace_top_level(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0_usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if start != index {
                    parts.push(&input[start..index]);
                }
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    if start < input.len() {
        parts.push(&input[start..]);
    }
    parts
}

// ---------------------------------------------------------------------------
// image-rendering
// ---------------------------------------------------------------------------

/// A CSS `image-rendering` hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageRendering {
    /// Browser default (smooth for photos, nearest for pixel art at large scales).
    #[default]
    Auto,
    /// Smooth (bilinear) interpolation.
    Smooth,
    /// Crisp edges: preserve contrast, prefer smooth where possible.
    CrispEdges,
    /// Nearest-neighbour interpolation.
    Pixelated,
}

impl ImageRendering {
    /// Parses a keyword, returning `None` for unknown values.
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "smooth" => Some(Self::Smooth),
            "crisp-edges" => Some(Self::CrispEdges),
            "pixelated" => Some(Self::Pixelated),
            _ => None,
        }
    }

    /// True when nearest-neighbour sampling should be used.
    pub fn is_pixelated(self) -> bool {
        matches!(self, Self::Pixelated | Self::CrispEdges)
    }
}

// ---------------------------------------------------------------------------
// Radial and conic gradients
// ---------------------------------------------------------------------------

/// The shape of a radial gradient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RadialShape {
    /// A circular gradient.
    Circle,
    /// An elliptical gradient (the CSS default).
    #[default]
    Ellipse,
}

/// The size of a radial gradient's ending shape.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum RadialSize {
    /// `farthest-corner` (the CSS default).
    #[default]
    FarthestCorner,
    /// `closest-corner`.
    ClosestCorner,
    /// `farthest-side`.
    FarthestSide,
    /// `closest-side`.
    ClosestSide,
    /// Explicit horizontal and vertical radii.
    Explicit(CssLength, CssLength),
}

/// A parsed CSS `radial-gradient()`.
#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    /// Ending shape.
    pub shape: RadialShape,
    /// Ending shape size.
    pub size: RadialSize,
    /// Centre of the gradient.
    pub center: GradientCenter,
    /// Colour stops in ascending position order.
    pub stops: Vec<GradientStop>,
}

impl RadialGradient {
    /// Parses a `radial-gradient(...)` string.
    pub fn parse(input: &str) -> Option<Self> {
        let inner = function_arguments(input, "radial-gradient")?;
        let parts = split_top_level(inner);
        if parts.len() < 2 {
            return None;
        }
        let preamble = parts[0].trim().to_ascii_lowercase();
        let (position_text, before_position) = match preamble.find(" at ") {
            Some(index) => (Some(preamble[index + 4..].trim()), &preamble[..index]),
            None => (None, preamble.as_str()),
        };
        let mut shape = RadialShape::Ellipse;
        let mut size = RadialSize::FarthestCorner;
        let mut explicit = Vec::new();
        for token in before_position.split_whitespace() {
            match token {
                "circle" => shape = RadialShape::Circle,
                "ellipse" => shape = RadialShape::Ellipse,
                "closest-corner" => size = RadialSize::ClosestCorner,
                "farthest-corner" => size = RadialSize::FarthestCorner,
                "closest-side" => size = RadialSize::ClosestSide,
                "farthest-side" => size = RadialSize::FarthestSide,
                _ => {
                    if let Some(length) = CssLength::parse(token) {
                        explicit.push(length);
                    }
                }
            }
        }
        if explicit.len() == 1 {
            shape = RadialShape::Circle;
            size = RadialSize::Explicit(explicit[0], explicit[0]);
        } else if explicit.len() >= 2 {
            shape = RadialShape::Ellipse;
            size = RadialSize::Explicit(explicit[0], explicit[1]);
        }
        let center = position_text
            .and_then(parse_center)
            .unwrap_or(GradientCenter::CENTER);
        let stops = parse_gradient_stops(&parts[1..])?;
        Some(Self {
            shape,
            size,
            center,
            stops,
        })
    }

    /// Computes the ending radii in CSS pixels for `rect`.
    fn radii(&self, rect: Rect) -> (f32, f32) {
        let (cx, cy) = self.center.resolve(rect);
        let left = cx - rect.x;
        let right = rect.right() - cx;
        let top = cy - rect.y;
        let bottom = rect.bottom() - cy;
        let (closest_x, farthest_x) = (left.min(right), left.max(right));
        let (closest_y, farthest_y) = (top.min(bottom), top.max(bottom));
        let (mut rx, mut ry) = match self.size {
            RadialSize::FarthestCorner => {
                let radius = farthest_x.hypot(farthest_y);
                (radius, radius)
            }
            RadialSize::ClosestCorner => {
                let value = closest_x.hypot(closest_y);
                (value, value)
            }
            RadialSize::FarthestSide => (farthest_x, farthest_y),
            RadialSize::ClosestSide => (closest_x, closest_y),
            RadialSize::Explicit(x, y) => {
                let base_y = if self.shape == RadialShape::Circle {
                    rect.width
                } else {
                    rect.height
                };
                (x.resolve(rect.width), y.resolve(base_y))
            }
        };
        if self.shape == RadialShape::Circle {
            let radius = rx.max(ry);
            rx = radius;
            ry = radius;
        }
        (rx.max(f32::EPSILON), ry.max(f32::EPSILON))
    }
}

/// Samples a radial gradient at the point `(x, y)` in CSS pixels.
pub fn sample_radial(gradient: &RadialGradient, rect: Rect, x: f32, y: f32) -> Color {
    let (cx, cy) = gradient.center.resolve(rect);
    let (rx, ry) = gradient.radii(rect);
    let dx = (x - cx) / rx;
    let dy = (y - cy) / ry;
    sample_gradient(&gradient.stops, dx.hypot(dy))
}

/// A parsed CSS `conic-gradient()`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConicGradient {
    /// Starting angle in degrees (0 = up, increasing clockwise).
    pub from_deg: f32,
    /// Centre of the gradient.
    pub center: GradientCenter,
    /// Colour stops in ascending position order.
    pub stops: Vec<GradientStop>,
}

impl ConicGradient {
    /// Parses a `conic-gradient(...)` string.
    pub fn parse(input: &str) -> Option<Self> {
        let inner = function_arguments(input, "conic-gradient")?;
        let parts = split_top_level(inner);
        if parts.len() < 2 {
            return None;
        }
        let preamble = parts[0].trim().to_ascii_lowercase();
        let mut from_deg = 0.0_f32;
        let mut center = GradientCenter::CENTER;
        let tokens = split_whitespace_top_level(&preamble);
        let mut index = 0;
        while index < tokens.len() {
            if tokens[index] == "from" && index + 1 < tokens.len() {
                from_deg = crate::commands::parse_angle(tokens[index + 1]).unwrap_or(0.0);
                index += 2;
            } else if tokens[index] == "at" {
                let rest = tokens[index + 1..].join(" ");
                center = parse_center(&rest).unwrap_or(GradientCenter::CENTER);
                break;
            } else {
                index += 1;
            }
        }
        let stops = parse_gradient_stops(&parts[1..])?;
        Some(Self {
            from_deg,
            center,
            stops,
        })
    }
}

/// Samples a conic gradient at the point `(x, y)` in CSS pixels.
pub fn sample_conic(gradient: &ConicGradient, rect: Rect, x: f32, y: f32) -> Color {
    let (cx, cy) = gradient.center.resolve(rect);
    // CSS conic angles start at 12 o'clock and grow clockwise.
    let degrees = (y - cy).atan2(x - cx).to_degrees() + 90.0;
    let t = (degrees - gradient.from_deg).rem_euclid(360.0) / 360.0;
    sample_gradient(&gradient.stops, t)
}

/// Parses `center`, `top`, `left 20%`, `50% 25%` and similar position values.
fn parse_center(input: &str) -> Option<GradientCenter> {
    let text = input.trim().to_ascii_lowercase();
    let mut words = text.split_whitespace();
    let first = words.next()?;
    let second = words.next();
    let length = |word: &str| -> CssLength {
        match word {
            "center" => CssLength::Percent(0.5),
            "top" | "left" => CssLength::Percent(0.0),
            "bottom" | "right" => CssLength::Percent(1.0),
            other => CssLength::parse(other).unwrap_or(CssLength::Percent(0.5)),
        }
    };
    let (x, y) = match second {
        Some(second) => (length(first), length(second)),
        None => match first {
            "top" => (CssLength::Percent(0.5), CssLength::Percent(0.0)),
            "bottom" => (CssLength::Percent(0.5), CssLength::Percent(1.0)),
            "left" => (CssLength::Percent(0.0), CssLength::Percent(0.5)),
            "right" => (CssLength::Percent(1.0), CssLength::Percent(0.5)),
            "center" => (CssLength::Percent(0.5), CssLength::Percent(0.5)),
            other => {
                let value = CssLength::parse(other).unwrap_or(CssLength::Percent(0.5));
                (value, CssLength::Percent(0.5))
            }
        },
    };
    Some(GradientCenter { x, y })
}

// ---------------------------------------------------------------------------
// clip-path and coverage masks
// ---------------------------------------------------------------------------

/// A single point of a `polygon()` clip, as two `<length-percentage>` values.
pub type PolygonPoint = (CssLength, CssLength);

/// A parsed CSS `clip-path` basic shape.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipPath {
    /// `inset()` with the four inset distances (top, right, bottom, left).
    Inset {
        /// Distance from the top edge.
        top: CssLength,
        /// Distance from the right edge.
        right: CssLength,
        /// Distance from the bottom edge.
        bottom: CssLength,
        /// Distance from the left edge.
        left: CssLength,
    },
    /// `circle()` with an optional radius (`None` means `farthest-corner`).
    Circle {
        /// Centre of the circle.
        center: GradientCenter,
        /// Radius, or `None` for the default `farthest-corner`.
        radius: Option<CssLength>,
    },
    /// `ellipse()` with optional horizontal/vertical radii.
    Ellipse {
        /// Centre of the ellipse.
        center: GradientCenter,
        /// Radii, or `None` for the default `farthest-corner`.
        radii: Option<(CssLength, CssLength)>,
    },
    /// `polygon()` with points expressed relative to the reference box.
    Polygon(Vec<PolygonPoint>),
}

impl ClipPath {
    /// Parses a `clip-path` basic shape. `none` returns `None`.
    pub fn parse(input: &str) -> Option<Self> {
        let text = input.trim();
        if text.eq_ignore_ascii_case("none") {
            return None;
        }
        if let Some(inner) = function_arguments(text, "inset") {
            let values = expand_four(&split_whitespace_top_level(inner))?;
            return Some(Self::Inset {
                top: values[0],
                right: values[1],
                bottom: values[2],
                left: values[3],
            });
        }
        if let Some(inner) = function_arguments(text, "circle") {
            let (center, lengths) = parse_shape_arguments(inner);
            return Some(Self::Circle {
                center,
                radius: lengths.first().copied(),
            });
        }
        if let Some(inner) = function_arguments(text, "ellipse") {
            let (center, lengths) = parse_shape_arguments(inner);
            let radii = if lengths.len() >= 2 {
                Some((lengths[0], lengths[1]))
            } else {
                None
            };
            return Some(Self::Ellipse { center, radii });
        }
        if let Some(inner) = function_arguments(text, "polygon") {
            let mut points = Vec::new();
            for item in split_top_level(inner) {
                let words = split_whitespace_top_level(item.trim());
                if words.len() != 2 {
                    return None;
                }
                points.push((CssLength::parse(words[0])?, CssLength::parse(words[1])?));
            }
            if points.len() < 3 {
                return None;
            }
            return Some(Self::Polygon(points));
        }
        None
    }

    /// If the shape is an axis-aligned rectangle, returns its rect in CSS pixels.
    ///
    /// Callers can lower this to the existing `PushClip` / `PopClip` commands without
    /// any rasterizer changes.
    pub fn clip_path_to_rect(&self, rect: Rect) -> Option<Rect> {
        match *self {
            Self::Inset {
                top,
                right,
                bottom,
                left,
            } => {
                let left_edge = rect.x + left.resolve(rect.width);
                let top_edge = rect.y + top.resolve(rect.height);
                let right_edge = rect.right() - right.resolve(rect.width);
                let bottom_edge = rect.bottom() - bottom.resolve(rect.height);
                Some(Rect::new(
                    left_edge,
                    top_edge,
                    (right_edge - left_edge).max(0.0),
                    (bottom_edge - top_edge).max(0.0),
                ))
            }
            _ => None,
        }
    }
}

/// Convenience free function mirroring [`ClipPath::clip_path_to_rect`].
pub fn clip_path_to_rect(clip: &ClipPath, rect: Rect) -> Option<Rect> {
    clip.clip_path_to_rect(rect)
}

fn parse_shape_arguments(inner: &str) -> (GradientCenter, Vec<CssLength>) {
    let lower = inner.trim().to_ascii_lowercase();
    let (position, lengths_text) = match lower.find(" at ") {
        Some(index) => (&lower[index + 4..], &lower[..index]),
        None => ("", lower.as_str()),
    };
    let center = if position.is_empty() {
        GradientCenter::CENTER
    } else {
        parse_center(position).unwrap_or(GradientCenter::CENTER)
    };
    let lengths = lengths_text
        .split_whitespace()
        .filter_map(CssLength::parse)
        .collect();
    (center, lengths)
}

/// Expands a 1-to-4 element shorthand into `[top, right, bottom, left]`.
fn expand_four(values: &[&str]) -> Option<[CssLength; 4]> {
    let parsed: Vec<CssLength> = values
        .iter()
        .map(|value| CssLength::parse(value))
        .collect::<Option<Vec<_>>>()?;
    match parsed.as_slice() {
        [a] => Ok([*a, *a, *a, *a]),
        [a, b] => Ok([*a, *b, *a, *b]),
        [a, b, c] => Ok([*a, *b, *c, *b]),
        [a, b, c, d] => Ok([*a, *b, *c, *d]),
        _ => Err(()),
    }
    .ok()
}

/// A single-channel coverage mask (0 = fully clipped, 255 = fully visible).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CoverageMask {
    /// Mask width in pixels.
    pub width: u32,
    /// Mask height in pixels.
    pub height: u32,
    /// Row-major coverage bytes.
    pub samples: Vec<u8>,
}

impl CoverageMask {
    /// Creates a fully transparent mask.
    pub fn new(width: u32, height: u32) -> Self {
        let width = width.min(crate::images::MAX_IMAGE_DIMENSION);
        let height = height.min(crate::images::MAX_IMAGE_DIMENSION);
        Self {
            width,
            height,
            samples: vec![0; (width as usize).saturating_mul(height as usize)],
        }
    }

    /// Coverage (0.0 ..= 1.0) at device pixel `(x, y)`.
    pub fn coverage_at(&self, x: u32, y: u32) -> f32 {
        if x >= self.width || y >= self.height {
            return 0.0;
        }
        f32::from(self.samples[y as usize * self.width as usize + x as usize]) / 255.0
    }
}

/// Rasterizes a [`ClipPath`] into an anti-aliased coverage mask covering `rect`.
///
/// Four-by-four supersampling is used, matching the coverage quality of the existing
/// rounded-rectangle path in `raster.rs`. The resulting mask is directly usable by the
/// raster owner when executing a `DrawMasked` / clip-path command.
pub fn rasterize_clip_path(clip: &ClipPath, rect: Rect, width: u32, height: u32) -> CoverageMask {
    let mut mask = CoverageMask::new(width, height);
    if rect.is_empty() || width == 0 || height == 0 {
        return mask;
    }
    let (center_x, center_y) = match clip {
        ClipPath::Circle { center, .. } | ClipPath::Ellipse { center, .. } => {
            (center.x.resolve(rect.width), center.y.resolve(rect.height))
        }
        _ => (0.0, 0.0),
    };
    let circle_radius = match clip {
        ClipPath::Circle { radius, .. } => {
            radius.map(|value| value.resolve(rect.width.max(rect.height) / 2.0))
        }
        _ => None,
    };
    let ellipse_radii = match clip {
        ClipPath::Ellipse { radii, .. } => {
            radii.map(|(rx, ry)| (rx.resolve(rect.width), ry.resolve(rect.height)))
        }
        _ => None,
    };
    let inset = match clip {
        ClipPath::Inset {
            top,
            right,
            bottom,
            left,
        } => Some((
            top.resolve(rect.height),
            right.resolve(rect.width),
            bottom.resolve(rect.height),
            left.resolve(rect.width),
        )),
        _ => None,
    };
    let polygon: Vec<(f32, f32)> = match clip {
        ClipPath::Polygon(points) => points
            .iter()
            .map(|(x, y)| (x.resolve(rect.width), y.resolve(rect.height)))
            .collect(),
        _ => Vec::new(),
    };
    let samples_count = (AA_SUBSAMPLES * AA_SUBSAMPLES) as f32;
    for (index, sample) in mask.samples.iter_mut().enumerate() {
        let mx = index as u32 % width;
        let my = index as u32 / width;
        let mut hits = 0_u32;
        for sy in 0..AA_SUBSAMPLES {
            let py = (my as f32 + (sy as f32 + 0.5) / AA_SUBSAMPLES as f32) / height as f32;
            for sx in 0..AA_SUBSAMPLES {
                let px = (mx as f32 + (sx as f32 + 0.5) / AA_SUBSAMPLES as f32) / width as f32;
                let x = px * rect.width;
                let y = py * rect.height;
                let inside = match clip {
                    ClipPath::Inset { .. } => {
                        let (top, right, bottom, left) = inset.expect("inset values are set");
                        x >= left
                            && y >= top
                            && x <= rect.width - right
                            && y <= rect.height - bottom
                    }
                    ClipPath::Circle { .. } => {
                        let radius = circle_radius
                            .unwrap_or_else(|| farthest_corner_radius(center_x, center_y, rect));
                        (x - center_x).hypot(y - center_y) <= radius
                    }
                    ClipPath::Ellipse { .. } => {
                        let (rx, ry) = ellipse_radii.unwrap_or_else(|| {
                            let radius = farthest_corner_radius(center_x, center_y, rect);
                            (radius, radius)
                        });
                        let dx = (x - center_x) / rx.max(f32::EPSILON);
                        let dy = (y - center_y) / ry.max(f32::EPSILON);
                        dx * dx + dy * dy <= 1.0
                    }
                    ClipPath::Polygon(_) => point_in_polygon(x, y, &polygon),
                };
                if inside {
                    hits += 1;
                }
            }
        }
        *sample = (hits as f32 * 255.0 / samples_count).round() as u8;
    }
    mask
}

fn farthest_corner_radius(cx: f32, cy: f32, rect: Rect) -> f32 {
    [
        (cx - rect.x).hypot(cy - rect.y),
        (rect.right() - cx).hypot(cy - rect.y),
        (cx - rect.x).hypot(rect.bottom() - cy),
        (rect.right() - cx).hypot(rect.bottom() - cy),
    ]
    .into_iter()
    .fold(0.0_f32, f32::max)
}

fn point_in_polygon(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut previous = points[points.len() - 1];
    for &current in points {
        if (current.1 > y) != (previous.1 > y) {
            let crossing =
                previous.0 + (y - previous.1) * (current.0 - previous.0) / (current.1 - previous.1);
            if x < crossing {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

/// A parsed CSS mask image.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum MaskData {
    /// No mask.
    #[default]
    None,
    /// An alpha coverage mask (gradients and basic shapes default to alpha).
    Alpha(CoverageMask),
    /// A luminance coverage mask (`mask-mode: luminance`).
    Luminance(CoverageMask),
    /// An external image URL that the compositor must resolve and rasterize.
    Url(String),
}

/// Parses `mask-image` / `-webkit-mask-image` into [`MaskData`].
///
/// Gradient values are rasterized into a coverage mask immediately (using their alpha
/// channel); `url(...)` values are retained for the compositor.
pub fn parse_mask_image(
    input: &str,
    rect: Rect,
    width: u32,
    height: u32,
    luminance: bool,
) -> MaskData {
    let text = input.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return MaskData::None;
    }
    if let Some(url) = function_arguments(text, "url") {
        return MaskData::Url(url.trim().trim_matches(['\'', '"']).to_string());
    }
    let lower = text.to_ascii_lowercase();
    let mask = if lower.contains("radial-gradient(") {
        gradient_mask(text, rect, width, height, GradientKind::Radial)
    } else if lower.contains("conic-gradient(") {
        gradient_mask(text, rect, width, height, GradientKind::Conic)
    } else if lower.contains("linear-gradient(") {
        // The linear path already has an executable command; for masks we still
        // expose its coverage so the compositor can apply arbitrary mask modes.
        let gradient = crate::commands::parse_linear_gradient(text);
        gradient.map(|(angle, stops)| linear_gradient_mask(&stops, angle, rect, width, height))
    } else {
        None
    };
    match mask {
        Some(coverage) if luminance => MaskData::Luminance(coverage),
        Some(coverage) => MaskData::Alpha(coverage),
        None => MaskData::None,
    }
}

enum GradientKind {
    Radial,
    Conic,
}

fn gradient_mask(
    input: &str,
    rect: Rect,
    width: u32,
    height: u32,
    kind: GradientKind,
) -> Option<CoverageMask> {
    // Locate the gradient function inside the (possibly wrapped) mask value.
    let name = match kind {
        GradientKind::Radial => "radial-gradient",
        GradientKind::Conic => "conic-gradient",
    };
    let lowered = input.to_ascii_lowercase();
    let needle = format!("{name}(");
    let start = lowered.find(needle.as_str())?;
    let arguments = function_arguments(&input[start..], name)?;
    let mock = format!("{name}({arguments})");
    let sample: Box<dyn Fn(f32, f32) -> Color> = match kind {
        GradientKind::Radial => {
            let gradient = RadialGradient::parse(&mock)?;
            Box::new(move |x, y| sample_radial(&gradient, rect, x, y))
        }
        GradientKind::Conic => {
            let gradient = ConicGradient::parse(&mock)?;
            Box::new(move |x, y| sample_conic(&gradient, rect, x, y))
        }
    };
    let mut mask = CoverageMask::new(width, height);
    if rect.is_empty() {
        return Some(mask);
    }
    for (index, value) in mask.samples.iter_mut().enumerate() {
        let mx = index as u32 % width;
        let my = index as u32 / width;
        let x = rect.x + (mx as f32 + 0.5) / width as f32 * rect.width;
        let y = rect.y + (my as f32 + 0.5) / height as f32 * rect.height;
        *value = sample(x, y).a;
    }
    Some(mask)
}

fn linear_gradient_mask(
    stops: &[(f32, Color)],
    angle_deg: f32,
    rect: Rect,
    width: u32,
    height: u32,
) -> CoverageMask {
    let mut mask = CoverageMask::new(width, height);
    if rect.is_empty() || stops.is_empty() {
        return mask;
    }
    let angle = angle_deg.to_radians();
    let direction = (angle.sin(), -angle.cos());
    let stops: Vec<GradientStop> = stops
        .iter()
        .map(|&(position, color)| GradientStop { position, color })
        .collect();
    for (index, value) in mask.samples.iter_mut().enumerate() {
        let mx = index as u32 % width;
        let my = index as u32 / width;
        let x = (mx as f32 + 0.5) / width as f32 - 0.5;
        let y = (my as f32 + 0.5) / height as f32 - 0.5;
        let t = (x * direction.0 + y * direction.1 + 0.5).clamp(0.0, 1.0);
        *value = sample_gradient(&stops, t).a;
    }
    mask
}

// ---------------------------------------------------------------------------
// border-image and outline
// ---------------------------------------------------------------------------

/// CSS `border-image-repeat` keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderImageRepeat {
    /// Stretch each slice (the default).
    #[default]
    Stretch,
    /// Repeat each slice.
    Repeat,
    /// Repeat and round to avoid clipping.
    Round,
    /// Space repeated slices evenly.
    Space,
}

impl BorderImageRepeat {
    fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "stretch" => Some(Self::Stretch),
            "repeat" => Some(Self::Repeat),
            "round" => Some(Self::Round),
            "space" => Some(Self::Space),
            _ => None,
        }
    }
}

/// A parsed CSS `border-image` shorthand.
#[derive(Debug, Clone, PartialEq)]
pub struct BorderImageData {
    /// `url(...)` or a gradient string for the image source.
    pub source: String,
    /// Nine-slice line distances (top, right, bottom, left).
    pub slice: [CssLength; 4],
    /// Border-image widths (top, right, bottom, left).
    pub width: [CssLength; 4],
    /// Outward distances (top, right, bottom, left).
    pub outset: [CssLength; 4],
    /// Horizontal and vertical repeat modes.
    pub repeat: [BorderImageRepeat; 2],
}

impl BorderImageData {
    /// Parses a `border-image` shorthand value.
    ///
    /// Missing sections fall back to their CSS initial values (`0` slices, `auto`
    /// widths, `0` outset, `stretch` repeat).
    pub fn parse(input: &str) -> Option<Self> {
        let text = input.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("none") {
            return None;
        }
        let tokens = split_whitespace_top_level(text);
        if tokens.is_empty() {
            return None;
        }
        let source = tokens[0].to_string();
        // Repeat keywords may appear after the last `/` section without their own
        // slash, so peel them off before splitting the width/outset sections.
        let mut repeat_values = Vec::new();
        let mut structural: Vec<&str> = Vec::new();
        for token in &tokens[1..] {
            if let Some(kind) = BorderImageRepeat::parse(token) {
                repeat_values.push(kind);
            } else {
                structural.push(*token);
            }
        }
        let mut repeat = [BorderImageRepeat::Stretch; 2];
        if let Some(first) = repeat_values.first() {
            repeat = [*first, *repeat_values.get(1).unwrap_or(first)];
        }
        let mut sections: Vec<Vec<&str>> = vec![Vec::new()];
        for token in structural {
            if token == "/" {
                sections.push(Vec::new());
            } else {
                sections
                    .last_mut()
                    .expect("at least one section")
                    .push(token);
            }
        }
        let slice = section_lengths(sections.first(), CssLength::Px(0.0), true)?;
        let width = section_lengths(sections.get(1), CssLength::Percent(1.0), true)?;
        let outset = section_lengths(sections.get(2), CssLength::Px(0.0), true)?;
        Some(Self {
            source,
            slice,
            width,
            outset,
            repeat,
        })
    }
}

fn section_lengths(
    section: Option<&Vec<&str>>,
    fallback: CssLength,
    allow_auto: bool,
) -> Option<[CssLength; 4]> {
    match section {
        None => Some([fallback; 4]),
        Some(values) if values.is_empty() => Some([fallback; 4]),
        Some(values) => {
            let filtered: Vec<&str> = values
                .iter()
                .copied()
                .filter(|value| !(allow_auto && value.eq_ignore_ascii_case("auto")))
                .collect();
            if filtered.is_empty() {
                return Some([fallback; 4]);
            }
            expand_four(&filtered)
        }
    }
}

/// A parsed CSS `outline` plus its `outline-offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineData {
    /// Outline width in CSS pixels.
    pub width: f32,
    /// Outline style.
    pub style: BorderStyle,
    /// Outline colour.
    pub color: Color,
    /// Distance between the border edge and the outline, in CSS pixels.
    pub offset: f32,
}

impl Default for OutlineData {
    fn default() -> Self {
        Self {
            width: 0.0,
            style: BorderStyle::None,
            color: Color::transparent(),
            offset: 0.0,
        }
    }
}

impl OutlineData {
    /// Parses an `outline` shorthand (`<width> <style> <color>` in any order).
    pub fn parse(input: &str) -> Option<Self> {
        let text = input.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("none") {
            return None;
        }
        let mut outline = Self {
            offset: 0.0,
            ..Self::default()
        };
        for token in split_whitespace_top_level(text) {
            if let Some(width) = CssLength::parse(token) {
                outline.width = width.resolve(1.0);
            } else if let Some(style) = parse_border_style(token) {
                outline.style = style;
            } else if let Some(color) = Value::parse(token).as_color() {
                outline.color = color;
            }
        }
        (outline.width > 0.0 && outline.style.is_visible()).then_some(outline)
    }

    /// Applies the `outline-offset` declaration.
    pub fn with_offset(mut self, input: &str) -> Self {
        if let Some(offset) = CssLength::parse(input) {
            self.offset = offset.resolve(1.0);
        }
        self
    }
}

fn parse_border_style(input: &str) -> Option<BorderStyle> {
    match input.to_ascii_lowercase().as_str() {
        "none" => Some(BorderStyle::None),
        "hidden" => Some(BorderStyle::Hidden),
        "solid" => Some(BorderStyle::Solid),
        "dashed" => Some(BorderStyle::Dashed),
        "dotted" => Some(BorderStyle::Dotted),
        "double" => Some(BorderStyle::Double),
        "groove" => Some(BorderStyle::Groove),
        "ridge" => Some(BorderStyle::Ridge),
        "inset" => Some(BorderStyle::Inset),
        "outset" => Some(BorderStyle::Outset),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Extended commands and effect-list construction
// ---------------------------------------------------------------------------

/// A paint operation that needs raster/compositor support beyond the existing
/// [`DisplayCommand`] set.
///
/// The uniform `Draw*` prefix mirrors [`DisplayCommand`] and keeps match arms
/// self-describing at call sites, so the shared-prefix lint is intentionally allowed.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum ExtendedCommand {
    /// Paint children into a temporary buffer and apply an alpha/luminance mask.
    DrawMasked {
        /// Commands producing the content to mask.
        child_commands: Vec<DisplayCommand>,
        /// Mask to apply.
        mask: MaskData,
    },
    /// Clip children with an arbitrary basic shape.
    DrawClipped {
        /// Commands producing the content to clip.
        child_commands: Vec<DisplayCommand>,
        /// Clip shape.
        clip: ClipPath,
        /// Reference box in CSS pixels.
        rect: Rect,
    },
    /// Blend a group with its backdrop and optionally filter that backdrop.
    DrawBlended {
        /// Commands producing the group.
        child_commands: Vec<DisplayCommand>,
        /// Blend mode to apply.
        blend_mode: BlendMode,
        /// Filters applied to the backdrop before blending.
        backdrop: BackdropFilters,
    },
    /// Draw a radial gradient.
    DrawRadialGradient {
        /// Rectangle in CSS pixels.
        rect: Rect,
        /// Gradient definition.
        gradient: RadialGradient,
        /// Corner radii in CSS order (top-left, top-right, bottom-right, bottom-left).
        radius: [f32; 4],
    },
    /// Draw a conic gradient.
    DrawConicGradient {
        /// Rectangle in CSS pixels.
        rect: Rect,
        /// Gradient definition.
        gradient: ConicGradient,
        /// Corner radii in CSS order (top-left, top-right, bottom-right, bottom-left).
        radius: [f32; 4],
    },
    /// Draw an image with an explicit interpolation hint.
    DrawImageWithRendering {
        /// Destination rectangle.
        rect: Rect,
        /// Index into [`crate::display_list::DisplayList::images`].
        image: usize,
        /// Interpolation hint.
        rendering: ImageRendering,
    },
    /// Draw a nine-slice border image.
    DrawBorderImage {
        /// Border box in CSS pixels.
        rect: Rect,
        /// Parsed border-image value.
        border_image: BorderImageData,
    },
    /// Draw an outline outside a box (already executable via
    /// [`crate::borders::outline_command`]).
    DrawOutline {
        /// Border box in CSS pixels.
        rect: Rect,
        /// Outline definition.
        outline: OutlineData,
    },
}

/// Raw declaration strings for effects the style engine does not model yet.
///
/// Populate this per node and pass it to [`build_effects_with`]. Once `browser_style`
/// gains the matching `Property` variants, this struct becomes unnecessary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawEffects {
    /// `mask-image` value.
    pub mask_image: Option<String>,
    /// `-webkit-mask-image` value (used as a fallback for `mask-image`).
    pub webkit_mask_image: Option<String>,
    /// `mask-mode: luminance` flag.
    pub mask_luminance: bool,
    /// `clip-path` value.
    pub clip_path: Option<String>,
    /// `mix-blend-mode` value.
    pub mix_blend_mode: Option<String>,
    /// `backdrop-filter` value.
    pub backdrop_filter: Option<String>,
    /// `border-image` value.
    pub border_image: Option<String>,
    /// `outline` value.
    pub outline: Option<String>,
    /// `outline-offset` value.
    pub outline_offset: Option<String>,
    /// `image-rendering` value.
    pub image_rendering: Option<String>,
}

/// An ordered list of extended commands.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectsList {
    /// Commands in painting order.
    pub commands: Vec<ExtendedCommand>,
}

impl EffectsList {
    /// Creates an empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a command.
    pub fn push(&mut self, command: ExtendedCommand) {
        self.commands.push(command);
    }

    /// Number of commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// True when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Builds extended commands for a layout tree using only computed style.
///
/// Radial/conic `background-image` values are reachable today; all other features
/// need a [`RawEffects`] entry and should use [`build_effects_with`].
pub fn build_effects(layout: &browser_layout::LayoutTree) -> EffectsList {
    build_effects_with(layout, &HashMap::new())
}

/// Builds extended commands for a layout tree, consulting `raw` for declarations that
/// `browser_style` does not model yet.
pub fn build_effects_with(
    layout: &browser_layout::LayoutTree,
    raw: &HashMap<NodeId, RawEffects>,
) -> EffectsList {
    let mut list = EffectsList::new();
    if let Some(root) = layout.root {
        collect_effects(layout, root, 0.0, 0.0, raw, &mut list);
    }
    list
}

fn collect_effects(
    layout: &browser_layout::LayoutTree,
    index: usize,
    dx: f32,
    dy: f32,
    raw: &HashMap<NodeId, RawEffects>,
    list: &mut EffectsList,
) {
    let Some(b) = layout.boxes.get(index) else {
        return;
    };
    let rect = b.rect.translate(dx, dy);
    let radius = corner_radii(b, rect);
    let empty = RawEffects::default();
    let effects = b
        .node
        .as_ref()
        .map_or(&empty, |node| raw.get(node).unwrap_or(&empty));

    if let Some(image) = b.style.background_image() {
        for layer in split_top_level(image) {
            let text = layer.trim();
            if let Some(gradient) = RadialGradient::parse(text) {
                list.push(ExtendedCommand::DrawRadialGradient {
                    rect,
                    gradient,
                    radius,
                });
            } else if let Some(gradient) = ConicGradient::parse(text) {
                list.push(ExtendedCommand::DrawConicGradient {
                    rect,
                    gradient,
                    radius,
                });
            }
        }
    }

    let children = || crate::commands::paint_box_commands(layout, index, dx, dy).commands;
    if let Some(text) = effects.clip_path.as_deref()
        && let Some(clip) = ClipPath::parse(text)
    {
        // Rectangular clips are emitted as data here; the caller may instead lower
        // them to PushClip/PopClip via `clip_path_to_rect` for an executable path.
        list.push(ExtendedCommand::DrawClipped {
            child_commands: children(),
            clip,
            rect,
        });
    }
    let mask_text = effects
        .mask_image
        .as_deref()
        .or(effects.webkit_mask_image.as_deref());
    if let Some(text) = mask_text {
        let mask = parse_mask_image(
            text,
            rect,
            rect.width.max(1.0) as u32,
            rect.height.max(1.0) as u32,
            effects.mask_luminance,
        );
        if mask != MaskData::None {
            list.push(ExtendedCommand::DrawMasked {
                child_commands: children(),
                mask,
            });
        }
    }
    if let Some(mode) = effects.mix_blend_mode.as_deref()
        && let Some(blend_mode) = BlendMode::parse(mode)
    {
        let backdrop = effects
            .backdrop_filter
            .as_deref()
            .map_or_else(BackdropFilters::default, BackdropFilters::parse);
        list.push(ExtendedCommand::DrawBlended {
            child_commands: children(),
            blend_mode,
            backdrop,
        });
    } else if let Some(filter) = effects.backdrop_filter.as_deref() {
        let backdrop = BackdropFilters::parse(filter);
        if !backdrop.is_empty() {
            list.push(ExtendedCommand::DrawBlended {
                child_commands: children(),
                blend_mode: BlendMode::Normal,
                backdrop,
            });
        }
    }
    if let Some(text) = effects.border_image.as_deref()
        && let Some(border_image) = BorderImageData::parse(text)
    {
        list.push(ExtendedCommand::DrawBorderImage { rect, border_image });
    }
    if let Some(text) = effects.outline.as_deref()
        && let Some(mut outline) = OutlineData::parse(text)
    {
        if let Some(offset) = effects.outline_offset.as_deref() {
            outline = outline.with_offset(offset);
        }
        list.push(ExtendedCommand::DrawOutline { rect, outline });
    }
    if let Some(text) = effects.image_rendering.as_deref()
        && let Some(rendering) = ImageRendering::parse(text)
        && b.box_type == browser_layout::BoxType::Image
    {
        // The image index is assigned in `DisplayList`; this records the hint so
        // the compositor can pair it with the corresponding DrawImage command.
        list.push(ExtendedCommand::DrawImageWithRendering {
            rect,
            image: usize::MAX,
            rendering,
        });
    }

    let (scroll_x, scroll_y) = b.scroll_offset;
    for child in &b.children {
        collect_effects(layout, *child, dx - scroll_x, dy - scroll_y, raw, list);
    }
}

fn corner_radii(b: &browser_layout::LayoutBox, rect: Rect) -> [f32; 4] {
    let corners = b.style.border_radius_corners().corners;
    let base = rect.width.min(rect.height);
    let resolve = |dim: browser_style::Dim| match dim {
        browser_style::Dim::Px(value) => value.max(0.0),
        browser_style::Dim::Percent(percent) => (base * percent / 100.0).max(0.0),
        browser_style::Dim::Calc { percent, px } => (base * percent / 100.0 + px).max(0.0),
        browser_style::Dim::Auto => 0.0,
    };
    [
        resolve(corners[0].horizontal),
        resolve(corners[1].horizontal),
        resolve(corners[2].horizontal),
        resolve(corners[3].horizontal),
    ]
}

/// Helper used by tests and compositors to detect a computed-string value.
pub fn computed_str(
    style: &browser_style::ComputedStyle,
    property: browser_style::Property,
) -> Option<&str> {
    match style.get(property) {
        Some(ComputedValue::Str(text))
            if !text.is_empty() && !text.eq_ignore_ascii_case("none") =>
        {
            Some(text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blend_modes_and_blends() {
        assert_eq!(BlendMode::parse("multiply"), Some(BlendMode::Multiply));
        assert_eq!(
            BlendMode::parse("PLUS-LIGHTER"),
            Some(BlendMode::PlusLighter)
        );
        assert_eq!(BlendMode::parse("nope"), None);
        assert_eq!(BlendMode::Multiply.blend(255, 128), 128);
        assert_eq!(BlendMode::Screen.blend(0, 128), 128);
        assert_eq!(BlendMode::PlusLighter.blend(200, 100), 255);
    }

    #[test]
    fn parses_backdrop_filters() {
        let filters = BackdropFilters::parse("blur(4px) brightness(0.5) hue-rotate(90deg)");
        assert_eq!(filters.len(), 3);
        assert_eq!(filters.0[0], DisplayFilter::Blur(4.0));
        assert_eq!(filters.0[1], DisplayFilter::Brightness(0.5));
        assert_eq!(filters.0[2], DisplayFilter::HueRotate(90.0));
        assert!(BackdropFilters::parse("none").is_empty());
    }

    #[test]
    fn parses_image_rendering() {
        assert_eq!(
            ImageRendering::parse("pixelated"),
            Some(ImageRendering::Pixelated)
        );
        assert_eq!(
            ImageRendering::parse("crisp-edges"),
            Some(ImageRendering::CrispEdges)
        );
        assert!(ImageRendering::Pixelated.is_pixelated());
        assert!(!ImageRendering::Auto.is_pixelated());
    }

    #[test]
    fn parses_radial_gradient() {
        let gradient = RadialGradient::parse(
            "radial-gradient(circle at 25% 50%, red 0%, rgb(0, 0, 255) 100%)",
        )
        .expect("valid radial gradient");
        assert_eq!(gradient.shape, RadialShape::Circle);
        assert_eq!(
            gradient.center,
            GradientCenter {
                x: CssLength::Percent(0.25),
                y: CssLength::Percent(0.5),
            }
        );
        assert_eq!(gradient.stops.len(), 2);
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            sample_radial(&gradient, rect, 25.0, 50.0),
            Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn parses_conic_gradient() {
        let gradient =
            ConicGradient::parse("conic-gradient(from 90deg at center, red 0%, blue 100%)")
                .expect("valid conic gradient");
        assert_eq!(gradient.from_deg, 90.0);
        assert_eq!(gradient.stops.len(), 2);
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        // At the right edge the unrotated angle is 90deg; with a 90deg start it is t=0.
        assert_eq!(
            sample_conic(&gradient, rect, 100.0, 50.0),
            Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn parses_clip_paths_and_lowers_rectangles() {
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        let inset = ClipPath::parse("inset(10px 20px)").expect("inset clip");
        assert_eq!(
            clip_path_to_rect(&inset, rect),
            Some(Rect::new(20.0, 10.0, 60.0, 80.0))
        );
        assert!(ClipPath::parse("none").is_none());
        let circle = ClipPath::parse("circle(20px at 50% 50%)").expect("circle clip");
        assert!(circle.clip_path_to_rect(rect).is_none());
    }

    #[test]
    fn rasterizes_circle_coverage_mask() {
        let rect = Rect::new(0.0, 0.0, 20.0, 20.0);
        let clip = ClipPath::parse("circle(50% at 50% 50%)").expect("circle clip");
        let mask = rasterize_clip_path(&clip, rect, 20, 20);
        assert_eq!((mask.width, mask.height), (20, 20));
        assert!(mask.coverage_at(10, 10) > 0.95, "centre should be covered");
        assert!(mask.coverage_at(0, 0) < 0.05, "corner should be clipped");
    }

    #[test]
    fn rasterizes_polygon_coverage_mask() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        let clip = ClipPath::parse("polygon(0% 0%, 100% 0%, 50% 100%)").expect("triangle");
        let mask = rasterize_clip_path(&clip, rect, 10, 10);
        assert!(mask.coverage_at(5, 2) > 0.8, "inside the triangle");
        assert!(mask.coverage_at(0, 9) < 0.2, "outside the triangle");
    }

    #[test]
    fn parses_mask_image_url_and_gradient() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(
            parse_mask_image("url(mask.png)", rect, 10, 10, false),
            MaskData::Url("mask.png".to_string())
        );
        let mask = parse_mask_image(
            "linear-gradient(to right, white, black)",
            rect,
            10,
            10,
            false,
        );
        assert!(matches!(mask, MaskData::Alpha(_)));
        assert!(matches!(
            parse_mask_image("none", rect, 10, 10, false),
            MaskData::None
        ));
    }

    #[test]
    fn parses_border_image_and_outline() {
        let border = BorderImageData::parse("url(border.png) 30 / 10px / 5px round")
            .expect("valid border image");
        assert_eq!(border.slice, [CssLength::Px(30.0); 4]);
        assert_eq!(border.width, [CssLength::Px(10.0); 4]);
        assert_eq!(border.outset, [CssLength::Px(5.0); 4]);
        assert_eq!(border.repeat, [BorderImageRepeat::Round; 2]);

        let outline = OutlineData::parse("2px solid red")
            .expect("valid outline")
            .with_offset("3px");
        assert_eq!(outline.width, 2.0);
        assert_eq!(outline.style, BorderStyle::Solid);
        assert_eq!(outline.color, Color::rgb(255, 0, 0));
        assert_eq!(outline.offset, 3.0);
    }

    #[test]
    fn effects_list_records_commands() {
        let mut list = EffectsList::new();
        assert!(list.is_empty());
        list.push(ExtendedCommand::DrawOutline {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            outline: OutlineData::default(),
        });
        assert_eq!(list.len(), 1);
    }
}
