//! Document object model for the Mag browser engine.
//!
//! The DOM is a flat arena of [`Node`]s referenced by [`NodeId`]. It has no
//! dependency on CSS, layout or rendering; those crates depend on the DOM, never
//! the other way round.
//!
//! ```
//! use browser_dom::{Document, serialize_children};
//! let mut doc = Document::new();
//! let html = doc.create_element("html");
//! let body = doc.create_element("body");
//! doc.append_child(doc.root(), html);
//! doc.append_child(html, body);
//! doc.append_text(body, "Hello");
//! assert_eq!(doc.text_content(body), "Hello");
//! assert_eq!(serialize_children(&doc, body), "Hello");
//! ```

#![forbid(unsafe_code)]

pub mod attributes;
pub mod custom_elements;
pub mod document;
pub mod element;
pub mod limits;
pub mod mutation;
pub mod node;
pub mod serialize;
pub mod shadow;
pub mod text;
pub mod traversal;

pub use attributes::Attributes;
pub use custom_elements::{
    CallbackHandle, CustomElementDefinition, CustomElementError, CustomElementRegistry,
    LifecycleCallbacks, RESERVED_CUSTOM_ELEMENT_NAMES, WhenDefinedCallback,
};
pub use document::Document;
pub use limits::Limits;
pub use node::{ElementData, Namespace, Node, NodeData, NodeId, TextData};
pub use serialize::{escape_attr, escape_text, serialize, serialize_children, serialize_outer};
pub use shadow::{ShadowRoot, ShadowRootMode};
