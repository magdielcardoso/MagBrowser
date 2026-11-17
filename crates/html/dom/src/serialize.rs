//! HTML serialisation, including `innerHTML`/`textContent` support.

use crate::document::Document;
use crate::node::{Namespace, NodeData, NodeId};

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Serialises a node (element, text or comment) including its subtree.
pub fn serialize(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    write_node(doc, id, &mut out);
    out
}

/// Serialises the children of `id` (this is what `innerHTML` returns).
pub fn serialize_children(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    for child in doc.children(id) {
        write_node(doc, *child, &mut out);
    }
    out
}

/// Serialises the `outerHTML` of a node.
pub fn serialize_outer(doc: &Document, id: NodeId) -> String {
    serialize(doc, id)
}

fn write_node(doc: &Document, id: NodeId, out: &mut String) {
    match &doc.node(id).data {
        NodeData::Document => {
            for child in doc.children(id) {
                write_node(doc, *child, out);
            }
        }
        NodeData::Doctype { name, .. } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
        NodeData::Comment(data) => {
            out.push_str("<!--");
            out.push_str(data);
            out.push_str("-->");
        }
        NodeData::Text(text) => escape_text_into(&text.data, out),
        NodeData::Element(element) => {
            let name = &element.name;
            out.push('<');
            out.push_str(name);
            for (key, value) in element.attributes.iter() {
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                escape_attr_into(value, out);
                out.push('"');
            }
            out.push('>');
            if element.namespace == Namespace::Html && VOID_ELEMENTS.contains(&name.as_str()) {
                return;
            }
            for child in doc.children(id) {
                write_node(doc, *child, out);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
    }
}

/// Escapes text for HTML content.
pub fn escape_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    escape_text_into(input, &mut out);
    out
}

/// Escapes a value for a double-quoted HTML attribute.
pub fn escape_attr(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    escape_attr_into(input, &mut out);
    out
}

fn escape_text_into(input: &str, out: &mut String) {
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}

fn escape_attr_into(input: &str, out: &mut String) {
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with_body() -> (Document, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        (doc, body)
    }

    #[test]
    fn serialises_elements_and_text() {
        let (mut doc, body) = doc_with_body();
        let p = doc.create_element("p");
        doc.set_attr(p, "class", "a");
        doc.append_child(body, p);
        doc.append_text(p, "a < b & c");
        assert_eq!(
            serialize_children(&doc, body),
            "<p class=\"a\">a &lt; b &amp; c</p>"
        );
    }

    #[test]
    fn void_elements_have_no_end_tag() {
        let (mut doc, body) = doc_with_body();
        let br = doc.create_element("br");
        doc.append_child(body, br);
        let img = doc.create_element("img");
        doc.set_attr(img, "src", "x.png");
        doc.append_child(body, img);
        assert_eq!(serialize_children(&doc, body), "<br><img src=\"x.png\">");
    }

    #[test]
    fn escapes_attribute_quotes() {
        let (mut doc, body) = doc_with_body();
        let a = doc.create_element("a");
        doc.set_attr(a, "title", "he said \"hi\"");
        doc.append_child(body, a);
        assert_eq!(
            serialize_children(&doc, body),
            "<a title=\"he said &quot;hi&quot;\"></a>"
        );
    }

    #[test]
    fn serialises_comments_and_doctypes() {
        let mut doc = Document::new();
        let dt = doc.create_doctype("html");
        doc.append_child(doc.root(), dt);
        let c = doc.create_comment(" hi ");
        doc.append_child(doc.root(), c);
        assert_eq!(serialize(&doc, doc.root()), "<!DOCTYPE html><!-- hi -->");
    }
}
