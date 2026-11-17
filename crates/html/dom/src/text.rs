//! Text content accessors and text-node helpers.

use crate::document::Document;
use crate::node::{NodeData, NodeId};

impl Document {
    /// Concatenated text of a node's descendants (comments excluded).
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.node(id).data {
            NodeData::Text(t) => out.push_str(&t.data),
            NodeData::Comment(_) | NodeData::Doctype { .. } | NodeData::Document => {}
            NodeData::Element(_) => {
                for child in &self.node(id).children {
                    self.collect_text(*child, out);
                }
            }
        }
    }

    /// Replaces the children of `id` with a single text node.
    ///
    /// Empty text removes all children. Returns `false` for text/document nodes.
    pub fn set_text_content(&mut self, id: NodeId, text: &str) -> bool {
        match self.node(id).data {
            NodeData::Element(_) | NodeData::Document => {}
            _ => return false,
        }
        let children = self.node(id).children.clone();
        for child in children {
            self.remove_child(id, child);
        }
        if !text.is_empty() {
            let node = self.create_text(text);
            self.append_child(id, node);
        }
        true
    }

    /// Appends a text node with `text` to `parent`.
    pub fn append_text(&mut self, parent: NodeId, text: &str) -> NodeId {
        let node = self.create_text(text);
        self.append_child(parent, node);
        node
    }

    /// Mutates the text data of a text node.
    pub fn set_text_data(&mut self, id: NodeId, data: &str) -> bool {
        match &mut self.node_mut(id).data {
            NodeData::Text(t) => {
                t.data = data.to_string();
                true
            }
            _ => false,
        }
    }

    /// Appends text to an existing text node.
    pub fn push_text(&mut self, id: NodeId, extra: &str) -> bool {
        match &mut self.node_mut(id).data {
            NodeData::Text(t) => {
                t.data.push_str(extra);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_concatenates_descendants() {
        let mut doc = Document::new();
        let p = doc.create_element("p");
        let b = doc.create_element("b");
        doc.append_child(p, b);
        doc.append_text(b, "Hel");
        doc.append_text(p, "lo ");
        let comment = doc.create_comment("ignored");
        doc.append_child(p, comment);
        doc.append_text(p, "world");
        assert_eq!(doc.text_content(p), "Hello world");
    }

    #[test]
    fn set_text_content_clears_children() {
        let mut doc = Document::new();
        let p = doc.create_element("p");
        doc.append_text(p, "old");
        assert!(doc.set_text_content(p, "new"));
        assert_eq!(doc.text_content(p), "new");
        assert_eq!(doc.node(p).children.len(), 1);
        assert!(doc.set_text_content(p, ""));
        assert!(doc.node(p).children.is_empty());
    }
}
