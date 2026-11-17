//! Out-of-flow positioning and subtree translation.

use crate::box_model::Rect;
use crate::tree::LayoutTree;

/// Translates a box and all of its descendants by `(dx, dy)`.
pub fn translate_subtree(tree: &mut LayoutTree, index: usize, dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let children = tree.boxes[index].children.clone();
    {
        let b = &mut tree.boxes[index];
        b.rect = b.rect.translate(dx, dy);
        b.content = b.content.translate(dx, dy);
        if let Some(clip) = b.clip {
            b.clip = Some(clip.translate(dx, dy));
        }
        for line in &mut b.lines {
            line.rect = line.rect.translate(dx, dy);
            for fragment in &mut line.fragments {
                fragment.x += dx;
                fragment.baseline += dy;
            }
            for atom in &mut line.atoms {
                atom.rect = atom.rect.translate(dx, dy);
            }
        }
    }
    for child in children {
        translate_subtree(tree, child, dx, dy);
    }
}

/// Replaces a box's border box rectangle, keeping content offsets consistent.
pub fn set_border_box(tree: &mut LayoutTree, index: usize, rect: Rect) {
    let b = &mut tree.boxes[index];
    let dx = rect.x - b.rect.x;
    let dy = rect.y - b.rect.y;
    let new_rect = rect;
    b.rect = new_rect;
    b.content = Rect::new(
        b.content.x + dx,
        b.content.y + dy,
        b.content.width,
        b.content.height,
    );
    b.content.height = (new_rect.height - b.borders.vertical() - b.paddings.vertical()).max(0.0);
    b.content.width = (new_rect.width - b.borders.horizontal() - b.paddings.horizontal()).max(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{BoxType, LayoutBox};
    use browser_style::ComputedStyle;

    fn tree_with_child() -> (LayoutTree, usize, usize) {
        let mut tree = LayoutTree::new();
        let root = tree.push(LayoutBox::new(
            None,
            BoxType::Block,
            ComputedStyle::initial(),
        ));
        let child = tree.push(LayoutBox::new(
            None,
            BoxType::Block,
            ComputedStyle::initial(),
        ));
        tree.boxes[root].children = vec![child];
        tree.boxes[root].rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[child].rect = Rect::new(10.0, 10.0, 50.0, 20.0);
        tree.root = Some(root);
        (tree, root, child)
    }

    #[test]
    fn translation_moves_descendants() {
        let (mut tree, root, child) = tree_with_child();
        translate_subtree(&mut tree, root, 5.0, 7.0);
        assert_eq!(tree.boxes[root].rect.x, 5.0);
        assert_eq!(tree.boxes[child].rect.x, 15.0);
        assert_eq!(tree.boxes[child].rect.y, 17.0);
    }

    #[test]
    fn set_border_box_keeps_content_inside() {
        let (mut tree, _root, child) = tree_with_child();
        set_border_box(&mut tree, child, Rect::new(20.0, 30.0, 40.0, 30.0));
        assert_eq!(tree.boxes[child].content.width, 40.0);
        assert_eq!(tree.boxes[child].content.height, 30.0);
    }
}
