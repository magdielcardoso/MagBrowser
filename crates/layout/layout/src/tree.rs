//! Layout tree: boxes, construction from the DOM and queries.

use std::collections::HashMap;

use browser_dom::{Document, NodeId};
use browser_style::{ComputedStyle, Dim, Display, StyledTree};

use crate::box_model::{Edges, Rect};
use crate::inline::LineBox;

/// The formatting role of a layout box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxType {
    /// Block-level box.
    Block,
    /// Inline-level box.
    Inline,
    /// Inline-block box (atomic inline).
    InlineBlock,
    /// Text run.
    Text,
    /// List item marker and content.
    ListItem,
    /// Flex container.
    Flex,
    /// Grid container.
    Grid,
    /// Table (laid out as a block).
    Table,
    /// Table row.
    TableRow,
    /// Table cell.
    TableCell,
    /// Image box.
    Image,
    /// `<br>` line break.
    LineBreak,
    /// Anonymous block wrapping inline content.
    AnonymousBlock,
}

impl BoxType {
    /// True for boxes that participate in block flow.
    pub fn is_block_level(self) -> bool {
        matches!(
            self,
            BoxType::Block
                | BoxType::ListItem
                | BoxType::Flex
                | BoxType::Grid
                | BoxType::Table
                | BoxType::TableRow
                | BoxType::TableCell
                | BoxType::AnonymousBlock
        )
    }
}

/// A node in the layout tree.
#[derive(Debug, Clone)]
pub struct LayoutBox {
    /// The DOM node this box was generated from (absent for anonymous boxes).
    pub node: Option<NodeId>,
    /// Formatting role.
    pub box_type: BoxType,
    /// Computed style.
    pub style: ComputedStyle,
    /// Border box rectangle in document coordinates.
    pub rect: Rect,
    /// Content box rectangle in document coordinates.
    pub content: Rect,
    /// Resolved margins.
    pub margins: Edges,
    /// Resolved borders.
    pub borders: Edges,
    /// Resolved paddings.
    pub paddings: Edges,
    /// Child box indices.
    pub children: Vec<usize>,
    /// Text content for text boxes.
    pub text: String,
    /// Line boxes produced by inline layout.
    pub lines: Vec<LineBox>,
    /// Clip rectangle for scroll containers.
    pub clip: Option<Rect>,
    /// Current scroll offset (x, y).
    pub scroll_offset: (f32, f32),
    /// Total scrollable height.
    pub scroll_height: f32,
    /// Paint order key.
    pub z_index: i32,
    /// True when the box is out of flow.
    pub positioned: bool,
}

impl LayoutBox {
    /// Creates a box.
    pub fn new(node: Option<NodeId>, box_type: BoxType, style: ComputedStyle) -> Self {
        Self {
            node,
            box_type,
            style,
            rect: Rect::ZERO,
            content: Rect::ZERO,
            margins: Edges::default(),
            borders: Edges::default(),
            paddings: Edges::default(),
            children: Vec::new(),
            text: String::new(),
            lines: Vec::new(),
            clip: None,
            scroll_offset: (0.0, 0.0),
            scroll_height: 0.0,
            z_index: 0,
            positioned: false,
        }
    }

    /// The content plus padding rectangle.
    pub fn padding_box(&self) -> Rect {
        Rect::new(
            self.rect.x + self.borders.left,
            self.rect.y + self.borders.top,
            (self.rect.width - self.borders.horizontal()).max(0.0),
            (self.rect.height - self.borders.vertical()).max(0.0),
        )
    }

    /// True when `point` (document coordinates) is inside the visible box.
    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        if self.rect.is_empty() || !self.rect.contains(x, y) {
            return false;
        }
        if let Some(clip) = self.clip
            && !clip.contains(x, y)
        {
            return false;
        }
        true
    }
}

/// The result of laying out a document.
#[derive(Debug, Clone, Default)]
pub struct LayoutTree {
    /// All boxes, indexed by box index.
    pub boxes: Vec<LayoutBox>,
    /// Index of the root box.
    pub root: Option<usize>,
    /// Total document height.
    pub document_height: f32,
    /// Node → box index mapping.
    node_map: HashMap<NodeId, usize>,
}

impl LayoutTree {
    /// Creates an empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a box, returning its index.
    pub fn push(&mut self, b: LayoutBox) -> usize {
        let index = self.boxes.len();
        if let Some(node) = b.node {
            self.node_map.entry(node).or_insert(index);
        }
        self.boxes.push(b);
        index
    }

    /// Borrows a box by index.
    pub fn get(&self, index: usize) -> Option<&LayoutBox> {
        self.boxes.get(index)
    }

    /// Mutably borrows a box by index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut LayoutBox> {
        self.boxes.get_mut(index)
    }

    /// The root box.
    pub fn root_box(&self) -> Option<&LayoutBox> {
        self.root.and_then(|i| self.boxes.get(i))
    }

    /// The box generated for a DOM node.
    pub fn find_node(&self, node: NodeId) -> Option<&LayoutBox> {
        self.node_map.get(&node).and_then(|i| self.boxes.get(*i))
    }

    /// Deepest box whose border box contains the point.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        fn walk(tree: &LayoutTree, index: usize, x: f32, y: f32, best: &mut Option<usize>) {
            let Some(b) = tree.boxes.get(index) else {
                return;
            };
            if b.rect.is_empty() {
                // Inline containers have no box geometry of their own; descend so
                // their text fragments remain hit-testable.
                if let Some(clip) = b.clip
                    && !clip.contains(x, y)
                {
                    return;
                }
                for child in &b.children {
                    walk(tree, *child, x, y, best);
                }
                return;
            }
            if !b.contains_point(x, y) {
                return;
            }
            *best = Some(index);
            for child in &b.children {
                walk(tree, *child, x, y, best);
            }
        }
        let mut best = None;
        if let Some(root) = self.root {
            walk(self, root, x, y, &mut best);
        }
        best
    }

    /// Paint order: descendants first, then the box itself, grouped by z-index.
    pub fn paint_order(&self) -> Vec<usize> {
        fn walk(tree: &LayoutTree, index: usize, out: &mut Vec<usize>) {
            let Some(b) = tree.boxes.get(index) else {
                return;
            };
            let mut children: Vec<usize> = b.children.clone();
            children.sort_by_key(|c| tree.boxes.get(*c).map_or(0, |b| b.z_index));
            for child in children {
                walk(tree, child, out);
            }
            out.push(index);
        }
        let mut out = Vec::new();
        if let Some(root) = self.root {
            walk(self, root, &mut out);
        }
        out
    }

    /// Bounding height of the document content.
    pub fn content_height(&self) -> f32 {
        self.boxes
            .iter()
            .map(|b| b.rect.bottom().max(b.rect.y))
            .fold(0.0_f32, f32::max)
    }
}

/// Builds the layout tree from the DOM.
pub struct TreeBuilder<'a> {
    doc: &'a Document,
    styled: &'a StyledTree,
    tree: LayoutTree,
}

impl<'a> TreeBuilder<'a> {
    /// Creates a builder.
    pub fn new(doc: &'a Document, styled: &'a StyledTree) -> Self {
        Self {
            doc,
            styled,
            tree: LayoutTree::new(),
        }
    }

    /// Builds and returns the tree (without geometry; call `layout` afterwards).
    pub fn build(mut self) -> LayoutTree {
        let root_node = self.doc.document_element().unwrap_or(self.doc.root());
        let initial = ComputedStyle::initial();
        if let Some(index) = self.build_node(root_node, &initial) {
            self.tree.root = Some(index);
        }
        self.tree
    }

    fn style_for(&self, node: NodeId, parent: &ComputedStyle) -> ComputedStyle {
        self.styled
            .get(node)
            .cloned()
            .unwrap_or_else(|| parent.clone())
    }

    fn build_node(&mut self, node: NodeId, parent_style: &ComputedStyle) -> Option<usize> {
        let dom_node = self.doc.node(node);
        let style = self.style_for(node, parent_style);
        match &dom_node.data {
            browser_dom::NodeData::Text(text) => {
                let mut b = LayoutBox::new(Some(node), BoxType::Text, style);
                b.text = text.data.clone();
                Some(self.tree.push(b))
            }
            browser_dom::NodeData::Element(_) => {
                if style.display() == Display::None {
                    return None;
                }
                let name = self.doc.element_name(node).unwrap_or("");
                let box_type = match name {
                    "br" => BoxType::LineBreak,
                    "img" => BoxType::Image,
                    _ => box_type_from_display(style.display()),
                };
                let mut b = LayoutBox::new(Some(node), box_type, style.clone());
                b.z_index = self.style_for(node, parent_style).z_index();
                b.positioned = matches!(
                    self.style_for(node, parent_style).position(),
                    browser_style::Position::Absolute | browser_style::Position::Fixed
                );
                let index = self.tree.push(b);

                let child_nodes: Vec<NodeId> = dom_node.children.clone();
                let block_container = matches!(
                    box_type,
                    BoxType::Block
                        | BoxType::ListItem
                        | BoxType::Flex
                        | BoxType::Grid
                        | BoxType::Table
                        | BoxType::TableCell
                        | BoxType::TableRow
                );

                let mut children: Vec<usize> = Vec::new();
                let mut inline_run: Vec<usize> = Vec::new();

                for child_node in &child_nodes {
                    if matches!(
                        self.doc.node(*child_node).data,
                        browser_dom::NodeData::Comment(_) | browser_dom::NodeData::Doctype { .. }
                    ) {
                        continue;
                    }
                    let child_is_text = self.doc.node(*child_node).is_text();
                    let child_style = self.style_for(*child_node, &style);
                    let child_is_inline = child_is_text
                        || (!matches!(
                            child_style.display(),
                            Display::None
                                | Display::Block
                                | Display::Flex
                                | Display::Grid
                                | Display::ListItem
                                | Display::Table
                                | Display::TableRow
                                | Display::TableCell
                        ));

                    let built = self.build_node(*child_node, &style);
                    let Some(child_index) = built else { continue };
                    if block_container
                        && child_is_inline
                        && self.has_block_sibling(&style, &child_style, &child_nodes)
                    {
                        inline_run.push(child_index);
                    } else if !inline_run.is_empty() {
                        let anon = self.wrap_anonymous(inline_run.clone(), &style);
                        children.push(anon);
                        inline_run.clear();
                        children.push(child_index);
                    } else {
                        children.push(child_index);
                    }
                }
                if !inline_run.is_empty() {
                    let anon = self.wrap_anonymous(inline_run, &style);
                    children.push(anon);
                }
                self.tree.boxes[index].children = children;
                Some(index)
            }
            _ => None,
        }
    }

    fn has_block_sibling(
        &self,
        parent_style: &ComputedStyle,
        _child_style: &ComputedStyle,
        children: &[NodeId],
    ) -> bool {
        children.iter().any(|child| {
            // Text nodes never establish block boxes of their own.
            if self.doc.node(*child).is_text() {
                return false;
            }
            let style = self.style_for(*child, parent_style);
            matches!(
                style.display(),
                Display::Block | Display::Flex | Display::Grid | Display::ListItem | Display::Table
            )
        })
    }

    fn wrap_anonymous(&mut self, children: Vec<usize>, parent_style: &ComputedStyle) -> usize {
        // Anonymous boxes carry no margins, borders, padding or background: they
        // exist only to hold inline content inside a block container.
        let mut style = parent_style.clone();
        for side in browser_style::Side::ALL {
            style.set(
                browser_style::margin_property(side),
                browser_style::ComputedValue::Px(0.0),
            );
            style.set(
                browser_style::padding_property(side),
                browser_style::ComputedValue::Px(0.0),
            );
            style.set(
                browser_style::border_width_property(side),
                browser_style::ComputedValue::Px(0.0),
            );
        }
        style.set(
            browser_style::Property::Display,
            browser_style::ComputedValue::keyword("block"),
        );
        let mut b = LayoutBox::new(None, BoxType::AnonymousBlock, style);
        b.children = children;
        self.tree.push(b)
    }
}

fn box_type_from_display(display: Display) -> BoxType {
    match display {
        Display::Block => BoxType::Block,
        Display::Inline => BoxType::Inline,
        Display::InlineBlock => BoxType::InlineBlock,
        Display::ListItem => BoxType::ListItem,
        Display::Flex => BoxType::Flex,
        Display::Grid => BoxType::Grid,
        Display::Table => BoxType::Table,
        Display::TableRow => BoxType::TableRow,
        Display::TableCell => BoxType::TableCell,
        Display::Contents | Display::None => BoxType::Block,
    }
}

/// Resolves a dimension against a base length, defaulting to 0 for `auto`.
pub fn resolve_dim(dim: Dim, base: f32) -> f32 {
    dim.resolve(base).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_style::Origin;
    use browser_style::{StyleEngine, Viewport};

    fn doc() -> (Document, NodeId, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let p = doc.create_element("p");
        doc.append_child(body, p);
        doc.append_text(p, "hello");
        (doc, body, p)
    }

    #[test]
    fn builds_boxes_for_elements_and_text() {
        let (doc, body, p) = doc();
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css("p { color: red; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        assert_eq!(
            tree.root_box().unwrap().node,
            Some(doc.document_element().unwrap())
        );
        let body_box = tree.find_node(body).unwrap();
        assert_eq!(body_box.box_type, BoxType::Block);
        let p_box = tree.find_node(p).unwrap();
        assert_eq!(p_box.children.len(), 1);
        assert_eq!(tree.boxes[p_box.children[0]].text, "hello");
    }

    #[test]
    fn display_none_produces_no_box() {
        let (doc, _body, p) = doc();
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css("p { display: none; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        assert!(tree.find_node(p).is_none());
    }
}
