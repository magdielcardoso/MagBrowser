//! Shadow DOM basics: shadow roots, host pointers and `<slot>` assignment.
//!
//! A shadow root is represented by a dedicated, detached node in the document
//! arena whose payload is [`NodeData::Document`]. This keeps the existing
//! tree representation and every downstream `NodeData` match intact while
//! giving shadow children ordinary `parent` / `children` links. The
//! [`ShadowRoot`] struct in the side table ties that arena node back to its
//! host element and records whether the root is open or closed.
//!
//! Traversal helpers ([`Document::descendants`], [`Document::ancestors`],
//! [`Document::elements_in_order`]) follow shadow roots, so style and layout
//! can see shadow content without a separate walk.

use crate::document::Document;
use crate::node::{NodeData, NodeId};
use crate::traversal::Descendants;

/// Whether a shadow root is open or closed to outside script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ShadowRootMode {
    /// The shadow root is reachable through `element.shadowRoot`.
    #[default]
    Open,
    /// The shadow root is hidden from outside script.
    Closed,
}

impl ShadowRootMode {
    /// The DOM API keyword for this mode (`"open"` or `"closed"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// A shadow root attached to a host element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowRoot {
    /// The element this shadow root is attached to.
    pub host: NodeId,
    /// The arena node that owns the shadow tree's child list.
    pub root: NodeId,
    /// Whether the root is open or closed.
    pub mode: ShadowRootMode,
}

impl ShadowRoot {
    /// Child nodes of the shadow tree, in tree order.
    ///
    /// This is a child list of its own, distinct from the host's light-DOM
    /// children.
    #[must_use]
    pub fn children<'a>(&self, doc: &'a Document) -> &'a [NodeId] {
        doc.children(self.root)
    }

    /// Element children of the shadow tree, in tree order.
    #[must_use]
    pub fn element_children(&self, doc: &Document) -> Vec<NodeId> {
        doc.element_children(self.root)
    }

    /// Pre-order traversal of the whole shadow tree, excluding the root.
    #[must_use]
    pub fn descendants<'a>(&self, doc: &'a Document) -> Descendants<'a> {
        doc.descendants(self.root)
    }
}

impl Document {
    /// The shadow root attached to `host`, if any.
    #[must_use]
    pub fn shadow_root(&self, host: NodeId) -> Option<&ShadowRoot> {
        self.shadow_roots.get(&host)
    }

    /// The arena node id of the shadow root attached to `host`.
    #[must_use]
    pub fn shadow_root_id(&self, host: NodeId) -> Option<NodeId> {
        self.shadow_roots.get(&host).map(|root| root.root)
    }

    /// True when `id` is the arena node representing a shadow root.
    #[must_use]
    pub fn is_shadow_root(&self, id: NodeId) -> bool {
        self.shadow_root_nodes.contains_key(&id)
    }

    /// The host element of a shadow-root arena node.
    #[must_use]
    pub fn shadow_root_host(&self, id: NodeId) -> Option<NodeId> {
        self.shadow_root_nodes.get(&id).copied()
    }

    /// Child list of `host`'s shadow tree, or an empty slice when it has none.
    #[must_use]
    pub fn shadow_children(&self, host: NodeId) -> &[NodeId] {
        self.shadow_root(host)
            .map_or(&[], |root| self.children(root.root))
    }

    /// Attaches an empty shadow root to an element.
    ///
    /// Returns the arena id of the new shadow root, which can be used with
    /// [`Document::append_child`] to populate the shadow tree. Returns `None`
    /// when `host` is not an element, already has a shadow root, or the node
    /// cap is reached.
    pub fn attach_shadow(&mut self, host: NodeId, mode: ShadowRootMode) -> Option<NodeId> {
        if !self.contains(host) || !self.node(host).is_element() {
            return None;
        }
        if self.shadow_roots.contains_key(&host) {
            return None;
        }
        let root = self.try_alloc(NodeData::Document)?;
        self.shadow_roots
            .insert(host, ShadowRoot { host, root, mode });
        self.shadow_root_nodes.insert(root, host);
        Some(root)
    }

    /// Detaches the shadow root from `host`, leaving its nodes detached.
    ///
    /// Returns `false` when `host` had no shadow root.
    pub fn detach_shadow(&mut self, host: NodeId) -> bool {
        let Some(shadow) = self.shadow_roots.remove(&host) else {
            return false;
        };
        self.shadow_root_nodes.remove(&shadow.root);
        true
    }

    /// The nearest shadow host enclosing `node`, walking through shadow roots.
    ///
    /// For a node inside a shadow tree this is the element that owns the tree;
    /// for a host element itself it is `None` (use [`Document::shadow_root`]).
    #[must_use]
    pub fn enclosing_host(&self, node: NodeId) -> Option<NodeId> {
        self.ancestors(node)
            .find(|id| self.is_shadow_root(*id))
            .and_then(|id| self.shadow_root_host(id))
    }

    /// Every `<slot>` element in `host`'s shadow tree, in tree order.
    #[must_use]
    pub fn slots(&self, host: NodeId) -> Vec<NodeId> {
        let Some(shadow) = self.shadow_root(host) else {
            return Vec::new();
        };
        self.descendants(shadow.root)
            .filter(|id| self.is_tag(*id, "slot"))
            .collect()
    }

    /// Light-DOM children assigned to `slot` (which must belong to `host`'s
    /// shadow tree).
    ///
    /// A named slot takes children with a matching `slot` attribute; the
    /// default slot takes children with no (or an empty) `slot` attribute.
    #[must_use]
    pub fn assigned_nodes(&self, host: NodeId, slot: NodeId) -> Vec<NodeId> {
        let Some(shadow) = self.shadow_root(host) else {
            return Vec::new();
        };
        if !self.is_tag(slot, "slot") || !self.descendants(shadow.root).any(|id| id == slot) {
            return Vec::new();
        }
        let name = self.attr(slot, "name");
        self.children(host)
            .iter()
            .copied()
            .filter(|child| match name {
                Some(name) => self.attr(*child, "slot") == Some(name),
                None => self
                    .attr(*child, "slot")
                    .is_none_or(|assigned| assigned.is_empty()),
            })
            .collect()
    }

    /// The slot in a shadow tree that `node` is assigned to, if any.
    #[must_use]
    pub fn assigned_slot(&self, node: NodeId) -> Option<NodeId> {
        // Only direct children of a shadow host participate in slotting.
        let host = self.parent(node)?;
        self.shadow_root(host)?;
        self.slots(host)
            .into_iter()
            .find(|slot| self.assigned_nodes(host, *slot).contains(&node))
    }

    /// True when `node` is assigned to a `<slot>` in a shadow tree.
    #[must_use]
    pub fn is_slotted(&self, node: NodeId) -> bool {
        self.assigned_slot(node).is_some()
    }
}

/// Adds the shadow trees on every node in `children` to a traversal stack.
/// Kept here so both [`Descendants`] and other helpers can share the rule.
pub(crate) fn push_shadow_roots(doc: &Document, id: NodeId, stack: &mut Vec<NodeId>) {
    if let Some(shadow) = doc.shadow_root(id) {
        for child in doc.children(shadow.root).iter().rev() {
            stack.push(*child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_with_shadow() -> (Document, NodeId, NodeId) {
        let mut doc = Document::new();
        let host = doc.create_element("x-card");
        doc.append_child(doc.root(), host);
        let root = doc.attach_shadow(host, ShadowRootMode::Open).unwrap();
        (doc, host, root)
    }

    #[test]
    fn attach_shadow_exposes_a_separate_child_list() {
        let mut doc = Document::new();
        let host = doc.create_element("x-card");
        let light = doc.create_element("span");
        doc.append_child(host, light);
        doc.append_child(doc.root(), host);

        let root = doc.attach_shadow(host, ShadowRootMode::Open).unwrap();
        let shadow_child = doc.create_element("p");
        doc.append_child(root, shadow_child);

        assert_eq!(doc.shadow_root(host).unwrap().host, host);
        assert_eq!(doc.shadow_root(host).unwrap().root, root);
        assert_eq!(doc.shadow_root(host).unwrap().mode, ShadowRootMode::Open);
        assert_eq!(doc.shadow_children(host), &[shadow_child]);
        assert_eq!(doc.children(host), &[light]);
        assert_eq!(doc.parent(shadow_child), Some(root));
        assert_eq!(doc.shadow_root_host(root), Some(host));
        assert!(doc.is_shadow_root(root));
        assert!(!doc.is_shadow_root(host));
    }

    #[test]
    fn attach_shadow_rejects_invalid_hosts_and_duplicates() {
        let mut doc = Document::new();
        let text = doc.create_text("hello");
        assert_eq!(doc.attach_shadow(text, ShadowRootMode::Open), None);
        assert_eq!(
            doc.attach_shadow(NodeId(usize::MAX), ShadowRootMode::Open),
            None
        );

        let host = doc.create_element("div");
        assert!(doc.attach_shadow(host, ShadowRootMode::Closed).is_some());
        assert_eq!(doc.attach_shadow(host, ShadowRootMode::Open), None);
        assert_eq!(doc.shadow_root(host).unwrap().mode, ShadowRootMode::Closed);
        assert!(doc.detach_shadow(host));
        assert!(doc.shadow_root(host).is_none());
        assert!(!doc.detach_shadow(host));
    }

    #[test]
    fn shadow_root_helpers_walk_the_tree() {
        let (mut doc, host, root) = host_with_shadow();
        let child = doc.create_element("div");
        let grandchild = doc.create_element("span");
        doc.append_child(root, child);
        doc.append_child(child, grandchild);

        let shadow = doc.shadow_root(host).unwrap().clone();
        assert_eq!(shadow.children(&doc), &[child]);
        assert_eq!(shadow.element_children(&doc), vec![child]);
        assert_eq!(
            shadow.descendants(&doc).collect::<Vec<_>>(),
            vec![child, grandchild]
        );
        assert_eq!(doc.enclosing_host(grandchild), Some(host));
        assert_eq!(doc.enclosing_host(host), None);
    }

    #[test]
    fn slot_assignment_basics() {
        let mut doc = Document::new();
        let host = doc.create_element("x-card");
        doc.append_child(doc.root(), host);
        let root = doc.attach_shadow(host, ShadowRootMode::Open).unwrap();

        let default_slot = doc.create_element("slot");
        let named_slot = doc.create_element("slot");
        doc.set_attr(named_slot, "name", "title");
        doc.append_child(root, default_slot);
        doc.append_child(root, named_slot);

        let unassigned = doc.create_element("p");
        let titled = doc.create_element("h1");
        doc.set_attr(titled, "slot", "title");
        let other = doc.create_element("em");
        doc.set_attr(other, "slot", "other");
        for node in [unassigned, titled, other] {
            doc.append_child(host, node);
        }

        assert_eq!(doc.slots(host), vec![default_slot, named_slot]);
        assert_eq!(doc.assigned_nodes(host, default_slot), vec![unassigned]);
        assert_eq!(doc.assigned_nodes(host, named_slot), vec![titled]);
        assert_eq!(doc.assigned_slot(unassigned), Some(default_slot));
        assert_eq!(doc.assigned_slot(titled), Some(named_slot));
        assert!(doc.is_slotted(unassigned));
        assert!(!doc.is_slotted(other));
        assert_eq!(doc.assigned_slot(root), None);
    }
}
