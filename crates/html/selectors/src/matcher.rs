//! Right-to-left selector matching against the DOM.

use browser_dom::{Document, NodeData, NodeId};

use crate::{
    AttrOp, Combinator, CompoundSelector, Direction, ElementState, PseudoClass, Selector,
    SelectorError, SimpleSelector, Specificity,
};

/// Returns the specificity recorded in a parsed selector.
#[must_use]
pub fn specificity(selector: &Selector) -> Specificity {
    selector.specificity
}

/// Matches a selector against a node with default (idle) element state.
///
/// Pseudo-elements never match here: a selector such as `p::before` does not
/// match the `p` element. Use [`matches_pseudo_element_host`] to match the
/// element a pseudo-element selector targets.
#[must_use]
pub fn matches(doc: &Document, node: NodeId, selector: &Selector) -> bool {
    matches_with_state(doc, node, selector, &ElementState::default())
}

/// Matches a selector against a node with the given interactive state.
///
/// Pseudo-elements never match here; see [`matches`].
#[must_use]
pub fn matches_with_state(
    doc: &Document,
    node: NodeId,
    selector: &Selector,
    state: &ElementState,
) -> bool {
    matches_inner(doc, node, selector, state, false)
}

/// Matches the *host element* of a pseudo-element selector with default
/// (idle) element state.
///
/// Unlike [`matches`], the trailing pseudo-element simple is treated as a
/// wildcard that matches, so `p.icon::before` matches `<p class="icon">`.
/// Selectors without a pseudo-element behave exactly as in [`matches`].
/// Legacy single-colon forms (`:before`) work too, since the parser records
/// them as pseudo-elements.
#[must_use]
pub fn matches_pseudo_element_host(doc: &Document, node: NodeId, selector: &Selector) -> bool {
    matches_pseudo_element_host_with_state(doc, node, selector, &ElementState::default())
}

/// Matches the host element of a pseudo-element selector with the given
/// interactive state.
///
/// See [`matches_pseudo_element_host`].
#[must_use]
pub fn matches_pseudo_element_host_with_state(
    doc: &Document,
    node: NodeId,
    selector: &Selector,
    state: &ElementState,
) -> bool {
    matches_inner(doc, node, selector, state, true)
}

/// Matches a `::slotted()` selector against a light-DOM child of a shadow host.
///
/// `node` must actually be assigned to a `<slot>` in a shadow tree and the
/// selector's argument must match it. The compounds preceding
/// `::slotted(...)` match the slot itself, so `slot[name=title]::slotted(h1)`
/// matches an `<h1>` assigned to the slot named `title`.
///
/// Like other pseudo-elements, `::slotted()` never matches through
/// [`matches`]; use this function (or [`matches_slotted_with_state`]).
#[must_use]
pub fn matches_slotted(doc: &Document, node: NodeId, selector: &Selector) -> bool {
    matches_slotted_with_state(doc, node, selector, &ElementState::default())
}

/// Like [`matches_slotted`], but honours interactive pseudo-class state.
#[must_use]
pub fn matches_slotted_with_state(
    doc: &Document,
    node: NodeId,
    selector: &Selector,
    state: &ElementState,
) -> bool {
    if selector.pseudo_element.as_deref() != Some("slotted") {
        return false;
    }
    let Some(slot) = doc.assigned_slot(node) else {
        return false;
    };
    // `ignore_pseudo_elements = true` treats the `::slotted()` simple as a
    // wildcard so the preceding compounds match the slot element itself.
    if !matches_inner(doc, slot, selector, state, true) {
        return false;
    }
    let Some(argument) = selector.pseudo_element_argument.as_deref() else {
        return false;
    };
    let Ok(selectors) = crate::parse_selectors(argument) else {
        return false;
    };
    selectors
        .iter()
        .any(|candidate| matches_inner(doc, node, candidate, state, false))
}

fn matches_inner(
    doc: &Document,
    node: NodeId,
    selector: &Selector,
    state: &ElementState,
    ignore_pseudo_elements: bool,
) -> bool {
    if selector.parts.is_empty() || !doc.node(node).is_element() {
        return false;
    }
    match_parts(
        doc,
        node,
        &selector.parts,
        selector.parts.len() - 1,
        state,
        ignore_pseudo_elements,
    )
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
    ignore_pseudo_elements: bool,
) -> bool {
    if !matches_compound(doc, node, &parts[index].1, state, ignore_pseudo_elements) {
        return false;
    }
    if index == 0 {
        return true;
    }

    let combinator = parts[index].0.unwrap_or(Combinator::Descendant);
    match combinator {
        Combinator::Descendant => doc.ancestors(node).any(|ancestor| {
            doc.node(ancestor).is_element()
                && match_parts(
                    doc,
                    ancestor,
                    parts,
                    index - 1,
                    state,
                    ignore_pseudo_elements,
                )
        }),
        Combinator::Child => doc.parent_element(node).is_some_and(|parent| {
            match_parts(doc, parent, parts, index - 1, state, ignore_pseudo_elements)
        }),
        Combinator::NextSibling => doc.previous_element_sibling(node).is_some_and(|sibling| {
            match_parts(
                doc,
                sibling,
                parts,
                index - 1,
                state,
                ignore_pseudo_elements,
            )
        }),
        Combinator::SubsequentSibling => {
            let mut current = doc.previous_element_sibling(node);
            while let Some(sibling) = current {
                if match_parts(
                    doc,
                    sibling,
                    parts,
                    index - 1,
                    state,
                    ignore_pseudo_elements,
                ) {
                    return true;
                }
                current = doc.previous_element_sibling(sibling);
            }
            false
        }
    }
}

/// Matches a relative selector (the argument of `:has()`) against `anchor`.
///
/// The subject of a relative selector is its last compound. It is matched
/// normally, then the combinators are followed backwards; the leading
/// combinator on the first compound is checked against `anchor` instead of
/// searching all ancestors. This bounds the match to the anchor's relation
/// (descendant, child, next sibling or subsequent sibling).
fn matches_relative(
    doc: &Document,
    anchor: NodeId,
    relative: &Selector,
    state: &ElementState,
    ignore_pseudo_elements: bool,
) -> bool {
    if relative.parts.is_empty() {
        return false;
    }
    // Only elements can satisfy a relative selector. The leading-combinator
    // check in `match_relative_parts` excludes the anchor itself for the
    // descendant and child forms.
    doc.nodes().filter(|node| node.is_element()).any(|node| {
        match_relative_parts(
            doc,
            node.id,
            &relative.parts,
            relative.parts.len() - 1,
            anchor,
            state,
            ignore_pseudo_elements,
        )
    })
}

fn match_relative_parts(
    doc: &Document,
    node: NodeId,
    parts: &[(Option<Combinator>, CompoundSelector)],
    index: usize,
    anchor: NodeId,
    state: &ElementState,
    ignore_pseudo_elements: bool,
) -> bool {
    if !matches_compound(doc, node, &parts[index].1, state, ignore_pseudo_elements) {
        return false;
    }
    if index == 0 {
        let leading = parts[0].0.unwrap_or(Combinator::Descendant);
        return relation_holds(doc, anchor, node, leading);
    }
    let combinator = parts[index].0.unwrap_or(Combinator::Descendant);
    match combinator {
        Combinator::Descendant => doc.ancestors(node).any(|ancestor| {
            doc.node(ancestor).is_element()
                && match_relative_parts(
                    doc,
                    ancestor,
                    parts,
                    index - 1,
                    anchor,
                    state,
                    ignore_pseudo_elements,
                )
        }),
        Combinator::Child => doc.parent_element(node).is_some_and(|parent| {
            match_relative_parts(
                doc,
                parent,
                parts,
                index - 1,
                anchor,
                state,
                ignore_pseudo_elements,
            )
        }),
        Combinator::NextSibling => doc.previous_element_sibling(node).is_some_and(|sibling| {
            match_relative_parts(
                doc,
                sibling,
                parts,
                index - 1,
                anchor,
                state,
                ignore_pseudo_elements,
            )
        }),
        Combinator::SubsequentSibling => {
            let mut current = doc.previous_element_sibling(node);
            while let Some(sibling) = current {
                if match_relative_parts(
                    doc,
                    sibling,
                    parts,
                    index - 1,
                    anchor,
                    state,
                    ignore_pseudo_elements,
                ) {
                    return true;
                }
                current = doc.previous_element_sibling(sibling);
            }
            false
        }
    }
}

/// True when `node` stands in `combinator` relation to `anchor`.
fn relation_holds(doc: &Document, anchor: NodeId, node: NodeId, combinator: Combinator) -> bool {
    match combinator {
        Combinator::Descendant => doc.is_ancestor_of(anchor, node),
        Combinator::Child => doc.parent_element(node) == Some(anchor),
        Combinator::NextSibling => doc.previous_element_sibling(node) == Some(anchor),
        Combinator::SubsequentSibling => {
            let mut current = doc.previous_element_sibling(node);
            while let Some(sibling) = current {
                if sibling == anchor {
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
    ignore_pseudo_elements: bool,
) -> bool {
    compound
        .simples
        .iter()
        .all(|simple| matches_simple(doc, node, simple, state, ignore_pseudo_elements))
}

fn matches_simple(
    doc: &Document,
    node: NodeId,
    simple: &SimpleSelector,
    state: &ElementState,
    ignore_pseudo_elements: bool,
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
        SimpleSelector::Pseudo(class) => {
            matches_pseudo_class(doc, node, class, state, ignore_pseudo_elements)
        }
        // A pseudo-element is not a real node. When matching the host of a
        // pseudo-element rule it is treated as an always-true simple.
        SimpleSelector::PseudoElement(_) => ignore_pseudo_elements,
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

/// The shadow host whose tree `node` belongs to, if any.
///
/// A host element matches `:host` / `:host-context()` when styles are
/// evaluated in its own shadow tree (`state.host`) or when it owns a shadow
/// root. For a node inside a shadow tree, the enclosing host is used.
fn shadow_context_host(doc: &Document, node: NodeId, state: &ElementState) -> Option<NodeId> {
    if state.host || doc.shadow_root(node).is_some() {
        return Some(node);
    }
    doc.enclosing_host(node)
}

fn matches_pseudo_class(
    doc: &Document,
    node: NodeId,
    class: &PseudoClass,
    state: &ElementState,
    ignore_pseudo_elements: bool,
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
        PseudoClass::NthChildOf(a, b, selectors) => matches_an_plus_b(
            filtered_sibling_position(doc, node, selectors, state, ignore_pseudo_elements, false)
                as i64,
            i64::from(*a),
            i64::from(*b),
        ),
        PseudoClass::NthLastChildOf(a, b, selectors) => matches_an_plus_b(
            filtered_sibling_position(doc, node, selectors, state, ignore_pseudo_elements, true)
                as i64,
            i64::from(*a),
            i64::from(*b),
        ),
        PseudoClass::NthOfType(a, b) => matches_an_plus_b(
            type_position(doc, node).map_or(0, |(position, _)| position) as i64,
            i64::from(*a),
            i64::from(*b),
        ),
        PseudoClass::NthLastOfType(a, b) => matches_an_plus_b(
            type_position(doc, node).map_or(0, |(position, count)| count - position + 1) as i64,
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
            .any(|selector| matches_inner(doc, node, selector, state, ignore_pseudo_elements)),
        PseudoClass::Is(selectors) | PseudoClass::Where(selectors) => selectors
            .iter()
            .any(|selector| matches_inner(doc, node, selector, state, ignore_pseudo_elements)),
        PseudoClass::Has(selectors) => selectors
            .iter()
            .any(|relative| matches_relative(doc, node, relative, state, ignore_pseudo_elements)),
        PseudoClass::Hover => state.hover,
        PseudoClass::Focus => state.focus,
        PseudoClass::Active => state.active,
        PseudoClass::Visited => state.visited,
        PseudoClass::Link | PseudoClass::AnyLink => is_link(doc, node),
        PseudoClass::Target => state.target || fragment_targets(doc, node),
        PseudoClass::Checked => checked(doc, node, state),
        PseudoClass::Disabled => supports_disabled(doc, node) && is_disabled(doc, node, state),
        PseudoClass::Enabled => supports_disabled(doc, node) && !is_disabled(doc, node, state),
        PseudoClass::Lang(language) => {
            element_language(doc, node).is_some_and(|actual| language_matches(&actual, language))
        }
        PseudoClass::FocusVisible => state.focus_visible,
        PseudoClass::FocusWithin => state.focus || state.focus_within,
        PseudoClass::Valid => !is_invalid(doc, node, state),
        PseudoClass::Invalid => is_invalid(doc, node, state),
        PseudoClass::Required => supports_required(doc, node) && doc.has_attr(node, "required"),
        PseudoClass::Optional => supports_required(doc, node) && !doc.has_attr(node, "required"),
        PseudoClass::PlaceholderShown => placeholder_shown(doc, node),
        PseudoClass::ReadOnly => !is_read_write(doc, node),
        PseudoClass::ReadWrite => is_read_write(doc, node),
        PseudoClass::Dir(wanted) => element_direction(doc, node) == *wanted,
        PseudoClass::Host => state.host || doc.shadow_root(node).is_some(),
        PseudoClass::HostContext(selectors) => {
            let Some(host) = shadow_context_host(doc, node, state) else {
                return false;
            };
            std::iter::once(host)
                .chain(doc.ancestors(host))
                .any(|ancestor| {
                    doc.node(ancestor).is_element()
                        && selectors.iter().any(|selector| {
                            matches_inner(doc, ancestor, selector, state, ignore_pseudo_elements)
                        })
                })
        }
        PseudoClass::Defined => is_defined(doc, node, state),
        PseudoClass::Modal => state.modal,
        PseudoClass::Autofill => state.autofill,
        PseudoClass::UserInvalid => state.user_invalid,
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

/// `:target` matches the element whose id equals the fragment of the
/// document's base URL. `ElementState::target` can force a match when the
/// caller tracks navigation separately from [`browser_dom::Document::base_url`].
fn fragment_targets(doc: &Document, node: NodeId) -> bool {
    let Some(base_url) = doc.base_url.as_deref() else {
        return false;
    };
    let Some((_, fragment)) = base_url.split_once('#') else {
        return false;
    };
    if fragment.is_empty() {
        return false;
    }
    // Fragments are percent-encoded in URLs; compare against the raw id and
    // also against a simple percent-decoded form.
    doc.attr(node, "id")
        .is_some_and(|id| id == fragment || percent_decode(fragment) == id)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (
                (bytes[index + 1] as char).to_digit(16),
                (bytes[index + 2] as char).to_digit(16),
            )
        {
            out.push((high * 16 + low) as u8);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// 1-based position of `node` among element siblings matched by `selectors`,
/// counted from either end. Returns 0 when the node itself does not match.
fn filtered_sibling_position(
    doc: &Document,
    node: NodeId,
    selectors: &[Selector],
    state: &ElementState,
    ignore_pseudo_elements: bool,
    from_end: bool,
) -> usize {
    let filtered: Vec<NodeId> = element_siblings(doc, node)
        .into_iter()
        .filter(|sibling| {
            selectors.iter().any(|selector| {
                matches_inner(doc, *sibling, selector, state, ignore_pseudo_elements)
            })
        })
        .collect();
    let Some(index) = filtered.iter().position(|sibling| *sibling == node) else {
        return 0;
    };
    if from_end {
        filtered.len() - index
    } else {
        index + 1
    }
}

/// Elements that support the `required` attribute.
fn supports_required(doc: &Document, node: NodeId) -> bool {
    matches!(
        doc.element_name(node),
        Some("input" | "select" | "textarea")
    )
}

/// `:placeholder-shown` matches a text control with a placeholder whose value
/// is currently empty.
fn placeholder_shown(doc: &Document, node: NodeId) -> bool {
    if !matches!(doc.element_name(node), Some("input" | "textarea")) {
        return false;
    }
    let Some(placeholder) = doc.attr(node, "placeholder") else {
        return false;
    };
    if placeholder.is_empty() {
        return false;
    }
    doc.attr(node, "value").unwrap_or("").is_empty()
}

/// `:read-write` matches editable controls and elements with an effective
/// `contenteditable="true"` (inherited from ancestors unless overridden).
fn is_read_write(doc: &Document, node: NodeId) -> bool {
    match doc.element_name(node) {
        Some("input" | "textarea") => {
            !doc.has_attr(node, "readonly") && !doc.has_attr(node, "disabled")
        }
        _ => {
            let mut current = Some(node);
            while let Some(id) = current {
                if let Some(value) = doc.attr(id, "contenteditable") {
                    return matches!(
                        value.to_ascii_lowercase().as_str(),
                        "" | "true" | "plaintext-only"
                    );
                }
                current = doc.parent(id);
            }
            false
        }
    }
}

/// Computed directionality: the nearest `dir` ancestor (including the element
/// itself), defaulting to left-to-right.
fn element_direction(doc: &Document, node: NodeId) -> Direction {
    let mut current = Some(node);
    while let Some(id) = current {
        if let Some(dir) = doc.attr(id, "dir") {
            match dir.to_ascii_lowercase().as_str() {
                "ltr" => return Direction::Ltr,
                "rtl" => return Direction::Rtl,
                // `auto` depends on content; keep inheriting.
                _ => {}
            }
        }
        current = doc.parent(id);
    }
    Direction::Ltr
}

/// Built-in elements are always defined; custom elements (names containing a
/// hyphen) only match `:defined` when the caller marks them as registered.
fn is_defined(doc: &Document, node: NodeId, state: &ElementState) -> bool {
    doc.element_name(node)
        .is_none_or(|name| !name.contains('-')) // built-in
        || state.defined
}

fn supports_constraint_validation(doc: &Document, node: NodeId) -> bool {
    supports_required(doc, node)
}

/// Whether a required control currently has an empty value.
fn value_is_empty(doc: &Document, node: NodeId, state: &ElementState) -> bool {
    match doc.element_name(node) {
        Some("input") => {
            let kind = doc
                .attr(node, "type")
                .unwrap_or("text")
                .to_ascii_lowercase();
            if matches!(kind.as_str(), "checkbox" | "radio") {
                return !checked(doc, node, state);
            }
            doc.attr(node, "value").unwrap_or("").is_empty()
        }
        Some("textarea") => doc.text_content(node).is_empty(),
        Some("select") => !doc
            .element_children(node)
            .iter()
            .any(|child| doc.is_tag(*child, "option")),
        _ => true,
    }
}

/// Basic constraint validation: an override wins; otherwise a required form
/// control with an empty value is invalid. Elements that do not participate in
/// constraint validation are always valid.
fn is_invalid(doc: &Document, node: NodeId, state: &ElementState) -> bool {
    if let Some(valid) = state.valid {
        return !valid;
    }
    supports_constraint_validation(doc, node)
        && doc.has_attr(node, "required")
        && value_is_empty(doc, node, state)
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

    struct HasFixture {
        doc: Document,
        s1: NodeId,
        s2: NodeId,
        child_div: NodeId,
        inner: NodeId,
        outer: NodeId,
        prev: NodeId,
        target: NodeId,
        next: NodeId,
    }

    /// Builds a document for exercising `:has()` in all its basic forms:
    ///
    /// ```text
    /// body
    ///  ├─ section#s1.container
    ///  │   ├─ div#c1.child
    ///  │   │   └─ span#d1.deep
    ///  │   ├─ p#c2.child
    ///  │   └─ a.hoverable
    ///  ├─ section#s2.container
    ///  ├─ div#outer.outer
    ///  │   ├─ div#inner.inner
    ///  │   │   └─ span#leaf.leaf
    ///  │   └─ p#marker.marker
    ///  └─ div#prev, div#target, div#next
    /// ```
    fn has_fixture() -> HasFixture {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);

        let s1 = doc.create_element("section");
        doc.set_attr(s1, "id", "s1");
        doc.set_attr(s1, "class", "container");
        let child_div = doc.create_element("div");
        doc.set_attr(child_div, "id", "c1");
        doc.set_attr(child_div, "class", "child");
        let deep = doc.create_element("span");
        doc.set_attr(deep, "id", "d1");
        doc.set_attr(deep, "class", "deep");
        let c2 = doc.create_element("p");
        doc.set_attr(c2, "id", "c2");
        doc.set_attr(c2, "class", "child");
        let hoverable = doc.create_element("a");
        doc.set_attr(hoverable, "class", "hoverable");
        doc.set_attr(hoverable, "href", "/h");
        doc.append_child(child_div, deep);
        doc.append_child(body, s1);
        doc.append_child(s1, child_div);
        doc.append_child(s1, c2);
        doc.append_child(s1, hoverable);

        let s2 = doc.create_element("section");
        doc.set_attr(s2, "id", "s2");
        doc.set_attr(s2, "class", "container");
        doc.append_child(body, s2);

        let outer = doc.create_element("div");
        doc.set_attr(outer, "id", "outer");
        doc.set_attr(outer, "class", "outer");
        let inner = doc.create_element("div");
        doc.set_attr(inner, "id", "inner");
        doc.set_attr(inner, "class", "inner");
        let leaf = doc.create_element("span");
        doc.set_attr(leaf, "id", "leaf");
        doc.set_attr(leaf, "class", "leaf");
        let marker = doc.create_element("p");
        doc.set_attr(marker, "id", "marker");
        doc.set_attr(marker, "class", "marker");
        doc.append_child(inner, leaf);
        doc.append_child(outer, inner);
        doc.append_child(outer, marker);
        doc.append_child(body, outer);

        let prev = doc.create_element("div");
        doc.set_attr(prev, "id", "prev");
        let target = doc.create_element("div");
        doc.set_attr(target, "id", "target");
        let next = doc.create_element("div");
        doc.set_attr(next, "id", "next");
        doc.append_child(body, prev);
        doc.append_child(body, target);
        doc.append_child(body, next);

        HasFixture {
            doc,
            s1,
            s2,
            child_div,
            inner,
            outer,
            prev,
            target,
            next,
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
    fn attribute_operators_with_case_flags() {
        let fixture = fixture();
        // `i` applies to every string operator, not just `=`.
        assert!(is_match(&fixture.doc, fixture.div, "[data-role^=MAIN i]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-role$=IN i]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-role*=AIN i]"));
        assert!(is_match(&fixture.doc, fixture.div, "[class~=BOX i]"));
        assert!(is_match(&fixture.doc, fixture.div, "[data-lang|=EN i]"));
        assert!(is_match(&fixture.doc, fixture.p1, "[title~=WORLD i]"));
        // `s` keeps the default, case-sensitive behaviour.
        assert!(is_match(&fixture.doc, fixture.div, "[data-role=Main s]"));
        assert!(!is_match(&fixture.doc, fixture.div, "[data-role=main s]"));
        // HTML attribute names are case-insensitive.
        assert!(is_match(&fixture.doc, fixture.div, "[DATA-ROLE=Main]"));
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
    fn pseudo_element_host_matching() {
        let fixture = fixture();
        let before = parse_selector("p::before").unwrap();
        // Ordinary matching still never matches a pseudo-element selector.
        assert!(!matches(&fixture.doc, fixture.p1, &before));
        // Host matching targets the real element.
        assert!(matches_pseudo_element_host(
            &fixture.doc,
            fixture.p1,
            &before
        ));
        assert!(!matches_pseudo_element_host(
            &fixture.doc,
            fixture.div,
            &before
        ));

        let after = parse_selector("p:after").unwrap();
        assert!(matches_pseudo_element_host(
            &fixture.doc,
            fixture.p1,
            &after
        ));
        assert!(!matches(&fixture.doc, fixture.p1, &after));

        let complex = parse_selector("div.box > p.first::before").unwrap();
        assert!(matches_pseudo_element_host(
            &fixture.doc,
            fixture.p1,
            &complex
        ));
        assert!(!matches_pseudo_element_host(
            &fixture.doc,
            fixture.p2,
            &complex
        ));

        let descendant = parse_selector("#main span::after").unwrap();
        assert!(matches_pseudo_element_host(
            &fixture.doc,
            fixture.span,
            &descendant
        ));
        assert!(!matches_pseudo_element_host(
            &fixture.doc,
            fixture.p1,
            &descendant
        ));
    }

    #[test]
    fn pseudo_element_host_with_state() {
        let fixture = fixture();
        let selector = parse_selector("p:hover::before").unwrap();
        assert!(!matches_pseudo_element_host(
            &fixture.doc,
            fixture.p1,
            &selector
        ));
        let hover = ElementState {
            hover: true,
            ..ElementState::default()
        };
        assert!(matches_pseudo_element_host_with_state(
            &fixture.doc,
            fixture.p1,
            &selector,
            &hover
        ));
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

    #[test]
    fn has_descendant_and_child_forms() {
        let fixture = has_fixture();
        assert!(is_match(&fixture.doc, fixture.s1, "#s1:has(.deep)"));
        assert!(is_match(&fixture.doc, fixture.s1, "#s1:has(.child)"));
        assert!(is_match(&fixture.doc, fixture.s1, "#s1:has(> .child)"));
        assert!(is_match(&fixture.doc, fixture.s1, "#s1:has(> *)"));
        assert!(!is_match(&fixture.doc, fixture.s2, "#s2:has(> *)"));
        assert!(is_match(&fixture.doc, fixture.s1, "#s1:has(> div.child)"));
        assert!(is_match(&fixture.doc, fixture.s1, "#s1:has(> div > .deep)"));
        assert!(is_match(
            &fixture.doc,
            fixture.child_div,
            "#c1:has(> .deep)"
        ));
        assert!(is_match(&fixture.doc, fixture.child_div, "#c1:has(.deep)"));
        assert!(!is_match(
            &fixture.doc,
            fixture.child_div,
            "#c1:has(> .child)"
        ));
        assert!(!is_match(&fixture.doc, fixture.s2, "#s2:has(.child)"));
        assert!(!is_match(&fixture.doc, fixture.s2, "#s2:has(> .child)"));
        assert!(!is_match(&fixture.doc, fixture.s1, "#s1:has(> p.deep)"));
        assert!(!is_match(&fixture.doc, fixture.s1, "#s1:has(.missing)"));
    }

    #[test]
    fn has_is_bounded_to_the_anchor() {
        let fixture = has_fixture();
        assert!(is_match(&fixture.doc, fixture.inner, "#inner:has(.leaf)"));
        assert!(!is_match(
            &fixture.doc,
            fixture.inner,
            "#inner:has(.marker)"
        ));
        assert!(!is_match(
            &fixture.doc,
            fixture.inner,
            "#inner:has(.outer .marker)"
        ));
        assert!(is_match(&fixture.doc, fixture.outer, "#outer:has(.marker)"));
        assert!(is_match(
            &fixture.doc,
            fixture.outer,
            "#outer:has(.inner .leaf)"
        ));
        // The anchor is not a descendant of itself.
        assert!(!is_match(&fixture.doc, fixture.outer, "#outer:has(.outer)"));
    }

    #[test]
    fn has_sibling_forms() {
        let fixture = has_fixture();
        assert!(is_match(&fixture.doc, fixture.prev, "#prev:has(+ #target)"));
        assert!(!is_match(
            &fixture.doc,
            fixture.target,
            "#target:has(+ #target)"
        ));
        assert!(is_match(&fixture.doc, fixture.prev, "#prev:has(~ #target)"));
        assert!(!is_match(
            &fixture.doc,
            fixture.next,
            "#next:has(~ #target)"
        ));
        assert!(!is_match(&fixture.doc, fixture.prev, "#prev:has(+ #next)"));
    }

    #[test]
    fn has_selector_lists_and_nesting() {
        let fixture = has_fixture();
        assert!(is_match(
            &fixture.doc,
            fixture.s1,
            "#s1:has(.missing, .deep)"
        ));
        assert!(!is_match(
            &fixture.doc,
            fixture.s2,
            "#s2:has(.missing, .deep)"
        ));
        assert!(is_match(
            &fixture.doc,
            fixture.s1,
            "#s1:has(> .child:not(p))"
        ));
        assert!(is_match(
            &fixture.doc,
            fixture.s1,
            "body:has(> section#s1) > section:has(.deep)"
        ));
        assert!(is_match(
            &fixture.doc,
            fixture.child_div,
            ":is(#c1):has(> .deep)"
        ));
    }

    #[test]
    fn has_works_in_query_selector_all() {
        let fixture = has_fixture();
        let found =
            query_selector_all(&fixture.doc, fixture.doc.root(), "section:has(.deep)").unwrap();
        assert_eq!(found, vec![fixture.s1]);
        let found = query_selector_all(&fixture.doc, fixture.doc.root(), ":has(> .child)").unwrap();
        assert_eq!(found, vec![fixture.s1]);
        assert!(
            query_selector_all(&fixture.doc, fixture.doc.root(), "#s2:has(*)")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn has_honours_element_state() {
        let fixture = has_fixture();
        let selector = parse_selector("#s1:has(> .hoverable:hover)").unwrap();
        assert!(!matches(&fixture.doc, fixture.s1, &selector));
        let hover = ElementState {
            hover: true,
            ..ElementState::default()
        };
        assert!(matches_with_state(
            &fixture.doc,
            fixture.s1,
            &selector,
            &hover
        ));
    }

    #[test]
    fn nth_of_type_formulas() {
        let fixture = fixture();
        assert!(is_match(&fixture.doc, fixture.p1, "p:nth-of-type(1)"));
        assert!(is_match(&fixture.doc, fixture.p2, "p:nth-of-type(2)"));
        assert!(!is_match(&fixture.doc, fixture.p2, "p:nth-of-type(1)"));
        assert!(is_match(&fixture.doc, fixture.p1, "p:nth-of-type(odd)"));
        assert!(is_match(&fixture.doc, fixture.p2, "p:nth-of-type(even)"));
        assert!(is_match(&fixture.doc, fixture.span, "span:nth-of-type(1)"));
        assert!(!is_match(&fixture.doc, fixture.span, "span:nth-of-type(2)"));
        assert!(!is_match(&fixture.doc, fixture.p2, "p:nth-of-type(3)"));

        assert!(is_match(&fixture.doc, fixture.p2, "p:nth-last-of-type(1)"));
        assert!(is_match(&fixture.doc, fixture.p1, "p:nth-last-of-type(2)"));
        assert!(!is_match(&fixture.doc, fixture.p1, "p:nth-last-of-type(1)"));
        assert!(is_match(&fixture.doc, fixture.li2, "li:nth-of-type(2n)"));
        assert!(!is_match(&fixture.doc, fixture.li1, "li:nth-of-type(2n)"));
        assert!(is_match(
            &fixture.doc,
            fixture.li1,
            "li:nth-last-of-type(3)"
        ));
        assert!(is_match(
            &fixture.doc,
            fixture.li1,
            "li:nth-last-of-type(odd)"
        ));
        assert!(!is_match(
            &fixture.doc,
            fixture.li2,
            "li:nth-last-of-type(odd)"
        ));
    }

    #[test]
    fn target_matches_fragment_and_state() {
        let fixture = fixture();
        assert!(!is_match(&fixture.doc, fixture.div, ":target"));

        let mut doc = fixture.doc.clone();
        doc.base_url = Some("https://example.test/page#main".into());
        assert!(is_match(&doc, fixture.div, "#main:target"));
        assert!(is_match(&doc, fixture.div, ":target"));
        assert!(!is_match(&doc, fixture.p1, ":target"));

        // State can force a match independently of the URL fragment.
        let selector = parse_selector("p:target").unwrap();
        let state = ElementState {
            target: true,
            ..ElementState::default()
        };
        assert!(matches_with_state(&doc, fixture.p1, &selector, &state));
        assert!(!matches(&doc, fixture.p1, &selector));
    }

    #[test]
    fn target_decodes_percent_encoded_fragments() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let section = doc.create_element("section");
        doc.set_attr(section, "id", "a b");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, section);
        doc.base_url = Some("https://example.test/page#a%20b".into());
        assert!(is_match(&doc, section, ":target"));

        doc.base_url = Some("https://example.test/page".into());
        assert!(!is_match(&doc, section, ":target"));
    }

    fn is_match_state(doc: &Document, node: NodeId, text: &str, state: &ElementState) -> bool {
        let selector = parse_selector(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        matches_with_state(doc, node, &selector, state)
    }

    struct FormFixture {
        doc: Document,
        req: NodeId,
        empty: NodeId,
        opt: NodeId,
        ro: NodeId,
        textarea: NodeId,
        editable: NodeId,
        edit_child: NodeId,
        edit_off: NodeId,
        plain: NodeId,
        rtl: NodeId,
        rtl_child: NodeId,
        custom: NodeId,
        checkbox: NodeId,
        checked_box: NodeId,
    }

    fn form_fixture() -> FormFixture {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);

        macro_rules! input {
            ($id:expr) => {{
                let node = doc.create_element("input");
                doc.set_attr(node, "id", $id);
                doc.append_child(body, node);
                node
            }};
        }

        let req = input!("req");
        doc.set_attr(req, "required", "");
        doc.set_attr(req, "value", "hi");

        let empty = input!("empty");
        doc.set_attr(empty, "required", "");
        doc.set_attr(empty, "placeholder", "Name");

        let opt = input!("opt");
        let ro = input!("ro");
        doc.set_attr(ro, "readonly", "");

        let textarea = doc.create_element("textarea");
        doc.set_attr(textarea, "id", "textarea");
        doc.set_attr(textarea, "placeholder", "Msg");
        doc.append_child(body, textarea);

        let editable = doc.create_element("div");
        doc.set_attr(editable, "id", "editable");
        doc.set_attr(editable, "contenteditable", "true");
        doc.append_child(body, editable);
        let edit_child = doc.create_element("span");
        doc.set_attr(edit_child, "id", "edit-child");
        doc.append_child(editable, edit_child);
        let edit_off = doc.create_element("span");
        doc.set_attr(edit_off, "id", "edit-off");
        doc.set_attr(edit_off, "contenteditable", "false");
        doc.append_child(editable, edit_off);

        let plain = doc.create_element("div");
        doc.set_attr(plain, "id", "plain");
        doc.append_child(body, plain);

        let rtl = doc.create_element("div");
        doc.set_attr(rtl, "id", "rtl");
        doc.set_attr(rtl, "dir", "rtl");
        doc.append_child(body, rtl);
        let rtl_child = doc.create_element("div");
        doc.set_attr(rtl_child, "id", "rtl-child");
        doc.append_child(rtl, rtl_child);

        let custom = doc.create_element("my-widget");
        doc.set_attr(custom, "id", "custom");
        doc.append_child(body, custom);

        let checkbox = input!("checkbox");
        doc.set_attr(checkbox, "type", "checkbox");
        doc.set_attr(checkbox, "required", "");
        let checked_box = input!("checked-box");
        doc.set_attr(checked_box, "type", "checkbox");
        doc.set_attr(checked_box, "checked", "");

        FormFixture {
            doc,
            req,
            empty,
            opt,
            ro,
            textarea,
            editable,
            edit_child,
            edit_off,
            plain,
            rtl,
            rtl_child,
            custom,
            checkbox,
            checked_box,
        }
    }

    #[test]
    fn required_and_optional_pseudo_classes() {
        let f = form_fixture();
        assert!(is_match(&f.doc, f.req, ":required"));
        assert!(is_match(&f.doc, f.empty, "input:required"));
        assert!(is_match(&f.doc, f.checkbox, ":required"));
        assert!(!is_match(&f.doc, f.opt, ":required"));
        assert!(!is_match(&f.doc, f.plain, ":required"));

        assert!(is_match(&f.doc, f.opt, ":optional"));
        assert!(is_match(&f.doc, f.textarea, ":optional"));
        assert!(!is_match(&f.doc, f.req, ":optional"));
        assert!(!is_match(&f.doc, f.plain, ":optional"));
    }

    #[test]
    fn valid_and_invalid_pseudo_classes() {
        let f = form_fixture();
        assert!(is_match(&f.doc, f.req, ":valid"));
        assert!(is_match(&f.doc, f.empty, ":invalid"));
        assert!(!is_match(&f.doc, f.empty, ":valid"));
        assert!(is_match(&f.doc, f.checkbox, ":invalid"));
        assert!(is_match(&f.doc, f.checked_box, ":valid"));
        assert!(is_match(&f.doc, f.plain, ":valid"));
        assert!(!is_match(&f.doc, f.plain, ":invalid"));

        let valid = ElementState {
            valid: Some(true),
            ..ElementState::default()
        };
        assert!(is_match_state(&f.doc, f.empty, ":valid", &valid));
        let invalid = ElementState {
            valid: Some(false),
            ..ElementState::default()
        };
        assert!(is_match_state(&f.doc, f.req, ":invalid", &invalid));
    }

    #[test]
    fn placeholder_shown_pseudo_class() {
        let f = form_fixture();
        assert!(is_match(&f.doc, f.empty, ":placeholder-shown"));
        assert!(is_match(&f.doc, f.textarea, "textarea:placeholder-shown"));
        assert!(!is_match(&f.doc, f.req, ":placeholder-shown"));
        assert!(!is_match(&f.doc, f.opt, ":placeholder-shown"));
        assert!(!is_match(&f.doc, f.plain, ":placeholder-shown"));
    }

    #[test]
    fn read_only_and_read_write_pseudo_classes() {
        let f = form_fixture();
        assert!(is_match(&f.doc, f.req, ":read-write"));
        assert!(is_match(&f.doc, f.opt, ":read-write"));
        assert!(is_match(&f.doc, f.textarea, ":read-write"));
        assert!(is_match(&f.doc, f.editable, ":read-write"));
        assert!(is_match(&f.doc, f.edit_child, ":read-write"));
        assert!(is_match(&f.doc, f.edit_off, ":read-only"));
        assert!(is_match(&f.doc, f.ro, ":read-only"));
        assert!(!is_match(&f.doc, f.ro, ":read-write"));
        assert!(is_match(&f.doc, f.plain, ":read-only"));
        assert!(!is_match(&f.doc, f.plain, ":read-write"));
    }

    #[test]
    fn dir_pseudo_class_inherits() {
        let f = form_fixture();
        assert!(is_match(&f.doc, f.rtl, ":dir(rtl)"));
        assert!(!is_match(&f.doc, f.rtl, ":dir(ltr)"));
        assert!(is_match(&f.doc, f.rtl_child, ":dir(rtl)"));
        assert!(is_match(&f.doc, f.req, ":dir(ltr)"));
        assert!(!is_match(&f.doc, f.req, ":dir(rtl)"));
    }

    #[test]
    fn defined_pseudo_class() {
        let f = form_fixture();
        assert!(is_match(&f.doc, f.req, ":defined"));
        assert!(is_match(&f.doc, f.plain, ":defined"));
        assert!(!is_match(&f.doc, f.custom, ":defined"));
        let defined = ElementState {
            defined: true,
            ..ElementState::default()
        };
        assert!(is_match_state(&f.doc, f.custom, ":defined", &defined));
    }

    #[test]
    fn state_only_pseudo_classes() {
        let f = form_fixture();
        for (text, state) in [
            (
                ":focus-visible",
                ElementState {
                    focus_visible: true,
                    ..ElementState::default()
                },
            ),
            (
                ":modal",
                ElementState {
                    modal: true,
                    ..ElementState::default()
                },
            ),
            (
                ":autofill",
                ElementState {
                    autofill: true,
                    ..ElementState::default()
                },
            ),
            (
                ":user-invalid",
                ElementState {
                    user_invalid: true,
                    ..ElementState::default()
                },
            ),
            (
                ":host",
                ElementState {
                    host: true,
                    ..ElementState::default()
                },
            ),
            (
                ":focus-within",
                ElementState {
                    focus_within: true,
                    ..ElementState::default()
                },
            ),
        ] {
            assert!(is_match_state(&f.doc, f.plain, text, &state), "{text}");
            assert!(!is_match(&f.doc, f.plain, text), "{text}");
        }

        let focused = ElementState {
            focus: true,
            ..ElementState::default()
        };
        assert!(is_match_state(&f.doc, f.plain, ":focus-within", &focused));
    }

    struct OfFixture {
        doc: Document,
        a: NodeId,
        b: NodeId,
        c: NodeId,
        d: NodeId,
        e: NodeId,
    }

    /// `<ul><li.item>a</li><li>b</li><li.item>c</li><div.item>d</div><li.item>e</li></ul>`
    fn of_fixture() -> OfFixture {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let ul = doc.create_element("ul");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, ul);

        let a = doc.create_element("li");
        doc.set_attr(a, "class", "item");
        let b = doc.create_element("li");
        let c = doc.create_element("li");
        doc.set_attr(c, "class", "item");
        let d = doc.create_element("div");
        doc.set_attr(d, "class", "item");
        let e = doc.create_element("li");
        doc.set_attr(e, "class", "item");
        for node in [a, b, c, d, e] {
            doc.append_child(ul, node);
        }
        OfFixture { doc, a, b, c, d, e }
    }

    #[test]
    fn nth_child_of_selector() {
        let f = of_fixture();
        assert!(is_match(&f.doc, f.a, ".item:nth-child(1 of .item)"));
        assert!(is_match(&f.doc, f.c, ".item:nth-child(2 of .item)"));
        assert!(!is_match(&f.doc, f.a, ".item:nth-child(2 of .item)"));
        assert!(is_match(&f.doc, f.d, ".item:nth-child(3 of .item)"));
        // A node that does not match S cannot occupy a position.
        assert!(!is_match(&f.doc, f.b, ":nth-child(2 of .item)"));
        assert!(is_match(&f.doc, f.c, ":nth-child(2n of .item)"));
        assert!(is_match(&f.doc, f.e, ":nth-child(2n of .item)"));
        assert!(!is_match(&f.doc, f.a, ":nth-child(2n of .item)"));
        assert!(!is_match(&f.doc, f.a, ":nth-child(5 of .item)"));
        // Selector lists and type selectors in the of clause.
        assert!(is_match(
            &f.doc,
            f.b,
            ":nth-child(2 of .item, li:not(.item))"
        ));
        assert!(is_match(&f.doc, f.a, ":nth-child(1 of li)"));
        assert!(is_match(&f.doc, f.c, ":nth-child(3 of li)"));
        // Dynamic state is threaded into the of selectors.
        let hover = ElementState {
            hover: true,
            ..ElementState::default()
        };
        assert!(is_match_state(
            &f.doc,
            f.b,
            ":nth-child(2 of li:hover)",
            &hover
        ));
        assert!(is_match_state(
            &f.doc,
            f.c,
            ":nth-child(3 of li:hover)",
            &hover
        ));
    }

    #[test]
    fn nth_last_child_of_selector() {
        let f = of_fixture();
        assert!(is_match(&f.doc, f.e, ":nth-last-child(1 of .item)"));
        assert!(is_match(&f.doc, f.d, ":nth-last-child(2 of .item)"));
        assert!(is_match(&f.doc, f.a, ":nth-last-child(4 of .item)"));
        assert!(!is_match(&f.doc, f.a, ":nth-last-child(1 of .item)"));
        assert!(is_match(&f.doc, f.b, ":nth-last-child(3 of li)"));
        assert!(!is_match(&f.doc, f.d, ":nth-last-child(1 of li)"));
    }

    struct ShadowFixture {
        doc: Document,
        body: NodeId,
        host: NodeId,
        light: NodeId,
        titled: NodeId,
        unassigned: NodeId,
        shadow_para: NodeId,
    }

    fn shadow_fixture() -> ShadowFixture {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);

        let host = doc.create_element("x-card");
        doc.set_attr(host, "class", "card");
        doc.append_child(body, host);
        let root = doc
            .attach_shadow(host, browser_dom::ShadowRootMode::Open)
            .unwrap();

        let default_slot = doc.create_element("slot");
        let named_slot = doc.create_element("slot");
        doc.set_attr(named_slot, "name", "title");
        doc.append_child(root, default_slot);
        doc.append_child(root, named_slot);
        let shadow_para = doc.create_element("p");
        doc.set_attr(shadow_para, "class", "inside");
        doc.append_child(root, shadow_para);

        let light = doc.create_element("span");
        doc.set_attr(light, "class", "item");
        let titled = doc.create_element("h1");
        doc.set_attr(titled, "slot", "title");
        doc.set_attr(titled, "class", "heading");
        let unassigned = doc.create_element("em");
        doc.set_attr(unassigned, "slot", "other");
        for node in [light, titled, unassigned] {
            doc.append_child(host, node);
        }

        ShadowFixture {
            doc,
            body,
            host,
            light,
            titled,
            unassigned,
            shadow_para,
        }
    }

    #[test]
    fn host_pseudo_class() {
        let f = shadow_fixture();
        assert!(is_match(&f.doc, f.host, ":host"));
        assert!(is_match(&f.doc, f.host, "x-card:host"));
        assert!(!is_match(&f.doc, f.light, ":host"));
        assert!(!is_match(&f.doc, f.shadow_para, ":host"));

        // The style engine can also mark any element as a host explicitly.
        let state = ElementState {
            host: true,
            ..ElementState::default()
        };
        assert!(is_match_state(&f.doc, f.light, ":host", &state));
    }

    #[test]
    fn host_context_pseudo_class() {
        let mut f = shadow_fixture();
        assert!(is_match(&f.doc, f.host, ":host-context(.card)"));
        assert!(is_match(&f.doc, f.host, ":host-context(body)"));
        assert!(!is_match(&f.doc, f.host, ":host-context(.theme)"));
        assert!(!is_match(&f.doc, f.light, ":host-context(.card)"));

        f.doc.set_attr(f.body, "class", "theme");
        assert!(is_match(&f.doc, f.host, ":host-context(.theme)"));
        assert!(is_match(
            &f.doc,
            f.host,
            ":host-context(body.theme, .other)"
        ));
        assert!(!is_match(&f.doc, f.host, ":host-context(html.theme)"));
    }

    #[test]
    fn slotted_pseudo_element() {
        let f = shadow_fixture();
        let selector = parse_selector("::slotted(.item)").unwrap();
        // Ordinary matching never matches a pseudo-element.
        assert!(!matches(&f.doc, f.light, &selector));
        assert!(matches_slotted(&f.doc, f.light, &selector));
        assert!(!matches_slotted(&f.doc, f.titled, &selector));
        assert!(!matches_slotted(&f.doc, f.unassigned, &selector));
        assert!(!matches_slotted(&f.doc, f.host, &selector));

        let named = parse_selector("slot[name=title]::slotted(h1.heading)").unwrap();
        assert!(matches_slotted(&f.doc, f.titled, &named));
        assert!(!matches_slotted(&f.doc, f.light, &named));

        let any = parse_selector("::slotted(*)").unwrap();
        assert!(matches_slotted(&f.doc, f.light, &any));
        assert!(matches_slotted(&f.doc, f.titled, &any));
        assert!(!matches_slotted(&f.doc, f.unassigned, &any));

        // A non-slotted pseudo-element selector is rejected by matches_slotted.
        let before = parse_selector("p::before").unwrap();
        assert!(!matches_slotted(&f.doc, f.light, &before));
    }
}
