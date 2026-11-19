//! Layout tree: boxes, construction from the DOM and queries.

use std::collections::HashMap;

use browser_dom::{Document, NodeId, escape_attr, escape_text};
use browser_style::{ComputedStyle, ComputedValue, Dim, Display, Property, Side, StyledTree};

use crate::box_model::{Edges, Rect};
use crate::inline::LineBox;

/// Maximum byte length of a serialized inline `<svg>` source kept on a layout
/// box. Bounding this keeps the paint-side rasterizer's input (and the work
/// spent walking the SVG subtree) finite on hostile pages.
pub const MAX_SVG_SOURCE_BYTES: usize = 256 * 1024;

/// Maximum nesting depth walked while serializing an inline `<svg>` subtree.
const MAX_SVG_DEPTH: usize = 128;

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
    /// Image box. Also used for atomic inline `<svg>` elements, which carry
    /// their serialized source in [`LayoutBox::svg_source`].
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
    /// Serialized SVG source for atomic inline `<svg>` boxes. When `Some`, the
    /// paint side rasterizes this string instead of using a decoded bitmap.
    pub svg_source: Option<String>,
    /// Line boxes produced by inline layout.
    pub lines: Vec<LineBox>,
    /// Clip rectangle for scroll containers.
    pub clip: Option<Rect>,
    /// Vertical scrollbar track/thumb rectangle for scroll containers whose
    /// content overflows. `None` when no vertical scrollbar is present (or when
    /// `scrollbar-width: none` suppresses it). Exposed for painting.
    pub scrollbar: Option<Rect>,
    /// Horizontal scrollbar rectangle, for the same containers as
    /// [`Self::scrollbar`] when content also overflows horizontally.
    pub horizontal_scrollbar: Option<Rect>,
    /// Current scroll offset (x, y).
    pub scroll_offset: (f32, f32),
    /// Total scrollable height.
    pub scroll_height: f32,
    /// Paint order key.
    pub z_index: i32,
    /// True when the box is out of flow.
    pub positioned: bool,
    /// Document y captured at layout time, before any sticky adjustment.
    /// Used by `position::update_sticky` to compute the sticky offset without
    /// accumulating error across scroll updates.
    pub sticky_static_y: f32,
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
            svg_source: None,
            lines: Vec::new(),
            clip: None,
            scrollbar: None,
            horizontal_scrollbar: None,
            scroll_offset: (0.0, 0.0),
            scroll_height: 0.0,
            z_index: 0,
            positioned: false,
            sticky_static_y: 0.0,
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
    /// True when the per-layout work budget was exhausted and some subtrees
    /// were skipped, so this tree is a partial rendering of the document.
    pub truncated: bool,
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
    ///
    /// Only in-flow boxes contribute. Absolutely and fixed positioned boxes
    /// are taken out of flow: their offsets may be relative to a scrolled or
    /// transformed ancestor, so letting them extend the document height makes
    /// a small page look endlessly tall (and paints an enormous area). Boxes
    /// with non-finite geometry are ignored for the same reason.
    pub fn content_height(&self) -> f32 {
        self.boxes
            .iter()
            .filter(|b| !b.positioned)
            .map(|b| b.rect.bottom().max(b.rect.y))
            .filter(|bottom| bottom.is_finite())
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
        // `layout()` begins the budget before calling us; only harvest the
        // truncation flag here when building standalone.
        let started_budget = crate::budget::ensure_active();
        let root_node = self.doc.document_element().unwrap_or(self.doc.root());
        let initial = ComputedStyle::initial();
        if let Some(index) = self.build_node(root_node, &initial) {
            self.tree.root = Some(index);
        }
        if started_budget {
            self.tree.truncated = crate::budget::finish();
        }
        self.tree
    }

    fn style_for(&self, node: NodeId, parent: &ComputedStyle) -> ComputedStyle {
        self.styled
            .get(node)
            .cloned()
            .unwrap_or_else(|| parent.clone())
    }

    /// Synthesizes an inline text box for a `::before`/`::after` pseudo-element.
    ///
    /// Returns `None` when the host has no such pseudo-element (or its
    /// `content` is not a string) or when the per-layout box budget is
    /// exhausted. The pseudo box counts toward the same budget as real boxes.
    fn build_pseudo(&mut self, node: NodeId, name: &str) -> Option<usize> {
        let (content, style) = {
            let pseudo = self.styled.pseudo(node, name)?;
            (pseudo.content.clone(), pseudo.style.clone())
        };
        if !crate::budget::try_create_box() {
            return None;
        }
        let mut b = LayoutBox::new(None, BoxType::Text, style);
        b.text = content;
        Some(self.tree.push(b))
    }

    fn build_node(&mut self, node: NodeId, parent_style: &ComputedStyle) -> Option<usize> {
        // Budget checks: stop creating boxes and skip the remaining subtrees
        // once the work limits are reached. Both the flat box count and the
        // recursion depth are bounded so neither time nor stack is unbounded.
        if !crate::budget::try_create_box() {
            return None;
        }
        let depth = crate::budget::DepthGuard::enter();
        if !depth.entered() {
            return None;
        }
        let dom_node = self.doc.node(node);
        let mut style = self.style_for(node, parent_style);
        // CSS logical properties are not longhands in the style crate, so the
        // cascade drops them. Map them onto physical properties here (in the
        // tree builder, where both the style and the authored inline `style`
        // attribute are available) before any box-model values are built.
        apply_logical_properties(&mut style, self.doc, node);
        match &dom_node.data {
            browser_dom::NodeData::Text(text) => {
                if !crate::budget::consume_inline_items(crate::budget::estimate_inline_items(
                    &text.data,
                )) {
                    return None;
                }
                let mut b = LayoutBox::new(Some(node), BoxType::Text, style);
                b.text = text.data.clone();
                Some(self.tree.push(b))
            }
            browser_dom::NodeData::Element(_) => {
                if style.display() == Display::None {
                    return None;
                }
                let name = self.doc.element_name(node).unwrap_or("");
                // Inline `<svg>` is a replaced/atomic box, just like `<img>`.
                // The SVG source is serialized onto the box for the paint side.
                let is_svg = name.eq_ignore_ascii_case("svg");
                let mut box_type = match name {
                    "br" => BoxType::LineBreak,
                    "img" | "svg" => BoxType::Image,
                    _ => box_type_from_display(style.display()),
                };
                // `float` blockifies the box, so a floated inline element takes part
                // in block flow (CSS 2.1 §9.7). Replaced SVG keeps its atomic type
                // so it remains paintable.
                if style.float() != browser_style::Float::None && !is_svg {
                    box_type = BoxType::Block;
                }

                // A text-like `<input size="N">` gets a presentational hint of
                // roughly `N` average character widths (≈ 8px each). This is a
                // low-priority UA declaration, so it only applies when no CSS
                // `width` won the cascade — an author rule such as
                // `.Ucigb { width: 458px }` must always take precedence.
                if name == "input"
                    && style.width().is_auto()
                    && let Some(raw) = self.doc.attr(node, "size")
                    && let Ok(size) = raw.trim().parse::<f32>()
                    && size > 0.0
                {
                    style.set(
                        browser_style::Property::Width,
                        browser_style::ComputedValue::Px(size * 8.0),
                    );
                }

                // An inline `<svg>` gets a definite size from its `width`/`height`
                // attributes (a presentational hint, so author CSS wins), falling
                // back to the CSS replaced-element default of 300x150. A viewBox
                // supplies the aspect ratio when only one edge is declared.
                let mut svg_source = None;
                if is_svg {
                    let (width, height, view_box) = svg_box_dimensions(self.doc, node);
                    if style.width().is_auto() {
                        style.set(
                            browser_style::Property::Width,
                            browser_style::ComputedValue::Px(width),
                        );
                    }
                    if style.height().is_auto() {
                        style.set(
                            browser_style::Property::Height,
                            browser_style::ComputedValue::Px(height),
                        );
                    }
                    if let Some((vb_w, vb_h)) = view_box {
                        style.set(
                            browser_style::Property::AspectRatio,
                            browser_style::ComputedValue::AspectRatio(
                                browser_style::AspectRatio::Ratio(vb_w, vb_h),
                            ),
                        );
                    }
                    // Serialize the SVG subtree (bounded in size and depth) and
                    // charge the per-layout work budget for the bytes produced.
                    if let Some(source) = serialize_svg_source(self.doc, node, width, height) {
                        let cost = (source.len() / 64).max(1);
                        if crate::budget::consume_inline_items(cost) {
                            svg_source = Some(source);
                        }
                    }
                }

                let mut b = LayoutBox::new(Some(node), box_type, style.clone());
                b.svg_source = svg_source;
                b.z_index = self.style_for(node, parent_style).z_index();
                b.positioned = matches!(
                    self.style_for(node, parent_style).position(),
                    browser_style::Position::Absolute | browser_style::Position::Fixed
                );
                let index = self.tree.push(b);

                // A replaced SVG paints from its serialized source; its DOM
                // children are not laid out as boxes.
                let child_nodes: Vec<NodeId> = if is_svg {
                    Vec::new()
                } else {
                    dom_node.children.clone()
                };
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
                // Used to decide whether generated text needs its own inline
                // formatting context: a bare text box in block flow is dropped.
                let has_block_children = self.has_block_sibling(&style, &style, &child_nodes);

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
                // Form controls are replaced elements: their label lives in a
                // `value` (or `placeholder`) attribute, not in a DOM text node.
                // Synthesize a text child so buttons and filled fields paint it.
                if name == "input" {
                    let kind = self
                        .doc
                        .attr(node, "type")
                        .unwrap_or("text")
                        .to_ascii_lowercase();
                    if !matches!(kind.as_str(), "hidden" | "image" | "checkbox" | "radio") {
                        let label = self
                            .doc
                            .attr(node, "value")
                            .or_else(|| self.doc.attr(node, "placeholder"))
                            .unwrap_or("");
                        if !label.is_empty() {
                            let mut text_box =
                                LayoutBox::new(Some(node), BoxType::Text, style.clone());
                            text_box.text = label.to_string();
                            let text_index = self.tree.push(text_box);
                            children.push(text_index);
                        }
                    }
                }
                // `::before`/`::after` generate inline text boxes in document
                // order around the element's real children. Replaced elements
                // (img/input) never generate pseudo-element boxes.
                if !matches!(name, "img" | "input") && !is_svg {
                    if let Some(before) = self.build_pseudo(node, "before") {
                        if has_block_children {
                            let anon = self.wrap_anonymous(vec![before], &style);
                            children.insert(0, anon);
                        } else {
                            children.insert(0, before);
                        }
                    }
                    if let Some(after) = self.build_pseudo(node, "after") {
                        if has_block_children {
                            let anon = self.wrap_anonymous(vec![after], &style);
                            children.push(anon);
                        } else {
                            children.push(after);
                        }
                    }
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
        // An anonymous box is only a wrapper for inline content. It must not
        // inherit sizing constraints from its parent: a parent's
        // `min-height`/`aspect-ratio`/definite size would otherwise be applied
        // once per anonymous wrapper (a block with several inline runs next to
        // block siblings gets one wrapper per run), multiplying the parent's
        // height by the number of wrappers. Reset every size-derived property
        // so the wrapper is sized solely by the inline content it holds.
        for property in [
            browser_style::Property::Width,
            browser_style::Property::Height,
            browser_style::Property::MinWidth,
            browser_style::Property::MaxWidth,
            browser_style::Property::MinHeight,
            browser_style::Property::MaxHeight,
            browser_style::Property::FlexBasis,
        ] {
            style.set(
                property,
                browser_style::ComputedValue::Dim(browser_style::Dim::Auto),
            );
        }
        style.set(
            browser_style::Property::AspectRatio,
            browser_style::ComputedValue::AspectRatio(browser_style::AspectRatio::Auto),
        );
        style.set(
            browser_style::Property::FlexGrow,
            browser_style::ComputedValue::Number(0.0),
        );
        style.set(
            browser_style::Property::FlexShrink,
            browser_style::ComputedValue::Number(1.0),
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
        Display::Flex | Display::InlineFlex => BoxType::Flex,
        Display::Grid | Display::InlineGrid => BoxType::Grid,
        Display::Table => BoxType::Table,
        Display::TableRow => BoxType::TableRow,
        Display::TableCell => BoxType::TableCell,
        Display::Contents | Display::None => BoxType::Block,
    }
}

/// Resolves the width and height of an inline `<svg>` from its presentation
/// attributes and viewBox. Returns `(width, height, view_box)` where `view_box`
/// is `(width, height)` in user units when a valid viewBox is present.
///
/// The CSS replaced-element default is 300x150. A viewBox supplies the aspect
/// ratio when only one edge is declared; when neither edge is declared the box
/// keeps the 300x150 default (the viewBox letterboxes inside it).
fn svg_box_dimensions(doc: &Document, node: NodeId) -> (f32, f32, Option<(f32, f32)>) {
    const DEFAULT_WIDTH: f32 = 300.0;
    const DEFAULT_HEIGHT: f32 = 150.0;

    let parse_length = |key: &str| -> Option<f32> {
        let raw = doc.attr(node, key)?.trim();
        let number = raw.strip_suffix("px").unwrap_or(raw);
        let value = number.parse::<f32>().ok()?;
        (value.is_finite() && value > 0.0).then_some(value)
    };

    let view_box = doc.attr(node, "viewBox").and_then(|raw| {
        let values: Vec<f32> = raw
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse::<f32>().ok())
            .collect();
        match values.as_slice() {
            [_, _, w, h] if *w > 0.0 && *h > 0.0 => Some((*w, *h)),
            _ => None,
        }
    });
    let aspect = view_box.map(|(w, h)| w / h);

    let mut width = parse_length("width");
    let mut height = parse_length("height");
    match (width, height, aspect) {
        (None, None, _) => {
            width = Some(DEFAULT_WIDTH);
            height = Some(DEFAULT_HEIGHT);
        }
        (Some(w), None, Some(ratio)) => height = Some(w / ratio),
        (None, Some(h), Some(ratio)) => width = Some(h * ratio),
        (Some(_), None, None) => height = Some(DEFAULT_HEIGHT),
        (None, Some(_), None) => width = Some(DEFAULT_WIDTH),
        (Some(_), Some(_), _) => {}
    }
    (
        width.unwrap_or(DEFAULT_WIDTH),
        height.unwrap_or(DEFAULT_HEIGHT),
        view_box,
    )
}

/// Restores SVG camelCase attribute names that the HTML parser lowercased.
fn svg_attr_name(name: &str) -> &str {
    match name {
        "viewbox" => "viewBox",
        "preserveaspectratio" => "preserveAspectRatio",
        "gradientunits" => "gradientUnits",
        "gradienttransform" => "gradientTransform",
        "patternunits" => "patternUnits",
        "patterncontentunits" => "patternContentUnits",
        _ => name,
    }
}

/// Serializes an inline `<svg>` subtree into a source string for the SVG
/// rasterizer. The output is capped at [`MAX_SVG_SOURCE_BYTES`] and a bounded
/// recursion depth; `None` means the subtree was too large to keep.
fn serialize_svg_source(doc: &Document, root: NodeId, width: f32, height: f32) -> Option<String> {
    let mut out = String::with_capacity(256);
    write_svg_node(doc, root, width, height, 0, &mut out)?;
    (out.len() <= MAX_SVG_SOURCE_BYTES).then_some(out)
}

fn write_svg_node(
    doc: &Document,
    node: NodeId,
    width: f32,
    height: f32,
    depth: usize,
    out: &mut String,
) -> Option<()> {
    if depth > MAX_SVG_DEPTH || out.len() >= MAX_SVG_SOURCE_BYTES {
        return None;
    }
    match &doc.node(node).data {
        browser_dom::NodeData::Element(element) => {
            let is_root = depth == 0;
            out.push('<');
            out.push_str(&element.name);
            for (key, value) in element.attributes.iter() {
                let name = svg_attr_name(key);
                // The resolved CSS size is injected below; skip the raw attrs.
                if is_root && name.eq_ignore_ascii_case("width") {
                    continue;
                }
                if is_root && name.eq_ignore_ascii_case("height") {
                    continue;
                }
                out.push(' ');
                out.push_str(name);
                out.push_str("=\"");
                out.push_str(&escape_attr(value));
                out.push('"');
                if out.len() >= MAX_SVG_SOURCE_BYTES {
                    return None;
                }
            }
            if is_root {
                out.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
                out.push_str(" width=\"");
                out.push_str(&width.to_string());
                out.push_str("\" height=\"");
                out.push_str(&height.to_string());
                out.push('"');
            }
            out.push('>');
            for child in doc.children(node) {
                write_svg_node(doc, *child, width, height, depth + 1, out)?;
            }
            out.push_str("</");
            out.push_str(&element.name);
            out.push('>');
        }
        browser_dom::NodeData::Text(text) => {
            out.push_str(&escape_text(&text.data));
        }
        // Comments, doctypes and the document node carry no SVG geometry.
        _ => {}
    }
    (out.len() <= MAX_SVG_SOURCE_BYTES).then_some(())
}

/// True for boxes that only contain collapsible whitespace.
///
/// Flex and grid containers must not create items for those (they would each
/// become an empty item and distort the layout).
pub fn is_ignorable_child(tree: &LayoutTree, index: usize) -> bool {
    let Some(b) = tree.boxes.get(index) else {
        return true;
    };
    match b.box_type {
        BoxType::Text => b.text.trim().is_empty(),
        BoxType::AnonymousBlock => {
            let mut stack = b.children.clone();
            while let Some(child) = stack.pop() {
                let Some(node) = tree.boxes.get(child) else {
                    continue;
                };
                match node.box_type {
                    BoxType::Text if !node.text.trim().is_empty() => return false,
                    BoxType::Text => {}
                    _ => stack.extend(node.children.iter().copied()),
                }
            }
            true
        }
        _ => false,
    }
}

/// Resolves a dimension against a base length, defaulting to 0 for `auto`.
pub fn resolve_dim(dim: Dim, base: f32) -> f32 {
    dim.resolve(base).unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Logical properties
//
// The style crate only knows physical longhands (`margin-left`, `width`, ...):
// `Property::from_name("margin-inline-start")` returns `None` and the cascade
// drops such declarations.  We therefore read logical properties generically —
// first through `style.get(Property::from_name(name))` (so this keeps working if
// the style crate gains support later), then from the authored inline `style`
// attribute — and map them to physical properties with `direction` /
// `writing-mode`.  This runs once per node during tree building, so the rest of
// layout keeps using the ordinary physical accessors.
// ---------------------------------------------------------------------------

/// The physical sides that the four logical edges map to.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LogicalAxes {
    pub inline_start: Side,
    pub inline_end: Side,
    pub block_start: Side,
    pub block_end: Side,
    /// True in the usual horizontal writing modes (inline axis is horizontal).
    pub inline_is_horizontal: bool,
}

impl LogicalAxes {
    pub(crate) fn resolve(style: &ComputedStyle, doc: &Document, node: NodeId) -> Self {
        let rtl = matches!(
            style.get(Property::Direction).and_then(|v| v.as_keyword()),
            Some("rtl")
        );
        let writing_mode = logical_keyword(style, doc, node, "writing-mode")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let vertical = writing_mode.starts_with("vertical");
        let right_to_left_column = writing_mode.contains("-rl");

        if vertical {
            // In vertical writing modes the inline axis runs top→bottom.
            // `vertical-rl` stacks block columns right→left, `vertical-lr`
            // left→right.
            Self {
                inline_start: Side::Top,
                inline_end: Side::Bottom,
                block_start: if right_to_left_column {
                    Side::Right
                } else {
                    Side::Left
                },
                block_end: if right_to_left_column {
                    Side::Left
                } else {
                    Side::Right
                },
                inline_is_horizontal: false,
            }
        } else if rtl {
            Self {
                inline_start: Side::Right,
                inline_end: Side::Left,
                block_start: Side::Top,
                block_end: Side::Bottom,
                inline_is_horizontal: true,
            }
        } else {
            Self {
                inline_start: Side::Left,
                inline_end: Side::Right,
                block_start: Side::Top,
                block_end: Side::Bottom,
                inline_is_horizontal: true,
            }
        }
    }
}

/// Finds a declaration by property name in an inline `style` attribute.
fn inline_declaration<'a>(css: &'a str, name: &str) -> Option<&'a str> {
    for declaration in css.split(';') {
        let mut pair = declaration.splitn(2, ':');
        let key = pair.next()?.trim();
        if key.eq_ignore_ascii_case(name) {
            return pair.next().map(str::trim);
        }
    }
    None
}

/// Reads a keyword-valued logical property generically.
fn logical_keyword(
    style: &ComputedStyle,
    doc: &Document,
    node: NodeId,
    name: &str,
) -> Option<String> {
    if let Some(property) = Property::from_name(name)
        && let Some(value) = style.get(property).and_then(ComputedValue::as_keyword)
    {
        return Some(value.to_ascii_lowercase());
    }
    doc.attr(node, "style")
        .and_then(|css| inline_declaration(css, name))
        .map(|raw| raw.trim().to_ascii_lowercase())
}

/// Parses a `<length>` / `<percentage>` / `auto` value.
fn parse_length(text: &str) -> Option<Dim> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("auto") {
        return Some(Dim::Auto);
    }
    if let Some(rest) = text.strip_suffix('%') {
        return rest.trim().parse::<f32>().ok().map(Dim::Percent);
    }
    if let Some(rest) = text.strip_suffix("px") {
        return rest.trim().parse::<f32>().ok().map(Dim::Px);
    }
    text.parse::<f32>().ok().map(Dim::Px)
}

/// Reads a length-valued logical property generically.
fn logical_dim(style: &ComputedStyle, doc: &Document, node: NodeId, name: &str) -> Option<Dim> {
    if let Some(property) = Property::from_name(name)
        && let Some(value) = style.get(property)
    {
        return match value {
            ComputedValue::Dim(dim) => Some(*dim),
            ComputedValue::Px(px) | ComputedValue::Number(px) => Some(Dim::Px(*px)),
            // The initial value is `auto`; treating it as a real declaration
            // would overwrite an authored physical `width`/`height`.
            ComputedValue::Keyword(keyword) if keyword.as_ref() == "auto" => None,
            ComputedValue::Str(raw) if raw.eq_ignore_ascii_case("auto") => None,
            ComputedValue::Str(raw) => parse_length(raw),
            _ => None,
        };
    }
    doc.attr(node, "style")
        .and_then(|css| inline_declaration(css, name))
        .and_then(parse_length)
}

/// Applies a pair of logical edge values (with a two-value shorthand) to the
/// corresponding physical properties.
#[allow(clippy::too_many_arguments)]
fn apply_logical_pair(
    updates: &mut Vec<(Property, ComputedValue)>,
    style: &ComputedStyle,
    doc: &Document,
    node: NodeId,
    start_name: &str,
    end_name: &str,
    shorthand: &str,
    start_side: Side,
    end_side: Side,
    to_property: fn(Side) -> Property,
) {
    let start = logical_dim(style, doc, node, start_name);
    let end = logical_dim(style, doc, node, end_name);
    let parts = logical_keyword(style, doc, node, shorthand)
        .map(|raw| {
            raw.split_whitespace()
                .filter_map(parse_length)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let shorthand_start = parts.first().copied();
    let shorthand_end = parts.get(1).copied().or(shorthand_start);

    if let Some(dim) = start.or(shorthand_start) {
        updates.push((to_property(start_side), ComputedValue::Dim(dim)));
    }
    if let Some(dim) = end.or(shorthand_end) {
        updates.push((to_property(end_side), ComputedValue::Dim(dim)));
    }
}

fn inset_property(side: Side) -> Property {
    match side {
        Side::Top => Property::Top,
        Side::Right => Property::Right,
        Side::Bottom => Property::Bottom,
        Side::Left => Property::Left,
    }
}

/// Maps every supported logical property on `node` onto its physical
/// counterpart in `style`. Missing properties are left untouched.
pub(crate) fn apply_logical_properties(style: &mut ComputedStyle, doc: &Document, node: NodeId) {
    let axes = LogicalAxes::resolve(style, doc, node);
    let mut updates: Vec<(Property, ComputedValue)> = Vec::new();

    apply_logical_pair(
        &mut updates,
        style,
        doc,
        node,
        "margin-inline-start",
        "margin-inline-end",
        "margin-inline",
        axes.inline_start,
        axes.inline_end,
        browser_style::margin_property,
    );
    apply_logical_pair(
        &mut updates,
        style,
        doc,
        node,
        "margin-block-start",
        "margin-block-end",
        "margin-block",
        axes.block_start,
        axes.block_end,
        browser_style::margin_property,
    );
    apply_logical_pair(
        &mut updates,
        style,
        doc,
        node,
        "padding-inline-start",
        "padding-inline-end",
        "padding-inline",
        axes.inline_start,
        axes.inline_end,
        browser_style::padding_property,
    );
    apply_logical_pair(
        &mut updates,
        style,
        doc,
        node,
        "padding-block-start",
        "padding-block-end",
        "padding-block",
        axes.block_start,
        axes.block_end,
        browser_style::padding_property,
    );
    apply_logical_pair(
        &mut updates,
        style,
        doc,
        node,
        "inset-inline-start",
        "inset-inline-end",
        "inset-inline",
        axes.inline_start,
        axes.inline_end,
        inset_property,
    );
    apply_logical_pair(
        &mut updates,
        style,
        doc,
        node,
        "inset-block-start",
        "inset-block-end",
        "inset-block",
        axes.block_start,
        axes.block_end,
        inset_property,
    );

    // `inline-size` / `block-size` follow the writing mode's logical axes.
    if let Some(dim) = logical_dim(style, doc, node, "inline-size") {
        let property = if axes.inline_is_horizontal {
            Property::Width
        } else {
            Property::Height
        };
        updates.push((property, ComputedValue::Dim(dim)));
    }
    if let Some(dim) = logical_dim(style, doc, node, "block-size") {
        let property = if axes.inline_is_horizontal {
            Property::Height
        } else {
            Property::Width
        };
        updates.push((property, ComputedValue::Dim(dim)));
    }

    // Border widths: prefer the explicit `-width` longhand, then the logical
    // shorthand (`border-inline-start: 2px solid ...`).
    for (longhand, shorthand, side) in [
        (
            "border-inline-start-width",
            "border-inline-start",
            axes.inline_start,
        ),
        (
            "border-inline-end-width",
            "border-inline-end",
            axes.inline_end,
        ),
        (
            "border-block-start-width",
            "border-block-start",
            axes.block_start,
        ),
        ("border-block-end-width", "border-block-end", axes.block_end),
    ] {
        let width = logical_dim(style, doc, node, longhand)
            .or_else(|| {
                let raw = logical_keyword(style, doc, node, shorthand)?;
                let first = raw.split_whitespace().next()?;
                match first {
                    "thin" => Some(Dim::Px(1.0)),
                    "medium" => Some(Dim::Px(3.0)),
                    "thick" => Some(Dim::Px(5.0)),
                    _ => parse_length(first),
                }
            })
            .and_then(|dim| match dim {
                Dim::Px(px) => Some(px),
                _ => None,
            });
        if let Some(px) = width {
            updates.push((
                browser_style::border_width_property(side),
                ComputedValue::Px(px),
            ));
        }
        // A border shorthand also carries a style keyword; without it the
        // width alone would not paint (the style crate hides invisible borders).
        if let Some(raw) = logical_keyword(style, doc, node, shorthand) {
            for token in raw.split_whitespace() {
                let keyword = match token {
                    "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset"
                    | "outset" | "hidden" | "none" => token,
                    _ => continue,
                };
                if let Some(property) =
                    Property::from_name(&format!("border-{}-style", side.suffix()))
                {
                    updates.push((property, ComputedValue::keyword(keyword)));
                }
            }
        }
    }

    for (property, value) in updates {
        style.set(property, value);
    }
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
    fn input_value_becomes_a_text_child() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let input = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, input);
        doc.set_attr(input, "type", "submit");
        doc.set_attr(input, "value", "Pesquisa Google");
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let input_box = tree.find_node(input).unwrap();
        assert_eq!(input_box.children.len(), 1);
        assert_eq!(tree.boxes[input_box.children[0]].text, "Pesquisa Google");
    }

    #[test]
    fn input_size_attribute_is_a_presentational_width_hint() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let input = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, input);
        doc.set_attr(input, "type", "text");
        doc.set_attr(input, "size", "57");
        doc.set_attr(input, "value", "hello");
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let input_box = tree.find_node(input).unwrap();
        // 57 characters * 8px, and the synthetic value-text child survives.
        assert_eq!(input_box.style.width(), Dim::Px(456.0));
        assert_eq!(input_box.children.len(), 1);
        assert_eq!(tree.boxes[input_box.children[0]].text, "hello");
    }

    #[test]
    fn css_width_overrides_input_size_hint() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let input = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, input);
        doc.set_attr(input, "class", "Ucigb");
        doc.set_attr(input, "size", "57");
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(".Ucigb { width: 458px; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let input_box = tree.find_node(input).unwrap();
        assert_eq!(input_box.style.width(), Dim::Px(458.0));
    }

    #[test]
    fn hidden_input_has_no_text_child() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let input = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, input);
        doc.set_attr(input, "type", "hidden");
        doc.set_attr(input, "value", "secret");
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        assert!(tree.find_node(input).is_none());
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

    fn div_doc() -> (Document, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let div = doc.create_element("div");
        doc.append_child(body, div);
        doc.append_text(div, "hello");
        (doc, div)
    }

    #[test]
    fn before_pseudo_produces_a_leading_text_box() {
        let (doc, div) = div_doc();
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css("div::before { content: \">>\"; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let div_box = tree.find_node(div).unwrap();
        assert_eq!(div_box.children.len(), 2);
        assert_eq!(tree.boxes[div_box.children[0]].box_type, BoxType::Text);
        assert_eq!(tree.boxes[div_box.children[0]].text, ">>");
        assert_eq!(tree.boxes[div_box.children[1]].text, "hello");
    }

    #[test]
    fn after_pseudo_appends_a_text_box() {
        let (doc, div) = div_doc();
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css("div::after { content: \"!\"; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let div_box = tree.find_node(div).unwrap();
        assert_eq!(div_box.children.len(), 2);
        assert_eq!(tree.boxes[div_box.children[0]].text, "hello");
        assert_eq!(tree.boxes[div_box.children[1]].text, "!");
    }

    #[test]
    fn content_none_produces_no_pseudo_box() {
        let (doc, div) = div_doc();
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(
            "div::before { content: none; } div::after { content: \"!\"; }",
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let div_box = tree.find_node(div).unwrap();
        assert_eq!(div_box.children.len(), 2);
        assert_eq!(tree.boxes[div_box.children[0]].text, "hello");
        assert_eq!(tree.boxes[div_box.children[1]].text, "!");
    }

    #[test]
    fn pseudo_text_box_uses_the_declared_color() {
        let (doc, div) = div_doc();
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(
            "div::before { content: \">>\"; color: #123456; }",
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let div_box = tree.find_node(div).unwrap();
        let before = &tree.boxes[div_box.children[0]];
        assert_eq!(
            before.style.color(),
            browser_style::Color::rgb(0x12, 0x34, 0x56)
        );
    }

    #[test]
    fn pseudo_text_appears_as_a_leading_fragment_after_layout() {
        let (doc, div) = div_doc();
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(
            "div::before { content: \">>\"; } div::after { content: \"!\"; }",
            Origin::Author,
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
        let div_box = tree.find_node(div).unwrap();
        let line = &div_box.lines[0];
        let texts: Vec<&str> = line.fragments.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, [">>", "hello", "!"]);
    }

    #[test]
    fn replaced_elements_have_no_pseudo_boxes() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        for tag in ["img", "input"] {
            let element = doc.create_element(tag);
            doc.append_child(body, element);
        }
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(
            "img::before, input::before { content: \">>\"; }",
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        for tag in ["img", "input"] {
            let node = doc
                .descendants(doc.root())
                .find(|node| doc.is_tag(*node, tag))
                .unwrap();
            let b = tree.find_node(node).unwrap();
            assert!(b.children.is_empty(), "{tag} generated a pseudo box");
        }
    }

    fn svg_doc(width: &str, height: &str, view_box: Option<&str>) -> (Document, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let svg = doc.create_element("svg");
        if !width.is_empty() {
            doc.set_attr(svg, "width", width);
        }
        if !height.is_empty() {
            doc.set_attr(svg, "height", height);
        }
        if let Some(view_box) = view_box {
            doc.set_attr(svg, "viewBox", view_box);
        }
        doc.append_child(body, svg);
        let rect = doc.create_element("rect");
        doc.set_attr(rect, "width", "40");
        doc.set_attr(rect, "height", "20");
        doc.set_attr(rect, "fill", "red");
        doc.append_child(svg, rect);
        (doc, svg)
    }

    #[test]
    fn inline_svg_is_an_atomic_image_box_with_serialized_source() {
        let (doc, svg) = svg_doc("40", "20", None);
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let b = tree.find_node(svg).unwrap();
        assert_eq!(b.box_type, BoxType::Image);
        assert!(b.children.is_empty(), "SVG children must stay atomic");
        assert_eq!(b.style.width(), Dim::Px(40.0));
        assert_eq!(b.style.height(), Dim::Px(20.0));
        let source = b.svg_source.as_deref().expect("serialized SVG source");
        assert!(source.starts_with("<svg"), "{source}");
        assert!(source.contains("<rect"), "{source}");
        assert!(source.contains("width=\"40\""), "{source}");
        assert!(source.contains("height=\"20\""), "{source}");
        assert!(!source.contains("viewBox"), "{source}");
    }

    #[test]
    fn inline_svg_defaults_to_300x150_and_restores_viewbox_case() {
        let (doc, svg) = svg_doc("", "", Some("0 0 10 5"));
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let b = tree.find_node(svg).unwrap();
        assert_eq!(b.style.width(), Dim::Px(300.0));
        assert_eq!(b.style.height(), Dim::Px(150.0));
        assert_eq!(
            b.style.aspect_ratio(),
            browser_style::AspectRatio::Ratio(10.0, 5.0)
        );
        let source = b.svg_source.as_deref().unwrap();
        assert!(source.contains("viewBox=\"0 0 10 5\""), "{source}");
    }

    #[test]
    fn inline_svg_single_dimension_uses_viewbox_aspect() {
        let (doc, svg) = svg_doc("40", "", Some("0 0 10 5"));
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let b = tree.find_node(svg).unwrap();
        assert_eq!(b.style.width(), Dim::Px(40.0));
        assert_eq!(b.style.height(), Dim::Px(20.0));
    }

    #[test]
    fn anonymous_wrappers_do_not_multiply_a_parent_min_height() {
        // A block container with inline runs separated by block children gets
        // one anonymous wrapper per run. Before the fix each wrapper inherited
        // the parent's `min-height`, so a 240px panel with three runs measured
        // over 700px tall. The wrappers must be sized by their text only.
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let panel = doc.create_element("div");
        doc.set_attr(panel, "id", "panel");
        doc.append_child(body, panel);
        doc.append_text(panel, "alpha");
        let block = doc.create_element("div");
        doc.append_child(panel, block);
        doc.append_text(panel, "beta");
        let block2 = doc.create_element("div");
        doc.append_child(panel, block2);
        doc.append_text(panel, "gamma");

        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(
            "body { margin:0; } #panel { min-height:240px; } #panel > div { height:10px; }",
            Origin::Author,
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
        let panel_box = tree.find_node(panel).unwrap();
        assert!(
            panel_box.content.height <= 260.0,
            "anonymous wrappers multiplied min-height: {}",
            panel_box.content.height
        );
        for &child in &panel_box.children {
            let wrapper = &tree.boxes[child];
            if wrapper.box_type == BoxType::AnonymousBlock {
                assert_eq!(
                    wrapper.style.dim(browser_style::Property::MinHeight),
                    browser_style::Dim::Auto,
                    "anonymous wrapper inherited min-height"
                );
            }
        }
    }

    #[test]
    fn out_of_flow_boxes_do_not_extend_content_height() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let normal = doc.create_element("div");
        doc.append_child(body, normal);
        let fixed = doc.create_element("div");
        doc.set_attr(fixed, "id", "fixed");
        doc.append_child(body, fixed);

        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css(
            "body { margin:0; } #normal { height:100px; } \
             #fixed { position:fixed; top:9000px; width:10px; height:10px; }",
            Origin::Author,
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
        assert!(
            tree.content_height() <= 200.0,
            "fixed box inflated content height: {}",
            tree.content_height()
        );
    }

    #[test]
    fn inline_svg_has_definite_geometry_after_layout() {
        let (doc, svg) = svg_doc("40", "20", None);
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: 800.0,
                height: 600.0,
            },
        );
        let b = tree.find_node(svg).unwrap();
        assert_eq!(b.rect.width, 40.0);
        assert_eq!(b.rect.height, 20.0);
    }

    #[test]
    fn logical_margin_inline_start_maps_to_left_in_ltr() {
        // The style crate drops `margin-inline-start`, so the tree builder must
        // read it from the authored style and map inline-start → left in LTR.
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let div = doc.create_element("div");
        doc.set_attr(
            div,
            "style",
            "margin-inline-start: 24px; width: 10px; height: 10px",
        );
        doc.append_child(body, div);
        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css("body { margin: 0; }", Origin::Author);
        let styled = engine.compute(&doc);

        let tree = TreeBuilder::new(&doc, &styled).build();
        let b = tree.find_node(div).unwrap();
        assert_eq!(b.style.margin(Side::Left), Dim::Px(24.0));
        assert_eq!(b.style.margin(Side::Right), Dim::Px(0.0));

        let tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: 800.0,
                height: 600.0,
            },
        );
        let b = tree.find_node(div).unwrap();
        assert_eq!(b.margins.left, 24.0, "inline-start did not map to left");
        assert_eq!(b.rect.x, 24.0);
    }

    #[test]
    fn logical_inline_size_maps_to_width_in_horizontal_writing_mode() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let div = doc.create_element("div");
        doc.set_attr(div, "style", "inline-size: 120px; block-size: 40px");
        doc.append_child(body, div);
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let tree = TreeBuilder::new(&doc, &styled).build();
        let b = tree.find_node(div).unwrap();
        assert_eq!(b.style.width(), Dim::Px(120.0));
        assert_eq!(b.style.height(), Dim::Px(40.0));
    }
}
