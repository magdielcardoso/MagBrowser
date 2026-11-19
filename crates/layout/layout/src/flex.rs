//! Flexbox layout with single-line and wrapping rows and columns, including
//! `row`, `row-reverse`, `column` and `column-reverse`.
//!
//! The algorithm is written once against an abstract *main* axis (the flex
//! direction) and *cross* axis (perpendicular to it). Rows and columns differ
//! only in which physical axis each logical axis maps to, so intrinsic
//! (min/max-content) sizing, `flex-grow` / `flex-shrink` with `flex-basis` and
//! `min/max` main-size clamping, `align-items` (including `stretch` and an
//! approximate `baseline`) on the cross axis, `justify-content` on the main
//! axis, `align-content` across wrapped lines, main-axis `margin: auto`
//! absorption, and `wrap-reverse` line-order reversal all behave alike.
//! Wrapping packs items greedily into lines along the cross axis. `order` is
//! not implemented yet.

use browser_dom::Document;
use browser_style::{
    AlignItems, BoxSizing, ComputedStyle, ComputedValue, Dim, FlexDirection, FlexWrap,
    JustifyContent, Property, Side,
};

use crate::block::{layout_box, resolve};
use crate::position::translate_subtree;
use crate::tree::{BoxType, LayoutTree};

/// A physical axis. The flex *main* axis is horizontal for rows and vertical
/// for columns; the cross axis is the other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn other(self) -> Self {
        match self {
            Axis::Horizontal => Axis::Vertical,
            Axis::Vertical => Axis::Horizontal,
        }
    }
}

/// Per-item data gathered during the measurement pass and used while the
/// flexible lengths of a line are resolved. All sizes are border-box main
/// sizes; `before`/`after` are main-axis margins and `cross_before` /
/// `cross_after` are cross-axis margins.
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
    /// Main-axis margin at the start edge (`left` for rows, `top` for columns).
    before: f32,
    /// Main-axis margin at the end edge (`right` for rows, `bottom` for columns).
    after: f32,
    before_auto: bool,
    after_auto: bool,
    /// Cross-axis margins.
    cross_before: f32,
    cross_after: f32,
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
    /// Item indices in visual order (reversed for `*-reverse` directions).
    order: Vec<usize>,
    /// Final border-box main sizes, indexed by item index.
    sizes: Vec<f32>,
    /// Final outer cross sizes (border box + margins), indexed by item index.
    cross: Vec<f32>,
    /// Downward cross shift that aligns a baseline item's text baseline with
    /// the line baseline (zero for non-baseline items).
    baseline_shift: Vec<f32>,
    /// Largest outer cross size on the line (including baseline shifts).
    line_cross: f32,
    /// Total used main size of the line (for a column's content height).
    main_used: f32,
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
        "flex-start" | "start" => AlignItems::FlexStart,
        "flex-end" | "end" => AlignItems::FlexEnd,
        "center" => AlignItems::Center,
        "stretch" => AlignItems::Stretch,
        "baseline" => AlignItems::Baseline,
        _ => default,
    }
}

/// Lays out a flex container's children and returns the content size along the
/// physical vertical axis (the container's content height).
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

    let (main_axis, main_reverse) = match direction {
        FlexDirection::Row => (Axis::Horizontal, false),
        FlexDirection::RowReverse => (Axis::Horizontal, true),
        FlexDirection::Column => (Axis::Vertical, false),
        FlexDirection::ColumnReverse => (Axis::Vertical, true),
    };
    let (main_gap, cross_gap) = match main_axis {
        Axis::Horizontal => (column_gap, row_gap),
        Axis::Vertical => (row_gap, column_gap),
    };
    // `wrap-reverse` reverses the order of cross-axis lines (the main-axis
    // order is unchanged, unlike `column-reverse`).
    let cross_reverse = style.flex_wrap() == FlexWrap::WrapReverse;

    layout_flex_generic(
        tree,
        doc,
        &style,
        content,
        main_axis,
        main_reverse,
        cross_reverse,
        &children,
        justify,
        align,
        main_gap,
        cross_gap,
    )
}

#[allow(clippy::too_many_arguments)]
fn layout_flex_generic(
    tree: &mut LayoutTree,
    doc: &Document,
    style: &ComputedStyle,
    content: crate::Rect,
    main_axis: Axis,
    main_reverse: bool,
    cross_reverse: bool,
    children: &[usize],
    justify: JustifyContent,
    align: AlignItems,
    main_gap: f32,
    cross_gap: f32,
) -> f32 {
    let cross_axis = main_axis.other();
    let border_box = style.box_sizing() == BoxSizing::BorderBox;
    // Width is always definite in this top-down layout; percentages on any
    // axis resolve against it (height percentages are deliberately treated as
    // indefinite, mirroring block layout).
    let inline_available = content.width.max(0.0);

    let main_insets = insets_for_axis(style, main_axis, inline_available);
    let main_available = match main_axis {
        Axis::Horizontal => Some(content.width.max(0.0)),
        Axis::Vertical => definite_main_size(style, inline_available, main_insets),
    };
    let available = main_available.unwrap_or(0.0);
    // A column with `height: auto` has no definite main size: there is no free
    // space to grow or shrink into, so items keep their base sizes.
    let indefinite_main = main_available.is_none();
    let container_cross = match main_axis {
        Axis::Horizontal => definite_container_cross(
            style,
            inline_available,
            insets_for_axis(style, Axis::Vertical, inline_available),
        ),
        Axis::Vertical => content.width.max(0.0),
    };
    // Percentage main sizes resolve against the container's main size. For
    // rows that is the (always definite) content width; for columns it is the
    // definite height, or zero when indefinite. Percentages of margins and
    // padding still resolve against the containing-block width below.
    let main_percent_base = match main_axis {
        Axis::Horizontal => inline_available,
        Axis::Vertical => available,
    };

    // --- Measurement pass: flex base sizes and intrinsic minima -------------
    let mut items: Vec<FlexItem> = Vec::with_capacity(children.len());
    for &child in children {
        if !crate::budget::try_step() {
            break;
        }
        let cs = tree.boxes[child].style.clone();
        let insets_main = insets_for_axis(&cs, main_axis, inline_available);
        let (min_content, max_content) = match main_axis {
            Axis::Horizontal => content_widths(tree, child, inline_available),
            Axis::Vertical => content_heights(tree, child, inline_available),
        };
        let (basis_dim, size_dim, min_prop, max_prop) = match main_axis {
            Axis::Horizontal => (
                cs.flex_basis(),
                cs.width(),
                Property::MinWidth,
                Property::MaxWidth,
            ),
            Axis::Vertical => (
                cs.flex_basis(),
                cs.height(),
                Property::MinHeight,
                Property::MaxHeight,
            ),
        };

        let base = if !basis_dim.is_auto() {
            definite_border_size(
                resolve(basis_dim, main_percent_base),
                border_box,
                insets_main,
            )
        } else if !size_dim.is_auto() {
            definite_border_size(
                resolve(size_dim, main_percent_base),
                border_box,
                insets_main,
            )
        } else {
            max_content
        };

        let min_main = match cs.dim(min_prop) {
            // `min-*: auto` on a flex item is its content-based minimum: the
            // min-content size, i.e. the longest unbreakable word on the main
            // axis. Cap it at the container's available main size so a nested
            // child can never demand (and geometrically inflate to) more than
            // the container itself. With an indefinite main size there is no
            // cap to apply.
            Dim::Auto => {
                if indefinite_main {
                    min_content.max(0.0)
                } else {
                    min_content.min(available).max(0.0)
                }
            }
            dim => definite_border_size(resolve(dim, main_percent_base), border_box, insets_main),
        };
        let max_main = match cs.dim(max_prop) {
            Dim::Auto => None,
            dim => Some(definite_border_size(
                resolve(dim, main_percent_base),
                border_box,
                insets_main,
            )),
        };
        let base = base
            .max(min_main)
            .min(max_main.unwrap_or(f32::INFINITY))
            .max(0.0);

        let (before_dim, after_dim, cross_before_dim, cross_after_dim) = match main_axis {
            Axis::Horizontal => (
                cs.margin(Side::Left),
                cs.margin(Side::Right),
                cs.margin(Side::Top),
                cs.margin(Side::Bottom),
            ),
            Axis::Vertical => (
                cs.margin(Side::Top),
                cs.margin(Side::Bottom),
                cs.margin(Side::Left),
                cs.margin(Side::Right),
            ),
        };
        let cross_auto = match cross_axis {
            Axis::Horizontal => cs.width().is_auto(),
            Axis::Vertical => cs.height().is_auto(),
        };
        items.push(FlexItem {
            child,
            grow: cs.flex_grow().max(0.0),
            shrink: cs.flex_shrink().max(0.0),
            base,
            min: min_main.min(base).max(0.0),
            max: max_main,
            before: resolve(before_dim, inline_available),
            after: resolve(after_dim, inline_available),
            before_auto: before_dim.is_auto(),
            after_auto: after_dim.is_auto(),
            cross_before: resolve(cross_before_dim, inline_available),
            cross_after: resolve(cross_after_dim, inline_available),
            insets_main,
            border_box,
            cross_auto,
            align: item_align_of(&cs, align),
        });
    }
    let count = items.len();
    if count == 0 {
        return 0.0;
    }

    // Lay out every item at its hypothetical main size first so cross-axis
    // measurement sees real geometry.
    for &it in &items {
        if !crate::budget::try_step() {
            break;
        }
        layout_item(
            tree,
            doc,
            it.child,
            inline_available,
            it.insets_main,
            it.border_box,
            it.base,
            main_axis,
        );
    }

    let wraps =
        matches!(style.flex_wrap(), FlexWrap::Wrap | FlexWrap::WrapReverse) && !indefinite_main;
    let outers: Vec<f32> = items
        .iter()
        .map(|it| it.base + it.before + it.after)
        .collect();
    let mut lines: Vec<Vec<usize>> = if wraps {
        greedy_lines(&outers, main_gap, available)
    } else {
        vec![(0..count).collect()]
    };
    if cross_reverse {
        lines.reverse();
    }

    // --- Phase 1: resolve each line's flexible lengths and cross sizes ------
    let mut prepared: Vec<PreparedLine> = Vec::with_capacity(lines.len());
    for line in &lines {
        if !crate::budget::try_step() {
            break;
        }
        let line_used: f32 = line.iter().map(|&i| outers[i]).sum::<f32>()
            + main_gap * line.len().saturating_sub(1) as f32;
        // With an indefinite main size there is no free space to distribute.
        let mut free = if indefinite_main {
            0.0
        } else {
            available - line_used
        };

        // `margin: auto` on the main axis absorbs positive free space before
        // flex-grow and justify-content are considered.
        let auto_margins: usize = line
            .iter()
            .map(|&i| usize::from(items[i].before_auto) + usize::from(items[i].after_auto))
            .sum();
        if free > 0.0 && auto_margins > 0 {
            let each = free / auto_margins as f32;
            for &i in line {
                if items[i].before_auto {
                    items[i].before += each;
                }
                if items[i].after_auto {
                    items[i].after += each;
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
        let main_used: f32 = resolved.iter().sum::<f32>()
            + line
                .iter()
                .map(|&i| items[i].before + items[i].after)
                .sum::<f32>()
            + main_gap * line.len().saturating_sub(1) as f32;

        // Re-layout items whose main size changed so text wraps at the final
        // size instead of overflowing at its hypothetical size.
        for &i in line {
            if !crate::budget::try_step() {
                break;
            }
            if (sizes[i] - items[i].base).abs() > 0.01 {
                layout_item(
                    tree,
                    doc,
                    items[i].child,
                    inline_available,
                    items[i].insets_main,
                    items[i].border_box,
                    sizes[i],
                    main_axis,
                );
            }
        }

        // An item with a preferred aspect ratio takes its cross size from its
        // FINAL main size (after shrink/grow), not from an inflated intrinsic
        // min-content measurement. `layout_box` already derives this from the
        // pinned main size, but re-assert it here so a nested percentage
        // expansion cannot leak a larger min-content back into the cross axis.
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
            let (insets_cross, mut content_cross) = {
                let b = &tree.boxes[child];
                let insets = match cross_axis {
                    Axis::Horizontal => (b.rect.width - b.content.width).max(0.0),
                    Axis::Vertical => (b.rect.height - b.content.height).max(0.0),
                };
                let value = if it.border_box {
                    match cross_axis {
                        // cross = height = width / ratio (rows)
                        Axis::Vertical => (sizes[i] / ratio - insets).max(0.0),
                        // cross = width = height * ratio (columns)
                        Axis::Horizontal => (sizes[i] * ratio - insets).max(0.0),
                    }
                } else {
                    match cross_axis {
                        Axis::Vertical => ((sizes[i] - it.insets_main) / ratio).max(0.0),
                        Axis::Horizontal => ((sizes[i] - it.insets_main) * ratio).max(0.0),
                    }
                };
                (insets, value)
            };
            let (min_prop, max_prop) = match cross_axis {
                Axis::Horizontal => (Property::MinWidth, Property::MaxWidth),
                Axis::Vertical => (Property::MinHeight, Property::MaxHeight),
            };
            if let Some(min) = cs.dim(min_prop).resolve(inline_available) {
                content_cross = content_cross.max(min);
            }
            if let Some(max) = cs.dim(max_prop).resolve(inline_available) {
                content_cross = content_cross.min(max);
            }
            let b = &mut tree.boxes[child];
            match cross_axis {
                Axis::Horizontal => {
                    b.content.width = content_cross;
                    b.rect.width = content_cross + insets_cross;
                }
                Axis::Vertical => {
                    b.content.height = content_cross;
                    b.rect.height = content_cross + insets_cross;
                }
            }
        }

        let mut cross = vec![0.0_f32; count];
        let mut baseline_shift = vec![0.0_f32; count];
        let mut line_cross = 0.0_f32;
        for &i in line {
            let b = &tree.boxes[items[i].child];
            let size = match cross_axis {
                Axis::Horizontal => b.rect.width,
                Axis::Vertical => b.rect.height,
            };
            cross[i] = size + items[i].cross_before + items[i].cross_after;
            line_cross = line_cross.max(cross[i]);
        }

        // Baseline alignment is meaningful when the cross axis is vertical
        // (rows). Use the first text fragment's absolute baseline and line up
        // all baseline items on the largest distance-from-cross-start.
        if main_axis == Axis::Horizontal {
            let mut baselines = vec![None; count];
            for &i in line {
                if items[i].align != AlignItems::Baseline {
                    continue;
                }
                let child = items[i].child;
                if let Some(baseline) = first_text_baseline(tree, child) {
                    let top = tree.boxes[child].rect.y;
                    baselines[i] = Some(baseline - top);
                }
            }
            let line_baseline = line
                .iter()
                .filter(|&&i| baselines[i].is_some())
                .map(|&i| baselines[i].unwrap() + items[i].cross_before)
                .fold(0.0, f32::max);
            for &i in line {
                if let Some(dist) = baselines[i] {
                    let shift = (line_baseline - (dist + items[i].cross_before)).max(0.0);
                    baseline_shift[i] = shift;
                    line_cross = line_cross.max(cross[i] + shift);
                }
            }
        }

        // Justify only the space left after flexible lengths and auto margins
        // have been resolved; this is zero when items grew to fill the line.
        let used_after: f32 = line
            .iter()
            .map(|&i| sizes[i] + items[i].before + items[i].after)
            .sum::<f32>()
            + main_gap * line.len().saturating_sub(1) as f32;
        let remaining = if indefinite_main {
            0.0
        } else {
            (available - used_after).max(0.0)
        };
        let (start, between) = justify_offsets(justify, line.len(), remaining);
        let mut order = line.clone();
        if main_reverse {
            order.reverse();
        }
        prepared.push(PreparedLine {
            order,
            sizes,
            cross,
            baseline_shift,
            line_cross,
            main_used,
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
        + cross_gap * line_count.saturating_sub(1) as f32;
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

    let cross_origin = match main_axis {
        Axis::Horizontal => content.y,
        Axis::Vertical => content.x,
    };
    let main_origin = match main_axis {
        Axis::Horizontal => content.x,
        Axis::Vertical => content.y,
    };
    let mut line_cross_pos = cross_origin + cross_start;
    let mut total_cross = 0.0_f32;
    let mut total_main = 0.0_f32;
    for (line_index, line) in prepared.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let effective_cross = line.line_cross + cross_extra;
        let mut cursor = main_origin + line.start;
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
                // For columns the baseline is not meaningful; treat it as the
                // cross-start edge, like `flex-start`.
                AlignItems::Baseline => {
                    if main_axis == Axis::Horizontal {
                        line.baseline_shift[i]
                    } else {
                        0.0
                    }
                }
                AlignItems::FlexStart | AlignItems::Stretch => 0.0,
            };
            let cross_pos = line_cross_pos + cross_offset + it.cross_before;
            let main_pos = cursor + it.before;
            let (x, y) = match main_axis {
                Axis::Horizontal => (main_pos, cross_pos),
                Axis::Vertical => (cross_pos, main_pos),
            };
            {
                let b = &mut tree.boxes[child];
                // `layout_box` resolves `auto` margins to zero; publish the
                // final (possibly auto-absorbed) margins used by hit testing.
                match main_axis {
                    Axis::Horizontal => {
                        b.margins.left = it.before;
                        b.margins.right = it.after;
                        b.margins.top = it.cross_before;
                        b.margins.bottom = it.cross_after;
                    }
                    Axis::Vertical => {
                        b.margins.top = it.before;
                        b.margins.bottom = it.after;
                        b.margins.left = it.cross_before;
                        b.margins.right = it.cross_after;
                    }
                }
            }
            translate_subtree(tree, child, x - current.x, y - current.y);

            // `align-items: stretch` (the default) fills the line's cross size
            // for items whose cross size is auto.
            if it.align == AlignItems::Stretch && it.cross_auto {
                let b = &mut tree.boxes[child];
                let insets_cross = match cross_axis {
                    Axis::Horizontal => b.borders.horizontal() + b.paddings.horizontal(),
                    Axis::Vertical => b.borders.vertical() + b.paddings.vertical(),
                };
                let inner =
                    (effective_cross - it.cross_before - it.cross_after - insets_cross).max(0.0);
                match cross_axis {
                    Axis::Horizontal => {
                        b.content.width = inner;
                        b.rect.width = inner + insets_cross;
                    }
                    Axis::Vertical => {
                        b.content.height = inner;
                        b.rect.height = inner + insets_cross;
                    }
                }
            }

            cursor += line.sizes[i] + it.before + it.after + main_gap + line.between;
        }
        total_cross += effective_cross;
        total_main = total_main.max(line.main_used);
        if line_index + 1 < line_count {
            total_cross += cross_gap + cross_between;
            line_cross_pos += effective_cross + cross_gap + cross_between;
        }
    }

    match main_axis {
        Axis::Horizontal => total_cross.max(style.line_height_px()),
        Axis::Vertical => total_main.max(style.line_height_px()),
    }
}

/// Turns a resolved `width`/`flex-basis`/`height` (content box under
/// `content-box` sizing, border box under `border-box` sizing) into a
/// border-box main size.
fn definite_border_size(resolved: f32, border_box: bool, insets: f32) -> f32 {
    if border_box {
        resolved.max(0.0)
    } else {
        (resolved + insets).max(0.0)
    }
}

/// Border + padding of `style` on the given axis. Percentage padding resolves
/// against `available` (the containing-block width) on either axis.
fn insets_for_axis(style: &ComputedStyle, axis: Axis, available: f32) -> f32 {
    match axis {
        Axis::Horizontal => {
            style.border_width(Side::Left)
                + style.border_width(Side::Right)
                + resolve(style.padding(Side::Left), available)
                + resolve(style.padding(Side::Right), available)
        }
        Axis::Vertical => {
            style.border_width(Side::Top)
                + style.border_width(Side::Bottom)
                + resolve(style.padding(Side::Top), available)
                + resolve(style.padding(Side::Bottom), available)
        }
    }
}

/// The container's definite content height, or `None` when height is auto (or
/// a percentage, which has no definite base in this top-down layout).
fn definite_main_size(style: &ComputedStyle, available: f32, insets: f32) -> Option<f32> {
    match style.height() {
        Dim::Auto | Dim::Percent(_) => None,
        Dim::Calc { percent, .. } if percent != 0.0 => None,
        dim => Some(definite_border_size(
            resolve(dim, available),
            style.box_sizing() == BoxSizing::BorderBox,
            insets,
        )),
    }
}

/// The container's definite content size on the cross axis (vertical for
/// rows), or zero when height is auto.
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

/// Lays out one item at a requested border-box main size.
///
/// `layout_box` honours a specified `width`/`height` over the size hint passed
/// to it, so a definite `flex-basis` (or a shrunk/grown size) is applied by
/// temporarily pinning the main-axis size. Percentage padding and margins
/// therefore still resolve against the flex container, and text is line-broken
/// at the final main size.
#[allow(clippy::too_many_arguments)]
fn layout_item(
    tree: &mut LayoutTree,
    doc: &Document,
    child: usize,
    available: f32,
    insets: f32,
    border_box: bool,
    main_border: f32,
    main_axis: Axis,
) {
    let prop = match main_axis {
        Axis::Horizontal => Property::Width,
        Axis::Vertical => Property::Height,
    };
    let original = tree.boxes[child]
        .style
        .get(prop)
        .cloned()
        .unwrap_or(ComputedValue::Dim(Dim::Auto));
    let size = if border_box {
        main_border.max(0.0)
    } else {
        (main_border - insets).max(0.0)
    };
    tree.boxes[child]
        .style
        .set(prop, ComputedValue::Dim(Dim::Px(size)));
    let _ = layout_box(tree, doc, child, 0.0, 0.0, available.max(1.0));
    tree.boxes[child].style.set(prop, original);
}

/// The absolute y of the first text fragment baseline in the subtree rooted at
/// `index`, or `None` when the subtree paints no text.
fn first_text_baseline(tree: &LayoutTree, index: usize) -> Option<f32> {
    let b = &tree.boxes[index];
    if let Some(fragment) = b.lines.iter().find_map(|line| line.fragments.first()) {
        return Some(fragment.baseline);
    }
    for &child in &b.children {
        if let Some(y) = first_text_baseline(tree, child) {
            return Some(y);
        }
    }
    None
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
        b.style.border_width(Side::Left)
            + b.style.border_width(Side::Right)
            + resolve(b.style.padding(Side::Left), 0.0)
            + resolve(b.style.padding(Side::Right), 0.0)
    } else {
        insets_for_axis(&b.style, Axis::Horizontal, available)
    };
    min += insets;
    max += insets;
    (min, max)
}

/// Returns `(min-content, max-content)` border-box widths for a flex item.
/// Percentages are deliberately treated as indefinite here.
fn content_widths(tree: &LayoutTree, index: usize, available: f32) -> (f32, f32) {
    intrinsic_width(tree, index, available, true)
}

/// Approximate intrinsic heights for column items: a text run occupies one
/// line, images use their declared height (or a replaced-element default),
/// and containers combine their children. Percentages are treated as
/// indefinite so an intrinsic contribution never depends on the container.
fn intrinsic_height(
    tree: &LayoutTree,
    index: usize,
    available: f32,
    ignore_percentages: bool,
) -> (f32, f32) {
    let b = &tree.boxes[index];
    let (mut min, mut max) = match b.box_type {
        BoxType::Text => {
            let line = b.style.line_height_px();
            (line, line)
        }
        BoxType::Image => {
            let h = if ignore_percentages {
                match b.style.height() {
                    Dim::Px(px) => px.max(0.0),
                    _ => 150.0,
                }
            } else {
                b.style.height().resolve(available).unwrap_or(150.0)
            };
            (h, h)
        }
        _ => {
            let mut mins = Vec::with_capacity(b.children.len());
            let mut maxs = Vec::with_capacity(b.children.len());
            for &child in &b.children {
                let (cmin, cmax) = intrinsic_height(tree, child, available, ignore_percentages);
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
    let pad = if ignore_percentages { 0.0 } else { available };
    min += b.style.border_width(Side::Top)
        + b.style.border_width(Side::Bottom)
        + resolve(b.style.padding(Side::Top), pad)
        + resolve(b.style.padding(Side::Bottom), pad);
    max += b.style.border_width(Side::Top)
        + b.style.border_width(Side::Bottom)
        + resolve(b.style.padding(Side::Top), pad)
        + resolve(b.style.padding(Side::Bottom), pad);
    (min, max)
}

fn content_heights(tree: &LayoutTree, index: usize, available: f32) -> (f32, f32) {
    intrinsic_height(tree, index, available, true)
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
/// item's `max` main size (and redistributing space freed by clamped items).
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

    // --- Column direction ---------------------------------------------------

    #[test]
    fn column_grow_fills_the_container_height() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; flex-direction: column; height: 90px; } \
             #g > div { flex: 1 1 auto; }",
            &[("div", "a", "A"), ("div", "b", "B")],
        );
        let col = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[col.children[0]];
        let b = &tree.boxes[col.children[1]];
        assert!(
            (a.rect.height + b.rect.height - 90.0).abs() < 1.0,
            "column items should grow to fill the height: {} + {}",
            a.rect.height,
            b.rect.height
        );
        assert!(
            (a.rect.height - b.rect.height).abs() < 1.0,
            "equal grow factors should yield equal heights"
        );
        assert!(
            b.rect.y >= a.rect.bottom() - 0.01,
            "grown column items must not overlap"
        );
    }

    #[test]
    fn column_shrink_and_min_max_main_size() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; flex-direction: column; height: 60px; } \
             #g > div { height: 50px; } #a { min-height: 10px; } #b { max-height: 20px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let col = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[col.children[0]];
        let b = &tree.boxes[col.children[1]];
        assert!(
            b.rect.height <= 20.5,
            "max-height not honoured: {}",
            b.rect.height
        );
        assert!(
            a.rect.height >= 9.5,
            "min-height not honoured: {}",
            a.rect.height
        );
        assert!(
            a.rect.height + b.rect.height <= 60.5,
            "column items should shrink to fit the container"
        );
    }

    #[test]
    fn column_align_items_center_on_the_cross_axis() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; flex-direction: column; width: 100px; \
             align-items: center; } #g > div { width: 20px; height: 10px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let col = tree.find_node(ids[1]).unwrap();
        for &child in &col.children {
            let b = &tree.boxes[child];
            assert!(
                (b.rect.x - 40.0).abs() < 0.5,
                "centered column item should start at x=40, got {}",
                b.rect.x
            );
        }
    }

    #[test]
    fn column_reverse_reverses_main_order() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; flex-direction: column-reverse; height: 40px; } \
             #g > div { height: 20px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let col = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[col.children[0]];
        let b = &tree.boxes[col.children[1]];
        assert!(
            b.rect.y < a.rect.y,
            "column-reverse must place the first DOM item below the second"
        );
    }

    // --- Wrapping and alignment ---------------------------------------------

    #[test]
    fn wrap_reverse_reverses_line_order() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; flex-wrap: wrap-reverse; width: 100px; } \
             #g > div { width: 60px; height: 20px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[row.children[0]];
        let b = &tree.boxes[row.children[1]];
        assert!(
            a.rect.y > b.rect.y,
            "items should still wrap onto two lines"
        );
        // With `wrap` the first item (a) would be on the first line at the
        // top; `wrap-reverse` reverses line order so b is topmost.
        assert!(
            b.rect.y + b.rect.height <= a.rect.y + 0.5,
            "wrap-reverse should put the second line above the first: a.y={} b.y={}",
            a.rect.y,
            b.rect.y
        );
    }

    #[test]
    fn align_content_space_between_column() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; flex-direction: column; width: 300px; \
             flex-wrap: wrap; height: 100px; align-content: space-between; } \
             #g > div { height: 40px; width: 50px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let col = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[col.children[0]];
        // Two 40px lines with a 100px cross extent leave 20px of free cross
        // space; space-between keeps the first line at the cross start.
        assert!(
            (a.rect.x - col.content.x).abs() < 0.5,
            "first column line should start at the cross origin"
        );
    }

    // --- Baseline ------------------------------------------------------------

    #[test]
    fn align_items_baseline_lines_up_text() {
        let (_doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: flex; align-items: baseline; font-size: 20px; } \
             #a { height: 20px; font-size: 20px; } \
             #b { height: 60px; font-size: 20px; padding-top: 20px; }",
            &[("div", "a", "Aa"), ("div", "b", "Bb")],
        );
        let row = tree.find_node(ids[1]).unwrap();
        let a = &tree.boxes[row.children[0]];
        let b = &tree.boxes[row.children[1]];
        let baseline_a = a.lines[0].fragments[0].baseline;
        let baseline_b = b.lines[0].fragments[0].baseline;
        assert!(
            (baseline_a - baseline_b).abs() < 1.0,
            "baseline items should share a text baseline: {} vs {}",
            baseline_a,
            baseline_b
        );
        // The item whose text starts lower (due to padding) stays put while
        // the other shifts down so their baselines line up.
        assert!(a.rect.y > b.rect.y);
    }
}
