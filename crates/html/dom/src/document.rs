//! The document arena, node creation and core tree queries.

use crate::attributes::Attributes;
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
    /// Creates an empty document with only the document node.
    pub fn new() -> Self {
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
            base_url: None,
            title: String::new(),
        }
    }

    /// The document root node.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Total number of nodes, including removed ones.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Allocates a new detached node and returns its id.
    fn alloc(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(Node {
            id,
            parent: None,
            children: Vec::new(),
            data,
        });
        id
    }

    /// Creates an HTML element (tag name lowercased).
    pub fn create_element(&mut self, name: &str) -> NodeId {
        self.create_element_ns(name, Namespace::Html)
    }

    /// Creates an element in a specific namespace.
    pub fn create_element_ns(&mut self, name: &str, namespace: Namespace) -> NodeId {
        let name = if namespace == Namespace::Html {
            name.to_ascii_lowercase()
        } else {
            name.to_string()
        };
        self.alloc(NodeData::Element(ElementData {
            name,
            namespace,
            attributes: Attributes::new(),
        }))
    }

    /// Creates a text node.
    pub fn create_text(&mut self, data: &str) -> NodeId {
        self.alloc(NodeData::Text(TextData {
            data: data.to_string(),
        }))
    }

    /// Creates a comment node.
    pub fn create_comment(&mut self, data: &str) -> NodeId {
        self.alloc(NodeData::Comment(data.to_string()))
    }

    /// Creates a doctype node.
    pub fn create_doctype(&mut self, name: &str) -> NodeId {
        self.alloc(NodeData::Doctype {
            name: name.to_ascii_lowercase(),
            public_id: String::new(),
            system_id: String::new(),
        })
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
    pub fn is_ancestor_of(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = self.nodes[node.0].parent;
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            current = self.nodes[id.0].parent;
        }
        false
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
}
