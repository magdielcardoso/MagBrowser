//! Out-of-flow positioning and subtree translation.

use browser_style::{Dim, Position, Property};

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
        // Accessibility-only content is commonly parked off-screen with
        // `position:absolute; left:-9999px`. Such a box and its descendants
        // must never contribute paint: give it an empty clip so the painter
        // discards everything inside it even if a glyph rasterizer resolves a
        // negative pen position into the visible part of the canvas.
        let off_left = b.rect.right() <= 0.0;
        let is_out_of_flow = matches!(b.style.position(), Position::Absolute | Position::Fixed);
        if off_left && is_out_of_flow {
            b.clip = Some(Rect::ZERO);
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

/// Resolves an inset, returning `None` for `auto` (no stickiness on that side).
fn resolve_inset(dim: Dim, base: f32) -> Option<f32> {
    match dim {
        Dim::Auto => None,
        dim => Some(crate::block::resolve(dim, base)),
    }
}

/// Re-positions the sticky descendants of a scroll container after its scroll
/// offset changes.
///
/// All four insets (`top`, `bottom`, `left`, `right`) are supported. The shift
/// is recomputed from each box's recorded static position on every update, so
/// repeated scrolling never accumulates error and the result is deterministic.
/// A sticky box is additionally clamped to its containing block so it never
/// escapes its parent (the scroll container's own clip bounds boxes whose
/// parent is the scroller itself). Boxes in nested scroll containers are left
/// to the container that actually scrolls them.
pub fn update_sticky(tree: &mut LayoutTree, container: usize) {
    fn walk(
        tree: &mut LayoutTree,
        index: usize,
        parent: Option<usize>,
        scroller: usize,
        container: usize,
    ) {
        let Some(b) = tree.boxes.get(index) else {
            return;
        };
        let is_scroller = b.clip.is_some();
        let is_sticky = b.style.position() == Position::Sticky;
        let child_scroller = if is_scroller && index != container {
            index
        } else {
            scroller
        };

        if is_sticky && index != container && scroller == container {
            apply_sticky(tree, index, parent, container);
        }

        let children = tree.boxes[index].children.clone();
        for child in children {
            walk(tree, child, Some(index), child_scroller, container);
        }
    }

    if tree.boxes.get(container).is_some() {
        walk(tree, container, None, container, container);
    }
}

/// Computes and applies the sticky shift for one box.
fn apply_sticky(tree: &mut LayoutTree, index: usize, parent: Option<usize>, container: usize) {
    let scrollport = tree.boxes[container]
        .clip
        .unwrap_or(tree.boxes[container].rect);
    let scroll = tree.boxes[container].scroll_offset;
    let style = tree.boxes[index].style.clone();
    let rect = tree.boxes[index].rect;

    let top = resolve_inset(style.dim(Property::Top), scrollport.height.max(0.0));
    let bottom = resolve_inset(style.dim(Property::Bottom), scrollport.height.max(0.0));
    let left = resolve_inset(style.dim(Property::Left), scrollport.width.max(0.0));
    let right = resolve_inset(style.dim(Property::Right), scrollport.width.max(0.0));
    if top.is_none() && bottom.is_none() && left.is_none() && right.is_none() {
        return;
    }

    // Static positions in document coordinates, before any sticky shift.
    // `sticky_static_y` is recorded by block layout. The horizontal static
    // position is derived from the containing block's content edge plus the
    // box's resolved left margin; neither changes when scrolling, so the
    // reference stays stable across updates.
    let static_y = tree.boxes[index].sticky_static_y;
    let static_x = parent.map_or(rect.x, |p| {
        tree.boxes[p].content.x + tree.boxes[index].margins.left
    });

    // Insets are measured against the scrollport, which moves with the scroll
    // offset. Convert them to document-coordinate bounds on the box's border
    // box, starting from the box's unshifted (static) position.
    let mut target_y = static_y;
    if let Some(t) = top {
        target_y = target_y.max(scrollport.y + t + scroll.1);
    }
    if let Some(bottom_inset) = bottom {
        target_y = target_y.min(scrollport.bottom() - bottom_inset + scroll.1 - rect.height);
    }
    let mut target_x = static_x;
    if let Some(l) = left {
        target_x = target_x.max(scrollport.x + l + scroll.0);
    }
    if let Some(right_inset) = right {
        target_x = target_x.min(scrollport.right() - right_inset + scroll.0 - rect.width);
    }

    // Clamp to the containing block so the box never escapes its parent. When
    // the parent is the scroll container itself the scrollport clip already
    // bounds the box, and clamping there would pin a box that is legitimately
    // scrolled far down the content, so only intermediate parents apply it.
    if let Some(p) = parent
        && p != container
    {
        let pb = &tree.boxes[p];
        let cb = Rect::new(
            pb.content.x,
            pb.content.y,
            pb.content.width,
            pb.content.height.max(pb.scroll_height),
        );
        target_y = target_y.clamp(cb.y, (cb.bottom() - rect.height).max(cb.y));
        target_x = target_x.clamp(cb.x, (cb.right() - rect.width).max(cb.x));
    }

    let dx = target_x - rect.x;
    let dy = target_y - rect.y;
    if dx.abs() > 0.001 || dy.abs() > 0.001 {
        translate_subtree(tree, index, dx, dy);
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

    fn sticky_tree() -> (LayoutTree, usize, usize) {
        let mut tree = LayoutTree::new();
        let mut scroll_style = ComputedStyle::initial();
        scroll_style.set(
            browser_style::Property::OverflowY,
            browser_style::ComputedValue::keyword("scroll"),
        );
        let scroller = tree.push(LayoutBox::new(None, BoxType::Block, scroll_style));
        tree.boxes[scroller].rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[scroller].content = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[scroller].clip = Some(tree.boxes[scroller].rect);
        tree.boxes[scroller].scroll_height = 400.0;

        let mut sticky_style = ComputedStyle::initial();
        sticky_style.set(
            browser_style::Property::Position,
            browser_style::ComputedValue::keyword("sticky"),
        );
        sticky_style.set(
            browser_style::Property::Top,
            browser_style::ComputedValue::Px(10.0),
        );
        let sticky = tree.push(LayoutBox::new(None, BoxType::Block, sticky_style));
        tree.boxes[sticky].rect = Rect::new(0.0, 50.0, 100.0, 20.0);
        tree.boxes[sticky].content = Rect::new(0.0, 50.0, 100.0, 20.0);
        tree.boxes[sticky].sticky_static_y = 50.0;
        tree.boxes[scroller].children = vec![sticky];
        tree.root = Some(scroller);
        (tree, scroller, sticky)
    }

    #[test]
    fn sticky_box_sticks_to_top_and_restores() {
        let (mut tree, scroller, sticky) = sticky_tree();

        // Before crossing the `top` inset the box scrolls normally.
        tree.boxes[scroller].scroll_offset = (0.0, 20.0);
        update_sticky(&mut tree, scroller);
        assert_eq!(tree.boxes[sticky].rect.y, 50.0);

        // Past the inset it is shifted down so its painted y stays at 10.
        tree.boxes[scroller].scroll_offset = (0.0, 60.0);
        update_sticky(&mut tree, scroller);
        assert_eq!(tree.boxes[sticky].rect.y, 70.0);
        assert_eq!(
            tree.boxes[sticky].rect.y - tree.boxes[scroller].scroll_offset.1,
            10.0
        );

        // Scrolling back to the top restores the original position exactly.
        tree.boxes[scroller].scroll_offset = (0.0, 0.0);
        update_sticky(&mut tree, scroller);
        assert_eq!(tree.boxes[sticky].rect.y, 50.0);
    }

    #[test]
    fn sticky_box_stays_pinned_at_the_inset_when_scrolled_far() {
        let (mut tree, scroller, sticky) = sticky_tree();
        tree.boxes[scroller].scroll_offset = (0.0, 1000.0);
        update_sticky(&mut tree, scroller);
        // Painted y is clamped at the scrollport top plus the `top` inset.
        assert_eq!(
            tree.boxes[sticky].rect.y - tree.boxes[scroller].scroll_offset.1,
            10.0
        );
    }

    #[test]
    fn sticky_box_sticks_to_bottom() {
        let mut tree = LayoutTree::new();
        let mut scroll_style = ComputedStyle::initial();
        scroll_style.set(
            browser_style::Property::OverflowY,
            browser_style::ComputedValue::keyword("scroll"),
        );
        let scroller = tree.push(LayoutBox::new(None, BoxType::Block, scroll_style));
        tree.boxes[scroller].rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[scroller].content = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[scroller].clip = Some(tree.boxes[scroller].rect);
        tree.boxes[scroller].scroll_height = 400.0;

        let mut sticky_style = ComputedStyle::initial();
        sticky_style.set(
            browser_style::Property::Position,
            browser_style::ComputedValue::keyword("sticky"),
        );
        sticky_style.set(
            browser_style::Property::Bottom,
            browser_style::ComputedValue::Px(10.0),
        );
        let sticky = tree.push(LayoutBox::new(None, BoxType::Block, sticky_style));
        // The box sits low in the scrolled content, below the scrollport.
        tree.boxes[sticky].rect = Rect::new(0.0, 350.0, 100.0, 20.0);
        tree.boxes[sticky].margins.left = 0.0;
        tree.boxes[sticky].sticky_static_y = 350.0;
        tree.boxes[scroller].children = vec![sticky];
        tree.root = Some(scroller);

        update_sticky(&mut tree, scroller);
        // Painted bottom is pinned to 10px above the scrollport bottom.
        let painted_bottom =
            tree.boxes[sticky].rect.bottom() - tree.boxes[scroller].scroll_offset.1;
        assert_eq!(painted_bottom, 90.0);

        // A scroll offset that leaves the box inside the scrollport removes
        // the shift entirely and restores the static position.
        tree.boxes[sticky].sticky_static_y = 50.0;
        update_sticky(&mut tree, scroller);
        assert_eq!(tree.boxes[sticky].rect.y, 50.0);
    }

    #[test]
    fn sticky_box_sticks_to_right() {
        let mut tree = LayoutTree::new();
        let mut scroll_style = ComputedStyle::initial();
        scroll_style.set(
            browser_style::Property::OverflowX,
            browser_style::ComputedValue::keyword("scroll"),
        );
        let scroller = tree.push(LayoutBox::new(None, BoxType::Block, scroll_style));
        tree.boxes[scroller].rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[scroller].content = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[scroller].clip = Some(tree.boxes[scroller].rect);

        let mut sticky_style = ComputedStyle::initial();
        sticky_style.set(
            browser_style::Property::Position,
            browser_style::ComputedValue::keyword("sticky"),
        );
        sticky_style.set(
            browser_style::Property::Right,
            browser_style::ComputedValue::Px(10.0),
        );
        let sticky = tree.push(LayoutBox::new(None, BoxType::Block, sticky_style));
        // Static x is derived from the parent's content edge plus the left
        // margin; here the box's normal flow position is x = 70.
        tree.boxes[sticky].rect = Rect::new(70.0, 0.0, 20.0, 20.0);
        tree.boxes[sticky].margins.left = 70.0;
        tree.boxes[scroller].children = vec![sticky];
        tree.root = Some(scroller);

        // Scrolling content to the right (negative scroll x) would carry the
        // box off the right edge; it is pulled back to the right inset.
        tree.boxes[scroller].scroll_offset = (-50.0, 0.0);
        update_sticky(&mut tree, scroller);
        let painted_right = tree.boxes[sticky].rect.right() - tree.boxes[scroller].scroll_offset.0;
        assert_eq!(painted_right, 90.0);

        // Returning the scroll offset restores the static x exactly.
        tree.boxes[scroller].scroll_offset = (0.0, 0.0);
        update_sticky(&mut tree, scroller);
        assert_eq!(tree.boxes[sticky].rect.x, 70.0);
    }

    #[test]
    fn sticky_box_is_clamped_to_its_parent() {
        let mut tree = LayoutTree::new();
        let scroller = tree.push(LayoutBox::new(
            None,
            BoxType::Block,
            ComputedStyle::initial(),
        ));
        tree.boxes[scroller].rect = Rect::new(0.0, 0.0, 100.0, 500.0);
        tree.boxes[scroller].content = Rect::new(0.0, 0.0, 100.0, 500.0);
        tree.boxes[scroller].clip = Some(tree.boxes[scroller].rect);
        tree.boxes[scroller].scroll_height = 500.0;

        let parent = tree.push(LayoutBox::new(
            None,
            BoxType::Block,
            ComputedStyle::initial(),
        ));
        tree.boxes[parent].rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[parent].content = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[parent].scroll_height = 100.0;

        let mut sticky_style = ComputedStyle::initial();
        sticky_style.set(
            browser_style::Property::Position,
            browser_style::ComputedValue::keyword("sticky"),
        );
        sticky_style.set(
            browser_style::Property::Top,
            browser_style::ComputedValue::Px(10.0),
        );
        let sticky = tree.push(LayoutBox::new(None, BoxType::Block, sticky_style));
        tree.boxes[sticky].rect = Rect::new(0.0, 50.0, 100.0, 20.0);
        tree.boxes[sticky].sticky_static_y = 50.0;
        tree.boxes[parent].children = vec![sticky];
        tree.boxes[scroller].children = vec![parent];
        tree.root = Some(scroller);

        tree.boxes[scroller].scroll_offset = (0.0, 200.0);
        update_sticky(&mut tree, scroller);
        // Without the containing-block clamp the box would be pushed to
        // y = 210 to honour `top: 10px`; the clamp keeps it inside the parent.
        let b = &tree.boxes[sticky];
        assert!(b.rect.y >= tree.boxes[parent].rect.y);
        assert!(b.rect.bottom() <= tree.boxes[parent].rect.bottom() + 0.01);
        assert_eq!(b.rect.y, 80.0);
    }

    #[test]
    fn offscreen_absolute_box_is_clipped() {
        let mut doc = browser_dom::Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let hidden = doc.create_element("div");
        doc.set_attr(hidden, "id", "hidden");
        doc.append_child(body, hidden);
        let text = doc.create_text("answer");
        doc.append_child(hidden, text);

        let mut engine = browser_style::StyleEngine::new(browser_style::Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(
            "body { margin: 0; } #hidden { position: absolute; left: -9999px; top: 0; }",
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
        let hidden_node = tree.find_node(hidden).unwrap().node.unwrap();
        let hidden_box = tree
            .boxes
            .iter()
            .find(|b| b.node == Some(hidden_node))
            .unwrap();
        assert!(hidden_box.rect.right() <= 0.0);
        assert_eq!(
            hidden_box.clip,
            Some(Rect::ZERO),
            "an off-screen absolute box must carry an empty clip"
        );
    }

    #[test]
    fn scrolling_a_laid_out_container_sticks_its_child() {
        let mut doc = browser_dom::Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let scroll = doc.create_element("div");
        doc.set_attr(scroll, "id", "scroll");
        doc.append_child(body, scroll);
        let sticky = doc.create_element("div");
        doc.set_attr(sticky, "id", "sticky");
        doc.append_child(scroll, sticky);
        let filler = doc.create_element("div");
        doc.append_child(scroll, filler);

        let mut engine = browser_style::StyleEngine::new(browser_style::Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(
            "body { margin: 0; } #scroll { height: 100px; overflow-y: scroll; } \
             #sticky { position: sticky; top: 0; height: 20px; } \
             #scroll > div:last-child { height: 400px; }",
            browser_style::Origin::Author,
        );
        let styled = engine.compute(&doc);
        let mut tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: 800.0,
                height: 600.0,
            },
        );
        let scroller_node = tree.find_node(scroll).unwrap().node.unwrap();
        let scroller_index = tree
            .boxes
            .iter()
            .position(|b| b.node == Some(scroller_node) && b.clip.is_some())
            .unwrap();
        let sticky_node = tree.find_node(sticky).unwrap().node.unwrap();
        let sticky_index = tree
            .boxes
            .iter()
            .position(|b| b.node == Some(sticky_node))
            .unwrap();
        let static_y = tree.boxes[sticky_index].rect.y;

        crate::scroll::set_scroll(&mut tree, scroller_index, 0.0, 50.0);
        let painted_y =
            tree.boxes[sticky_index].rect.y - tree.boxes[scroller_index].scroll_offset.1;
        assert!(
            (painted_y - static_y).abs() < 0.01,
            "sticky child painted at {painted_y}, static was {static_y}"
        );
    }
}
