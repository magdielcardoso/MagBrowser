//! The stylesheet AST produced by the parser.

use crate::diagnostics::ParseDiagnostic;
use crate::values::Value;

/// Where a stylesheet came from; used by the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Origin {
    /// An author stylesheet (the default).
    #[default]
    Author,
    /// The user-agent (browser default) stylesheet.
    UserAgent,
    /// A user stylesheet.
    User,
}

/// A parsed stylesheet.
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    /// Top-level rules in source order.
    pub rules: Vec<Rule>,
    /// The origin of the stylesheet.
    pub origin: Origin,
    /// Notes describing content that was skipped by a parse limit.
    ///
    /// This is normally empty; it is populated when input exceeds one of the
    /// safety limits in [`crate::limits`].
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// A single rule in a stylesheet.
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    /// A qualified style rule.
    Style(StyleRule),
    /// An `@media` rule.
    Media(MediaRule),
    /// An `@import` rule.
    Import(ImportRule),
    /// An `@keyframes` rule.
    Keyframes(KeyframesRule),
    /// An `@font-face` rule.
    FontFace(FontFaceRule),
    /// An `@supports` rule.
    Supports(SupportsRule),
    /// An `@container` rule.
    Container(ContainerRule),
    /// A statement-form `@layer` rule (`@layer a, b;`).
    LayerStatement(LayerStatementRule),
    /// A block-form `@layer` rule (`@layer a { ... }`).
    LayerBlock(LayerBlockRule),
    /// An `@scope` rule.
    Scope(ScopeRule),
    /// An `@starting-style` rule.
    StartingStyle(StartingStyleRule),
    /// An `@property` rule.
    Property(PropertyRule),
}

/// A style rule: selectors (raw text) and declarations.
#[derive(Debug, Clone, PartialEq)]
pub struct StyleRule {
    /// Selector texts, split on top-level commas and trimmed.
    pub selectors: Vec<String>,
    /// Declarations inside the block.
    pub declarations: Vec<Declaration>,
}

/// An `@media` rule with its query and nested rules.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaRule {
    /// The raw media query list text.
    pub query: String,
    /// Rules nested inside the media block.
    pub rules: Vec<Rule>,
}

/// An `@import` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRule {
    /// The imported URL.
    pub url: String,
    /// Optional media query list restricting the import.
    pub media: Option<String>,
}

/// A `@keyframes` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframesRule {
    /// The animation name.
    pub name: String,
    /// The keyframes in source order.
    pub keyframes: Vec<Keyframe>,
}

/// A single keyframe.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyframe {
    /// Offset in the animation, from 0.0 (`from`) to 1.0 (`to`).
    pub offset: f32,
    /// Declarations applying at this offset.
    pub declarations: Vec<Declaration>,
}

/// An `@font-face` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct FontFaceRule {
    /// Font descriptors declared inside the block.
    pub declarations: Vec<Declaration>,
}

/// A single declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// Lowercased property name (custom properties keep their case).
    pub name: String,
    /// The parsed value.
    pub value: Value,
    /// True when the declaration was marked `!important`.
    pub important: bool,
}

impl Declaration {
    /// The component values of the declaration value.
    #[must_use]
    pub fn tokens(&self) -> &[crate::values::ComponentValue] {
        &self.value.components
    }
}

// ---------------------------------------------------------------------------
// Modern conditional / descriptor at-rules
// ---------------------------------------------------------------------------

/// An `@supports` rule: a feature condition plus a nested rule list.
#[derive(Debug, Clone, PartialEq)]
pub struct SupportsRule {
    /// The parsed feature condition.
    pub condition: SupportsCondition,
    /// Rules nested inside the supports block.
    pub rules: Vec<Rule>,
}

/// A boolean condition used by [`SupportsRule`].
///
/// The variants mirror the CSS `<supports-condition>` grammar so a style engine
/// can evaluate the expression without re-parsing raw text.
#[derive(Debug, Clone, PartialEq)]
pub enum SupportsCondition {
    /// A declaration test such as `(display: grid)`.
    Declaration(Declaration),
    /// A selector test such as `selector(:has(*))`; holds the raw selector.
    Selector(String),
    /// `not <supports-in-parens>`.
    Not(Box<SupportsCondition>),
    /// Two or more conditions joined by `and`.
    And(Vec<SupportsCondition>),
    /// Two or more conditions joined by `or`.
    Or(Vec<SupportsCondition>),
    /// An unrecognised `<general-enclosed>` construct, stored verbatim.
    ///
    /// Kept so callers can apply their own policy (CSS treats unknown
    /// functions as unsupported and arbitrary parenthesised tokens as
    /// supported).
    GeneralEnclosed(String),
}

/// An `@container` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerRule {
    /// The optional container name used to select a query container.
    pub name: Option<String>,
    /// The container condition, after the name has been extracted.
    pub query: ContainerQuery,
    /// Rules nested inside the container block.
    pub rules: Vec<Rule>,
}

/// A condition used by [`ContainerRule`].
#[derive(Debug, Clone, PartialEq)]
pub enum ContainerQuery {
    /// A size feature test such as `(width >= 400px)`.
    Size(ContainerSizeFeature),
    /// A style query such as `style(--theme: dark)`; holds the raw feature.
    Style(String),
    /// `not <size-query>`.
    Not(Box<ContainerQuery>),
    /// Two or more queries joined by `and`.
    And(Vec<ContainerQuery>),
    /// Two or more queries joined by `or`.
    Or(Vec<ContainerQuery>),
    /// An unrecognised `<general-enclosed>` construct, stored verbatim.
    GeneralEnclosed(String),
}

/// A single size feature inside a [`ContainerQuery`].
///
/// A simple comparison has one operator and one value; a range query such as
/// `(400px <= width <= 700px)` has two of each. The operator at index `i`
/// relates `values[i]` to the feature name (the final operator relates the
/// name to `values[1]` in a two-valued range).
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerSizeFeature {
    /// Feature name, e.g. `width`, `inline-size` or `min-width`.
    pub name: String,
    /// Comparison operators, left to right.
    pub operators: Vec<SizeOperator>,
    /// Operand values as source text, left to right.
    pub values: Vec<String>,
}

impl ContainerSizeFeature {
    /// True when this is a range query with two operands.
    #[must_use]
    pub fn is_range(&self) -> bool {
        self.values.len() > 1
    }
}

/// A comparison operator in a [`ContainerSizeFeature`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeOperator {
    /// `:`, as in `(min-width: 400px)`.
    Colon,
    /// `>`.
    GreaterThan,
    /// `>=`.
    GreaterThanOrEqual,
    /// `<`.
    LessThan,
    /// `<=`.
    LessThanOrEqual,
    /// `=`.
    Equal,
}

impl SizeOperator {
    /// The source spelling of the operator.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Colon => ":",
            Self::GreaterThan => ">",
            Self::GreaterThanOrEqual => ">=",
            Self::LessThan => "<",
            Self::LessThanOrEqual => "<=",
            Self::Equal => "=",
        }
    }
}

/// A statement-form `@layer` rule, e.g. `@layer reset, base, theme;`.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerStatementRule {
    /// Layer names in declaration order; dotted names are kept verbatim.
    pub names: Vec<String>,
}

/// A block-form `@layer` rule, e.g. `@layer base { ... }`.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerBlockRule {
    /// The layer name, or `None` for an anonymous layer.
    pub name: Option<String>,
    /// Rules nested inside the layer block.
    pub rules: Vec<Rule>,
}

impl LayerStatementRule {
    /// Each layer name split into its dotted segments.
    #[must_use]
    pub fn segments(&self) -> Vec<Vec<&str>> {
        self.names.iter().map(|name| layer_segments(name)).collect()
    }
}

impl LayerBlockRule {
    /// The dotted segments of the layer name (empty for an anonymous layer).
    #[must_use]
    pub fn segments(&self) -> Vec<&str> {
        self.name.as_deref().map(layer_segments).unwrap_or_default()
    }
}

/// Splits a layer name such as `base.theme` into `["base", "theme"]`.
#[must_use]
pub fn layer_segments(name: &str) -> Vec<&str> {
    name.split('.')
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// An `@scope` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeRule {
    /// The scope root selector (the `(.root)` part), if present.
    pub root: Option<String>,
    /// The scope limit selector (the `to (.limit)` part), if present.
    pub limit: Option<String>,
    /// Rules nested inside the scope block.
    pub rules: Vec<Rule>,
}

/// An `@starting-style` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct StartingStyleRule {
    /// Rules describing the starting styles of their elements.
    pub rules: Vec<Rule>,
}

/// An `@property` rule registering a custom property.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyRule {
    /// The registered custom property name, e.g. `--my-color`.
    pub name: String,
    /// The `syntax` descriptor, if present.
    pub syntax: Option<String>,
    /// The `inherits` descriptor, if present.
    pub inherits: Option<bool>,
    /// The `initial-value` descriptor, if present.
    pub initial_value: Option<Value>,
    /// Every descriptor as a parsed declaration, in source order.
    pub descriptors: Vec<Declaration>,
}
