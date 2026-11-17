//! Right-to-left selector matching against the DOM.

use browser_dom::{Document, NodeData, NodeId};

use crate::{
    AttrOp, Combinator, CompoundSelector, ElementState, PseudoClass, Selector, SelectorError,
    SimpleSelector, Specificity,
};

/// Returns the specificity recorded in a parsed selector.
#[must_use]
pub fn specificity(selector: &Selector) -> Specificity {
    selector.specificity
}

/// Matches a selector against a node with default (idle) element state.
#[must_use]
pub fn matches(doc: &Document, node: NodeId, selector: &Selector) -> bool {
    matches_with_state(doc, node, selector, &ElementState::default())
}

/// Matches a selector against a node with the given interactive state.
#[must_use]
pub fn matches_with_state(
    doc: &Document,
    node: NodeId,
    selector: &Selector,
    state: &ElementState,
) -> bool {
    if selector.parts.is_empty() || !doc.node(node).is_element() {
        return false;
    }
    match_parts(doc, node, &selector.parts, selector.parts.len() - 1, state)
}

/// Returns the first element in `root`'s subtree matching `text`.
///
/// # Errors
/// Returns [`SelectorError`] when `text` is not a valid selector list.
pub fn query_selector(
    doc: &Document,
    root: NodeId,
    text: &str,
) -> Result<Option<NodeId>, SelectorError> {
    Ok(query_selector_all(doc, root, text)?.into_iter().next())
}

/// Returns every element in `root`'s subtree matching `text`, in document
/// order and without duplicates. The root itself is not considered.
///
/// # Errors
/// Returns [`SelectorError`] when `text` is not a valid selector list.
pub fn query_selector_all(
    doc: &Document,
    root: NodeId,
    text: &str,
) -> Result<Vec<NodeId>, SelectorError> {
    let selectors = crate::parse_selectors(text)?;
    let mut result = Vec::new();
    for node in doc.descendants(root) {
        if !doc.node(node).is_element() {
            continue;
        }
        if selectors
            .iter()
            .any(|selector| matches(doc, node, selector))
        {
            result.push(node);
        }
    }
    Ok(result)
}

fn match_parts(
    doc: &Document,
    node: NodeId,
    parts: &[(Option<Combinator>, CompoundSelector)],
    index: usize,
    state: &ElementState,
) -> bool {
    if !matches_compound(doc, node, &parts[index].1, state) {
        return false;
    }
    if index == 0 {
        return true;
    }

    let combinator = parts[index].0.unwrap_or(Combinator::Descendant);
    match combinator {
        Combinator::Descendant => doc.ancestors(node).any(|ancestor| {
            doc.node(ancestor).is_element() && match_parts(doc, ancestor, parts, index - 1, state)
        }),
        Combinator::Child => doc
            .parent_element(node)
            .is_some_and(|parent| match_parts(doc, parent, parts, index - 1, state)),
        Combinator::NextSibling => doc
            .previous_element_sibling(node)
            .is_some_and(|sibling| match_parts(doc, sibling, parts, index - 1, state)),
        Combinator::SubsequentSibling => {
            let mut current = doc.previous_element_sibling(node);
            while let Some(sibling) = current {
                if match_parts(doc, sibling, parts, index - 1, state) {
                    return true;
                }
                current = doc.previous_element_sibling(sibling);
            }
            false
        }
    }
}

fn matches_compound(
    doc: &Document,
    node: NodeId,
    compound: &CompoundSelector,
    state: &ElementState,
) -> bool {
    compound
        .simples
        .iter()
        .all(|simple| matches_simple(doc, node, simple, state))
}

fn matches_simple(
    doc: &Document,
    node: NodeId,
    simple: &SimpleSelector,
    state: &ElementState,
) -> bool {
    match simple {
        SimpleSelector::Universal => true,
        SimpleSelector::Type(name) => doc.element_name(node).is_some_and(|actual| {
            if doc.is_html_element(node) {
                actual.eq_ignore_ascii_case(name)
            } else {
                actual == name
            }
        }),
        SimpleSelector::Id(id) => doc.attr(node, "id") == Some(id.as_str()),
        SimpleSelector::Class(class) => doc.class_list(node).contains(&class.as_str()),
        SimpleSelector::Attribute {
            name,
            op,
            value,
            case_insensitive,
        } => matches_attribute(doc, node, name, op, value.as_deref(), *case_insensitive),
        SimpleSelector::Pseudo(class) => matches_pseudo_class(doc, node, class, state),
        SimpleSelector::PseudoElement(_) => false,
    }
}

fn compare(actual: &str, expected: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        actual.eq_ignore_ascii_case(expected)
    } else {
        actual == expected
    }
}

fn matches_attribute(
    doc: &Document,
    node: NodeId,
    name: &str,
    op: &AttrOp,
    expected: Option<&str>,
    case_insensitive: bool,
) -> bool {
    let Some(actual) = doc.attr(node, name) else {
        return false;
    };
    if matches!(op, AttrOp::Exists) {
        return true;
    }
    let Some(expected) = expected else {
        return false;
    };

    match op {
        AttrOp::Exists => true,
        AttrOp::Equals => compare(actual, expected, case_insensitive),
        AttrOp::Includes => actual
            .split_ascii_whitespace()
            .any(|token| compare(token, expected, case_insensitive)),
        AttrOp::Prefix => starts_with(actual, expected, case_insensitive),
        AttrOp::Suffix => ends_with(actual, expected, case_insensitive),
        AttrOp::Substring => contains(actual, expected, case_insensitive),
        AttrOp::DashMatch => {
            compare(actual, expected, case_insensitive)
                || (actual.len() > expected.len()
                    && starts_with(actual, expected, case_insensitive)
                    && actual.as_bytes().get(expected.len()) == Some(&b'-'))
        }
    }
}

fn starts_with(actual: &str, expected: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        actual
            .to_ascii_lowercase()
            .starts_with(&expected.to_ascii_lowercase())
    } else {
        actual.starts_with(expected)
    }
}

fn ends_with(actual: &str, expected: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        actual
            .to_ascii_lowercase()
            .ends_with(&expected.to_ascii_lowercase())
    } else {
        actual.ends_with(expected)
    }
}

fn contains(actual: &str, expected: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        actual
            .to_ascii_lowercase()
            .contains(&expected.to_ascii_lowercase())
    } else {
        actual.contains(expected)
    }
}

fn matches_pseudo_class(
    doc: &Document,
    node: NodeId,
    class: &PseudoClass,
    state: &ElementState,
) -> bool {
    match class {
        PseudoClass::FirstChild => doc.previous_element_sibling(node).is_none(),
        PseudoClass::LastChild => doc.next_element_sibling(node).is_none(),
        PseudoClass::OnlyChild => {
            doc.previous_element_sibling(node).is_none() && doc.next_element_sibling(node).is_none()
        }
        PseudoClass::NthChild(a, b) => {
            matches_an_plus_b(child_index(doc, node) as i64, i64::from(*a), i64::from(*b))
        }
        PseudoClass::NthLastChild(a, b) => matches_an_plus_b(
            child_index_from_end(doc, node) as i64,
            i64::from(*a),
            i64::from(*b),
        ),
        PseudoClass::Root => doc.document_element() == Some(node),
        PseudoClass::Empty => is_empty_element(doc, node),
        PseudoClass::FirstOfType => type_position(doc, node) == Some((1, type_count(doc, node))),
        PseudoClass::LastOfType => {
            let count = type_count(doc, node);
            type_position(doc, node) == Some((count, count))
        }
        PseudoClass::OnlyOfType => type_count(doc, node) == 1,
        PseudoClass::Not(selectors) => !selectors
            .iter()
            .any(|selector| matches_with_state(doc, node, selector, state)),
        PseudoClass::Is(selectors) | PseudoClass::Where(selectors) => selectors
            .iter()
            .any(|selector| matches_with_state(doc, node, selector, state)),
        PseudoClass::Hover => state.hover,
        PseudoClass::Focus => state.focus,
        PseudoClass::Active => state.active,
        PseudoClass::Visited => state.visited,
        PseudoClass::Link | PseudoClass::AnyLink => is_link(doc, node),
        PseudoClass::Checked => checked(doc, node, state),
        PseudoClass::Disabled => supports_disabled(doc, node) && is_disabled(doc, node, state),
        PseudoClass::Enabled => supports_disabled(doc, node) && !is_disabled(doc, node, state),
        PseudoClass::Lang(language) => {
            element_language(doc, node).is_some_and(|actual| language_matches(&actual, language))
        }
    }
}

fn element_siblings(doc: &Document, node: NodeId) -> Vec<NodeId> {
    let Some(parent) = doc.parent(node) else {
        return Vec::new();
    };
    doc.children(parent)
        .iter()
        .copied()
        .filter(|child| doc.node(*child).is_element())
        .collect()
}

fn child_index(doc: &Document, node: NodeId) -> usize {
    element_siblings(doc, node)
        .iter()
        .position(|sibling| *sibling == node)
        .map_or(0, |index| index + 1)
}

fn child_index_from_end(doc: &Document, node: NodeId) -> usize {
    let siblings = element_siblings(doc, node);
    siblings
        .iter()
        .position(|sibling| *sibling == node)
        .map_or(0, |index| siblings.len() - index)
}

fn matches_an_plus_b(index: i64, a: i64, b: i64) -> bool {
    if index <= 0 {
        return false;
    }
    if a == 0 {
        return index == b;
    }
    let difference = index - b;
    difference % a == 0 && difference / a >= 0
}

fn same_type(doc: &Document, first: NodeId, second: NodeId) -> bool {
    match (doc.element_name(first), doc.element_name(second)) {
        (Some(left), Some(right)) => {
            if doc.is_html_element(first) && doc.is_html_element(second) {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        }
        _ => false,
    }
}

/// Returns the 1-based position of `node` among siblings of the same type,
/// and the total number of such siblings.
fn type_position(doc: &Document, node: NodeId) -> Option<(usize, usize)> {
    let siblings: Vec<NodeId> = element_siblings(doc, node)
        .into_iter()
        .filter(|sibling| same_type(doc, *sibling, node))
        .collect();
    let position = siblings.iter().position(|sibling| *sibling == node)? + 1;
    Some((position, siblings.len()))
}

fn type_count(doc: &Document, node: NodeId) -> usize {
    type_position(doc, node).map_or(0, |(_, count)| count)
}

fn is_empty_element(doc: &Document, node: NodeId) -> bool {
    doc.children(node)
        .iter()
        .all(|child| match &doc.node(*child).data {
            NodeData::Comment(_) => true,
            NodeData::Text(text) => text.data.is_empty(),
            _ => false,
        })
}

fn is_link(doc: &Document, node: NodeId) -> bool {
    matches!(doc.element_name(node), Some("a" | "area" | "link")) && doc.has_attr(node, "href")
}

fn checked(doc: &Document, node: NodeId, state: &ElementState) -> bool {
    if let Some(value) = state.checked {
        return value;
    }
    match doc.element_name(node) {
        Some("input") => doc.has_attr(node, "checked"),
        Some("option") => doc.has_attr(node, "selected"),
        _ => false,
    }
}

fn supports_disabled(doc: &Document, node: NodeId) -> bool {
    matches!(
        doc.element_name(node),
        Some("button" | "input" | "select" | "textarea" | "optgroup" | "option" | "fieldset")
    )
}

fn is_disabled(doc: &Document, node: NodeId, state: &ElementState) -> bool {
    state.disabled || doc.has_attr(node, "disabled")
}

fn element_language(doc: &Document, node: NodeId) -> Option<String> {
    let mut current = Some(node);
    while let Some(id) = current {
        if doc.node(id).is_element()
            && let Some(language) = doc.attr(id, "lang")
        {
            return Some(language.to_ascii_lowercase());
        }
        current = doc.parent(id);
    }
    None
}

fn language_matches(actual: &str, requested: &str) -> bool {
    let requested = requested.to_ascii_lowercase();
    actual == requested
        || (actual.len() > requested.len()
            && actual.starts_with(&requested)
            && actual.as_bytes().get(requested.len()) == Some(&b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_selector, parse_selectors};

    struct Fixture {
        doc: Document,
        html: NodeId,
        head: NodeId,
        body: NodeId,
        div: NodeId,
        p1: NodeId,
        p2: NodeId,
        span: NodeId,
        ul: NodeId,
        li1: NodeId,
        li2: NodeId,
        li3: NodeId,
        anchor: NodeId,
        checkbox: NodeId,
        text_input: NodeId,
        button: NodeId,
    }

    fn fixture() -> Fixture {
        let mut doc = Document::new();
        let root = doc.root();
        let html = doc.create_element("html");
        doc.set_attr(html, "lang", "en");
        let head = doc.create_element("head");
        let body = doc.create_element("body");
        doc.append_child(root, html);
        doc.append_child(html, head);
        doc.append_child(html, body);

        let div = doc.create_element("div");
        doc.set_attr(div, "id", "main");
        doc.set_attr(div, "class", "box wide");
        doc.set_attr(div, "data-role", "Main");
        doc.set_attr(div, "data-lang", "en-US");
        doc.append_child(body, div);

        let p1 = doc.create_element("p");
        doc.set_attr(p1, "class", "first a.b");
        doc.set_attr(p1, "title", "hello world");
        doc.append_text(p1, "Hello");
        doc.append_child(div, p1);

        let p2 = doc.create_element("p");
        doc.set_attr(p2, "class", "second");
        doc.set_attr(p2, "lang", "fr-CA");
        doc.append_child(div, p2);

        let span = doc.create_element("span");
        let comment = doc.create_comment("ignored");
        doc.append_child(span, comment);
        doc.append_child(div, span);

        let ul = doc.create_element("ul");
        doc.append_child(body, ul);
        let li1 = doc.create_element("li");
        doc.set_attr(li1, "id", "one");
        doc.append_text(li1, "a");
        doc.append_child(ul, li1);
        let li2 = doc.create_element("li");
        doc.set_attr(li2, "class", "two");
        doc.append_child(ul, li2);
        let li3 = doc.create_element("li");
        doc.append_child(ul, li3);

        let anchor = doc.create_element("a");
        doc.set_attr(anchor, "href", "/x");
        doc.append_text(anchor, "link");
        doc.append_child(body, anchor);

        let checkbox = doc.create_element("input");
        doc.set_attr(checkbox, "type", "checkbox");
        doc.set_attr(checkbox, "checked", "");
        doc.append_child(body, checkbox);

        let text_input = doc.create_element("input");
        doc.append_child(body, text_input);

        let button = doc.create_element("button");
        doc.set_attr(button, "disabled", "");
        doc.append_child(body, button);

        Fixture {
            doc,
            html,
            head,
            body,
            div,
            p1,
            p2,
            span,
            ul,
            li1,
            li2,
            li3,
            anchor,
            checkbox,
            text_input,
            button,
        }
    }

    fn is_match(doc: &Document, node: NodeId, text: &str) -> bool {
        let selector = parse_selector(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        matches(doc, node, &selector)
    }

    #[test]
    fn type_and_universal_selectors() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, "p"));
        assert!(is_match(&fixture.doc, fixture.p1, "P"));
        assert!(is_match(&fixture.doc, fixture.p1, "*"));
        assert!(!is_match(&fixture.doc, fixture.p1, "div"));
        assert!(!is_match(&fixture.doc, fixture.head, "body"));
    }

    #[test]
    fn id_and_class_selectors() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.div, "#main"));
        assert!(is_match(&fixture.doc, fixture.div, ".box"));
        assert!(is_match(&fixture.doc, fixture.div, ".box.wide"));
        assert!(!is_match(&fixture.doc, fixture.div, ".wide.box.extra"));
        assert!(!is_match(&fixture.doc, fixture.p1, ".box"));
        assert!(!is_match(&fixture.doc, fixture.div, "#other"));
    }

    #[test]
    fn descendant_combinator() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, "div p"));
        assert!(is_match(&fixture.doc, fixture.p1, "body div p"));
        assert!(is_match(&fixture.doc, fixture.p1, "html p"));
        assert!(!is_match(&fixture.doc, fixture.div, "body p"));
    }

    #[test]
    fn child_combinator() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, "div > p"));
        assert!(is_match(&fixture.doc, fixture.span, "div > span"));
        assert!(!is_match(&fixture.doc, fixture.p1, "body > p"));
        assert_eq!(fixture.doc.children(fixture.div).len(), 3);
        assert!(!is_match(&fixture.doc, fixture.p1, "div > p > span"));
    }

    #[test]
    fn sibling_combinators() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p2, "p + p"));
        assert!(is_match(&fixture.doc, fixture.span, "p + span"));
        assert!(!is_match(&fixture.doc, fixture.p1, "p + p"));
        assert!(is_match(&fixture.doc, fixture.li2, "li + li"));
        assert!(!is_match(&fixture.doc, fixture.li1, "li + li"));

        assert!(is_match(&fixture.doc, fixture.li3, "li ~ li"));
        assert!(is_match(&fixture.doc, fixture.span, "p ~ span"));
        assert!(!is_match(&fixture.doc, fixture.li1, "li ~ li"));
    }

    #[test]
    fn long_selector_chain() {
        let fixture = fixture();
        assert!(is_match(
            &fixture.doc,
            fixture.p1,
            "html body div.box > p.first"
        ));
        assert!(is_match(
            &fixture.doc,
            fixture.li3,
            "body > ul > li + li + li"
        ));
        assert!(!is_match(&fixture.doc, fixture.li3, "body > li"));
    }

    #[test]
    fn attribute_selectors() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.div, "[data-role]"));
        assert!(is_match(&fixture.doc, fixture.div, "[DATA-ROLE]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-role=Main]"));
        assert!(!is_match(&fixture.doc, fixture.div, "[data-role=main]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-role=main i]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-role^=Ma]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-role$=in]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-role*=ai]"));
        assert!(!is_match(&fixture.doc, fixture.div, "[data-role^=ain]"));
        assert!(is_match(&fixture.doc, fixture.div, "[class~=wide]"));
        assert!(!is_match(&fixture.doc, fixture.div, "[class~=wid]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-lang|=en]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-lang|=EN i]"));
        assert!(!is_match(&fixture.doc, fixture.div, "[data-lang|=e]"));
        assert!(!is_match(&fixture.doc, fixture.p1, "[data-role]"));
    }

    #[test]
    fn child_position_pseudo_classes() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, ":first-child"));
        assert!(!is_match(&fixture.doc, fixture.p2, ":first-child"));
        assert!(is_match(&fixture.doc, fixture.span, ":last-child"));
        assert!(!is_match(&fixture.doc, fixture.p2, ":last-child"));
        assert!(!is_match(&fixture.doc, fixture.li2, ":only-child"));
        assert!(!is_match(&fixture.doc, fixture.div, ":only-child"));
        assert!(is_match(&fixture.doc, fixture.html, ":only-child"));
        assert!(is_match(&fixture.doc, fixture.head, ":first-child"));
        assert!(is_match(&fixture.doc, fixture.body, ":last-child"));
    }

    #[test]
    fn nth_child_formulas() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.li1, "li:nth-child(1)"));
        assert!(is_match(&fixture.doc, fixture.li2, "li:nth-child(2)"));
        assert!(is_match(&fixture.doc, fixture.li3, "li:nth-child(3)"));
        assert!(!is_match(&fixture.doc, fixture.li3, "li:nth-child(4)"));

        assert!(is_match(&fixture.doc, fixture.li1, "li:nth-child(odd)"));
        assert!(is_match(&fixture.doc, fixture.li2, "li:nth-child(even)"));
        assert!(is_match(&fixture.doc, fixture.li3, "li:nth-child(2n+1)"));
        assert!(is_match(&fixture.doc, fixture.li2, "li:nth-child(2n)"));
        assert!(is_match(&fixture.doc, fixture.li1, "li:nth-child(-n+3)"));
        assert!(is_match(&fixture.doc, fixture.li3, "li:nth-child(-n+3)"));
        assert!(!is_match(&fixture.doc, fixture.li3, "li:nth-child(-n+2)"));
        assert!(!is_match(&fixture.doc, fixture.li1, "li:nth-child(-2n+2)"));
        assert!(is_match(&fixture.doc, fixture.li2, "li:nth-child(-2n+2)"));
        assert!(!is_match(&fixture.doc, fixture.li1, "li:nth-child(0)"));
    }

    #[test]
    fn nth_last_child_formulas() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.li3, "li:nth-last-child(1)"));
        assert!(is_match(&fixture.doc, fixture.li1, "li:nth-last-child(3)"));
        assert!(is_match(
            &fixture.doc,
            fixture.li1,
            "li:nth-last-child(odd)"
        ));
        assert!(!is_match(
            &fixture.doc,
            fixture.li2,
            "li:nth-last-child(odd)"
        ));
    }

    #[test]
    fn root_and_empty() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.html, ":root"));
        assert!(!is_match(&fixture.doc, fixture.body, ":root"));
        assert!(is_match(&fixture.doc, fixture.p2, "p:empty"));
        assert!(is_match(&fixture.doc, fixture.span, "span:empty"));
        assert!(!is_match(&fixture.doc, fixture.p1, "p:empty"));
        assert!(!is_match(&fixture.doc, fixture.div, "div:empty"));
    }

    #[test]
    fn type_position_pseudo_classes() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, "p:first-of-type"));
        assert!(!is_match(&fixture.doc, fixture.p2, "p:first-of-type"));
        assert!(is_match(&fixture.doc, fixture.p2, "p:last-of-type"));
        assert!(is_match(&fixture.doc, fixture.span, "span:only-of-type"));
        assert!(!is_match(&fixture.doc, fixture.p1, "p:only-of-type"));
        assert!(is_match(&fixture.doc, fixture.div, "div:only-of-type"));
    }

    #[test]
    fn not_is_and_where() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, ":not(.second)"));
        assert!(!is_match(&fixture.doc, fixture.p1, ":not(p)"));
        assert!(!is_match(&fixture.doc, fixture.div, ":not([data-role])"));
        assert!(is_match(&fixture.doc, fixture.p1, ":not(div, .second)"));
        assert!(is_match(&fixture.doc, fixture.p1, ":is(.first, .nope)"));
        assert!(!is_match(&fixture.doc, fixture.p1, ":is(.second, div)"));
        assert!(is_match(&fixture.doc, fixture.p1, ":where(p.first)"));
        assert!(is_match(
            &fixture.doc,
            fixture.p1,
            "div :is(p, span):first-child"
        ));
    }

    #[test]
    fn interactive_state_pseudo_classes() {
        let fixture = fixture();
        let selector = parse_selector(":hover").unwrap();
        assert!(!matches(&fixture.doc, fixture.p1, &selector));
        let hover = ElementState {
            hover: true,
            ..ElementState::default()
        };
        assert!(matches_with_state(
            &fixture.doc,
            fixture.p1,
            &selector,
            &hover
        ));

        for (text, state) in [
            (
                ":hover",
                ElementState {
                    hover: true,
                    ..ElementState::default()
                },
            ),
            (
                ":focus",
                ElementState {
                    focus: true,
                    ..ElementState::default()
                },
            ),
            (
                ":active",
                ElementState {
                    active: true,
                    ..ElementState::default()
                },
            ),
            (
                ":visited",
                ElementState {
                    visited: true,
                    ..ElementState::default()
                },
            ),
        ] {
            let selector = parse_selector(text).unwrap();
            assert!(
                matches_with_state(&fixture.doc, fixture.p1, &selector, &state),
                "{text}"
            );
            assert!(!matches(&fixture.doc, fixture.p1, &selector), "{text}");
        }
    }

    #[test]
    fn link_pseudo_classes() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.anchor, ":link"));
        assert!(is_match(&fixture.doc, fixture.anchor, ":any-link"));
        assert!(!is_match(&fixture.doc, fixture.anchor, ":visited"));
        assert!(!is_match(&fixture.doc, fixture.div, ":link"));
    }

    #[test]
    fn checked_pseudo_class() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.checkbox, ":checked"));
        assert!(!is_match(&fixture.doc, fixture.text_input, ":checked"));
        assert!(!is_match(&fixture.doc, fixture.div, ":checked"));

        let selector = parse_selector("input:checked").unwrap();
        let unchecked = ElementState {
            checked: Some(false),
            ..ElementState::default()
        };
        assert!(!matches_with_state(
            &fixture.doc,
            fixture.checkbox,
            &selector,
            &unchecked
        ));
        let checked = ElementState {
            checked: Some(true),
            ..ElementState::default()
        };
        assert!(matches_with_state(
            &fixture.doc,
            fixture.text_input,
            &selector,
            &checked
        ));
    }

    #[test]
    fn disabled_and_enabled_pseudo_classes() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.button, ":disabled"));
        assert!(!is_match(&fixture.doc, fixture.text_input, ":disabled"));
        assert!(is_match(&fixture.doc, fixture.text_input, ":enabled"));
        assert!(!is_match(&fixture.doc, fixture.button, ":enabled"));
        assert!(!is_match(&fixture.doc, fixture.div, ":disabled"));
        assert!(!is_match(&fixture.doc, fixture.div, ":enabled"));

        let selector = parse_selector("input:disabled").unwrap();
        let disabled = ElementState {
            disabled: true,
            ..ElementState::default()
        };
        assert!(matches_with_state(
            &fixture.doc,
            fixture.text_input,
            &selector,
            &disabled
        ));
    }

    #[test]
    fn lang_pseudo_class_inherits() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p2, ":lang(fr)"));
        assert!(is_match(&fixture.doc, fixture.p2, ":lang(fr-CA)"));
        assert!(is_match(&fixture.doc, fixture.p2, ":lang(FR)"));
        assert!(!is_match(&fixture.doc, fixture.p2, ":lang(en)"));
        assert!(is_match(&fixture.doc, fixture.p1, ":lang(en)"));
        assert!(!is_match(&fixture.doc, fixture.p1, ":lang(fr)"));
    }

    #[test]
    fn pseudo_elements_never_match() {
        let fixture = fixture();
        assert!(!is_match(&fixture.doc, fixture.p1, "p::before"));
        assert!(!is_match(&fixture.doc, fixture.p1, "p::after"));
        assert!(!is_match(&fixture.doc, fixture.p1, "p:first-line"));
        assert!(!is_match(&fixture.doc, fixture.p1, "p:first-letter"));
        assert!(!is_match(&fixture.doc, fixture.p1, "::selection"));
    }

    #[test]
    fn escaped_identifiers_match() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, ".a\\.b"));
        assert!(is_match(&fixture.doc, fixture.p1, "p.a\\.b"));
        assert!(!is_match(&fixture.doc, fixture.p1, ".ab"));
    }

    #[test]
    fn non_element_nodes_never_match() {
        let fixture = fixture();
        let text = fixture.doc.children(fixture.p1)[0];
        assert!(!is_match(&fixture.doc, text, "*"));
        let comment = fixture.doc.children(fixture.span)[0];
        assert!(!is_match(&fixture.doc, comment, "span"));
    }

    #[test]
    fn specificity_function_matches_field() {
        let selector = parse_selector("div#a.b[c]::before").unwrap();
        assert_eq!(specificity(&selector), Specificity::new(1, 2, 2));
        assert_eq!(selector.specificity, Specificity::new(1, 2, 2));
    }

    #[test]
    fn query_selector_finds_first_in_document_order() {
        let fixture = fixture();
        assert_eq!(
            query_selector(&fixture.doc, fixture.doc.root(), "p").unwrap(),
            Some(fixture.p1)
        );
        assert_eq!(
            query_selector(&fixture.doc, fixture.doc.root(), ".second").unwrap(),
            Some(fixture.p2)
        );
        assert_eq!(
            query_selector(&fixture.doc, fixture.doc.root(), ".nope").unwrap(),
            None
        );
    }

    #[test]
    fn query_selector_respects_subtree() {
        let fixture = fixture();
        assert_eq!(
            query_selector(&fixture.doc, fixture.div, "p").unwrap(),
            Some(fixture.p1)
        );
        assert_eq!(
            query_selector(&fixture.doc, fixture.ul, "li").unwrap(),
            Some(fixture.li1)
        );
        assert_eq!(
            query_selector(&fixture.doc, fixture.div, "div").unwrap(),
            None
        );
        assert_eq!(query_selector(&fixture.doc, fixture.p1, "p").unwrap(), None);
    }

    #[test]
    fn query_selector_all_returns_document_order() {
        let fixture = fixture();
        let lis = query_selector_all(&fixture.doc, fixture.doc.root(), "li").unwrap();
        assert_eq!(lis, vec![fixture.li1, fixture.li2, fixture.li3]);
        let inputs = query_selector_all(&fixture.doc, fixture.doc.root(), "input").unwrap();
        assert_eq!(inputs, vec![fixture.checkbox, fixture.text_input]);
    }

    #[test]
    fn query_selector_all_deduplicates() {
        let fixture = fixture();
        let matched =
            query_selector_all(&fixture.doc, fixture.doc.root(), "p, .first, #main p").unwrap();
        assert_eq!(matched, vec![fixture.p1, fixture.p2]);
    }

    #[test]
    fn query_selector_all_on_subtree() {
        let fixture = fixture();
        let matched = query_selector_all(&fixture.doc, fixture.ul, "li:not(.two)").unwrap();
        assert_eq!(matched, vec![fixture.li1, fixture.li3]);
        let matched = query_selector_all(&fixture.doc, fixture.body, "body, html").unwrap();
        assert!(matched.is_empty());
    }

    #[test]
    fn query_selector_reports_errors() {
        let fixture = fixture();
        assert!(query_selector(&fixture.doc, fixture.doc.root(), "a >").is_err());
        assert!(query_selector_all(&fixture.doc, fixture.doc.root(), "[").is_err());
    }

    #[test]
    fn selectors_match_multiple_compounds_with_state() {
        let fixture = fixture();
        let selector = parse_selector("div > p:first-child").unwrap();
        assert!(matches(&fixture.doc, fixture.p1, &selector));
        assert!(!matches(&fixture.doc, fixture.p2, &selector));

        let selector = parse_selector("body:not(.x) > ul li:nth-child(2)").unwrap();
        assert!(matches(&fixture.doc, fixture.li2, &selector));
        assert!(!matches(&fixture.doc, fixture.li1, &selector));
    }

    #[test]
    fn selector_list_parsing_used_by_query() {
        let fixture = fixture();
        let selectors = parse_selectors("#main, ul > li.two").unwrap();
        assert_eq!(selectors.len(), 2);
        assert!(matches(&fixture.doc, fixture.div, &selectors[0]));
        assert!(matches(&fixture.doc, fixture.li2, &selectors[1]));
    }
}
