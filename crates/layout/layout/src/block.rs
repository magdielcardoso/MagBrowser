//! Block and inline formatting: the core layout algorithm.
//!
//! Floats are supported with a simplified CSS 2.1 model: floats are placed in
//! the current block formatting context, consecutive floats sit side by side,
//! line boxes and following block boxes are shortened by the float band, and
//! `clear` moves a box below the relevant floats. One deliberate
//! simplification: a block container's auto height grows to cover its floats
//! (up to the lowest float bottom) instead of ignoring them as CSS 2.1 does.
//! This keeps floats inside scroll containers without extra machinery.

use browser_dom::{Document, NodeId};
use browser_style::{
    BorderStyle, BoxSizing, ComputedStyle, Dim, Display, Float, Overflow, Property, Side,
    TextDecoration,
};

use crate::box_model::{Edges, Rect, collapse_margins};
use crate::flex;
use crate::grid;
use crate::inline::{FloatBand, InlineItem, LineBreaker, collapse_whitespace, whitespace_mode};
use crate::position::translate_subtree;
use crate::tree::{BoxType, LayoutBox, LayoutTree};

/// Width/height of a classical scrollbar gutter, in CSS pixels. Matching
/// Chrome's scrollbars matters for pixel fidelity: the gutter is subtracted
/// from the content box before children are laid out.
pub const SCROLLBAR_SIZE: f32 = 15.0;

/// Side a floated box is placed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatSide {
    /// `float: left`
    Left,
    /// `float: right`
    Right,
}

/// Which floats `clear` should avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearSide {
    /// `clear: none`
    None,
    /// `clear: left`
    Left,
    /// `clear: right`
    Right,
    /// `clear: both`
    Both,
}

/// A float already placed in a block formatting context, in document
/// coordinates.
#[derive(Debug, Clone, Copy)]
pub struct PlacedFloat {
    /// Side the float was placed on.
    pub side: FloatSide,
    /// Border box of the float.
    pub rect: Rect,
}

/// Float bookkeeping for one block formatting context.
#[derive(Debug, Clone, Default)]
pub struct FloatContext {
    floats: Vec<PlacedFloat>,
}

impl FloatContext {
    /// Creates an empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when no float has been placed yet.
    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    /// The lowest float bottom, or negative infinity when there is none.
    pub fn max_bottom(&self) -> f32 {
        self.floats
            .iter()
            .map(|f| f.rect.bottom())
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// Floats placed so far.
    pub fn floats(&self) -> &[PlacedFloat] {
        &self.floats
    }

    fn span_at(&self, y: f32, height: f32, left: f32, right: f32) -> (f32, f32) {
        let mut span_left = left;
        let mut span_right = right;
        for f in &self.floats {
            if f.rect.bottom() <= y || f.rect.y >= y + height {
                continue;
            }
            match f.side {
                FloatSide::Left => span_left = span_left.max(f.rect.right().min(right)),
                FloatSide::Right => span_right = span_right.min(f.rect.x.max(left)),
            }
        }
        (span_left, (span_right - span_left).max(0.0))
    }

    /// The available horizontal span for a band starting at `y`, as an inset
    /// from `content.x` plus a width.
    pub fn available(&self, y: f32, height: f32, content: Rect) -> (f32, f32) {
        let (left, width) = self.span_at(y, height.max(1.0), content.x, content.right());
        ((left - content.x).max(0.0), width)
    }

    /// Moves `y` below the floats selected by `clear`.
    pub fn clearance(&self, y: f32, clear: ClearSide) -> f32 {
        let mut bottom = y;
        for f in &self.floats {
            let applies = match clear {
                ClearSide::None => false,
                ClearSide::Both => true,
                ClearSide::Left => f.side == FloatSide::Left,
                ClearSide::Right => f.side == FloatSide::Right,
            };
            if applies {
                bottom = bottom.max(f.rect.bottom());
            }
        }
        bottom
    }

    /// Places a float of `width` × `height`, starting at `start_y`, returning
    /// the chosen border-box origin.
    pub fn place(
        &mut self,
        side: FloatSide,
        width: f32,
        height: f32,
        start_y: f32,
        content: Rect,
    ) -> (f32, f32) {
        let content_width = content.width.max(0.0);
        let width = width.min(content_width).max(0.0);
        let height = height.max(0.0);
        let mut y = start_y;
        let mut placed = false;
        let mut x = content.x;

        // At most one vertical step per already-placed float is needed.
        for _ in 0..=self.floats.len() {
            let (left, span) = self.span_at(y, height.max(1.0), content.x, content.right());
            if span + 0.01 >= width {
                x = match side {
                    FloatSide::Left => left,
                    FloatSide::Right => left + span - width,
                };
                placed = true;
                break;
            }
            let next = self
                .floats
                .iter()
                .filter(|f| f.rect.bottom() > y)
                .map(|f| f.rect.bottom())
                .fold(f32::INFINITY, f32::min);
            if !next.is_finite() {
                break;
            }
            y = next;
        }

        if !placed {
            // Wider than the available band: place at the content edge and let
            // it overflow, like an unbreakable line would.
            x = match side {
                FloatSide::Left => content.x,
                FloatSide::Right => (content.right() - width).max(content.x),
            };
        }
        self.floats.push(PlacedFloat {
            side,
            rect: Rect::new(x, y, width, height),
        });
        (x, y)
    }

    /// Bands for the inline line breaker. X coordinates are relative to
    /// `content`, Y coordinates stay absolute.
    pub fn bands(&self, content: Rect) -> Vec<FloatBand> {
        self.floats
            .iter()
            .map(|f| FloatBand {
                left: f.side == FloatSide::Left,
                rect: Rect::new(f.rect.x - content.x, f.rect.y, f.rect.width, f.rect.height),
            })
            .collect()
    }
}

/// Resolves the content height from a declared height, falling back to the
/// measured content height for indefinite values.
fn resolve_content_height(
    height_dim: Dim,
    border_box: bool,
    vertical_extra: f32,
    auto: f32,
) -> f32 {
    match height_dim {
        Dim::Auto => auto,
        Dim::Px(px) => {
            if border_box {
                (px - vertical_extra).max(0.0)
            } else {
                px.max(0.0)
            }
        }
        // A percentage height has no definite base in this top-down layout;
        // resolving it against the width would create a sizing feedback loop.
        Dim::Percent(_) => auto,
        Dim::Calc { percent, .. } if percent != 0.0 => auto,
        Dim::Calc { px, .. } => px.max(0.0),
    }
}

/// Lays out the children of `index`, returning the auto content height.
///
/// Factored out so a scroll container can be laid out twice: once at full width
/// to measure overflow, then once more with a scrollbar gutter reserved.
fn layout_children(
    tree: &mut LayoutTree,
    doc: &Document,
    index: usize,
    floats: &mut FloatContext,
    content_y: f32,
) -> f32 {
    let box_type = tree.boxes[index].box_type;
    let children = tree.boxes[index].children.clone();
    if box_type == BoxType::Table {
        crate::table::layout_table(tree, doc, index)
    } else if box_type == BoxType::Flex {
        flex::layout_flex(tree, doc, index)
    } else if box_type == BoxType::Grid {
        grid::layout_grid(tree, doc, index)
    } else if children
        .iter()
        .any(|c| tree.boxes[*c].box_type.is_block_level())
    {
        let normal = layout_block_children(tree, doc, index, floats);
        if floats.is_empty() {
            normal
        } else {
            normal.max(floats.max_bottom() - content_y)
        }
    } else {
        layout_inline_children(tree, doc, index, floats)
    }
}

/// Intrinsic width of a box's *content* (excluding its own insets), used to
/// detect horizontal overflow for `overflow-x: auto`.
fn content_intrinsic_width(tree: &LayoutTree, index: usize, available: f32) -> f32 {
    let box_ref = &tree.boxes[index];
    let child_widths: Vec<f32> = box_ref
        .children
        .iter()
        .map(|&child| max_content_width(tree, child, available))
        .collect();
    let has_inline = box_ref.children.iter().any(|&child| {
        matches!(
            tree.boxes[child].box_type,
            BoxType::Text | BoxType::Inline | BoxType::InlineBlock | BoxType::Image
        )
    });
    if has_inline {
        child_widths.iter().sum()
    } else {
        child_widths.iter().copied().fold(0.0, f32::max)
    }
}

/// True when `scrollbar-width: none` suppresses scrollbars (and therefore the
/// gutter) for this box. The style crate does not know the property, so the
/// authored inline `style` attribute is checked as well.
fn scrollbar_suppressed(style: &ComputedStyle, doc: &Document, node: Option<NodeId>) -> bool {
    if let Some(property) = Property::from_name("scrollbar-width")
        && let Some(keyword) = style.get(property).and_then(|v| v.as_keyword())
        && keyword == "none"
    {
        return true;
    }
    if let Some(node) = node
        && let Some(css) = doc.attr(node, "style")
        && css.split(';').any(|declaration| {
            let mut pair = declaration.splitn(2, ':');
            let is_width = pair
                .next()
                .is_some_and(|name| name.trim().eq_ignore_ascii_case("scrollbar-width"));
            is_width
                && pair
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("none"))
        })
    {
        return true;
    }
    false
}

/// Lays out the whole tree inside a viewport of `viewport_width` × `viewport_height`.
pub fn layout_document(
    tree: &mut LayoutTree,
    doc: &Document,
    viewport_width: f32,
    viewport_height: f32,
) -> f32 {
    let _ = viewport_height;
    let Some(root) = tree.root else { return 0.0 };
    let bottom = layout_box(tree, doc, root, 0.0, 0.0, viewport_width);
    tree.document_height = bottom.max(tree.content_height());
    tree.document_height
}

/// Lays out one box at the given containing block left edge and border-box top.
///
/// Returns the border-box bottom edge.
pub fn layout_box(
    tree: &mut LayoutTree,
    doc: &Document,
    index: usize,
    cb_x: f32,
    border_y: f32,
    cb_width: f32,
) -> f32 {
    let mut floats = FloatContext::new();
    layout_box_with(tree, doc, index, cb_x, border_y, cb_width, &mut floats)
}

/// Lays out one box inside an existing float context and returns the
/// border-box bottom edge.
pub fn layout_box_with(
    tree: &mut LayoutTree,
    doc: &Document,
    index: usize,
    cb_x: f32,
    border_y: f32,
    cb_width: f32,
    inherited_floats: &mut FloatContext,
) -> f32 {
    let Some(container) = tree.boxes.get(index) else {
        return border_y;
    };
    let style = container.style.clone();
    let box_type = container.box_type;

    if box_type == BoxType::LineBreak {
        tree.boxes[index].rect = Rect::new(cb_x, border_y, 0.0, 0.0);
        return border_y;
    }

    let border_box = style.box_sizing() == BoxSizing::BorderBox;
    let margins = Edges {
        top: resolve(style.margin(Side::Top), cb_width),
        right: resolve(style.margin(Side::Right), cb_width),
        bottom: resolve(style.margin(Side::Bottom), cb_width),
        left: resolve(style.margin(Side::Left), cb_width),
    };
    let borders = Edges {
        top: style.border_width(Side::Top),
        right: style.border_width(Side::Right),
        bottom: style.border_width(Side::Bottom),
        left: style.border_width(Side::Left),
    };
    let paddings = Edges {
        top: resolve(style.padding(Side::Top), cb_width),
        right: resolve(style.padding(Side::Right), cb_width),
        bottom: resolve(style.padding(Side::Bottom), cb_width),
        left: resolve(style.padding(Side::Left), cb_width),
    };

    let horizontal_extra = borders.horizontal() + paddings.horizontal();
    let vertical_extra = borders.vertical() + paddings.vertical();
    let width_dim = style.width();
    let height_dim = style.height();
    let height_is_definite = !height_dim.is_auto() && !has_percentage(height_dim);
    let aspect = preferred_aspect_ratio(&style);

    let mut content_width = match width_dim {
        Dim::Auto => (cb_width - margins.horizontal() - horizontal_extra).max(0.0),
        Dim::Px(px) => {
            if border_box {
                (px - horizontal_extra).max(0.0)
            } else {
                px.max(0.0)
            }
        }
        Dim::Percent(p) => {
            let base = cb_width * p / 100.0;
            if border_box {
                (base - horizontal_extra).max(0.0)
            } else {
                base.max(0.0)
            }
        }
        Dim::Calc { percent, px } => {
            let base = cb_width * percent / 100.0 + px;
            if border_box {
                (base - horizontal_extra).max(0.0)
            } else {
                base.max(0.0)
            }
        }
    };

    content_width = clamp_dim(
        content_width,
        style.dim(Property::MinWidth),
        style.dim(Property::MaxWidth),
        cb_width,
    );

    // When the width is auto but the height is definite, derive the width
    // from the preferred aspect ratio instead of stretching to fill the
    // containing block. Min/max width constraints still apply.
    if width_dim.is_auto()
        && let Some(ratio) = aspect
        && let Some(content_h) = definite_content_dim(height_dim, border_box, vertical_extra)
    {
        content_width = if border_box {
            ((content_h + vertical_extra) * ratio - horizontal_extra).max(0.0)
        } else {
            (content_h * ratio).max(0.0)
        };
        content_width = clamp_dim(
            content_width,
            style.dim(Property::MinWidth),
            style.dim(Property::MaxWidth),
            cb_width,
        );
    }

    // A flex/grid item's intrinsic min-content measurement resolves a
    // descendant's percentage width against the *container* width. A deeply
    // nested child with, say, `width: 150%` can therefore report a min-content
    // width larger than the container, which becomes the item's `min-width:
    // auto` base size. The next level then resolves its percentages against
    // that inflated width and the layout grows geometrically. A flow box
    // never needs a content box wider than its containing block here, so cap
    // it before any descendant geometry is derived from it.
    let containing_content_width = (cb_width - margins.horizontal() - horizontal_extra).max(0.0);
    content_width = content_width.min(containing_content_width);

    let x = cb_x + margins.left;
    let mut rect = Rect::new(x, border_y, content_width + horizontal_extra, 0.0);
    let mut content = Rect::new(
        x + borders.left + paddings.left,
        border_y + borders.top + paddings.top,
        content_width,
        0.0,
    );

    {
        let b = &mut tree.boxes[index];
        b.margins = margins;
        b.borders = borders;
        b.paddings = paddings;
        b.rect = rect;
        b.content = content;
        b.scrollbar = None;
        b.horizontal_scrollbar = None;
    }

    if box_type == BoxType::Image {
        // `object-fit` is deliberately ignored: it only affects how the
        // replaced content is painted inside the box, never the box size, so
        // an image reporting it must not disturb layout or panic here.
        let mut height = if height_is_definite {
            resolve(height_dim, cb_width).max(0.0)
        } else if !width_dim.is_auto()
            && let Some(ratio) = aspect
        {
            // Indefinite height (auto or a percentage we cannot resolve): use
            // the intrinsic ratio instead of treating the percentage as a
            // fraction of the *width*, which would make every `height: 100%`
            // image a square and feed that square back into flex/grid sizing.
            (content_width / ratio).max(0.0)
        } else {
            150.0
        };
        if border_box && height_is_definite {
            height = (height - vertical_extra).max(0.0);
        }
        rect.height = height + vertical_extra;
        content.height = height;
        let b = &mut tree.boxes[index];
        b.rect = rect;
        b.content = content;
        b.scroll_height = height;
        return tree.boxes[index].rect.bottom();
    }

    // Boxes that establish a new block formatting context do not see the
    // floats of their parent context.
    let new_context = matches!(
        box_type,
        BoxType::InlineBlock | BoxType::Flex | BoxType::Grid | BoxType::Table | BoxType::TableCell
    ) || crate::inline::is_scroll_container(&style)
        || matches!(
            style.position(),
            browser_style::Position::Absolute | browser_style::Position::Fixed
        );
    let mut local_floats = FloatContext::new();
    let floats: &mut FloatContext = if new_context {
        &mut local_floats
    } else {
        inherited_floats
    };

    // First pass at the full content width. Scroll containers are re-laid out
    // below at a narrower width when a scrollbar gutter is needed.
    let mut auto_content_height = layout_children(tree, doc, index, floats, content.y);
    let mut content_height =
        resolve_content_height(height_dim, border_box, vertical_extra, auto_content_height);

    // Classical scrollbars live inside the padding box, so a vertical scrollbar
    // reduces the width available to children. Reserve a gutter only for a
    // scrollable axis (`auto`/`scroll`) whose content actually overflows, and
    // only when `scrollbar-width: none` has not suppressed it.
    let suppressed = scrollbar_suppressed(&style, doc, tree.boxes[index].node);
    let overflow_x = style.overflow_x();
    let overflow_y = style.overflow_y();
    let vertical_overflow = height_is_definite && auto_content_height > content_height + 0.5;
    let horizontal_overflow = content_intrinsic_width(tree, index, cb_width) > content_width + 0.5;
    let scrollable = |axis: Overflow| matches!(axis, Overflow::Scroll | Overflow::Auto);
    let want_v = !suppressed && scrollable(overflow_y) && vertical_overflow;
    let want_h = !suppressed && scrollable(overflow_x) && horizontal_overflow;

    let hbar = if want_h { SCROLLBAR_SIZE } else { 0.0 };
    if want_v || want_h {
        let vbar = if want_v { SCROLLBAR_SIZE } else { 0.0 };
        content_width = (content_width - vbar).max(0.0);
        let containing_content_width =
            (cb_width - margins.horizontal() - horizontal_extra).max(0.0);
        content_width = content_width.min(containing_content_width);
        rect.width = content_width + horizontal_extra;
        content = Rect::new(
            x + borders.left + paddings.left,
            border_y + borders.top + paddings.top,
            content_width,
            (content_height - hbar).max(0.0),
        );
        {
            let b = &mut tree.boxes[index];
            b.rect = rect;
            b.content = content;
        }
        // Re-flow at the reduced size. A scroll container establishes its own
        // formatting context, so a fresh float context is correct here.
        let mut gutter_floats = FloatContext::new();
        auto_content_height = layout_children(tree, doc, index, &mut gutter_floats, content.y);
        content_height =
            resolve_content_height(height_dim, border_box, vertical_extra, auto_content_height);
    }

    // Symmetrically, a definite width and an indefinite height derive the
    // height from the ratio instead of using the intrinsic/content height.
    if !height_is_definite
        && !width_dim.is_auto()
        && let Some(ratio) = aspect
    {
        content_height = if border_box {
            ((content_width + horizontal_extra) / ratio - vertical_extra).max(0.0)
        } else {
            (content_width / ratio).max(0.0)
        };
    }
    content_height = clamp_dim(
        content_height,
        style.dim(Property::MinHeight),
        style.dim(Property::MaxHeight),
        cb_width,
    );

    let scrollport_height = (content_height - hbar).max(0.0);
    let total_height = content_height + borders.vertical() + paddings.vertical();
    {
        let b = &mut tree.boxes[index];
        b.rect.width = rect.width;
        b.rect.height = total_height;
        b.content = Rect::new(content.x, content.y, content.width, scrollport_height);
        b.scroll_height = auto_content_height.max(scrollport_height);
        if crate::inline::is_scroll_container(&b.style) {
            b.clip = Some(b.rect);
        }
        if want_v {
            // `b.content.width` has already had the gutter removed, so its
            // right edge is where the vertical scrollbar begins.
            b.scrollbar = Some(Rect::new(
                b.content.right(),
                b.content.y,
                SCROLLBAR_SIZE,
                scrollport_height.max(0.0),
            ));
        }
        if want_h {
            // Likewise the horizontal bar starts at the bottom of the reduced
            // content area.
            b.horizontal_scrollbar = Some(Rect::new(
                b.content.x,
                b.content.bottom(),
                content_width.max(0.0),
                SCROLLBAR_SIZE,
            ));
        }
    }
    let bottom = tree.boxes[index].rect.bottom();

    // Remember the unshifted position of sticky boxes so scroll updates can
    // recompute their offset deterministically.
    if tree.boxes[index].style.position() == browser_style::Position::Sticky {
        tree.boxes[index].sticky_static_y = tree.boxes[index].rect.y;
    }

    // Position out-of-flow children relative to this box.
    let positioned: Vec<usize> = tree.boxes[index]
        .children
        .clone()
        .into_iter()
        .filter(|c| tree.boxes[*c].positioned)
        .collect();
    for child in positioned {
        layout_absolute(tree, doc, child, content, cb_width);
    }

    // Relative offsets shift the box and its descendants after flow.
    if tree.boxes[index].style.position() == browser_style::Position::Relative {
        let style = tree.boxes[index].style.clone();
        let dx = resolve(style.dim(Property::Left), cb_width)
            - resolve(style.dim(Property::Right), cb_width);
        let dy = resolve(style.dim(Property::Top), cb_width)
            - resolve(style.dim(Property::Bottom), cb_width);
        if dx != 0.0 || dy != 0.0 {
            translate_subtree(tree, index, dx, dy);
            if tree.boxes[index].style.position() == browser_style::Position::Sticky {
                tree.boxes[index].sticky_static_y = tree.boxes[index].rect.y;
            }
            return tree.boxes[index].rect.bottom();
        }
    }

    bottom
}

fn layout_absolute(
    tree: &mut LayoutTree,
    doc: &Document,
    index: usize,
    containing: Rect,
    cb_width: f32,
) {
    let style = tree.boxes[index].style.clone();
    let shrink_width = if style.width().is_auto() {
        shrink_to_fit(tree, index, containing.width).max(0.0)
    } else {
        containing.width
    };
    let bottom = layout_box(tree, doc, index, 0.0, 0.0, shrink_width.max(1.0));
    let height = bottom;
    let mut x = containing.x;
    let mut y = containing.y;
    let left = style.dim(Property::Left);
    let right = style.dim(Property::Right);
    let top = style.dim(Property::Top);
    let bottom_dim = style.dim(Property::Bottom);
    if !left.is_auto() {
        x += resolve(left, containing.width);
    } else if !right.is_auto() {
        x += containing.width - resolve(right, containing.width) - tree.boxes[index].rect.width;
    }
    if !top.is_auto() {
        y += resolve(top, containing.height.max(0.0));
    } else if !bottom_dim.is_auto() {
        y += containing.height
            - resolve(bottom_dim, containing.height.max(0.0))
            - height.min(tree.boxes[index].rect.height);
    }
    let current = tree.boxes[index].rect;
    translate_subtree(tree, index, x - current.x, y - current.y);
    let _ = cb_width;
}

/// Stacks block-level children with margin collapsing and float handling.
fn layout_block_children(
    tree: &mut LayoutTree,
    doc: &Document,
    index: usize,
    floats: &mut FloatContext,
) -> f32 {
    let content = tree.boxes[index].content;
    let children = tree.boxes[index].children.clone();
    let mut cursor = content.y;
    let mut content_bottom = content.y;
    let mut prev_margin_bottom = 0.0;
    let mut first = true;

    for child in children {
        if tree.boxes[child].positioned || tree.boxes[child].box_type == BoxType::LineBreak {
            continue;
        }
        let child_style = tree.boxes[child].style.clone();
        let margin_top = resolve(child_style.margin(Side::Top), content.width);
        let margin_bottom = resolve(child_style.margin(Side::Bottom), content.width);

        // Floats are out of flow: they sit at the current line top and do not
        // advance the flow cursor.
        if child_style.float() != Float::None {
            layout_float(tree, doc, child, &child_style, cursor, content, floats);
            continue;
        }

        if first {
            cursor += margin_top;
            first = false;
        } else {
            cursor += collapse_margins(prev_margin_bottom, margin_top);
        }
        let clear = clear_side(&child_style);
        if clear != ClearSide::None {
            cursor = floats.clearance(cursor, clear);
        }
        let (inset, width) = floats.available(cursor, 1.0, content);
        // A float that blocks the whole band is handled by the line breaker
        // (lines drop below it); keep the block box at its normal width in
        // that case instead of squeezing it to nothing.
        let (inset, width) = if width <= 1.0 {
            (0.0, content.width)
        } else {
            (inset, width)
        };
        let bottom = layout_box_with(
            tree,
            doc,
            child,
            content.x + inset,
            cursor,
            width.max(1.0),
            floats,
        );
        content_bottom = bottom;
        cursor = bottom;
        prev_margin_bottom = margin_bottom;
    }
    if !first {
        content_bottom += prev_margin_bottom;
    }
    (content_bottom - content.y).max(0.0)
}

fn layout_float(
    tree: &mut LayoutTree,
    doc: &Document,
    child: usize,
    style: &ComputedStyle,
    start_y: f32,
    content: Rect,
    floats: &mut FloatContext,
) {
    // Floats shrink to fit when no width is specified.
    let measure_width = if style.width().is_auto() {
        (shrink_to_fit(tree, child, content.width)
            + resolve(style.margin(Side::Left), content.width)
            + resolve(style.margin(Side::Right), content.width))
        .max(1.0)
    } else {
        content.width.max(1.0)
    };
    // A float establishes its own formatting context.
    let mut inner = FloatContext::new();
    let _ = layout_box_with(tree, doc, child, 0.0, 0.0, measure_width, &mut inner);

    let rect = tree.boxes[child].rect;
    let side = match style.float() {
        Float::Right => FloatSide::Right,
        _ => FloatSide::Left,
    };
    let (x, y) = floats.place(side, rect.width, rect.height, start_y, content);
    translate_subtree(tree, child, x - rect.x, y - rect.y);
}

fn clear_side(style: &ComputedStyle) -> ClearSide {
    match style.get(Property::Clear).and_then(|v| v.as_keyword()) {
        Some("left") => ClearSide::Left,
        Some("right") => ClearSide::Right,
        Some("both") => ClearSide::Both,
        _ => ClearSide::None,
    }
}

/// Lays out the inline content of a block container and returns its height.
fn layout_inline_children(
    tree: &mut LayoutTree,
    doc: &Document,
    index: usize,
    floats: &FloatContext,
) -> f32 {
    let content = tree.boxes[index].content;
    let style = tree.boxes[index].style.clone();
    let mut items = Vec::new();
    collect_inline_items(tree, doc, index, content.width, &mut items);

    // Whitespace between block-level siblings collapses away entirely.
    if items
        .iter()
        .all(|item| matches!(item, InlineItem::Space { .. }))
    {
        tree.boxes[index].lines.clear();
        return 0.0;
    }

    let mut breaker = LineBreaker::new(content.width.max(1.0), content.y);
    breaker.set_floats(floats.bands(content));
    for item in items {
        breaker.push(item);
    }
    let mut lines = breaker.finish(style.text_align(), content.width);
    let start_y = content.y;

    let mut next_y = start_y;
    for line in &mut lines {
        line.rect.x += content.x;
        for fragment in &mut line.fragments {
            fragment.x += content.x;
        }
        for atom in &mut line.atoms {
            atom.rect.x += content.x;
        }
        next_y = line.rect.bottom().max(next_y);
    }

    // Place atomic inline boxes now that line positions are final.
    let atoms: Vec<(usize, Rect)> = lines
        .iter()
        .flat_map(|line| line.atoms.iter().map(|a| (a.box_index, a.rect)))
        .collect();
    for (child, target) in atoms {
        let current = tree.boxes[child].rect;
        translate_subtree(tree, child, target.x - current.x, target.y - current.y);
    }

    // Give text boxes a rectangle covering their fragments.
    let mut text_rects: std::collections::HashMap<NodeId, Rect> = std::collections::HashMap::new();
    for line in &lines {
        for fragment in &line.fragments {
            let rect = Rect::new(
                fragment.x,
                fragment.baseline - fragment.font_size,
                fragment.width,
                fragment.font_size * 1.2,
            );
            text_rects
                .entry(fragment.node)
                .and_modify(|r| *r = r.union(&rect))
                .or_insert(rect);
        }
    }
    for (node, rect) in text_rects {
        if let Some(index) = tree
            .boxes
            .iter()
            .position(|b| b.node == Some(node) && b.box_type == BoxType::Text)
        {
            let b = &mut tree.boxes[index];
            b.rect = rect;
            b.content = rect;
        }
    }

    tree.boxes[index].lines = lines;
    (next_y - start_y).max(0.0)
}

fn collect_inline_items(
    tree: &mut LayoutTree,
    doc: &Document,
    index: usize,
    available_width: f32,
    items: &mut Vec<InlineItem>,
) {
    let children = tree.boxes[index].children.clone();
    for child in children {
        let box_type = tree.boxes[child].box_type;
        let style = tree.boxes[child].style.clone();
        match box_type {
            BoxType::Text => {
                let text = tree.boxes[child].text.clone();
                let mode = whitespace_mode(&style);
                let collapsed = collapse_whitespace(&text, mode);
                let font_size = style.font_size();
                let color = style.color();
                let decoration = style.text_decoration();
                let bold = style.font_weight() >= 600;
                let italic = style.is_italic();
                for segment in split_segments(&collapsed, mode) {
                    match segment {
                        Segment::Word(word) => {
                            // Split the word at UAX #14-style opportunities so
                            // the breaker can wrap inside long hyphenated or
                            // bracketed runs while keeping punctuation glued to
                            // the word it belongs to.
                            for unit in split_break_units(&word) {
                                let width = measure(&unit, font_size);
                                items.push(InlineItem::Text {
                                    node: tree.boxes[child].node.unwrap_or(NodeId(0)),
                                    text: unit,
                                    width,
                                    font_size,
                                    color,
                                    underline: decoration == TextDecoration::Underline,
                                    line_through: decoration == TextDecoration::LineThrough,
                                    bold,
                                    italic,
                                    break_before: true,
                                });
                            }
                        }
                        Segment::Space => {
                            let width = measure(" ", font_size);
                            items.push(InlineItem::Space { width, font_size });
                        }
                        Segment::Newline => {
                            items.push(InlineItem::ForcedBreak { font_size });
                        }
                    }
                }
            }
            BoxType::LineBreak => {
                items.push(InlineItem::ForcedBreak {
                    font_size: style.font_size(),
                });
            }
            BoxType::Inline => {
                collect_inline_items(tree, doc, child, available_width, items);
            }
            BoxType::InlineBlock | BoxType::Image => {
                // A replaced/atomic inline with an auto width must shrink to fit
                // its contents instead of stretching across the containing
                // block. This is what keeps a submit `<input>` (auto width,
                // definite height, a label) a compact button rather than a
                // full-width bar. A definite width — from CSS or from an
                // `<img width>` / `<input size>` presentational hint — is left
                // untouched and honoured by `layout_box` as before.
                let cb_width = if box_type == BoxType::InlineBlock && style.width().is_auto() {
                    shrink_to_fit(tree, child, available_width).max(0.0)
                } else {
                    available_width
                };
                let bottom = layout_box(tree, doc, child, 0.0, 0.0, cb_width);
                let b = &tree.boxes[child];
                let width = b.rect.width;
                let height = (b.content.bottom() - b.rect.y).max(b.rect.height);
                let _ = bottom;
                items.push(InlineItem::Atom {
                    box_index: child,
                    width,
                    height,
                    baseline: height,
                });
            }
            _ => {
                items.push(InlineItem::ForcedBreak {
                    font_size: style.font_size(),
                });
            }
        }
    }
}

enum Segment {
    Word(String),
    Space,
    Newline,
}

fn split_segments(text: &str, mode: browser_style::WhiteSpace) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut word = String::new();
    let preserves_newlines = matches!(
        mode,
        browser_style::WhiteSpace::Pre
            | browser_style::WhiteSpace::PreWrap
            | browser_style::WhiteSpace::PreLine
    );
    for ch in text.chars() {
        if ch == '\n' && preserves_newlines {
            if !word.is_empty() {
                segments.push(Segment::Word(std::mem::take(&mut word)));
            }
            segments.push(Segment::Newline);
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                segments.push(Segment::Word(std::mem::take(&mut word)));
            }
            segments.push(Segment::Space);
        } else {
            word.push(ch);
        }
    }
    if !word.is_empty() {
        segments.push(Segment::Word(word));
    }
    segments
}

/// Splits a whitespace-delimited word into line-break units, approximating the
/// relevant UAX #14 rules:
///
/// * no break before closing punctuation (`, . ! ? ; : ) ] }`) — it stays in
///   the current unit;
/// * no break after opening punctuation (`( [ {`) — the opener starts a new
///   unit, so a break is allowed *before* it but not after it;
/// * a break is allowed after `-` and `/`, so those end a unit.
///
/// Each returned unit is a separate [`InlineItem::Text`] with `break_before`
/// set, which is where the greedy line breaker introduces a wrap opportunity.
fn split_break_units(word: &str) -> Vec<String> {
    const OPENERS: [char; 3] = ['(', '[', '{'];
    const BREAK_AFTER: [char; 2] = ['-', '/'];

    let mut units = Vec::new();
    let mut current = String::new();
    for ch in word.chars() {
        if OPENERS.contains(&ch) {
            if !current.is_empty() {
                units.push(std::mem::take(&mut current));
            }
            current.push(ch);
        } else {
            current.push(ch);
            if BREAK_AFTER.contains(&ch) {
                units.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

fn measure(text: &str, font_size: f32) -> f32 {
    browser_text::Font::monospace().measure(text, font_size)
}

/// Resolves `auto` to zero and percentages against `base`.
pub fn resolve(dim: Dim, base: f32) -> f32 {
    dim.resolve(base).unwrap_or(0.0)
}

fn clamp_dim(value: f32, min: Dim, max: Dim, base: f32) -> f32 {
    let mut value = value;
    if let Some(min) = min.resolve(base) {
        value = value.max(min);
    }
    if let Some(max) = max.resolve(base) {
        value = value.min(max);
    }
    value
}

/// Reads the preferred aspect ratio (width / height) from a style.
///
/// The value is read generically through `ComputedStyle::get` and parsed
/// defensively: the typed `AspectRatio` value is preferred, but keyword,
/// string and plain-number representations are accepted too. Returns `None`
/// for `auto` or for any malformed / non-positive ratio.
pub(crate) fn preferred_aspect_ratio(style: &ComputedStyle) -> Option<f32> {
    let property = browser_style::Property::from_name("aspect-ratio")?;
    let value = style.get(property)?;
    match value {
        browser_style::ComputedValue::AspectRatio(ratio) => match ratio {
            browser_style::AspectRatio::Ratio(w, h) => positive_ratio(w / h),
            browser_style::AspectRatio::Auto => None,
        },
        browser_style::ComputedValue::Number(n) => positive_ratio(*n),
        browser_style::ComputedValue::Px(n) => positive_ratio(*n),
        browser_style::ComputedValue::Str(s) => parse_ratio_text(s),
        browser_style::ComputedValue::Keyword(k) => parse_ratio_text(k),
        browser_style::ComputedValue::IdentList(list) => parse_ratio_text(&list.join(" ")),
        _ => None,
    }
}

fn positive_ratio(ratio: f32) -> Option<f32> {
    (ratio.is_finite() && ratio > 0.0).then_some(ratio)
}

/// Parses `16 / 9`, `16/9`, `1.5`, `auto 16 / 9` and similar forms.
fn parse_ratio_text(text: &str) -> Option<f32> {
    let mut text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("auto") || text.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(rest) = text
        .strip_prefix("auto ")
        .or_else(|| text.strip_prefix("auto\t"))
    {
        text = rest.trim_start();
    }
    let mut parts = text.split('/');
    let width: f32 = parts.next()?.trim().parse().ok()?;
    let height: f32 = match parts.next() {
        Some(part) => {
            let part = part.trim();
            if part.is_empty() || part.eq_ignore_ascii_case("auto") {
                1.0
            } else {
                part.parse().ok()?
            }
        }
        None => 1.0,
    };
    if parts.next().is_some() {
        return None;
    }
    positive_ratio(width / height)
}

/// True when a dimension contains a percentage of an unknown base.
///
/// Layout only knows the containing block's width, so a percentage height has
/// no definite value. Resolving it against the width and then deriving another
/// dimension from it is the feedback loop that makes deeply nested flex/grid
/// content grow geometrically.
fn has_percentage(dim: Dim) -> bool {
    matches!(dim, Dim::Percent(_)) || matches!(dim, Dim::Calc { percent, .. } if percent != 0.0)
}

/// Resolves a definite dimension to an inner (content) size, or `None` when
/// the dimension is indefinite (`auto` or percentage-based). `border_box`
/// follows the `box-sizing` convention used by the rest of this module.
fn definite_content_dim(dim: Dim, border_box: bool, extra: f32) -> Option<f32> {
    let resolved = match dim {
        Dim::Auto => return None,
        Dim::Px(px) => px,
        // Percentage heights have no definite base here; using `base` (the
        // width) would let a `height: 100%` box set its own width via the
        // aspect ratio and inflate every nesting level.
        Dim::Percent(_) => return None,
        Dim::Calc { percent, .. } if percent != 0.0 => return None,
        Dim::Calc { px, .. } => px,
    };
    Some(if border_box {
        (resolved - extra).max(0.0)
    } else {
        resolved.max(0.0)
    })
}

/// Max-content width estimate used for shrink-to-fit boxes (inline-blocks,
/// floats and auto grid tracks).
pub(crate) fn shrink_to_fit(tree: &LayoutTree, index: usize, available: f32) -> f32 {
    let width = max_content_width(tree, index, available);
    width.min(available)
}

/// Recursive max-content estimate: it accounts for explicitly sized children
/// and for the box insets that the old text-only walk ignored. Floats and
/// inline-blocks use this, so missing a child width used to push right-floated
/// navigation links off the viewport.
fn max_content_width(tree: &LayoutTree, index: usize, available: f32) -> f32 {
    let box_ref = &tree.boxes[index];
    let mut width = match box_ref.box_type {
        BoxType::Text => measure(&box_ref.text, box_ref.style.font_size()),
        _ => {
            if let Some(explicit) = box_ref.style.width().resolve(available) {
                explicit
            } else {
                let children: Vec<f32> = box_ref
                    .children
                    .iter()
                    .map(|&child| max_content_width(tree, child, available))
                    .collect();
                let inline_children = box_ref.children.iter().any(|&child| {
                    matches!(
                        tree.boxes[child].box_type,
                        BoxType::Text | BoxType::Inline | BoxType::InlineBlock | BoxType::Image
                    )
                });
                if inline_children {
                    children.iter().sum()
                } else {
                    children.iter().copied().fold(0.0, f32::max)
                }
            }
        }
    };
    for side in [Side::Left, Side::Right] {
        width += box_ref
            .style
            .padding(side)
            .resolve(available)
            .unwrap_or(0.0);
        width += box_ref.style.border_width(side);
        width += box_ref.style.margin(side).resolve(available).unwrap_or(0.0);
    }
    width
}

/// Border style helper used by paint through the layout tree.
pub fn border_visible(style: &ComputedStyle, side: Side) -> bool {
    style.border_style(side) != BorderStyle::None && style.border_width(side) > 0.0
}

/// True when the display value generates a block-level box.
pub fn display_is_block_level(display: Display) -> bool {
    matches!(
        display,
        Display::Block | Display::Flex | Display::Grid | Display::ListItem | Display::Table
    )
}

/// Computes the inner content height of a box for scrolling.
pub fn scrollable_content_height(b: &LayoutBox) -> f32 {
    b.scroll_height.max(b.content.height)
}

#[cfg(test)]
mod tests {
    use browser_dom::Document;
    use browser_style::{Origin, StyleEngine, Viewport};

    use crate::test_util::{layout_grid, layout_with};

    /// Builds a document whose body holds a single `<input>` with the given
    /// attributes, lays it out at `viewport_width`, and returns its box.
    fn layout_input(
        css: &str,
        attrs: &[(&str, &str)],
        viewport_width: f32,
    ) -> (Document, browser_dom::NodeId, crate::LayoutTree) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let input = doc.create_element("input");
        for (name, value) in attrs {
            doc.set_attr(input, name, value);
        }
        doc.append_child(body, input);
        let mut engine = StyleEngine::new(Viewport {
            width: viewport_width,
            height: 800.0,
        });
        engine.add_css(css, Origin::Author);
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: viewport_width,
                height: 800.0,
            },
        );
        (doc, input, tree)
    }

    #[test]
    fn input_css_width_is_honored_over_size_hint() {
        // The Google case: `.Ucigb { width: 458px }` plus `size="57"` (a 456px
        // hint). The author width must win and the control must not stretch to
        // the ~890px containing block.
        let (_doc, input, tree) = layout_input(
            "body { margin: 8px; } .Ucigb { width: 458px; }",
            &[("class", "lst Ucigb"), ("size", "57")],
            906.0,
        );
        let b = tree.find_node(input).unwrap();
        // rect.width includes the UA border/padding; the specified width is
        // the content width under the default content-box sizing.
        assert_eq!(b.content.width, 458.0, "specified CSS width was ignored");
    }

    #[test]
    fn input_size_hint_applies_without_css_width() {
        // With no CSS width, `size="57"` becomes an ~8px-per-character hint:
        // 57 * 8 = 456px, still far short of the 890px containing block.
        let (_doc, input, tree) = layout_input(
            "body { margin: 8px; }",
            &[("type", "text"), ("size", "57")],
            906.0,
        );
        let b = tree.find_node(input).unwrap();
        assert_eq!(
            b.content.width, 456.0,
            "size presentational hint was ignored"
        );
    }

    #[test]
    fn auto_width_submit_input_shrinks_to_its_label() {
        // A submit input has a definite height and a synthetic label child but
        // no width: it must size to its content, not become a full-width bar.
        let (_doc, input, tree) = layout_input(
            "body { margin: 0; } input { height: 30px; }",
            &[("type", "submit"), ("value", "Pesquisa Google")],
            800.0,
        );
        let b = tree.find_node(input).unwrap();
        // The author `height` sizes the content box; the UA sheet's padding and
        // border are added on top, so the border box is taller than 30px.
        assert_eq!(b.content.height, 30.0);
        assert!(b.rect.height > 30.0, "border box should include UA insets");
        assert_eq!(b.children.len(), 1, "synthetic label child must survive");
        assert!(
            b.rect.width > 0.0 && b.rect.width < 800.0,
            "submit input stretched to the containing block: {}",
            b.rect.width
        );
    }

    #[test]
    fn auto_width_inline_block_uses_content_width() {
        let (_doc, tree, ids) = layout_with(
            "body { margin: 0; } #b { display: inline-block; }",
            &[("span", "b", "abc")],
        );
        let b = tree.find_node(ids[1]).unwrap();
        assert!(
            b.rect.width > 0.0 && b.rect.width < 800.0,
            "auto-width inline-block filled the containing block: {}",
            b.rect.width
        );
    }

    #[test]
    fn two_left_floats_sit_side_by_side() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; } #a, #b { float: left; width: 100px; height: 50px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        // ids: body, a, b
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        assert_eq!(a.rect.x, 0.0);
        assert_eq!(a.rect.y, 0.0);
        assert_eq!(b.rect.x, 100.0);
        assert_eq!(b.rect.y, 0.0);
        let _ = doc;
    }

    #[test]
    fn right_float_is_offset_to_the_right_edge() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; } #a { float: right; width: 100px; height: 20px; }",
            &[("div", "a", "")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        assert_eq!(a.rect.x, 700.0);
        assert_eq!(a.rect.right(), 800.0);
        let _ = doc;
    }

    #[test]
    fn right_float_shrink_width_includes_padding() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; } #a { float: right; padding: 0 20px; font-size: 10px; }",
            &[("div", "a", "abcd")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        // Old text-only estimate was ~24px; horizontal padding must count too.
        assert!(a.rect.width >= 60.0, "float width was {}", a.rect.width);
        assert!(a.rect.right() <= 800.0 + 0.01);
        let _ = doc;
    }

    #[test]
    fn text_wraps_beside_a_left_float() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; font-size: 10px; } #a { float: left; width: 200px; height: 40px; }",
            &[
                ("div", "a", ""),
                (
                    "p",
                    "b",
                    "one two three four five six seven eight nine ten eleven twelve",
                ),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        assert_eq!(a.rect.x, 0.0);
        let p = tree.find_node(ids[2]).unwrap();
        assert_eq!(p.content.x, 200.0);
        assert_eq!(p.content.width, 600.0);
        assert!(!p.lines.is_empty(), "paragraph should have lines");
        for line in &p.lines {
            assert!(line.rect.right() <= 800.0 + 0.01);
        }
        let _ = doc;
    }

    #[test]
    fn clear_both_moves_box_below_the_float() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; } #f { float: left; width: 100px; height: 60px; } #c { clear: both; height: 10px; }",
            &[("div", "f", ""), ("div", "c", "")],
        );
        let c = tree.find_node(ids[2]).unwrap();
        assert_eq!(c.rect.y, 60.0);
        assert_eq!(c.rect.x, 0.0);
        let _ = doc;
    }

    #[test]
    fn float_taller_than_explicit_container_overflows() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; height: 100px; } #f { float: left; width: 50px; height: 200px; }",
            &[("div", "f", "")],
        );
        let body = tree.find_node(ids[0]).unwrap();
        let f = tree.find_node(ids[1]).unwrap();
        assert_eq!(body.content.height, 100.0);
        assert_eq!(f.rect.height, 200.0);
        assert_eq!(f.rect.y, 0.0);
        let _ = doc;
    }

    #[test]
    fn float_height_bounds_parent_auto_height() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; } #f { float: left; width: 50px; height: 120px; } #n { height: 20px; }",
            &[("div", "f", ""), ("div", "n", "")],
        );
        let body = tree.find_node(ids[0]).unwrap();
        // The float contributes up to its own bottom, but no further.
        assert_eq!(body.content.height, 120.0);
        let _ = doc;
    }

    #[test]
    fn float_blocking_the_whole_line_pushes_text_below() {
        let (doc, tree, ids) = layout_with(
            "body { margin: 0; width: 100px; font-size: 10px; } #f { float: left; width: 100px; height: 40px; }",
            &[("div", "f", ""), ("p", "p", "alpha beta gamma")],
        );
        let p = tree.find_node(ids[2]).unwrap();
        assert!(!p.lines.is_empty());
        for line in &p.lines {
            assert!(
                line.rect.y >= 40.0,
                "line at {} should be below the float",
                line.rect.y
            );
        }
        let _ = doc;
    }

    /// Lays out a `<div>` holding a single `<img>`, returning the image box.
    fn layout_image(css: &str) -> (Document, browser_dom::NodeId, crate::LayoutTree) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let box_node = doc.create_element("div");
        doc.set_attr(box_node, "id", "box");
        doc.append_child(body, box_node);
        let img = doc.create_element("img");
        doc.append_child(box_node, img);
        let mut engine = browser_style::StyleEngine::new(browser_style::Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(css, Origin::Author);
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: 800.0,
                height: 600.0,
            },
        );
        let _ = box_node;
        (doc, img, tree)
    }

    #[test]
    fn percentage_image_height_does_not_resolve_against_width() {
        // A 100% height inside a 400px box used to resolve against the
        // containing-block *width*, turning the image into a 400x400 square.
        // The base is indefinite, so the replaced element keeps its intrinsic
        // 150px height instead of feeding the width back into its height.
        let (_doc, img, tree) =
            layout_image("#box { width:400px; } img { width:100%; height:100%; }");
        let b = tree.find_node(img).unwrap();
        assert_eq!(b.rect.width, 400.0);
        assert!(
            b.rect.height <= 160.0,
            "percentage height resolved against the width: {}",
            b.rect.height
        );
    }

    #[test]
    fn oversized_box_is_capped_at_its_containing_block() {
        // A flex/grid measurement that resolves a descendant's percentage
        // width against the container can pin an item wider than the
        // container. Capping the content width stops the geometric growth at
        // the next nesting level.
        let (_doc, tree, ids) = layout_grid(
            "body { margin:0; } #g { width:200px; } #g > div { width:1000px; height:10px; }",
            &[("div", "wide", "")],
        );
        let wide = tree.find_node(ids[1]).unwrap();
        assert!(
            wide.rect.width <= 200.0,
            "box grew past its containing block: {}",
            wide.rect.width
        );
    }

    #[test]
    fn aspect_ratio_text_is_parsed_defensively() {
        assert_eq!(super::parse_ratio_text("16 / 9"), Some(16.0 / 9.0));
        assert_eq!(super::parse_ratio_text("16/9"), Some(16.0 / 9.0));
        assert_eq!(super::parse_ratio_text("1.5"), Some(1.5));
        assert_eq!(super::parse_ratio_text("auto 2 / 1"), Some(2.0));
        assert_eq!(super::parse_ratio_text("auto"), None);
        assert_eq!(super::parse_ratio_text("none"), None);
        assert_eq!(super::parse_ratio_text("0 / 9"), None);
        assert_eq!(super::parse_ratio_text("nonsense"), None);
        assert_eq!(super::parse_ratio_text("2 / 0"), None);
    }

    #[test]
    fn aspect_ratio_derives_height_from_definite_width() {
        let (_doc, tree, ids) = layout_with(
            "body { margin: 0; } #box { width: 200px; aspect-ratio: 2 / 1; }",
            &[("div", "box", "")],
        );
        let b = tree.find_node(ids[1]).unwrap();
        assert_eq!(b.content.width, 200.0);
        assert!(
            (b.content.height - 100.0).abs() < 0.01,
            "{}",
            b.content.height
        );
    }

    #[test]
    fn aspect_ratio_derives_width_from_definite_height() {
        let (_doc, tree, ids) = layout_with(
            "body { margin: 0; } #box { height: 100px; aspect-ratio: 16 / 9; }",
            &[("div", "box", "")],
        );
        let b = tree.find_node(ids[1]).unwrap();
        assert_eq!(b.content.height, 100.0);
        let expected = 100.0 * 16.0 / 9.0;
        assert!(
            (b.content.width - expected).abs() < 0.01,
            "got {}, expected {}",
            b.content.width,
            expected
        );
    }

    #[test]
    fn aspect_ratio_accepts_a_plain_number_with_border_box() {
        let (_doc, tree, ids) = layout_with(
            "body { margin: 0; } #box { width: 160px; aspect-ratio: 1.5; box-sizing: border-box; padding: 0 10px; }",
            &[("div", "box", "")],
        );
        let b = tree.find_node(ids[1]).unwrap();
        // The ratio applies to the border box: 160 / 1.5 ≈ 106.67.
        assert!((b.rect.width - 160.0).abs() < 0.01);
        let expected = 160.0 / 1.5;
        assert!(
            (b.rect.height - expected).abs() < 0.01,
            "got {}, expected {}",
            b.rect.height,
            expected
        );
    }

    #[test]
    fn aspect_ratio_sizes_a_replaced_element() {
        let mut doc = browser_dom::Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let img = doc.create_element("img");
        doc.set_attr(img, "width", "200");
        doc.append_child(body, img);
        let mut engine = browser_style::StyleEngine::new(browser_style::Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(
            "body { margin: 0; } img { aspect-ratio: 2 / 1; }",
            browser_style::Origin::Author,
        );
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: 800.0,
                height: 600.0,
            },
        );
        let b = tree.find_node(img).unwrap();
        assert_eq!(b.content.width, 200.0);
        assert!(
            (b.content.height - 100.0).abs() < 0.01,
            "{}",
            b.content.height
        );
    }

    /// Builds a `div#scroll` (with `scroller_css`) holding a single `div#child`
    /// (with `child_css`), lays it out, and returns the tree plus both ids.
    fn layout_scroller(
        scroller_css: &str,
        child_css: &str,
        scroll_style_attr: &str,
    ) -> (
        Document,
        browser_dom::NodeId,
        browser_dom::NodeId,
        crate::LayoutTree,
    ) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let scroll = doc.create_element("div");
        doc.set_attr(scroll, "id", "scroll");
        if !scroll_style_attr.is_empty() {
            doc.set_attr(scroll, "style", scroll_style_attr);
        }
        doc.append_child(body, scroll);
        let child = doc.create_element("div");
        doc.set_attr(child, "id", "child");
        doc.append_child(scroll, child);
        let mut engine = StyleEngine::new(browser_style::Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(
            &format!("body {{ margin: 0; }} #scroll {scroller_css} #child {child_css}"),
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: 800.0,
                height: 600.0,
            },
        );
        (doc, scroll, child, tree)
    }

    #[test]
    fn scroll_overflow_reserves_a_gutter_from_content_width() {
        // The 300px child overflows the 100px scroller, so the 15px gutter must
        // be removed from the content width before the child is laid out.
        let (_doc, scroll, child, tree) = layout_scroller(
            "{ width: 200px; height: 100px; overflow-y: scroll; }",
            "{ height: 300px; }",
            "",
        );
        let s = tree.find_node(scroll).unwrap();
        let bar = s.scrollbar.expect("vertical scrollbar");
        assert_eq!(bar.width, super::SCROLLBAR_SIZE);
        assert_eq!(bar.height, 100.0);
        assert!((bar.x - (200.0 - super::SCROLLBAR_SIZE)).abs() < 0.01);
        assert_eq!(s.content.width, 200.0 - super::SCROLLBAR_SIZE);
        let c = tree.find_node(child).unwrap();
        assert_eq!(
            c.content.width,
            200.0 - super::SCROLLBAR_SIZE,
            "child was laid out at the full width, not the scrollbar-reduced width"
        );
    }

    #[test]
    fn auto_overflow_reserves_a_gutter_only_when_content_overflows() {
        // A 300px child in a 100px-tall scroller overflows → gutter reserved.
        let (_doc, scroll, child, tree) = layout_scroller(
            "{ width: 200px; height: 100px; overflow-y: auto; }",
            "{ height: 300px; }",
            "",
        );
        let s = tree.find_node(scroll).unwrap();
        assert!(
            s.scrollbar.is_some(),
            "overflowing auto box needs a scrollbar"
        );
        assert_eq!(s.content.width, 200.0 - super::SCROLLBAR_SIZE);
        let c = tree.find_node(child).unwrap();
        assert_eq!(c.content.width, 200.0 - super::SCROLLBAR_SIZE);

        // A 20px child fits → no scrollbar, full content width.
        let (_doc, scroll, child, tree) = layout_scroller(
            "{ width: 200px; height: 100px; overflow-y: auto; }",
            "{ height: 20px; }",
            "",
        );
        let s = tree.find_node(scroll).unwrap();
        assert!(
            s.scrollbar.is_none(),
            "non-overflowing auto box has no scrollbar"
        );
        assert_eq!(s.content.width, 200.0);
        let c = tree.find_node(child).unwrap();
        assert_eq!(c.content.width, 200.0);
    }

    #[test]
    fn scrollbar_width_none_suppresses_the_gutter() {
        // The style crate drops `scrollbar-width` from stylesheets, so the
        // authored inline `style` attribute carries it here.
        let (_doc, scroll, child, tree) = layout_scroller(
            "{ width: 200px; height: 100px; overflow: scroll; }",
            "{ height: 20px; }",
            "scrollbar-width: none",
        );
        let s = tree.find_node(scroll).unwrap();
        assert!(
            s.scrollbar.is_none(),
            "scrollbar-width:none must hide the bar"
        );
        assert_eq!(s.content.width, 200.0);
        let c = tree.find_node(child).unwrap();
        assert_eq!(c.content.width, 200.0);
    }

    #[test]
    fn break_units_respect_uax14_punctuation_rules() {
        assert_eq!(super::split_break_units("hello,"), vec!["hello,"]);
        assert_eq!(super::split_break_units("(hello"), vec!["(hello"]);
        assert_eq!(super::split_break_units("a(b)c"), vec!["a", "(b)c"]);
        assert_eq!(
            super::split_break_units("foo-bar/baz"),
            vec!["foo-", "bar/", "baz"]
        );
    }

    #[test]
    fn long_hyphenated_word_wraps_after_the_hyphen() {
        let (_doc, tree, ids) = layout_with(
            "body { margin: 0; font-size: 10px; } #p { width: 40px; }",
            &[("p", "p", "aaaa-bbbb-cccc")],
        );
        let p = tree.find_node(ids[1]).unwrap();
        assert!(
            p.lines.len() > 1,
            "a hyphenated word should wrap into multiple lines"
        );
    }

    #[test]
    fn closing_punctuation_does_not_begin_a_line() {
        let (_doc, tree, ids) = layout_with(
            "body { margin: 0; font-size: 10px; } #p { width: 45px; }",
            &[("p", "p", "alpha beta, gamma, delta, epsilon,")],
        );
        let p = tree.find_node(ids[1]).unwrap();
        for line in &p.lines {
            for fragment in &line.fragments {
                assert!(
                    !fragment
                        .text
                        .starts_with([',', '.', '!', '?', ';', ':', ')', ']', '}']),
                    "line began with punctuation: {:?}",
                    fragment.text
                );
            }
        }
    }
}
