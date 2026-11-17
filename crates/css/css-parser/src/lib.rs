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

#![forbid(unsafe_code)]

mod ast;
mod declarations;
mod parser;
pub mod tokenizer;
mod values;

pub use ast::{
    Declaration, FontFaceRule, ImportRule, Keyframe, KeyframesRule, MediaRule, Origin, Rule,
    StyleRule, Stylesheet,
};
pub use declarations::{parse_declarations, parse_inline_style};
pub use tokenizer::Token;
pub use values::{Color, ComponentValue, Length, Value};
