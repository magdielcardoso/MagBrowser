//! The stylesheet AST produced by the parser.

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
}

/// A single rule in a stylesheet.
#[derive(Debug, Clone)]
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
}

/// A style rule: selectors (raw text) and declarations.
#[derive(Debug, Clone)]
pub struct StyleRule {
    /// Selector texts, split on top-level commas and trimmed.
    pub selectors: Vec<String>,
    /// Declarations inside the block.
    pub declarations: Vec<Declaration>,
}

/// An `@media` rule with its query and nested rules.
#[derive(Debug, Clone)]
pub struct MediaRule {
    /// The raw media query list text.
    pub query: String,
    /// Rules nested inside the media block.
    pub rules: Vec<Rule>,
}

/// An `@import` rule.
#[derive(Debug, Clone)]
pub struct ImportRule {
    /// The imported URL.
    pub url: String,
    /// Optional media query list restricting the import.
    pub media: Option<String>,
}

/// A `@keyframes` rule.
#[derive(Debug, Clone)]
pub struct KeyframesRule {
    /// The animation name.
    pub name: String,
    /// The keyframes in source order.
    pub keyframes: Vec<Keyframe>,
}

/// A single keyframe.
#[derive(Debug, Clone)]
pub struct Keyframe {
    /// Offset in the animation, from 0.0 (`from`) to 1.0 (`to`).
    pub offset: f32,
    /// Declarations applying at this offset.
    pub declarations: Vec<Declaration>,
}

/// An `@font-face` rule.
#[derive(Debug, Clone)]
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
