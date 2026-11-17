//! CSS selector parsing and matching over the DOM.
//!
//! Selectors are parsed into [`Selector`] values and matched against DOM nodes
//! from [`browser_dom`]. Matching is performed right-to-left, as in a real
//! engine, and respects HTML case-insensitivity for tag and attribute names.
//!
//! ```
//! use browser_dom::Document;
//! use browser_selectors::{matches, parse_selector};
//!
//! let mut doc = Document::new();
//! let html = doc.create_element("html");
//! let div = doc.create_element("div");
//! doc.append_child(doc.root(), html);
//! doc.append_child(html, div);
//! doc.set_attr(div, "class", "box");
//!
//! let selector = parse_selector("div.box").unwrap();
//! assert!(matches(&doc, div, &selector));
//! ```

#![forbid(unsafe_code)]

mod matcher;
mod parser;

use std::fmt;

pub use matcher::{matches, matches_with_state, query_selector, query_selector_all, specificity};
pub use parser::{parse_selector, parse_selectors};

/// A combinator joining two compound selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// Whitespace: any ancestor.
    Descendant,
    /// `>`: the parent element.
    Child,
    /// `+`: the immediately preceding element sibling.
    NextSibling,
    /// `~`: any preceding element sibling.
    SubsequentSibling,
}

/// Selector specificity, ordered by id, class and type counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Specificity {
    /// Number of id selectors.
    pub a: u32,
    /// Number of class selectors, attribute selectors and pseudo-classes.
    pub b: u32,
    /// Number of type selectors and pseudo-elements.
    pub c: u32,
}

impl Specificity {
    /// Creates a specificity from its three components.
    #[must_use]
    pub const fn new(a: u32, b: u32, c: u32) -> Self {
        Self { a, b, c }
    }

    /// Adds two specificities component-wise.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub const fn add(self, other: Self) -> Self {
        Self {
            a: self.a.saturating_add(other.a),
            b: self.b.saturating_add(other.b),
            c: self.c.saturating_add(other.c),
        }
    }
}

/// The operation of an attribute selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrOp {
    /// `[attr]`.
    Exists,
    /// `[attr=value]`.
    Equals,
    /// `[attr~=value]`.
    Includes,
    /// `[attr^=value]`.
    Prefix,
    /// `[attr$=value]`.
    Suffix,
    /// `[attr*=value]`.
    Substring,
    /// `[attr|=value]`.
    DashMatch,
}

/// A pseudo-class selector.
#[derive(Debug, Clone, PartialEq)]
pub enum PseudoClass {
    /// `:first-child`.
    FirstChild,
    /// `:last-child`.
    LastChild,
    /// `:only-child`.
    OnlyChild,
    /// `:nth-child(an+b)`.
    NthChild(i32, i32),
    /// `:nth-last-child(an+b)`.
    NthLastChild(i32, i32),
    /// `:root`.
    Root,
    /// `:empty`.
    Empty,
    /// `:first-of-type`.
    FirstOfType,
    /// `:last-of-type`.
    LastOfType,
    /// `:only-of-type`.
    OnlyOfType,
    /// `:not(...)`.
    Not(Vec<Selector>),
    /// `:is(...)`.
    Is(Vec<Selector>),
    /// `:where(...)`.
    Where(Vec<Selector>),
    /// `:hover`.
    Hover,
    /// `:focus`.
    Focus,
    /// `:active`.
    Active,
    /// `:link`.
    Link,
    /// `:visited`.
    Visited,
    /// `:checked`.
    Checked,
    /// `:disabled`.
    Disabled,
    /// `:enabled`.
    Enabled,
    /// `:any-link`.
    AnyLink,
    /// `:lang(...)`.
    Lang(String),
}

/// A simple selector inside a compound selector.
#[derive(Debug, Clone, PartialEq)]
pub enum SimpleSelector {
    /// `*`.
    Universal,
    /// `div`.
    Type(String),
    /// `#id`.
    Id(String),
    /// `.class`.
    Class(String),
    /// `[attr op value]`.
    Attribute {
        /// Lowercased attribute name.
        name: String,
        /// The matching operation.
        op: AttrOp,
        /// The expected value, absent for [`AttrOp::Exists`].
        value: Option<String>,
        /// True for the `i` case-insensitivity flag.
        case_insensitive: bool,
    },
    /// A pseudo-class.
    Pseudo(PseudoClass),
    /// A pseudo-element such as `::before`; never matches a real element.
    PseudoElement(String),
}

/// A compound selector: simple selectors with no combinators between them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CompoundSelector {
    /// The simple selectors, in source order.
    pub simples: Vec<SimpleSelector>,
}

/// A complete complex selector.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// Compounds paired with the combinator that precedes them; the first
    /// entry has no combinator.
    pub parts: Vec<(Option<Combinator>, CompoundSelector)>,
    /// The specificity of the whole selector.
    pub specificity: Specificity,
    /// The raw selector text, trimmed.
    pub text: String,
}

/// Dynamic state used when matching interactive pseudo-classes.
#[derive(Debug, Clone, Default)]
pub struct ElementState {
    /// `:hover`.
    pub hover: bool,
    /// `:focus`.
    pub focus: bool,
    /// `:active`.
    pub active: bool,
    /// `:visited`.
    pub visited: bool,
    /// Overrides `:checked` when set.
    pub checked: Option<bool>,
    /// `:disabled`.
    pub disabled: bool,
}

/// An error produced while parsing a selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectorError {
    /// A human-readable description.
    pub message: String,
    /// Byte position in the parsed input where the error occurred.
    pub position: usize,
}

impl fmt::Display for SelectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at position {}", self.message, self.position)
    }
}

impl std::error::Error for SelectorError {}
