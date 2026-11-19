//! Scrolling for overflow containers.

use browser_style::Overflow;

use crate::tree::LayoutTree;

/// The maximum scroll offset of a box.
pub fn max_scroll(tree: &LayoutTree, index: usize) -> (f32, f32) {
    let Some(b) = tree.boxes.get(index) else {
        return (0.0, 0.0);
    };
    let max_y = (b.scroll_height - b.content.height).max(0.0);
    (0.0, max_y)
}

/// Sets the scroll offset of a box, clamped to its scrollable range.
pub fn set_scroll(tree: &mut LayoutTree, index: usize, x: f32, y: f32) -> bool {
    let (max_x, max_y) = max_scroll(tree, index);
    let Some(b) = tree.boxes.get_mut(index) else {
        return false;
    };
    if b.clip.is_none() {
        return false;
    }
    b.scroll_offset = (x.clamp(0.0, max_x), y.clamp(0.0, max_y));
    // Keep sticky descendants pinned to the scrollport after the offset move.
    crate::position::update_sticky(tree, index);
    true
}

/// Scrolls by a delta, clamped.
pub fn scroll_by(tree: &mut LayoutTree, index: usize, dx: f32, dy: f32) -> bool {
    let (x, y) = match tree.boxes.get(index) {
        Some(b) => b.scroll_offset,
        None => return false,
    };
    set_scroll(tree, index, x + dx, y + dy)
}

/// The deepest scroll container containing a point (document coordinates).
pub fn scroll_container_at(tree: &LayoutTree, x: f32, y: f32) -> Option<usize> {
    fn walk(tree: &LayoutTree, index: usize, x: f32, y: f32, best: &mut Option<usize>) {
        let Some(b) = tree.boxes.get(index) else {
            return;
        };
        if b.clip.is_some()
            && matches!(
                b.style.overflow_y(),
                Overflow::Scroll | Overflow::Auto | Overflow::Hidden
            )
            && b.contains_point(x, y)
        {
            *best = Some(index);
        }
        for child in &b.children {
            walk(tree, *child, x, y, best);
        }
    }
    let mut best = None;
    if let Some(root) = tree.root {
        walk(tree, root, x, y, &mut best);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::box_model::Rect;
    use crate::tree::{BoxType, LayoutBox};
    use browser_style::ComputedStyle;

    fn scrollable() -> (LayoutTree, usize) {
        let mut tree = LayoutTree::new();
        let mut style = ComputedStyle::initial();
        style.set(
            browser_style::Property::OverflowY,
            browser_style::ComputedValue::keyword("scroll"),
        );
        let index = tree.push(LayoutBox::new(None, BoxType::Block, style));
        let b = &mut tree.boxes[index];
        b.rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        b.content = Rect::new(0.0, 0.0, 100.0, 100.0);
        b.clip = Some(b.rect);
        b.scroll_height = 400.0;
        tree.root = Some(index);
        (tree, index)
    }

    #[test]
    fn clamps_scroll_offsets() {
        let (mut tree, index) = scrollable();
        assert!(set_scroll(&mut tree, index, 0.0, 150.0));
        assert_eq!(tree.boxes[index].scroll_offset, (0.0, 150.0));
        assert!(set_scroll(&mut tree, index, 0.0, 9999.0));
        assert_eq!(tree.boxes[index].scroll_offset, (0.0, 300.0));
        assert!(scroll_by(&mut tree, index, 0.0, -100.0));
        assert_eq!(tree.boxes[index].scroll_offset, (0.0, 200.0));
    }

    #[test]
    fn finds_scroll_container() {
        let (tree, index) = scrollable();
        assert_eq!(scroll_container_at(&tree, 50.0, 50.0), Some(index));
        assert_eq!(scroll_container_at(&tree, 500.0, 50.0), None);
    }
}
