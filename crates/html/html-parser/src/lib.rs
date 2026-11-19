//! HTML tokenizer and tree builder producing a DOM.
//!
//! The crate exposes a small, dependency-free HTML parser built from two
//! layers:
//!
//! * [`Tokenizer`] turns source text into [`Token`]s using the WHATWG tokenizer
//!   states (tags, attributes, comments, doctypes, RCDATA/rawtext and script
//!   data).
//! * [`TreeBuilder`] turns tokens into a [`Document`](browser_dom::Document)
//!   using the WHATWG insertion modes.
//!
//! The simple entry points are [`parse_document`] and [`parse_fragment`].
//!
//! ```
//! use browser_html_parser::parse_document;
//! use browser_dom::serialize_children;
//!
//! let doc = parse_document("<p>Hello <b>world</b></p>");
//! let (_, body) = doc.head_and_body();
//! assert_eq!(
//!     serialize_children(&doc, body.unwrap()),
//!     "<p>Hello <b>world</b></p>"
//! );
//! ```

#![forbid(unsafe_code)]

pub mod entities;
pub mod insertion_modes;
pub mod tokenizer;
pub mod tokens;
pub mod tree_builder;

pub use tokenizer::Tokenizer;
pub use tokens::Token;
pub use tree_builder::TreeBuilder;

use browser_dom::{Document, Namespace, NodeData, NodeId};

/// Default cap on the number of input bytes a single parse will consume.
///
/// Inputs larger than this are truncated at a UTF-8 boundary and reported as a
/// recoverable error. This keeps hostile pages from exhausting memory or CPU.
pub const DEFAULT_MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Default cap on the number of DOM nodes a single parse may create.
///
/// Once the cap is reached the remaining tokens are ignored, leaving a
/// well-formed (truncated) document.
pub const DEFAULT_MAX_NODES: usize = 200_000;

/// Default cap on the depth of the open-element stack (and therefore of the
/// constructed tree).
pub const DEFAULT_MAX_DEPTH: usize = 256;

/// Options controlling HTML parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    /// Whether scripting is enabled. When true, `noscript` content is treated
    /// as raw text, as browsers do with scripting turned on.
    pub scripting: bool,
    /// Maximum number of bytes of source text that will be processed. Longer
    /// input is truncated at a character boundary and reported as an error.
    pub max_input_bytes: usize,
    /// Maximum number of DOM nodes the tree builder may create. Extra content
    /// is discarded once this many nodes exist.
    pub max_nodes: usize,
    /// Maximum depth of the open-element stack. Deeper elements are still
    /// created but are not pushed, which keeps parsing of adversarial input
    /// from growing the stack (and the tree) without bound.
    pub max_depth: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            scripting: true,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_nodes: DEFAULT_MAX_NODES,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// A diagnostic produced while parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Human-readable description of the recovery that took place.
    pub message: String,
    /// Character offset in the input when known; `0` for errors detected by
    /// the tree builder when no position was supplied.
    pub position: usize,
}

/// The result of parsing a document.
#[derive(Debug)]
pub struct ParseResult {
    /// The constructed DOM.
    pub document: Document,
    /// Recoverable errors encountered during parsing.
    pub errors: Vec<ParseError>,
}

/// Parses a complete HTML document with the default options.
///
/// This function never panics; malformed input is recovered from and reported
/// in [`ParseResult::errors`] when [`parse_document_with`] is used.
#[must_use]
pub fn parse_document(input: &str) -> Document {
    parse_document_with(input, &ParseOptions::default()).document
}

/// Parses a complete HTML document with explicit options.
#[must_use]
pub fn parse_document_with(input: &str, options: &ParseOptions) -> ParseResult {
    let mut tokenizer = Tokenizer::with_max_input_bytes(input, options.max_input_bytes)
        .with_scripting(options.scripting);
    let mut builder = TreeBuilder::new(*options);
    loop {
        let token = tokenizer.next_token();
        let eof = matches!(token, Token::Eof);
        builder.set_position(tokenizer.position());
        builder.feed(token);
        if eof || builder.is_saturated() {
            break;
        }
    }
    let mut result = builder.finish();
    if tokenizer.input_truncated() {
        result.errors.insert(
            0,
            ParseError {
                message: format!(
                    "input exceeded {} bytes and was truncated",
                    options.max_input_bytes
                ),
                position: options.max_input_bytes,
            },
        );
    }
    result
}

/// Parses `input` as the contents of `context` and imports the resulting nodes
/// into `document`.
///
/// The returned nodes are created inside `document` but are detached; the
/// caller decides where to append them. This is the operation behind
/// `innerHTML` setters and `insertAdjacentHTML`.
#[must_use]
pub fn parse_fragment(document: &mut Document, context: NodeId, input: &str) -> Vec<NodeId> {
    let (context_name, context_namespace) = document
        .node(context)
        .element()
        .map(|element| (element.name.clone(), element.namespace))
        .unwrap_or_else(|| ("body".to_string(), Namespace::Html));
    let context_attributes: Vec<(String, String)> = document
        .attributes(context)
        .map(|attributes| {
            attributes
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Parse into a scratch document so the caller's tree is never partially
    // modified. The scratch document mirrors the minimal ancestor chain.
    let mut scratch = Document::new();
    let html = scratch.create_element("html");
    scratch.append_child(scratch.root(), html);
    let head = scratch.create_element("head");
    scratch.append_child(html, head);
    let body = scratch.create_element("body");
    scratch.append_child(html, body);
    let scratch_context = scratch.create_element_ns(&context_name, context_namespace);
    for (key, value) in &context_attributes {
        scratch.set_attr(scratch_context, key, value);
    }
    scratch.append_child(body, scratch_context);

    let options = ParseOptions::default();
    let mut builder = TreeBuilder::for_fragment(scratch, scratch_context, options);
    let mut tokenizer = Tokenizer::with_max_input_bytes(input, options.max_input_bytes);
    loop {
        let token = tokenizer.next_token();
        let eof = matches!(token, Token::Eof);
        builder.set_position(tokenizer.position());
        builder.feed(token);
        if eof || builder.is_saturated() {
            break;
        }
    }
    let result = builder.finish();
    let scratch = result.document;
    let children: Vec<NodeId> = scratch.children(scratch_context).to_vec();
    let mut imported = Vec::with_capacity(children.len());
    for child in children {
        imported.push(import_node(document, &scratch, child, 0));
    }
    imported
}

/// Upper bound for the recursive subtree import. The tree builder already
/// caps depth (see [`ParseOptions::max_depth`]); this is a final guard so a
/// malformed or hand-built context can never trigger a stack overflow here.
const MAX_IMPORT_DEPTH: usize = DEFAULT_MAX_DEPTH * 2 + 16;

/// Deep-copies a node and its subtree from one document into another.
fn import_node(destination: &mut Document, source: &Document, id: NodeId, depth: usize) -> NodeId {
    let data = source.node(id).data.clone();
    let new_id = match data {
        NodeData::Element(element) => {
            let new_id = destination.create_element_ns(&element.name, element.namespace);
            for (key, value) in element.attributes.iter() {
                destination.set_attr(new_id, key, value);
            }
            new_id
        }
        NodeData::Text(text) => destination.create_text(&text.data),
        NodeData::Comment(text) => destination.create_comment(&text),
        NodeData::Doctype { name, .. } => destination.create_doctype(&name),
        NodeData::Document => destination.root(),
    };
    if !matches!(source.node(id).data, NodeData::Document) && depth < MAX_IMPORT_DEPTH {
        for child in source.children(id).to_vec() {
            let child_id = import_node(destination, source, child, depth + 1);
            destination.append_child(new_id, child_id);
        }
    }
    new_id
}

#[cfg(test)]
mod tests;
