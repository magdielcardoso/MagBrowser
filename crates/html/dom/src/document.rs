//! The document arena, node creation and core tree queries.

use crate::attributes::Attributes;
use crate::limits::Limits;
use crate::node::{ElementData, Namespace, Node, NodeData, NodeId, TextData};

/// A parsed document.
///
/// Nodes live in a flat arena and reference each other by [`NodeId`]. This keeps
/// traversal cache-friendly and lets the layout and style engines hold plain ids
/// without borrow-checker friction.
#[derive(Debug, Clone)]
pub struct Document {
    nodes: Vec<Node>,
    root: NodeId,
    limits: Limits,
    /// Base URL of the document (from the navigation or `<base href>`).
    pub base_url: Option<String>,
    /// Document title cache (updated by the HTML parser).
    pub title: String,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// Creates an empty document with only the document node and default limits.
    pub fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    /// Creates an empty document with custom resource [`Limits`].
    pub fn with_limits(limits: Limits) -> Self {
        let root = NodeId(0);
        let nodes = vec![Node {
            id: root,
            parent: None,
            children: Vec::new(),
            data: NodeData::Document,
        }];
        Self {
            nodes,
            root,
            limits,
            base_url: None,
            title: String::new(),
        }
    }

    /// Resource caps currently applied to this document.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Replaces the resource caps. Existing nodes are not removed, but all
    /// subsequent allocations and mutations respect the new values.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// The document root node.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Total number of nodes, including removed ones.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// True when `id` refers to a node in this document.
    pub fn contains(&self, id: NodeId) -> bool {
        id.0 < self.nodes.len()
    }

    /// Allocates a new detached node, returning `None` when the node cap is
    /// reached.
    fn try_alloc(&mut self, data: NodeData) -> Option<NodeId> {
        if self.nodes.len() >= self.limits.max_nodes {
            return None;
        }
        let id = NodeId(self.nodes.len());
        self.nodes.push(Node {
            id,
            parent: None,
            children: Vec::new(),
            data,
        });
        Some(id)
    }

    /// Tries to create an HTML element (tag name lowercased).
    ///
    /// Returns `None` instead of allocating when the node cap is reached.
    pub fn try_create_element(&mut self, name: &str) -> Option<NodeId> {
        self.try_create_element_ns(name, Namespace::Html)
    }

    /// Tries to create an element in a specific namespace.
    pub fn try_create_element_ns(&mut self, name: &str, namespace: Namespace) -> Option<NodeId> {
        let name = normalize_name(name, namespace);
        let attributes = Attributes::with_limit(self.limits.max_attributes_per_element);
        self.try_alloc(NodeData::Element(ElementData {
            name,
            namespace,
            attributes,
        }))
    }

    /// Tries to create a text node, truncating `data` to the configured text
    /// byte limit. Returns `None` when the node cap is reached.
    pub fn try_create_text(&mut self, data: &str) -> Option<NodeId> {
        let data = self.limits.clamp_text(data).to_string();
        self.try_alloc(NodeData::Text(TextData { data }))
    }

    /// Tries to create a comment node. Returns `None` when the node cap is reached.
    pub fn try_create_comment(&mut self, data: &str) -> Option<NodeId> {
        self.try_alloc(NodeData::Comment(data.to_string()))
    }

    /// Tries to create a doctype node. Returns `None` when the node cap is reached.
    pub fn try_create_doctype(&mut self, name: &str) -> Option<NodeId> {
        self.try_alloc(NodeData::Doctype {
            name: name.to_ascii_lowercase(),
            public_id: String::new(),
            system_id: String::new(),
        })
    }

    /// Creates an HTML element (tag name lowercased).
    ///
    /// When the node cap is reached the allocation is ignored and the document
    /// root is returned. Use [`Document::try_create_element`] to detect this.
    pub fn create_element(&mut self, name: &str) -> NodeId {
        self.try_create_element(name).unwrap_or(self.root)
    }

    /// Creates an element in a specific namespace.
    ///
    /// See [`Document::create_element`] for the behaviour at the node cap.
    pub fn create_element_ns(&mut self, name: &str, namespace: Namespace) -> NodeId {
        self.try_create_element_ns(name, namespace)
            .unwrap_or(self.root)
    }

    /// Creates a text node, truncating oversized data to the configured limit.
    ///
    /// See [`Document::create_element`] for the behaviour at the node cap.
    pub fn create_text(&mut self, data: &str) -> NodeId {
        self.try_create_text(data).unwrap_or(self.root)
    }

    /// Creates a comment node.
    ///
    /// See [`Document::create_element`] for the behaviour at the node cap.
    pub fn create_comment(&mut self, data: &str) -> NodeId {
        self.try_create_comment(data).unwrap_or(self.root)
    }

    /// Creates a doctype node.
    ///
    /// See [`Document::create_element`] for the behaviour at the node cap.
    pub fn create_doctype(&mut self, name: &str) -> NodeId {
        self.try_create_doctype(name).unwrap_or(self.root)
    }

    /// Borrows a node.
    ///
    /// # Panics
    /// Panics when `id` does not belong to this document.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    /// Mutably borrows a node.
    ///
    /// # Panics
    /// Panics when `id` does not belong to this document.
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0]
    }

    /// Iterates over every allocated node.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter()
    }

    /// The `<html>` element, when present.
    pub fn document_element(&self) -> Option<NodeId> {
        self.nodes[self.root.0]
            .children
            .iter()
            .copied()
            .find(|id| self.nodes[id.0].is_element())
    }

    /// The `<head>` and `<body>` elements, when present.
    pub fn head_and_body(&self) -> (Option<NodeId>, Option<NodeId>) {
        let mut head = None;
        let mut body = None;
        if let Some(html) = self.document_element() {
            for child in &self.nodes[html.0].children {
                match self.nodes[child.0].element_name() {
                    Some("head") => head = Some(*child),
                    Some("body") => body = Some(*child),
                    _ => {}
                }
            }
        }
        (head, body)
    }

    /// First element with the given `id` attribute, in document order.
    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.nodes.iter().find_map(|node| {
            node.element()
                .filter(|e| e.attributes.get("id") == Some(id))
                .map(|_| node.id)
        })
    }

    /// Every element with the given tag name (case-insensitive).
    pub fn get_elements_by_tag_name(&self, name: &str) -> Vec<NodeId> {
        let name = name.to_ascii_lowercase();
        self.nodes
            .iter()
            .filter(|n| {
                n.element_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&name) || name == "*")
            })
            .map(|n| n.id)
            .collect()
    }

    /// Every element carrying all of the given class names.
    pub fn get_elements_by_class_name(&self, classes: &str) -> Vec<NodeId> {
        let wanted: Vec<&str> = classes.split_ascii_whitespace().collect();
        self.nodes
            .iter()
            .filter(|n| {
                n.element().is_some_and(|e| {
                    let list = e.attributes.class_list();
                    wanted.iter().all(|c| list.contains(c))
                })
            })
            .map(|n| n.id)
            .collect()
    }

    /// True when `ancestor` is a strict ancestor of `node`.
    ///
    /// Unknown ids and a corrupted parent chain are treated as "not an
    /// ancestor" instead of panicking or looping forever.
    pub fn is_ancestor_of(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = self.nodes.get(node.0).and_then(|n| n.parent);
        let mut hops = 0;
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            hops += 1;
            if hops > self.nodes.len() {
                return false;
            }
            current = self.nodes.get(id.0).and_then(|n| n.parent);
        }
        false
    }

    /// True when `child` can be attached under `parent` without creating a
    /// cycle or moving the document node.
    pub fn can_attach(&self, parent: NodeId, child: NodeId) -> bool {
        self.valid_pair(parent, child)
    }

    /// Detaches `child` from its current parent without re-attaching it.
    pub fn detach(&mut self, child: NodeId) -> bool {
        let Some(parent) = self.nodes[child.0].parent else {
            return false;
        };
        self.nodes[parent.0].children.retain(|id| *id != child);
        self.nodes[child.0].parent = None;
        true
    }

    /// Appends `child` as the last child of `parent`.
    ///
    /// Returns `false` (leaving the tree unchanged) when the operation would
    /// create a cycle or involve an unknown node.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        if !self.valid_pair(parent, child) {
            return false;
        }
        self.detach(child);
        self.nodes[parent.0].children.push(child);
        self.nodes[child.0].parent = Some(parent);
        true
    }

    /// Inserts `child` before `reference` inside `parent`.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: NodeId) -> bool {
        if !self.valid_pair(parent, child) {
            return false;
        }
        let Some(index) = self.nodes[parent.0]
            .children
            .iter()
            .position(|id| *id == reference)
        else {
            return false;
        };
        self.detach(child);
        let index = self.nodes[parent.0]
            .children
            .iter()
            .position(|id| *id == reference)
            .unwrap_or(index.min(self.nodes[parent.0].children.len()));
        self.nodes[parent.0].children.insert(index, child);
        self.nodes[child.0].parent = Some(parent);
        true
    }

    /// Removes `child` from `parent`. Returns `false` when it is not a child.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        if self.nodes.get(child.0).is_none() || self.nodes.get(parent.0).is_none() {
            return false;
        }
        if self.nodes[child.0].parent != Some(parent) {
            return false;
        }
        self.nodes[parent.0].children.retain(|id| *id != child);
        self.nodes[child.0].parent = None;
        true
    }

    /// Replaces `old` (a child of `parent`) with `new`.
    pub fn replace_child(&mut self, parent: NodeId, new: NodeId, old: NodeId) -> bool {
        if !self.valid_pair(parent, new) {
            return false;
        }
        let Some(index) = self.nodes[parent.0]
            .children
            .iter()
            .position(|id| *id == old)
        else {
            return false;
        };
        self.detach(new);
        let index = self.nodes[parent.0]
            .children
            .iter()
            .position(|id| *id == old)
            .unwrap_or(index.min(self.nodes[parent.0].children.len()));
        self.nodes[parent.0].children[index] = new;
        self.nodes[new.0].parent = Some(parent);
        self.nodes[old.0].parent = None;
        true
    }

    fn valid_pair(&self, parent: NodeId, child: NodeId) -> bool {
        parent.0 < self.nodes.len()
            && child.0 < self.nodes.len()
            && parent != child
            && !self.is_ancestor_of(child, parent)
            && !matches!(self.nodes[child.0].data, NodeData::Document)
    }
}

fn normalize_name(name: &str, namespace: Namespace) -> String {
    if namespace == Namespace::Html {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Document, NodeId, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        (doc, html, body)
    }

    #[test]
    fn builds_and_queries_tree() {
        let (mut doc, html, body) = sample();
        assert_eq!(doc.document_element(), Some(html));
        assert_eq!(doc.head_and_body(), (None, Some(body)));
        let text = doc.create_text("hello");
        doc.append_child(body, text);
        assert_eq!(doc.node(body).children.len(), 1);
        assert_eq!(doc.node(text).text(), Some("hello"));
        assert_eq!(doc.node_count(), 4);
    }

    #[test]
    fn rejects_cycles_and_document_moves() {
        let (mut doc, html, body) = sample();
        assert!(!doc.append_child(body, html));
        assert!(!doc.append_child(html, doc.root()));
        assert!(!doc.append_child(html, html));
        assert_eq!(doc.node(html).parent, Some(doc.root()));
    }

    #[test]
    fn insert_remove_and_replace() {
        let (mut doc, _html, body) = sample();
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        let c = doc.create_element("c");
        doc.append_child(body, a);
        doc.append_child(body, c);
        assert!(doc.insert_before(body, b, c));
        assert_eq!(doc.node(body).children, vec![a, b, c]);
        assert!(doc.remove_child(body, b));
        assert_eq!(doc.node(body).children, vec![a, c]);
        assert!(doc.replace_child(body, b, c));
        assert_eq!(doc.node(body).children, vec![a, b]);
        assert_eq!(doc.node(c).parent, None);
    }

    #[test]
    fn queries_by_id_class_and_tag() {
        let (mut doc, _html, body) = sample();
        let div = doc.create_element("div");
        doc.node_mut(div)
            .element_mut()
            .unwrap()
            .attributes
            .set("id", "main");
        doc.node_mut(div)
            .element_mut()
            .unwrap()
            .attributes
            .set("class", "box wide");
        doc.append_child(body, div);
        assert_eq!(doc.get_element_by_id("main"), Some(div));
        assert_eq!(doc.get_elements_by_tag_name("DIV"), vec![div]);
        assert_eq!(doc.get_elements_by_class_name("wide"), vec![div]);
        assert!(doc.get_elements_by_class_name("nope").is_empty());
    }

    #[test]
    fn limits_are_configurable() {
        let mut doc = Document::new();
        assert_eq!(doc.limits(), Limits::default());

        let custom = Limits {
            max_nodes: 8,
            max_attributes_per_element: 3,
            max_text_bytes: 16,
        };
        doc.set_limits(custom);
        assert_eq!(doc.limits(), custom);

        let other = Document::with_limits(custom);
        assert_eq!(other.limits(), custom);
    }

    #[test]
    fn node_cap_stops_allocation_gracefully() {
        // Root plus three more nodes.
        let limits = Limits {
            max_nodes: 4,
            ..Limits::unlimited()
        };
        let mut doc = Document::with_limits(limits);

        let a = doc.try_create_element("a").expect("first node fits");
        let b = doc.try_create_text("b").expect("second node fits");
        let c = doc.try_create_comment("c").expect("third node fits");
        assert_eq!(doc.node_count(), 4);

        // All fallible constructors fail once the cap is reached.
        assert_eq!(doc.try_create_element("d"), None);
        assert_eq!(doc.try_create_text("d"), None);
        assert_eq!(doc.try_create_comment("d"), None);
        assert_eq!(doc.try_create_doctype("d"), None);
        assert_eq!(doc.node_count(), 4);

        // Infallible constructors return the inert document root instead of
        // panicking or growing the arena.
        assert_eq!(doc.create_element("d"), doc.root());
        assert_eq!(doc.create_text("d"), doc.root());
        assert_eq!(doc.node_count(), 4);

        // The placeholder cannot be attached or mutated as an element.
        assert!(!doc.append_child(a, doc.root()));
        assert!(!doc.set_attr(doc.root(), "id", "x"));
        assert_eq!(doc.node_count(), 4);
        assert_eq!(doc.node(a).id, a);
        assert_eq!(doc.node(b).id, b);
        assert_eq!(doc.node(c).id, c);
    }

    #[test]
    fn lowering_node_cap_does_not_remove_existing_nodes() {
        let mut doc = Document::new();
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        doc.set_limits(Limits {
            max_nodes: doc.node_count(),
            ..doc.limits()
        });
        assert_eq!(doc.try_create_element("c"), None);
        assert!(doc.contains(a));
        assert!(doc.contains(b));
        assert!(!doc.contains(NodeId(doc.node_count())));
    }

    #[test]
    fn cycles_are_rejected_for_append_insert_and_replace() {
        let (mut doc, html, body) = sample();
        let div = doc.create_element("div");
        doc.append_child(body, div);

        assert!(doc.is_ancestor_of(html, div));
        assert!(!doc.is_ancestor_of(div, html));

        // Ancestor into descendant (and the reverse) are both refused.
        assert!(!doc.can_attach(div, html));
        assert!(!doc.append_child(div, html));
        assert!(!doc.insert_before(div, html, body));
        assert!(!doc.replace_child(div, html, body));

        // Self and unknown ids are refused without panicking.
        assert!(!doc.append_child(div, div));
        assert!(!doc.append_child(div, NodeId(usize::MAX)));
        assert!(!doc.append_child(NodeId(usize::MAX), div));
        assert!(!doc.is_ancestor_of(NodeId(usize::MAX), div));
        assert!(!doc.is_ancestor_of(div, NodeId(usize::MAX)));

        // The original tree is untouched.
        assert_eq!(doc.parent(html), Some(doc.root()));
        assert_eq!(doc.parent(body), Some(html));
        assert_eq!(doc.parent(div), Some(body));
    }

    #[test]
    fn deep_tree_stays_well_formed_and_cycle_checks_are_bounded() {
        let mut doc = Document::new();
        let mut parent = doc.root();
        let mut nodes = Vec::new();
        for _ in 0..2_000 {
            let child = doc.create_element("div");
            assert!(doc.append_child(parent, child));
            nodes.push(child);
            parent = child;
        }
        assert!(doc.is_ancestor_of(nodes[0], *nodes.last().unwrap()));
        assert!(!doc.append_child(*nodes.last().unwrap(), nodes[0]));
        assert_eq!(doc.text_content(doc.root()), "");
    }
}
