//! CSS tokenizer, parser and typed stylesheet model.
//!
//! The crate parses a stylesheet into [`Stylesheet`] without ever failing:
//! malformed rules and declarations are skipped individually so a single bad
//! rule cannot discard the rest of the sheet.
//!
//! ```
//! use browser_css::{Origin, Rule, Stylesheet};
//!
//! let sheet = Stylesheet::parse("p { color: red; }", Origin::Author);
//! let Rule::Style(rule) = &sheet.rules[0] else {
//!     panic!("expected a style rule");
//! };
//! assert_eq!(rule.selectors, ["p"]);
//! assert_eq!(rule.declarations[0].value.ident(), Some("red"));
//! ```
//!
//! Besides style rules, the AST exposes structured nodes for `@media`,
//! `@import`, `@keyframes`, `@font-face`, `@supports`, `@container`, `@layer`,
//! `@scope`, `@starting-style` and `@property`. Conditional at-rules keep
//! parsed condition trees so a style engine can evaluate them without
//! re-parsing source text.

#![forbid(unsafe_code)]

mod ast;
mod at_rules;
mod declarations;
pub mod diagnostics;
pub mod limits;
mod parser;
pub mod tokenizer;
mod values;

pub use ast::{
    ContainerQuery, ContainerRule, ContainerSizeFeature, Declaration, FontFaceRule, ImportRule,
    Keyframe, KeyframesRule, LayerBlockRule, LayerStatementRule, MediaRule, Origin, PropertyRule,
    Rule, ScopeRule, SizeOperator, StartingStyleRule, StyleRule, Stylesheet, SupportsCondition,
    SupportsRule, layer_segments,
};
pub use declarations::{parse_declarations, parse_inline_style};
pub use diagnostics::{DiagnosticKind, ParseDiagnostic, push_diagnostic};
pub use limits::{Limits, MAX_DECLARATIONS, MAX_NESTING_DEPTH, MAX_RULES, MAX_STYLESHEET_BYTES};
pub use tokenizer::Token;
pub use values::{Color, ComponentValue, Length, Value};
