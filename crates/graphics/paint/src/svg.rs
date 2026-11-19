//! Minimal, dependency-free SVG rasterizer.
//!
//! This module turns a small subset of SVG 1.1 into an [`ImageBuffer`] using
//! only the standard library. It is deliberately self-contained: no XML,
//! path or colour library is used, and every failure is reported as a
//! [`DecodeError`] rather than a panic.
//!
//! # Supported subset
//!
//! * Root `<svg>` with `width`, `height`, `viewBox` and `preserveAspectRatio`.
//! * Shape elements `<rect>` (with `rx`/`ry`), `<circle>`, `<ellipse>`,
//!   `<line>`, `<polyline>`, `<polygon>` and `<path>`.
//! * Path commands `M`, `L`, `H`, `V`, `C`, `Q` and `Z` (both cases). Cubic and
//!   quadratic Bézier segments are flattened into line segments. Other
//!   commands (including arcs and shorthand curves) are rejected.
//! * Presentation attributes `fill`, `stroke`, `stroke-width`, `opacity`,
//!   `fill-opacity`, `stroke-opacity` and `fill-rule`, either as attributes or
//!   inside a `style="..."` property list. Group attributes are inherited.
//! * `<g>` containers with `transform="translate|scale|rotate|matrix(...)"`,
//!   including nested groups and rotation about an arbitrary centre.
//!
//! Colours may be `none`, `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`,
//! `rgba()` or a small table of common CSS named colours. Everything else is
//! reported as unsupported.
//!
//! Edges are anti-aliased with 4x4 superscript coverage: each pixel takes
//! sixteen evenly spaced samples, and the covered fraction becomes the source
//! alpha of a source-over composite.

use std::collections::HashMap;

use crate::decode::DecodeError;
use crate::images::{MAX_IMAGE_DIMENSION, check_dimensions};
use crate::raster::ImageBuffer;

/// Sub-samples per axis for anti-aliasing (4x4 = 16 taps).
const AA: usize = 4;
const AA_SAMPLES: f32 = (AA * AA) as f32;
/// Number of line segments used to flatten one Bézier curve.
const CURVE_SEGMENTS: usize = 16;
/// Number of segments used for one elliptical arc of a rounded rectangle.
const ARC_SEGMENTS: usize = 8;
/// Number of segments used for a full circle or ellipse.
const ELLIPSE_SEGMENTS: usize = 72;

fn err(message: impl Into<String>) -> DecodeError {
    DecodeError::new(message)
}

// ---------------------------------------------------------------------------
// Affine transforms
// ---------------------------------------------------------------------------

/// A 2D point in user/screen space.
type Point = (f32, f32);

/// Affine transform stored as `[a c e; b d f; 0 0 1]`.
#[derive(Clone, Copy, Debug)]
struct Matrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Matrix {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// `self * other`: `other` is applied to a point first, then `self`.
    fn mul(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    fn apply(self, p: Point) -> Point {
        (
            self.a * p.0 + self.c * p.1 + self.e,
            self.b * p.0 + self.d * p.1 + self.f,
        )
    }

    /// Inverts the affine transform, when it is non-singular.
    fn inverse(self) -> Option<Self> {
        let determinant = self.a * self.d - self.b * self.c;
        if !determinant.is_finite() || determinant.abs() < 1e-6 {
            return None;
        }
        let inv = 1.0 / determinant;
        Some(Self {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }

    fn translation(x: f32, y: f32) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }
}

// ---------------------------------------------------------------------------
// A tiny XML reader
// ---------------------------------------------------------------------------

/// One parsed element. Text nodes are discarded.
#[derive(Debug, Clone)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

impl Node {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

struct Xml<'a> {
    bytes: &'a [u8],
    pos: usize,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Removes an XML namespace prefix (`svg:rect` becomes `rect`).
fn local_name(name: &str) -> String {
    name.split_once(':')
        .map_or(name, |(_, local)| local)
        .to_owned()
}

fn decode_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        match entity {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

impl<'a> Xml<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }

    fn starts(&self, marker: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(marker)
    }

    fn skip_until(&mut self, marker: &[u8]) -> Result<(), DecodeError> {
        let Some(offset) = find_subslice(&self.bytes[self.pos..], marker) else {
            return Err(err("unterminated XML comment or declaration"));
        };
        self.pos += offset + marker.len();
        Ok(())
    }

    /// Skips whitespace, comments, processing instructions and declarations.
    fn skip_misc(&mut self) -> Result<(), DecodeError> {
        loop {
            self.skip_ws();
            if self.starts(b"<!--") {
                self.skip_until(b"-->")?;
            } else if self.starts(b"<?") {
                self.skip_until(b"?>")?;
            } else if self.starts(b"<!") {
                self.skip_until(b">")?;
            } else {
                return Ok(());
            }
        }
    }

    fn read_name(&mut self) -> Result<String, DecodeError> {
        let start = self.pos;
        while self.bytes.get(self.pos).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-' | b'.')
        }) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(err("expected a name in SVG markup"));
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| err("invalid UTF-8 in SVG name"))?;
        Ok(raw.to_owned())
    }

    fn parse_document(mut self) -> Result<Node, DecodeError> {
        self.skip_misc()?;
        if self.pos >= self.bytes.len() {
            return Err(err("empty SVG document"));
        }
        let root = self.parse_element()?;
        self.skip_misc()?;
        if self.pos < self.bytes.len() {
            return Err(err("unexpected content after the root SVG element"));
        }
        Ok(root)
    }

    fn parse_element(&mut self) -> Result<Node, DecodeError> {
        if self.bytes.get(self.pos) != Some(&b'<') {
            return Err(err("expected '<' at the start of an SVG element"));
        }
        self.pos += 1;
        self.skip_ws();
        let name = local_name(&self.read_name()?);
        let mut attrs = Vec::new();
        let mut self_closing = false;

        loop {
            self.skip_ws();
            match self.bytes.get(self.pos).copied() {
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                Some(b'/') if self.bytes.get(self.pos + 1) == Some(&b'>') => {
                    self.pos += 2;
                    self_closing = true;
                    break;
                }
                Some(_) => {
                    let key = local_name(&self.read_name()?);
                    self.skip_ws();
                    if self.bytes.get(self.pos) != Some(&b'=') {
                        return Err(err(format!("missing '=' for attribute '{key}'")));
                    }
                    self.pos += 1;
                    self.skip_ws();
                    let quote = self
                        .bytes
                        .get(self.pos)
                        .copied()
                        .ok_or_else(|| err("missing attribute value"))?;
                    if quote != b'"' && quote != b'\'' {
                        return Err(err("attribute values must be quoted"));
                    }
                    self.pos += 1;
                    let start = self.pos;
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != quote {
                        self.pos += 1;
                    }
                    if self.pos >= self.bytes.len() {
                        return Err(err("unterminated attribute value"));
                    }
                    let raw = std::str::from_utf8(&self.bytes[start..self.pos])
                        .map_err(|_| err("invalid UTF-8 in attribute value"))?;
                    self.pos += 1;
                    attrs.push((key, decode_entities(raw)));
                }
                None => return Err(err("unexpected end of SVG markup")),
            }
        }

        let mut children = Vec::new();
        if !self_closing {
            loop {
                self.skip_misc()?;
                if self.pos >= self.bytes.len() {
                    return Err(err(format!("unclosed <{name}> element")));
                }
                if self.bytes[self.pos] == b'<' {
                    if self.bytes.get(self.pos + 1) == Some(&b'/') {
                        self.pos += 2;
                        let close = local_name(&self.read_name()?);
                        self.skip_ws();
                        if self.bytes.get(self.pos) != Some(&b'>') {
                            return Err(err("malformed closing tag"));
                        }
                        self.pos += 1;
                        if close != name {
                            return Err(err(format!(
                                "closing tag </{close}> does not match <{name}>"
                            )));
                        }
                        break;
                    }
                    children.push(self.parse_element()?);
                } else {
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'<' {
                        self.pos += 1;
                    }
                }
            }
        }

        Ok(Node {
            name,
            attrs,
            children,
        })
    }
}

// ---------------------------------------------------------------------------
// Numbers, lengths and colours
// ---------------------------------------------------------------------------

/// Scans one SVG number (sign, digits, one dot and an optional exponent).
fn scan_number(bytes: &[u8], pos: &mut usize) -> Result<f32, DecodeError> {
    let start = *pos;
    let first = *bytes
        .get(*pos)
        .ok_or_else(|| err("expected a number in SVG data"))?;
    if !(first.is_ascii_digit() || matches!(first, b'.' | b'-' | b'+')) {
        return Err(err(format!("expected a number, found '{}'", first as char)));
    }
    if matches!(first, b'-' | b'+') {
        *pos += 1;
    }
    let mut dots = 0;
    while let Some(&byte) = bytes.get(*pos) {
        if byte.is_ascii_digit() {
            *pos += 1;
        } else if byte == b'.' {
            dots += 1;
            if dots > 1 {
                break;
            }
            *pos += 1;
        } else {
            break;
        }
    }
    if matches!(bytes.get(*pos), Some(b'e' | b'E')) {
        let mut end = *pos + 1;
        if matches!(bytes.get(end), Some(b'-' | b'+')) {
            end += 1;
        }
        let digits = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end > digits {
            *pos = end;
        }
    }
    let text =
        std::str::from_utf8(&bytes[start..*pos]).map_err(|_| err("invalid UTF-8 in number"))?;
    text.parse::<f32>()
        .map_err(|_| err(format!("invalid number '{text}'")))
}

/// Parses a whitespace/comma separated list of numbers.
fn parse_number_list(text: &str) -> Result<Vec<f32>, DecodeError> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < bytes.len() {
        if bytes[pos].is_ascii_whitespace() || bytes[pos] == b',' {
            pos += 1;
            continue;
        }
        out.push(scan_number(bytes, &mut pos)?);
    }
    Ok(out)
}

/// Parses a CSS length, accepting plain numbers and `px` only.
fn parse_length(text: &str) -> Result<f32, DecodeError> {
    let trimmed = text.trim();
    let number = trimmed.strip_suffix("px").unwrap_or(trimmed);
    let value: f32 = number
        .parse()
        .map_err(|_| err(format!("unsupported SVG length '{text}'")))?;
    if !value.is_finite() {
        return Err(err(format!("invalid SVG length '{text}'")));
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug)]
struct Color {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Color {
    const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        }
    }

    const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
}

// ---------------------------------------------------------------------------
// Paint servers: gradients and patterns
// ---------------------------------------------------------------------------

/// How the coordinates of a gradient or pattern are interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Units {
    /// Fractions of the shape's bounding box (the SVG default).
    ObjectBoundingBox,
    /// Absolute user-space coordinates.
    UserSpaceOnUse,
}

/// A resolved paint: a flat colour or a reference to a definition in `<defs>`.
#[derive(Clone, Debug)]
enum Paint {
    Solid(Color),
    Gradient(Gradient),
    Pattern(Pattern),
}

#[derive(Clone, Copy, Debug)]
struct Stop {
    offset: f32,
    color: Color,
}

#[derive(Clone, Copy, Debug)]
enum GradientKind {
    Linear { x1: f32, y1: f32, x2: f32, y2: f32 },
    Radial { cx: f32, cy: f32, r: f32 },
}

#[derive(Clone, Debug)]
struct Gradient {
    kind: GradientKind,
    stops: Vec<Stop>,
    units: Units,
    /// `gradientTransform` declared on the gradient.
    transform: Matrix,
    /// Transform of the element that references the gradient.
    matrix: Matrix,
}

#[derive(Clone, Debug)]
struct Pattern {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    units: Units,
    /// `patternTransform` declared on the pattern.
    transform: Matrix,
    /// Transform of the element that references the pattern.
    matrix: Matrix,
    node: Node,
}

/// Paint-server definitions (`<linearGradient>`, `<radialGradient>` and
/// `<pattern>`) collected from the whole document and keyed by `id`.
#[derive(Default)]
struct Resources {
    defs: HashMap<String, Node>,
}

/// Guards against a pattern that (directly or indirectly) references itself.
const MAX_PATTERN_DEPTH: usize = 4;

fn split_args(text: &str) -> Vec<&str> {
    text.split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .collect()
}

fn parse_channel(text: &str) -> Result<f32, DecodeError> {
    if let Some(percent) = text.strip_suffix('%') {
        let value: f32 = percent
            .parse()
            .map_err(|_| err(format!("invalid colour component '{text}'")))?;
        return Ok((value / 100.0).clamp(0.0, 1.0));
    }
    let value: f32 = text
        .parse()
        .map_err(|_| err(format!("invalid colour component '{text}'")))?;
    Ok((value / 255.0).clamp(0.0, 1.0))
}

fn parse_alpha(text: &str) -> Result<f32, DecodeError> {
    if let Some(percent) = text.strip_suffix('%') {
        let value: f32 = percent
            .parse()
            .map_err(|_| err(format!("invalid alpha component '{text}'")))?;
        return Ok((value / 100.0).clamp(0.0, 1.0));
    }
    let value: f32 = text
        .parse()
        .map_err(|_| err(format!("invalid alpha component '{text}'")))?;
    Ok(value.clamp(0.0, 1.0))
}

/// Parses a coordinate that may be a plain number or a percentage. Percentages
/// are fractions of the relevant space (bounding box or viewport).
fn parse_fraction(text: &str) -> Result<f32, DecodeError> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        let value: f32 = percent
            .parse()
            .map_err(|_| err(format!("invalid SVG coordinate '{text}'")))?;
        return Ok((value / 100.0).clamp(0.0, 1.0));
    }
    let number = text.strip_suffix("px").unwrap_or(text);
    let value: f32 = number
        .parse()
        .map_err(|_| err(format!("invalid SVG coordinate '{text}'")))?;
    if !value.is_finite() {
        return Err(err(format!("invalid SVG coordinate '{text}'")));
    }
    Ok(value)
}

fn fraction_attr(node: &Node, name: &str, default: f32) -> Result<f32, DecodeError> {
    match node.attr(name) {
        Some(value) => parse_fraction(value),
        None => Ok(default),
    }
}

fn parse_units(value: Option<&str>) -> Units {
    match value.map(str::trim) {
        Some("userSpaceOnUse") => Units::UserSpaceOnUse,
        _ => Units::ObjectBoundingBox,
    }
}

fn parse_gradient_transform(node: &Node, attribute: &str) -> Result<Matrix, DecodeError> {
    match node.attr(attribute) {
        Some(value) => parse_transform(value),
        None => Ok(Matrix::IDENTITY),
    }
}

fn parse_stops(node: &Node) -> Result<Vec<Stop>, DecodeError> {
    let mut stops = Vec::new();
    for child in &node.children {
        if child.name != "stop" {
            continue;
        }
        let offset = match child.attr("offset") {
            Some(value) => parse_fraction(value)?.clamp(0.0, 1.0),
            None => 0.0,
        };
        let mut color = Color::BLACK; // SVG's default stop colour.
        let mut opacity = 1.0;
        for (key, value) in &child.attrs {
            if key == "style" {
                for declaration in value.split(';') {
                    if let Some((property, value)) = declaration.split_once(':') {
                        match property.trim() {
                            "stop-color" => color = parse_color(value)?.unwrap_or(Color::BLACK),
                            "stop-opacity" => opacity = parse_alpha(value.trim())?,
                            _ => {}
                        }
                    }
                }
            } else {
                match key.as_str() {
                    "stop-color" => color = parse_color(value)?.unwrap_or(Color::BLACK),
                    "stop-opacity" => opacity = parse_alpha(value.trim())?,
                    _ => {}
                }
            }
        }
        color.a *= opacity;
        stops.push(Stop { offset, color });
    }
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if stops.is_empty() {
        return Err(err("gradient requires at least one <stop>"));
    }
    Ok(stops)
}

fn parse_gradient(node: &Node, matrix: Matrix) -> Result<Gradient, DecodeError> {
    let stops = parse_stops(node)?;
    let units = parse_units(node.attr("gradientUnits"));
    let transform = parse_gradient_transform(node, "gradientTransform")?;
    let kind = match node.name.as_str() {
        "linearGradient" => GradientKind::Linear {
            x1: fraction_attr(node, "x1", 0.0)?,
            y1: fraction_attr(node, "y1", 0.0)?,
            x2: fraction_attr(node, "x2", 1.0)?,
            y2: fraction_attr(node, "y2", 0.0)?,
        },
        "radialGradient" => {
            let cx = fraction_attr(node, "cx", 0.5)?;
            let cy = fraction_attr(node, "cy", 0.5)?;
            let r = fraction_attr(node, "r", 0.5)?;
            GradientKind::Radial { cx, cy, r }
        }
        _ => return Err(err(format!("not a gradient: <{}>", node.name))),
    };
    Ok(Gradient {
        kind,
        stops,
        units,
        transform,
        matrix,
    })
}

fn parse_pattern(node: &Node, matrix: Matrix) -> Result<Pattern, DecodeError> {
    let width = match node.attr("width") {
        Some(value) => parse_length(value)?,
        None => return Err(err("pattern requires a width")),
    };
    let height = match node.attr("height") {
        Some(value) => parse_length(value)?,
        None => return Err(err("pattern requires a height")),
    };
    if width <= 0.0 || height <= 0.0 {
        return Err(err("pattern requires a positive width and height"));
    }
    Ok(Pattern {
        x: fraction_attr(node, "x", 0.0)?,
        y: fraction_attr(node, "y", 0.0)?,
        width,
        height,
        units: parse_units(node.attr("patternUnits")),
        transform: parse_gradient_transform(node, "patternTransform")?,
        matrix,
        node: node.clone(),
    })
}

impl Resources {
    fn collect(node: &Node, defs: &mut HashMap<String, Node>) {
        if matches!(
            node.name.as_str(),
            "linearGradient" | "radialGradient" | "pattern"
        ) && let Some(id) = node.attr("id")
        {
            defs.entry(id.to_string()).or_insert_with(|| node.clone());
        }
        for child in &node.children {
            Self::collect(child, defs);
        }
    }

    /// Resolves `url(#id)` to a paint. Missing or malformed definitions are
    /// transparent so they can never turn into a black block.
    fn resolve(&self, id: &str, matrix: Matrix) -> Option<Paint> {
        let node = self.defs.get(id)?;
        match node.name.as_str() {
            "linearGradient" | "radialGradient" => {
                parse_gradient(node, matrix).ok().map(Paint::Gradient)
            }
            "pattern" => parse_pattern(node, matrix).ok().map(Paint::Pattern),
            _ => None,
        }
    }
}

/// Parses a paint value. `url(#id)` references a paint server; unresolved
/// references are treated as `none` (transparent) rather than as black.
fn parse_paint(
    raw: &str,
    matrix: Matrix,
    resources: &Resources,
) -> Result<Option<Paint>, DecodeError> {
    let text = raw.trim();
    if let Some(inner) = text
        .strip_prefix("url(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let reference = inner.trim().trim_matches(|c| c == '\'' || c == '"');
        if let Some(id) = reference.strip_prefix('#') {
            return Ok(resources.resolve(id.trim(), matrix));
        }
        return Ok(None);
    }
    Ok(parse_color(text)?.map(Paint::Solid))
}

/// Interpolates the gradient stops at normalised position `t`.
fn sample_stops(stops: &[Stop], t: f32) -> Color {
    if t <= stops[0].offset {
        return stops[0].color;
    }
    for pair in stops.windows(2) {
        if t <= pair[1].offset {
            let span = pair[1].offset - pair[0].offset;
            let mix = if span <= 0.0 {
                1.0
            } else {
                (t - pair[0].offset) / span
            };
            let a = pair[0].color;
            let b = pair[1].color;
            return Color {
                r: a.r + (b.r - a.r) * mix,
                g: a.g + (b.g - a.g) * mix,
                b: a.b + (b.b - a.b) * mix,
                a: a.a + (b.a - a.a) * mix,
            };
        }
    }
    stops[stops.len() - 1].color
}

fn sample_gradient(gradient: &Gradient, bbox: (f32, f32, f32, f32), sample: Point) -> Color {
    let point = match gradient.units {
        Units::ObjectBoundingBox => {
            let width = bbox.2 - bbox.0;
            let height = bbox.3 - bbox.1;
            if width <= 0.0 || height <= 0.0 {
                return Color::TRANSPARENT;
            }
            ((sample.0 - bbox.0) / width, (sample.1 - bbox.1) / height)
        }
        Units::UserSpaceOnUse => sample,
    };
    let inverse = gradient
        .matrix
        .mul(gradient.transform)
        .inverse()
        .unwrap_or(Matrix::IDENTITY);
    let point = inverse.apply(point);
    let t = match gradient.kind {
        GradientKind::Linear { x1, y1, x2, y2 } => {
            let dx = x2 - x1;
            let dy = y2 - y1;
            let length_squared = dx * dx + dy * dy;
            if length_squared <= 0.0 {
                0.0
            } else {
                ((point.0 - x1) * dx + (point.1 - y1) * dy) / length_squared
            }
        }
        GradientKind::Radial { cx, cy, r } => {
            if r <= 0.0 {
                0.0
            } else {
                let dx = point.0 - cx;
                let dy = point.1 - cy;
                dx.hypot(dy) / r
            }
        }
    };
    sample_stops(&gradient.stops, t)
}

fn named_color(name: &str) -> Option<Color> {
    let (r, g, b) = match name {
        "black" | "currentcolor" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "green" => (0, 128, 0),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "navy" => (0, 0, 128),
        "purple" => (128, 0, 128),
        "teal" => (0, 128, 128),
        "orange" => (255, 165, 0),
        "pink" => (255, 192, 203),
        "brown" => (165, 42, 42),
        _ => return None,
    };
    Some(Color::from_rgb(r, g, b))
}

/// `Ok(None)` means "none"/"transparent" (no paint).
fn parse_color(raw: &str) -> Result<Option<Color>, DecodeError> {
    let text = raw.trim();
    let lower = text.to_ascii_lowercase();
    if matches!(lower.as_str(), "" | "none" | "transparent") {
        return Ok(None);
    }

    if let Some(hex) = lower.strip_prefix('#') {
        if !hex.is_ascii() {
            return Err(err(format!("unsupported colour '{raw}'")));
        }
        let nibble = |c: char| -> Result<u8, DecodeError> {
            c.to_digit(16)
                .map(|v| v as u8)
                .ok_or_else(|| err(format!("invalid hex colour '{raw}'")))
        };
        let color = match hex.len() {
            3 => {
                let r = nibble(hex.chars().next().unwrap())?;
                let g = nibble(hex.chars().nth(1).unwrap())?;
                let b = nibble(hex.chars().nth(2).unwrap())?;
                Color::from_rgb(r * 17, g * 17, b * 17)
            }
            4 => {
                let mut it = hex.chars();
                let r = nibble(it.next().unwrap())?;
                let g = nibble(it.next().unwrap())?;
                let b = nibble(it.next().unwrap())?;
                let a = nibble(it.next().unwrap())?;
                let mut c = Color::from_rgb(r * 17, g * 17, b * 17);
                c.a = f32::from(a * 17) / 255.0;
                c
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16)
                    .map_err(|_| err(format!("invalid hex colour '{raw}'")))?;
                let g = u8::from_str_radix(&hex[2..4], 16)
                    .map_err(|_| err(format!("invalid hex colour '{raw}'")))?;
                let b = u8::from_str_radix(&hex[4..6], 16)
                    .map_err(|_| err(format!("invalid hex colour '{raw}'")))?;
                Color::from_rgb(r, g, b)
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16)
                    .map_err(|_| err(format!("invalid hex colour '{raw}'")))?;
                let g = u8::from_str_radix(&hex[2..4], 16)
                    .map_err(|_| err(format!("invalid hex colour '{raw}'")))?;
                let b = u8::from_str_radix(&hex[4..6], 16)
                    .map_err(|_| err(format!("invalid hex colour '{raw}'")))?;
                let a = u8::from_str_radix(&hex[6..8], 16)
                    .map_err(|_| err(format!("invalid hex colour '{raw}'")))?;
                let mut c = Color::from_rgb(r, g, b);
                c.a = f32::from(a) / 255.0;
                c
            }
            _ => return Err(err(format!("unsupported colour '{raw}'"))),
        };
        return Ok(Some(color));
    }

    if let Some(inner) = lower
        .strip_prefix("rgb(")
        .or_else(|| lower.strip_prefix("rgba("))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let args = split_args(inner);
        let (expected, with_alpha) = if lower.starts_with("rgba(") {
            (4, true)
        } else {
            (3, false)
        };
        if args.len() != expected {
            return Err(err(format!("unsupported colour '{raw}'")));
        }
        let mut color = Color {
            r: parse_channel(args[0])?,
            g: parse_channel(args[1])?,
            b: parse_channel(args[2])?,
            a: 1.0,
        };
        if with_alpha {
            color.a = parse_alpha(args[3])?;
        }
        return Ok(Some(color));
    }

    named_color(&lower)
        .map(Some)
        .ok_or_else(|| err(format!("unsupported colour '{raw}'")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillRule {
    Nonzero,
    EvenOdd,
}

#[derive(Clone, Debug)]
struct Style {
    fill: Option<Paint>,
    stroke: Option<Paint>,
    stroke_width: f32,
    opacity: f32,
    fill_opacity: f32,
    stroke_opacity: f32,
    fill_rule: FillRule,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            fill: Some(Paint::Solid(Color::BLACK)),
            stroke: None,
            stroke_width: 1.0,
            opacity: 1.0,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            fill_rule: FillRule::Nonzero,
        }
    }
}

impl Style {
    fn apply_property(
        &mut self,
        key: &str,
        value: &str,
        matrix: Matrix,
        resources: &Resources,
    ) -> Result<(), DecodeError> {
        match key.trim() {
            "fill" => self.fill = parse_paint(value, matrix, resources)?,
            "stroke" => self.stroke = parse_paint(value, matrix, resources)?,
            "stroke-width" => {
                let width = parse_length(value)?;
                if width < 0.0 {
                    return Err(err("stroke-width cannot be negative"));
                }
                self.stroke_width = width;
            }
            "opacity" => self.opacity = parse_alpha(value.trim())?,
            "fill-opacity" => self.fill_opacity = parse_alpha(value.trim())?,
            "stroke-opacity" => self.stroke_opacity = parse_alpha(value.trim())?,
            "fill-rule" => {
                self.fill_rule = match value.trim() {
                    "evenodd" => FillRule::EvenOdd,
                    "nonzero" => FillRule::Nonzero,
                    other => return Err(err(format!("unsupported fill-rule '{other}'"))),
                };
            }
            // Unknown presentation properties are ignored.
            _ => {}
        }
        Ok(())
    }

    fn apply(
        &mut self,
        node: &Node,
        matrix: Matrix,
        resources: &Resources,
    ) -> Result<(), DecodeError> {
        for (key, value) in &node.attrs {
            if key == "style" {
                for declaration in value.split(';') {
                    if let Some((property, value)) = declaration.split_once(':') {
                        self.apply_property(property, value, matrix, resources)?;
                    }
                }
            } else {
                self.apply_property(key, value, matrix, resources)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Paths and shapes
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SubPath {
    pts: Vec<Point>,
    closed: bool,
}

#[derive(Debug)]
struct Path {
    subs: Vec<SubPath>,
}

#[derive(Clone, Copy, Debug)]
enum Token {
    Command(u8),
    Number(f32),
}

fn tokenize_path(data: &str) -> Result<Vec<Token>, DecodeError> {
    let bytes = data.as_bytes();
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < bytes.len() {
        let byte = bytes[pos];
        if byte.is_ascii_whitespace() || byte == b',' {
            pos += 1;
            continue;
        }
        if byte.is_ascii_alphabetic() {
            out.push(Token::Command(byte));
            pos += 1;
            continue;
        }
        out.push(Token::Number(scan_number(bytes, &mut pos)?));
    }
    Ok(out)
}

fn take_number(tokens: &[Token], pos: &mut usize) -> Result<f32, DecodeError> {
    match tokens.get(*pos) {
        Some(Token::Number(value)) => {
            *pos += 1;
            Ok(*value)
        }
        _ => Err(err("path command is missing a number")),
    }
}

fn flush_open(subs: &mut Vec<SubPath>, current: &mut Vec<Point>) {
    if current.len() >= 2 {
        subs.push(SubPath {
            pts: std::mem::take(current),
            closed: false,
        });
    } else {
        current.clear();
    }
}

fn add(base: Point, delta: Point) -> Point {
    (base.0 + delta.0, base.1 + delta.1)
}

fn flatten_cubic(p0: Point, p1: Point, p2: Point, p3: Point, out: &mut Vec<Point>) {
    if out.is_empty() {
        out.push(p0);
    }
    for step in 1..=CURVE_SEGMENTS {
        let t = step as f32 / CURVE_SEGMENTS as f32;
        let u = 1.0 - t;
        let w0 = u * u * u;
        let w1 = 3.0 * u * u * t;
        let w2 = 3.0 * u * t * t;
        let w3 = t * t * t;
        out.push((
            w0 * p0.0 + w1 * p1.0 + w2 * p2.0 + w3 * p3.0,
            w0 * p0.1 + w1 * p1.1 + w2 * p2.1 + w3 * p3.1,
        ));
    }
}

fn flatten_quadratic(p0: Point, p1: Point, p2: Point, out: &mut Vec<Point>) {
    if out.is_empty() {
        out.push(p0);
    }
    for step in 1..=CURVE_SEGMENTS {
        let t = step as f32 / CURVE_SEGMENTS as f32;
        let u = 1.0 - t;
        let w0 = u * u;
        let w1 = 2.0 * u * t;
        let w2 = t * t;
        out.push((
            w0 * p0.0 + w1 * p1.0 + w2 * p2.0,
            w0 * p0.1 + w1 * p1.1 + w2 * p2.1,
        ));
    }
}

fn parse_path_data(data: &str) -> Result<Path, DecodeError> {
    let tokens = tokenize_path(data)?;
    let mut subs = Vec::new();
    let mut current: Vec<Point> = Vec::new();
    let mut cur: Point = (0.0, 0.0);
    let mut start: Point = (0.0, 0.0);
    let mut command: Option<u8> = None;
    let mut pos = 0;

    while pos < tokens.len() {
        if let Token::Command(c) = tokens[pos] {
            pos += 1;
            if c.eq_ignore_ascii_case(&b'z') {
                flush_closed(&mut subs, &mut current);
                cur = start;
                command = None;
                continue;
            }
            command = Some(c);
        }
        let cmd = command.ok_or_else(|| err("path data starts with coordinates"))?;
        let relative = cmd.is_ascii_lowercase();
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let x = take_number(&tokens, &mut pos)?;
                let y = take_number(&tokens, &mut pos)?;
                flush_open(&mut subs, &mut current);
                cur = if relative { add(cur, (x, y)) } else { (x, y) };
                start = cur;
                current.push(cur);
                // Further coordinate pairs after M are implicit line-tos.
                command = Some(if relative { b'l' } else { b'L' });
            }
            b'L' => {
                let x = take_number(&tokens, &mut pos)?;
                let y = take_number(&tokens, &mut pos)?;
                cur = if relative { add(cur, (x, y)) } else { (x, y) };
                current.push(cur);
            }
            b'H' => {
                let x = take_number(&tokens, &mut pos)?;
                cur = (if relative { cur.0 + x } else { x }, cur.1);
                current.push(cur);
            }
            b'V' => {
                let y = take_number(&tokens, &mut pos)?;
                cur = (cur.0, if relative { cur.1 + y } else { y });
                current.push(cur);
            }
            b'C' => {
                let x1 = take_number(&tokens, &mut pos)?;
                let y1 = take_number(&tokens, &mut pos)?;
                let x2 = take_number(&tokens, &mut pos)?;
                let y2 = take_number(&tokens, &mut pos)?;
                let x = take_number(&tokens, &mut pos)?;
                let y = take_number(&tokens, &mut pos)?;
                let (p1, p2, p3) = if relative {
                    (add(cur, (x1, y1)), add(cur, (x2, y2)), add(cur, (x, y)))
                } else {
                    ((x1, y1), (x2, y2), (x, y))
                };
                flatten_cubic(cur, p1, p2, p3, &mut current);
                cur = p3;
            }
            b'Q' => {
                let x1 = take_number(&tokens, &mut pos)?;
                let y1 = take_number(&tokens, &mut pos)?;
                let x = take_number(&tokens, &mut pos)?;
                let y = take_number(&tokens, &mut pos)?;
                let (p1, p2) = if relative {
                    (add(cur, (x1, y1)), add(cur, (x, y)))
                } else {
                    ((x1, y1), (x, y))
                };
                flatten_quadratic(cur, p1, p2, &mut current);
                cur = p2;
            }
            other => {
                return Err(err(format!("unsupported path command '{}'", other as char)));
            }
        }
    }
    flush_open(&mut subs, &mut current);
    Ok(Path { subs })
}

fn flush_closed(subs: &mut Vec<SubPath>, current: &mut Vec<Point>) {
    if current.len() >= 2 {
        subs.push(SubPath {
            pts: std::mem::take(current),
            closed: true,
        });
    } else {
        current.clear();
    }
}

fn rect_path(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Result<Path, DecodeError> {
    if w < 0.0 || h < 0.0 || rx < 0.0 || ry < 0.0 {
        return Err(err("negative rectangle size or radius"));
    }
    let (mut rx, mut ry) = (rx.min(w / 2.0), ry.min(h / 2.0));
    if rx > 0.0 && ry == 0.0 {
        ry = rx;
    }
    if ry > 0.0 && rx == 0.0 {
        rx = ry;
    }
    let pts = if rx <= 0.0 || ry <= 0.0 {
        vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
    } else {
        let mut pts = Vec::with_capacity(4 * (ARC_SEGMENTS + 1));
        let corners = [
            (x + w - rx, y + ry, -90.0_f32),
            (x + w - rx, y + h - ry, 0.0_f32),
            (x + rx, y + h - ry, 90.0_f32),
            (x + rx, y + ry, 180.0_f32),
        ];
        for (cx, cy, from) in corners {
            for step in 0..=ARC_SEGMENTS {
                let angle = (from + 90.0 * step as f32 / ARC_SEGMENTS as f32).to_radians();
                pts.push((cx + rx * angle.cos(), cy + ry * angle.sin()));
            }
        }
        pts
    };
    Ok(Path {
        subs: vec![SubPath { pts, closed: true }],
    })
}

fn ellipse_path(cx: f32, cy: f32, rx: f32, ry: f32) -> Result<Path, DecodeError> {
    if rx < 0.0 || ry < 0.0 {
        return Err(err("negative ellipse radius"));
    }
    let pts = (0..ELLIPSE_SEGMENTS)
        .map(|i| {
            let angle = std::f32::consts::TAU * i as f32 / ELLIPSE_SEGMENTS as f32;
            (cx + rx * angle.cos(), cy + ry * angle.sin())
        })
        .collect();
    Ok(Path {
        subs: vec![SubPath { pts, closed: true }],
    })
}

fn transform_path(path: &Path, matrix: Matrix) -> Path {
    Path {
        subs: path
            .subs
            .iter()
            .map(|sub| SubPath {
                pts: sub.pts.iter().map(|&p| matrix.apply(p)).collect(),
                closed: sub.closed,
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Rasterization with 4x4 coverage
// ---------------------------------------------------------------------------

fn is_left(a: Point, b: Point, p: Point) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1)
}

fn point_in_path(p: Point, path: &Path, rule: FillRule) -> bool {
    let mut inside = false;
    let mut winding = 0_i32;
    for sub in &path.subs {
        let n = sub.pts.len();
        if n < 2 {
            continue;
        }
        let edges = if sub.closed { n } else { n - 1 };
        for i in 0..edges {
            let a = sub.pts[i];
            let b = sub.pts[(i + 1) % n];
            match rule {
                FillRule::EvenOdd => {
                    if (a.1 > p.1) != (b.1 > p.1)
                        && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0
                    {
                        inside = !inside;
                    }
                }
                FillRule::Nonzero => {
                    if a.1 <= p.1 {
                        if b.1 > p.1 && is_left(a, b, p) > 0.0 {
                            winding += 1;
                        }
                    } else if b.1 <= p.1 && is_left(a, b, p) < 0.0 {
                        winding -= 1;
                    }
                }
            }
        }
    }
    match rule {
        FillRule::EvenOdd => inside,
        FillRule::Nonzero => winding != 0,
    }
}

fn segments(path: &Path) -> Vec<(Point, Point)> {
    let mut out = Vec::new();
    for sub in &path.subs {
        let n = sub.pts.len();
        if n < 2 {
            continue;
        }
        for pair in sub.pts.windows(2) {
            out.push((pair[0], pair[1]));
        }
        if sub.closed {
            out.push((sub.pts[n - 1], sub.pts[0]));
        }
    }
    out
}

fn near_segment(p: Point, a: Point, b: Point, half: f32) -> bool {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let length_squared = dx * dx + dy * dy;
    let t = if length_squared == 0.0 {
        0.0
    } else {
        ((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_squared
    }
    .clamp(0.0, 1.0);
    let ex = p.0 - (a.0 + t * dx);
    let ey = p.1 - (a.1 + t * dy);
    ex * ex + ey * ey <= half * half
}

/// Source-over composite one paint over `pixels` at byte offset `index`.
fn blend(pixels: &mut [u8], index: usize, color: Color, coverage: f32) {
    let src_alpha = (color.a * coverage).clamp(0.0, 1.0);
    if src_alpha <= 0.0 {
        return;
    }
    let dst_alpha = f32::from(pixels[index + 3]) / 255.0;
    let out_alpha = src_alpha + dst_alpha * (1.0 - src_alpha);
    if out_alpha <= 0.0 {
        return;
    }
    let src_weight = src_alpha / out_alpha;
    let dst_weight = dst_alpha * (1.0 - src_alpha) / out_alpha;
    let channels = [color.r, color.g, color.b];
    for (offset, src) in channels.into_iter().enumerate() {
        let dst = f32::from(pixels[index + offset]) / 255.0;
        let value = (src * src_weight + dst * dst_weight) * 255.0;
        pixels[index + offset] = value.round().clamp(0.0, 255.0) as u8;
    }
    pixels[index + 3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// A rasterised pattern tile together with its placement in screen space.
struct PatternTile {
    image: ImageBuffer,
    origin_x: f32,
    origin_y: f32,
    width: f32,
    height: f32,
}

fn pattern_tile(
    pattern: &Pattern,
    bbox: (f32, f32, f32, f32),
    resources: &Resources,
    depth: usize,
) -> PatternTile {
    let (origin_x, origin_y, width_px, height_px) = match pattern.units {
        Units::ObjectBoundingBox => {
            let box_width = bbox.2 - bbox.0;
            let box_height = bbox.3 - bbox.1;
            (
                bbox.0 + pattern.x * box_width,
                bbox.1 + pattern.y * box_height,
                pattern.width * box_width,
                pattern.height * box_height,
            )
        }
        Units::UserSpaceOnUse => {
            let origin = pattern.matrix.apply((pattern.x, pattern.y));
            (
                origin.0,
                origin.1,
                pattern.width * pattern.matrix.a.abs(),
                pattern.height * pattern.matrix.d.abs(),
            )
        }
    };
    let width = width_px.round().clamp(1.0, MAX_IMAGE_DIMENSION as f32);
    let height = height_px.round().clamp(1.0, MAX_IMAGE_DIMENSION as f32);
    let mut image = ImageBuffer::new(width as u32, height as u32);
    if depth < MAX_PATTERN_DEPTH {
        // Pattern content uses the referencing element's user space. Shifting
        // by the tile origin puts that content into the small tile buffer.
        let mut style = Style::default();
        if style
            .apply(&pattern.node, pattern.matrix, resources)
            .is_ok()
        {
            let child_matrix = Matrix::translation(-origin_x, -origin_y).mul(pattern.matrix);
            for child in &pattern.node.children {
                let _ = render_node(
                    child,
                    child_matrix,
                    &style,
                    &mut image,
                    resources,
                    depth + 1,
                );
            }
        }
    }
    PatternTile {
        image,
        origin_x,
        origin_y,
        width,
        height,
    }
}

fn sample_pattern(tile: &PatternTile, pattern: &Pattern, sample: Point) -> Color {
    let inverse = pattern.transform.inverse().unwrap_or(Matrix::IDENTITY);
    let local = inverse.apply((sample.0 - tile.origin_x, sample.1 - tile.origin_y));
    let u = (local.0 / tile.width).rem_euclid(1.0);
    let v = (local.1 / tile.height).rem_euclid(1.0);
    let [r, g, b, a] = tile.image.pixel(
        (u * tile.image.width as f32) as u32,
        (v * tile.image.height as f32) as u32,
    );
    Color {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: f32::from(a) / 255.0,
    }
}

fn draw_path(
    buffer: &mut ImageBuffer,
    path: &Path,
    style: &Style,
    resources: &Resources,
    depth: usize,
) {
    if path.subs.is_empty() {
        return;
    }
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for sub in &path.subs {
        for &(x, y) in &sub.pts {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    let pad = if style.stroke.is_some() {
        style.stroke_width / 2.0 + 1.0
    } else {
        1.0
    };
    let x_start = ((min_x - pad).floor() as i32).max(0);
    let y_start = ((min_y - pad).floor() as i32).max(0);
    let x_end = ((max_x + pad).ceil() as i32 + 1).min(buffer.width as i32);
    let y_end = ((max_y + pad).ceil() as i32 + 1).min(buffer.height as i32);
    if x_start >= x_end || y_start >= y_end {
        return;
    }

    let stroke_segments = if style.stroke.is_some() {
        segments(path)
    } else {
        Vec::new()
    };
    let half_width = style.stroke_width / 2.0;
    let bbox = (min_x, min_y, max_x, max_y);
    let tile_for = |paint: Option<&Paint>| match paint {
        Some(Paint::Pattern(pattern)) => Some(pattern_tile(pattern, bbox, resources, depth)),
        _ => None,
    };
    let fill_tile = tile_for(style.fill.as_ref());
    let stroke_tile = tile_for(style.stroke.as_ref());

    for y in y_start..y_end {
        for x in x_start..x_end {
            let mut fill_hits = 0_u32;
            let mut stroke_hits = 0_u32;
            let mut fill_sum = [0.0_f32; 4];
            let mut stroke_sum = [0.0_f32; 4];
            for sy in 0..AA {
                let py = y as f32 + (sy as f32 + 0.5) / AA as f32;
                for sx in 0..AA {
                    let px = x as f32 + (sx as f32 + 0.5) / AA as f32;
                    let sample = (px, py);
                    if style.fill.is_some() && point_in_path(sample, path, style.fill_rule) {
                        fill_hits += 1;
                        if let Some(paint) = &style.fill {
                            let color = paint_color(paint, sample, bbox, fill_tile.as_ref());
                            for (slot, value) in fill_sum
                                .iter_mut()
                                .zip([color.r, color.g, color.b, color.a])
                            {
                                *slot += value;
                            }
                        }
                    }
                    if style.stroke.is_some()
                        && stroke_segments
                            .iter()
                            .any(|&(a, b)| near_segment(sample, a, b, half_width))
                    {
                        stroke_hits += 1;
                        if let Some(paint) = &style.stroke {
                            let color = paint_color(paint, sample, bbox, stroke_tile.as_ref());
                            for (slot, value) in stroke_sum
                                .iter_mut()
                                .zip([color.r, color.g, color.b, color.a])
                            {
                                *slot += value;
                            }
                        }
                    }
                }
            }
            let index = ((y as usize) * buffer.width as usize + x as usize) * 4;
            if let Some(paint) = &style.fill
                && fill_hits > 0
            {
                let color = match paint {
                    Paint::Solid(color) => *color,
                    _ => average(fill_sum, fill_hits),
                };
                let alpha = fill_hits as f32 / AA_SAMPLES * style.fill_opacity * style.opacity;
                blend(&mut buffer.pixels, index, color, alpha);
            }
            if let Some(paint) = &style.stroke
                && stroke_hits > 0
            {
                let color = match paint {
                    Paint::Solid(color) => *color,
                    _ => average(stroke_sum, stroke_hits),
                };
                let alpha = stroke_hits as f32 / AA_SAMPLES * style.stroke_opacity * style.opacity;
                blend(&mut buffer.pixels, index, color, alpha);
            }
        }
    }
}

/// Resolves a paint at one sub-pixel sample point.
fn paint_color(
    paint: &Paint,
    sample: Point,
    bbox: (f32, f32, f32, f32),
    tile: Option<&PatternTile>,
) -> Color {
    match paint {
        Paint::Solid(color) => *color,
        Paint::Gradient(gradient) => sample_gradient(gradient, bbox, sample),
        Paint::Pattern(pattern) => tile
            .map(|tile| sample_pattern(tile, pattern, sample))
            .unwrap_or(Color::TRANSPARENT),
    }
}

fn average(sum: [f32; 4], hits: u32) -> Color {
    let hits = hits as f32;
    Color {
        r: sum[0] / hits,
        g: sum[1] / hits,
        b: sum[2] / hits,
        a: sum[3] / hits,
    }
}

// ---------------------------------------------------------------------------
// Transforms and document traversal
// ---------------------------------------------------------------------------

fn transform_function(name: &str, args: &[f32]) -> Result<Matrix, DecodeError> {
    let first = || {
        args.first()
            .copied()
            .ok_or_else(|| err(format!("{name}() is missing an argument")))
    };
    match name {
        "translate" => Ok(Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: first()?,
            f: args.get(1).copied().unwrap_or(0.0),
        }),
        "scale" => {
            let sx = first()?;
            Ok(Matrix {
                a: sx,
                b: 0.0,
                c: 0.0,
                d: args.get(1).copied().unwrap_or(sx),
                e: 0.0,
                f: 0.0,
            })
        }
        "rotate" => {
            let radians = first()?.to_radians();
            let (sin, cos) = radians.sin_cos();
            let rotation = Matrix {
                a: cos,
                b: sin,
                c: -sin,
                d: cos,
                e: 0.0,
                f: 0.0,
            };
            if args.len() >= 3 {
                let (cx, cy) = (args[1], args[2]);
                let to_origin = Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: cx,
                    f: cy,
                };
                let from_origin = Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: -cx,
                    f: -cy,
                };
                Ok(to_origin.mul(rotation).mul(from_origin))
            } else {
                Ok(rotation)
            }
        }
        "matrix" => {
            if args.len() != 6 {
                return Err(err("matrix() requires exactly six arguments"));
            }
            Ok(Matrix {
                a: args[0],
                b: args[1],
                c: args[2],
                d: args[3],
                e: args[4],
                f: args[5],
            })
        }
        other => Err(err(format!("unsupported transform function '{other}()'"))),
    }
}

fn parse_transform(text: &str) -> Result<Matrix, DecodeError> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut result = Matrix::IDENTITY;
    while pos < bytes.len() {
        if bytes[pos].is_ascii_whitespace() || bytes[pos] == b',' {
            pos += 1;
            continue;
        }
        let start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_alphabetic) {
            pos += 1;
        }
        if pos == start {
            return Err(err("invalid transform list"));
        }
        let name = std::str::from_utf8(&bytes[start..pos])
            .map_err(|_| err("invalid UTF-8 in transform"))?
            .to_ascii_lowercase();
        while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
            pos += 1;
        }
        if bytes.get(pos) != Some(&b'(') {
            return Err(err("expected '(' after transform function"));
        }
        pos += 1;
        let args_start = pos;
        while pos < bytes.len() && bytes[pos] != b')' {
            pos += 1;
        }
        if pos >= bytes.len() {
            return Err(err("unterminated transform function"));
        }
        let args = std::str::from_utf8(&bytes[args_start..pos])
            .map_err(|_| err("invalid UTF-8 in transform"))?;
        pos += 1;
        result = result.mul(transform_function(&name, &parse_number_list(args)?)?);
    }
    Ok(result)
}

fn required_attr<'a>(node: &'a Node, name: &str) -> Result<&'a str, DecodeError> {
    node.attr(name)
        .ok_or_else(|| err(format!("<{}> requires a '{name}' attribute", node.name)))
}

fn optional_length(node: &Node, name: &str, default: f32) -> Result<f32, DecodeError> {
    match node.attr(name) {
        Some(value) => parse_length(value),
        None => Ok(default),
    }
}

fn render_node(
    node: &Node,
    parent: Matrix,
    inherited: &Style,
    buffer: &mut ImageBuffer,
    resources: &Resources,
    depth: usize,
) -> Result<(), DecodeError> {
    if matches!(
        node.name.as_str(),
        "title"
            | "desc"
            | "metadata"
            | "style"
            | "defs"
            | "linearGradient"
            | "radialGradient"
            | "pattern"
            | "stop"
    ) {
        return Ok(());
    }

    let matrix = match node.attr("transform") {
        Some(text) => parent.mul(parse_transform(text)?),
        None => parent,
    };
    let mut style = inherited.clone();
    style.apply(node, matrix, resources)?;

    let path = match node.name.as_str() {
        "g" | "svg" => {
            for child in &node.children {
                render_node(child, matrix, &style, buffer, resources, depth)?;
            }
            return Ok(());
        }
        "rect" => {
            let x = optional_length(node, "x", 0.0)?;
            let y = optional_length(node, "y", 0.0)?;
            let w = optional_length(node, "width", 0.0)?;
            let h = optional_length(node, "height", 0.0)?;
            let rx = optional_length(node, "rx", 0.0)?;
            let ry = optional_length(node, "ry", 0.0)?;
            rect_path(x, y, w, h, rx, ry)?
        }
        "circle" => {
            let cx = optional_length(node, "cx", 0.0)?;
            let cy = optional_length(node, "cy", 0.0)?;
            let r = optional_length(node, "r", 0.0)?;
            ellipse_path(cx, cy, r, r)?
        }
        "ellipse" => {
            let cx = optional_length(node, "cx", 0.0)?;
            let cy = optional_length(node, "cy", 0.0)?;
            let rx = optional_length(node, "rx", 0.0)?;
            let ry = optional_length(node, "ry", 0.0)?;
            ellipse_path(cx, cy, rx, ry)?
        }
        "line" => {
            let x1 = optional_length(node, "x1", 0.0)?;
            let y1 = optional_length(node, "y1", 0.0)?;
            let x2 = optional_length(node, "x2", 0.0)?;
            let y2 = optional_length(node, "y2", 0.0)?;
            Path {
                subs: vec![SubPath {
                    pts: vec![(x1, y1), (x2, y2)],
                    closed: false,
                }],
            }
        }
        "polyline" | "polygon" => {
            let numbers = parse_number_list(required_attr(node, "points")?)?;
            if numbers.len() % 2 != 0 {
                return Err(err("points list has an odd number of coordinates"));
            }
            let pts: Vec<Point> = numbers
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| (p[0], p[1]))
                .collect();
            Path {
                subs: vec![SubPath {
                    pts,
                    closed: node.name == "polygon",
                }],
            }
        }
        "path" => parse_path_data(required_attr(node, "d")?)?,
        other => return Err(err(format!("unsupported SVG element <{other}>"))),
    };

    let path = transform_path(&path, matrix);
    draw_path(buffer, &path, &style, resources, depth);
    Ok(())
}

#[derive(Clone, Copy)]
struct ViewBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn parse_view_box(text: &str) -> Result<ViewBox, DecodeError> {
    let values = parse_number_list(text)?;
    if values.len() != 4 {
        return Err(err("viewBox requires exactly four numbers"));
    }
    if values[2] <= 0.0 || values[3] <= 0.0 {
        return Err(err("viewBox width and height must be positive"));
    }
    Ok(ViewBox {
        x: values[0],
        y: values[1],
        width: values[2],
        height: values[3],
    })
}

/// Rasterizes an SVG document into a transparent RGBA buffer.
///
/// Returns a [`DecodeError`] for malformed XML, unsupported elements, path
/// commands, colours or transforms, and for invalid or oversized dimensions.
pub fn rasterize(svg: &str) -> Result<ImageBuffer, DecodeError> {
    let root = Xml::new(svg).parse_document()?;
    if root.name != "svg" {
        return Err(err("the root element must be <svg>"));
    }

    let view_box = match root.attr("viewBox") {
        Some(value) => Some(parse_view_box(value)?),
        None => None,
    };
    let (width_f, height_f) = match (root.attr("width"), root.attr("height"), view_box) {
        (Some(w), Some(h), _) => (parse_length(w)?, parse_length(h)?),
        (Some(w), None, Some(vb)) => {
            let width = parse_length(w)?;
            (width, width * vb.height / vb.width)
        }
        (None, Some(h), Some(vb)) => {
            let height = parse_length(h)?;
            (height * vb.width / vb.height, height)
        }
        (None, None, Some(vb)) => (vb.width, vb.height),
        _ => return Err(err("<svg> requires width and height or a viewBox")),
    };
    if !width_f.is_finite()
        || !height_f.is_finite()
        || width_f <= 0.0
        || height_f <= 0.0
        || width_f > MAX_IMAGE_DIMENSION as f32
        || height_f > MAX_IMAGE_DIMENSION as f32
    {
        return Err(err("invalid or oversized SVG dimensions"));
    }
    let width = width_f.round() as u32;
    let height = height_f.round() as u32;
    check_dimensions(width, height)?;
    let mut buffer = ImageBuffer::new(width, height);

    let base = match view_box {
        Some(vb) => {
            let scale_x = width_f / vb.width;
            let scale_y = height_f / vb.height;
            let none = root
                .attr("preserveAspectRatio")
                .is_some_and(|value| value.trim_start().starts_with("none"));
            if none {
                Matrix {
                    a: scale_x,
                    b: 0.0,
                    c: 0.0,
                    d: scale_y,
                    e: -vb.x * scale_x,
                    f: -vb.y * scale_y,
                }
            } else {
                let scale = scale_x.min(scale_y);
                Matrix {
                    a: scale,
                    b: 0.0,
                    c: 0.0,
                    d: scale,
                    e: (width_f - vb.width * scale) / 2.0 - vb.x * scale,
                    f: (height_f - vb.height * scale) / 2.0 - vb.y * scale,
                }
            }
        }
        None => Matrix::IDENTITY,
    };

    let mut defs = HashMap::new();
    Resources::collect(&root, &mut defs);
    let resources = Resources { defs };

    let style = Style::default();
    for child in &root.children {
        render_node(child, base, &style, &mut buffer, &resources, 0)?;
    }
    Ok(buffer)
}

/// Convenience wrapper for callers holding SVG bytes.
pub fn rasterize_bytes(bytes: &[u8]) -> Result<ImageBuffer, DecodeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| err("SVG data is not valid UTF-8"))?;
    rasterize(text.strip_prefix('\u{feff}').unwrap_or(text))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn render(svg: &str) -> ImageBuffer {
        rasterize(svg).expect("test SVG should render")
    }

    #[test]
    fn parses_dimensions_and_view_box() {
        let image = render(
            "<svg xmlns='http://www.w3.org/2000/svg' width='20' height='12' \
             viewBox='0 0 10 6'></svg>",
        );
        assert_eq!((image.width, image.height), (20, 12));
        assert_eq!(image.pixel(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn view_box_can_supply_dimensions() {
        let image = render("<svg viewBox='0 0 7 5'></svg>");
        assert_eq!((image.width, image.height), (7, 5));
    }

    #[test]
    fn fills_a_rect_and_anti_aliases_its_edge() {
        let image = render(
            "<svg width='10' height='10'>\
             <rect x='2.5' y='2.5' width='5' height='5' fill='#ff0000'/>\
             </svg>",
        );
        assert_eq!(image.pixel(5, 5), [255, 0, 0, 255]);
        assert_eq!(image.pixel(0, 0), [0, 0, 0, 0]);
        let edge = image.pixel(2, 2)[3];
        assert!(
            (1..255).contains(&edge),
            "edge alpha should be partial: {edge}"
        );
    }

    #[test]
    fn rounded_rect_leaves_its_corner_empty() {
        let image = render(
            "<svg width='20' height='20'>\
             <rect x='2' y='2' width='16' height='16' rx='6' ry='6' fill='blue'/>\
             </svg>",
        );
        assert_eq!(image.pixel(10, 10)[3], 255);
        assert_eq!(image.pixel(2, 2)[3], 0);
    }

    #[test]
    fn fills_a_circle_and_an_ellipse() {
        let image = render(
            "<svg width='20' height='20'>\
             <circle cx='6' cy='6' r='4' fill='green'/>\
             <ellipse cx='14' cy='14' rx='5' ry='3' fill='rgb(255, 0, 255)'/>\
             </svg>",
        );
        assert_eq!(image.pixel(6, 6), [0, 128, 0, 255]);
        assert_eq!(image.pixel(14, 14), [255, 0, 255, 255]);
        assert_eq!(image.pixel(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn strokes_a_line_and_polyline() {
        let image = render(
            "<svg width='12' height='12'>\
             <line x1='2' y1='2' x2='10' y2='2' stroke='black' stroke-width='2'/>\
             <polyline points='2,10 6,10 10,10' fill='none' stroke='red' stroke-width='2'/>\
             </svg>",
        );
        assert_eq!(image.pixel(6, 2), [0, 0, 0, 255]);
        assert_eq!(image.pixel(6, 10), [255, 0, 0, 255]);
        assert_eq!(image.pixel(6, 6), [0, 0, 0, 0]);
    }

    #[test]
    fn fills_a_polygon_with_nonzero_winding() {
        let image = render(
            "<svg width='12' height='12'>\
             <polygon points='1,1 11,1 6,11' fill='orange'/>\
             </svg>",
        );
        assert_eq!(image.pixel(6, 4), [255, 165, 0, 255]);
        assert_eq!(image.pixel(1, 11), [0, 0, 0, 0]);
    }

    #[test]
    fn renders_path_commands_including_curves() {
        let image = render(
            "<svg width='20' height='20'>\
             <path d='M2 2 C2 12 18 12 18 2 L18 18 L2 18 Z' fill='purple'/>\
             </svg>",
        );
        assert_eq!(image.pixel(10, 14), [128, 0, 128, 255]);
        assert_eq!(image.pixel(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn renders_relative_horizontal_and_vertical_commands() {
        let image = render(
            "<svg width='12' height='12'>\
             <path d='m2 2 h8 v8 h-8 z' fill='#0f0'/>\
             </svg>",
        );
        assert_eq!(image.pixel(6, 6), [0, 255, 0, 255]);
        assert_eq!(image.pixel(11, 11), [0, 0, 0, 0]);
    }

    #[test]
    fn group_transforms_are_applied_and_inherited() {
        let image = render(
            "<svg width='20' height='20'>\
             <g fill='red' transform='translate(5, 5) scale(2)'>\
               <rect x='0' y='0' width='4' height='4'/>\
             </g></svg>",
        );
        assert_eq!(image.pixel(7, 7), [255, 0, 0, 255]);
        assert_eq!(image.pixel(2, 2), [0, 0, 0, 0]);
        assert_eq!(image.pixel(14, 13), [0, 0, 0, 0]);
    }

    #[test]
    fn rotation_about_a_center_is_supported() {
        let image = render(
            "<svg width='20' height='20'>\
             <rect x='8' y='2' width='4' height='4' fill='red' \
                   transform='rotate(90, 10, 10)'/>\
             </svg>",
        );
        assert_eq!(image.pixel(16, 10), [255, 0, 0, 255]);
    }

    #[test]
    fn opacity_produces_partial_alpha() {
        let image = render(
            "<svg width='10' height='10'>\
             <rect x='2' y='2' width='6' height='6' fill='red' fill-opacity='0.5'/>\
             </svg>",
        );
        let alpha = image.pixel(5, 5)[3];
        assert!(
            (126..=130).contains(&alpha),
            "alpha should be ~128: {alpha}"
        );
    }

    #[test]
    fn style_attribute_and_inline_css_are_accepted() {
        let image = render(
            "<svg width='10' height='10'>\
             <rect x='2' y='2' width='6' height='6' style='fill: #00f; stroke: none'/>\
             </svg>",
        );
        assert_eq!(image.pixel(5, 5), [0, 0, 255, 255]);
    }

    #[test]
    fn comments_and_declarations_are_skipped() {
        let image = render(
            "<?xml version='1.0'?><!-- hello -->\
             <svg width='8' height='8'><rect width='8' height='8' fill='red'/></svg>",
        );
        assert_eq!(image.pixel(4, 4), [255, 0, 0, 255]);
    }

    #[test]
    fn rejects_missing_dimensions() {
        let error = rasterize("<svg></svg>").expect_err("must fail");
        assert!(error.message.contains("width"));
    }

    #[test]
    fn rejects_unsupported_elements() {
        let error = rasterize("<svg width='4' height='4'><text>hi</text></svg>")
            .expect_err("text is not supported");
        assert!(error.message.contains("<text>"));
    }

    #[test]
    fn rejects_unsupported_path_commands() {
        let error = rasterize(
            "<svg width='4' height='4'>\
             <path d='M0 0 A2 2 0 0 1 4 4' fill='red'/>\
             </svg>",
        )
        .expect_err("arcs are not supported");
        assert!(error.message.contains("'A'"));
    }

    #[test]
    fn rejects_unsupported_colours_and_transforms() {
        assert!(
            rasterize(
                "<svg width='4' height='4'><rect width='4' height='4' fill='chartreuse'/></svg>"
            )
            .is_err()
        );
        assert!(
            rasterize(
                "<svg width='4' height='4'><rect width='4' height='4' fill='red' \
             transform='skewX(20)'/></svg>"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_malformed_xml() {
        assert!(rasterize("<svg width='4' height='4'><rect></svg>").is_err());
        assert!(rasterize("<svg width='4' height='4'>").is_err());
        assert!(rasterize("<rect width='4' height='4'/>").is_err());
    }

    #[test]
    fn even_odd_fill_rule_creates_a_hole() {
        let image = render(
            "<svg width='12' height='12'>\
             <path d='M1 1 H11 V11 H1 Z M3 3 H9 V9 H3 Z' \
                   fill='red' fill-rule='evenodd'/>\
             </svg>",
        );
        assert_eq!(image.pixel(2, 2), [255, 0, 0, 255]);
        assert_eq!(image.pixel(6, 6), [0, 0, 0, 0]);
    }

    #[test]
    fn rasterize_bytes_decodes_utf8() {
        let image =
            rasterize_bytes(b"<svg width='4' height='4'><rect width='4' height='4'/></svg>")
                .expect("bytes should render");
        assert_eq!(image.pixel(0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn rejects_invalid_utf8_bytes() {
        assert!(rasterize_bytes(b"\xff\xfe\x00").is_err());
    }

    #[test]
    fn unresolved_paint_reference_is_transparent_not_black() {
        let image = render(
            "<svg width='10' height='10'>\
               <rect x='2' y='2' width='6' height='6' fill='url(#missing)'/>\
               </svg>",
        );
        for y in 2..8 {
            for x in 2..8 {
                assert_eq!(image.pixel(x, y), [0, 0, 0, 0], "pixel ({x}, {y})");
            }
        }

        // An unresolved stroke on an otherwise unfilled shape is transparent too.
        let image = render(
            "<svg width='10' height='10'>\
               <rect x='2' y='2' width='6' height='6' fill='none' \
                     stroke='url(#missing)' stroke-width='2'/>\
               </svg>",
        );
        assert_eq!(image.pixel(3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn linear_gradient_stops_resolve_left_to_right() {
        let image = render(
            "<svg width='10' height='4'>\
               <defs><linearGradient id='g'>\
                 <stop offset='0' stop-color='#ff0000'/>\
                 <stop offset='1' stop-color='#0000ff'/>\
               </linearGradient></defs>\
               <rect width='10' height='4' fill='url(#g)'/>\
               </svg>",
        );
        let left = image.pixel(0, 2);
        let right = image.pixel(9, 2);
        assert!(left[0] > 200 && left[2] < 40, "left stop: {left:?}");
        assert!(right[2] > 200 && right[0] < 40, "right stop: {right:?}");
        let middle = image.pixel(5, 2);
        assert!(
            (100..=150).contains(&middle[0]) && (100..=150).contains(&middle[2]),
            "unexpected mid-stop colour: {middle:?}"
        );
        assert_eq!(middle[3], 255);
    }

    #[test]
    fn linear_gradient_supports_vertical_direction_and_angle() {
        let vertical = render(
            "<svg width='4' height='10'>\
               <defs><linearGradient id='g' x1='0' y1='0' x2='0' y2='1'>\
                 <stop offset='0' stop-color='white'/>\
                 <stop offset='1' stop-color='black'/>\
               </linearGradient></defs>\
               <rect width='4' height='10' fill='url(#g)'/>\
               </svg>",
        );
        let top = vertical.pixel(2, 0);
        let bottom = vertical.pixel(2, 9);
        assert!(top.iter().take(3).all(|channel| *channel > 200), "{top:?}");
        assert!(
            bottom.iter().take(3).all(|channel| *channel < 40),
            "{bottom:?}"
        );
        assert_eq!(top[3], 255);
        assert_eq!(bottom[3], 255);

        // A rotated gradient uses gradientTransform="rotate(...)".
        let angled = render(
            "<svg width='10' height='10'>\
               <defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='0' \
                     gradientTransform='rotate(90, 0.5, 0.5)'>\
                 <stop offset='0' stop-color='red'/>\
                 <stop offset='1' stop-color='blue'/>\
               </linearGradient></defs>\
               <rect width='10' height='10' fill='url(#g)'/>\
               </svg>",
        );
        let angled_top = angled.pixel(5, 0);
        let angled_bottom = angled.pixel(5, 9);
        assert!(
            angled_top[0] > 200 && angled_top[2] < 40,
            "angled top: {angled_top:?}"
        );
        assert!(
            angled_bottom[2] > 200 && angled_bottom[0] < 40,
            "angled bottom: {angled_bottom:?}"
        );
    }

    #[test]
    fn radial_gradient_stops_resolve_from_the_center() {
        let image = render(
            "<svg width='20' height='20'>\
               <defs><radialGradient id='g' r='0.45'>\
                 <stop offset='0' stop-color='white'/>\
                 <stop offset='1' stop-color='black'/>\
               </radialGradient></defs>\
               <circle cx='10' cy='10' r='10' fill='url(#g)'/>\
               </svg>",
        );
        let center = image.pixel(10, 10);
        assert!(
            center.iter().take(3).all(|channel| *channel > 200),
            "{center:?}"
        );
        let edge = image.pixel(19, 10);
        assert!(edge.iter().take(3).all(|channel| *channel < 40), "{edge:?}");
    }

    #[test]
    fn pattern_reference_tiles_its_child_shapes() {
        let image = render(
            "<svg width='12' height='4'>\
               <defs><pattern id='p' width='4' height='4' patternUnits='userSpaceOnUse'>\
                 <rect width='4' height='4' fill='red'/>\
                 <rect width='2' height='2' fill='blue'/>\
               </pattern></defs>\
               <rect width='12' height='4' fill='url(#p)'/>\
               </svg>",
        );
        assert_eq!(image.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(image.pixel(3, 3), [255, 0, 0, 255]);
        assert_eq!(image.pixel(4, 0), [0, 0, 255, 255]);
    }
}
