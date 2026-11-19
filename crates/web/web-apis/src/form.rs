//! Form helpers shared by the DOM bindings and the embedder.
//!
//! These are pure DOM queries: they never perform navigation themselves. The
//! bindings use them to implement implicit submission (Enter in a single-line
//! text control), while `browser-core` calls [`form_for_input`] from its input
//! handlers and consumes the [`FormSubmission`] recorded in `HostState`.

use browser_dom::{Document, NodeId};

/// A form submission requested from a script or by implicit activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormSubmission {
    /// The form element being submitted.
    pub form: NodeId,
    /// The submit button that triggered it, when there was one.
    pub submitter: Option<NodeId>,
}

/// Returns the nearest ancestor `<form>` element of `node`, if any.
///
/// `node` itself is not considered, so passing a `<form>` returns the form that
/// contains it (usually `None`).
pub fn form_for_input(doc: &Document, node: NodeId) -> Option<NodeId> {
    if !doc.contains(node) {
        return None;
    }
    let mut current = doc.parent(node);
    while let Some(id) = current {
        if doc
            .element_name(id)
            .is_some_and(|name| name.eq_ignore_ascii_case("form"))
        {
            return Some(id);
        }
        current = doc.parent(id);
    }
    None
}

/// Nodes in document order below `root` (not including `root` itself).
pub fn descendants(doc: &Document, root: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = doc.children(root).iter().rev().copied().collect();
    while let Some(node) = stack.pop() {
        out.push(node);
        stack.extend(doc.children(node).iter().rev().copied());
    }
    out
}

/// True when `node` is a control whose value is submitted as text.
fn is_text_control(doc: &Document, node: NodeId) -> bool {
    let Some(name) = doc.element_name(node) else {
        return false;
    };
    let tag = name.to_ascii_lowercase();
    if tag == "textarea" {
        return true;
    }
    if tag != "input" {
        return false;
    }
    let kind = doc.attr(node, "type").unwrap_or("").to_ascii_lowercase();
    matches!(
        kind.as_str(),
        "" | "text"
            | "search"
            | "url"
            | "tel"
            | "email"
            | "password"
            | "number"
            | "date"
            | "month"
            | "week"
            | "time"
            | "datetime-local"
            | "color"
            | "range"
    )
}

/// True when a form control is disabled, honouring the `fieldset`/`legend` rule.
pub fn is_disabled(doc: &Document, node: NodeId, form: NodeId) -> bool {
    let mut current = node;
    // Set when we step out of the first <legend> of a disabled <fieldset>: that
    // fieldset's `disabled` attribute does not apply to controls in its legend.
    let mut from_first_legend = false;
    loop {
        if current == form {
            return false;
        }
        if !from_first_legend && doc.has_attr(current, "disabled") {
            return true;
        }
        let Some(parent) = doc.parent(current) else {
            return false;
        };
        from_first_legend = doc
            .element_name(parent)
            .is_some_and(|name| name.eq_ignore_ascii_case("fieldset"))
            && doc
                .element_name(current)
                .is_some_and(|name| name.eq_ignore_ascii_case("legend"))
            && doc.children(parent).iter().copied().find(|child| {
                doc.element_name(*child)
                    .is_some_and(|name| name.eq_ignore_ascii_case("legend"))
            }) == Some(current);
        current = parent;
    }
}

/// Returns the sole text control of `form` when it has exactly one enabled one.
pub fn single_text_control(doc: &Document, form: NodeId) -> Option<NodeId> {
    let mut found = None;
    for node in descendants(doc, form) {
        if !is_text_control(doc, node) || is_disabled(doc, node, form) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(node);
    }
    found
}

/// The first submit button of a form, used as the implicit submitter.
pub fn default_submit_button(doc: &Document, form: NodeId) -> Option<NodeId> {
    descendants(doc, form).into_iter().find(|node| {
        let Some(name) = doc.element_name(*node) else {
            return false;
        };
        let tag = name.to_ascii_lowercase();
        let kind = doc.attr(*node, "type").unwrap_or("").to_ascii_lowercase();
        if is_disabled(doc, *node, form) {
            return false;
        }
        match tag.as_str() {
            "button" => kind.is_empty() || kind == "submit",
            "input" => kind == "submit" || kind == "image",
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse_document;

    #[test]
    fn finds_the_nearest_ancestor_form() {
        let doc = parse_document(
            "<html><body><form id='outer'><div><input id='q'></div></form>\
             <input id='loose'></body></html>",
        );
        let outer = doc.get_element_by_id("outer").unwrap();
        let q = doc.get_element_by_id("q").unwrap();
        let loose = doc.get_element_by_id("loose").unwrap();
        assert_eq!(form_for_input(&doc, q), Some(outer));
        assert_eq!(form_for_input(&doc, outer), None);
        assert_eq!(form_for_input(&doc, loose), None);
    }

    #[test]
    fn detects_the_sole_text_control_and_default_button() {
        let doc = parse_document(
            "<html><body><form id='f'><input name='q'>\
             <input type='hidden' name='t' value='x'>\
             <input type='checkbox' name='c'>\
             <button type='submit' name='go' value='1'>Go</button></form></body></html>",
        );
        let form = doc.get_element_by_id("f").unwrap();
        let q = doc
            .get_elements_by_tag_name("input")
            .into_iter()
            .find(|node| doc.attr(*node, "name") == Some("q"))
            .unwrap();
        let go = doc
            .get_elements_by_tag_name("button")
            .first()
            .copied()
            .unwrap();
        assert_eq!(single_text_control(&doc, form), Some(q));
        assert_eq!(default_submit_button(&doc, form), Some(go));
    }

    #[test]
    fn two_text_controls_disable_implicit_submission() {
        let doc = parse_document(
            "<html><body><form id='f'><input name='a'><input name='b'></form></body></html>",
        );
        let form = doc.get_element_by_id("f").unwrap();
        assert_eq!(single_text_control(&doc, form), None);
    }
}
