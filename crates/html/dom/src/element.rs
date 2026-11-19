//! Element-focused accessors on [`Document`].

use crate::attributes::Attributes;
use crate::document::Document;
use crate::node::{Namespace, NodeId};

impl Document {
    /// The tag name of an element node.
    pub fn element_name(&self, id: NodeId) -> Option<&str> {
        self.node(id).element_name()
    }

    /// The namespace of an element node.
    pub fn namespace(&self, id: NodeId) -> Option<Namespace> {
        self.node(id).element().map(|e| e.namespace)
    }

    /// The attribute list of an element node.
    pub fn attributes(&self, id: NodeId) -> Option<&Attributes> {
        self.node(id).element().map(|e| &e.attributes)
    }

    /// Reads an attribute from an element node.
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.attributes(id)?.get(name)
    }

    /// True when the element has the attribute.
    pub fn has_attr(&self, id: NodeId, name: &str) -> bool {
        self.attr(id, name).is_some()
    }

    /// Sets an attribute on an element node.
    ///
    /// Returns `false` for non-elements, unknown nodes and when the element is
    /// already at its attribute cap (for a new attribute name). Updating an
    /// existing attribute always succeeds.
    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) -> bool {
        if !self.contains(id) {
            return false;
        }
        let Some(element) = self.node(id).element() else {
            return false;
        };
        let cap = self
            .limits()
            .max_attributes_per_element
            .min(element.attributes.limit());
        if !element.attributes.contains(name) && element.attributes.len() >= cap {
            return false;
        }
        self.node_mut(id)
            .element_mut()
            .is_some_and(|e| e.attributes.try_set(name, value))
    }

    /// Removes an attribute from an element node.
    pub fn remove_attr(&mut self, id: NodeId, name: &str) -> Option<String> {
        self.node_mut(id).element_mut()?.attributes.remove(name)
    }

    /// The `id` attribute value.
    pub fn id_attr(&self, id: NodeId) -> Option<&str> {
        self.attr(id, "id")
    }

    /// The `class` tokens of an element.
    pub fn class_list(&self, id: NodeId) -> Vec<&str> {
        self.attributes(id)
            .map(Attributes::class_list)
            .unwrap_or_default()
    }

    /// True when the element is in the HTML namespace.
    pub fn is_html_element(&self, id: NodeId) -> bool {
        self.namespace(id) == Some(Namespace::Html)
    }

    /// True when the element has the given tag name (case-insensitive).
    pub fn is_tag(&self, id: NodeId, name: &str) -> bool {
        self.element_name(id)
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    #[test]
    fn set_attr_respects_document_limit() {
        let mut doc = Document::with_limits(Limits {
            max_attributes_per_element: 2,
            ..Limits::unlimited()
        });
        let el = doc.create_element("div");
        assert_eq!(doc.attributes(el).unwrap().limit(), 2);

        assert!(doc.set_attr(el, "a", "1"));
        assert!(doc.set_attr(el, "b", "2"));
        assert!(!doc.set_attr(el, "c", "3"));
        assert_eq!(doc.attributes(el).unwrap().len(), 2);
        assert_eq!(doc.attr(el, "c"), None);

        // Lowering the cap at runtime is honoured for new attributes.
        doc.set_limits(Limits {
            max_attributes_per_element: 1,
            ..doc.limits()
        });
        assert!(!doc.set_attr(el, "d", "4"));
        // Existing attributes remain updatable.
        assert!(doc.set_attr(el, "A", "updated"));
        assert_eq!(doc.attr(el, "a"), Some("updated"));
    }
}
