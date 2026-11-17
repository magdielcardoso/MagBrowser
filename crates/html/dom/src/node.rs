//! Node identity, node data and the flat node arena.

use crate::attributes::Attributes;

/// Identifier of a node inside a [`Document`](crate::Document).
///
/// Ids are stable for the lifetime of the document and index into the arena;
/// a removed node's id is never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

/// HTML/SVG/MathML namespaces understood by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// The HTML namespace.
    Html,
    /// The SVG namespace.
    Svg,
    /// The MathML namespace.
    MathMl,
}

impl Namespace {
    /// Namespace URL as used by DOM APIs.
    pub fn url(self) -> &'static str {
        match self {
            Self::Html => "http://www.w3.org/1999/xhtml",
            Self::Svg => "http://www.w3.org/2000/svg",
            Self::MathMl => "http://www.w3.org/1998/Math/MathML",
        }
    }
}

/// Data carried by an element node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementData {
    /// Lowercased tag name for HTML elements.
    pub name: String,
    /// Element namespace.
    pub namespace: Namespace,
    /// Attributes in insertion order.
    pub attributes: Attributes,
}

/// Data carried by a text node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextData {
    /// Text contents exactly as parsed (entities already decoded).
    pub data: String,
}

/// The payload of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeData {
    /// The document root.
    Document,
    /// A document type declaration.
    Doctype {
        /// Doctype name, for example `html`.
        name: String,
        /// Legacy public identifier.
        public_id: String,
        /// Legacy system identifier.
        system_id: String,
    },
    /// An element.
    Element(ElementData),
    /// A text node.
    Text(TextData),
    /// A comment node.
    Comment(String),
}

/// A node in the document arena.
#[derive(Debug, Clone)]
pub struct Node {
    /// Stable node id.
    pub id: NodeId,
    /// Parent node, if any.
    pub parent: Option<NodeId>,
    /// Children in document order.
    pub children: Vec<NodeId>,
    /// Node payload.
    pub data: NodeData,
}

impl Node {
    /// True when the node is an element.
    pub fn is_element(&self) -> bool {
        matches!(self.data, NodeData::Element(_))
    }

    /// True when the node is a text node.
    pub fn is_text(&self) -> bool {
        matches!(self.data, NodeData::Text(_))
    }

    /// The element name for element nodes.
    pub fn element_name(&self) -> Option<&str> {
        match &self.data {
            NodeData::Element(e) => Some(&e.name),
            _ => None,
        }
    }

    /// The element data for element nodes.
    pub fn element(&self) -> Option<&ElementData> {
        match &self.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Mutable element data for element nodes.
    pub fn element_mut(&mut self) -> Option<&mut ElementData> {
        match &mut self.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Text contents for text nodes.
    pub fn text(&self) -> Option<&str> {
        match &self.data {
            NodeData::Text(t) => Some(&t.data),
            _ => None,
        }
    }
}
