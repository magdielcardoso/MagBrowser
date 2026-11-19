//! CSS `::before` / `::after` pseudo-element support.
//!
//! A pseudo-element is not a real DOM node, so it never matches through
//! [`matches`](crate::matches). Instead, a selector such as `p.icon::before`
//! records the pseudo-element name in [`Selector::pseudo_element`](crate::Selector::pseudo_element)
//! and matches its *host* element through
//! [`matches_pseudo_element_host`](crate::matches_pseudo_element_host).
//!
//! This module builds on those two primitives and exposes a small, clean API
//! that collects the declarations of every `::before` / `::after` rule that
//! applies to an element. It deliberately stops short of the cascade: the
//! declarations are returned in source order so the style engine can run them
//! through its normal cascade and interpret them into computed values.

use browser_css::{Declaration, Rule, Stylesheet, Value};
use browser_dom::{Document, NodeId};

use crate::{ElementState, matches_pseudo_element_host_with_state, parse_selector};

/// The declarations collected for one pseudo-element of a host element.
///
/// Declarations are in stylesheet source order; when the same property is
/// declared more than once, the last one wins the cascade. Use
/// [`PseudoElement::declaration`] to get that winning declaration directly.
#[derive(Debug, Clone, PartialEq)]
pub struct PseudoElement {
    /// Canonical pseudo-element name: `before` or `after`.
    pub name: String,
    /// Declarations from every matching rule, in source order.
    pub declarations: Vec<Declaration>,
}

impl PseudoElement {
    /// The winning declaration for `property`, if one was declared.
    ///
    /// Property names are compared ASCII case-insensitively. Because
    /// declarations are kept in source order, a later declaration shadows an
    /// earlier one for the same property.
    #[must_use]
    pub fn declaration(&self, property: &str) -> Option<&Declaration> {
        self.declarations
            .iter()
            .rev()
            .find(|declaration| declaration.name.eq_ignore_ascii_case(property))
    }

    /// The winning `content` value, if a `content` declaration was present.
    ///
    /// This is the value the integrator needs to decide whether the
    /// pseudo-element generates a box (`content: none` and `content: normal`
    /// suppress it; a string or `url()` generates content).
    #[must_use]
    pub fn content(&self) -> Option<&Value> {
        self.declaration("content")
            .map(|declaration| &declaration.value)
    }

    /// The string of a `content: "…"` declaration, if that is the winning
    /// content value.
    #[must_use]
    pub fn content_text(&self) -> Option<&str> {
        let components = &self.content()?.components;
        if components.len() == 1
            && let browser_css::ComponentValue::String(text) = &components[0]
        {
            return Some(text);
        }
        None
    }

    /// True when the winning `content` declaration explicitly suppresses the
    /// pseudo-element (`content: none`).
    #[must_use]
    pub fn is_suppressed(&self) -> bool {
        self.content().and_then(Value::ident) == Some("none")
    }
}

/// Collects the `::before` and `::after` pseudo-elements that apply to `node`
/// in `stylesheet`, using idle element state.
///
/// Only top-level `Rule::Style` rules are considered; `@media` blocks are
/// skipped because their conditions cannot be evaluated here. Call
/// [`pseudo_elements_from_rules`] with the rules you have already selected
/// (for example after media-query matching) to include them.
///
/// Legacy single-colon forms (`:before`, `:after`) work as well.
///
/// ```
/// use browser_css::{Origin, Stylesheet};
/// use browser_dom::Document;
/// use browser_selectors::pseudo_elements;
///
/// let mut doc = Document::new();
/// let html = doc.create_element("html");
/// let p = doc.create_element("p");
/// doc.set_attr(p, "class", "icon");
/// doc.append_child(doc.root(), html);
/// doc.append_child(html, p);
///
/// let sheet = Stylesheet::parse("p.icon::before { content: \"★\"; }", Origin::Author);
/// let elements = pseudo_elements(&doc, p, &sheet);
/// assert_eq!(elements[0].name, "before");
/// assert_eq!(elements[0].content_text(), Some("★"));
/// ```
#[must_use]
pub fn pseudo_elements(
    doc: &Document,
    node: NodeId,
    stylesheet: &Stylesheet,
) -> Vec<PseudoElement> {
    pseudo_elements_with_state(doc, node, stylesheet, &ElementState::default())
}

/// Like [`pseudo_elements`], but honours interactive pseudo-class state while
/// matching host selectors (for example `button:hover::after`).
#[must_use]
pub fn pseudo_elements_with_state(
    doc: &Document,
    node: NodeId,
    stylesheet: &Stylesheet,
    state: &ElementState,
) -> Vec<PseudoElement> {
    collect(
        doc,
        node,
        stylesheet.rules.iter(),
        state,
        false, // top-level rules only
    )
}

/// Collects pseudo-elements from an arbitrary set of rules.
///
/// Unlike [`pseudo_elements`], this recurses into `@media` blocks and treats
/// their nested style rules as active. Pass only the rules that apply in your
/// current media context to avoid false positives.
#[must_use]
pub fn pseudo_elements_from_rules<'a>(
    doc: &Document,
    node: NodeId,
    rules: impl IntoIterator<Item = &'a Rule>,
    state: &ElementState,
) -> Vec<PseudoElement> {
    collect(doc, node, rules, state, true)
}

/// Collects a single named pseudo-element (`"before"` or `"after"`) that
/// applies to `node`. Leading colons are optional, so `":before"`,
/// `"::before"` and `"before"` are all accepted.
///
/// Returns `None` when no rule for that pseudo-element matches, or when the
/// name is not `before`/`after`.
#[must_use]
pub fn pseudo_element(
    doc: &Document,
    node: NodeId,
    stylesheet: &Stylesheet,
    name: &str,
) -> Option<PseudoElement> {
    pseudo_element_with_state(doc, node, stylesheet, name, &ElementState::default())
}

/// Like [`pseudo_element`], but honours interactive pseudo-class state.
#[must_use]
pub fn pseudo_element_with_state(
    doc: &Document,
    node: NodeId,
    stylesheet: &Stylesheet,
    name: &str,
    state: &ElementState,
) -> Option<PseudoElement> {
    let name = name.trim_start_matches(':').to_ascii_lowercase();
    if name != "before" && name != "after" {
        return None;
    }
    pseudo_elements_with_state(doc, node, stylesheet, state)
        .into_iter()
        .find(|element| element.name == name)
}

fn collect<'a>(
    doc: &Document,
    node: NodeId,
    rules: impl IntoIterator<Item = &'a Rule>,
    state: &ElementState,
    recurse_media: bool,
) -> Vec<PseudoElement> {
    let mut before = Vec::new();
    let mut after = Vec::new();

    collect_into(
        doc,
        node,
        rules,
        state,
        recurse_media,
        &mut before,
        &mut after,
    );

    let mut out = Vec::with_capacity(2);
    if !before.is_empty() {
        out.push(PseudoElement {
            name: "before".to_string(),
            declarations: before,
        });
    }
    if !after.is_empty() {
        out.push(PseudoElement {
            name: "after".to_string(),
            declarations: after,
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn collect_into<'a>(
    doc: &Document,
    node: NodeId,
    rules: impl IntoIterator<Item = &'a Rule>,
    state: &ElementState,
    recurse_media: bool,
    before: &mut Vec<Declaration>,
    after: &mut Vec<Declaration>,
) {
    for rule in rules {
        match rule {
            Rule::Style(style_rule) => {
                for selector_text in &style_rule.selectors {
                    let Ok(selector) = parse_selector(selector_text) else {
                        continue;
                    };
                    let Some(pseudo_element) = selector.pseudo_element.as_deref() else {
                        continue;
                    };
                    let target = match pseudo_element.to_ascii_lowercase().as_str() {
                        "before" => &mut *before,
                        "after" => &mut *after,
                        // Other pseudo-elements (`first-line`, `::part()`,
                        // `::selection`, ...) are out of scope here.
                        _ => continue,
                    };
                    if !matches_pseudo_element_host_with_state(doc, node, &selector, state) {
                        continue;
                    }
                    target.extend(style_rule.declarations.iter().cloned());
                }
            }
            Rule::Media(media_rule) if recurse_media => {
                collect_into(
                    doc,
                    node,
                    media_rule.rules.iter(),
                    state,
                    recurse_media,
                    before,
                    after,
                );
            }
            // Other at-rules (`@import`, `@keyframes`, `@supports`, `@layer`,
            // `@scope`, `@property`, ...) do not contribute pseudo-elements.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use browser_css::{ComponentValue, Origin, Stylesheet};
    use browser_dom::Document;

    use super::*;

    /// Builds `<html><body><p id="target" class="icon">Hi</p><div class="icon"></div></body></html>`.
    fn fixture() -> (Document, NodeId, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let p = doc.create_element("p");
        doc.set_attr(p, "id", "target");
        doc.set_attr(p, "class", "icon");
        doc.append_text(p, "Hi");
        let div = doc.create_element("div");
        doc.set_attr(div, "class", "icon");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, p);
        doc.append_child(body, div);
        (doc, p, div)
    }

    fn sheet(css: &str) -> Stylesheet {
        Stylesheet::parse(css, Origin::Author)
    }

    #[test]
    fn collects_before_and_after_declarations() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet(
            "p::before { content: \"+\"; color: red; } \
             p::after { content: \"!\"; font-weight: bold; }",
        );
        let elements = pseudo_elements(&doc, p, &stylesheet);
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].name, "before");
        assert_eq!(elements[1].name, "after");
        assert_eq!(elements[0].content_text(), Some("+"));
        assert_eq!(elements[1].content_text(), Some("!"));
        assert!(elements[0].declaration("color").is_some());
        assert!(elements[1].declaration("font-weight").is_some());
    }

    #[test]
    fn matches_host_selectors_and_ignores_other_elements() {
        let (doc, p, div) = fixture();
        let stylesheet = sheet(
            "#target::before { content: \"id\"; } \
             .icon::after { content: \"class\"; } \
             div::before { content: \"div\"; }",
        );
        let before = pseudo_element(&doc, p, &stylesheet, "::before").unwrap();
        assert_eq!(before.content_text(), Some("id"));
        let after = pseudo_element(&doc, p, &stylesheet, "after").unwrap();
        assert_eq!(after.content_text(), Some("class"));

        // The div matches `.icon::after` and `div::before`, but not `#target`.
        let before = pseudo_element(&doc, div, &stylesheet, "before").unwrap();
        assert_eq!(before.content_text(), Some("div"));
        let after = pseudo_element(&doc, div, &stylesheet, "after").unwrap();
        assert_eq!(after.content_text(), Some("class"));
    }

    #[test]
    fn legacy_single_colon_forms_work() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet(
            "p:before { content: \"legacy-before\"; } p:after { content: \"legacy-after\"; }",
        );
        let elements = pseudo_elements(&doc, p, &stylesheet);
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].content_text(), Some("legacy-before"));
        assert_eq!(elements[1].content_text(), Some("legacy-after"));
    }

    #[test]
    fn later_declarations_win() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet(
            "p::before { content: \"first\"; color: red; } \
             .icon::before { content: \"second\"; }",
        );
        let before = pseudo_element(&doc, p, &stylesheet, "before").unwrap();
        assert_eq!(before.content_text(), Some("second"));
        assert_eq!(before.declarations.len(), 3);
        assert_eq!(before.declaration("content").unwrap().value.ident(), None);
        assert!(before.declaration("color").is_some());
    }

    #[test]
    fn no_match_returns_empty_and_non_pseudo_rules_are_ignored() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet("p { color: red; } .icon { color: blue; }");
        assert!(pseudo_elements(&doc, p, &stylesheet).is_empty());
        assert!(pseudo_element(&doc, p, &stylesheet, "before").is_none());
        assert!(pseudo_element(&doc, p, &stylesheet, "first-line").is_none());
    }

    #[test]
    fn suppression_and_content_helpers() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet("p::before { content: none; }");
        let before = pseudo_element(&doc, p, &stylesheet, "before").unwrap();
        assert!(before.is_suppressed());
        assert_eq!(before.content().unwrap().ident(), Some("none"));
        assert_eq!(before.content_text(), None);

        let stylesheet = sheet("p::before { content: url(icon.svg); }");
        let before = pseudo_element(&doc, p, &stylesheet, "before").unwrap();
        assert!(!before.is_suppressed());
        assert!(matches!(
            before.content().unwrap().components.as_slice(),
            [ComponentValue::Url(url)] if url == "icon.svg"
        ));
    }

    #[test]
    fn interactive_state_is_honoured() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet("p:hover::after { content: \"hovered\"; }");
        assert!(pseudo_element(&doc, p, &stylesheet, "after").is_none());
        let hover = ElementState {
            hover: true,
            ..ElementState::default()
        };
        let after = pseudo_element_with_state(&doc, p, &stylesheet, "after", &hover).unwrap();
        assert_eq!(after.content_text(), Some("hovered"));
    }

    #[test]
    fn media_rules_are_opt_in() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet("@media (min-width: 100px) { p::before { content: \"media\"; } }");
        // Conservative default: @media blocks are skipped.
        assert!(pseudo_elements(&doc, p, &stylesheet).is_empty());
        // Caller-selected rules include the nested style rule.
        let rules: Vec<_> = stylesheet.rules.iter().collect();
        let elements = pseudo_elements_from_rules(&doc, p, rules, &ElementState::default());
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].content_text(), Some("media"));
    }

    #[test]
    fn selector_lists_and_combinators_work() {
        let (doc, p, _div) = fixture();
        let stylesheet = sheet(
            "div.icon::before, body > p#target::before { content: \"list\"; } \
             html body p.icon::after { content: \"chain\"; }",
        );
        let elements = pseudo_elements(&doc, p, &stylesheet);
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].content_text(), Some("list"));
        assert_eq!(elements[1].content_text(), Some("chain"));
    }
}
