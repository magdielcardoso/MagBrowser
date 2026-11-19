//! Text content accessors and text-node helpers.

use crate::document::Document;
use crate::limits::clamp_str;
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
    /// Empty text removes all children. Returns `false` for text/document
    /// nodes or unknown ids. Oversized text is truncated to the configured
    /// per-node byte limit.
    pub fn set_text_content(&mut self, id: NodeId, text: &str) -> bool {
        if !self.contains(id) {
            return false;
        }
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

    /// Tries to append a text node with `text` to `parent`.
    ///
    /// Returns `None` when the node cap is reached.
    pub fn try_append_text(&mut self, parent: NodeId, text: &str) -> Option<NodeId> {
        let node = self.try_create_text(text)?;
        self.append_child(parent, node);
        Some(node)
    }

    /// Appends a text node with `text` to `parent`, truncating oversized text.
    ///
    /// See [`Document::create_element`] for the behaviour at the node cap.
    pub fn append_text(&mut self, parent: NodeId, text: &str) -> NodeId {
        self.try_append_text(parent, text)
            .unwrap_or_else(|| self.root())
    }

    /// Mutates the text data of a text node.
    ///
    /// Returns `false` (leaving the node unchanged) for non-text nodes, unknown
    /// ids, or when `data` exceeds the configured per-node byte limit.
    pub fn set_text_data(&mut self, id: NodeId, data: &str) -> bool {
        if !self.contains(id) || data.len() > self.limits().max_text_bytes {
            return false;
        }
        match &mut self.node_mut(id).data {
            NodeData::Text(t) => {
                t.data = data.to_string();
                true
            }
            _ => false,
        }
    }

    /// Appends text to an existing text node.
    ///
    /// Appends only as many bytes as fit under the configured limit; returns
    /// `false` when nothing could be appended or the node is not a text node.
    pub fn push_text(&mut self, id: NodeId, extra: &str) -> bool {
        if !self.contains(id) {
            return false;
        }
        let Some(existing_len) = self.node(id).text().map(str::len) else {
            return false;
        };
        let room = self.limits().max_text_bytes.saturating_sub(existing_len);
        if room == 0 {
            return false;
        }
        let addition = clamp_str(extra, room);
        if addition.is_empty() {
            return false;
        }
        match &mut self.node_mut(id).data {
            NodeData::Text(t) => {
                t.data.push_str(addition);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

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

    #[test]
    fn text_cap_truncates_creation_and_rejects_oversized_updates() {
        let mut doc = Document::with_limits(Limits {
            max_text_bytes: 5,
            ..Limits::unlimited()
        });
        let text = doc.create_text("abcdefgh");
        assert_eq!(doc.node(text).text(), Some("abcde"));

        // A multi-byte boundary is respected (5 bytes -> "aé" is 3 bytes;
        // adding "€" would make 6, so it is left out).
        let unicode = doc.create_text("aé€");
        assert_eq!(doc.node(unicode).text(), Some("aé"));

        assert!(doc.set_text_data(text, "12345"));
        assert_eq!(doc.node(text).text(), Some("12345"));
        assert!(!doc.set_text_data(text, "123456"));
        assert_eq!(doc.node(text).text(), Some("12345"));
        assert!(!doc.set_text_data(NodeId(usize::MAX), "x"));
    }

    #[test]
    fn push_text_stops_at_the_cap() {
        let mut doc = Document::with_limits(Limits {
            max_text_bytes: 5,
            ..Limits::unlimited()
        });
        let text = doc.create_text("abc");
        assert!(doc.push_text(text, "de"));
        assert_eq!(doc.node(text).text(), Some("abcde"));
        assert!(!doc.push_text(text, "f"));
        assert_eq!(doc.node(text).text(), Some("abcde"));

        // Partial multi-byte characters are never appended.
        let text2 = doc.create_text("abc");
        assert!(doc.push_text(text2, "é!")); // "é" is 2 bytes -> exactly 5.
        assert_eq!(doc.node(text2).text(), Some("abcé"));
        assert!(!doc.push_text(text2, "!"));
    }
}
