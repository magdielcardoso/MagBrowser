//! Token types produced by the HTML tokenizer.

/// A lexical token produced by [`Tokenizer`](crate::Tokenizer).
///
/// Tag and attribute names are lowercased, as required for HTML documents.
/// Character data is normally delivered as a coalesced [`Token::Text`] run;
/// [`Token::Character`] exists for consumers that prefer single characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// A `<!DOCTYPE ...>` declaration.
    Doctype {
        /// Document type name, lowercased.
        name: String,
        /// Legacy public identifier, empty when absent.
        public_id: String,
        /// Legacy system identifier, empty when absent.
        system_id: String,
    },
    /// A start tag such as `<div class="box">`.
    StartTag {
        /// Lowercased tag name.
        name: String,
        /// Attributes in source order, names lowercased.
        attributes: Vec<(String, String)>,
        /// Whether the tag ended with `/>`.
        self_closing: bool,
    },
    /// An end tag such as `</div>`.
    EndTag {
        /// Lowercased tag name.
        name: String,
    },
    /// A single character of text.
    Character(char),
    /// A coalesced run of character data.
    Text(String),
    /// A comment, including bogus comments.
    Comment(String),
    /// End of input.
    Eof,
}
