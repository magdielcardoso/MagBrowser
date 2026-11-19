//! Hard limits applied while parsing untrusted CSS.
//!
//! Real pages ship megabytes of CSS with deeply nested rules. Parsing all of
//! it would block the UI thread, so the parser works with a shared
//! [`ParseBudget`] and stops (recording a diagnostic) as soon as a limit is
//! reached. The defaults are deliberately generous for real stylesheets but
//! small enough to bound worst-case work.

use crate::diagnostics::{DiagnosticKind, ParseDiagnostic, push_diagnostic};

/// Maximum number of bytes accepted from a single stylesheet (16 MiB).
/// Large real sites (YouTube, news portals) ship several MiB of CSS.
pub const MAX_STYLESHEET_BYTES: usize = 16 * 1024 * 1024;

/// Maximum number of rules (including rules nested in at-rules) per sheet.
pub const MAX_RULES: usize = 200_000;

/// Maximum number of declarations per sheet.
pub const MAX_DECLARATIONS: usize = 2_000_000;

/// Maximum nesting depth of at-rules / selector blocks.
pub const MAX_NESTING_DEPTH: usize = 64;

/// The numeric limits used by one parse. Callers can override them with
/// [`Stylesheet::parse_with_limits`](crate::Stylesheet::parse_with_limits),
/// which is primarily useful for tests and for embedding with tighter budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum input size in bytes.
    pub max_bytes: usize,
    /// Maximum number of rules.
    pub max_rules: usize,
    /// Maximum number of declarations.
    pub max_declarations: usize,
    /// Maximum at-rule / selector nesting depth.
    pub max_nesting_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_STYLESHEET_BYTES,
            max_rules: MAX_RULES,
            max_declarations: MAX_DECLARATIONS,
            max_nesting_depth: MAX_NESTING_DEPTH,
        }
    }
}

/// Mutable state shared by every nested parse of one stylesheet.
#[derive(Debug)]
pub(crate) struct ParseBudget {
    limits: Limits,
    rules: usize,
    declarations: usize,
    rule_limit_reported: bool,
    declaration_limit_reported: bool,
}

impl ParseBudget {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            rules: 0,
            declarations: 0,
            rule_limit_reported: false,
            declaration_limit_reported: false,
        }
    }

    pub(crate) fn rule_limit_reached(&self) -> bool {
        self.rules >= self.limits.max_rules
    }

    /// Consumes one rule slot, returning `false` once the budget is exhausted.
    pub(crate) fn take_rule(&mut self, diagnostics: &mut Vec<ParseDiagnostic>) -> bool {
        if self.rule_limit_reached() {
            self.report_rule_limit(diagnostics);
            return false;
        }
        self.rules += 1;
        true
    }

    pub(crate) fn report_rule_limit(&mut self, diagnostics: &mut Vec<ParseDiagnostic>) {
        if self.rule_limit_reported {
            return;
        }
        self.rule_limit_reported = true;
        push_diagnostic(
            diagnostics,
            DiagnosticKind::Rules,
            format!(
                "rule limit of {} reached; skipping remaining rules",
                self.limits.max_rules
            ),
        );
    }

    /// Consumes one declaration slot, returning `false` once exhausted.
    pub(crate) fn take_declaration(&mut self, diagnostics: &mut Vec<ParseDiagnostic>) -> bool {
        if self.declarations >= self.limits.max_declarations {
            if !self.declaration_limit_reported {
                self.declaration_limit_reported = true;
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::Declarations,
                    format!(
                        "declaration limit of {} reached; skipping remaining declarations",
                        self.limits.max_declarations
                    ),
                );
            }
            return false;
        }
        self.declarations += 1;
        true
    }

    pub(crate) fn max_nesting_depth(&self) -> usize {
        self.limits.max_nesting_depth
    }
}

/// Returns the longest prefix of `css` that fits in `max_bytes` and ends on a
/// UTF-8 character boundary. Reports a diagnostic when truncation occurs.
pub(crate) fn clamp_input<'css>(
    css: &'css str,
    max_bytes: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> &'css str {
    if css.len() <= max_bytes {
        return css;
    }
    let mut end = max_bytes;
    while end > 0 && !css.is_char_boundary(end) {
        end -= 1;
    }
    push_diagnostic(
        diagnostics,
        DiagnosticKind::InputSize,
        format!(
            "stylesheet input of {} bytes exceeds the {max_bytes}-byte limit; skipping the remainder",
            css.len()
        ),
    );
    &css[..end]
}
