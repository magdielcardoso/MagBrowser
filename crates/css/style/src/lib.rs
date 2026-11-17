//! Style engine: cascade, inheritance and computed styles for the DOM.
//!
//! The engine consumes [`browser_css`] stylesheets and [`browser_selectors`]
//! selectors and produces a [`StyledTree`] keyed by [`NodeId`]. Layout and paint
//! read computed values from that tree exclusively.
//!
//! ```
//! use browser_css::{Origin, Stylesheet};
//! use browser_dom::Document;
//! use browser_style::{StyleEngine, Viewport};
//!
//! let mut doc = Document::new();
//! let html = doc.create_element("html");
//! let body = doc.create_element("body");
//! doc.append_child(doc.root(), html);
//! doc.append_child(html, body);
//!
//! let mut engine = StyleEngine::new(Viewport { width: 800.0, height: 600.0 });
//! let sheet = Stylesheet::parse("body { background-color: #ff0000; }", Origin::Author);
//! engine.add_stylesheet(sheet);
//! let styled = engine.compute(&doc);
//! assert_eq!(styled.get(body).unwrap().background_color().r, 255);
//! ```

#![forbid(unsafe_code)]

pub mod cascade;
pub mod computed;
pub mod inheritance;
pub mod interpret;
pub mod media_queries;
pub mod properties;
pub mod specificity;
pub mod ua;
pub mod variables;

use std::collections::HashMap;

use browser_css::{ComponentValue, Declaration, Rule, Stylesheet, Value};
use browser_dom::{Document, NodeId};
use browser_selectors::{ElementState, Selector, parse_selector};

pub use browser_css::{Color, Origin};
pub use cascade::Candidate;
pub use computed::{
    AlignItems, BorderStyle, BoxSizing, ComputedStyle, ComputedValue, Dim, Display, FlexDirection,
    Float, JustifyContent, LineHeight, Overflow, Position, Side, TextAlign, TextDecoration,
    Visibility, WhiteSpace, border_width_property, margin_property, padding_property,
};
pub use media_queries::{MediaContext, media_matches};
pub use properties::Property;

/// Viewport size in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Viewport width.
    pub width: f32,
    /// Viewport height.
    pub height: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1024.0,
            height: 768.0,
        }
    }
}

/// Computed styles for a document.
#[derive(Debug, Clone, Default)]
pub struct StyledTree {
    styles: HashMap<NodeId, ComputedStyle>,
    customs: HashMap<NodeId, HashMap<String, Value>>,
    /// Font size of the root element, used for `rem`.
    pub root_font_size: f32,
}

impl StyledTree {
    /// Creates an empty styled tree.
    pub fn new() -> Self {
        Self {
            styles: HashMap::new(),
            customs: HashMap::new(),
            root_font_size: 16.0,
        }
    }

    /// The computed style of a node.
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(&id)
    }

    /// The computed style of a node, falling back to the initial style.
    pub fn style(&self, id: NodeId) -> ComputedStyle {
        self.styles
            .get(&id)
            .cloned()
            .unwrap_or_else(ComputedStyle::initial)
    }

    /// Custom properties declared on a node.
    pub fn customs(&self, id: NodeId) -> Option<&HashMap<String, Value>> {
        self.customs.get(&id)
    }

    /// Number of styled nodes.
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    /// True when nothing was styled.
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    /// Every styled element in insertion (document) order.
    pub fn entries(&self) -> impl Iterator<Item = (&NodeId, &ComputedStyle)> {
        self.styles.iter()
    }
}

#[derive(Debug, Clone)]
struct CustomDeclaration {
    value: Value,
    important: bool,
    origin: Origin,
    specificity: browser_selectors::Specificity,
    inline: bool,
    order: usize,
}

impl CustomDeclaration {
    fn weight(&self) -> (u8, u8, u8, u32, u32, u32, usize) {
        Candidate {
            property: Property::Color,
            value: self.value.clone(),
            important: self.important,
            origin: self.origin,
            specificity: self.specificity,
            inline: self.inline,
            order: self.order,
        }
        .weight()
    }
}

/// The style engine.
#[derive(Debug)]
pub struct StyleEngine {
    viewport: Viewport,
    user_agent: Stylesheet,
    user: Vec<Stylesheet>,
    author: Vec<Stylesheet>,
    media_type: String,
    prefers_color_scheme: String,
}

impl StyleEngine {
    /// Creates an engine with the built-in user-agent stylesheet.
    pub fn new(viewport: Viewport) -> Self {
        Self {
            viewport,
            user_agent: Stylesheet::parse(ua::DEFAULT_STYLESHEET, Origin::UserAgent),
            user: Vec::new(),
            author: Vec::new(),
            media_type: "screen".to_string(),
            prefers_color_scheme: "light".to_string(),
        }
    }

    /// Updates the viewport used for media queries and viewport units.
    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = viewport;
    }

    /// The current viewport.
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// Sets the preferred colour scheme (`light`/`dark`).
    pub fn set_prefers_color_scheme(&mut self, scheme: &str) {
        self.prefers_color_scheme = scheme.to_string();
    }

    /// Registers a parsed stylesheet.
    pub fn add_stylesheet(&mut self, sheet: Stylesheet) {
        match sheet.origin {
            Origin::UserAgent => self.user_agent.rules.extend(sheet.rules),
            Origin::User => self.user.push(sheet),
            Origin::Author => self.author.push(sheet),
        }
    }

    /// Parses and registers CSS text.
    pub fn add_css(&mut self, css: &str, origin: Origin) {
        self.add_stylesheet(Stylesheet::parse(css, origin));
    }

    /// Removes every author stylesheet (used when navigating).
    pub fn clear_author_stylesheets(&mut self) {
        self.author.clear();
    }

    /// Removes every non-UA stylesheet.
    pub fn clear_all_stylesheets(&mut self) {
        self.author.clear();
        self.user.clear();
        self.user_agent = Stylesheet::parse(ua::DEFAULT_STYLESHEET, Origin::UserAgent);
    }

    /// Computes styles for every element in `doc`.
    pub fn compute(&self, doc: &Document) -> StyledTree {
        self.compute_with_state(doc, &HashMap::new())
    }

    /// Computes styles, honouring dynamic pseudo-class state (hover, focus...).
    pub fn compute_with_state(
        &self,
        doc: &Document,
        states: &HashMap<NodeId, ElementState>,
    ) -> StyledTree {
        let mut tree = StyledTree::new();
        let media = MediaContext {
            width: self.viewport.width,
            height: self.viewport.height,
            media_type: self.media_type.clone(),
            prefers_color_scheme: self.prefers_color_scheme.clone(),
        };
        let default_state = ElementState::default();
        let mut order = 0_usize;

        for node in doc.descendants(doc.root()) {
            if !doc.node(node).is_element() {
                continue;
            }
            let parent_id = doc.parent_element(node);
            let parent_style = parent_id
                .and_then(|p| tree.styles.get(&p).cloned())
                .unwrap_or_else(ComputedStyle::initial);
            let parent_customs = parent_id
                .and_then(|p| tree.customs.get(&p).cloned())
                .unwrap_or_default();
            let state = states.get(&node).unwrap_or(&default_state);

            let mut candidates: Vec<Candidate> = Vec::new();
            let mut custom_decls: Vec<(String, CustomDeclaration)> = Vec::new();

            for (origin, sheets) in [
                (Origin::UserAgent, std::slice::from_ref(&self.user_agent)),
                (Origin::User, self.user.as_slice()),
                (Origin::Author, self.author.as_slice()),
            ] {
                for sheet in sheets {
                    self.collect_from_rules(
                        doc,
                        node,
                        &sheet.rules,
                        origin,
                        &media,
                        state,
                        &mut candidates,
                        &mut custom_decls,
                        &mut order,
                    );
                }
            }

            // Inline `style` attribute.
            if let Some(css) = doc.attr(node, "style") {
                let specificity = specificity::inline_specificity();
                for declaration in browser_css::parse_inline_style(css) {
                    order += 1;
                    self.collect_declaration(
                        declaration,
                        Origin::Author,
                        specificity,
                        true,
                        &mut candidates,
                        &mut custom_decls,
                        order,
                    );
                }
            }

            // Custom properties: inherit, then override with winners.
            let mut customs = parent_customs;
            let mut winners: HashMap<String, CustomDeclaration> = HashMap::new();
            for (name, declaration) in custom_decls {
                match winners.get(&name) {
                    Some(current) if declaration.weight() < current.weight() => {}
                    _ => {
                        winners.insert(name, declaration);
                    }
                }
            }
            for (name, declaration) in winners {
                if let Some(value) = variables::substitute(&declaration.value, &customs) {
                    customs.insert(name, value);
                }
            }

            let style = self.compute_element_style(
                node,
                doc,
                &parent_style,
                &customs,
                candidates,
                &mut tree,
            );

            tree.styles.insert(node, style);
            tree.customs.insert(node, customs);

            if doc.is_tag(node, "html") {
                tree.root_font_size = tree
                    .styles
                    .get(&node)
                    .map_or(16.0, ComputedStyle::font_size);
            }
        }
        tree
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_from_rules(
        &self,
        doc: &Document,
        node: NodeId,
        rules: &[Rule],
        origin: Origin,
        media: &MediaContext,
        state: &ElementState,
        candidates: &mut Vec<Candidate>,
        customs: &mut Vec<(String, CustomDeclaration)>,
        order: &mut usize,
    ) {
        for rule in rules {
            match rule {
                Rule::Style(style_rule) => {
                    for selector_text in &style_rule.selectors {
                        let Ok(selector) = parse_selector(selector_text) else {
                            continue;
                        };
                        if !selector_matches(doc, node, &selector, state) {
                            continue;
                        }
                        for declaration in &style_rule.declarations {
                            *order += 1;
                            self.collect_declaration(
                                declaration.clone(),
                                origin,
                                selector.specificity,
                                false,
                                candidates,
                                customs,
                                *order,
                            );
                        }
                    }
                }
                Rule::Media(media_rule) => {
                    if media_matches(&media_rule.query, media) {
                        self.collect_from_rules(
                            doc,
                            node,
                            &media_rule.rules,
                            origin,
                            media,
                            state,
                            candidates,
                            customs,
                            order,
                        );
                    }
                }
                Rule::Import(import) => {
                    if let Some(media_list) = &import.media
                        && !media_matches(media_list, media)
                    {
                        continue;
                    }
                    // @import fetching is handled by the browser core, which
                    // appends the imported rules as separate stylesheets.
                }
                Rule::Keyframes(_) | Rule::FontFace(_) => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_declaration(
        &self,
        declaration: Declaration,
        origin: Origin,
        specificity: browser_selectors::Specificity,
        inline: bool,
        candidates: &mut Vec<Candidate>,
        customs: &mut Vec<(String, CustomDeclaration)>,
        order: usize,
    ) {
        if variables::is_custom_property(&declaration.name) {
            customs.push((
                declaration.name.clone(),
                CustomDeclaration {
                    value: declaration.value,
                    important: declaration.important,
                    origin,
                    specificity,
                    inline,
                    order,
                },
            ));
            return;
        }
        for (property, value) in cascade::expand(&declaration) {
            candidates.push(Candidate {
                property,
                value,
                important: declaration.important,
                origin,
                specificity,
                inline,
                order,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_element_style(
        &self,
        _node: NodeId,
        _doc: &Document,
        parent: &ComputedStyle,
        customs: &HashMap<String, Value>,
        candidates: Vec<Candidate>,
        tree: &mut StyledTree,
    ) -> ComputedStyle {
        let winners = cascade::cascade(candidates);
        let mut style = ComputedStyle::default();
        let parent_font_size = parent.font_size();

        let base_ctx = interpret::InterpretContext {
            font_size: parent_font_size,
            parent_font_size,
            root_font_size: tree.root_font_size,
            viewport_width: self.viewport.width,
            viewport_height: self.viewport.height,
        };

        // Font size is resolved first because every other `em` value depends on it.
        let font_size_winner = winners
            .iter()
            .find(|c| c.property == Property::FontSize)
            .cloned();
        let font_size = match font_size_winner {
            Some(candidate) => {
                let value = variables::substitute(&candidate.value, customs)
                    .unwrap_or_else(|| candidate.value.clone());
                match inheritance::WideKeyword::detect(&value) {
                    Some(keyword) => {
                        inheritance::apply_wide(keyword, Property::FontSize, Some(parent))
                            .map_or(parent_font_size, |v| v.as_px())
                    }
                    None => interpret::font_size(&value, &base_ctx).unwrap_or(parent_font_size),
                }
            }
            None => parent_font_size,
        };
        style.set(Property::FontSize, ComputedValue::Px(font_size));

        let ctx = interpret::InterpretContext {
            font_size,
            ..base_ctx
        };

        for winner in &winners {
            if winner.property == Property::FontSize {
                continue;
            }
            let value = match variables::substitute(&winner.value, customs) {
                Some(value) => value,
                None => continue,
            };
            let computed = match inheritance::WideKeyword::detect(&value) {
                Some(keyword) => inheritance::apply_wide(keyword, winner.property, Some(parent)),
                None => interpret::interpret(winner.property, &value, &ctx),
            };
            if let Some(computed) = computed {
                style.set(winner.property, computed);
            }
        }

        // Inherit or initialise everything that was not declared.
        for property in Property::ALL {
            if style.get(*property).is_some() {
                continue;
            }
            if property.is_inherited() {
                style.inherit_property(*property, parent);
            } else if let Some(initial) = properties::initial_value(*property) {
                style.set(*property, initial);
            }
        }
        style
    }
}

fn selector_matches(
    doc: &Document,
    node: NodeId,
    selector: &Selector,
    state: &ElementState,
) -> bool {
    browser_selectors::matches_with_state(doc, node, selector, state)
}

/// Convenience: the computed style for an element with no stylesheets applied
/// beyond the user-agent sheet.
pub fn default_style_for(tag: &str) -> ComputedStyle {
    let engine = StyleEngine::new(Viewport::default());
    let mut doc = Document::new();
    let html = doc.create_element("html");
    doc.append_child(doc.root(), html);
    let element = doc.create_element(tag);
    doc.append_child(html, element);
    let _ = engine.compute(&doc);
    engine
        .compute(&doc)
        .get(element)
        .cloned()
        .unwrap_or_else(ComputedStyle::initial)
}

/// True when a value is a `var()` reference that could not be resolved.
pub fn has_unresolved_var(value: &Value) -> bool {
    value.components.iter().any(|component| {
        matches!(component, ComponentValue::Function(name, _) if name.eq_ignore_ascii_case("var"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_css::Color;

    fn document() -> (Document, NodeId, NodeId, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let p = doc.create_element("p");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, p);
        doc.set_attr(p, "id", "target");
        (doc, html, body, p)
    }

    fn engine_with(css: &str) -> StyleEngine {
        let mut engine = StyleEngine::new(Viewport {
            width: 1000.0,
            height: 800.0,
        });
        engine.add_css(css, Origin::Author);
        engine
    }

    #[test]
    fn applies_ua_defaults() {
        let (doc, _html, body, _p) = document();
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(body).unwrap().display(), Display::Block);
        assert_eq!(styled.get(body).unwrap().margin(Side::Top), Dim::Px(8.0));
    }

    #[test]
    fn class_rule_wins_over_ua() {
        let (mut doc, _html, body, p) = document();
        doc.set_attr(p, "class", "hi");
        let engine = engine_with(".hi { color: #00ff00; font-size: 20px; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(p).unwrap().color(), Color::rgb(0, 255, 0));
        assert_eq!(styled.get(p).unwrap().font_size(), 20.0);
        assert_eq!(styled.get(body).unwrap().color(), Color::rgb(0, 0, 0));
    }

    #[test]
    fn inheritance_reaches_children() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("body { color: #123456; font-size: 30px; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(p).unwrap().color(), Color::rgb(0x12, 0x34, 0x56));
        assert_eq!(styled.get(p).unwrap().font_size(), 30.0);
    }

    #[test]
    fn inline_style_beats_stylesheet() {
        let (mut doc, _html, _body, p) = document();
        doc.set_attr(p, "style", "color: blue; width: 50%");
        let engine = engine_with("#target { color: red; width: 100px; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(p).unwrap().color(), Color::rgb(0, 0, 255));
        assert_eq!(styled.get(p).unwrap().width(), Dim::Percent(50.0));
    }

    #[test]
    fn important_declaration_wins() {
        let (mut doc, _html, _body, p) = document();
        doc.set_attr(p, "id", "target");
        let engine = engine_with("#target { color: red !important; } p { color: blue; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(p).unwrap().color(), Color::rgb(255, 0, 0));
    }

    #[test]
    fn media_queries_gate_rules() {
        let (doc, _html, body, _p) = document();
        let engine = engine_with(
            "@media (min-width: 2000px) { body { background-color: red; } }\n\
             @media (max-width: 2000px) { body { background-color: blue; } }",
        );
        let styled = engine.compute(&doc);
        assert_eq!(
            styled.get(body).unwrap().background_color(),
            Color::rgb(0, 0, 255)
        );
    }

    #[test]
    fn css_variables_resolve() {
        let (doc, _html, body, _p) = document();
        let engine =
            engine_with(":root { --brand: #112233; }\nbody { background-color: var(--brand); }");
        let styled = engine.compute(&doc);
        assert_eq!(
            styled.get(body).unwrap().background_color(),
            Color::rgb(0x11, 0x22, 0x33)
        );
    }

    #[test]
    fn display_none_from_stylesheet() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("#target { display: none; }");
        let styled = engine.compute(&doc);
        assert!(styled.get(p).unwrap().is_display_none());
    }

    #[test]
    fn inherit_keyword_copies_parent() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("body { width: 123px; } #target { width: inherit; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(p).unwrap().width(), Dim::Px(123.0));
    }

    #[test]
    fn em_units_use_own_font_size() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("#target { font-size: 20px; margin-top: 2em; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(p).unwrap().margin(Side::Top), Dim::Px(40.0));
    }

    #[test]
    fn default_style_helper_uses_ua_sheet() {
        let style = default_style_for("h1");
        assert_eq!(style.display(), Display::Block);
        assert_eq!(style.font_size(), 32.0);
        assert_eq!(style.font_weight(), 700);
    }
}
