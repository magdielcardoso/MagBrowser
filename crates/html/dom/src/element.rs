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

    /// Sets an attribute on an element node. Returns `false` for non-elements.
    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) -> bool {
        let Some(element) = self.node_mut(id).element_mut() else {
            return false;
        };
        element.attributes.set(name, value);
        true
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
