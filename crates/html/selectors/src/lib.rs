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
//!
//! # Supported pseudo-classes
//!
//! Structural: `:root`, `:empty`, `:first-child`, `:last-child`, `:only-child`,
//! `:nth-child()`, `:nth-last-child()` (including the `of S` clause),
//! `:first-of-type`, `:last-of-type`, `:only-of-type`, `:nth-of-type()`,
//! `:nth-last-of-type()`.
//!
//! Logical: `:not()`, `:is()`, `:where()`, `:has()`.
//!
//! Interactive / link: `:hover`, `:focus`, `:focus-visible`, `:focus-within`,
//! `:active`, `:visited`, `:link`, `:any-link`, `:target`.
//!
//! Form / input: `:checked`, `:disabled`, `:enabled`, `:valid`, `:invalid`,
//! `:required`, `:optional`, `:placeholder-shown`, `:read-only`, `:read-write`,
//! `:autofill`, `:user-invalid`.
//!
//! Other: `:lang()`, `:dir(ltr|rtl)`, `:defined`, `:host`,
//! `:host-context()`, `:modal`.
//!
//! Shadow DOM: `::slotted()` is matched with [`matches_slotted`] against
//! light-DOM children assigned to a slot; host selectors use [`ElementState`]
//! together with [`browser_dom::Document`]'s shadow helpers.
//!
//! Stateful pseudo-classes consult [`ElementState`]; the remainder are derived
//! from the document tree and attributes.

#![forbid(unsafe_code)]

mod matcher;
mod parser;
mod pseudo;

use std::fmt;

pub use matcher::{
    matches, matches_pseudo_element_host, matches_pseudo_element_host_with_state, matches_slotted,
    matches_slotted_with_state, matches_with_state, query_selector, query_selector_all,
    specificity,
};
pub use parser::{parse_selector, parse_selectors};
pub use pseudo::{
    PseudoElement, pseudo_element, pseudo_element_with_state, pseudo_elements,
    pseudo_elements_from_rules, pseudo_elements_with_state,
};

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

/// A CSS text directionality value, as accepted by `:dir()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Left-to-right.
    Ltr,
    /// Right-to-left.
    Rtl,
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
    /// `:nth-child(an+b of S)`. The position is taken among siblings that
    /// match at least one selector in `S`.
    NthChildOf(i32, i32, Vec<Selector>),
    /// `:nth-last-child(an+b of S)`. Like [`PseudoClass::NthChildOf`], but
    /// counted from the last matching sibling.
    NthLastChildOf(i32, i32, Vec<Selector>),
    /// `:nth-of-type(an+b)`.
    NthOfType(i32, i32),
    /// `:nth-last-of-type(an+b)`.
    NthLastOfType(i32, i32),
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
    /// `:has(<relative-selector-list>)`.
    Has(Vec<Selector>),
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
    /// `:target`.
    Target,
    /// `:lang(...)`.
    Lang(String),
    /// `:focus-visible`.
    FocusVisible,
    /// `:focus-within`. Matches when the element or a descendant has focus.
    FocusWithin,
    /// `:valid`.
    Valid,
    /// `:invalid`.
    Invalid,
    /// `:required`.
    Required,
    /// `:optional`.
    Optional,
    /// `:placeholder-shown`.
    PlaceholderShown,
    /// `:read-only`.
    ReadOnly,
    /// `:read-write`.
    ReadWrite,
    /// `:dir(ltr)` / `:dir(rtl)`.
    Dir(Direction),
    /// `:host`.
    Host,
    /// `:host-context(<selector-list>)`: the host or one of its light-tree
    /// ancestors matches the argument.
    HostContext(Vec<Selector>),
    /// `:defined`.
    Defined,
    /// `:modal`.
    Modal,
    /// `:autofill`.
    Autofill,
    /// `:user-invalid`.
    UserInvalid,
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
    /// The pseudo-element targeted by this selector, in canonical lowercase
    /// form (for example `before` or `after`), if any.
    ///
    /// A selector targeting a pseudo-element still matches its *host* element
    /// via [`matches_pseudo_element_host`]; it never matches through the
    /// ordinary [`matches`] function, because a pseudo-element is not a real
    /// DOM node.
    pub pseudo_element: Option<String>,
    /// Raw argument of a functional pseudo-element such as `::slotted(div)`.
    ///
    /// `None` for non-functional pseudo-elements and for selectors without a
    /// pseudo-element. The matcher parses this with [`parse_selectors`].
    pub pseudo_element_argument: Option<String>,
}

/// Dynamic state used when matching interactive pseudo-classes.
///
/// The style engine supplies one value per element. Every field defaults to a
/// neutral value through [`Default`], so existing callers that build a state
/// with `..ElementState::default()` keep working unchanged.
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
    /// `:target`. When false, the target is derived from the fragment of
    /// [`browser_dom::Document::base_url`] instead.
    pub target: bool,
    /// `:focus-visible`.
    pub focus_visible: bool,
    /// `:focus-within` for a *descendant* focus. The focused element itself
    /// matches `:focus-within` through [`ElementState::focus`] as well.
    pub focus_within: bool,
    /// Overrides form validity when set: `Some(true)` makes `:valid` match and
    /// `Some(false)` makes `:invalid` match. When absent, validity is derived
    /// from `required` and the control's current value.
    pub valid: Option<bool>,
    /// Whether a custom element is defined. Built-in elements (tag names
    /// without a hyphen) always match `:defined` regardless of this field.
    pub defined: bool,
    /// `:modal`. Set for a dialog shown with `showModal()`.
    pub modal: bool,
    /// `:autofill`.
    pub autofill: bool,
    /// `:user-invalid`, i.e. the user has interacted with an invalid control.
    pub user_invalid: bool,
    /// `:host`. Set when matching inside a shadow tree for its host.
    pub host: bool,
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
