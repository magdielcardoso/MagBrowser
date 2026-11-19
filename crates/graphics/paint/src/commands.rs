//! Display-list construction from a laid-out document.

use browser_css::{Color, Value};
use browser_layout::tree::MAX_SVG_SOURCE_BYTES;
use browser_layout::{BoxType, LayoutBox, LayoutTree, Rect, Size};
use browser_style::{FilterFunction, TransformFunction};

use crate::display_list::{DisplayCommand, DisplayFilter, DisplayList};
use crate::images::ImageData;
use crate::raster::{ImageBuffer, Rasterizer};

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use browser_dom::NodeId;

/// Builds the display list for a laid-out page without external images.
pub fn build(layout: &LayoutTree) -> DisplayList {
    build_with_images(layout, &HashMap::new())
}

/// Builds the display list, using decoded images for `<img>` nodes when present.
///
/// Nodes without a decoded image are painted with a deterministic placeholder, so
/// a missing or unsupported image never leaves a hole in the page.
pub fn build_with_images(layout: &LayoutTree, images: &HashMap<NodeId, ImageData>) -> DisplayList {
    let mut list = DisplayList::new();
    let mut image_indices: HashMap<NodeId, usize> = HashMap::new();
    if let Some(root) = layout.root {
        paint_box(
            layout,
            root,
            &mut list,
            0.0,
            0.0,
            images,
            &mut image_indices,
        );
    }
    list
}

/// Builds the standard display list together with the extended effect commands
/// ([`crate::effects::EffectsList`]) for a laid-out page.
///
/// This is the bridge for callers that need mask / clip-path / blend / gradient
/// effects: the regular list stays executable by the current rasterizer while the
/// effects list holds data the raster/compositor owner can execute (see the
/// integration notes at the top of [`crate::effects`]). Raw declaration strings for
/// properties `browser_style` does not model yet can be supplied via `raw`.
pub fn build_with_effects(
    layout: &LayoutTree,
    images: &HashMap<NodeId, ImageData>,
    raw: &HashMap<NodeId, crate::effects::RawEffects>,
) -> (DisplayList, crate::effects::EffectsList) {
    let mut list = build_with_images(layout, images);
    let effects = crate::effects::build_effects_with(layout, raw);
    append_executable_effects(&mut list, &effects);
    (list, effects)
}

fn first_rect(commands: &[DisplayCommand]) -> Option<Rect> {
    commands.iter().find_map(|command| match command {
        DisplayCommand::FillRect { rect, .. }
        | DisplayCommand::DrawImage { rect, .. }
        | DisplayCommand::DrawBorder { rect, .. } => Some(*rect),
        DisplayCommand::DrawRoundedRect { rect, .. }
        | DisplayCommand::DrawImageWithRendering { rect, .. }
        | DisplayCommand::DrawBorderImage { rect, .. }
        | DisplayCommand::DrawLinearGradient { rect, .. }
        | DisplayCommand::DrawRadialGradient { rect, .. }
        | DisplayCommand::DrawConicGradient { rect, .. }
        | DisplayCommand::DrawShadow { rect, .. } => Some(*rect),
        DisplayCommand::DrawTransformed { child_commands, .. }
        | DisplayCommand::DrawFiltered { child_commands, .. }
        | DisplayCommand::DrawMasked { child_commands, .. }
        | DisplayCommand::DrawClipped { child_commands, .. }
        | DisplayCommand::DrawBlended { child_commands, .. } => first_rect(child_commands),
        _ => None,
    })
}

/// Lowers parsed effect data into commands the software rasterizer executes.
fn append_executable_effects(list: &mut DisplayList, effects: &crate::effects::EffectsList) {
    for effect in &effects.commands {
        use crate::effects::ExtendedCommand as E;
        let command = match effect {
            E::DrawMasked {
                child_commands,
                mask,
            } => DisplayCommand::DrawMasked {
                rect: first_rect(child_commands).unwrap_or_default(),
                child_commands: child_commands.clone(),
                mask: mask.clone(),
            },
            E::DrawClipped {
                child_commands,
                clip,
                rect,
            } => DisplayCommand::DrawClipped {
                child_commands: child_commands.clone(),
                clip: clip.clone(),
                rect: *rect,
            },
            E::DrawBlended {
                child_commands,
                blend_mode,
                backdrop,
            } => DisplayCommand::DrawBlended {
                child_commands: child_commands.clone(),
                blend_mode: *blend_mode,
                backdrop: backdrop.clone(),
            },
            E::DrawRadialGradient {
                rect,
                gradient,
                radius,
            } => DisplayCommand::DrawRadialGradient {
                rect: *rect,
                gradient: gradient.clone(),
                radius: *radius,
            },
            E::DrawConicGradient {
                rect,
                gradient,
                radius,
            } => DisplayCommand::DrawConicGradient {
                rect: *rect,
                gradient: gradient.clone(),
                radius: *radius,
            },
            E::DrawImageWithRendering {
                rect,
                image,
                rendering,
            } => DisplayCommand::DrawImageWithRendering {
                rect: *rect,
                image: *image,
                rendering: *rendering,
            },
            // border-image needs a resolved image index and outline is already
            // emitted by the regular border path; both remain data-only here.
            E::DrawBorderImage { .. } | E::DrawOutline { .. } => continue,
        };
        list.push(command);
    }
}

/// Paints one box (and its descendants) into a fresh display list.
///
/// Used by [`crate::effects`] to capture the content a mask, clip or blend group
/// applies to. Uses no decoded images; callers that need `<img>` pixels should build
/// the main display list with [`build_with_images`] instead.
#[doc(hidden)]
pub fn paint_box_commands(layout: &LayoutTree, index: usize, dx: f32, dy: f32) -> DisplayList {
    let mut list = DisplayList::new();
    let images = HashMap::new();
    let mut image_indices = HashMap::new();
    paint_box_content(
        layout,
        index,
        &mut list,
        dx,
        dy,
        &images,
        &mut image_indices,
    );
    list
}

/// Key for the process-wide inline-SVG rasterization cache.
///
/// `crate::rasterize` derives the output entirely from the serialized source,
/// but the fallback for a failed rasterization is sized to the box, so the box
/// size (in whole CSS pixels, as used by the fallback) is part of the key. This
/// keeps cached results byte-identical to a fresh rasterization in every case.
type SvgCacheKey = (String, u32, u32);

/// Upper bound on cached SVGs before the cache is reset. Real pages use well
/// under a hundred distinct icons, so this only guards against unbounded growth
/// when a process browses a very large number of pages.
const SVG_CACHE_CAPACITY: usize = 4096;

/// Process-wide cache of rasterized inline SVGs.
///
/// Display lists are rebuilt on every navigation, stylesheet/image load, scroll
/// and trace replay, and one page typically contains many copies of the same
/// icon. Rasterizing each of those with the full XML parser and software
/// rasterizer dominated display-list construction (3 s on a page with 149
/// inline SVGs). The source string is deterministic, so results are safe to
/// reuse across builds and across pages.
fn svg_cache() -> &'static Mutex<HashMap<SvgCacheKey, Arc<ImageData>>> {
    static CACHE: OnceLock<Mutex<HashMap<SvgCacheKey, Arc<ImageData>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clears the SVG cache. Primarily useful for tests that want to observe cache
/// behaviour, and for callers that need to release the memory held by old pages.
#[doc(hidden)]
pub fn clear_svg_cache() {
    svg_cache().lock().unwrap().clear();
}

/// Rasterizes an inline SVG source into image data for the display list.
///
/// Results are memoized by (source, box width, box height) in a process-wide
/// cache — see [`svg_cache`]. On any failure (malformed SVG, unsupported
/// element, oversized dimensions) the box is filled with a transparent image —
/// never black — sized to the box, and that fallback is cached too.
fn svg_image_data(source: &str, width: f32, height: f32) -> ImageData {
    let w = width.max(1.0) as u32;
    let h = height.max(1.0) as u32;
    let key: SvgCacheKey = (source.to_owned(), w, h);
    if let Some(cached) = svg_cache().lock().unwrap().get(&key) {
        return (**cached).clone();
    }

    let transparent = || ImageData::new(w, h);
    let image = match crate::rasterize(source) {
        Ok(buffer) => ImageData::from_rgba(buffer.width, buffer.height, buffer.pixels)
            .unwrap_or_else(|_| transparent()),
        Err(_) => transparent(),
    };

    let shared = Arc::new(image);
    let mut cache = svg_cache().lock().unwrap();
    if cache.len() >= SVG_CACHE_CAPACITY {
        cache.clear();
    }
    cache.insert(key, Arc::clone(&shared));
    (*shared).clone()
}

/// Paints one box. Transforms, filters and opacity are applied by wrapping the
/// box's own painting (and its descendants) in effect commands.
#[allow(clippy::too_many_arguments)]
fn paint_box(
    layout: &LayoutTree,
    index: usize,
    list: &mut DisplayList,
    dx: f32,
    dy: f32,
    images: &HashMap<NodeId, ImageData>,
    image_indices: &mut HashMap<NodeId, usize>,
) {
    let Some(b) = layout.boxes.get(index) else {
        return;
    };
    let rect = b.rect.translate(dx, dy);
    let transform = transform_matrix(b, rect);
    let filters = display_filters(b.style.filter_raw());
    let opacity = b.style.opacity().clamp(0.0, 1.0);
    let needs_transform = transform.is_some() || opacity < 1.0;
    let needs_filter = !filters.is_empty();
    if !needs_transform && !needs_filter {
        paint_box_content(layout, index, list, dx, dy, images, image_indices);
        return;
    }

    // Record the box normally, then lift its commands into effect wrappers.
    // Images stay in the parent list so their indices remain valid.
    let start = list.len();
    paint_box_content(layout, index, list, dx, dy, images, image_indices);
    let mut wrapped: Vec<DisplayCommand> = list.commands.split_off(start);
    if wrapped.is_empty() {
        return;
    }
    if needs_filter {
        wrapped = vec![DisplayCommand::DrawFiltered {
            child_commands: wrapped,
            filters,
        }];
    }
    if needs_transform {
        wrapped = vec![DisplayCommand::DrawTransformed {
            child_commands: wrapped,
            matrix: transform.unwrap_or(IDENTITY_TRANSFORM),
            opacity,
        }];
    }
    // These wrappers replace the commands just removed, so the command cap is
    // never exceeded by adding them directly.
    list.commands.extend(wrapped);
}

#[allow(clippy::too_many_arguments)]
fn paint_box_content(
    layout: &LayoutTree,
    index: usize,
    list: &mut DisplayList,
    dx: f32,
    dy: f32,
    images: &HashMap<NodeId, ImageData>,
    image_indices: &mut HashMap<NodeId, usize>,
) {
    let Some(b) = layout.boxes.get(index) else {
        return;
    };
    let rect = b.rect.translate(dx, dy);

    if b.box_type != BoxType::Text && !rect.is_empty() {
        // Resolve per-corner radii (percentages are relative to the smaller
        // side of the box, as in CSS) before choosing a surface command.
        let corners = b.style.border_radius_corners().corners;
        let base = rect.width.min(rect.height);
        let resolve_radius = |dim: browser_style::Dim| match dim {
            browser_style::Dim::Px(value) => value.max(0.0),
            browser_style::Dim::Percent(percent) => (base * percent / 100.0).max(0.0),
            browser_style::Dim::Calc { percent, px } => (base * percent / 100.0 + px).max(0.0),
            browser_style::Dim::Auto => 0.0,
        };
        let radius = [
            resolve_radius(corners[0].horizontal),
            resolve_radius(corners[1].horizontal),
            resolve_radius(corners[2].horizontal),
            resolve_radius(corners[3].horizontal),
        ];
        let has_radius = radius.iter().any(|&value| value > 0.05);
        let mut surface_handles_border = false;

        // Shadows go underneath the surface.
        for shadow in b.style.box_shadow_raw() {
            if shadow.inset {
                continue;
            }
            let spread = shadow.spread;
            let shadow_rect = Rect::new(
                rect.x - spread,
                rect.y - spread,
                (rect.width + 2.0 * spread).max(0.0),
                (rect.height + 2.0 * spread).max(0.0),
            );
            if let Some(command) = crate::commands::shadow_command(
                shadow_rect,
                radius,
                (shadow.offset_x, shadow.offset_y),
                shadow.blur,
                shadow.color,
            ) {
                list.push(command);
            }
        }

        if has_radius {
            let border_width = b.borders.top;
            let uniform_border = b.borders.left == border_width
                && b.borders.right == border_width
                && b.borders.bottom == border_width;
            let border = if uniform_border && border_width > 0.0 {
                let color = b.style.border_color(browser_style::Side::Top);
                let ring = (color.a > 0).then_some((color, border_width));
                surface_handles_border = ring.is_some();
                ring
            } else {
                None
            };
            if let Some(command) =
                crate::commands::surface_command(rect, radius, b.style.background_color(), border)
            {
                list.push(command);
            }
        } else if let Some(command) =
            crate::background::background_command(rect, b.style.background_color())
        {
            list.push(command);
        }
        // CSS paints background images above the background colour. A gradient
        // supplied through `background-image` is resolved here into the existing
        // linear-gradient primitive; other image URLs are handled elsewhere.
        if let Some(image) = b.style.background_image()
            && let Some(command) = linear_gradient_command_from_css(rect, radius, image)
        {
            list.push(command);
        }
        if b.box_type == BoxType::Image {
            // An inline `<svg>` carries its own serialized source on the layout
            // box; rasterize it instead of looking for a decoded bitmap. The
            // source is already capped when serialized; the check here is
            // defence in depth. Failures paint a *transparent* placeholder.
            let svg_source = b
                .svg_source
                .as_deref()
                .filter(|source| source.len() <= MAX_SVG_SOURCE_BYTES);
            let image_index = if let Some(source) = svg_source {
                match b.node {
                    Some(node) => *image_indices.entry(node).or_insert_with(|| {
                        list.add_image(svg_image_data(source, rect.width, rect.height))
                    }),
                    None => list.add_image(svg_image_data(source, rect.width, rect.height)),
                }
            } else {
                let placeholder = || {
                    ImageData::placeholder(rect.width.max(1.0) as u32, rect.height.max(1.0) as u32)
                };
                match b.node {
                    Some(node) => *image_indices.entry(node).or_insert_with(|| {
                        let image = images.get(&node).cloned().unwrap_or_else(placeholder);
                        list.add_image(image)
                    }),
                    None => list.add_image(placeholder()),
                }
            };
            list.push(DisplayCommand::DrawImage {
                rect,
                image: image_index,
            });
        }
        if b.box_type == BoxType::ListItem {
            let kind = b
                .style
                .get(browser_style::Property::ListStyleType)
                .and_then(|value| value.as_keyword())
                .unwrap_or("disc");
            let numbered = matches!(
                kind,
                "decimal" | "lower-alpha" | "upper-alpha" | "lower-roman" | "upper-roman"
            );
            if kind != "none" && !numbered {
                let baseline = b
                    .lines
                    .iter()
                    .find_map(|line| line.fragments.first().map(|fragment| fragment.baseline))
                    .map_or(rect.y + 12.0, |baseline| baseline + dy);
                let size = if kind == "square" { 5.0 } else { 4.0 };
                list.push(DisplayCommand::FillRect {
                    rect: Rect::new(rect.x - 14.0, baseline - 5.0, size, size),
                    color: b.style.color(),
                });
            }
        }
        if !surface_handles_border
            && let Some(command) = crate::borders::border_command(
                rect,
                b.borders,
                [
                    b.style.border_color(browser_style::Side::Top),
                    b.style.border_color(browser_style::Side::Right),
                    b.style.border_color(browser_style::Side::Bottom),
                    b.style.border_color(browser_style::Side::Left),
                ],
                [
                    b.style.border_style(browser_style::Side::Top),
                    b.style.border_style(browser_style::Side::Right),
                    b.style.border_style(browser_style::Side::Bottom),
                    b.style.border_style(browser_style::Side::Left),
                ],
            )
        {
            list.push(command);
        }
    }

    for line in &b.lines {
        for fragment in &line.fragments {
            list.push(DisplayCommand::DrawText {
                x: fragment.x + dx,
                baseline: fragment.baseline + dy,
                text: fragment.text.clone(),
                font_size: fragment.font_size,
                color: fragment.color,
                underline: fragment.underline,
                line_through: fragment.line_through,
                bold: fragment.bold,
                italic: fragment.italic,
            });
        }
    }

    let pushed_clip = if let Some(clip) = b.clip {
        list.push(DisplayCommand::PushClip(clip.translate(dx, dy)));
        true
    } else {
        false
    };

    let (scroll_x, scroll_y) = b.scroll_offset;
    let mut children: Vec<usize> = b.children.clone();
    children.sort_by_key(|c| layout.boxes.get(*c).map_or(0, |b| b.z_index));
    for child in children {
        paint_box(
            layout,
            child,
            list,
            dx - scroll_x,
            dy - scroll_y,
            images,
            image_indices,
        );
    }

    if pushed_clip {
        list.push(DisplayCommand::PopClip);
    }
}

/// Renders a laid-out page to an image buffer.
pub fn render(layout: &LayoutTree, viewport: Size) -> ImageBuffer {
    let list = build(layout);
    Rasterizer::new(viewport, browser_css::Color::rgb(255, 255, 255)).execute(&list)
}

/// Renders a laid-out page with an explicit background colour.
pub fn render_with_background(
    layout: &LayoutTree,
    viewport: Size,
    background: browser_css::Color,
) -> ImageBuffer {
    let list = build(layout);
    Rasterizer::new(viewport, background).execute(&list)
}

/// The bounding box of all painted content (useful for screenshot tools).
pub fn content_bounds(layout: &LayoutTree) -> Rect {
    layout
        .box_fragments()
        .iter()
        .map(|f| f.rect)
        .fold(Rect::ZERO, |acc, rect| acc.union(&rect))
}

/// Builds an anti-aliased rounded-surface command, or `None` when nothing
/// would be painted.
///
/// `radius` lists the four corner radii in the order top-left, top-right,
/// bottom-right, bottom-left. `border` is an optional `(colour, width)` ring
/// drawn just inside the surface edge. Both the fill and the border use 4x4
/// supersampled coverage, so corners and edges fade smoothly.
pub fn surface_command(
    rect: Rect,
    radius: [f32; 4],
    background: Color,
    border: Option<(Color, f32)>,
) -> Option<DisplayCommand> {
    if rect.is_empty() {
        return None;
    }
    let paints_background = background.a > 0;
    let paints_border = border.is_some_and(|(color, width)| color.a > 0 && width > 0.0);
    if !paints_background && !paints_border {
        return None;
    }
    Some(DisplayCommand::DrawRoundedRect {
        rect,
        radius,
        background,
        border,
    })
}

/// Builds a soft-shadow command, or `None` when the shadow is invisible.
pub fn shadow_command(
    rect: Rect,
    radius: [f32; 4],
    offset: (f32, f32),
    blur: f32,
    color: Color,
) -> Option<DisplayCommand> {
    if rect.is_empty() || color.a == 0 {
        return None;
    }
    Some(DisplayCommand::DrawShadow {
        rect,
        radius,
        offset,
        blur,
        color,
    })
}

/// Builds a linear-gradient command, or `None` when the gradient is empty.
pub fn linear_gradient_command(
    rect: Rect,
    stops: Vec<(f32, Color)>,
    angle_deg: f32,
    radius: [f32; 4],
) -> Option<DisplayCommand> {
    if rect.is_empty() || stops.is_empty() {
        return None;
    }
    Some(DisplayCommand::DrawLinearGradient {
        rect,
        stops,
        angle_deg,
        radius,
    })
}

/// The identity affine transform `[a, b, c, d, e, f]`.
const IDENTITY_TRANSFORM: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Composes two affine matrices; the result applies `a` after `b`.
fn compose(a: [f32; 6], b: [f32; 6]) -> [f32; 6] {
    let [a0, a1, a2, a3, a4, a5] = a;
    let [b0, b1, b2, b3, b4, b5] = b;
    [
        a0 * b0 + a2 * b1,
        a1 * b0 + a3 * b1,
        a0 * b2 + a2 * b3,
        a1 * b2 + a3 * b3,
        a0 * b4 + a2 * b5 + a4,
        a1 * b4 + a3 * b5 + a5,
    ]
}

fn translation_matrix(x: f32, y: f32) -> [f32; 6] {
    [1.0, 0.0, 0.0, 1.0, x, y]
}

fn dim_px(dim: browser_style::Dim, base: f32) -> f32 {
    dim.resolve(base).unwrap_or(0.0)
}

/// Resolves a box's CSS `transform` into an affine matrix around its centre.
fn transform_matrix(box_: &LayoutBox, rect: Rect) -> Option<[f32; 6]> {
    let functions = box_.style.transform_raw();
    if functions.is_empty() {
        return None;
    }
    let mut matrix = IDENTITY_TRANSFORM;
    for function in functions {
        let part = match *function {
            TransformFunction::Translate(x, y) => {
                translation_matrix(dim_px(x, rect.width), dim_px(y, rect.height))
            }
            TransformFunction::TranslateX(x) => translation_matrix(dim_px(x, rect.width), 0.0),
            TransformFunction::TranslateY(y) => translation_matrix(0.0, dim_px(y, rect.height)),
            TransformFunction::Scale(x, y) => [x, 0.0, 0.0, y, 0.0, 0.0],
            TransformFunction::ScaleX(x) => [x, 0.0, 0.0, 1.0, 0.0, 0.0],
            TransformFunction::ScaleY(y) => [1.0, 0.0, 0.0, y, 0.0, 0.0],
            TransformFunction::Rotate(degrees) => {
                let (sine, cosine) = degrees.to_radians().sin_cos();
                [cosine, sine, -sine, cosine, 0.0, 0.0]
            }
            TransformFunction::Skew(x, y) => [
                1.0,
                y.to_radians().tan(),
                x.to_radians().tan(),
                1.0,
                0.0,
                0.0,
            ],
            TransformFunction::Matrix(matrix) => matrix,
            TransformFunction::Raw(_) => continue,
        };
        matrix = compose(matrix, part);
    }
    // CSS transforms act around the transform origin (box centre by default).
    let origin_x = rect.x + rect.width * 0.5;
    let origin_y = rect.y + rect.height * 0.5;
    Some(compose(
        compose(translation_matrix(origin_x, origin_y), matrix),
        translation_matrix(-origin_x, -origin_y),
    ))
}

/// Converts typed CSS filters into their display-list representation.
fn display_filters(raw: &[FilterFunction]) -> Vec<DisplayFilter> {
    raw.iter()
        .filter_map(|filter| match *filter {
            FilterFunction::Blur(value) => Some(DisplayFilter::Blur(value)),
            FilterFunction::Brightness(value) => Some(DisplayFilter::Brightness(value)),
            FilterFunction::Contrast(value) => Some(DisplayFilter::Contrast(value)),
            FilterFunction::Grayscale(value) => Some(DisplayFilter::Grayscale(value)),
            FilterFunction::HueRotate(value) => Some(DisplayFilter::HueRotate(value)),
            FilterFunction::Invert(value) => Some(DisplayFilter::Invert(value)),
            FilterFunction::Opacity(value) => Some(DisplayFilter::Opacity(value)),
            FilterFunction::Saturate(value) => Some(DisplayFilter::Saturate(value)),
            FilterFunction::Sepia(value) => Some(DisplayFilter::Sepia(value)),
            FilterFunction::Raw(_) => None,
        })
        .collect()
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

/// Parses a CSS gradient angle, including `to <side>` keywords.
///
/// Kept crate-visible so the conic-gradient parser in [`crate::effects`] can share
/// the same angle semantics as linear gradients.
pub(crate) fn parse_angle(input: &str) -> Option<f32> {
    let lower = input.trim().to_ascii_lowercase();
    if let Some(direction) = lower.strip_prefix("to ") {
        let words: Vec<&str> = direction.split_whitespace().collect();
        let angle = match words.as_slice() {
            ["top"] => 0.0,
            ["right"] => 90.0,
            ["bottom"] => 180.0,
            ["left"] => 270.0,
            ["top", "right"] | ["right", "top"] => 45.0,
            ["bottom", "right"] | ["right", "bottom"] => 135.0,
            ["bottom", "left"] | ["left", "bottom"] => 225.0,
            ["top", "left"] | ["left", "top"] => 315.0,
            _ => return None,
        };
        return Some(angle);
    }
    for (suffix, factor) in [
        ("deg", 1.0_f32),
        ("turn", 360.0),
        ("grad", 0.9),
        ("rad", 180.0 / std::f32::consts::PI),
    ] {
        if let Some(number) = lower.strip_suffix(suffix) {
            let value: f32 = number.trim().parse().ok()?;
            return Some((value * factor).rem_euclid(360.0));
        }
    }
    None
}

/// Splits a colour-stop token into its colour text and optional position.
fn split_stop(token: &str) -> (&str, Option<f32>) {
    let trimmed = token.trim();
    if trimmed.ends_with('%')
        && let Some(index) = trimmed.rfind(char::is_whitespace)
    {
        let color = trimmed[..index].trim();
        let position = trimmed[index..]
            .trim()
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .map(|value| (value / 100.0).clamp(0.0, 1.0));
        if let Some(position) = position {
            return (color, Some(position));
        }
    }
    (trimmed, None)
}

/// Parses a CSS `linear-gradient(...)` string into its angle and colour stops.
///
/// Accepts hex, `rgb()`/`rgba()` and named colours, with optional percentage
/// stop positions. Stops without positions are spaced evenly. Returns `None`
/// for anything that is not a well-formed two-stop-or-more linear gradient.
pub fn parse_linear_gradient(input: &str) -> Option<(f32, Vec<(f32, Color)>)> {
    let trimmed = input.trim();
    if !trimmed.to_ascii_lowercase().starts_with("linear-gradient(") {
        return None;
    }
    let arguments_start = "linear-gradient(".len();
    let bytes = trimmed.as_bytes();
    let mut depth = 1_i32;
    let mut end = None;
    for (index, &byte) in bytes.iter().enumerate().skip(arguments_start) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = &trimmed[arguments_start..end?];
    let mut parts = split_top_level(inner);
    let mut angle_deg = 180.0_f32;
    if let Some(first) = parts.first()
        && let Some(angle) = parse_angle(first.trim())
    {
        angle_deg = angle;
        parts.remove(0);
    }

    let mut stops: Vec<(Option<f32>, Color)> = Vec::new();
    for part in &parts {
        let (color_text, position) = split_stop(part);
        let color = Value::parse(color_text).as_color()?;
        stops.push((position, color));
    }
    if stops.len() < 2 {
        return None;
    }
    let count = stops.len();
    let resolved = stops
        .into_iter()
        .enumerate()
        .map(|(index, (position, color))| {
            let position = position.unwrap_or_else(|| {
                if count == 1 {
                    0.0
                } else {
                    index as f32 / (count - 1) as f32
                }
            });
            (position, color)
        })
        .collect();
    Some((angle_deg, resolved))
}

/// Builds a linear-gradient command from a CSS `background-image` string.
pub fn linear_gradient_command_from_css(
    rect: Rect,
    radius: [f32; 4],
    css: &str,
) -> Option<DisplayCommand> {
    let (angle_deg, stops) = parse_linear_gradient(css)?;
    linear_gradient_command(rect, stops, angle_deg, radius)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_css::Origin;
    use browser_dom::Document;
    use browser_layout::layout;
    use browser_style::{StyleEngine, Viewport};

    fn page(css: &str, body: &str) -> LayoutTree {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let head = doc.create_element("head");
        let body_node = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, head);
        doc.append_child(html, body_node);
        doc.append_text(body_node, body);
        let mut engine = StyleEngine::new(Viewport {
            width: 200.0,
            height: 100.0,
        });
        engine.add_css(css, Origin::Author);
        let styled = engine.compute(&doc);
        layout(
            &doc,
            &styled,
            Size {
                width: 200.0,
                height: 100.0,
            },
        )
    }

    #[test]
    fn paints_background_and_text() {
        let tree = page(
            "body { margin: 0; background-color: #00ff00; color: #000000; }",
            "Hello",
        );
        let list = build(&tree);
        assert!(list.fill_count() >= 1);
        assert!(list.text_count() >= 1);
        let image = render(
            &tree,
            Size {
                width: 200.0,
                height: 100.0,
            },
        );
        assert_eq!(image.pixel(150, 5), [0, 255, 0, 255]);
        assert!(image.coverage_vs(browser_css::Color::rgb(0, 255, 0)) > 0.001);
    }

    #[test]
    fn paints_borders() {
        let tree = page(
            "body { margin: 0; border: 2px solid red; width: 50px; height: 20px; }",
            "x",
        );
        let list = build(&tree);
        assert!(
            list.commands
                .iter()
                .any(|c| matches!(c, DisplayCommand::DrawBorder { .. }))
        );
    }

    #[test]
    fn scroll_containers_emit_clips() {
        let tree = page(
            "body { margin: 0; height: 20px; overflow: scroll; }",
            "content that is taller than the box",
        );
        let list = build(&tree);
        assert!(list.push_clip_count() > 0);
        assert!(
            tree.boxes
                .iter()
                .any(|b| b.style.overflow_y() != browser_style::Overflow::Visible)
        );
    }

    #[test]
    fn surface_command_builds_rounded_surface() {
        let command = surface_command(
            Rect::new(1.0, 2.0, 30.0, 20.0),
            [4.0, 5.0, 6.0, 7.0],
            Color::rgb(9, 8, 7),
            Some((Color::rgb(1, 2, 3), 2.0)),
        );
        assert!(matches!(
            command,
            Some(DisplayCommand::DrawRoundedRect { .. })
        ));
    }

    #[test]
    fn surface_command_skips_invisible_surfaces() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(surface_command(Rect::ZERO, [0.0; 4], Color::WHITE, None).is_none());
        assert!(surface_command(rect, [0.0; 4], Color::transparent(), None).is_none());
        assert!(
            surface_command(
                rect,
                [0.0; 4],
                Color::transparent(),
                Some((Color::BLACK, 0.0))
            )
            .is_none()
        );
        assert!(
            surface_command(
                rect,
                [0.0; 4],
                Color::transparent(),
                Some((Color::BLACK, 1.0))
            )
            .is_some()
        );
    }

    #[test]
    fn shadow_and_gradient_commands() {
        assert!(
            shadow_command(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                [2.0; 4],
                (1.0, 1.0),
                4.0,
                Color::BLACK,
            )
            .is_some()
        );
        assert!(
            linear_gradient_command(
                Rect::new(0.0, 0.0, 10.0, 10.0),
                vec![(0.0, Color::WHITE), (1.0, Color::BLACK)],
                180.0,
                [2.0; 4],
            )
            .is_some()
        );
    }

    #[test]
    fn parses_linear_gradient_from_css() {
        let (angle, stops) =
            parse_linear_gradient("linear-gradient(90deg, #ff0000 0%, rgb(0, 0, 255) 100%)")
                .expect("valid gradient");
        assert_eq!(angle, 90.0);
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0], (0.0, Color::rgb(255, 0, 0)));
        assert_eq!(stops[1], (1.0, Color::rgb(0, 0, 255)));
    }

    #[test]
    fn background_image_gradient_emits_command() {
        let mut tree = page("body { margin: 0; width: 40px; height: 40px; }", "");
        // The style engine does not yet retain gradient declarations, so set the
        // computed string directly to exercise the paint-side detection.
        for b in &mut tree.boxes {
            if b.node.is_some() {
                b.style.set(
                    browser_style::Property::BackgroundImage,
                    browser_style::ComputedValue::Str(
                        "linear-gradient(to right, red, blue)".to_string(),
                    ),
                );
                // Keep this paint-side test independent of how layout resolves
                // an explicit CSS height (which has changed across engines).
                b.rect = Rect::new(0.0, 0.0, 40.0, 40.0);
            }
        }
        let list = build(&tree);
        assert!(
            list.commands
                .iter()
                .any(|command| matches!(command, DisplayCommand::DrawLinearGradient { .. })),
            "expected a DrawLinearGradient command"
        );
    }

    #[test]
    fn radial_and_conic_background_emit_effect_commands() {
        let mut tree = page("body { margin: 0; width: 40px; height: 40px; }", "");
        for b in &mut tree.boxes {
            if b.node.is_some() {
                b.style.set(
                    browser_style::Property::BackgroundImage,
                    browser_style::ComputedValue::Str(
                        "radial-gradient(circle, red, blue)".to_string(),
                    ),
                );
            }
        }
        let effects = crate::effects::build_effects(&tree);
        assert!(
            effects.commands.iter().any(|command| matches!(
                command,
                crate::effects::ExtendedCommand::DrawRadialGradient { .. }
            )),
            "expected a DrawRadialGradient effect command"
        );

        for b in &mut tree.boxes {
            if b.node.is_some() {
                b.style.set(
                    browser_style::Property::BackgroundImage,
                    browser_style::ComputedValue::Str(
                        "conic-gradient(from 45deg, red, blue)".to_string(),
                    ),
                );
            }
        }
        let effects = crate::effects::build_effects(&tree);
        assert!(
            effects.commands.iter().any(|command| matches!(
                command,
                crate::effects::ExtendedCommand::DrawConicGradient { .. }
            )),
            "expected a DrawConicGradient effect command"
        );
    }

    #[test]
    fn raw_effects_emit_mask_clip_and_blend_commands() {
        let tree = page("body { margin: 0; width: 40px; height: 40px; }", "x");
        let node = tree
            .boxes
            .iter()
            .find_map(|b| b.node)
            .expect("body box has a node");
        let mut raw = HashMap::new();
        raw.insert(
            node,
            crate::effects::RawEffects {
                mask_image: Some("linear-gradient(to right, white, transparent)".to_string()),
                clip_path: Some("inset(10px)".to_string()),
                mix_blend_mode: Some("multiply".to_string()),
                backdrop_filter: Some("blur(4px)".to_string()),
                border_image: Some("url(b.png) 30 / 10px".to_string()),
                outline: Some("2px solid red".to_string()),
                outline_offset: Some("3px".to_string()),
                image_rendering: None,
                webkit_mask_image: None,
                mask_luminance: false,
            },
        );
        let (list, effects) = build_with_effects(&tree, &HashMap::new(), &raw);
        assert!(!list.commands.is_empty());
        assert!(
            effects.commands.iter().any(|command| matches!(
                command,
                crate::effects::ExtendedCommand::DrawMasked { .. }
            )),
            "expected a mask command"
        );
        assert!(
            effects.commands.iter().any(|command| matches!(
                command,
                crate::effects::ExtendedCommand::DrawClipped { .. }
            )),
            "expected a clip-path command"
        );
        assert!(
            effects.commands.iter().any(|command| matches!(
                command,
                crate::effects::ExtendedCommand::DrawBlended { .. }
            )),
            "expected a blend command"
        );
        assert!(
            effects.commands.iter().any(|command| matches!(
                command,
                crate::effects::ExtendedCommand::DrawBorderImage { .. }
            )),
            "expected a border-image command"
        );
        assert!(
            effects.commands.iter().any(|command| matches!(
                command,
                crate::effects::ExtendedCommand::DrawOutline { .. }
            )),
            "expected an outline command"
        );
    }

    fn find_effects(commands: &[DisplayCommand]) -> (bool, bool) {
        let mut found = (false, false);
        for command in commands {
            match command {
                DisplayCommand::DrawTransformed { child_commands, .. } => {
                    found.0 = true;
                    let nested = find_effects(child_commands);
                    found.1 |= nested.1;
                }
                DisplayCommand::DrawFiltered { child_commands, .. } => {
                    found.1 = true;
                    let nested = find_effects(child_commands);
                    found.0 |= nested.0;
                }
                _ => {}
            }
        }
        found
    }

    #[test]
    fn transform_and_filter_css_emit_effect_commands() {
        let tree = page(
            "body { margin: 0; transform: translate(10px, 5px); filter: brightness(0.5); }",
            "x",
        );
        let list = build(&tree);
        let (has_transform, has_filter) = find_effects(&list.commands);
        assert!(has_transform, "expected a DrawTransformed command");
        assert!(has_filter, "expected a nested DrawFiltered command");
    }

    fn svg_page() -> LayoutTree {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let head = doc.create_element("head");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, head);
        doc.append_child(html, body);
        let svg = doc.create_element("svg");
        doc.set_attr(svg, "width", "40");
        doc.set_attr(svg, "height", "20");
        doc.append_child(body, svg);
        let rect = doc.create_element("rect");
        doc.set_attr(rect, "width", "40");
        doc.set_attr(rect, "height", "20");
        doc.set_attr(rect, "fill", "red");
        doc.append_child(svg, rect);
        let engine = StyleEngine::new(Viewport {
            width: 200.0,
            height: 100.0,
        });
        let styled = engine.compute(&doc);
        layout(
            &doc,
            &styled,
            Size {
                width: 200.0,
                height: 100.0,
            },
        )
    }

    #[test]
    fn inline_svg_paints_rasterized_pixels() {
        let tree = svg_page();
        let list = build(&tree);
        assert!(
            list.commands
                .iter()
                .any(|c| matches!(c, DisplayCommand::DrawImage { .. })),
            "expected a DrawImage command for the inline SVG"
        );
        assert_eq!(list.images.len(), 1);
        let image = &list.images[0];
        assert_eq!((image.width, image.height), (40, 20));
        assert!(
            image
                .pixels
                .chunks(4)
                .any(|p| p[0] > 200 && p[1] < 60 && p[2] < 60 && p[3] > 200),
            "the rasterized SVG should contain opaque red pixels"
        );

        let rendered = render(
            &tree,
            Size {
                width: 200.0,
                height: 100.0,
            },
        );
        assert!(
            rendered
                .pixels
                .chunks(4)
                .any(|p| p[0] > 200 && p[1] < 60 && p[2] < 60),
            "the red SVG rectangle should appear in the rendered page"
        );
    }

    #[test]
    fn inline_svg_failure_paints_transparent_not_black() {
        let mut tree = svg_page();
        for b in &mut tree.boxes {
            if let Some(source) = &mut b.svg_source {
                *source = "<svg width=\"40\" height=\"20\"><foo/></svg>".to_string();
            }
        }
        let list = build(&tree);
        assert_eq!(list.images.len(), 1);
        assert!(
            list.images[0].pixels.chunks(4).all(|p| p[3] == 0),
            "a failed SVG must fall back to a transparent (not black) image"
        );
    }

    #[test]
    fn svg_cache_returns_byte_identical_images() {
        let source = "<svg width='8' height='6'><rect width='8' height='6' fill='red'/></svg>";
        clear_svg_cache();
        let cold = svg_image_data(source, 8.0, 6.0);
        let cached = svg_image_data(source, 8.0, 6.0);
        assert_eq!(cold, cached, "a cache hit must be byte-identical");

        clear_svg_cache();
        let after_clear = svg_image_data(source, 8.0, 6.0);
        assert_eq!(
            cold, after_clear,
            "a fresh rasterization must be byte-identical"
        );
    }

    #[test]
    fn svg_cache_keys_on_source_and_size() {
        clear_svg_cache();
        let red = svg_image_data(
            "<svg width='8' height='6'><rect width='8' height='6' fill='red'/></svg>",
            8.0,
            6.0,
        );
        let blue = svg_image_data(
            "<svg width='8' height='6'><rect width='8' height='6' fill='blue'/></svg>",
            8.0,
            6.0,
        );
        let larger = svg_image_data(
            "<svg width='9' height='6'><rect width='9' height='6' fill='red'/></svg>",
            9.0,
            6.0,
        );
        assert_ne!(red, blue, "different sources must not share a cache entry");
        assert_ne!(red, larger, "different sizes must not share a cache entry");
    }

    #[test]
    fn svg_cache_keeps_display_list_identical() {
        let tree = svg_page();
        clear_svg_cache();
        let cold = build(&tree).identity_hash();
        let warm = build(&tree).identity_hash();
        clear_svg_cache();
        let cold_again = build(&tree).identity_hash();
        assert_eq!(
            cold, warm,
            "a warm SVG cache must not change the display list"
        );
        assert_eq!(
            cold, cold_again,
            "rebuilding after a clear must be identical"
        );
    }
}
