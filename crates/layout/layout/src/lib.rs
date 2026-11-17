//! Layout engine: turns a styled DOM into positioned boxes.
//!
//! Supported: block flow, inline flow with line breaking and `text-align`,
//! margin collapsing between siblings, `inline-block`, `position: relative`,
//! `position: absolute/fixed` (basic), `overflow` clipping with scrolling,
//! table rows/cells laid out as blocks, a small flexbox subset, floats with
//! `clear`, and a CSS grid subset (`grid-template-columns/rows`, `fr`, `auto`,
//! `repeat()`, gaps).
//!
//! Not supported (documented): full table formatting, bidirectional text, and
//! the advanced parts of grid (explicit placement, spanning, named areas). See
//! `docs/roadmap.md`.
//!
//! ```
//! use browser_dom::Document;
//! use browser_style::{StyleEngine, Viewport};
//! use browser_layout::{Size, layout};
//!
//! let mut doc = Document::new();
//! let html = doc.create_element("html");
//! let body = doc.create_element("body");
//! doc.append_child(doc.root(), html);
//! doc.append_child(html, body);
//! doc.append_text(body, "hello layout");
//!
//! let styled = StyleEngine::new(Viewport::default()).compute(&doc);
//! let tree = layout(&doc, &styled, Size { width: 800.0, height: 600.0 });
//! assert!(tree.content_height() > 0.0);
//! ```

#![forbid(unsafe_code)]

pub mod block;
pub mod box_model;
pub mod flex;
pub mod fragment;
pub mod grid;
pub mod inline;
pub mod position;
pub mod scroll;
pub mod table;
pub mod tree;

pub use box_model::{Edges, Rect, collapse_margins};
pub use fragment::{BoxFragment, TextRun};
pub use inline::{LayoutAtom, LineBox, TextFragment};
pub use tree::{BoxType, LayoutBox, LayoutTree, TreeBuilder};

/// Viewport size in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Default for Size {
    fn default() -> Self {
        Self {
            width: 1024.0,
            height: 768.0,
        }
    }
}

/// Builds and lays out a document, returning the layout tree.
pub fn layout(
    doc: &browser_dom::Document,
    styled: &browser_style::StyledTree,
    viewport: Size,
) -> LayoutTree {
    let mut tree = TreeBuilder::new(doc, styled).build();
    let _ = block::layout_document(&mut tree, doc, viewport.width, viewport.height);
    tree
}

#[cfg(test)]
pub(crate) mod test_util {
    //! Shared deterministic fixtures for layout tests.

    use browser_dom::{Document, NodeId};
    use browser_style::{Origin, StyleEngine, Viewport};

    use crate::{LayoutTree, Size, layout};

    /// Lays out a document whose body contains the given `(tag, id, text)`
    /// children. Returns the document, the tree and the node ids `[body, ...]`.
    pub fn layout_with(
        css: &str,
        children: &[(&str, &str, &str)],
    ) -> (Document, LayoutTree, Vec<NodeId>) {
        build(css, |doc, body| {
            let mut ids = Vec::new();
            for (tag, id, text) in children {
                ids.push(append(doc, *body, tag, id, text));
            }
            ids
        })
    }

    /// Lays out a document with a single `div#g` grid container holding the
    /// given children. Returns ids `[body, g, ...items]`.
    pub fn layout_grid(
        css: &str,
        items: &[(&str, &str, &str)],
    ) -> (Document, LayoutTree, Vec<NodeId>) {
        build(css, |doc, body| {
            let grid = append(doc, *body, "div", "g", "");
            let mut ids = vec![grid];
            for (tag, id, text) in items {
                ids.push(append(doc, grid, tag, id, text));
            }
            ids
        })
    }

    fn append(doc: &mut Document, parent: NodeId, tag: &str, id: &str, text: &str) -> NodeId {
        let element = doc.create_element(tag);
        if !id.is_empty() {
            doc.set_attr(element, "id", id);
        }
        doc.append_child(parent, element);
        if !text.is_empty() {
            doc.append_text(element, text);
        }
        element
    }

    fn build(
        css: &str,
        populate: impl FnOnce(&mut Document, &NodeId) -> Vec<NodeId>,
    ) -> (Document, LayoutTree, Vec<NodeId>) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let inner = populate(&mut doc, &body);
        let mut ids = vec![body];
        ids.extend(inner);

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
        (doc, tree, ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::NodeId;
    use browser_style::Origin;
    use browser_style::{StyleEngine, Viewport};

    struct Fixture {
        body: NodeId,
        first: NodeId,
        second: NodeId,
    }

    fn fixture(css: &str) -> (Fixture, LayoutTree) {
        let mut doc = browser_dom::Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let first = doc.create_element("div");
        let second = doc.create_element("div");
        doc.append_child(body, first);
        doc.append_child(body, second);
        doc.append_text(first, "Hello");
        doc.append_text(second, "World");
        doc.set_attr(second, "id", "second");

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
        (
            Fixture {
                body,
                first,
                second,
            },
            tree,
        )
    }

    #[test]
    fn block_boxes_stack_vertically() {
        let (f, tree) = fixture("body { margin: 0; } div { height: 50px; }");
        let body_box = tree.find_node(f.body).unwrap();
        assert_eq!(body_box.content.height, 100.0);
        let first = tree.find_node(f.first).unwrap();
        let second = tree.find_node(f.second).unwrap();
        assert_eq!(first.rect.y, 0.0);
        assert_eq!(second.rect.y, 50.0);
    }

    #[test]
    fn widths_fill_and_respect_percentages() {
        let (f, tree) = fixture("body { margin: 0; } div { height: 10px; } #none { }");
        assert_eq!(tree.find_node(f.first).unwrap().rect.width, 800.0);
        let (f2, tree2) = fixture("body { margin: 0; } div { height: 10px; width: 50%; }");
        assert_eq!(tree2.find_node(f2.first).unwrap().rect.width, 400.0);
    }

    #[test]
    fn padding_and_border_grow_border_box() {
        let (f, tree) = fixture(
            "body { margin: 0; } div { height: 10px; padding: 5px; border: 2px solid black; }",
        );
        let b = tree.find_node(f.first).unwrap();
        assert_eq!(b.rect.width, 800.0);
        assert_eq!(b.content.width, 800.0 - 10.0 - 4.0);
        assert_eq!(b.rect.height, 10.0 + 10.0 + 4.0);
    }

    #[test]
    fn inline_text_produces_line_boxes() {
        let (f, tree) = fixture("body { margin: 0; } div { display: inline; }");
        let body_box = tree.find_node(f.body).unwrap();
        assert!(!body_box.lines.is_empty());
        assert!(
            body_box.lines[0]
                .fragments
                .iter()
                .any(|fr| fr.text == "Hello")
        );
    }

    #[test]
    fn two_block_children_collapse_margins() {
        let (f, tree) =
            fixture("body { margin: 0; } div { height: 20px; margin-bottom: 30px; } #x { }");
        let first_bottom = tree.find_node(f.first).unwrap().rect.bottom();
        let second_top = tree.find_node(f.second).unwrap().rect.y;
        assert_eq!(second_top - first_bottom, 30.0);
    }

    #[test]
    fn hit_testing_finds_deepest_box() {
        let (f, tree) = fixture("body { margin: 0; } div { height: 50px; }");
        let index = tree.hit_test(10.0, 60.0).unwrap();
        let node = tree.boxes[index].node.unwrap();
        assert!(
            node == f.second || tree.boxes[index].box_type == BoxType::Text,
            "unexpected hit: {node:?}"
        );
    }

    #[test]
    fn overflow_creates_clip_and_scroll_range() {
        let (_f, tree) = fixture(
            "body { margin: 0; } div { height: 20px; } #second { height: 40px; overflow: scroll; }",
        );
        let scroll = tree
            .boxes
            .iter()
            .find(|b| b.clip.is_some())
            .expect("a scroll container");
        assert_eq!(scroll.content.height, 40.0);
    }
}
