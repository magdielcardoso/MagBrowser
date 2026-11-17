//! Display-list construction from a laid-out document.

use browser_layout::{BoxType, LayoutTree, Rect, Size};

use crate::display_list::{DisplayCommand, DisplayList};
use crate::images::ImageData;
use crate::raster::{ImageBuffer, Rasterizer};

use std::collections::HashMap;

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

    if b.box_type != BoxType::Text && !rect.is_empty() {
        if let Some(command) =
            crate::background::background_command(rect, b.style.background_color())
        {
            list.push(command);
        }
        if b.box_type == BoxType::Image {
            let placeholder =
                || ImageData::placeholder(rect.width.max(1.0) as u32, rect.height.max(1.0) as u32);
            let image_index = match b.node {
                Some(node) => *image_indices.entry(node).or_insert_with(|| {
                    let image = images.get(&node).cloned().unwrap_or_else(placeholder);
                    list.add_image(image)
                }),
                None => list.add_image(placeholder()),
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
        if let Some(command) = crate::borders::border_command(
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
        ) {
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
}
