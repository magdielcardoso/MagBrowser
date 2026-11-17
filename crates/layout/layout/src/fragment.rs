//! Paint-ready fragments extracted from the layout tree.

use browser_dom::NodeId;
use browser_style::Color;

use crate::box_model::{Edges, Rect};
use crate::inline::TextFragment;
use crate::tree::{BoxType, LayoutTree};

/// One box ready to be painted.
#[derive(Debug, Clone)]
pub struct BoxFragment {
    /// Index in the layout tree.
    pub box_index: usize,
    /// Originating DOM node.
    pub node: Option<NodeId>,
    /// Layout role.
    pub box_type: BoxType,
    /// Border box in document coordinates.
    pub rect: Rect,
    /// Content box in document coordinates.
    pub content: Rect,
    /// Background colour.
    pub background: Color,
    /// Border widths.
    pub borders: Edges,
    /// Border colours per side (top, right, bottom, left).
    pub border_colors: [Color; 4],
    /// Border styles per side.
    pub border_styles: [browser_style::BorderStyle; 4],
    /// Clip rectangle (scroll containers / overflow hidden).
    pub clip: Option<Rect>,
    /// Scroll offset applied to descendants.
    pub scroll: (f32, f32),
    /// Paint order key.
    pub z_index: i32,
    /// Opacity.
    pub opacity: f32,
}

/// Text ready to be painted.
#[derive(Debug, Clone)]
pub struct TextRun {
    /// Index of the owning box.
    pub box_index: usize,
    /// The text fragment.
    pub fragment: TextFragment,
}

impl LayoutTree {
    /// Box fragments in paint order (back to front).
    pub fn box_fragments(&self) -> Vec<BoxFragment> {
        self.paint_order()
            .into_iter()
            .filter_map(|index| {
                let b = self.boxes.get(index)?;
                if b.rect.is_empty() || b.box_type == BoxType::Text {
                    return None;
                }
                Some(BoxFragment {
                    box_index: index,
                    node: b.node,
                    box_type: b.box_type,
                    rect: b.rect,
                    content: b.content,
                    background: b.style.background_color(),
                    borders: b.borders,
                    border_colors: [
                        b.style.border_color(browser_style::Side::Top),
                        b.style.border_color(browser_style::Side::Right),
                        b.style.border_color(browser_style::Side::Bottom),
                        b.style.border_color(browser_style::Side::Left),
                    ],
                    border_styles: [
                        b.style.border_style(browser_style::Side::Top),
                        b.style.border_style(browser_style::Side::Right),
                        b.style.border_style(browser_style::Side::Bottom),
                        b.style.border_style(browser_style::Side::Left),
                    ],
                    clip: b.clip,
                    scroll: b.scroll_offset,
                    z_index: b.z_index,
                    opacity: b.style.opacity(),
                })
            })
            .collect()
    }

    /// Text runs in paint order.
    pub fn text_runs(&self) -> Vec<TextRun> {
        let mut out = Vec::new();
        for index in self.paint_order() {
            let Some(b) = self.boxes.get(index) else {
                continue;
            };
            for line in &b.lines {
                for fragment in &line.fragments {
                    out.push(TextRun {
                        box_index: index,
                        fragment: fragment.clone(),
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::LayoutBox;
    use browser_style::ComputedStyle;

    #[test]
    fn extracts_fragments_in_paint_order() {
        let mut tree = LayoutTree::new();
        let parent = tree.push(LayoutBox::new(
            None,
            BoxType::Block,
            ComputedStyle::initial(),
        ));
        let child = tree.push(LayoutBox::new(
            None,
            BoxType::Block,
            ComputedStyle::initial(),
        ));
        tree.boxes[parent].children = vec![child];
        tree.boxes[parent].rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        tree.boxes[child].rect = Rect::new(0.0, 0.0, 50.0, 50.0);
        tree.root = Some(parent);

        let fragments = tree.box_fragments();
        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].box_index, child);
        assert_eq!(fragments[1].box_index, parent);
    }
}
