//! Parsers for the modern conditional and descriptor at-rules.
//!
//! These helpers consume the already-tokenized prelude of an at-rule (with
//! byte spans into the original stylesheet) plus the raw body text produced by
//! the rule parser. They share the stylesheet-wide [`ParseBudget`] with the
//! rest of the parser and never panic on malformed input.

use crate::ast::{
    ContainerQuery, ContainerRule, ContainerSizeFeature, LayerBlockRule, LayerStatementRule,
    PropertyRule, Rule, ScopeRule, SizeOperator, StartingStyleRule, SupportsCondition,
    SupportsRule,
};
use crate::declarations::{parse_declarations_inner, parse_single_declaration};
use crate::diagnostics::ParseDiagnostic;
use crate::limits::ParseBudget;
use crate::parser::parse_nested_rules;
use crate::tokenizer::{SpannedToken, Token};

/// A cursor over the spanned tokens of an at-rule prelude.
///
/// Tokens carry absolute byte offsets into the original source, so the raw
/// text of any span can be recovered with [`Cursor::text`] even when the
/// cursor operates on a sub-slice of the prelude.
#[derive(Clone)]
struct Cursor<'a, 'css> {
    tokens: &'a [SpannedToken],
    css: &'css str,
    pos: usize,
}

impl<'a, 'css> Cursor<'a, 'css> {
    fn new(tokens: &'a [SpannedToken], css: &'css str) -> Self {
        Self {
            tokens,
            css,
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(Token::Whitespace)) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|spanned| &spanned.token)
    }

    fn first_non_ws(&self) -> Option<(usize, &Token)> {
        let mut index = self.pos;
        while matches!(
            self.tokens.get(index).map(|spanned| &spanned.token),
            Some(Token::Whitespace)
        ) {
            index += 1;
        }
        self.tokens
            .get(index)
            .map(|spanned| (index, &spanned.token))
    }

    /// Consumes `word` (skipping leading whitespace) when present.
    fn consume_ident_if(&mut self, word: &str) -> bool {
        self.skip_ws();
        if matches!(self.peek(), Some(Token::Ident(name)) if name.eq_ignore_ascii_case(word)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn consume(&mut self) {
        self.pos += 1;
    }

    /// Source text covered by the token range `[start, end)`.
    fn text(&self, start: usize, end: usize) -> &'css str {
        text_range(self.tokens, self.css, start, end)
    }
}

/// Source text covered by the token range `[start, end)`.
fn text_range<'css>(
    tokens: &[SpannedToken],
    css: &'css str,
    start: usize,
    end: usize,
) -> &'css str {
    if start >= end || start >= tokens.len() {
        return "";
    }
    let end = end.min(tokens.len());
    let first = tokens[start].start;
    let last = tokens[end - 1].end;
    if first <= last && last <= css.len() {
        &css[first..last]
    } else {
        ""
    }
}

/// All source text spanned by `tokens`.
fn raw_text<'css>(tokens: &[SpannedToken], css: &'css str) -> &'css str {
    text_range(tokens, css, 0, tokens.len())
}

/// Finds the token closing an opening parenthesis.
///
/// A [`Token::Function`] has already consumed its own `(`, so it counts as an
/// opening parenthesis here; callers that start *inside* a function pass an
/// `initial_depth` of 1, callers positioned on a `(` pass 0.
fn matching_right_paren(
    tokens: &[SpannedToken],
    start: usize,
    initial_depth: i32,
) -> Option<usize> {
    let mut depth = initial_depth;
    for (index, spanned) in tokens.iter().enumerate().skip(start) {
        match &spanned.token {
            Token::LeftParen | Token::Function(_) => depth += 1,
            Token::RightParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogicalOp {
    And,
    Or,
}

/// Combines parsed operands, flattening chains that use a single operator and
/// folding mixed (invalid but tolerated) chains left-to-right.
fn build_logical<C>(
    mut operands: Vec<C>,
    ops: Vec<LogicalOp>,
    and: impl Fn(Vec<C>) -> C,
    or: impl Fn(Vec<C>) -> C,
) -> C {
    if ops.is_empty() {
        return operands.pop().expect("build_logical requires an operand");
    }
    if ops.iter().all(|op| *op == LogicalOp::And) {
        return and(operands);
    }
    if ops.iter().all(|op| *op == LogicalOp::Or) {
        return or(operands);
    }
    let mut operands = operands.into_iter();
    let mut result = operands.next().expect("build_logical requires an operand");
    for (op, operand) in ops.into_iter().zip(operands) {
        result = if op == LogicalOp::And {
            and(vec![result, operand])
        } else {
            or(vec![result, operand])
        };
    }
    result
}

fn logical_op(name: &str) -> Option<LogicalOp> {
    if name.eq_ignore_ascii_case("and") {
        Some(LogicalOp::And)
    } else if name.eq_ignore_ascii_case("or") {
        Some(LogicalOp::Or)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// @supports
// ---------------------------------------------------------------------------

pub(crate) fn parse_supports(
    css: &str,
    prelude: &[SpannedToken],
    body: Option<&str>,
    budget: &mut ParseBudget,
    diagnostics: &mut Vec<ParseDiagnostic>,
    depth: usize,
) -> Option<Rule> {
    let body = body?;
    let mut cursor = Cursor::new(prelude, css);
    let condition = parse_supports_condition(&mut cursor)?;
    Some(Rule::Supports(SupportsRule {
        condition,
        rules: parse_nested_rules(body, budget, diagnostics, depth),
    }))
}

fn parse_supports_condition(cursor: &mut Cursor<'_, '_>) -> Option<SupportsCondition> {
    if cursor.consume_ident_if("not") {
        let inner = parse_supports_in_parens(cursor)?;
        return Some(SupportsCondition::Not(Box::new(inner)));
    }

    let first = parse_supports_in_parens(cursor)?;
    let mut operands = vec![first];
    let mut ops = Vec::new();
    loop {
        cursor.skip_ws();
        let op = match cursor.peek() {
            Some(Token::Ident(name)) => logical_op(name),
            _ => None,
        };
        let Some(op) = op else {
            break;
        };
        let save = cursor.pos;
        cursor.consume();
        if let Some(next) = parse_supports_in_parens(cursor) {
            operands.push(next);
            ops.push(op);
        } else {
            cursor.pos = save;
            break;
        }
    }
    Some(build_logical(
        operands,
        ops,
        SupportsCondition::And,
        SupportsCondition::Or,
    ))
}

fn parse_supports_in_parens(cursor: &mut Cursor<'_, '_>) -> Option<SupportsCondition> {
    cursor.skip_ws();
    match cursor.peek()? {
        Token::Function(name) if name.eq_ignore_ascii_case("selector") => {
            let function = cursor.pos;
            cursor.consume();
            let close = matching_right_paren(cursor.tokens, function + 1, 1)?;
            let selector = cursor.text(function + 1, close).trim().to_string();
            cursor.pos = close + 1;
            Some(SupportsCondition::Selector(selector))
        }
        Token::LeftParen => {
            let open = cursor.pos;
            let close = matching_right_paren(cursor.tokens, open, 0)?;
            let inner = &cursor.tokens[open + 1..close];
            let condition = parse_supports_parenthesized(inner, cursor.css);
            cursor.pos = close + 1;
            condition
        }
        Token::Function(_) => {
            let start = cursor.pos;
            let function = cursor.pos;
            cursor.consume();
            let close =
                matching_right_paren(cursor.tokens, function + 1, 1).unwrap_or(cursor.tokens.len());
            let end = (close + 1).min(cursor.tokens.len());
            let text = cursor.text(start, end).trim().to_string();
            cursor.pos = end;
            Some(SupportsCondition::GeneralEnclosed(text))
        }
        _ => None,
    }
}

fn parse_supports_parenthesized(inner: &[SpannedToken], css: &str) -> Option<SupportsCondition> {
    let mut probe = Cursor::new(inner, css);
    probe.skip_ws();
    if matches!(probe.peek(), Some(Token::Ident(_))) {
        let mut after_name = probe.clone();
        after_name.consume();
        after_name.skip_ws();
        if matches!(after_name.peek(), Some(Token::Colon)) {
            let tokens: Vec<Token> = inner.iter().map(|spanned| spanned.token.clone()).collect();
            if let Some(declaration) = parse_single_declaration(&tokens) {
                return Some(SupportsCondition::Declaration(declaration));
            }
        }
    }

    let mut nested = Cursor::new(inner, css);
    if let Some(condition) = parse_supports_condition(&mut nested) {
        nested.skip_ws();
        if nested.pos >= inner.len() {
            return Some(condition);
        }
    }

    let text = raw_text(inner, css).trim().to_string();
    (!text.is_empty()).then_some(SupportsCondition::GeneralEnclosed(text))
}

// ---------------------------------------------------------------------------
// @container
// ---------------------------------------------------------------------------

pub(crate) fn parse_container(
    css: &str,
    prelude: &[SpannedToken],
    body: Option<&str>,
    budget: &mut ParseBudget,
    diagnostics: &mut Vec<ParseDiagnostic>,
    depth: usize,
) -> Option<Rule> {
    let body = body?;
    let mut cursor = Cursor::new(prelude, css);
    cursor.skip_ws();

    // `[<container-name> and]? <container-query>`
    let mut name = None;
    if let Some(Token::Ident(candidate)) = cursor.peek() {
        let candidate = candidate.clone();
        if !matches!(
            candidate.to_ascii_lowercase().as_str(),
            "not" | "and" | "or"
        ) {
            let save = cursor.pos;
            cursor.consume();
            let next_is_query = matches!(
                cursor.first_non_ws().map(|(_, token)| token),
                Some(Token::LeftParen) | Some(Token::Function(_))
            );
            if cursor.consume_ident_if("and") || next_is_query {
                name = Some(candidate);
            } else {
                cursor.pos = save;
            }
        }
    }

    let query = parse_container_query(&mut cursor)?;
    Some(Rule::Container(ContainerRule {
        name,
        query,
        rules: parse_nested_rules(body, budget, diagnostics, depth),
    }))
}

fn parse_container_query(cursor: &mut Cursor<'_, '_>) -> Option<ContainerQuery> {
    if cursor.consume_ident_if("not") {
        let inner = parse_container_size_query(cursor)?;
        return Some(ContainerQuery::Not(Box::new(inner)));
    }

    let first = parse_container_size_query(cursor)?;
    let mut operands = vec![first];
    let mut ops = Vec::new();
    loop {
        cursor.skip_ws();
        let op = match cursor.peek() {
            Some(Token::Ident(name)) => logical_op(name),
            _ => None,
        };
        let Some(op) = op else {
            break;
        };
        let save = cursor.pos;
        cursor.consume();
        if let Some(next) = parse_container_size_query(cursor) {
            operands.push(next);
            ops.push(op);
        } else {
            cursor.pos = save;
            break;
        }
    }
    Some(build_logical(
        operands,
        ops,
        ContainerQuery::And,
        ContainerQuery::Or,
    ))
}

fn parse_container_size_query(cursor: &mut Cursor<'_, '_>) -> Option<ContainerQuery> {
    cursor.skip_ws();
    match cursor.peek()? {
        Token::Function(name) if name.eq_ignore_ascii_case("style") => {
            let function = cursor.pos;
            cursor.consume();
            let close = matching_right_paren(cursor.tokens, function + 1, 1)?;
            let feature = cursor.text(function + 1, close).trim().to_string();
            cursor.pos = close + 1;
            Some(ContainerQuery::Style(feature))
        }
        Token::LeftParen => {
            let open = cursor.pos;
            let close = matching_right_paren(cursor.tokens, open, 0)?;
            let inner = &cursor.tokens[open + 1..close];
            let query = {
                let mut nested = Cursor::new(inner, cursor.css);
                if let Some(nested_query) = parse_container_query(&mut nested) {
                    nested.skip_ws();
                    if nested.pos >= inner.len() {
                        Some(nested_query)
                    } else {
                        parse_size_feature(inner, cursor.css)
                    }
                } else {
                    parse_size_feature(inner, cursor.css)
                }
            };
            cursor.pos = close + 1;
            query
        }
        Token::Function(_) => {
            let start = cursor.pos;
            let function = cursor.pos;
            cursor.consume();
            let close =
                matching_right_paren(cursor.tokens, function + 1, 1).unwrap_or(cursor.tokens.len());
            let end = (close + 1).min(cursor.tokens.len());
            let text = cursor.text(start, end).trim().to_string();
            cursor.pos = end;
            Some(ContainerQuery::GeneralEnclosed(text))
        }
        _ => None,
    }
}

enum SizePart {
    Op(SizeOperator),
    Value(String),
}

fn is_boundary(token: &Token) -> bool {
    matches!(
        token,
        Token::Whitespace
            | Token::Colon
            | Token::Comma
            | Token::Semicolon
            | Token::Delim('>')
            | Token::Delim('<')
            | Token::Delim('=')
            | Token::LeftParen
            | Token::RightParen
            | Token::LeftBrace
            | Token::RightBrace
            | Token::Function(_)
    )
}

fn next_is_delim(tokens: &[SpannedToken], index: usize, character: char) -> bool {
    matches!(
        tokens.get(index).map(|spanned| &spanned.token),
        Some(Token::Delim(found)) if *found == character
    )
}

fn is_ident_text(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_alphabetic() || character == '-' || character == '_'
        })
}

fn parse_size_feature(inner: &[SpannedToken], css: &str) -> Option<ContainerQuery> {
    let mut parts = Vec::new();
    let mut index = 0;
    while index < inner.len() {
        if matches!(inner[index].token, Token::Whitespace) {
            index += 1;
            continue;
        }
        match &inner[index].token {
            Token::Colon => {
                parts.push(SizePart::Op(SizeOperator::Colon));
                index += 1;
            }
            Token::Delim('>') => {
                if next_is_delim(inner, index + 1, '=') {
                    parts.push(SizePart::Op(SizeOperator::GreaterThanOrEqual));
                    index += 2;
                } else {
                    parts.push(SizePart::Op(SizeOperator::GreaterThan));
                    index += 1;
                }
            }
            Token::Delim('<') => {
                if next_is_delim(inner, index + 1, '=') {
                    parts.push(SizePart::Op(SizeOperator::LessThanOrEqual));
                    index += 2;
                } else {
                    parts.push(SizePart::Op(SizeOperator::LessThan));
                    index += 1;
                }
            }
            Token::Delim('=') => {
                parts.push(SizePart::Op(SizeOperator::Equal));
                index += 1;
            }
            Token::Function(_) => {
                let start = index;
                let close = matching_right_paren(inner, index + 1, 1).unwrap_or(inner.len());
                index = (close + 1).min(inner.len());
                parts.push(SizePart::Value(
                    text_range(inner, css, start, index).trim().to_string(),
                ));
            }
            Token::RightParen | Token::LeftParen => index += 1,
            _ => {
                let start = index;
                index += 1;
                while index < inner.len() && !is_boundary(&inner[index].token) {
                    index += 1;
                }
                parts.push(SizePart::Value(
                    text_range(inner, css, start, index).trim().to_string(),
                ));
            }
        }
    }

    let name_index = parts.iter().position(|part| match part {
        SizePart::Value(value) => is_ident_text(value),
        SizePart::Op(_) => false,
    })?;
    let SizePart::Value(name) = parts.remove(name_index) else {
        return None;
    };
    let mut operators = Vec::new();
    let mut values = Vec::new();
    for part in parts {
        match part {
            SizePart::Op(operator) => operators.push(operator),
            SizePart::Value(value) => values.push(value),
        }
    }

    if operators.is_empty() && values.is_empty() {
        return Some(ContainerQuery::GeneralEnclosed(name));
    }
    Some(ContainerQuery::Size(ContainerSizeFeature {
        name,
        operators,
        values,
    }))
}

// ---------------------------------------------------------------------------
// @layer
// ---------------------------------------------------------------------------

pub(crate) fn parse_layer(
    css: &str,
    prelude: &[SpannedToken],
    body: Option<&str>,
    budget: &mut ParseBudget,
    diagnostics: &mut Vec<ParseDiagnostic>,
    depth: usize,
) -> Option<Rule> {
    let text = raw_text(prelude, css).trim();
    if let Some(body) = body {
        let name = (!text.is_empty()).then(|| text.to_string());
        Some(Rule::LayerBlock(LayerBlockRule {
            name,
            rules: parse_nested_rules(body, budget, diagnostics, depth),
        }))
    } else {
        let names: Vec<String> = text
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
        if names.is_empty() {
            return None;
        }
        Some(Rule::LayerStatement(LayerStatementRule { names }))
    }
}

// ---------------------------------------------------------------------------
// @scope
// ---------------------------------------------------------------------------

pub(crate) fn parse_scope(
    css: &str,
    prelude: &[SpannedToken],
    body: Option<&str>,
    budget: &mut ParseBudget,
    diagnostics: &mut Vec<ParseDiagnostic>,
    depth: usize,
) -> Option<Rule> {
    let body = body?;
    let (start, end) = split_scope_prelude(prelude);
    Some(Rule::Scope(ScopeRule {
        root: scope_selector(start, css),
        limit: scope_selector(end, css),
        rules: parse_nested_rules(body, budget, diagnostics, depth),
    }))
}

/// Splits a scope prelude into its root and `to (...)` limit parts.
fn split_scope_prelude(tokens: &[SpannedToken]) -> (&[SpannedToken], &[SpannedToken]) {
    let mut depth = 0i32;
    for index in 0..tokens.len() {
        match &tokens[index].token {
            Token::LeftParen | Token::Function(_) => depth += 1,
            Token::RightParen => depth = (depth - 1).max(0),
            Token::Ident(word) if depth == 0 && word.eq_ignore_ascii_case("to") => {
                let preceded_by_space =
                    index == 0 || matches!(tokens[index - 1].token, Token::Whitespace);
                let followed_by_group = matches!(
                    tokens.get(index + 1).map(|spanned| &spanned.token),
                    Some(Token::Whitespace) | Some(Token::LeftParen)
                );
                if preceded_by_space && followed_by_group {
                    return (&tokens[..index], &tokens[index + 1..]);
                }
            }
            _ => {}
        }
    }
    (tokens, &[])
}

fn scope_selector(tokens: &[SpannedToken], css: &str) -> Option<String> {
    let mut start = 0;
    while matches!(
        tokens.get(start).map(|spanned| &spanned.token),
        Some(Token::Whitespace)
    ) {
        start += 1;
    }
    let tokens = &tokens[start..];
    if tokens.is_empty() {
        return None;
    }
    if matches!(tokens[0].token, Token::LeftParen) {
        let close = matching_right_paren(tokens, 0, 0)?;
        let inner = &tokens[1..close];
        let text = raw_text(inner, css).trim().to_string();
        return (!text.is_empty()).then_some(text);
    }
    let text = raw_text(tokens, css).trim().to_string();
    (!text.is_empty()).then_some(text)
}

// ---------------------------------------------------------------------------
// @starting-style
// ---------------------------------------------------------------------------

pub(crate) fn parse_starting_style(
    body: Option<&str>,
    budget: &mut ParseBudget,
    diagnostics: &mut Vec<ParseDiagnostic>,
    depth: usize,
) -> Option<Rule> {
    let body = body?;
    Some(Rule::StartingStyle(StartingStyleRule {
        rules: parse_nested_rules(body, budget, diagnostics, depth),
    }))
}

// ---------------------------------------------------------------------------
// @property
// ---------------------------------------------------------------------------

pub(crate) fn parse_property(
    css: &str,
    prelude: &[SpannedToken],
    body: Option<&str>,
    budget: &mut ParseBudget,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<Rule> {
    let body = body?;
    let mut cursor = Cursor::new(prelude, css);
    cursor.skip_ws();
    let name = match cursor.peek() {
        Some(Token::Ident(name)) => name.clone(),
        _ => raw_text(prelude, css).trim().to_string(),
    };
    if name.is_empty() {
        return None;
    }

    let descriptors = parse_declarations_inner(body, budget, diagnostics);
    let mut syntax = None;
    let mut inherits = None;
    let mut initial_value = None;
    for descriptor in &descriptors {
        match descriptor.name.as_str() {
            "syntax" => syntax = descriptor.value.as_string().map(str::to_string),
            "inherits" => {
                inherits = match descriptor.value.ident() {
                    Some("true") => Some(true),
                    Some("false") => Some(false),
                    _ => None,
                }
            }
            "initial-value" => initial_value = Some(descriptor.value.clone()),
            _ => {}
        }
    }

    Some(Rule::Property(PropertyRule {
        name,
        syntax,
        inherits,
        initial_value,
        descriptors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Origin, StyleRule, Stylesheet};
    use crate::limits::Limits;

    fn sheet(css: &str) -> Stylesheet {
        Stylesheet::parse(css, Origin::Author)
    }

    fn selectors(rules: &[Rule]) -> Vec<&str> {
        rules
            .iter()
            .filter_map(|rule| match rule {
                Rule::Style(StyleRule { selectors, .. }) => Some(selectors[0].as_str()),
                _ => None,
            })
            .collect()
    }

    // -- @supports -----------------------------------------------------------

    #[test]
    fn supports_declaration_condition() {
        let sheet = sheet("@supports (display: grid) { p { color: red } }");
        let Rule::Supports(rule) = &sheet.rules[0] else {
            panic!("expected @supports");
        };
        let SupportsCondition::Declaration(declaration) = &rule.condition else {
            panic!("expected a declaration condition");
        };
        assert_eq!(declaration.name, "display");
        assert_eq!(declaration.value.ident(), Some("grid"));
        assert_eq!(selectors(&rule.rules), ["p"]);
    }

    #[test]
    fn supports_and_or_not_conditions() {
        let sheet = sheet(
            "@supports (display: grid) and (color: red) or (not (width: 0)) { a { color: red } }",
        );
        let Rule::Supports(rule) = &sheet.rules[0] else {
            panic!("expected @supports");
        };
        let SupportsCondition::Or(parts) = &rule.condition else {
            panic!("expected an `or` condition, got {:?}", rule.condition);
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], SupportsCondition::And(inner) if inner.len() == 2));
        assert!(
            matches!(&parts[1], SupportsCondition::Not(inner) if matches!(**inner, SupportsCondition::Declaration(_)))
        );
    }

    #[test]
    fn supports_nested_parentheses() {
        let sheet = sheet("@supports ((display: grid) or (color: red)) { a { color: red } }");
        let Rule::Supports(rule) = &sheet.rules[0] else {
            panic!("expected @supports");
        };
        let SupportsCondition::Or(inner) = &rule.condition else {
            panic!("expected `or`, got {:?}", rule.condition);
        };
        assert_eq!(inner.len(), 2);
    }

    #[test]
    fn supports_selector_and_general_enclosed() {
        let sheet =
            sheet("@supports selector(:has(*)) and (font-tech(color-COLRv1)) { a { color: red } }");
        let Rule::Supports(rule) = &sheet.rules[0] else {
            panic!("expected @supports");
        };
        let SupportsCondition::And(parts) = &rule.condition else {
            panic!("expected `and`, got {:?}", rule.condition);
        };
        assert!(
            matches!(&parts[0], SupportsCondition::Selector(selector) if selector == ":has(*)")
        );
        assert!(
            matches!(&parts[1], SupportsCondition::GeneralEnclosed(text) if text.contains("font-tech"))
        );
    }

    #[test]
    fn supports_empty_or_broken_condition_is_skipped() {
        assert!(sheet("@supports { p { color: red } }").rules.is_empty());
        assert!(sheet("@supports not { p { color: red } }").rules.is_empty());
    }

    // -- @container ----------------------------------------------------------

    #[test]
    fn container_named_min_width() {
        let sheet = sheet("@container card (min-width: 40em) { p { color: red } }");
        let Rule::Container(rule) = &sheet.rules[0] else {
            panic!("expected @container");
        };
        assert_eq!(rule.name.as_deref(), Some("card"));
        let ContainerQuery::Size(feature) = &rule.query else {
            panic!("expected a size feature, got {:?}", rule.query);
        };
        assert_eq!(feature.name, "min-width");
        assert_eq!(feature.operators, [SizeOperator::Colon]);
        assert_eq!(feature.values, ["40em"]);
        assert!(!feature.is_range());
        assert_eq!(selectors(&rule.rules), ["p"]);
    }

    #[test]
    fn container_comparison_and_range_queries() {
        let sheet = sheet(
            "@container (width >= 400px) { a { color: red } } \
             @container (400px <= width <= 700px) { b { color: red } }",
        );
        let Rule::Container(first) = &sheet.rules[0] else {
            panic!("expected @container");
        };
        let ContainerQuery::Size(feature) = &first.query else {
            panic!("expected a size feature");
        };
        assert_eq!(feature.name, "width");
        assert_eq!(feature.operators, [SizeOperator::GreaterThanOrEqual]);
        assert_eq!(feature.values, ["400px"]);
        assert_eq!(feature.operators[0].as_str(), ">=");

        let Rule::Container(second) = &sheet.rules[1] else {
            panic!("expected @container");
        };
        let ContainerQuery::Size(feature) = &second.query else {
            panic!("expected a size feature");
        };
        assert_eq!(feature.name, "width");
        assert!(feature.is_range());
        assert_eq!(feature.operators, [SizeOperator::LessThanOrEqual; 2]);
        assert_eq!(feature.values, ["400px", "700px"]);
    }

    #[test]
    fn container_logical_and_style_queries() {
        let sheet = sheet(
            "@container (width > 10px) and (height < 20px) { a { color: red } } \
             @container style(--theme: dark) { b { color: red } } \
             @container not (width = 0) { c { color: red } }",
        );
        let Rule::Container(first) = &sheet.rules[0] else {
            panic!("expected @container");
        };
        assert!(matches!(&first.query, ContainerQuery::And(parts) if parts.len() == 2));
        let Rule::Container(second) = &sheet.rules[1] else {
            panic!("expected @container");
        };
        assert!(matches!(&second.query, ContainerQuery::Style(text) if text == "--theme: dark"));
        let Rule::Container(third) = &sheet.rules[2] else {
            panic!("expected @container");
        };
        assert!(
            matches!(&third.query, ContainerQuery::Not(inner) if matches!(**inner, ContainerQuery::Size(_)))
        );
    }

    #[test]
    fn container_without_query_is_skipped() {
        assert!(
            sheet("@container card { p { color: red } }")
                .rules
                .is_empty()
        );
        assert!(sheet("@container { p { color: red } }").rules.is_empty());
    }

    // -- @layer --------------------------------------------------------------

    #[test]
    fn layer_statement_names_and_segments() {
        let sheet = sheet("@layer reset, base.theme, utilities;");
        let Rule::LayerStatement(rule) = &sheet.rules[0] else {
            panic!("expected a layer statement");
        };
        assert_eq!(rule.names, ["reset", "base.theme", "utilities"]);
        assert_eq!(rule.segments()[1], ["base", "theme"]);
        assert_eq!(crate::layer_segments("a.b.c"), ["a", "b", "c"]);
    }

    #[test]
    fn layer_named_and_anonymous_blocks() {
        let sheet = sheet("@layer base { p { color: red } } @layer { q { color: blue } }");
        let Rule::LayerBlock(named) = &sheet.rules[0] else {
            panic!("expected a layer block");
        };
        assert_eq!(named.name.as_deref(), Some("base"));
        assert_eq!(named.segments(), ["base"]);
        assert_eq!(selectors(&named.rules), ["p"]);

        let Rule::LayerBlock(anonymous) = &sheet.rules[1] else {
            panic!("expected a layer block");
        };
        assert_eq!(anonymous.name, None);
        assert!(anonymous.segments().is_empty());
        assert_eq!(selectors(&anonymous.rules), ["q"]);
    }

    #[test]
    fn empty_layer_statement_is_skipped() {
        assert!(sheet("@layer ;").rules.is_empty());
    }

    // -- @scope --------------------------------------------------------------

    #[test]
    fn scope_root_and_limit() {
        let sheet = sheet("@scope (.card) to (.card p) { a { color: red } }");
        let Rule::Scope(rule) = &sheet.rules[0] else {
            panic!("expected @scope");
        };
        assert_eq!(rule.root.as_deref(), Some(".card"));
        assert_eq!(rule.limit.as_deref(), Some(".card p"));
        assert_eq!(selectors(&rule.rules), ["a"]);
    }

    #[test]
    fn scope_without_limit_and_without_root() {
        let sheet = sheet("@scope (.card) { a { color: red } } @scope { b { color: blue } }");
        let Rule::Scope(first) = &sheet.rules[0] else {
            panic!("expected @scope");
        };
        assert_eq!(first.root.as_deref(), Some(".card"));
        assert_eq!(first.limit, None);

        let Rule::Scope(second) = &sheet.rules[1] else {
            panic!("expected @scope");
        };
        assert_eq!(second.root, None);
        assert_eq!(second.limit, None);
    }

    #[test]
    fn scope_to_only() {
        let sheet = sheet("@scope to (.card p) { a { color: red } }");
        let Rule::Scope(rule) = &sheet.rules[0] else {
            panic!("expected @scope");
        };
        assert_eq!(rule.root, None);
        assert_eq!(rule.limit.as_deref(), Some(".card p"));
    }

    // -- @starting-style / @property -----------------------------------------

    #[test]
    fn starting_style_holds_nested_rules() {
        let sheet = sheet("@starting-style { p { opacity: 0; transform: scale(0.9) } }");
        let Rule::StartingStyle(rule) = &sheet.rules[0] else {
            panic!("expected @starting-style");
        };
        assert_eq!(selectors(&rule.rules), ["p"]);
        let Rule::Style(style) = &rule.rules[0] else {
            panic!("expected a style rule");
        };
        assert_eq!(style.declarations.len(), 2);
    }

    #[test]
    fn property_descriptors() {
        let sheet = sheet(
            "@property --my-color { syntax: \"<color>\"; inherits: false; initial-value: red; }",
        );
        let Rule::Property(rule) = &sheet.rules[0] else {
            panic!("expected @property");
        };
        assert_eq!(rule.name, "--my-color");
        assert_eq!(rule.syntax.as_deref(), Some("<color>"));
        assert_eq!(rule.inherits, Some(false));
        assert_eq!(
            rule.initial_value.as_ref().and_then(|value| value.ident()),
            Some("red")
        );
        assert_eq!(rule.descriptors.len(), 3);
    }

    #[test]
    fn property_missing_descriptors_are_none() {
        let sheet = sheet("@property --x { inherits: true }");
        let Rule::Property(rule) = &sheet.rules[0] else {
            panic!("expected @property");
        };
        assert_eq!(rule.inherits, Some(true));
        assert_eq!(rule.syntax, None);
        assert_eq!(rule.initial_value, None);
    }

    // -- integration: nesting, budgets, recovery -----------------------------

    #[test]
    fn modern_at_rules_repeat_at_top_level() {
        let sheet = sheet(
            "@layer base { a { color: red } } \
             @supports (display: grid) { b { color: red } } \
             @container (width > 0px) { c { color: red } } \
             @scope (a) { d { color: red } } \
             @starting-style { e { color: red } } \
             @property --x { syntax: '*'; inherits: false; } \
             z { color: red }",
        );
        assert_eq!(sheet.rules.len(), 7);
        assert_eq!(selectors(std::slice::from_ref(&sheet.rules[6])), ["z"]);
    }

    #[test]
    fn deep_conditional_nesting_is_structured() {
        let sheet = sheet(
            "@supports (display: grid) { \
                @container (width > 0px) { \
                    @layer base { p { color: red } } \
                } \
             }",
        );
        let Rule::Supports(supports) = &sheet.rules[0] else {
            panic!("expected @supports");
        };
        let Rule::Container(container) = &supports.rules[0] else {
            panic!("expected @container");
        };
        assert!(matches!(container.rules[0], Rule::LayerBlock(_)));
    }

    #[test]
    fn nested_rules_share_the_rule_budget() {
        let sheet = crate::Stylesheet::parse_with_limits(
            "@supports (display: grid) { a { color: red } b { color: blue } } c { color: green }",
            Origin::Author,
            Limits {
                max_rules: 3,
                ..Limits::default()
            },
        );
        // The nested rules claim their slots before the parent at-rule, so
        // `@supports`, `a` and `b` fit while `c` is skipped.
        assert_eq!(sheet.rules.len(), 1);
        let Rule::Supports(supports) = &sheet.rules[0] else {
            panic!("expected @supports");
        };
        assert_eq!(supports.rules.len(), 2);
        assert_eq!(sheet.diagnostics.len(), 1);
        assert_eq!(sheet.diagnostics[0].kind, crate::DiagnosticKind::Rules);
    }

    #[test]
    fn nested_at_rules_respect_depth_limit() {
        let css = "@supports (display: grid) { @container (width > 0px) { p { color: red } } }";
        let sheet = crate::Stylesheet::parse_with_limits(
            css,
            Origin::Author,
            Limits {
                max_nesting_depth: 1,
                ..Limits::default()
            },
        );
        assert!(
            sheet
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == crate::DiagnosticKind::NestingDepth)
        );
    }

    #[test]
    fn declarations_in_modern_at_rules_share_the_declaration_budget() {
        let sheet = crate::Stylesheet::parse_with_limits(
            "@property --x { syntax: '*'; inherits: false; initial-value: red }",
            Origin::Author,
            Limits {
                max_declarations: 2,
                ..Limits::default()
            },
        );
        let Rule::Property(rule) = &sheet.rules[0] else {
            panic!("expected @property");
        };
        assert_eq!(rule.descriptors.len(), 2);
        assert_eq!(sheet.diagnostics.len(), 1);
        assert_eq!(
            sheet.diagnostics[0].kind,
            crate::DiagnosticKind::Declarations
        );
    }

    #[test]
    fn malformed_modern_at_rules_never_panic() {
        for css in [
            "@supports",
            "@supports (",
            "@supports not",
            "@supports () { p { color: red } }",
            "@supports (display: grid) {",
            "@container",
            "@container (",
            "@container not",
            "@container (width >)",
            "@layer",
            "@layer ;",
            "@layer {",
            "@scope",
            "@scope (",
            "@scope to",
            "@scope {",
            "@starting-style",
            "@property",
            "@property --x {",
            "@supports selector(:has({},",
        ] {
            let _ = sheet(css);
        }
    }
}
