//! Diagnostics emitted when untrusted CSS is skipped by a safety limit.
//!
//! The parser and the style engine are deliberately infallible: a malformed or
//! oversized stylesheet never returns an error and never panics. Instead the
//! offending content is skipped and a [`ParseDiagnostic`] is recorded. In debug
//! builds the diagnostic is also printed to stderr so developers notice it
//! during local development.

/// Why a piece of CSS was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// The stylesheet text exceeded the byte budget and was truncated.
    InputSize,
    /// The stylesheet contained more rules than allowed.
    Rules,
    /// The stylesheet contained more declarations than allowed.
    Declarations,
    /// Nested at-rules (or selector blocks) were too deeply nested.
    NestingDepth,
    /// An `@import` chain was deeper than allowed.
    ImportDepth,
    /// The same `@import` URL was seen more than once.
    DuplicateImport,
}

/// A single note describing skipped CSS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    /// The category of limit that was hit.
    pub kind: DiagnosticKind,
    /// A human-readable explanation.
    pub message: String,
}

impl ParseDiagnostic {
    /// Creates a diagnostic.
    #[must_use]
    pub fn new(kind: DiagnosticKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Records a diagnostic and, in debug builds, prints it for developers.
pub fn push_diagnostic(
    diagnostics: &mut Vec<ParseDiagnostic>,
    kind: DiagnosticKind,
    message: impl Into<String>,
) {
    let message = message.into();
    #[cfg(debug_assertions)]
    eprintln!("[browser-css] {message}");
    diagnostics.push(ParseDiagnostic { kind, message });
}
