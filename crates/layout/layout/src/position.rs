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
        // Accessibility-only content is commonly parked off-screen with
        // `position:absolute; left:-9999px`. Such a box and its descendants
        // must never contribute paint: give it an empty clip so the painter
        // discards everything inside it even if a glyph rasterizer resolves a
        // negative pen position into the visible part of the canvas.
        let off_left = b.rect.right() <= 0.0;
        let is_out_of_flow = matches!(
            b.style.position(),
            browser_style::Position::Absolute | browser_style::Position::Fixed
        );
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

/// Re-positions the sticky descendants of a scroll container after its scroll
/// offset changes.
///
/// Only vertical (`top`) stickiness is implemented. The shift is recomputed
/// from the box's recorded static y on every update, so repeated scrolling
/// never accumulates error. Boxes in nested scroll containers are left to the
/// container that actually scrolls them.
pub fn update_sticky(tree: &mut LayoutTree, container: usize) {
    fn walk(tree: &mut LayoutTree, index: usize, scroller: usize, container: usize) {
        let Some(b) = tree.boxes.get(index) else {
            return;
        };
        let is_scroller = b.clip.is_some();
        let is_sticky = b.style.position() == browser_style::Position::Sticky;
        let child_scroller = if is_scroller && index != container {
            index
        } else {
            scroller
        };

        if is_sticky && index != container && scroller == container {
            let (static_y, current_y, top) = {
                let sticky = &tree.boxes[index];
                let scrollport = tree.boxes[container]
                    .clip
                    .unwrap_or(tree.boxes[container].rect);
                let top = match sticky.style.dim(browser_style::Property::Top) {
                    browser_style::Dim::Auto => 0.0,
                    browser_style::Dim::Px(px) => px,
                    browser_style::Dim::Percent(p) => scrollport.height.max(0.0) * p / 100.0,
                    browser_style::Dim::Calc { percent, px } => {
                        scrollport.height.max(0.0) * percent / 100.0 + px
                    }
                };
                (sticky.sticky_static_y, sticky.rect.y, top)
            };
            let (scroll_y, clip_top) = {
                let scrollport = tree.boxes[container]
                    .clip
                    .unwrap_or(tree.boxes[container].rect);
                (tree.boxes[container].scroll_offset.1, scrollport.y)
            };

            // Where the box would paint without any sticky shift, and where
            // the `top` inset wants it to be. Scrolling far simply pins the
            // box at the inset; boxes still below the fold are left alone
            // (the scrollport clip hides them, as usual).
            let natural = static_y - scroll_y;
            let desired = natural.max(clip_top + top);
            let delta = desired - (current_y - scroll_y);
            if delta.abs() > 0.001 {
                translate_subtree(tree, index, 0.0, delta);
            }
        }

        let children = tree.boxes[index].children.clone();
        for child in children {
            walk(tree, child, child_scroller, container);
        }
    }

    if tree.boxes.get(container).is_some() {
        walk(tree, container, container, container);
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
