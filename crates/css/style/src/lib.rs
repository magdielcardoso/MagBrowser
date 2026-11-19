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
pub mod conditions;
pub mod inheritance;
pub mod interpret;
pub mod media_queries;
pub mod properties;
pub mod specificity;
pub mod ua;
pub mod variables;

mod nesting;
mod selector_index;

use std::collections::{HashMap, HashSet};

use browser_css::{
    ComponentValue, Declaration, DiagnosticKind, ParseDiagnostic, Rule, Stylesheet, Value,
};
use browser_dom::{Document, NodeId};
use browser_selectors::{ElementState, PseudoElement, Selector, Specificity};

use cascade::{NO_SCOPE, UNLAYERED, pack_order};
use selector_index::{NodeKeys, RuleKind, SelectorIndex};
use variables::{PropertyRegistry, substitute_with_registry};

pub use browser_css::{Color, Origin};
pub use cascade::Candidate;
pub use computed::{
    AlignItems, AspectRatio, BorderRadius, BorderStyle, BoxSizing, ComputedStyle, ComputedValue,
    CornerRadius, Dim, Display, FilterFunction, FilterList, FlexDirection, FlexWrap, Float,
    JustifyContent, LineHeight, ObjectFit, Overflow, Position, Shadow, ShadowList, Side, TextAlign,
    TextDecoration, TransformFunction, TransformList, Visibility, WhiteSpace,
    border_width_property, margin_property, padding_property,
};
pub use conditions::{ContainerContext, ContainerSize};
pub use media_queries::{MediaContext, media_matches};
pub use properties::Property;

/// Maximum number of `@import` hops followed from one stylesheet.
///
/// Each extra hop is recorded as a [`DiagnosticKind::ImportDepth`] diagnostic
/// instead of being followed, so a cycle such as `a.css -> b.css -> a.css`
/// cannot hang the style engine.
pub const MAX_IMPORT_DEPTH: usize = 4;

/// An `@import` that a caller should fetch and register.
///
/// Returned by [`StyleEngine::add_css_collect_imports`]. The caller passes the
/// fetched CSS back with the same `depth` via another call to that method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    /// URL to fetch.
    pub url: String,
    /// Optional media query restricting the import.
    pub media: Option<String>,
    /// Number of `@import` hops taken to reach this request (starts at 1).
    pub depth: usize,
}

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

/// The generated content of a `::before` or `::after` pseudo-element.
///
/// Pseudo-elements are not real DOM nodes, so layout reads them from the
/// [`StyledTree`] alongside their host's computed style.
#[derive(Debug, Clone)]
pub struct PseudoStyle {
    /// The unescaped text of the winning `content: "…"` declaration.
    pub content: String,
    /// Computed style for the generated text box (host style plus the
    /// pseudo-element's own declarations, such as `color` and `font`).
    pub style: ComputedStyle,
}

#[derive(Debug, Clone, Default)]
struct PseudoStyles {
    before: Option<PseudoStyle>,
    after: Option<PseudoStyle>,
}

/// Sentinel node used for diagnostics that are not attached to one element,
/// such as an entire at-rule that the engine ignores.
const AT_RULE_NODE: NodeId = NodeId(usize::MAX);

/// A CSS declaration (or at-rule) that this engine could not fully interpret.
///
/// These are kept as *visible diagnostics*: a declaration that fails to parse
/// is skipped on its own, and a declaration that parses but contains syntax we
/// cannot act on (for example `min()` inside a grid track list) is kept at its
/// raw value. In both cases the occurrence is recorded here so a caller (for
/// example a DevTools panel) can surface it, instead of silently degrading the
/// layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedDeclaration {
    /// Element the declaration applied to. At-rule diagnostics use the
    /// sentinel `NodeId(usize::MAX)` because they are not tied to one element.
    pub node: NodeId,
    /// Canonical property name (`grid-template-columns`) or at-rule name
    /// (`@container`).
    pub property: String,
    /// Authored value text, preserved verbatim where possible.
    pub value: String,
    /// True when the whole declaration was skipped because it could not be
    /// interpreted. False when the raw value was kept on a best-effort basis.
    pub skipped: bool,
}

impl std::fmt::Display for UnsupportedDeclaration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.node == AT_RULE_NODE {
            write!(
                f,
                "[CSS] Unsupported at-rule '{}': {}",
                self.property, self.value
            )
        } else if self.skipped {
            write!(
                f,
                "[CSS] Skipped declaration '{}': {} (node {:?})",
                self.property, self.value, self.node
            )
        } else {
            write!(
                f,
                "[CSS] Unsupported value for '{}': {} (node {:?})",
                self.property, self.value, self.node
            )
        }
    }
}

/// Computed styles for a document.
#[derive(Debug, Clone, Default)]
pub struct StyledTree {
    styles: HashMap<NodeId, ComputedStyle>,
    customs: HashMap<NodeId, HashMap<String, Value>>,
    pseudos: HashMap<NodeId, PseudoStyles>,
    /// Unsupported declarations found while computing this tree, in source
    /// order. Exposed as visible diagnostics (see [`Self::unsupported_declarations`]).
    unsupported: Vec<UnsupportedDeclaration>,
    /// `@property` registrations used for typed custom-property fallbacks.
    property_registry: PropertyRegistry,
    /// Font size of the root element, used for `rem`.
    pub root_font_size: f32,
}

impl StyledTree {
    /// Creates an empty styled tree.
    pub fn new() -> Self {
        Self {
            styles: HashMap::new(),
            customs: HashMap::new(),
            pseudos: HashMap::new(),
            unsupported: Vec::new(),
            property_registry: PropertyRegistry::new(),
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

    /// The generated `::before`/`::after` content for a node, if any.
    ///
    /// Leading colons are optional, so `"before"`, `"::before"` and
    /// `":before"` are all accepted. Only named pseudo-elements with a string
    /// `content` value produce an entry.
    pub fn pseudo(&self, id: NodeId, name: &str) -> Option<&PseudoStyle> {
        let styles = self.pseudos.get(&id)?;
        match name.trim_start_matches(':').to_ascii_lowercase().as_str() {
            "before" => styles.before.as_ref(),
            "after" => styles.after.as_ref(),
            _ => None,
        }
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

    /// Number of declarations (or at-rules) this engine could not fully
    /// interpret while computing the tree.
    pub fn unsupported_count(&self) -> usize {
        self.unsupported.len()
    }

    /// Unsupported declarations as `(property, raw value)` pairs, in source
    /// order.
    ///
    /// This is the structured form of the diagnostic; use
    /// [`Self::unsupported_console_messages`] for strings ready to hand to a
    /// DevTools console.
    pub fn unsupported_declarations(&self) -> Vec<(String, String)> {
        self.unsupported
            .iter()
            .map(|entry| (entry.property.clone(), entry.value.clone()))
            .collect()
    }

    /// Full diagnostic entries, including the element and whether the
    /// declaration was skipped or kept at its raw value.
    pub fn unsupported_entries(&self) -> &[UnsupportedDeclaration] {
        &self.unsupported
    }

    /// DevTools-console-ready messages, one per unsupported declaration, in
    /// source order. Each message is a plain string a host can pass straight
    /// to `console.warn`/`console.log`.
    pub fn unsupported_console_messages(&self) -> Vec<String> {
        self.unsupported.iter().map(ToString::to_string).collect()
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
    fn weight(&self) -> (u8, u8, u8, u32, u32, u32, u32, u32, usize) {
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

/// A matched declaration whose value has not yet been expanded into longhands.
///
/// Expansion is deferred until custom properties have been resolved so that
/// `var()` references inside shorthands can be substituted *before* the
/// shorthand is split into its longhands. Without this, `border: 1px solid
/// var(--line)` and `background: var(--white)` lose their colour: the raw
/// `var()` token is an opaque function to the shorthand expander, so it is
/// never recognised as a colour.
#[derive(Debug, Clone)]
struct PendingDeclaration {
    declaration: Declaration,
    origin: Origin,
    specificity: Specificity,
    inline: bool,
    order: usize,
}

/// Expands collected declarations into longhand cascade candidates.
///
/// Values containing `var()` are resolved against `customs` first, so
/// shorthands see the final colours/lengths rather than an opaque function
/// token. A declaration whose variables cannot be resolved is skipped and
/// surfaced as a visible diagnostic, matching the per-longhand substitution in
/// [`StyleEngine::compute_element_style`]. Non-`var()` values are passed
/// through untouched, preserving the existing behaviour.
fn expand_pending(
    pending: &mut Vec<PendingDeclaration>,
    customs: &HashMap<String, Value>,
    node: NodeId,
    tree: &mut StyledTree,
    candidates: &mut Vec<Candidate>,
) {
    let registry = tree.property_registry.clone();
    candidates.clear();
    for entry in pending.drain(..) {
        let value = if has_unresolved_var(&entry.declaration.value) {
            match substitute_with_registry(&entry.declaration.value, customs, &registry) {
                Some(value) => value,
                None => {
                    tree.unsupported.push(UnsupportedDeclaration {
                        node,
                        property: entry.declaration.name.clone(),
                        value: value_text(&entry.declaration.value),
                        skipped: true,
                    });
                    continue;
                }
            }
        } else {
            entry.declaration.value.clone()
        };
        let declaration = Declaration {
            name: entry.declaration.name.clone(),
            value,
            important: entry.declaration.important,
        };
        for (property, value) in cascade::expand(&declaration) {
            candidates.push(Candidate {
                property,
                value,
                important: entry.declaration.important,
                origin: entry.origin,
                specificity: entry.specificity,
                inline: entry.inline,
                order: entry.order,
            });
        }
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
    /// URLs of stylesheets already registered through an `@import`.
    imported_urls: HashSet<String>,
    /// Diagnostics from CSS parsing and from skipped imports.
    diagnostics: Vec<ParseDiagnostic>,
    /// Selectors parsed once and indexed for fast rule matching.
    index: SelectorIndex,
    /// Custom properties registered with `@property`.
    property_registry: PropertyRegistry,
}

impl StyleEngine {
    /// Creates an engine with the built-in user-agent stylesheet.
    pub fn new(viewport: Viewport) -> Self {
        let user_agent = Stylesheet::parse(ua::DEFAULT_STYLESHEET, Origin::UserAgent);
        let index = SelectorIndex::build(&user_agent, &[], &[]);
        Self {
            viewport,
            user_agent,
            user: Vec::new(),
            author: Vec::new(),
            media_type: "screen".to_string(),
            prefers_color_scheme: "light".to_string(),
            imported_urls: HashSet::new(),
            diagnostics: Vec::new(),
            index,
            property_registry: PropertyRegistry::new(),
        }
    }

    /// Rebuilds the selector index and the `@property` registry after
    /// stylesheets change.
    fn rebuild_index(&mut self) {
        self.index = SelectorIndex::build(&self.user_agent, &self.user, &self.author);
        self.property_registry =
            collect_property_registrations(&self.user_agent, &self.user, &self.author);
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
        self.diagnostics.extend(sheet.diagnostics.iter().cloned());
        match sheet.origin {
            Origin::UserAgent => self.user_agent.rules.extend(sheet.rules),
            Origin::User => self.user.push(sheet),
            Origin::Author => self.author.push(sheet),
        }
        self.rebuild_index();
    }

    /// Parses and registers CSS text.
    ///
    /// Import requests found in the text are ignored here; callers that fetch
    /// `@import` URLs should use [`Self::add_css_collect_imports`] instead.
    pub fn add_css(&mut self, css: &str, origin: Origin) {
        let _ = self.add_css_collect_imports(css, None, origin, 0);
    }

    /// Notes describing skipped CSS: parse-limit hits and ignored imports.
    pub fn diagnostics(&self) -> &[ParseDiagnostic] {
        &self.diagnostics
    }

    /// Parses and registers CSS, returning the `@import` requests it contains.
    ///
    /// `url` is the URL this sheet was fetched from (`None` for inline or root
    /// stylesheets) and `depth` is the number of `@import` hops taken to reach
    /// it, starting at zero. Imports that would exceed [`MAX_IMPORT_DEPTH`] or
    /// that repeat an already-imported URL are skipped, recorded in
    /// [`Self::diagnostics`], and omitted from the returned requests. Register
    /// a fetched sheet by calling this method again with the returned `depth`.
    pub fn add_css_collect_imports(
        &mut self,
        css: &str,
        url: Option<&str>,
        origin: Origin,
        depth: usize,
    ) -> Vec<ImportRequest> {
        if depth > MAX_IMPORT_DEPTH {
            browser_css::push_diagnostic(
                &mut self.diagnostics,
                DiagnosticKind::ImportDepth,
                format!(
                    "@import depth limit of {MAX_IMPORT_DEPTH} reached; skipping import at depth {depth}"
                ),
            );
            return Vec::new();
        }
        if let Some(url) = url
            && !self.imported_urls.insert(url.to_string())
        {
            browser_css::push_diagnostic(
                &mut self.diagnostics,
                DiagnosticKind::DuplicateImport,
                format!("duplicate @import of '{url}'; skipping"),
            );
            return Vec::new();
        }

        // Flatten CSS nesting before handing the text to the parser, which
        // only understands flat style rules. Returns `css` unchanged when no
        // nested rule is present, so existing cascades stay byte-identical.
        let flattened = nesting::flatten(css);
        let sheet = Stylesheet::parse(&flattened, origin);
        let mut requests = Vec::new();
        let mut seen = HashSet::new();
        Self::collect_imports(
            &sheet.rules,
            depth,
            &self.imported_urls,
            &mut seen,
            &mut requests,
            &mut self.diagnostics,
        );
        self.add_stylesheet(sheet);
        requests
    }

    /// Collects import requests from a rule list, applying the depth and
    /// duplicate guards. Nested `@media` rules are traversed as well.
    fn collect_imports(
        rules: &[Rule],
        depth: usize,
        imported_urls: &HashSet<String>,
        seen: &mut HashSet<String>,
        requests: &mut Vec<ImportRequest>,
        diagnostics: &mut Vec<ParseDiagnostic>,
    ) {
        for rule in rules {
            match rule {
                Rule::Import(import) => {
                    let url = import.url.clone();
                    let next = depth + 1;
                    if next > MAX_IMPORT_DEPTH {
                        browser_css::push_diagnostic(
                            diagnostics,
                            DiagnosticKind::ImportDepth,
                            format!(
                                "@import depth limit of {MAX_IMPORT_DEPTH} reached; skipping '{url}'"
                            ),
                        );
                        continue;
                    }
                    if imported_urls.contains(&url) || !seen.insert(url.clone()) {
                        browser_css::push_diagnostic(
                            diagnostics,
                            DiagnosticKind::DuplicateImport,
                            format!("duplicate @import of '{url}'; skipping"),
                        );
                        continue;
                    }
                    requests.push(ImportRequest {
                        url,
                        media: import.media.clone(),
                        depth: next,
                    });
                }
                Rule::Media(media) => Self::collect_imports(
                    &media.rules,
                    depth,
                    imported_urls,
                    seen,
                    requests,
                    diagnostics,
                ),
                _ => {}
            }
        }
    }

    /// Removes every author stylesheet (used when navigating).
    pub fn clear_author_stylesheets(&mut self) {
        self.author.clear();
        self.imported_urls.clear();
        self.rebuild_index();
    }

    /// Removes every non-UA stylesheet.
    pub fn clear_all_stylesheets(&mut self) {
        self.author.clear();
        self.user.clear();
        self.imported_urls.clear();
        self.diagnostics.clear();
        self.user_agent = Stylesheet::parse(ua::DEFAULT_STYLESHEET, Origin::UserAgent);
        self.rebuild_index();
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
        self.compute_with_container(doc, states, &HashMap::new())
    }

    /// Computes styles with an explicit container-size context.
    ///
    /// `containers` maps an element to the query container it should be
    /// evaluated against. Elements without an entry use a context whose
    /// unnamed size is the engine's viewport; named containers can be added
    /// with [`ContainerContext::with_named`]. This is what makes `@container`
    /// rules match during the cascade.
    pub fn compute_with_container(
        &self,
        doc: &Document,
        states: &HashMap<NodeId, ElementState>,
        containers: &HashMap<NodeId, ContainerContext>,
    ) -> StyledTree {
        let mut tree = StyledTree::new();
        tree.property_registry = self.property_registry.clone();
        // Modern at-rules are recorded as visible compatibility diagnostics
        // (not silently dropped) so callers can surface them in DevTools.
        for sheets in [
            std::slice::from_ref(&self.user_agent),
            self.user.as_slice(),
            self.author.as_slice(),
        ] {
            for sheet in sheets {
                collect_unsupported_at_rules(&sheet.rules, &mut tree.unsupported);
            }
        }
        let media = MediaContext {
            width: self.viewport.width,
            height: self.viewport.height,
            media_type: self.media_type.clone(),
            prefers_color_scheme: self.prefers_color_scheme.clone(),
        };
        let default_state = ElementState::default();
        let mut order = 0_usize;
        let media_ok = self.index.media_ok(&media);

        // Buffers reused for every element so the hot path does not allocate
        // a fresh Vec for each node.
        let mut candidates: Vec<usize> = Vec::new();
        let mut pseudo_candidates: Vec<usize> = Vec::new();
        let mut pending: Vec<PendingDeclaration> = Vec::new();
        let mut custom_decls: Vec<(String, CustomDeclaration)> = Vec::new();
        let mut expanded: Vec<Candidate> = Vec::new();
        let mut winners: Vec<Candidate> = Vec::new();

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
            let keys = NodeKeys::new(doc, node);
            let container_ctx = containers.get(&node).cloned().unwrap_or_else(|| {
                ContainerContext::new(ContainerSize {
                    width: self.viewport.width,
                    height: self.viewport.height,
                })
            });

            pending.clear();
            custom_decls.clear();

            // Only rules whose right-hand compound could match this element
            // are considered, and they are visited in source order so `order`
            // matches the naive full scan exactly.
            self.index.lookup(
                RuleKind::Normal,
                &keys,
                &media_ok,
                &container_ctx,
                &mut candidates,
            );
            for &index in &candidates {
                let rule = self.index.rule(index);
                let proximity = match &rule.scope {
                    Some(scope) => match scope.proximity(doc, node, state) {
                        Some(proximity) => proximity,
                        None => continue,
                    },
                    None => NO_SCOPE,
                };
                if !selector_matches(doc, node, &rule.selector, state) {
                    continue;
                }
                for declaration in &rule.declarations {
                    order += 1;
                    self.collect_declaration(
                        declaration.clone(),
                        rule.origin,
                        rule.selector.specificity,
                        false,
                        &mut pending,
                        &mut custom_decls,
                        pack_order(order, rule.layer, proximity),
                    );
                }
            }

            // Presentational hints: HTML presentational attributes behave like
            // low-priority UA declarations (specificity 0-0-1). They are
            // collected here — after the UA sheet, but before inline `style` —
            // and author CSS still wins in the cascade because of the origin
            // weight. Without this, attributes such as `<img width>`,
            // `<td width>` and `<td align>` are ignored.
            for (property, value) in presentational_hints(doc, node) {
                let css = format!("{property}: {value}");
                for declaration in browser_css::parse_inline_style(&css) {
                    order += 1;
                    self.collect_declaration(
                        declaration,
                        Origin::UserAgent,
                        Specificity::new(0, 0, 1),
                        false,
                        &mut pending,
                        &mut custom_decls,
                        pack_order(order, UNLAYERED, NO_SCOPE),
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
                        &mut pending,
                        &mut custom_decls,
                        pack_order(order, UNLAYERED, NO_SCOPE),
                    );
                }
            }

            // Custom properties: inherit, then override with winners.
            let mut customs = parent_customs;
            let mut custom_winners: HashMap<String, CustomDeclaration> = HashMap::new();
            for (name, declaration) in custom_decls.drain(..) {
                match custom_winners.get(&name) {
                    Some(current) if declaration.weight() < current.weight() => {}
                    _ => {
                        custom_winners.insert(name, declaration);
                    }
                }
            }
            for (name, declaration) in custom_winners {
                if let Some(value) =
                    substitute_with_registry(&declaration.value, &customs, &tree.property_registry)
                {
                    customs.insert(name, value);
                }
            }

            // Substitute `var()` references before expanding shorthands so a
            // shorthand such as `border: 1px solid var(--line)` keeps its
            // colour (see `expand_pending`). Both buffers are reused across
            // elements.
            expand_pending(&mut pending, &customs, node, &mut tree, &mut expanded);
            cascade::cascade_into(&mut expanded, &mut winners);
            let style =
                self.compute_element_style(node, doc, &parent_style, &customs, &winners, &mut tree);

            // Generate `::before`/`::after` text content for this element. The
            // declarations are gathered in source order across all origins and
            // already respect media queries and interactive state; only a
            // string `content` value generates a box (`none`, `normal`,
            // `url(...)`, etc. do not).
            for name in ["before", "after"] {
                let kind = if name == "before" {
                    RuleKind::Before
                } else {
                    RuleKind::After
                };
                let Some(pseudo) = self.pseudo_element_for(
                    doc,
                    node,
                    kind,
                    &keys,
                    &media_ok,
                    &container_ctx,
                    state,
                    &mut pseudo_candidates,
                ) else {
                    continue;
                };
                let Some(text) = pseudo.content_text() else {
                    continue;
                };
                let content = text.to_string();
                let pseudo_style = self.compute_pseudo_style(
                    node,
                    doc,
                    &pseudo.declarations,
                    &style,
                    &customs,
                    &mut expanded,
                    &mut winners,
                    &mut tree,
                );
                let entry = tree.pseudos.entry(node).or_default();
                let target = if name == "before" {
                    &mut entry.before
                } else {
                    &mut entry.after
                };
                *target = Some(PseudoStyle {
                    content,
                    style: pseudo_style,
                });
            }

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

    /// The media context used to evaluate pseudo-element rules.
    fn media_context(&self) -> MediaContext {
        MediaContext {
            width: self.viewport.width,
            height: self.viewport.height,
            media_type: self.media_type.clone(),
            prefers_color_scheme: self.prefers_color_scheme.clone(),
        }
    }

    /// Finds the pseudo-element of `kind` for `node`, gathering its
    /// declarations from every origin in source order (user-agent, user, then
    /// author). Uses the pre-parsed index instead of re-parsing every selector
    /// in every sheet for every element.
    #[allow(clippy::too_many_arguments)]
    fn pseudo_element_for(
        &self,
        doc: &Document,
        node: NodeId,
        kind: RuleKind,
        keys: &NodeKeys<'_>,
        media_ok: &[bool],
        containers: &ContainerContext,
        state: &ElementState,
        out: &mut Vec<usize>,
    ) -> Option<PseudoElement> {
        self.index
            .pseudo_element_for(kind, doc, node, keys, media_ok, containers, state, out)
    }

    /// Computes the style of a generated text box, inheriting from its host
    /// element so `color`, `font` and other inherited properties are honored.
    #[allow(clippy::too_many_arguments)]
    fn compute_pseudo_style(
        &self,
        node: NodeId,
        doc: &Document,
        declarations: &[Declaration],
        host: &ComputedStyle,
        customs: &HashMap<String, Value>,
        candidates: &mut Vec<Candidate>,
        winners: &mut Vec<Candidate>,
        tree: &mut StyledTree,
    ) -> ComputedStyle {
        let mut pending = Vec::new();
        let mut custom_decls = Vec::new();
        let mut order = 0_usize;
        for declaration in declarations {
            order += 1;
            self.collect_declaration(
                declaration.clone(),
                Origin::Author,
                Specificity::default(),
                false,
                &mut pending,
                &mut custom_decls,
                order,
            );
        }

        // Custom properties declared on the pseudo-element override the ones
        // inherited from the host, mirroring the main cascade.
        let mut customs = customs.clone();
        let mut custom_winners: HashMap<String, CustomDeclaration> = HashMap::new();
        for (name, declaration) in custom_decls {
            match custom_winners.get(&name) {
                Some(current) if declaration.weight() < current.weight() => {}
                _ => {
                    custom_winners.insert(name, declaration);
                }
            }
        }
        let registry = tree.property_registry.clone();
        for (name, declaration) in custom_winners {
            if let Some(value) = substitute_with_registry(&declaration.value, &customs, &registry) {
                customs.insert(name, value);
            }
        }

        expand_pending(&mut pending, &customs, node, tree, candidates);
        cascade::cascade_into(candidates, winners);
        self.compute_element_style(node, doc, host, &customs, winners, tree)
    }

    /// The uncascaded declarations of a matching `::before`/`::after` rule.
    ///
    /// Declarations are gathered from every origin (user-agent, user, author)
    /// in source order, so a later declaration wins when the same property is
    /// declared more than once. `@media` blocks are evaluated against the
    /// current viewport; interactive state defaults to idle. The computed
    /// result is also available through [`StyledTree::pseudo`].
    pub fn pseudo_declarations(
        &self,
        doc: &Document,
        node: NodeId,
        name: &str,
    ) -> Vec<Declaration> {
        let media = self.media_context();
        let media_ok = self.index.media_ok(&media);
        let keys = NodeKeys::new(doc, node);
        let kind = match name.trim_start_matches(':').to_ascii_lowercase().as_str() {
            "before" => RuleKind::Before,
            "after" => RuleKind::After,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        let containers = ContainerContext::new(ContainerSize {
            width: self.viewport.width,
            height: self.viewport.height,
        });
        self.pseudo_element_for(
            doc,
            node,
            kind,
            &keys,
            &media_ok,
            &containers,
            &ElementState::default(),
            &mut out,
        )
        .map(|element| element.declarations)
        .unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_declaration(
        &self,
        declaration: Declaration,
        origin: Origin,
        specificity: browser_selectors::Specificity,
        inline: bool,
        pending: &mut Vec<PendingDeclaration>,
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
        // Expansion is deferred: `var()` references in shorthand values must
        // be substituted (with the resolved custom map) before the shorthand is
        // split into longhands. See `expand_pending`.
        pending.push(PendingDeclaration {
            declaration,
            origin,
            specificity,
            inline,
            order,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_element_style(
        &self,
        node: NodeId,
        _doc: &Document,
        parent: &ComputedStyle,
        customs: &HashMap<String, Value>,
        winners: &[Candidate],
        tree: &mut StyledTree,
    ) -> ComputedStyle {
        let mut style = ComputedStyle::default();
        let parent_font_size = parent.font_size();
        let registry = tree.property_registry.clone();

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
                let value = substitute_with_registry(&candidate.value, customs, &registry)
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

        for winner in winners {
            if winner.property == Property::FontSize {
                continue;
            }
            let value = match substitute_with_registry(&winner.value, customs, &registry) {
                Some(value) => value,
                None => {
                    // An unresolved `var()` is the one declaration we cannot
                    // use; skip just this one and leave the rest of the style
                    // (and layout) intact, but make the skip visible.
                    tree.unsupported.push(UnsupportedDeclaration {
                        node,
                        property: winner.property.name().to_string(),
                        value: value_text(&winner.value),
                        skipped: true,
                    });
                    continue;
                }
            };
            // `env()`/`attr()` cannot be resolved into typed values here, but
            // on properties that accept opaque text we keep the authored text
            // as a string instead of dropping the declaration. Numeric/length
            // contexts keep the historical "unsupported + diagnostic" path.
            let opaque_function = is_opaque_text_property(winner.property)
                && interpret::env_or_attr(&value).is_some();
            let computed = if opaque_function {
                interpret::env_or_attr(&value)
            } else {
                match inheritance::WideKeyword::detect(&value) {
                    Some(keyword) => {
                        inheritance::apply_wide(keyword, winner.property, Some(parent))
                    }
                    None => match value.ident() {
                        // `interpret::interpret` predates the inline container
                        // keywords and rejects them. Keep them verbatim so
                        // `ComputedStyle::display()` can map them to the new
                        // `Display::InlineFlex` / `Display::InlineGrid` variants.
                        // Every other value follows the normal interpret path.
                        Some(keyword @ ("inline-flex" | "inline-grid"))
                            if winner.property == Property::Display =>
                        {
                            Some(ComputedValue::keyword(keyword))
                        }
                        _ => interpret::interpret(winner.property, &value, &ctx),
                    },
                }
            };
            match computed {
                Some(computed) => {
                    // Best effort: raw-text properties keep their value even
                    // when it contains syntax we cannot act on (for example a
                    // `min()` inside a grid track list). Surface it as a
                    // diagnostic rather than silently degrading the layout.
                    // Gracefully kept `env()`/`attr()` strings are exempt.
                    if !opaque_function
                        && is_raw_text_property(winner.property)
                        && value_has_unsupported_syntax(&value)
                    {
                        tree.unsupported.push(UnsupportedDeclaration {
                            node,
                            property: winner.property.name().to_string(),
                            value: value_text(&value),
                            skipped: false,
                        });
                    }
                    style.set(winner.property, computed);
                }
                None => {
                    // The declaration truly cannot be interpreted: skip only
                    // this one declaration and report it visibly.
                    tree.unsupported.push(UnsupportedDeclaration {
                        node,
                        property: winner.property.name().to_string(),
                        value: value_text(&value),
                        skipped: true,
                    });
                }
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

/// Returns low-priority CSS declarations derived from HTML presentational
/// attributes on `node`. Each pair is `(property, value)`.
///
/// Bare HTML lengths (e.g. `width="25"`) are CSS pixels; percentages and
/// values with an explicit unit pass through unchanged. `cellspacing` would
/// map to `border-spacing`, but this engine has no such property yet, so it is
/// intentionally skipped.
fn presentational_hints(doc: &Document, node: NodeId) -> Vec<(&'static str, String)> {
    let Some(name) = doc.element_name(node) else {
        return Vec::new();
    };
    let mut hints = Vec::new();

    let length_hint = |raw: &str| -> Option<String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        Some(if raw.parse::<f64>().is_ok() {
            format!("{raw}px")
        } else {
            raw.to_string()
        })
    };

    if name.eq_ignore_ascii_case("img") {
        for attribute in ["width", "height"] {
            if let Some(value) = doc.attr(node, attribute).and_then(length_hint) {
                hints.push((attribute, value));
            }
        }
    }

    if name.eq_ignore_ascii_case("td") || name.eq_ignore_ascii_case("th") {
        if let Some(value) = doc.attr(node, "width").and_then(length_hint) {
            hints.push(("width", value));
        }
        if let Some(align) = doc.attr(node, "align") {
            let align = align.trim().to_ascii_lowercase();
            if matches!(align.as_str(), "left" | "center" | "right") {
                hints.push(("text-align", align));
            }
        }
        if doc.has_attr(node, "nowrap") {
            hints.push(("white-space", "nowrap".to_string()));
        }
        // `cellpadding` is written on the table but applies to every cell.
        if let Some(value) = doc
            .ancestors(node)
            .find(|ancestor| doc.is_tag(*ancestor, "table"))
            .and_then(|table| doc.attr(table, "cellpadding"))
            .and_then(length_hint)
        {
            hints.push(("padding", value));
        }
    }

    hints
}

/// Serializes a parsed value back to readable CSS text for diagnostics.
///
/// Whitespace is normalized to single spaces and the result is trimmed; the
/// text is intended for human-facing diagnostics, not for re-parsing.
fn value_text(value: &Value) -> String {
    fn push(components: &[ComponentValue], out: &mut String) {
        for component in components {
            match component {
                ComponentValue::Ident(text) => out.push_str(text),
                ComponentValue::Number(number) => out.push_str(&number.to_string()),
                ComponentValue::Percentage(percent) => {
                    out.push_str(&percent.to_string());
                    out.push('%');
                }
                ComponentValue::Dimension(number, unit) => {
                    out.push_str(&number.to_string());
                    out.push_str(unit);
                }
                ComponentValue::String(text) => {
                    out.push('"');
                    out.push_str(text);
                    out.push('"');
                }
                ComponentValue::Url(url) => {
                    out.push_str("url(");
                    out.push_str(url);
                    out.push(')');
                }
                ComponentValue::Hash(text) => {
                    out.push('#');
                    out.push_str(text);
                }
                ComponentValue::Function(name, args) => {
                    out.push_str(name);
                    out.push('(');
                    push(args, out);
                    out.push(')');
                }
                ComponentValue::Delim(ch) => out.push(*ch),
                ComponentValue::Comma => {
                    out.push(',');
                    out.push(' ');
                }
                ComponentValue::Whitespace => out.push(' '),
            }
        }
    }

    let mut out = String::new();
    push(&value.components, &mut out);
    // Collapse runs of whitespace introduced by normalized separators.
    let mut normalized = String::with_capacity(out.len());
    let mut pending_space = false;
    for ch in out.chars() {
        if ch.is_whitespace() {
            pending_space = !normalized.is_empty();
        } else {
            if pending_space {
                normalized.push(' ');
                pending_space = false;
            }
            normalized.push(ch);
        }
    }
    normalized
}

/// CSS functions the engine does not interpret. When one of these appears in
/// a declaration value, the declaration is surfaced as an unsupported
/// diagnostic even if the surrounding property is stored as raw text.
fn is_unsupported_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "min"
            | "max"
            | "clamp"
            | "env"
            | "attr"
            | "image-set"
            | "cross-fade"
            | "paint"
            | "element"
            | "anchor"
            | "anchor-size"
            | "if"
            | "light-dark"
            | "contrast-color"
    )
}

/// Units the engine can resolve. Unknown units in a raw-text value are also
/// surfaced as diagnostics instead of being silently kept as text.
fn is_known_unit(unit: &str) -> bool {
    matches!(
        unit.to_ascii_lowercase().as_str(),
        "px" | "pt"
            | "pc"
            | "in"
            | "cm"
            | "mm"
            | "q"
            | "em"
            | "rem"
            | "ex"
            | "ch"
            | "cap"
            | "ic"
            | "lh"
            | "rlh"
            | "vw"
            | "vh"
            | "vmin"
            | "vmax"
            | "vi"
            | "vb"
            | "svw"
            | "svh"
            | "lvw"
            | "lvh"
            | "dvw"
            | "dvh"
            | "deg"
            | "rad"
            | "grad"
            | "turn"
            | "s"
            | "ms"
            | "hz"
            | "khz"
            | "dpi"
            | "dpcm"
            | "dppx"
            | "x"
            | "fr"
    )
}

/// True when `value` contains a function or unit the engine cannot interpret.
fn value_has_unsupported_syntax(value: &Value) -> bool {
    fn walk(components: &[ComponentValue]) -> bool {
        for component in components {
            match component {
                ComponentValue::Function(name, args) => {
                    if is_unsupported_function(name) || walk(args) {
                        return true;
                    }
                }
                ComponentValue::Dimension(_, unit) if !is_known_unit(unit) => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }
    walk(&value.components)
}

/// Properties whose values are kept verbatim for the layout engine rather than
/// validated by `interpret`. Unsupported syntax in these values would
/// otherwise pass through without any diagnostic.
fn is_raw_text_property(property: Property) -> bool {
    matches!(
        property,
        Property::GridTemplateColumns
            | Property::GridTemplateRows
            | Property::GridTemplateAreas
            | Property::GridColumn
            | Property::GridRow
            | Property::GridArea
            | Property::GridAutoRows
            | Property::GridAutoColumns
            | Property::GridAutoFlow
            | Property::JustifyItems
            | Property::JustifySelf
            | Property::AlignSelf
            | Property::AlignContent
    )
}

/// Properties whose values can be kept as opaque strings when they contain an
/// unresolvable `env()` or `attr()` call.
fn is_opaque_text_property(property: Property) -> bool {
    is_raw_text_property(property)
        || matches!(property, Property::FontFamily | Property::ListStyleType)
}

/// Builds the `@property` registry from all of the engine's stylesheets.
///
/// Later registrations with the same name win, matching normal cascade order.
fn collect_property_registrations(
    user_agent: &Stylesheet,
    user: &[Stylesheet],
    author: &[Stylesheet],
) -> PropertyRegistry {
    fn visit(rules: &[Rule], registry: &mut PropertyRegistry) {
        for rule in rules {
            match rule {
                Rule::Property(property) => {
                    registry.insert(
                        property.name.clone(),
                        variables::PropertyRegistration {
                            syntax: property.syntax.clone(),
                            inherits: property.inherits.unwrap_or(false),
                            initial_value: property.initial_value.clone(),
                        },
                    );
                }
                Rule::Media(rule) => visit(&rule.rules, registry),
                Rule::Supports(rule) if conditions::supports_condition_matches(&rule.condition) => {
                    visit(&rule.rules, registry)
                }
                Rule::Container(rule) => visit(&rule.rules, registry),
                Rule::LayerBlock(rule) => visit(&rule.rules, registry),
                Rule::Scope(rule) => visit(&rule.rules, registry),
                Rule::StartingStyle(rule) => visit(&rule.rules, registry),
                _ => {}
            }
        }
    }

    let mut registry = PropertyRegistry::new();
    visit(&user_agent.rules, &mut registry);
    for sheet in user {
        visit(&sheet.rules, &mut registry);
    }
    for sheet in author {
        visit(&sheet.rules, &mut registry);
    }
    registry
}

/// Describes an at-rule that is surfaced as a compatibility diagnostic.
///
/// Most of these are now evaluated by the cascade; the diagnostic is retained
/// so existing callers that surface "modern at-rule seen" notices keep working.
fn unsupported_at_rule(rule: &Rule) -> Option<(&'static str, String)> {
    match rule {
        Rule::Supports(_) => Some(("@supports", "conditional rule evaluated".to_string())),
        Rule::Container(_) => Some(("@container", "container query evaluated".to_string())),
        Rule::LayerStatement(_) => Some(("@layer", "layer order applied".to_string())),
        Rule::LayerBlock(layer) => Some((
            "@layer",
            layer
                .name
                .clone()
                .unwrap_or_else(|| "anonymous".to_string()),
        )),
        Rule::Scope(_) => Some(("@scope", "scoped rule applied".to_string())),
        Rule::StartingStyle(_) => Some((
            "@starting-style",
            "transition starting styles ignored".to_string(),
        )),
        Rule::Property(property) => Some(("@property", property.name.clone())),
        _ => None,
    }
}

/// Collects compatibility diagnostics for modern at-rules, including ones
/// nested inside conditional groups. `@starting-style` is the one rule in this
/// set the normal computed-style cascade still ignores.
fn collect_unsupported_at_rules(rules: &[Rule], out: &mut Vec<UnsupportedDeclaration>) {
    for rule in rules {
        if let Some((name, detail)) = unsupported_at_rule(rule) {
            out.push(UnsupportedDeclaration {
                node: AT_RULE_NODE,
                property: name.to_string(),
                value: detail,
                skipped: true,
            });
        }
        match rule {
            Rule::Media(rule) => collect_unsupported_at_rules(&rule.rules, out),
            Rule::Supports(rule) => collect_unsupported_at_rules(&rule.rules, out),
            Rule::Container(rule) => collect_unsupported_at_rules(&rule.rules, out),
            Rule::LayerBlock(rule) => collect_unsupported_at_rules(&rule.rules, out),
            Rule::Scope(rule) => collect_unsupported_at_rules(&rule.rules, out),
            Rule::StartingStyle(rule) => collect_unsupported_at_rules(&rule.rules, out),
            _ => {}
        }
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

/// True when a value contains a `var()` reference anywhere in its component
/// tree (including inside functions such as `rgb(var(--r), 0, 0)`).
pub fn has_unresolved_var(value: &Value) -> bool {
    fn walk(components: &[ComponentValue]) -> bool {
        components.iter().any(|component| match component {
            ComponentValue::Function(name, args) => name.eq_ignore_ascii_case("var") || walk(args),
            _ => false,
        })
    }
    walk(&value.components)
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
    fn ua_gives_submit_input_pill_defaults() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let submit = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, submit);
        doc.set_attr(submit, "type", "submit");

        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let style = styled.get(submit).unwrap();
        assert!(
            style.border_radius().iter().any(|&r| r > 0.0),
            "UA should round submit buttons, got {:?}",
            style.border_radius()
        );
        assert_ne!(
            style.background_color(),
            Color::transparent(),
            "UA should give submit buttons a gray background"
        );
    }

    #[test]
    fn author_border_radius_overrides_ua_form_defaults() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let submit = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, submit);
        doc.set_attr(submit, "type", "submit");

        let engine = engine_with("input { border-radius: 0; }");
        let styled = engine.compute(&doc);
        assert_eq!(
            styled.get(submit).unwrap().border_radius(),
            [0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn ua_gives_search_input_rounded_white_field() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let search = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, search);
        doc.set_attr(search, "type", "search");

        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let style = styled.get(search).unwrap();
        assert!(
            style.border_radius().iter().any(|&r| r > 0.0),
            "UA should round search fields"
        );
        assert_eq!(style.background_color(), Color::rgb(255, 255, 255));
        assert_eq!(style.padding(Side::Left), Dim::Px(16.0));
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

    /// Helper: a document with an `<html><body><div>` tree.
    fn document_with_div() -> (Document, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let div = doc.create_element("div");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, div);
        (doc, div)
    }

    #[test]
    fn var_in_border_shorthand_keeps_color() {
        let (doc, div) = document_with_div();
        let engine = engine_with(":root { --line: #ddd; } div { border: 1px solid var(--line); }");
        let styled = engine.compute(&doc);
        let style = styled.get(div).unwrap();

        assert_eq!(style.border_width(Side::Top), 1.0);
        assert_eq!(style.border_style(Side::Top), BorderStyle::Solid);
        // The substituted colour survives the shorthand expansion on all four
        // sides (before the fix the raw var() token was dropped).
        let light = Color::rgb(0xdd, 0xdd, 0xdd);
        for side in Side::ALL {
            assert_eq!(
                style.border_color(side),
                light,
                "border colour on {side:?} should be the light grey from --line"
            );
        }
    }

    #[test]
    fn var_in_background_shorthand_keeps_color() {
        let (doc, div) = document_with_div();
        let engine = engine_with(":root { --white: #fff; } div { background: var(--white); }");
        let styled = engine.compute(&doc);
        assert_eq!(
            styled.get(div).unwrap().background_color(),
            Color::rgb(0xff, 0xff, 0xff)
        );
    }

    #[test]
    fn unresolved_var_in_shorthand_is_a_visible_diagnostic() {
        let (doc, _html, _body, target) = document();
        let engine = engine_with("#target { border: 1px solid var(--missing); }");
        let styled = engine.compute(&doc);

        let entries = styled.unsupported_entries();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].property, "border");
        assert!(entries[0].skipped);
        // The rest of the style is intact.
        assert_eq!(styled.get(target).unwrap().font_size(), 16.0);
    }

    #[test]
    fn inline_flex_and_grid_display_survive_compute() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let flex = doc.create_element("div");
        let grid = doc.create_element("div");
        doc.set_attr(flex, "id", "flex");
        doc.set_attr(grid, "id", "grid");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, flex);
        doc.append_child(body, grid);

        let engine = engine_with("#flex { display: inline-flex; } #grid { display: inline-grid; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(flex).unwrap().display(), Display::InlineFlex);
        assert_eq!(styled.get(grid).unwrap().display(), Display::InlineGrid);
        assert!(Display::InlineFlex.is_flex_container());
        assert!(Display::InlineGrid.is_grid_container());
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
    fn img_dimensions_are_presentational_hints() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let img = doc.create_element("img");
        doc.append_child(doc.root(), html);
        doc.append_child(html, img);
        doc.set_attr(img, "width", "272");
        doc.set_attr(img, "height", "92");
        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        let style = styled.get(img).unwrap();
        assert_eq!(style.width(), Dim::Px(272.0));
        assert_eq!(style.height(), Dim::Px(92.0));
    }

    #[test]
    fn author_css_overrides_img_presentational_hint() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let img = doc.create_element("img");
        doc.append_child(doc.root(), html);
        doc.append_child(html, img);
        doc.set_attr(img, "width", "272");
        let engine = engine_with("img { width: 120px; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(img).unwrap().width(), Dim::Px(120.0));
    }

    #[test]
    fn table_cell_presentational_hints_are_applied() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let table = doc.create_element("table");
        doc.append_child(doc.root(), html);
        doc.append_child(html, table);
        doc.set_attr(table, "cellpadding", "4");
        doc.set_attr(table, "cellspacing", "0");
        let row = doc.create_element("tr");
        doc.append_child(table, row);
        let left = doc.create_element("td");
        doc.set_attr(left, "width", "25%");
        doc.append_child(row, left);
        let middle = doc.create_element("td");
        doc.set_attr(middle, "align", "center");
        doc.set_attr(middle, "nowrap", "");
        doc.append_child(row, middle);
        let right = doc.create_element("th");
        doc.set_attr(right, "width", "100");
        doc.set_attr(right, "align", "right");
        doc.append_child(row, right);

        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);

        assert_eq!(styled.get(left).unwrap().width(), Dim::Percent(25.0));
        assert_eq!(styled.get(right).unwrap().width(), Dim::Px(100.0));
        assert_eq!(styled.get(middle).unwrap().text_align(), TextAlign::Center);
        assert_eq!(
            styled.get(middle).unwrap().white_space(),
            WhiteSpace::Nowrap
        );
        assert_eq!(styled.get(right).unwrap().text_align(), TextAlign::Right);
        // cellpadding becomes padding on every cell, not on the table itself.
        for cell in [left, middle, right] {
            assert_eq!(styled.get(cell).unwrap().padding(Side::Top), Dim::Px(4.0));
            assert_eq!(styled.get(cell).unwrap().padding(Side::Left), Dim::Px(4.0));
        }
    }

    #[test]
    fn author_css_overrides_table_cell_hints() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let table = doc.create_element("table");
        doc.append_child(doc.root(), html);
        doc.append_child(html, table);
        let row = doc.create_element("tr");
        doc.append_child(table, row);
        let cell = doc.create_element("td");
        doc.set_attr(cell, "width", "25%");
        doc.set_attr(cell, "align", "center");
        doc.append_child(row, cell);

        let engine = engine_with("td { width: 33%; text-align: left; }");
        let styled = engine.compute(&doc);
        let style = styled.get(cell).unwrap();
        assert_eq!(style.width(), Dim::Percent(33.0));
        assert_eq!(style.text_align(), TextAlign::Left);
    }

    #[test]
    fn font_shorthand_sets_size_and_line_height() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("#target { font: 13px/27px Arial, sans-serif; }");
        let styled = engine.compute(&doc);
        let style = styled.get(p).unwrap();
        assert_eq!(style.font_size(), 13.0);
        assert_eq!(style.line_height(), LineHeight::Px(27.0));
    }

    #[test]
    fn logical_and_scrollbar_properties_survive_compute() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "#target { margin-inline-start: 12px; inline-size: 200px; scrollbar-width: none; }",
        );
        let styled = engine.compute(&doc);
        let style = styled.get(p).unwrap();
        assert_eq!(
            Property::from_name("margin-inline-start"),
            Some(Property::MarginInlineStart)
        );
        assert!(style.get(Property::MarginInlineStart).is_some());
        assert!(style.get(Property::InlineSize).is_some());
        assert!(style.get(Property::ScrollbarWidth).is_some());
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

    #[test]
    fn modern_properties_flow_through_cascade_and_interpret() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "#target {
                border-radius: 8px 12px / 4px;
                box-shadow: 0 2px 6px rgba(0, 0, 0, 0.3);
                transform: translate(10px, 20px) rotate(90deg);
                aspect-ratio: 16 / 9;
                object-fit: cover;
                filter: blur(2px) brightness(120%);
                text-shadow: 1px 1px 2px red;
            }",
        );
        let styled = engine.compute(&doc);
        let style = styled.get(p).unwrap();

        assert_eq!(style.border_radius(), [8.0, 12.0, 8.0, 12.0]);
        assert_eq!(style.box_shadow_raw().len(), 1);
        assert_eq!(style.box_shadow_raw()[0].blur, 6.0);
        assert_eq!(style.text_shadow_raw().len(), 1);
        assert_eq!(style.transform_raw().len(), 2);
        assert_eq!(style.aspect_ratio(), AspectRatio::Ratio(16.0, 9.0));
        assert_eq!(style.object_fit(), ObjectFit::Cover);
        assert_eq!(style.filter_raw().len(), 2);
    }

    #[test]
    fn collects_import_requests_with_media_and_depth() {
        let mut engine = StyleEngine::new(Viewport::default());
        let requests = engine.add_css_collect_imports(
            "@import \"a.css\" screen; @media print { @import url(b.css); }",
            None,
            Origin::Author,
            0,
        );
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url, "a.css");
        assert_eq!(requests[0].media.as_deref(), Some("screen"));
        assert_eq!(requests[0].depth, 1);
        assert_eq!(requests[1].url, "b.css");
        assert_eq!(requests[1].depth, 1);
        assert!(engine.diagnostics().is_empty());
    }

    #[test]
    fn import_recursion_is_bounded() {
        let mut engine = StyleEngine::new(Viewport::default());
        let mut requests =
            engine.add_css_collect_imports("@import \"a.css\";", None, Origin::Author, 0);
        assert_eq!(requests.len(), 1);

        let mut hops = 0;
        while let Some(request) = requests.first().cloned() {
            requests = engine.add_css_collect_imports(
                &format!("@import \"next{hops}.css\";"),
                Some(&request.url),
                Origin::Author,
                request.depth,
            );
            hops += 1;
            assert!(hops <= MAX_IMPORT_DEPTH, "import chain must terminate");
        }
        assert_eq!(hops, MAX_IMPORT_DEPTH);
        assert!(
            engine
                .diagnostics()
                .iter()
                .any(|d| d.kind == DiagnosticKind::ImportDepth)
        );
    }

    #[test]
    fn duplicate_imports_are_suppressed() {
        let mut engine = StyleEngine::new(Viewport::default());
        let requests = engine.add_css_collect_imports(
            "@import \"same.css\"; @import \"same.css\";",
            None,
            Origin::Author,
            0,
        );
        // Repeated requests in one sheet are deduplicated.
        assert_eq!(requests.len(), 1);
        assert!(
            engine
                .diagnostics()
                .iter()
                .any(|d| d.kind == DiagnosticKind::DuplicateImport)
        );

        // Registering the imported sheet marks the URL as seen.
        let again = engine.add_css_collect_imports("p {}", Some("same.css"), Origin::Author, 1);
        assert_eq!(again.len(), 0);
        let repeated = engine.add_css_collect_imports("p {}", Some("same.css"), Origin::Author, 1);
        assert!(repeated.is_empty());

        // Later sheets cannot import it again.
        let later =
            engine.add_css_collect_imports("@import \"same.css\";", None, Origin::Author, 0);
        assert!(later.is_empty());
    }

    #[test]
    fn clears_reset_import_tracking() {
        let mut engine = StyleEngine::new(Viewport::default());
        let _ = engine.add_css_collect_imports("p {}", Some("a.css"), Origin::Author, 1);
        engine.clear_author_stylesheets();
        let requests =
            engine.add_css_collect_imports("@import \"a.css\";", None, Origin::Author, 0);
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn engine_absorbs_parse_limit_diagnostics() {
        let mut engine = StyleEngine::new(Viewport::default());
        let css = format!("/*{}*/", "x".repeat(browser_css::MAX_STYLESHEET_BYTES));
        engine.add_css(&css, Origin::Author);
        assert!(
            engine
                .diagnostics()
                .iter()
                .any(|d| d.kind == DiagnosticKind::InputSize)
        );
    }

    fn declared_content(declaration: &Declaration) -> String {
        match declaration.value.components.as_slice() {
            [ComponentValue::String(text)] => text.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn pseudo_declarations_are_collected_in_source_order() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "p::before { content: \"first\"; color: red; } \
             #target::before { content: \"second\"; } \
             p::after { content: \"!\" }",
        );
        let before = engine.pseudo_declarations(&doc, p, "before");
        assert_eq!(before.len(), 3);
        assert_eq!(declared_content(&before[0]), "first");
        assert_eq!(before[1].name, "color");
        assert_eq!(declared_content(&before[2]), "second");
        let after = engine.pseudo_declarations(&doc, p, "::after");
        assert_eq!(after.len(), 1);
        assert_eq!(declared_content(&after[0]), "!");
        assert!(engine.pseudo_declarations(&doc, p, "first-line").is_empty());
    }

    #[test]
    fn pseudo_declarations_respect_media_queries() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "@media (min-width: 2000px) { p::before { content: \"wide\"; } } \
             @media (max-width: 2000px) { p::before { content: \"narrow\"; } }",
        );
        let before = engine.pseudo_declarations(&doc, p, "before");
        assert_eq!(before.len(), 1);
        assert_eq!(declared_content(&before[0]), "narrow");
    }

    #[test]
    fn pseudo_element_stores_content_and_computed_style() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("p::before { content: \">>\"; color: #123456; }");
        let styled = engine.compute(&doc);
        let pseudo = styled.pseudo(p, "::before").unwrap();
        assert_eq!(pseudo.content, ">>");
        assert_eq!(pseudo.style.color(), Color::rgb(0x12, 0x34, 0x56));
        // Inherited properties fall back to the host element's style.
        assert_eq!(
            pseudo.style.font_weight(),
            styled.get(p).unwrap().font_weight()
        );
    }

    #[test]
    fn content_none_or_url_produces_no_pseudo() {
        let (doc, _html, _body, p) = document();
        let engine =
            engine_with("p::before { content: none; } p::after { content: url(icon.svg); }");
        let styled = engine.compute(&doc);
        assert!(styled.pseudo(p, "before").is_none());
        assert!(styled.pseudo(p, "after").is_none());
    }

    #[test]
    fn pseudo_content_unescapes_strings() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("p::before { content: \"a\\\"b\"; }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.pseudo(p, "before").unwrap().content, "a\"b");
    }

    #[test]
    fn unsupported_function_in_grid_track_list_is_reported_but_kept() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let grid = doc.create_element("div");
        doc.set_attr(grid, "id", "g");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, grid);

        let engine =
            engine_with("#g { display: grid; grid-template-columns: repeat(3, min(1fr, 100px)); }");
        let styled = engine.compute(&doc);

        assert_eq!(styled.unsupported_count(), 1);
        let declarations = styled.unsupported_declarations();
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].0, "grid-template-columns");
        assert!(
            declarations[0].1.contains("min("),
            "raw value should be preserved: {:?}",
            declarations[0].1
        );
        // Best effort: the value is still kept on the computed style and the
        // display is untouched — no silent fallback to block layout here.
        assert_eq!(styled.get(grid).unwrap().display(), Display::Grid);
        assert!(
            styled
                .get(grid)
                .unwrap()
                .grid_template_columns()
                .unwrap()
                .contains("min(")
        );
        assert!(!styled.unsupported_entries()[0].skipped);
        assert!(
            styled.unsupported_console_messages()[0].starts_with("[CSS]"),
            "messages must be console-ready"
        );
    }

    #[test]
    fn normal_flex_page_has_no_unsupported_diagnostics() {
        let (doc, _html, _body, _p) = document();
        let engine = engine_with(
            "#target { display: flex; flex-direction: row; gap: 8px; } \
             p { width: 100px; height: 40px; background-color: #123456; }",
        );
        let styled = engine.compute(&doc);
        assert_eq!(styled.unsupported_count(), 0);
        assert!(styled.unsupported_declarations().is_empty());
        assert!(styled.unsupported_console_messages().is_empty());
    }

    #[test]
    fn unparseable_declaration_is_skipped_without_disturbing_others() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "#target { width: env(safe-area-inset-left); background-color: #123456; font-size: 20px; }",
        );
        let styled = engine.compute(&doc);
        let style = styled.get(p).unwrap();

        // Only the unparseable declaration is skipped.
        assert_eq!(styled.unsupported_count(), 1);
        assert_eq!(styled.unsupported_declarations()[0].0, "width");
        assert!(styled.unsupported_entries()[0].skipped);
        // The other declarations still apply; layout-relevant values intact.
        assert_eq!(style.background_color(), Color::rgb(0x12, 0x34, 0x56));
        assert_eq!(style.font_size(), 20.0);
        assert!(style.width().is_auto());
    }

    #[test]
    fn supported_math_function_is_interpreted_without_a_diagnostic() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with("#target { width: min(100px, 50%); }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.unsupported_count(), 0);
        assert_eq!(
            styled.get(p).unwrap().width(),
            Dim::Calc {
                percent: 50.0,
                px: 0.0
            }
        );
    }

    #[test]
    fn unsupported_at_rules_are_reported_as_diagnostics() {
        let (doc, _html, _body, _p) = document();
        let engine = engine_with(
            "@supports (display: grid) { p { display: grid; } } \
             @container (width > 100px) { p { color: red; } } \
             @layer base { p { font-weight: bold; } }",
        );
        let styled = engine.compute(&doc);
        let properties: Vec<&str> = styled
            .unsupported_entries()
            .iter()
            .map(|entry| entry.property.as_str())
            .collect();
        assert!(properties.contains(&"@supports"), "{properties:?}");
        assert!(properties.contains(&"@container"), "{properties:?}");
        assert!(properties.contains(&"@layer"), "{properties:?}");
        assert!(
            styled
                .unsupported_console_messages()
                .iter()
                .any(|message| message.contains("@container")),
        );
    }

    // ---- Performance regression -----------------------------------------

    /// A 4000-node document with 5000 rules must stay comfortably interactive.
    ///
    /// The old full-scan implementation re-parsed every selector for every
    /// element and took tens of seconds here; the indexed path parses each
    /// selector once and tests only the rules that could match.
    #[test]
    fn styles_4000_nodes_with_5000_rules_quickly() {
        use std::time::Instant;

        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        for i in 0..4000 {
            let element = doc.create_element("div");
            doc.set_attr(element, "class", &format!("c{i}"));
            doc.set_attr(element, "id", &format!("n{i}"));
            doc.append_child(body, element);
        }
        let mut css = String::with_capacity(5000 * 64);
        for i in 0..5000 {
            css.push_str(&format!(
                ".c{i} {{ color: rgb({},{},{}); padding: {}px; }}\n",
                i % 255,
                (i * 3) % 255,
                (i * 7) % 255,
                i % 20
            ));
        }
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(&css, Origin::Author);

        let start = Instant::now();
        let styled = engine.compute(&doc);
        let elapsed = start.elapsed();
        assert_eq!(styled.len(), 4002); // html, body, 4000 divs
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "styling 4000 nodes / 5000 rules took {elapsed:?}"
        );
    }

    // ---- Byte-identical cascade checks ----------------------------------

    /// Serializes every computed property of every node (plus pseudo styles,
    /// custom properties and diagnostics) for exact before/after comparison.
    fn dump_styled(styled: &StyledTree) -> String {
        let mut out = String::new();
        let mut entries: Vec<_> = styled.entries().collect();
        entries.sort_by_key(|(id, _)| **id);
        for (id, style) in entries {
            out.push_str(&format!("{id:?} {style:?}\n"));
            for name in ["before", "after"] {
                if let Some(pseudo) = styled.pseudo(*id, name) {
                    out.push_str(&format!(
                        "  ::{name} {:?} {:?}\n",
                        pseudo.content, pseudo.style
                    ));
                }
            }
            if let Some(customs) = styled.customs(*id) {
                let mut entries: Vec<_> = customs.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                out.push_str(&format!("  vars {entries:?}\n"));
            }
        }
        out.push_str(&format!("unsupported {:?}\n", styled.unsupported_entries()));
        out.push_str(&format!("root-font-size {:?}\n", styled.root_font_size));
        out
    }

    /// Reference implementation: the original full scan that re-parses every
    /// selector for every element. Used only to assert the indexed path is
    /// byte-identical for representative documents.
    #[allow(clippy::too_many_arguments)]
    fn naive_collect_rules(
        engine: &StyleEngine,
        doc: &Document,
        node: NodeId,
        rules: &[Rule],
        origin: Origin,
        media: &MediaContext,
        state: &ElementState,
        pending: &mut Vec<PendingDeclaration>,
        customs: &mut Vec<(String, CustomDeclaration)>,
        order: &mut usize,
    ) {
        for rule in rules {
            match rule {
                Rule::Style(style_rule) => {
                    for selector_text in &style_rule.selectors {
                        let Ok(selector) = browser_selectors::parse_selector(selector_text) else {
                            continue;
                        };
                        if !selector_matches(doc, node, &selector, state) {
                            continue;
                        }
                        for declaration in &style_rule.declarations {
                            *order += 1;
                            engine.collect_declaration(
                                declaration.clone(),
                                origin,
                                selector.specificity,
                                false,
                                pending,
                                customs,
                                *order,
                            );
                        }
                    }
                }
                Rule::Media(media_rule) if media_matches(&media_rule.query, media) => {
                    naive_collect_rules(
                        engine,
                        doc,
                        node,
                        &media_rule.rules,
                        origin,
                        media,
                        state,
                        pending,
                        customs,
                        order,
                    );
                }
                _ => {}
            }
        }
    }

    fn naive_pseudo_rules<'a>(rules: &'a [Rule], media: &MediaContext, out: &mut Vec<&'a Rule>) {
        for rule in rules {
            match rule {
                Rule::Style(_) => out.push(rule),
                Rule::Media(media_rule) if media_matches(&media_rule.query, media) => {
                    naive_pseudo_rules(&media_rule.rules, media, out);
                }
                _ => {}
            }
        }
    }

    fn naive_pseudo(
        engine: &StyleEngine,
        doc: &Document,
        node: NodeId,
        name: &str,
        media: &MediaContext,
        state: &ElementState,
    ) -> Option<PseudoElement> {
        let name = name.trim_start_matches(':').to_ascii_lowercase();
        if name != "before" && name != "after" {
            return None;
        }
        let mut rules = Vec::new();
        for sheets in [
            std::slice::from_ref(&engine.user_agent),
            engine.user.as_slice(),
            engine.author.as_slice(),
        ] {
            for sheet in sheets {
                naive_pseudo_rules(&sheet.rules, media, &mut rules);
            }
        }
        browser_selectors::pseudo_elements_from_rules(doc, node, rules, state)
            .into_iter()
            .find(|element| element.name == name)
    }

    fn naive_compute(
        engine: &StyleEngine,
        doc: &Document,
        states: &HashMap<NodeId, ElementState>,
    ) -> StyledTree {
        let mut tree = StyledTree::new();
        for sheets in [
            std::slice::from_ref(&engine.user_agent),
            engine.user.as_slice(),
            engine.author.as_slice(),
        ] {
            for sheet in sheets {
                collect_unsupported_at_rules(&sheet.rules, &mut tree.unsupported);
            }
        }
        let media = MediaContext {
            width: engine.viewport.width,
            height: engine.viewport.height,
            media_type: engine.media_type.clone(),
            prefers_color_scheme: engine.prefers_color_scheme.clone(),
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
            let mut pending = Vec::new();
            let mut custom_decls: Vec<(String, CustomDeclaration)> = Vec::new();

            for (origin, sheets) in [
                (Origin::UserAgent, std::slice::from_ref(&engine.user_agent)),
                (Origin::User, engine.user.as_slice()),
                (Origin::Author, engine.author.as_slice()),
            ] {
                for sheet in sheets {
                    naive_collect_rules(
                        engine,
                        doc,
                        node,
                        &sheet.rules,
                        origin,
                        &media,
                        state,
                        &mut pending,
                        &mut custom_decls,
                        &mut order,
                    );
                }
            }
            for (property, value) in presentational_hints(doc, node) {
                for declaration in browser_css::parse_inline_style(&format!("{property}: {value}"))
                {
                    order += 1;
                    engine.collect_declaration(
                        declaration,
                        Origin::UserAgent,
                        Specificity::new(0, 0, 1),
                        false,
                        &mut pending,
                        &mut custom_decls,
                        order,
                    );
                }
            }
            if let Some(css) = doc.attr(node, "style") {
                let specificity = specificity::inline_specificity();
                for declaration in browser_css::parse_inline_style(css) {
                    order += 1;
                    engine.collect_declaration(
                        declaration,
                        Origin::Author,
                        specificity,
                        true,
                        &mut pending,
                        &mut custom_decls,
                        order,
                    );
                }
            }

            let mut customs = parent_customs;
            let mut custom_winners: HashMap<String, CustomDeclaration> = HashMap::new();
            for (name, declaration) in custom_decls {
                match custom_winners.get(&name) {
                    Some(current) if declaration.weight() < current.weight() => {}
                    _ => {
                        custom_winners.insert(name, declaration);
                    }
                }
            }
            for (name, declaration) in custom_winners {
                if let Some(value) = variables::substitute(&declaration.value, &customs) {
                    customs.insert(name, value);
                }
            }

            let mut candidates = Vec::new();
            let mut winners = Vec::new();
            expand_pending(&mut pending, &customs, node, &mut tree, &mut candidates);
            cascade::cascade_into(&mut candidates, &mut winners);
            let style = engine.compute_element_style(
                node,
                doc,
                &parent_style,
                &customs,
                &winners,
                &mut tree,
            );

            for name in ["before", "after"] {
                let Some(pseudo) = naive_pseudo(engine, doc, node, name, &media, state) else {
                    continue;
                };
                let Some(text) = pseudo.content_text() else {
                    continue;
                };
                let content = text.to_string();
                let pseudo_style = engine.compute_pseudo_style(
                    node,
                    doc,
                    &pseudo.declarations,
                    &style,
                    &customs,
                    &mut candidates,
                    &mut winners,
                    &mut tree,
                );
                let entry = tree.pseudos.entry(node).or_default();
                let target = if name == "before" {
                    &mut entry.before
                } else {
                    &mut entry.after
                };
                *target = Some(PseudoStyle {
                    content,
                    style: pseudo_style,
                });
            }

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

    #[test]
    fn indexed_cascade_is_byte_identical_to_full_scan() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let tags = ["div", "p", "span", "a", "li", "input", "td", "section", "b"];
        let mut nodes = Vec::new();
        for (i, tag) in tags.iter().enumerate() {
            let node = doc.create_element(tag);
            doc.set_attr(node, "class", if i % 2 == 0 { "box big" } else { "text" });
            doc.set_attr(node, "id", &format!("n{i}"));
            doc.set_attr(node, "data-x", "y");
            doc.set_attr(node, "style", "letter-spacing: 1px;");
            doc.append_child(body, node);
            nodes.push(node);
        }
        let child = doc.create_element("b");
        doc.set_attr(child, "class", "text");
        doc.append_child(nodes[2], child);
        nodes.push(child);

        let css = r#"
            * { box-sizing: border-box; }
            div { color: red; margin: 1px 2px 3px; padding: 5px; border: 1px solid blue; }
            .box { font-size: 20px; }
            #n0 { color: blue !important; background: green; }
            div.box { color: purple; width: 100px; }
            body > div { height: 10px; }
            .box + p { text-align: center; }
            li ~ li { font-weight: bold; }
            [data-x="y"] { border-style: dashed; }
            a:hover { color: orange; }
            :root { --brand: teal; }
            div { background-color: var(--brand); }
            p::before { content: "BEFORE"; color: red; font-weight: bold; }
            p::after { content: "AFTER"; color: green; }
            .box::before { content: "BOX"; }
            @media (max-width: 900px) { .box { color: navy; } #n0 { border-width: 3px; } }
            @media (min-width: 2000px) { .box { color: yellow; } }
            @media screen and (max-width: 1000px) { @media (min-width: 500px) { .text { color: gray; } } }
            input:checked { outline: 2px solid red; }
            td { text-align: right; }
            .text:first-child { color: pink; }
            b { display: inline; }
        "#;
        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_stylesheet(Stylesheet::parse(
            "p::before { content: \"UA\"; }",
            Origin::UserAgent,
        ));
        engine.add_css(
            "div { font-family: serif; } #n0 { color: aqua !important; } \
             .box { opacity: 0.5; } p::after { content: \"USER\"; color: blue; }",
            Origin::User,
        );
        engine.add_css(css, Origin::Author);

        let mut hovered = HashMap::new();
        hovered.insert(
            nodes[3],
            ElementState {
                hover: true,
                ..ElementState::default()
            },
        );
        hovered.insert(
            nodes[5],
            ElementState {
                checked: Some(true),
                ..ElementState::default()
            },
        );

        for states in [HashMap::new(), hovered] {
            let fast = engine.compute_with_state(&doc, &states);
            let reference = naive_compute(&engine, &doc, &states);
            assert_eq!(dump_styled(&fast), dump_styled(&reference));
        }
    }

    // ---- Conditional at-rule evaluation ---------------------------------

    #[test]
    fn supports_includes_matching_rules_and_excludes_unsupported_ones() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "p { color: black; } \
             @supports (display: grid) { p { color: red; } } \
             @supports (not-a-property: value) { p { color: blue; } } \
             @supports selector(:has(*)) { p { font-weight: bold; } }",
        );
        let styled = engine.compute(&doc);
        let style = styled.get(p).unwrap();
        assert_eq!(style.color(), Color::rgb(255, 0, 0));
        assert_eq!(style.font_weight(), 700);
    }

    #[test]
    fn supports_logical_operators_gate_rules() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "p { color: black; } \
             @supports (display: grid) and (color: red) { p { color: green; } } \
             @supports (display: grid) and (no-such-property: x) { p { color: blue; } } \
             @supports not (no-such-property: x) { p { font-size: 20px; } }",
        );
        let styled = engine.compute(&doc);
        let style = styled.get(p).unwrap();
        assert_eq!(style.color(), Color::rgb(0, 128, 0));
        assert_eq!(style.font_size(), 20.0);
    }

    #[test]
    fn container_query_uses_viewport_by_default() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "p { color: black; } \
             @container (min-width: 400px) { p { color: red; } }",
        );
        assert_eq!(
            engine.compute(&doc).get(p).unwrap().color(),
            Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn container_query_uses_explicit_container_context() {
        use conditions::ContainerSize;

        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "p { color: black; } \
             @container (min-width: 400px) { p { color: red; } } \
             @container (width < 300px) { p { font-size: 12px; } }",
        );
        let mut containers = HashMap::new();
        containers.insert(
            p,
            ContainerContext::new(ContainerSize {
                width: 200.0,
                height: 100.0,
            }),
        );
        let styled = engine.compute_with_container(&doc, &HashMap::new(), &containers);
        let style = styled.get(p).unwrap();
        assert_eq!(style.color(), Color::rgb(0, 0, 0));
        assert_eq!(style.font_size(), 12.0);
    }

    #[test]
    fn named_container_query_selects_named_size() {
        use conditions::ContainerSize;

        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "p { color: black; } \
             @container sidebar (min-width: 300px) { p { color: blue; } }",
        );
        let mut containers = HashMap::new();
        containers.insert(
            p,
            ContainerContext::new(ContainerSize {
                width: 100.0,
                height: 100.0,
            })
            .with_named(
                "sidebar",
                ContainerSize {
                    width: 400.0,
                    height: 200.0,
                },
            ),
        );
        let styled = engine.compute_with_container(&doc, &HashMap::new(), &containers);
        assert_eq!(styled.get(p).unwrap().color(), Color::rgb(0, 0, 255));
    }

    #[test]
    fn later_layers_win_over_specificity() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "@layer early { #target { color: red; } } \
             @layer late { p { color: blue; } }",
        );
        assert_eq!(
            engine.compute(&doc).get(p).unwrap().color(),
            Color::rgb(0, 0, 255)
        );
    }

    #[test]
    fn unlayered_rules_win_over_layers() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "@layer base { #target { color: red; } } \
             p { color: green; }",
        );
        assert_eq!(
            engine.compute(&doc).get(p).unwrap().color(),
            Color::rgb(0, 128, 0)
        );
    }

    #[test]
    fn layer_statement_defines_order() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "@layer early, late; \
             @layer early { p { color: red; } } \
             @layer late { p { color: blue; } }",
        );
        assert_eq!(
            engine.compute(&doc).get(p).unwrap().color(),
            Color::rgb(0, 0, 255)
        );
    }

    #[test]
    fn important_layer_order_reverses() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "@layer early { p { color: red !important; } } \
             @layer late { p { color: blue !important; } }",
        );
        assert_eq!(
            engine.compute(&doc).get(p).unwrap().color(),
            Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn scope_root_and_limit_constrain_matching() {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let root = doc.create_element("div");
        doc.set_attr(root, "class", "root");
        doc.append_child(body, root);
        let inside = doc.create_element("p");
        doc.append_child(root, inside);
        let limited = doc.create_element("div");
        doc.set_attr(limited, "class", "limit");
        doc.append_child(root, limited);
        let excluded = doc.create_element("p");
        doc.append_child(limited, excluded);
        let outside = doc.create_element("p");
        doc.append_child(body, outside);

        let engine = engine_with("@scope (.root) to (.limit) { p { color: red; } }");
        let styled = engine.compute(&doc);
        assert_eq!(styled.get(inside).unwrap().color(), Color::rgb(255, 0, 0));
        assert_ne!(styled.get(excluded).unwrap().color(), Color::rgb(255, 0, 0));
        assert_ne!(styled.get(outside).unwrap().color(), Color::rgb(255, 0, 0));
    }

    #[test]
    fn property_registration_supplies_typed_fallback() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "@property --brand { syntax: \"<color>\"; inherits: true; initial-value: #abcdef; } \
             p { color: var(--brand); }",
        );
        assert_eq!(
            engine.compute(&doc).get(p).unwrap().color(),
            Color::rgb(0xab, 0xcd, 0xef)
        );
    }

    #[test]
    fn env_and_attr_are_kept_as_opaque_strings() {
        let (doc, _html, _body, p) = document();
        let engine = engine_with(
            "#target { font-family: env(safe-area-inset-left); \
             grid-template-areas: attr(data-areas); }",
        );
        let styled = engine.compute(&doc);
        let style = styled.get(p).unwrap();
        assert!(matches!(
            style.get(Property::FontFamily),
            Some(ComputedValue::Str(text)) if text.contains("env(")
        ));
        assert!(matches!(
            style.get(Property::GridTemplateAreas),
            Some(ComputedValue::Str(text)) if text.contains("attr(")
        ));
    }

    #[test]
    fn nested_rules_are_flattened_with_combined_specificity() {
        let (mut doc, _html, _body, p) = document();
        doc.set_attr(p, "class", "card");
        let engine = engine_with(
            "p { color: black; } \
             .card { &:hover { color: red; } .title { color: blue; } }",
        );
        // `&:hover` flattens to `.card:hover`: it wins over `p` for the
        // hovered card, and the implicit descendant becomes `.card .title`.
        let mut states = HashMap::new();
        states.insert(
            p,
            ElementState {
                hover: true,
                ..ElementState::default()
            },
        );
        let styled = engine.compute_with_state(&doc, &states);
        assert_eq!(styled.get(p).unwrap().color(), Color::rgb(255, 0, 0));
    }
}
