//! Tree traversal iterators.

use crate::document::Document;
use crate::node::NodeId;
use crate::shadow::push_shadow_roots;

impl Document {
    /// Parent of a node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    /// Children of a node.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).children
    }

    /// Number of children.
    pub fn child_count(&self, id: NodeId) -> usize {
        self.node(id).children.len()
    }

    /// First child, if any.
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).children.first().copied()
    }

    /// Last child, if any.
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).children.last().copied()
    }

    /// Next sibling, if any.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.node(id).parent?;
        let siblings = &self.node(parent).children;
        let index = siblings.iter().position(|s| *s == id)?;
        siblings.get(index + 1).copied()
    }

    /// Previous sibling, if any.
    pub fn previous_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.node(id).parent?;
        let siblings = &self.node(parent).children;
        let index = siblings.iter().position(|s| *s == id)?;
        index.checked_sub(1).and_then(|i| siblings.get(i).copied())
    }

    /// Iterates from `id` up to the document root (excluding `id` itself).
    pub fn ancestors(&self, id: NodeId) -> Ancestors<'_> {
        Ancestors {
            doc: self,
            current: self.node(id).parent,
        }
    }

    /// Pre-order traversal of `id`'s subtree, excluding `id`.
    ///
    /// Shadow trees attached to any node in the subtree are included, so
    /// style and layout see shadow content through the normal traversal path.
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        let stack: Vec<NodeId> = self.node(id).children.iter().rev().copied().collect();
        Descendants { doc: self, stack }
    }

    /// Element children of `id`.
    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.node(id)
            .children
            .iter()
            .copied()
            .filter(|c| self.node(*c).is_element())
            .collect()
    }

    /// Every element in document order (pre-order), including shadow trees.
    pub fn elements_in_order(&self) -> Vec<NodeId> {
        self.descendants(self.root())
            .filter(|id| self.node(*id).is_element())
            .collect()
    }

    /// The nearest ancestor element of `id`.
    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        self.ancestors(id).find(|a| self.node(*a).is_element())
    }

    /// The previous element sibling.
    pub fn previous_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut current = self.previous_sibling(id);
        while let Some(node) = current {
            if self.node(node).is_element() {
                return Some(node);
            }
            current = self.previous_sibling(node);
        }
        None
    }

    /// The next element sibling.
    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut current = self.next_sibling(id);
        while let Some(node) = current {
            if self.node(node).is_element() {
                return Some(node);
            }
            current = self.next_sibling(node);
        }
        None
    }

    /// Deep-clones a subtree, returning the new root (which is detached).
    pub fn clone_subtree(&mut self, id: NodeId) -> NodeId {
        let data = self.node(id).data.clone();
        let copy = self.alloc_clone(data);
        let children = self.node(id).children.clone();
        for child in children {
            let child_copy = self.clone_subtree(child);
            self.append_child(copy, child_copy);
        }
        copy
    }

    fn alloc_clone(&mut self, data: crate::node::NodeData) -> NodeId {
        // Reuse the public constructors so attribute maps stay consistent.
        match data {
            crate::node::NodeData::Element(e) => {
                let new = self.create_element_ns(&e.name, e.namespace);
                for (k, v) in e.attributes.iter() {
                    self.set_attr(new, k, v);
                }
                new
            }
            crate::node::NodeData::Text(t) => self.create_text(&t.data),
            crate::node::NodeData::Comment(c) => self.create_comment(&c),
            crate::node::NodeData::Doctype { name, .. } => self.create_doctype(&name),
            crate::node::NodeData::Document => self.root(),
        }
    }
}

/// Iterator over the ancestors of a node.
pub struct Ancestors<'a> {
    doc: &'a Document,
    current: Option<NodeId>,
}

impl Iterator for Ancestors<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.current?;
        // A shadow root arena node has no parent of its own; logically its
        // parent is the host element, so inherited styles and :host-context()
        // continue into the light tree.
        self.current = self
            .doc
            .shadow_root_host(id)
            .or_else(|| self.doc.node(id).parent);
        Some(id)
    }
}

/// Pre-order iterator over a subtree.
pub struct Descendants<'a> {
    doc: &'a Document,
    stack: Vec<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.stack.pop()?;
        // Light children are pushed last so they are visited before the
        // shadow tree; this keeps ordinary document order unchanged when no
        // shadow roots are present.
        push_shadow_roots(self.doc, id, &mut self.stack);
        let children = &self.doc.node(id).children;
        for child in children.iter().rev() {
            self.stack.push(*child);
        }
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_order_and_siblings() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let a = doc.create_element("a");
        let b = doc.create_element("b");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, a);
        doc.append_child(body, b);

        assert_eq!(
            doc.descendants(doc.root()).collect::<Vec<_>>(),
            vec![html, body, a, b]
        );
        assert_eq!(
            doc.ancestors(b).collect::<Vec<_>>(),
            vec![body, html, doc.root()]
        );
        assert_eq!(doc.next_sibling(a), Some(b));
        assert_eq!(doc.previous_sibling(b), Some(a));
        assert_eq!(doc.first_child(body), Some(a));
        assert_eq!(doc.last_child(body), Some(b));
        assert_eq!(doc.elements_in_order(), vec![html, body, a, b]);
        assert_eq!(doc.parent_element(a), Some(body));
    }

    #[test]
    fn traversal_includes_shadow_content_and_hosts() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let host = doc.create_element("x-card");
        doc.append_child(doc.root(), html);
        doc.append_child(html, host);

        let light = doc.create_element("span");
        doc.append_child(host, light);
        let root = doc
            .attach_shadow(host, crate::ShadowRootMode::Open)
            .unwrap();
        let shadow_child = doc.create_element("p");
        let shadow_text = doc.create_text("inside");
        doc.append_child(root, shadow_child);
        doc.append_child(shadow_child, shadow_text);

        // Descendants of the document include both light and shadow content.
        assert_eq!(
            doc.descendants(doc.root()).collect::<Vec<_>>(),
            vec![html, host, light, shadow_child, shadow_text]
                .into_iter()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            doc.elements_in_order(),
            vec![html, host, light, shadow_child]
        );

        // Ancestors of shadow content continue through the host into the light tree.
        assert_eq!(
            doc.ancestors(shadow_text).collect::<Vec<_>>(),
            vec![shadow_child, root, host, html, doc.root()]
        );
    }

    #[test]
    fn clone_subtree_is_deep_and_detached() {
        let mut doc = Document::new();
        let div = doc.create_element("div");
        doc.set_attr(div, "id", "x");
        let p = doc.create_element("p");
        doc.append_text(p, "text");
        doc.append_child(div, p);

        let copy = doc.clone_subtree(div);
        assert_ne!(copy, div);
        assert_eq!(doc.parent(copy), None);
        assert_eq!(doc.text_content(copy), "text");
        assert_eq!(doc.attr(copy, "id"), Some("x"));
    }
}
