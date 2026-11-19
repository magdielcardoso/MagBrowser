//! Evaluation of the conditional at-rules the cascade understands.
//!
//! The CSS parser exposes structured condition trees for `@supports` and
//! `@container`; this module turns those trees into booleans using the
//! engine's own idea of what a supported property/value is and the container
//! size supplied by the caller. It also builds the `@layer` order and matches
//! `@scope` roots/limits against the document.
//!
//! Everything here is deliberately conservative: an expression the engine
//! cannot evaluate evaluates to `false`, so a rule is included only when the
//! engine is sure the condition holds.

use std::collections::HashMap;

use browser_css::{
    ComponentValue, ContainerQuery, ContainerSizeFeature, Rule, SizeOperator, SupportsCondition,
    Value,
};
use browser_dom::{Document, NodeId};
use browser_selectors::{ElementState, Selector, parse_selector};

use crate::cascade::UNLAYERED;
use crate::interpret::{InterpretContext, interpret};
use crate::properties::Property;

// ---------------------------------------------------------------------------
// @supports
// ---------------------------------------------------------------------------

/// Evaluates a parsed `@supports` condition against this engine's own set of
/// supported properties and values.
///
/// A declaration test is true when the property is known and the value can be
/// interpreted by the engine (custom properties are always accepted, as CSS
/// requires). Selector tests use the real selector parser. A general-enclosed
/// function is unsupported; arbitrary parenthesised tokens are supported.
pub fn supports_condition_matches(condition: &SupportsCondition) -> bool {
    match condition {
        SupportsCondition::Declaration(declaration) => supports_declaration(declaration),
        SupportsCondition::Selector(text) => parse_selector(text).is_ok(),
        SupportsCondition::Not(inner) => !supports_condition_matches(inner),
        SupportsCondition::And(parts) => {
            !parts.is_empty() && parts.iter().all(supports_condition_matches)
        }
        SupportsCondition::Or(parts) => parts.iter().any(supports_condition_matches),
        SupportsCondition::GeneralEnclosed(text) => {
            // `<general-enclosed>` that is a function token is unsupported;
            // a plain parenthesised group of arbitrary tokens is supported.
            !is_function_token(text)
        }
    }
}

fn supports_declaration(declaration: &browser_css::Declaration) -> bool {
    if declaration.name.starts_with("--") {
        return true;
    }
    let Some(property) = Property::from_name(&declaration.name) else {
        return false;
    };
    interpret(property, &declaration.value, &InterpretContext::default()).is_some()
}

fn is_function_token(text: &str) -> bool {
    let text = text.trim();
    let Some(open) = text.find('(') else {
        return false;
    };
    let name = text[..open].trim();
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        && text.trim_end().ends_with(')')
}

// ---------------------------------------------------------------------------
// @container
// ---------------------------------------------------------------------------

/// The width and height of a query container, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContainerSize {
    /// The container's inline/viewport-relative width.
    pub width: f32,
    /// The container's block height.
    pub height: f32,
}

impl Default for ContainerSize {
    fn default() -> Self {
        Self {
            width: 1024.0,
            height: 768.0,
        }
    }
}

/// Container sizes available while computing a tree.
///
/// `size` is the nearest query container for an element that has no entry in
/// the per-element map used by
/// [`StyleEngine::compute_with_container`](crate::StyleEngine::compute_with_container);
/// `named` holds named query containers that `@container foo (…)` can select.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContainerContext {
    /// The default (unnamed) query container size.
    pub size: ContainerSize,
    /// Sizes of named query containers, keyed by container name.
    pub named: HashMap<String, ContainerSize>,
}

impl ContainerContext {
    /// Creates a context with a single, unnamed container size.
    #[must_use]
    pub fn new(size: ContainerSize) -> Self {
        Self {
            size,
            named: HashMap::new(),
        }
    }

    /// Registers a named container and returns the updated context.
    #[must_use]
    pub fn with_named(mut self, name: impl Into<String>, size: ContainerSize) -> Self {
        self.named.insert(name.into(), size);
        self
    }
}

/// Evaluates a parsed `@container` query.
///
/// `name` is the optional container name from the rule prelude. A named query
/// with no matching registered container is false. Style queries and unknown
/// features are also false; the engine only evaluates size features.
pub fn container_query_matches(
    query: &ContainerQuery,
    name: Option<&str>,
    ctx: &ContainerContext,
) -> bool {
    let size = match name {
        Some(name) => match ctx.named.get(name) {
            Some(size) => *size,
            None => return false,
        },
        None => ctx.size,
    };
    container_query_matches_size(query, size)
}

fn container_query_matches_size(query: &ContainerQuery, size: ContainerSize) -> bool {
    match query {
        ContainerQuery::Size(feature) => size_feature_matches(feature, size),
        ContainerQuery::Style(_) => false,
        ContainerQuery::Not(inner) => !container_query_matches_size(inner, size),
        ContainerQuery::And(parts) => {
            !parts.is_empty() && parts.iter().all(|q| container_query_matches_size(q, size))
        }
        ContainerQuery::Or(parts) => parts.iter().any(|q| container_query_matches_size(q, size)),
        ContainerQuery::GeneralEnclosed(_) => false,
    }
}

/// Evaluates one size feature such as `(min-width: 400px)` or
/// `(400px <= width <= 700px)`.
fn size_feature_matches(feature: &ContainerSizeFeature, size: ContainerSize) -> bool {
    let (axis, is_min, is_max) = match feature.name.to_ascii_lowercase().as_str() {
        "width" | "inline-size" => (size.width, false, false),
        "height" | "block-size" => (size.height, false, false),
        "min-width" | "min-inline-size" => (size.width, true, false),
        "min-height" | "min-block-size" => (size.height, true, false),
        "max-width" | "max-inline-size" => (size.width, false, true),
        "max-height" | "max-block-size" => (size.height, false, true),
        _ => return false,
    };

    // `(min-width: 400px)` / `(max-width: 400px)` are colon operators with a
    // single operand; they fold into the corresponding comparison.
    if is_min || is_max {
        if feature.values.len() != 1 || feature.operators.len() != 1 {
            return false;
        }
        let operand = match container_length(&feature.values[0], axis) {
            Some(operand) => operand,
            None => return false,
        };
        return if is_min {
            axis >= operand
        } else {
            axis <= operand
        };
    }

    match (feature.operators.as_slice(), feature.values.as_slice()) {
        ([operator], [value]) => {
            let operand = match container_length(value, axis) {
                Some(operand) => operand,
                None => return false,
            };
            compare(*operator, axis, operand)
        }
        ([left, right], [low, high]) => {
            let (low, high) = match (container_length(low, axis), container_length(high, axis)) {
                (Some(low), Some(high)) => (low, high),
                _ => return false,
            };
            // The parser stores each operator with the operand order from the
            // source: value[0] OP0 feature and feature OP1 value[1].
            compare(*left, low, axis) && compare(*right, axis, high)
        }
        _ => false,
    }
}

fn compare(operator: SizeOperator, left: f32, right: f32) -> bool {
    match operator {
        SizeOperator::GreaterThan => left > right,
        SizeOperator::GreaterThanOrEqual => left >= right,
        SizeOperator::LessThan => left < right,
        SizeOperator::LessThanOrEqual => left <= right,
        SizeOperator::Equal => (left - right).abs() < 0.5,
        SizeOperator::Colon => false,
    }
}

/// Parses a single absolute length (or a percentage of `axis`).
fn container_length(text: &str, axis: f32) -> Option<f32> {
    let value = Value::parse(text.trim());
    let component = value
        .components
        .iter()
        .find(|component| !matches!(component, ComponentValue::Whitespace))?;
    match component {
        ComponentValue::Number(n) if *n == 0.0 => Some(0.0),
        ComponentValue::Percentage(p) => Some(axis * *p / 100.0),
        ComponentValue::Dimension(n, unit) => {
            let n = *n as f32;
            let px = match unit.to_ascii_lowercase().as_str() {
                "px" => n,
                "in" => n * 96.0,
                "cm" => n * 96.0 / 2.54,
                "mm" => n * 96.0 / 25.4,
                "q" => n * 96.0 / 101.6,
                "pt" => n * 96.0 / 72.0,
                "pc" => n * 16.0,
                _ => return None,
            };
            Some(px)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// @layer
// ---------------------------------------------------------------------------

/// The order of named layers, assigned from first appearance across the
/// engine's stylesheets. Later layers win; unlayered rules win over all
/// layers (for normal declarations).
#[derive(Debug, Clone, Default)]
pub struct LayerOrder {
    ranks: HashMap<String, u32>,
}

impl LayerOrder {
    /// Builds the layer order from a rule list, recursing through conditional
    /// and scoping at-rules in source order.
    pub fn from_rules<'a>(sheets: impl IntoIterator<Item = &'a [Rule]>) -> Self {
        let mut order = Self::default();
        let mut anonymous = 0usize;
        for rules in sheets {
            for rule in rules {
                order.visit(rule, "", &mut anonymous);
            }
        }
        order
    }

    fn visit(&mut self, rule: &Rule, prefix: &str, anonymous: &mut usize) {
        match rule {
            Rule::LayerStatement(statement) => {
                for name in &statement.names {
                    self.register(&qualify(prefix, name));
                    // A dotted name implies its parent layers, in order.
                    let mut parts: Vec<&str> = name.split('.').collect();
                    while parts.len() > 1 {
                        parts.pop();
                        self.register(&qualify(prefix, &parts.join(".")));
                    }
                }
            }
            Rule::LayerBlock(block) => {
                *anonymous += 1;
                let own = block
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("\0anon{anonymous}"));
                let full = qualify(prefix, &own);
                self.register(&full);
                for child in &block.rules {
                    self.visit(child, &full, anonymous);
                }
            }
            Rule::Media(rule) => {
                for child in &rule.rules {
                    self.visit(child, prefix, anonymous);
                }
            }
            Rule::Supports(rule) => {
                if supports_condition_matches(&rule.condition) {
                    for child in &rule.rules {
                        self.visit(child, prefix, anonymous);
                    }
                }
            }
            Rule::Container(rule) => {
                for child in &rule.rules {
                    self.visit(child, prefix, anonymous);
                }
            }
            Rule::Scope(rule) => {
                for child in &rule.rules {
                    self.visit(child, prefix, anonymous);
                }
            }
            Rule::StartingStyle(rule) => {
                for child in &rule.rules {
                    self.visit(child, prefix, anonymous);
                }
            }
            _ => {}
        }
    }

    fn register(&mut self, name: &str) {
        if !self.ranks.contains_key(name) {
            let rank = self.ranks.len() as u32 + 1;
            self.ranks.insert(name.to_string(), rank);
        }
    }

    /// The priority of a layer path. `None` means an unlayered rule.
    ///
    /// Values are capped so they fit in the packed cascade order field.
    #[must_use]
    pub fn rank(&self, path: &str) -> u32 {
        match self.ranks.get(path) {
            Some(&rank) => rank.min(UNLAYERED - 1),
            None => UNLAYERED,
        }
    }
}

fn qualify(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

// ---------------------------------------------------------------------------
// @scope
// ---------------------------------------------------------------------------

/// Compiled root and limit selectors of an `@scope` rule.
#[derive(Debug, Clone)]
pub struct ScopeMatcher {
    root: Option<Selector>,
    limit: Option<Selector>,
}

impl ScopeMatcher {
    /// Compiles the raw selectors. An unparseable selector is treated as
    /// absent so the rest of the rule can still apply.
    #[must_use]
    pub fn new(root: Option<&str>, limit: Option<&str>) -> Self {
        Self {
            root: root.and_then(|text| parse_selector(text).ok()),
            limit: limit.and_then(|text| parse_selector(text).ok()),
        }
    }

    /// Returns the scope proximity (larger = closer root) when `node` lies
    /// inside the scope, or `None` when it is outside.
    #[must_use]
    pub fn proximity(&self, doc: &Document, node: NodeId, state: &ElementState) -> Option<u32> {
        let root = self.root_ancestor(doc, node, state);
        if let Some(root) = root {
            if root != node && !doc.ancestors(node).any(|ancestor| ancestor == root) {
                return None;
            }
        } else if self.root.is_some() {
            return None;
        }

        if let Some(limit) = &self.limit {
            // The limit element and its subtree are excluded. A limit that is
            // outside the root's subtree does not constrain anything.
            for ancestor in std::iter::once(node).chain(doc.ancestors(node)) {
                if browser_selectors::matches_with_state(doc, ancestor, limit, state) {
                    let outside_root = root
                        .is_some_and(|r| ancestor != r && !doc.ancestors(ancestor).any(|a| a == r));
                    if root.is_none() || !outside_root {
                        return None;
                    }
                }
            }
        }

        let distance = root.map_or(0, |r| {
            std::iter::once(node)
                .chain(doc.ancestors(node))
                .take_while(|&current| current != r)
                .count()
        });
        Some(
            (u16::MAX as u32)
                .saturating_sub(distance.min(u16::MAX as usize) as u32)
                .max(1),
        )
    }

    fn root_ancestor(&self, doc: &Document, node: NodeId, state: &ElementState) -> Option<NodeId> {
        let root = self.root.as_ref()?;
        std::iter::once(node)
            .chain(doc.ancestors(node))
            .find(|&candidate| browser_selectors::matches_with_state(doc, candidate, root, state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_css::Stylesheet;

    fn value(text: &str) -> Value {
        Value::parse(text)
    }

    fn declaration(name: &str, text: &str) -> browser_css::Declaration {
        browser_css::Declaration {
            name: name.to_string(),
            value: value(text),
            important: false,
        }
    }

    #[test]
    fn supports_declarations_use_engine_support() {
        assert!(supports_condition_matches(&SupportsCondition::Declaration(
            declaration("display", "grid")
        )));
        assert!(supports_condition_matches(&SupportsCondition::Declaration(
            declaration("--custom", "anything")
        )));
        assert!(!supports_condition_matches(
            &SupportsCondition::Declaration(declaration("not-a-property", "x"))
        ));
        assert!(!supports_condition_matches(
            &SupportsCondition::Declaration(declaration("display", "not-a-display-value"))
        ));
    }

    #[test]
    fn supports_logical_operators() {
        let yes = SupportsCondition::Declaration(declaration("color", "red"));
        let no = SupportsCondition::Declaration(declaration("no-such-property", "x"));
        assert!(supports_condition_matches(&SupportsCondition::Not(
            Box::new(no.clone())
        )));
        assert!(supports_condition_matches(&SupportsCondition::And(vec![
            yes.clone(),
            yes.clone()
        ])));
        assert!(!supports_condition_matches(&SupportsCondition::And(vec![
            yes.clone(),
            no.clone()
        ])));
        assert!(supports_condition_matches(&SupportsCondition::Or(vec![
            no, yes
        ])));
    }

    #[test]
    fn supports_general_enclosed_functions_are_unsupported() {
        assert!(!supports_condition_matches(
            &SupportsCondition::GeneralEnclosed("font-tech(color-COLRv1)".to_string())
        ));
        assert!(supports_condition_matches(
            &SupportsCondition::GeneralEnclosed("foo bar".to_string())
        ));
    }

    fn feature(name: &str, operators: &[SizeOperator], values: &[&str]) -> ContainerSizeFeature {
        ContainerSizeFeature {
            name: name.to_string(),
            operators: operators.to_vec(),
            values: values.iter().map(|text| (*text).to_string()).collect(),
        }
    }

    #[test]
    fn container_min_max_and_comparisons() {
        let size = ContainerSize {
            width: 500.0,
            height: 300.0,
        };
        assert!(container_query_matches_size(
            &ContainerQuery::Size(feature("min-width", &[SizeOperator::Colon], &["400px"])),
            size
        ));
        assert!(!container_query_matches_size(
            &ContainerQuery::Size(feature("max-width", &[SizeOperator::Colon], &["400px"])),
            size
        ));
        assert!(container_query_matches_size(
            &ContainerQuery::Size(feature("width", &[SizeOperator::GreaterThan], &["400px"])),
            size
        ));
        assert!(!container_query_matches_size(
            &ContainerQuery::Size(feature("height", &[SizeOperator::GreaterThan], &["300px"])),
            size
        ));
    }

    #[test]
    fn container_range_queries() {
        let size = ContainerSize {
            width: 500.0,
            height: 300.0,
        };
        let range = ContainerQuery::Size(feature(
            "width",
            &[SizeOperator::LessThanOrEqual, SizeOperator::LessThanOrEqual],
            &["400px", "600px"],
        ));
        assert!(container_query_matches_size(&range, size));
        let outside = ContainerQuery::Size(feature(
            "width",
            &[SizeOperator::LessThan, SizeOperator::LessThan],
            &["600px", "700px"],
        ));
        assert!(!container_query_matches_size(&outside, size));
    }

    #[test]
    fn named_containers_are_selected_by_name() {
        let query = ContainerQuery::Size(feature("width", &[SizeOperator::GreaterThan], &["10px"]));
        let ctx = ContainerContext::new(ContainerSize::default()).with_named(
            "sidebar",
            ContainerSize {
                width: 200.0,
                height: 100.0,
            },
        );
        assert!(container_query_matches(&query, Some("sidebar"), &ctx));
        assert!(!container_query_matches(&query, Some("missing"), &ctx));
    }

    #[test]
    fn layer_order_registers_first_appearance() {
        let sheet = |css: &str| Stylesheet::parse(css, browser_css::Origin::Author);
        let a = sheet("@layer reset, theme;");
        let b = sheet("@layer base { p {} }");
        let order = LayerOrder::from_rules([a.rules.as_slice(), b.rules.as_slice()]);
        assert!(order.rank("reset") < order.rank("theme"));
        assert!(order.rank("theme") < order.rank("base"));
        assert_eq!(order.rank("unknown"), UNLAYERED);
    }
}
