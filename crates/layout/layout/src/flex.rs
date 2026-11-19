//! Flexbox layout with single-line and wrapping rows, `row`, `row-reverse`,
//! `column` and `column-reverse`.
//!
//! Rows implement intrinsic (min/max-content) sizing, `flex-grow` /
//! `flex-shrink` with `flex-basis` and `min-width: auto` clamping, `align-items`
//! (including the `stretch` default), `align-content` on wrapped lines and
//! main-axis `margin: auto` absorption. Wrapping packs items greedily into
//! lines and applies `justify-content` per line. `order` and baseline
//! alignment are not implemented yet.

use browser_dom::Document;
use browser_style::{
    AlignItems, BoxSizing, ComputedStyle, ComputedValue, Dim, FlexDirection, FlexWrap,
    JustifyContent, Property, Side,
};

use crate::block::{layout_box, resolve};
use crate::tree::{BoxType, LayoutTree};

/// Per-item data gathered during the measurement pass and used while the
/// flexible lengths of a line are resolved.
#[derive(Debug, Clone, Copy)]
struct FlexItem {
    child: usize,
    /// `flex-grow` factor.
    grow: f32,
    /// `flex-shrink` factor.
    shrink: f32,
    /// Flex base size as a border-box main size, already clamped by min/max.
    base: f32,
    /// Automatic (min-content) or specified minimum main size, border box.
    min: f32,
    /// Specified maximum main size, border box.
    max: Option<f32>,
    /// Resolved main-axis margins (auto margins receive free space later).
    ml: f32,
    mr: f32,
    ml_auto: bool,
    mr_auto: bool,
    /// Resolved cross-axis margins.
    mt: f32,
    mb: f32,
    /// Border + padding on the main axis (used when re-laying out the item).
    insets_main: f32,
    /// True when the box is laid out with `box-sizing: border-box`.
    border_box: bool,
    /// True when the cross size is `auto` and may be stretched.
    cross_auto: bool,
    /// `align-self` resolved against the container's `align-items`.
    align: AlignItems,
}

/// Final geometry of one flex line, computed before cross-axis positioning so
/// that `align-content` can distribute the free cross space between lines.
#[derive(Debug)]
struct PreparedLine {
    /// Item indices in visual order (reversed for `row-reverse`).
    order: Vec<usize>,
    /// Final border-box main sizes, indexed by item index.
    sizes: Vec<f32>,
    /// Final outer cross sizes (border box + margins), indexed by item index.
    cross: Vec<f32>,
    /// Largest outer cross size on the line.
    line_cross: f32,
    /// `justify-content` start offset.
    start: f32,
    /// `justify-content` space between items.
    between: f32,
}

/// Cross-axis distribution of the free space between flex lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlignContent {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Stretch,
}

fn align_content_of(style: &ComputedStyle) -> AlignContent {
    match style
        .align_content()
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "flex-start" | "start" => AlignContent::Start,
        "flex-end" | "end" => AlignContent::End,
        "center" => AlignContent::Center,
        "space-between" => AlignContent::SpaceBetween,
        "space-around" => AlignContent::SpaceAround,
        "space-evenly" => AlignContent::SpaceEvenly,
        // `normal` behaves as `stretch` in flexbox.
        _ => AlignContent::Stretch,
    }
}

fn item_align_of(style: &ComputedStyle, default: AlignItems) -> AlignItems {
    match style
        .align_self()
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "auto" => default,
        "flex-start" | "start" | "baseline" => AlignItems::FlexStart,
        "flex-end" | "end" => AlignItems::FlexEnd,
        "center" => AlignItems::Center,
        "stretch" => AlignItems::Stretch,
        _ => default,
    }
}

/// Lays out a flex container's children and returns the content height.
pub fn layout_flex(tree: &mut LayoutTree, doc: &Document, index: usize) -> f32 {
    let style = tree.boxes[index].style.clone();
    let content = tree.boxes[index].content;
    let direction = style.flex_direction();
    let justify = style.justify_content();
    let align = style.align_items();
    let row_gap = style.row_gap().max(0.0);
    let column_gap = style.column_gap().max(0.0);
    let children: Vec<usize> = tree.boxes[index]
        .children
        .iter()
        .copied()
        .filter(|child| !crate::tree::is_ignorable_child(tree, *child))
        .collect();
    if children.is_empty() {
        return 0.0;
    }

    match direction {
        FlexDirection::Row | FlexDirection::RowReverse => layout_flex_row(
            tree,
            doc,
            &style,
            content,
            direction == FlexDirection::RowReverse,
            &children,
            justify,
            align,
            column_gap,
            row_gap,
        ),
        FlexDirection::Column | FlexDirection::ColumnReverse => {
            let gap = row_gap;
            let available = content.width;
            let mut cursor = content.y;
            let mut order: Vec<usize> = (0..children.len()).collect();
            if direction == FlexDirection::ColumnReverse {
                order.reverse();
            }
            for child in &order {
                if !crate::budget::try_step() {
                    break;
                }
                let child = children[*child];
                let mt = resolve(tree.boxes[child].style.margin(Side::Top), available);
                let mb = resolve(tree.boxes[child].style.margin(Side::Bottom), available);
                let bottom = layout_box(tree, doc, child, content.x, cursor + mt, available);
                cursor = bottom + mb + gap;
            }
            (cursor - content.y - gap).max(0.0)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn layout_flex_row(
    tree: &mut LayoutTree,
    doc: &Document,
    style: &ComputedStyle,
    content: crate::Rect,
    reverse: bool,
    children: &[usize],
    justify: JustifyContent,
    align: AlignItems,
    gap: f32,
    line_gap: f32,
) -> f32 {
    let available = content.width.max(0.0);
    let count = children.len();
    let border_box = style.box_sizing() == BoxSizing::BorderBox;

    // The container's own vertical insets, needed to turn a definite height
    // into a definite *content* height for `align-content`.
    let container_insets_v = style.border_width(Side::Top)
        + style.border_width(Side::Bottom)
        + resolve(style.padding(Side::Top), available)
        + resolve(style.padding(Side::Bottom), available);

    // --- Measurement pass: flex base sizes and intrinsic minima -------------
    let mut items: Vec<FlexItem> = Vec::with_capacity(count);
    for &child in children {
        if !crate::budget::try_step() {
            break;
        }
        let cs = tree.boxes[child].style.clone();
        let insets_main = style_insets_main(&cs, available);
        let extra = insets_main;
        let (min_content, max_content) = content_widths(tree, child, available);

        let basis = cs.flex_basis();
        let width = cs.width();
        let base = if !basis.is_auto() {
            definite_border_size(resolve(basis, available), border_box, extra)
        } else if !width.is_auto() {
            definite_border_size(resolve(width, available), border_box, extra)
        } else {
            max_content
        };

        let min_main = match cs.dim(Property::MinWidth) {
            // `min-width: auto` on a flex item is its content-based minimum:
            // the min-content width, i.e. the longest unbreakable word. Cap it
            // at the container's available main size so a nested flex child
            // can never demand (and geometrically inflate to) more than the
            // container itself.
            Dim::Auto => min_content.min(available).max(0.0),
            dim => definite_border_size(resolve(dim, available), border_box, extra),
        };
        let max_main = match cs.dim(Property::MaxWidth) {
            Dim::Auto => None,
            dim => Some(definite_border_size(
                resolve(dim, available),
                border_box,
                extra,
            )),
        };
        let base = base
            .max(min_main)
            .min(max_main.unwrap_or(f32::INFINITY))
            .max(0.0);

        let ml_dim = cs.margin(Side::Left);
        let mr_dim = cs.margin(Side::Right);
        let item = FlexItem {
            child,
            grow: cs.flex_grow().max(0.0),
            shrink: cs.flex_shrink().max(0.0),
            base,
            min: min_main.min(base).max(0.0),
            max: max_main,
            ml: resolve(ml_dim, available),
            mr: resolve(mr_dim, available),
            ml_auto: ml_dim.is_auto(),
            mr_auto: mr_dim.is_auto(),
            mt: resolve(cs.margin(Side::Top), available),
            mb: resolve(cs.margin(Side::Bottom), available),
            insets_main,
            border_box,
            cross_auto: cs.height().is_auto(),
            align: item_align_of(&cs, align),
        };
        items.push(item);
    }
    // If the budget was exhausted mid-measurement, lay out what we have.
    let count = items.len();
    if count == 0 {
        return 0.0;
    }

    for &it in &items {
        if !crate::budget::try_step() {
            break;
        }
        layout_item(
            tree,
            doc,
            it.child,
            available,
            it.insets_main,
            it.border_box,
            it.base,
        );
    }

    let wraps = matches!(style.flex_wrap(), FlexWrap::Wrap | FlexWrap::WrapReverse);
    let outers: Vec<f32> = items.iter().map(|it| it.base + it.ml + it.mr).collect();
    let lines: Vec<Vec<usize>> = if wraps {
        greedy_lines(&outers, gap, available)
    } else {
        vec![(0..count).collect()]
    };

    // --- Phase 1: resolve each line's flexible lengths and cross sizes ------
    let mut prepared: Vec<PreparedLine> = Vec::with_capacity(lines.len());
    for line in &lines {
        if !crate::budget::try_step() {
            break;
        }
        let line_used: f32 = line.iter().map(|&i| outers[i]).sum::<f32>()
            + gap * line.len().saturating_sub(1) as f32;
        let mut free = available - line_used;

        // `margin: auto` on the main axis absorbs positive free space before
        // flex-grow and justify-content are considered.
        let auto_margins: usize = line
            .iter()
            .map(|&i| usize::from(items[i].ml_auto) + usize::from(items[i].mr_auto))
            .sum();
        if free > 0.0 && auto_margins > 0 {
            let each = free / auto_margins as f32;
            for &i in line {
                if items[i].ml_auto {
                    items[i].ml += each;
                }
                if items[i].mr_auto {
                    items[i].mr += each;
                }
            }
            free = 0.0;
        }

        let resolved = if free < 0.0 {
            distribute_shrink(&items, line, -free)
        } else if free > 0.0 {
            distribute_grow(&items, line, free)
        } else {
            line.iter().map(|&i| items[i].base).collect()
        };
        // Store sizes by global item index so positioning can look them up
        // directly, regardless of which line an item belongs to.
        let mut sizes = vec![0.0_f32; count];
        for (k, &i) in line.iter().enumerate() {
            sizes[i] = resolved[k];
        }

        // Re-layout items whose main size changed so text wraps at the final
        // width instead of overflowing at its hypothetical size.
        for &i in line {
            if !crate::budget::try_step() {
                break;
            }
            if (sizes[i] - items[i].base).abs() > 0.01 {
                layout_item(
                    tree,
                    doc,
                    items[i].child,
                    available,
                    items[i].insets_main,
                    items[i].border_box,
                    sizes[i],
                );
            }
        }

        // An item with a preferred aspect ratio takes its cross size from its
        // FINAL main size (after shrink/grow), not from an inflated intrinsic
        // min-content measurement. `layout_box` already derives this from the
        // pinned width, but re-assert it here so a nested percentage expansion
        // cannot leak a larger min-content back into the cross axis (for
        // example a `.phone` with `aspect-ratio: 9/19.4` growing tall because a
        // descendant reported a 750px min-content width).
        for &i in line {
            let child = items[i].child;
            let it = &items[i];
            if !it.cross_auto {
                continue;
            }
            let cs = tree.boxes[child].style.clone();
            let Some(ratio) = crate::block::preferred_aspect_ratio(&cs) else {
                continue;
            };
            let b = &tree.boxes[child];
            let insets_cross = (b.rect.height - b.content.height).max(0.0);
            // Match `layout_box`: under border-box sizing the ratio applies to
            // the border-box main size; under content-box sizing it applies to
            // the content box.
            let mut content_cross = if it.border_box {
                (sizes[i] / ratio - insets_cross).max(0.0)
            } else {
                ((sizes[i] - it.insets_main) / ratio).max(0.0)
            };
            if let Some(min) = cs.dim(Property::MinHeight).resolve(available) {
                content_cross = content_cross.max(min);
            }
            if let Some(max) = cs.dim(Property::MaxHeight).resolve(available) {
                content_cross = content_cross.min(max);
            }
            let b = &mut tree.boxes[child];
            b.content.height = content_cross;
            b.rect.height = content_cross + insets_cross;
        }

        let mut cross = vec![0.0_f32; count];
        let mut line_cross = 0.0_f32;
        for &i in line {
            let b = &tree.boxes[items[i].child];
            cross[i] = b.rect.height + items[i].mt + items[i].mb;
            line_cross = line_cross.max(cross[i]);
        }

        // Justify only the space left after flexible lengths and auto margins
        // have been resolved; this is zero when items grew to fill the line.
        let used_after: f32 = line
            .iter()
            .map(|&i| sizes[i] + items[i].ml + items[i].mr)
            .sum::<f32>()
            + gap * line.len().saturating_sub(1) as f32;
        let remaining = (available - used_after).max(0.0);
        let (start, between) = justify_offsets(justify, line.len(), remaining);
        let mut order = line.clone();
        if reverse {
            order.reverse();
        }
        prepared.push(PreparedLine {
            order,
            sizes,
            cross,
            line_cross,
            start,
            between,
        });
    }
    if prepared.is_empty() {
        return 0.0;
    }

    // --- Phase 2: align-content, cross positioning and stretch --------------
    let line_count = prepared.len();
    let used_cross: f32 = prepared.iter().map(|l| l.line_cross).sum::<f32>()
        + line_gap * line_count.saturating_sub(1) as f32;
    let container_cross = definite_container_cross(style, available, container_insets_v);
    let free_cross = (container_cross - used_cross).max(0.0);
    let (cross_start, cross_between, cross_extra) = match align_content_of(style) {
        AlignContent::Start => (0.0, 0.0, 0.0),
        AlignContent::End => (free_cross, 0.0, 0.0),
        AlignContent::Center => (free_cross / 2.0, 0.0, 0.0),
        AlignContent::SpaceBetween if line_count > 1 => {
            (0.0, free_cross / (line_count - 1) as f32, 0.0)
        }
        AlignContent::SpaceAround => {
            let each = free_cross / line_count as f32;
            (each / 2.0, each, 0.0)
        }
        AlignContent::SpaceEvenly => {
            let each = free_cross / (line_count + 1) as f32;
            (each, each, 0.0)
        }
        AlignContent::Stretch | AlignContent::SpaceBetween => {
            (0.0, 0.0, free_cross / line_count as f32)
        }
    };

    let mut line_y = content.y + cross_start;
    let mut total_cross = 0.0_f32;
    for (line_index, line) in prepared.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let effective_cross = line.line_cross + cross_extra;
        let mut cursor = content.x + line.start;
        for &i in &line.order {
            if !crate::budget::try_step() {
                break;
            }
            let it = &items[i];
            let child = it.child;
            let current = tree.boxes[child].rect;
            let cross_offset = match it.align {
                AlignItems::Center => (effective_cross - line.cross[i]).max(0.0) / 2.0,
                AlignItems::FlexEnd => (effective_cross - line.cross[i]).max(0.0),
                AlignItems::FlexStart | AlignItems::Baseline | AlignItems::Stretch => 0.0,
            };
            let y = line_y + cross_offset + it.mt;
            {
                let b = &mut tree.boxes[child];
                // `layout_box` resolves `auto` margins to zero; publish the
                // final (possibly auto-absorbed) margins used by hit testing.
                b.margins.left = it.ml;
                b.margins.right = it.mr;
                b.margins.top = it.mt;
                b.margins.bottom = it.mb;
            }
            crate::position::translate_subtree(
                tree,
                child,
                cursor + it.ml - current.x,
                y - current.y,
            );

            // `align-items: stretch` (the default) fills the line's cross size
            // for items whose cross size is auto.
            if it.align == AlignItems::Stretch && it.cross_auto {
                let b = &mut tree.boxes[child];
                let insets_v = b.borders.vertical() + b.paddings.vertical();
                let inner = (effective_cross - it.mt - it.mb - insets_v).max(0.0);
                b.content.height = inner;
                b.rect.height = inner + insets_v;
            }

            cursor += line.sizes[i] + it.ml + it.mr + gap + line.between;
        }
        total_cross += effective_cross;
        if line_index + 1 < line_count {
            total_cross += line_gap + cross_between;
            line_y += effective_cross + line_gap + cross_between;
        }
    }
    total_cross.max(style.line_height_px())
}

/// Turns a resolved `width`/`flex-basis` (content box under `content-box`
/// sizing, border box under `border-box` sizing) into a border-box main size.
fn definite_border_size(resolved: f32, border_box: bool, insets: f32) -> f32 {
    if border_box {
        resolved.max(0.0)
    } else {
        (resolved + insets).max(0.0)
    }
}

/// The container's definite content height, or zero when height is auto.
fn definite_container_cross(style: &ComputedStyle, available: f32, insets_v: f32) -> f32 {
    match style.height() {
        Dim::Auto => 0.0,
        dim => definite_border_size(
            resolve(dim, available),
            style.box_sizing() == BoxSizing::BorderBox,
            insets_v,
        ),
    }
}

/// Border + padding on the main (horizontal) axis of an item.
fn style_insets_main(style: &ComputedStyle, available: f32) -> f32 {
    style.border_width(Side::Left)
        + style.border_width(Side::Right)
        + resolve(style.padding(Side::Left), available)
        + resolve(style.padding(Side::Right), available)
}

/// Lays out one item at a requested border-box main size.
///
/// `layout_box` honours a specified `width` over the width hint passed to it,
/// so a definite `flex-basis` (or a shrunk/grown size) is applied by
/// temporarily pinning `width` to the resolved value. Percentage padding and
/// margins therefore still resolve against the flex container, and text is
/// line-broken at the final main size.
fn layout_item(
    tree: &mut LayoutTree,
    doc: &Document,
    child: usize,
    available: f32,
    insets: f32,
    border_box: bool,
    main_border: f32,
) {
    let original = tree.boxes[child]
        .style
        .get(Property::Width)
        .cloned()
        .unwrap_or(ComputedValue::Dim(Dim::Auto));
    let width = if border_box {
        main_border.max(0.0)
    } else {
        (main_border - insets).max(0.0)
    };
    tree.boxes[child]
        .style
        .set(Property::Width, ComputedValue::Dim(Dim::Px(width)));
    let _ = layout_box(tree, doc, child, 0.0, 0.0, available.max(1.0));
    tree.boxes[child].style.set(Property::Width, original);
}

/// Computes the intrinsic `(min-content, max-content)` border-box widths of a
/// box, recursing through its descendants.
///
/// For text, min-content is the longest unbreakable word so a flex item shrunk
/// to that width wraps each word onto its own line instead of overflowing.
/// Insets are included; margins are handled by the outer size.
///
/// `ignore_percentages` controls how percentage-based sizes are treated. A
/// flex item's intrinsic contribution must be independent of the flex
/// container's width; otherwise a deeply nested descendant declaring, say,
/// `width: 150%` reports a min-content larger than the container and becomes
/// the item's `min-width: auto` base size. Each nesting level then resolves its
/// percentages against that inflated width and the layout grows geometrically.
/// When the flag is set, percentages are treated as indefinite: images fall
/// back to their default intrinsic size, percentage padding resolves to zero,
/// and a box contributes only its text/children intrinsic width.
fn intrinsic_width(
    tree: &LayoutTree,
    index: usize,
    available: f32,
    ignore_percentages: bool,
) -> (f32, f32) {
    let b = &tree.boxes[index];
    let (mut min, mut max) = match b.box_type {
        BoxType::Text => {
            let font = b.style.font_size();
            let min_word = b
                .text
                .split_whitespace()
                .map(|word| browser_text::Font::monospace().measure(word, font))
                .fold(0.0, f32::max);
            (
                min_word,
                browser_text::Font::monospace().measure(&b.text, font),
            )
        }
        BoxType::Image => {
            let w = if ignore_percentages {
                match b.style.width() {
                    // Definite pixel widths are safe to use; a percentage can
                    // only be resolved against a container, so treat it as
                    // indefinite and fall back to the replaced-element default.
                    Dim::Px(px) => px.max(0.0),
                    _ => 150.0,
                }
            } else {
                b.style.width().resolve(available).unwrap_or(150.0)
            };
            (w, w)
        }
        _ => {
            let mut mins = Vec::with_capacity(b.children.len());
            let mut maxs = Vec::with_capacity(b.children.len());
            for &child in &b.children {
                let (cmin, cmax) = intrinsic_width(tree, child, available, ignore_percentages);
                mins.push(cmin);
                maxs.push(cmax);
            }
            let inline_children = b.children.iter().any(|&child| {
                matches!(
                    tree.boxes[child].box_type,
                    BoxType::Text | BoxType::Inline | BoxType::InlineBlock | BoxType::Image
                )
            });
            let min = mins.iter().copied().fold(0.0, f32::max);
            let max = if inline_children {
                maxs.iter().sum()
            } else {
                maxs.iter().copied().fold(0.0, f32::max)
            };
            (min, max)
        }
    };
    let insets = if ignore_percentages {
        // Percentage padding is indefinite during intrinsic sizing, so resolve
        // it against zero rather than against the flex container's width.
        b.style.border_width(Side::Left)
            + b.style.border_width(Side::Right)
            + resolve(b.style.padding(Side::Left), 0.0)
            + resolve(b.style.padding(Side::Right), 0.0)
    } else {
        style_insets_main(&b.style, available)
    };
    min += insets;
    max += insets;
    (min, max)
}

/// Returns `(min-content, max-content)` border-box widths for a flex item.
///
/// Percentages are deliberately treated as indefinite here: an intrinsic
/// contribution must never be derived from the flex container's own width.
fn content_widths(tree: &LayoutTree, index: usize, available: f32) -> (f32, f32) {
    intrinsic_width(tree, index, available, true)
}

/// Shrinks the items of one line so their outer sizes fit `available`.
///
/// The total deficit is distributed in proportion to `flex-shrink × base
/// size`. Items that would drop below their automatic/specified minimum are
/// frozen there and their unused share is redistributed.
fn distribute_shrink(items: &[FlexItem], line: &[usize], deficit: f32) -> Vec<f32> {
    let mut sizes: Vec<f32> = line.iter().map(|&i| items[i].base).collect();
    let mut frozen = vec![false; line.len()];
    let mut remaining = deficit.max(0.0);
    for _ in 0..=line.len() {
        let active: Vec<usize> = (0..line.len())
            .filter(|&k| {
                !frozen[k] && items[line[k]].shrink > 0.0 && sizes[k] > items[line[k]].min + 0.01
            })
            .collect();
        if active.is_empty() || remaining <= 0.0 {
            break;
        }
        let scaled: f32 = active
            .iter()
            .map(|&k| items[line[k]].shrink * sizes[k])
            .sum();
        if scaled <= 0.0 {
            break;
        }
        let mut next = sizes.clone();
        let mut saved = 0.0;
        let mut clamped = false;
        for &k in &active {
            let it = &items[line[k]];
            let reduction = remaining * (it.shrink * sizes[k]) / scaled;
            let capacity = (sizes[k] - it.min).max(0.0);
            if reduction >= capacity {
                saved += reduction - capacity;
                next[k] = it.min;
                frozen[k] = true;
                clamped = true;
            } else {
                next[k] = sizes[k] - reduction;
            }
        }
        sizes = next;
        remaining -= saved;
        if !clamped {
            break;
        }
    }
    sizes
}

/// Grows the items of one line into the positive free space, honouring each
/// item's `max-width` (and redistributing space freed by clamped items).
fn distribute_grow(items: &[FlexItem], line: &[usize], free: f32) -> Vec<f32> {
    let mut sizes: Vec<f32> = line.iter().map(|&i| items[i].base).collect();
    let mut frozen = vec![false; line.len()];
    let mut remaining = free.max(0.0);
    for _ in 0..=line.len() {
        let active: Vec<usize> = (0..line.len())
            .filter(|&k| {
                !frozen[k]
                    && items[line[k]].grow > 0.0
                    && items[line[k]].max.is_none_or(|max| sizes[k] < max - 0.01)
            })
            .collect();
        if active.is_empty() || remaining <= 0.0 {
            break;
        }
        let scaled: f32 = active.iter().map(|&k| items[line[k]].grow * sizes[k]).sum();
        // Per spec, when the scaled flex factors add up to zero (for example
        // empty items with `flex: 1 1 auto` whose base size is zero), the free
        // space is distributed equally instead.
        if scaled <= 0.0 {
            let each = remaining / active.len() as f32;
            for &k in &active {
                let it = &items[line[k]];
                let capacity = it
                    .max
                    .map_or(f32::INFINITY, |max| (max - sizes[k]).max(0.0));
                sizes[k] += each.min(capacity);
            }
            break;
        }
        let mut next = sizes.clone();
        let mut saved = 0.0;
        let mut clamped = false;
        for &k in &active {
            let it = &items[line[k]];
            let extra = remaining * (it.grow * sizes[k]) / scaled;
            let capacity = it
                .max
                .map_or(f32::INFINITY, |max| (max - sizes[k]).max(0.0));
            if extra >= capacity {
                saved += extra - capacity;
                next[k] = it.max.unwrap_or(sizes[k]);
                frozen[k] = true;
                clamped = true;
            } else {
                next[k] = sizes[k] + extra;
            }
        }
        sizes = next;
        remaining -= saved;
        if !clamped {
            break;
        }
    }
    sizes
}

/// Greedily packs items into lines without exceeding `available`.
fn greedy_lines(widths: &[f32], gap: f32, available: f32) -> Vec<Vec<usize>> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    let mut used = 0.0_f32;
    for (index, width) in widths.iter().enumerate() {
        let addition = if current.is_empty() {
            *width
        } else {
            gap + *width
        };
        if !current.is_empty() && used + addition > available + 0.01 {
            lines.push(std::mem::take(&mut current));
            used = *width;
        } else {
            used += addition;
        }
        current.push(index);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn justify_offsets(justify: JustifyContent, count: usize, remaining: f32) -> (f32, f32) {
    match justify {
        JustifyContent::FlexStart => (0.0, 0.0),
        JustifyContent::FlexEnd => (remaining, 0.0),
        JustifyContent::Center => (remaining / 2.0, 0.0),
        JustifyContent::SpaceBetween => (
            0.0,
            if count > 1 {
                remaining / (count - 1) as f32
            } else {
                0.0
            },
        ),
        JustifyContent::SpaceAround => {
            let each = remaining / count.max(1) as f32;
            (each / 2.0, each)
        }
        JustifyContent::SpaceEvenly => {
            let each = remaining / (count + 1) as f32;
            (each, each)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use browser_dom::NodeId;
    use browser_style::{Origin, StyleEngine, Viewport};

    use crate::test_util::layout_grid;
    use crate::{Size, layout};

    /// Builds a chain of nested flex containers, each containing one child with
    /// the given id. Returns the layout tree and the ids of every box in the
    /// chain (outermost container first).
    fn nested_flex_chain(css: &str, levels: usize, leaf_text: &str) -> (LayoutTree, Vec<NodeId>) {
        let mut doc = browser_dom::Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);

        let mut ids = Vec::new();
        let mut parent = body;
        for level in 0..levels {
            let el = doc.create_element("div");
            doc.set_attr(el, "id", &format!("c{level}"));
            doc.append_child(parent, el);
            ids.push(el);
            parent = el;
        }
        let leaf = doc.create_element("div");
        doc.set_attr(leaf, "id", "leaf");
        doc.append_child(parent, leaf);
        if !leaf_text.is_empty() {
            doc.append_text(leaf, leaf_text);
        }
        ids.push(leaf);

        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(css, Origin::Author);
        let styled = engine.compute(&doc);
        let tree = layout(
            &doc,
            &styled,
            Size {
                width: 800.0,
                height: 600.0,
            },
        );
        (tree, ids)
    }

    #[test]
    fn nested_percentage_widths_do_not_inflate_flex_items() {
        // Three nested flex containers, each with a `width: 150%` child. A
        // naive intrinsic measurement resolves each percentage against the
        // flex container and feeds it into `min-width: auto`, inflating the
        // width geometrically at every level (1.5^3 in this example). The
        // outermost item must still fit inside its 200px container.
        let css = "body { margin: 0; } \
                   div { display: flex; height: 20px; font-size: 10px; } \
                   #c0 { width: 200px; } \
                   #c0 > div, #c1 > div, #c2 > div { width: 150%; }";
        let (tree, ids) = nested_flex_chain(css, 3, "a-long-unbreakable-word");
        for (level, id) in ids.iter().enumerate() {
            let b = tree.find_node(*id).unwrap();
            assert!(
                b.rect.width <= 200.0 + 0.5,
                "nested flex item at level {level} inflated to {}px (container is 200px)",
                b.rect.width
            );
        }
    }

    #[test]
    fn greedy_lines_packs_by_available_width() {
        assert_eq!(
            greedy_lines(&[40.0, 40.0, 40.0], 0.0, 100.0),
            vec![vec![0, 1], vec![2]]
        );
        assert_eq!(
            greedy_lines(&[30.0, 30.0, 30.0], 10.0, 100.0),
            vec![vec![0, 1], vec![2]]
        );
    }

    #[test]
    fn justify_offsets_distribute_space() {
        assert_eq!(
            justify_offsets(JustifyContent::Center, 2, 40.0),
            (20.0, 0.0)
        );
        assert_eq!(
            justify_offsets(JustifyContent::SpaceBetween, 2, 40.0),
            (0.0, 40.0)
        );
    }

    #[test]
    fn flex_items_shrink_to_fit_the_container() {
        // Two 80px items in a 100px row cannot both keep their base size;
        // with equal shrink factors each is reduced to 50px.
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; width: 100px; } \
             #g > div { width: 80px; height: 20px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[row.children[0]];
        let b = &tree.boxes[row.children[1]];
        assert!(
            (a.rect.width - 50.0).abs() < 0.5,
            "first item width was {}",
            a.rect.width
        );
        assert!(
            (b.rect.width - 50.0).abs() < 0.5,
            "second item width was {}",
            b.rect.width
        );
        assert!(
            b.rect.x >= a.rect.right() - 0.01,
            "shrunk items must not overlap"
        );
    }

    #[test]
    fn flex_item_text_wraps_at_min_content_width() {
        // A single text item forced to shrink below its max-content width
        // wraps at word boundaries instead of overflowing the row.
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; width: 60px; font-size: 10px; } \
             #g > div { height: 20px; }",
            &[("div", "item", "alpha beta gamma delta epsilon")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let item_box = &tree.boxes[row.children[0]];
        assert!(
            item_box.rect.right() <= row.content.right() + 0.5,
            "text item overflowed its flex container: {} > {}",
            item_box.rect.right(),
            row.content.right()
        );
        // The item's specified height stays 20px, but its content wraps into
        // several lines, which shows up in the scrollable content height.
        assert!(
            item_box.scroll_height > 20.0,
            "item should have wrapped into multiple lines, scroll height {}",
            item_box.scroll_height
        );
    }

    #[test]
    fn wrapped_lines_are_centered_by_align_content() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; flex-wrap: wrap; width: 100px; \
             height: 200px; gap: 10px; align-content: center; } \
             #g > div { width: 60px; height: 20px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[row.children[0]];
        let b = &tree.boxes[row.children[1]];
        assert!(b.rect.y > a.rect.y, "items should wrap onto two lines");
        // Two 20px lines with a 10px gap occupy 50px, leaving 150px of free
        // cross space; centering pushes the first line down by 75px.
        assert!(
            (a.rect.y - 75.0).abs() < 1.0,
            "align-content: center placed the first line at y={}",
            a.rect.y
        );
    }

    #[test]
    fn margin_auto_pushes_item_to_the_right() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; width: 200px; } \
             #a { margin-left: auto; width: 20px; height: 20px; }",
            &[("div", "a", "")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let push = &tree.boxes[row.children[0]];
        assert!(
            (push.rect.right() - 200.0).abs() < 0.5,
            "margin:auto did not push the item to the right edge: {}",
            push.rect.right()
        );
    }

    #[test]
    fn align_items_stretch_fills_the_line_cross_size() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; width: 100px; height: 80px; \
             align-content: flex-start; } \
             #g > div { width: 20px; } #b { height: 40px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let short = &tree.boxes[row.children[0]];
        let tall = &tree.boxes[row.children[1]];
        assert_eq!(tall.rect.height, 40.0);
        assert!(
            (short.rect.height - 40.0).abs() < 0.5,
            "auto-height item should stretch to the tallest sibling, got {}",
            short.rect.height
        );
    }

    #[test]
    fn percentage_flex_basis_resolves_against_the_container() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; width: 200px; } \
             #g > div { flex-basis: 50%; height: 20px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[row.children[0]];
        assert!(
            (a.rect.width - 100.0).abs() < 0.5,
            "50% flex-basis should be 100px, got {}",
            a.rect.width
        );
    }

    #[test]
    fn grow_consumes_free_space_before_justify_content() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; width: 200px; \
             justify-content: center; } #g > div { flex: 1 1 auto; height: 20px; }",
            &[("div", "a", "A"), ("div", "b", "B")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[row.children[0]];
        let b = &tree.boxes[row.children[1]];
        assert!(
            a.rect.x.abs() < 0.5 && (b.rect.right() - 200.0).abs() < 1.0,
            "grown items must fill the row without an extra justify offset: \
             x={} right={}",
            a.rect.x,
            b.rect.right()
        );
    }

    #[test]
    fn flex_grow_with_auto_basis_fills_free_space() {
        // `flex: 1 1 auto` grows items with a content-based base size into the
        // remaining space of the line.
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; width: 200px; } \
             #g > div { flex: 1 1 auto; height: 20px; }",
            &[("div", "a", "A"), ("div", "b", "B")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[row.children[0]];
        let b = &tree.boxes[row.children[1]];
        assert!(
            (a.rect.width + b.rect.width - 200.0).abs() < 1.0,
            "items should grow to fill the row: {} + {}",
            a.rect.width,
            b.rect.width
        );
    }
}
