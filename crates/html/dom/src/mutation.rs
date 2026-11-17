//! Structural mutations used by the HTML parser and DOM APIs.

use crate::document::Document;
use crate::node::{NodeData, NodeId};

impl Document {
    /// Removes every child of `id`.
    pub fn clear_children(&mut self, id: NodeId) -> usize {
        let children = self.node(id).children.clone();
        let count = children.len();
        for child in children {
            self.remove_child(id, child);
        }
        count
    }

    /// Replaces all children of `parent` with `new_children`.
    pub fn replace_children(&mut self, parent: NodeId, new_children: Vec<NodeId>) -> bool {
        for child in new_children {
            if !self.append_child(parent, child) {
                return false;
            }
        }
        true
    }

    /// Moves every child of `from` to `to`, preserving order.
    pub fn move_children(&mut self, from: NodeId, to: NodeId) -> usize {
        let children = self.node(from).children.clone();
        for child in &children {
            self.append_child(to, *child);
        }
        children.len()
    }

    /// Merges a text node with its previous sibling when both are text.
    pub fn merge_text_with_previous(&mut self, id: NodeId) -> bool {
        let Some(previous) = self.previous_sibling(id) else {
            return false;
        };
        let data = match &self.node(id).data {
            NodeData::Text(t) => t.data.clone(),
            _ => return false,
        };
        if !self.node(previous).is_text() {
            return false;
        }
        self.push_text(previous, &data);
        self.remove_child(self.node(id).parent.unwrap_or(previous), id)
    }

    /// Appends text to `parent`, merging with an existing trailing text node.
    pub fn append_text_merged(&mut self, parent: NodeId, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.last_child(parent)
            && self.node(last).is_text()
            && self.push_text(last, text)
        {
            return;
        }
        self.append_text(parent, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_adjacent_text() {
        let mut doc = Document::new();
        let p = doc.create_element("p");
        doc.append_text_merged(p, "a");
        doc.append_text_merged(p, "b");
        assert_eq!(doc.node(p).children.len(), 1);
        assert_eq!(doc.text_content(p), "ab");
    }

    #[test]
    fn moves_and_clears_children() {
        let mut doc = Document::new();
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        let x = doc.create_text("x");
        doc.append_child(a, x);
        let y = doc.create_text("y");
        doc.append_child(a, y);
        assert_eq!(doc.move_children(a, b), 2);
        assert!(doc.node(a).children.is_empty());
        assert_eq!(doc.text_content(b), "xy");
        assert_eq!(doc.clear_children(b), 2);
        assert!(doc.node(b).children.is_empty());
    }
}
