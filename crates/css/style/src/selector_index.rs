//! Pre-parsed, key-indexed stylesheet rules for the style cascade.
//!
//! A naive cascade re-parses every selector in every stylesheet for every
//! element (`O(nodes × rules)` parses). This module parses each selector
//! **once**, when stylesheets are registered, and indexes it under a single
//! key taken from the right-hand compound (`#id`, `.class` or `type`).
//!
//! At match time an element only considers the rules in the buckets for its
//! own tag, id and classes, plus the small "universal" bucket for selectors
//! whose right-hand compound has no such key (attribute selectors, bare
//! pseudo-classes, `*`). Every candidate is still verified with the real
//! matcher, so indexing can only add false positives, never false negatives.
//!
//! Rules are kept in global source order (user-agent, user, author; then
//! stylesheet, rule and selector order). Lookups are sorted by that order
//! before the caller collects declarations, which makes the cascade's `order`
//! counter — and therefore the resulting computed styles — byte-identical to
//! a full scan.

use std::collections::HashMap;

use browser_css::{ContainerQuery, Declaration, Origin, Rule, Stylesheet};
use browser_dom::{Document, NodeId};
use browser_selectors::{
    ElementState, PseudoElement, Selector, SimpleSelector, matches_pseudo_element_host_with_state,
    parse_selector,
};

use crate::conditions::{
    ContainerContext, LayerOrder, ScopeMatcher, container_query_matches, supports_condition_matches,
};
use crate::media_queries::{MediaContext, media_matches};

/// Which family of rules to look up for an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleKind {
    /// Rules that contribute to the element's own computed style.
    Normal,
    /// Rules targeting `::before`.
    Before,
    /// Rules targeting `::after`.
    After,
}

/// A selector parsed once, with everything the cascade needs to apply it.
#[derive(Debug)]
pub(crate) struct CompiledRule {
    /// Parsed selector (never parsed again per element).
    pub(crate) selector: Selector,
    /// Declarations from the style rule the selector came from.
    pub(crate) declarations: Vec<Declaration>,
    /// Origin of the owning stylesheet.
    pub(crate) origin: Origin,
    /// Global source order, used to restore declaration order after lookup.
    pub(crate) seq: usize,
    /// Media query lists along the path to this rule; all must match.
    media: Vec<String>,
    /// `@container` queries along the path; all must match the element's
    /// container context. Each entry is `(optional name, query)`.
    containers: Vec<(Option<String>, ContainerQuery)>,
    /// `@layer` rank assigned from source order (`UNLAYERED` when unlayered).
    pub(crate) layer: u32,
    /// Innermost `@scope` root/limit matcher, if any.
    pub(crate) scope: Option<ScopeMatcher>,
}

/// Index buckets for one rule family. Each vector is kept in ascending `seq`.
#[derive(Debug, Default)]
struct Buckets {
    by_tag: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_id: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
}

impl Buckets {
    /// Index `rule` under one key from its right-hand compound.
    ///
    /// Every simple in that compound must match the element, so any one of
    /// them is a sound key. Preferring the most selective key keeps candidate
    /// lists short. Selectors with no key go to `universal`.
    fn insert(&mut self, rule: &CompiledRule, index: usize) {
        let compound = match rule.selector.parts.last() {
            Some((_, compound)) => compound,
            None => return,
        };
        let mut id = None;
        let mut class = None;
        let mut tag = None;
        for simple in &compound.simples {
            match simple {
                SimpleSelector::Id(value) if id.is_none() => id = Some(value.clone()),
                SimpleSelector::Class(value) if class.is_none() => class = Some(value.clone()),
                SimpleSelector::Type(value) if tag.is_none() => {
                    // HTML tag names match case-insensitively; lowercasing the
                    // key (and the element name at lookup) can only add false
                    // positives, never drop a rule that really matches.
                    tag = Some(value.to_ascii_lowercase());
                }
                _ => {}
            }
        }
        if let Some(id) = id {
            self.by_id.entry(id).or_default().push(index);
        } else if let Some(class) = class {
            self.by_class.entry(class).or_default().push(index);
        } else if let Some(tag) = tag {
            self.by_tag.entry(tag).or_default().push(index);
        } else {
            self.universal.push(index);
        }
    }
}

/// The identity of an element as far as key lookup is concerned, computed
/// once per element and shared by the normal and both pseudo-element lookups.
pub(crate) struct NodeKeys<'a> {
    tag: Option<String>,
    id: Option<&'a str>,
    classes: Vec<&'a str>,
}

impl<'a> NodeKeys<'a> {
    /// Collects the lookup keys of `node`.
    pub(crate) fn new(doc: &'a Document, node: NodeId) -> Self {
        Self {
            tag: doc.element_name(node).map(str::to_ascii_lowercase),
            id: doc.attr(node, "id"),
            classes: doc.class_list(node),
        }
    }
}

/// All parsed rules, indexed by the keys that can select them.
#[derive(Debug, Default)]
pub(crate) struct SelectorIndex {
    rules: Vec<CompiledRule>,
    normal: Buckets,
    before: Buckets,
    after: Buckets,
}

impl SelectorIndex {
    /// Parses and indexes every rule in the engine's sheets, in cascade order.
    pub(crate) fn build(
        user_agent: &Stylesheet,
        user: &[Stylesheet],
        author: &[Stylesheet],
    ) -> Self {
        let mut index = Self::default();
        let mut sheet_rules: Vec<&[Rule]> = vec![user_agent.rules.as_slice()];
        sheet_rules.extend(user.iter().map(|sheet| sheet.rules.as_slice()));
        sheet_rules.extend(author.iter().map(|sheet| sheet.rules.as_slice()));
        let layer_order = LayerOrder::from_rules(sheet_rules);
        let mut seq = 0_usize;
        for (origin, sheets) in [
            (Origin::UserAgent, std::slice::from_ref(user_agent)),
            (Origin::User, user),
            (Origin::Author, author),
        ] {
            for sheet in sheets {
                let mut media = Vec::new();
                let mut anonymous = 0usize;
                index.add_rules(
                    &sheet.rules,
                    origin,
                    &mut media,
                    &mut seq,
                    &layer_order,
                    "",
                    &mut anonymous,
                    &[],
                    None,
                );
            }
        }
        index
    }

    /// Adds style rules to a bucket, carrying the enclosing conditional
    /// context (media, supports, container, layer and scope) with them.
    #[allow(clippy::too_many_arguments)]
    fn add_rules(
        &mut self,
        rules: &[Rule],
        origin: Origin,
        media: &mut Vec<String>,
        seq: &mut usize,
        layer_order: &LayerOrder,
        layer_path: &str,
        anonymous: &mut usize,
        containers: &[(Option<String>, ContainerQuery)],
        scope: Option<&ScopeMatcher>,
    ) {
        for rule in rules {
            match rule {
                Rule::Style(style_rule) => {
                    for selector_text in &style_rule.selectors {
                        let Ok(selector) = parse_selector(selector_text) else {
                            continue;
                        };
                        let kind = match selector.pseudo_element.as_deref() {
                            None => Some(RuleKind::Normal),
                            Some(name) if name.eq_ignore_ascii_case("before") => {
                                Some(RuleKind::Before)
                            }
                            Some(name) if name.eq_ignore_ascii_case("after") => {
                                Some(RuleKind::After)
                            }
                            // Other pseudo-elements (::first-line, ::part(),
                            // ::selection, ...) are out of scope, as in the
                            // pseudo-element collector in browser-selectors.
                            Some(_) => None,
                        };
                        let Some(kind) = kind else {
                            continue;
                        };
                        let compiled = CompiledRule {
                            selector,
                            declarations: style_rule.declarations.clone(),
                            origin,
                            seq: *seq,
                            media: media.clone(),
                            containers: containers.to_vec(),
                            layer: layer_order.rank(layer_path),
                            scope: scope.cloned(),
                        };
                        *seq += 1;
                        let index = self.rules.len();
                        let buckets = match kind {
                            RuleKind::Normal => &mut self.normal,
                            RuleKind::Before => &mut self.before,
                            RuleKind::After => &mut self.after,
                        };
                        buckets.insert(&compiled, index);
                        self.rules.push(compiled);
                    }
                }
                Rule::Media(media_rule) => {
                    media.push(media_rule.query.clone());
                    self.add_rules(
                        &media_rule.rules,
                        origin,
                        media,
                        seq,
                        layer_order,
                        layer_path,
                        anonymous,
                        containers,
                        scope,
                    );
                    media.pop();
                }
                Rule::Supports(supports_rule) => {
                    // Support is static for an engine, so decide once here.
                    if !supports_condition_matches(&supports_rule.condition) {
                        continue;
                    }
                    self.add_rules(
                        &supports_rule.rules,
                        origin,
                        media,
                        seq,
                        layer_order,
                        layer_path,
                        anonymous,
                        containers,
                        scope,
                    );
                }
                Rule::Container(container_rule) => {
                    let mut nested = containers.to_vec();
                    nested.push((container_rule.name.clone(), container_rule.query.clone()));
                    self.add_rules(
                        &container_rule.rules,
                        origin,
                        media,
                        seq,
                        layer_order,
                        layer_path,
                        anonymous,
                        &nested,
                        scope,
                    );
                }
                Rule::LayerBlock(layer_block) => {
                    *anonymous += 1;
                    let own = layer_block
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("\0anon{anonymous}"));
                    let full = if layer_path.is_empty() {
                        own
                    } else {
                        format!("{layer_path}.{own}")
                    };
                    self.add_rules(
                        &layer_block.rules,
                        origin,
                        media,
                        seq,
                        layer_order,
                        &full,
                        anonymous,
                        containers,
                        scope,
                    );
                }
                Rule::Scope(scope_rule) => {
                    let matcher =
                        ScopeMatcher::new(scope_rule.root.as_deref(), scope_rule.limit.as_deref());
                    self.add_rules(
                        &scope_rule.rules,
                        origin,
                        media,
                        seq,
                        layer_order,
                        layer_path,
                        anonymous,
                        containers,
                        Some(&matcher),
                    );
                }
                // `@starting-style` supplies transition start values, which
                // are not part of the normal computed style. `@layer`
                // statements only define order (handled above), and imports
                // are fetched by the browser core.
                _ => {}
            }
        }
    }

    /// Evaluates media queries for every compiled rule. Called once per style
    /// computation rather than once per element.
    pub(crate) fn media_ok(&self, media: &MediaContext) -> Vec<bool> {
        self.rules
            .iter()
            .map(|rule| rule.media.iter().all(|query| media_matches(query, media)))
            .collect()
    }

    /// Lists the rules of `kind` that could match `keys`, in source order.
    ///
    /// `out` is cleared and reused across elements to avoid reallocating.
    /// Rules whose `@container` conditions do not match `containers` are
    /// filtered out here, alongside the media queries.
    pub(crate) fn lookup(
        &self,
        kind: RuleKind,
        keys: &NodeKeys<'_>,
        media_ok: &[bool],
        containers: &ContainerContext,
        out: &mut Vec<usize>,
    ) {
        out.clear();
        let buckets = match kind {
            RuleKind::Normal => &self.normal,
            RuleKind::Before => &self.before,
            RuleKind::After => &self.after,
        };
        Self::push_matching(&buckets.universal, self, media_ok, containers, out);
        if let Some(tag) = &keys.tag
            && let Some(list) = buckets.by_tag.get(tag)
        {
            Self::push_matching(list, self, media_ok, containers, out);
        }
        if let Some(id) = keys.id
            && let Some(list) = buckets.by_id.get(id)
        {
            Self::push_matching(list, self, media_ok, containers, out);
        }
        for class in &keys.classes {
            if let Some(list) = buckets.by_class.get(*class) {
                Self::push_matching(list, self, media_ok, containers, out);
            }
        }
        // A rule lives in exactly one bucket, so there are no duplicates;
        // sorting restores the declaration order the cascade expects.
        out.sort_unstable_by_key(|&index| self.rules[index].seq);
    }

    fn push_matching(
        list: &[usize],
        index: &Self,
        media_ok: &[bool],
        containers: &ContainerContext,
        out: &mut Vec<usize>,
    ) {
        for &pos in list {
            if !media_ok[pos] {
                continue;
            }
            let rule = &index.rules[pos];
            if rule
                .containers
                .iter()
                .all(|(name, query)| container_query_matches(query, name.as_deref(), containers))
            {
                out.push(pos);
            }
        }
    }

    /// The compiled rule at `index`.
    pub(crate) fn rule(&self, index: usize) -> &CompiledRule {
        &self.rules[index]
    }

    /// Builds the `::before`/`::after` declarations for `node`.
    ///
    /// This replaces `browser_selectors::pseudo_elements_from_rules`, which
    /// re-parsed every selector in every sheet for every element. Declarations
    /// are in the same source order as the original collector.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pseudo_element_for(
        &self,
        kind: RuleKind,
        doc: &Document,
        node: NodeId,
        keys: &NodeKeys<'_>,
        media_ok: &[bool],
        containers: &ContainerContext,
        state: &ElementState,
        out: &mut Vec<usize>,
    ) -> Option<PseudoElement> {
        self.lookup(kind, keys, media_ok, containers, out);
        let mut declarations = Vec::new();
        for &index in out.iter() {
            let rule = &self.rules[index];
            if !matches_pseudo_element_host_with_state(doc, node, &rule.selector, state) {
                continue;
            }
            declarations.extend(rule.declarations.iter().cloned());
        }
        if declarations.is_empty() {
            return None;
        }
        let name = match kind {
            RuleKind::Before => "before",
            RuleKind::After => "after",
            RuleKind::Normal => unreachable!("pseudo_element_for requires a pseudo kind"),
        };
        Some(PseudoElement {
            name: name.to_string(),
            declarations,
        })
    }
}
