//! Basic flexbox layout (single line, `row`, `row-reverse`, `column`).
//!
//! This is a deliberately small implementation of the flexbox algorithm: no
//! wrapping, no `order`, no `align-content`, no baseline alignment. It is enough
//! for toolbars, navigation bars and card rows, and it is documented as a
//! limitation in `docs/roadmap.md`.

use browser_dom::Document;
use browser_style::{AlignItems, FlexDirection, JustifyContent, Side};

use crate::block::{layout_box, resolve, shrink_to_fit};
use crate::tree::LayoutTree;

/// Lays out a flex container's children and returns the content height.
pub fn layout_flex(tree: &mut LayoutTree, doc: &Document, index: usize) -> f32 {
    let style = tree.boxes[index].style.clone();
    let content = tree.boxes[index].content;
    let direction = style.flex_direction();
    let justify = style.justify_content();
    let align = style.align_items();
    let row_gap = style.row_gap();
    let column_gap = style.column_gap();
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
        FlexDirection::Row | FlexDirection::RowReverse => {
            let gap = column_gap;
            let available = content.width;
            let count = children.len();
            let gaps = gap * (count.saturating_sub(1)) as f32;
            let flexible_space = (available - gaps).max(0.0);
            let _ = flexible_space;

            let mut widths: Vec<f32> = Vec::with_capacity(count);
            let mut heights: Vec<f32> = Vec::with_capacity(count);
            let mut grow: Vec<f32> = Vec::with_capacity(count);

            for child in &children {
                let child_style = tree.boxes[*child].style.clone();
                let auto_width =
                    child_style.width().is_auto() && child_style.flex_basis().is_auto();
                let max_content = if auto_width {
                    shrink_to_fit(tree, doc, *child, available).max(1.0)
                } else {
                    0.0
                };
                let width_hint = if child_style.flex_basis().is_auto() {
                    if auto_width {
                        max_content
                    } else {
                        resolve(child_style.width(), available)
                    }
                } else {
                    resolve(child_style.flex_basis(), available)
                };
                layout_box(tree, doc, *child, 0.0, 0.0, width_hint.max(1.0));
                if auto_width {
                    let b = &mut tree.boxes[*child];
                    let extra = b.borders.horizontal() + b.paddings.horizontal();
                    b.content.width = max_content;
                    b.rect.width = max_content + extra;
                }
                let b = &tree.boxes[*child];
                widths.push(b.rect.width + b.margins.horizontal());
                heights.push(b.rect.height + b.margins.vertical());
                grow.push(child_style.flex_grow());
            }

            let used: f32 = widths.iter().sum::<f32>() + gaps;
            let free = (available - used).max(0.0);
            let total_grow: f32 = grow.iter().sum();
            let mut extra: Vec<f32> = vec![0.0; count];
            if total_grow > 0.0 {
                for (i, g) in grow.iter().enumerate() {
                    extra[i] = free * (g / total_grow);
                }
            }

            let tallest = heights.iter().copied().fold(0.0_f32, f32::max);
            let mut cursor = content.x;
            let remaining = (available - (used + extra.iter().sum::<f32>())).max(0.0);
            let (start_offset, between) = match justify {
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
                    let each = remaining / count as f32;
                    (each / 2.0, each)
                }
                JustifyContent::SpaceEvenly => {
                    let each = remaining / (count + 1) as f32;
                    (each, each)
                }
            };
            cursor += start_offset;

            let order: Vec<usize> = if direction == FlexDirection::RowReverse {
                (0..count).rev().collect()
            } else {
                (0..count).collect()
            };

            for (position, i) in order.iter().enumerate() {
                let child = children[*i];
                if extra[*i] > 0.0 {
                    let target = widths[*i] - tree.boxes[child].margins.horizontal() + extra[*i];
                    layout_box(tree, doc, child, 0.0, 0.0, target.max(1.0));
                    let b = &mut tree.boxes[child];
                    let padding_border = b.borders.horizontal() + b.paddings.horizontal();
                    b.content.width = (target - padding_border).max(0.0);
                    b.rect.width = b.content.width + padding_border;
                }
                let b = &tree.boxes[child];
                let mt = b.margins.top;
                let child_height = b.rect.height;
                let y = match align {
                    AlignItems::Center => content.y + (tallest - child_height - mt).max(0.0) / 2.0,
                    AlignItems::FlexEnd => content.y + (tallest - child_height - mt).max(0.0),
                    _ => content.y,
                };
                let current = tree.boxes[child].rect;
                crate::position::translate_subtree(
                    tree,
                    child,
                    cursor + b.margins.left - current.x,
                    y + mt - current.y,
                );
                cursor += widths[*i] + gap + between;
                if position + 1 == order.len() {
                    // trailing gap is not part of the content box
                }
            }
            tallest.max(style.line_height_px())
        }
        FlexDirection::Column | FlexDirection::ColumnReverse => {
            let gap = row_gap;
            let available = content.width;
            let mut cursor = content.y;
            let mut order: Vec<usize> = (0..children.len()).collect();
            if direction == FlexDirection::ColumnReverse {
                order.reverse();
            }
            for child in &order {
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
