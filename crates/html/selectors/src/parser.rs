//! Selector parsing: complex selectors, compounds, attributes and
//! pseudo-classes.

use crate::{
    AttrOp, Combinator, CompoundSelector, Direction, PseudoClass, Selector, SelectorError,
    SimpleSelector, Specificity,
};

/// Parses a single complex selector.
///
/// # Errors
/// Returns [`SelectorError`] when the input is not exactly one valid selector.
pub fn parse_selector(input: &str) -> Result<Selector, SelectorError> {
    let mut selectors = parse_list(input, 0)?;
    if selectors.len() != 1 {
        return Err(SelectorError {
            message: "expected a single selector".to_string(),
            position: first_top_level_comma(input),
        });
    }
    Ok(selectors.remove(0))
}

/// Parses a comma-separated selector list.
///
/// # Errors
/// Returns [`SelectorError`] for the first invalid selector.
pub fn parse_selectors(input: &str) -> Result<Vec<Selector>, SelectorError> {
    parse_list(input, 0)
}

fn parse_list(text: &str, base: usize) -> Result<Vec<Selector>, SelectorError> {
    parse_list_inner(text, base, false)
}

/// Parses a `<relative-selector-list>`, as accepted by functional
/// pseudo-classes such as `:has()`. Unlike a normal selector list, each
/// selector may begin with a combinator (`>`, `+`, `~`) describing its
/// relationship to the element that owns the pseudo-class.
fn parse_relative_list(text: &str, base: usize) -> Result<Vec<Selector>, SelectorError> {
    parse_list_inner(text, base, true)
}

fn parse_list_inner(
    text: &str,
    base: usize,
    allow_leading_combinator: bool,
) -> Result<Vec<Selector>, SelectorError> {
    let mut selectors = Vec::new();
    for (start, end) in split_top_level_commas(text) {
        let part = &text[start..end];
        if part.trim().is_empty() {
            return Err(SelectorError {
                message: "empty selector".to_string(),
                position: base + start,
            });
        }
        selectors.push(parse_complex_inner(
            part,
            base + start,
            allow_leading_combinator,
        )?);
    }
    if selectors.is_empty() {
        return Err(SelectorError {
            message: "empty selector".to_string(),
            position: base,
        });
    }
    Ok(selectors)
}

fn split_top_level_commas(text: &str) -> Vec<(usize, usize)> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote = None;
    let mut escaped = false;

    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(open) = quote {
            if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => quote = Some(character),
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                parts.push((start, index));
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push((start, text.len()));
    parts
}

fn first_top_level_comma(text: &str) -> usize {
    split_top_level_commas(text)
        .get(1)
        .map_or(text.len(), |(start, _)| start.saturating_sub(1))
}

fn parse_complex_inner(
    text: &str,
    base: usize,
    allow_leading_combinator: bool,
) -> Result<Selector, SelectorError> {
    let mut cursor = Cursor::new(text, base);
    let mut parts: Vec<(Option<Combinator>, CompoundSelector)> = Vec::new();
    let mut specificity = Specificity::default();
    let mut pending: Option<Combinator> = None;
    let mut pseudo_element: Option<String> = None;

    loop {
        cursor.skip_whitespace();
        if cursor.at_end() {
            if pending.is_some() {
                return Err(cursor.error("expected selector after combinator"));
            }
            break;
        }

        let combinator_position = cursor.position();
        if let Some(combinator) = cursor.consume_combinator() {
            if parts.is_empty() {
                if allow_leading_combinator && pending.is_none() {
                    // A relative selector may start with a combinator; it
                    // describes how the first compound relates to the anchor.
                    pending = Some(combinator);
                    continue;
                }
                return Err(SelectorError {
                    message: "unexpected combinator".to_string(),
                    position: combinator_position,
                });
            }
            if pending.is_some() {
                return Err(SelectorError {
                    message: "duplicate combinator".to_string(),
                    position: combinator_position,
                });
            }
            pending = Some(combinator);
            continue;
        }

        let (compound, compound_specificity, compound_pseudo) = parse_compound(&mut cursor)?;
        specificity = specificity.add(compound_specificity);
        if compound_pseudo.is_some() {
            pseudo_element = compound_pseudo;
        }
        if parts.is_empty() {
            // For relative selectors the first compound carries the leading
            // combinator; `None` means the implicit descendant combinator.
            parts.push((pending.take(), compound));
        } else {
            parts.push((
                Some(pending.take().unwrap_or(Combinator::Descendant)),
                compound,
            ));
        }

        if pseudo_element.is_some() {
            cursor.skip_whitespace();
            if !cursor.at_end() {
                return Err(cursor.error("unexpected selector after pseudo-element"));
            }
        }
    }

    if parts.is_empty() {
        return Err(SelectorError {
            message: "empty selector".to_string(),
            position: base,
        });
    }

    Ok(Selector {
        parts,
        specificity,
        text: text.trim().to_string(),
        pseudo_element,
    })
}

fn parse_compound(
    cursor: &mut Cursor<'_>,
) -> Result<(CompoundSelector, Specificity, Option<String>), SelectorError> {
    let mut simples = Vec::new();
    let mut specificity = Specificity::default();
    let mut pseudo_element: Option<String> = None;

    match cursor.peek() {
        Some('*') => {
            cursor.bump();
            simples.push(SimpleSelector::Universal);
        }
        Some('#' | '.' | '[' | ':') => {}
        _ if cursor.starts_ident() => {
            let name = cursor
                .consume_ident()
                .ok_or_else(|| cursor.error("expected type selector"))?;
            specificity.c += 1;
            simples.push(SimpleSelector::Type(name));
        }
        _ => {}
    }

    loop {
        match cursor.peek() {
            Some('#') => {
                cursor.bump();
                let id = cursor.consume_ident_required("expected identifier after '#'")?;
                specificity.a += 1;
                simples.push(SimpleSelector::Id(id));
            }
            Some('.') => {
                cursor.bump();
                let class = cursor.consume_ident_required("expected identifier after '.'")?;
                specificity.b += 1;
                simples.push(SimpleSelector::Class(class));
            }
            Some('[') => {
                cursor.bump();
                simples.push(parse_attribute(cursor)?);
                specificity.b += 1;
            }
            Some(':') => {
                cursor.bump();
                if cursor.eat(':') {
                    let element = parse_pseudo_element(cursor)?;
                    specificity.c += 1;
                    pseudo_element = Some(element.clone());
                    simples.push(SimpleSelector::PseudoElement(element));
                    break;
                }
                match parse_pseudo(cursor)? {
                    Pseudo::Class(class) => {
                        match &class {
                            PseudoClass::Where(_) => {}
                            PseudoClass::Not(list)
                            | PseudoClass::Is(list)
                            | PseudoClass::Has(list) => {
                                specificity = specificity.add(max_specificity(list));
                            }
                            // `:nth-child(... of S)` contributes one
                            // pseudo-class plus the most specific selector in
                            // its `of` argument.
                            PseudoClass::NthChildOf(_, _, list)
                            | PseudoClass::NthLastChildOf(_, _, list) => {
                                specificity.b += 1;
                                specificity = specificity.add(max_specificity(list));
                            }
                            _ => specificity.b += 1,
                        }
                        simples.push(SimpleSelector::Pseudo(class));
                    }
                    Pseudo::Element(element) => {
                        specificity.c += 1;
                        pseudo_element = Some(element.clone());
                        simples.push(SimpleSelector::PseudoElement(element));
                        break;
                    }
                }
            }
            _ => break,
        }
    }

    if simples.is_empty() {
        return Err(cursor.error("expected selector"));
    }

    Ok((CompoundSelector { simples }, specificity, pseudo_element))
}

fn max_specificity(selectors: &[Selector]) -> Specificity {
    selectors
        .iter()
        .map(|selector| selector.specificity)
        .max()
        .unwrap_or_default()
}

enum Pseudo {
    Class(PseudoClass),
    Element(String),
}

fn parse_pseudo(cursor: &mut Cursor<'_>) -> Result<Pseudo, SelectorError> {
    let start = cursor.position().saturating_sub(1);
    let name = cursor
        .consume_ident_required("expected pseudo-class name")?
        .to_ascii_lowercase();

    let legacy_element = matches!(
        name.as_str(),
        "before" | "after" | "first-line" | "first-letter"
    );

    if cursor.peek() == Some('(') {
        if legacy_element {
            return Err(SelectorError {
                message: format!("pseudo-element ':{name}' cannot take arguments"),
                position: start,
            });
        }
        let (content, content_base) = cursor.consume_parenthesized()?;
        let class = match name.as_str() {
            "not" => PseudoClass::Not(parse_list(content, content_base)?),
            "is" => PseudoClass::Is(parse_list(content, content_base)?),
            "where" => PseudoClass::Where(parse_list(content, content_base)?),
            "has" => PseudoClass::Has(parse_relative_list(content, content_base)?),
            "nth-child" => {
                let (formula, of) = split_nth_of(content, content_base);
                let (a, b) = parse_nth(formula, content_base)?;
                match of {
                    Some((list, list_base)) => {
                        PseudoClass::NthChildOf(a, b, parse_list(list, list_base)?)
                    }
                    None => PseudoClass::NthChild(a, b),
                }
            }
            "nth-last-child" => {
                let (formula, of) = split_nth_of(content, content_base);
                let (a, b) = parse_nth(formula, content_base)?;
                match of {
                    Some((list, list_base)) => {
                        PseudoClass::NthLastChildOf(a, b, parse_list(list, list_base)?)
                    }
                    None => PseudoClass::NthLastChild(a, b),
                }
            }
            "nth-of-type" => {
                let (a, b) = parse_nth(content, content_base)?;
                PseudoClass::NthOfType(a, b)
            }
            "nth-last-of-type" => {
                let (a, b) = parse_nth(content, content_base)?;
                PseudoClass::NthLastOfType(a, b)
            }
            "lang" => PseudoClass::Lang(unquote(content.trim()).to_string()),
            "dir" => {
                let value = unquote(content.trim()).to_ascii_lowercase();
                match value.as_str() {
                    "ltr" => PseudoClass::Dir(Direction::Ltr),
                    "rtl" => PseudoClass::Dir(Direction::Rtl),
                    _ => {
                        return Err(SelectorError {
                            message: format!("invalid direction ':dir({value})'"),
                            position: start,
                        });
                    }
                }
            }
            _ => {
                return Err(SelectorError {
                    message: format!("unknown pseudo-class ':{name}'"),
                    position: start,
                });
            }
        };
        return Ok(Pseudo::Class(class));
    }

    if legacy_element {
        return Ok(Pseudo::Element(name));
    }

    let class = match name.as_str() {
        "first-child" => PseudoClass::FirstChild,
        "last-child" => PseudoClass::LastChild,
        "only-child" => PseudoClass::OnlyChild,
        "root" => PseudoClass::Root,
        "empty" => PseudoClass::Empty,
        "first-of-type" => PseudoClass::FirstOfType,
        "last-of-type" => PseudoClass::LastOfType,
        "only-of-type" => PseudoClass::OnlyOfType,
        "hover" => PseudoClass::Hover,
        "focus" => PseudoClass::Focus,
        "active" => PseudoClass::Active,
        "link" => PseudoClass::Link,
        "visited" => PseudoClass::Visited,
        "checked" => PseudoClass::Checked,
        "disabled" => PseudoClass::Disabled,
        "enabled" => PseudoClass::Enabled,
        "any-link" => PseudoClass::AnyLink,
        "target" => PseudoClass::Target,
        "focus-visible" => PseudoClass::FocusVisible,
        "focus-within" => PseudoClass::FocusWithin,
        "valid" => PseudoClass::Valid,
        "invalid" => PseudoClass::Invalid,
        "required" => PseudoClass::Required,
        "optional" => PseudoClass::Optional,
        "placeholder-shown" => PseudoClass::PlaceholderShown,
        "read-only" => PseudoClass::ReadOnly,
        "read-write" => PseudoClass::ReadWrite,
        "host" => PseudoClass::Host,
        "defined" => PseudoClass::Defined,
        "modal" => PseudoClass::Modal,
        "autofill" => PseudoClass::Autofill,
        "user-invalid" => PseudoClass::UserInvalid,
        _ => {
            return Err(SelectorError {
                message: format!("unknown pseudo-class ':{name}'"),
                position: start,
            });
        }
    };
    Ok(Pseudo::Class(class))
}

fn parse_pseudo_element(cursor: &mut Cursor<'_>) -> Result<String, SelectorError> {
    let name = cursor
        .consume_ident_required("expected pseudo-element name")?
        .to_ascii_lowercase();
    if cursor.peek() == Some('(') {
        // Functional pseudo-elements (`::part()`, `::slotted()`) are parsed
        // and their arguments consumed, but they never match.
        let _ = cursor.consume_parenthesized()?;
    }
    Ok(name)
}

fn parse_attribute(cursor: &mut Cursor<'_>) -> Result<SimpleSelector, SelectorError> {
    cursor.skip_whitespace();
    let name = cursor
        .consume_ident_required("expected attribute name")?
        .to_ascii_lowercase();
    cursor.skip_whitespace();

    if cursor.eat(']') {
        return Ok(SimpleSelector::Attribute {
            name,
            op: AttrOp::Exists,
            value: None,
            case_insensitive: false,
        });
    }

    let op = match cursor.peek() {
        Some('=') => {
            cursor.bump();
            AttrOp::Equals
        }
        Some('~') if cursor.peek_at(1) == Some('=') => {
            cursor.bump();
            cursor.bump();
            AttrOp::Includes
        }
        Some('|') if cursor.peek_at(1) == Some('=') => {
            cursor.bump();
            cursor.bump();
            AttrOp::DashMatch
        }
        Some('^') if cursor.peek_at(1) == Some('=') => {
            cursor.bump();
            cursor.bump();
            AttrOp::Prefix
        }
        Some('$') if cursor.peek_at(1) == Some('=') => {
            cursor.bump();
            cursor.bump();
            AttrOp::Suffix
        }
        Some('*') if cursor.peek_at(1) == Some('=') => {
            cursor.bump();
            cursor.bump();
            AttrOp::Substring
        }
        _ => return Err(cursor.error("expected attribute operator")),
    };

    cursor.skip_whitespace();
    let value = match cursor.peek() {
        Some('"' | '\'') => cursor.consume_string()?,
        _ => cursor.consume_ident_required("expected attribute value")?,
    };
    cursor.skip_whitespace();

    let mut case_insensitive = false;
    if cursor.starts_ident() {
        let flag_position = cursor.position();
        let flag = cursor
            .consume_ident()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match flag.as_str() {
            "i" => case_insensitive = true,
            "s" => {}
            _ => {
                return Err(SelectorError {
                    message: format!("invalid attribute flag '{flag}'"),
                    position: flag_position,
                });
            }
        }
        cursor.skip_whitespace();
    }

    if !cursor.eat(']') {
        return Err(cursor.error("expected ']' to close attribute selector"));
    }

    Ok(SimpleSelector::Attribute {
        name,
        op,
        value: Some(value),
        case_insensitive,
    })
}

/// Splits the `of S` clause off an `:nth-child()` / `:nth-last-child()`
/// argument. The returned slice and offset describe the selector list so error
/// positions stay relative to the original input.
fn split_nth_of(content: &str, content_base: usize) -> (&str, Option<(&str, usize)>) {
    let lowered = content.to_ascii_lowercase();
    if let Some(index) = lowered.find(" of ") {
        let list_start = index + " of ".len();
        (
            &content[..index],
            Some((&content[list_start..], content_base + list_start)),
        )
    } else {
        (content, None)
    }
}

/// Parses the `an+b` part of an `:nth-*()` argument. `content` must not contain
/// an `of S` clause; use [`split_nth_of`] first.
fn parse_nth(content: &str, base: usize) -> Result<(i32, i32), SelectorError> {
    let compact: String = content
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();

    let error = || SelectorError {
        message: format!("invalid nth expression '{content}'"),
        position: base,
    };

    match compact.as_str() {
        "odd" => return Ok((2, 1)),
        "even" => return Ok((2, 0)),
        _ => {}
    }

    if let Some(n_index) = compact.find('n') {
        let a_part = &compact[..n_index];
        let b_part = &compact[n_index + 1..];
        let a = match a_part {
            "" | "+" => 1,
            "-" => -1,
            other => other.parse::<i32>().map_err(|_| error())?,
        };
        let b = if b_part.is_empty() {
            0
        } else {
            let sign = b_part.as_bytes().first().copied();
            if !matches!(sign, Some(b'+' | b'-')) {
                return Err(error());
            }
            let digits = if sign == Some(b'+') {
                &b_part[1..]
            } else {
                b_part
            };
            digits.parse::<i32>().map_err(|_| error())?
        };
        return Ok((a, b));
    }

    let b = compact.parse::<i32>().map_err(|_| error())?;
    Ok((0, b))
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn is_name_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_' || !character.is_ascii()
}

fn is_name_char(character: char) -> bool {
    is_name_start(character) || character.is_ascii_digit() || character == '-'
}

fn is_css_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

struct Cursor<'a> {
    text: &'a str,
    base: usize,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str, base: usize) -> Self {
        Self { text, base, pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.text[self.pos..].chars().nth(offset)
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.pos += character.len_utf8();
        Some(character)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn position(&self) -> usize {
        self.base + self.pos
    }

    fn error(&self, message: impl Into<String>) -> SelectorError {
        SelectorError {
            message: message.into(),
            position: self.position(),
        }
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            return true;
        }
        false
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(character) if is_css_whitespace(character)) {
            self.bump();
        }
    }

    fn consume_combinator(&mut self) -> Option<Combinator> {
        let combinator = match self.peek()? {
            '>' => Combinator::Child,
            '+' => Combinator::NextSibling,
            '~' => Combinator::SubsequentSibling,
            _ => return None,
        };
        self.bump();
        Some(combinator)
    }

    fn valid_escape(&self, offset: usize) -> bool {
        self.peek_at(offset) == Some('\\')
            && !matches!(self.peek_at(offset + 1), None | Some('\n' | '\r' | '\x0c'))
    }

    fn starts_ident(&self) -> bool {
        match self.peek() {
            Some('-') => match self.peek_at(1) {
                Some(character) if is_name_start(character) || character == '-' => true,
                Some('\\') => self.valid_escape(1),
                _ => false,
            },
            Some(character) if is_name_start(character) => true,
            Some('\\') => self.valid_escape(0),
            _ => false,
        }
    }

    fn consume_ident(&mut self) -> Option<String> {
        if !self.starts_ident() {
            return None;
        }
        let mut out = String::new();
        loop {
            match self.peek() {
                Some('\\') if self.valid_escape(0) => out.push(self.consume_escape()),
                Some(character) if is_name_char(character) => {
                    out.push(character);
                    self.bump();
                }
                _ => break,
            }
        }
        Some(out)
    }

    fn consume_ident_required(&mut self, message: &str) -> Result<String, SelectorError> {
        self.consume_ident().ok_or_else(|| self.error(message))
    }

    fn consume_escape(&mut self) -> char {
        self.bump(); // backslash
        let Some(character) = self.peek() else {
            return '\u{fffd}';
        };
        if character.is_ascii_hexdigit() {
            let mut hex = String::new();
            while hex.len() < 6 {
                match self.peek() {
                    Some(digit) if digit.is_ascii_hexdigit() => {
                        hex.push(digit);
                        self.bump();
                    }
                    _ => break,
                }
            }
            if matches!(self.peek(), Some(current) if is_css_whitespace(current)) {
                self.bump();
            }
            u32::from_str_radix(&hex, 16)
                .ok()
                .and_then(char::from_u32)
                .unwrap_or('\u{fffd}')
        } else {
            self.bump();
            character
        }
    }

    fn consume_string(&mut self) -> Result<String, SelectorError> {
        let start = self.position();
        let quote = self.bump().unwrap_or('"');
        let mut out = String::new();
        while let Some(character) = self.peek() {
            if character == quote {
                self.bump();
                return Ok(out);
            }
            if character == '\\' {
                if self.valid_escape(0) {
                    out.push(self.consume_escape());
                } else {
                    // Escaped newline: a line continuation.
                    self.bump();
                    self.bump();
                }
                continue;
            }
            if matches!(character, '\n' | '\r' | '\x0c') {
                break;
            }
            out.push(character);
            self.bump();
        }
        Err(SelectorError {
            message: "unterminated string".to_string(),
            position: start,
        })
    }

    /// Consumes a parenthesized block, returning its content and its absolute
    /// byte offset in the original input.
    fn consume_parenthesized(&mut self) -> Result<(&'a str, usize), SelectorError> {
        if !self.eat('(') {
            return Err(self.error("expected '('"));
        }
        let start = self.pos;
        let mut depth = 0i32;
        let mut quote = None;
        let mut escaped = false;

        while let Some(character) = self.peek() {
            if escaped {
                escaped = false;
                self.bump();
                continue;
            }
            if character == '\\' {
                escaped = true;
                self.bump();
                continue;
            }
            if let Some(open) = quote {
                if character == open {
                    quote = None;
                }
                self.bump();
                continue;
            }
            match character {
                '"' | '\'' => {
                    quote = Some(character);
                    self.bump();
                }
                '(' | '[' => {
                    depth += 1;
                    self.bump();
                }
                ')' if depth == 0 => {
                    let content = &self.text[start..self.pos];
                    self.bump();
                    return Ok((content, self.base + start));
                }
                ')' | ']' => {
                    depth = (depth - 1).max(0);
                    self.bump();
                }
                _ => {
                    self.bump();
                }
            }
        }

        Err(self.error("unclosed parenthesis"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Combinator;

    fn selector(text: &str) -> Selector {
        parse_selector(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn parses_type_and_universal() {
        let parsed = selector("div");
        assert_eq!(parsed.parts.len(), 1);
        assert_eq!(
            parsed.parts[0].1.simples,
            vec![SimpleSelector::Type("div".into())]
        );
        assert_eq!(parsed.specificity, Specificity::new(0, 0, 1));
        assert_eq!(selector("*").specificity, Specificity::default());
    }

    #[test]
    fn parses_id_and_class() {
        let parsed = selector("div#main.box.wide");
        assert_eq!(
            parsed.parts[0].1.simples,
            vec![
                SimpleSelector::Type("div".into()),
                SimpleSelector::Id("main".into()),
                SimpleSelector::Class("box".into()),
                SimpleSelector::Class("wide".into()),
            ]
        );
        assert_eq!(parsed.specificity, Specificity::new(1, 2, 1));
    }

    #[test]
    fn parses_combinators() {
        assert_eq!(selector("a b").parts[1].0, Some(Combinator::Descendant));
        assert_eq!(selector("a > b").parts[1].0, Some(Combinator::Child));
        assert_eq!(selector("a+b").parts[1].0, Some(Combinator::NextSibling));
        assert_eq!(
            selector("a ~ b").parts[1].0,
            Some(Combinator::SubsequentSibling)
        );
        assert_eq!(selector("a>b").parts.len(), 2);
    }

    #[test]
    fn keeps_selector_text_trimmed() {
        assert_eq!(selector("  div > p  ").text, "div > p");
    }

    #[test]
    fn parses_attribute_operators() {
        let cases = [
            ("[a]", AttrOp::Exists, None),
            ("[a=b]", AttrOp::Equals, Some("b")),
            ("[a~=b]", AttrOp::Includes, Some("b")),
            ("[a|=b]", AttrOp::DashMatch, Some("b")),
            ("[a^=b]", AttrOp::Prefix, Some("b")),
            ("[a$=b]", AttrOp::Suffix, Some("b")),
            ("[a*=b]", AttrOp::Substring, Some("b")),
        ];
        for (text, op, value) in cases {
            let parsed = selector(text);
            let SimpleSelector::Attribute {
                name,
                op: parsed_op,
                value: parsed_value,
                ..
            } = &parsed.parts[0].1.simples[0]
            else {
                panic!("expected attribute for {text}");
            };
            assert_eq!(name, "a");
            assert_eq!(*parsed_op, op);
            assert_eq!(parsed_value.as_deref(), value);
        }
    }

    #[test]
    fn attribute_names_are_lowercased_and_values_quoted() {
        let parsed = selector("[DATA-X=\"Hello World\"]");
        let SimpleSelector::Attribute {
            name,
            value,
            case_insensitive,
            ..
        } = &parsed.parts[0].1.simples[0]
        else {
            panic!("expected attribute");
        };
        assert_eq!(name, "data-x");
        assert_eq!(value.as_deref(), Some("Hello World"));
        assert!(!case_insensitive);
    }

    #[test]
    fn parses_attribute_case_flag() {
        let parsed = selector("[data-x=\"AbC\" i]");
        let SimpleSelector::Attribute {
            case_insensitive, ..
        } = &parsed.parts[0].1.simples[0]
        else {
            panic!("expected attribute");
        };
        assert!(case_insensitive);
        assert!(
            !selector("[data-x=a s]").parts[0]
                .1
                .simples
                .iter()
                .any(|simple| matches!(
                    simple,
                    SimpleSelector::Attribute {
                        case_insensitive: true,
                        ..
                    }
                ))
        );
    }

    #[test]
    fn parses_structural_pseudo_classes() {
        for (text, expected) in [
            (":first-child", PseudoClass::FirstChild),
            (":last-child", PseudoClass::LastChild),
            (":only-child", PseudoClass::OnlyChild),
            (":root", PseudoClass::Root),
            (":empty", PseudoClass::Empty),
            (":first-of-type", PseudoClass::FirstOfType),
            (":last-of-type", PseudoClass::LastOfType),
            (":only-of-type", PseudoClass::OnlyOfType),
            (":hover", PseudoClass::Hover),
            (":focus", PseudoClass::Focus),
            (":checked", PseudoClass::Checked),
            (":enabled", PseudoClass::Enabled),
            (":any-link", PseudoClass::AnyLink),
        ] {
            assert_eq!(
                selector(text).parts[0].1.simples[0],
                SimpleSelector::Pseudo(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn parses_nth_formulas() {
        for (text, a, b) in [
            (":nth-child(odd)", 2, 1),
            (":nth-child(even)", 2, 0),
            (":nth-child(2n+1)", 2, 1),
            (":nth-child(-n+3)", -1, 3),
            (":nth-child( 2n + 1 )", 2, 1),
            (":nth-child(0n+2)", 0, 2),
            (":nth-child(n)", 1, 0),
            (":nth-child(5)", 0, 5),
            (":nth-child(-2n+5)", -2, 5),
            (":nth-child(3n - 1)", 3, -1),
            (":nth-last-child(2n)", 2, 0),
        ] {
            let parsed = selector(text);
            let expected = if text.contains("last") {
                PseudoClass::NthLastChild(a, b)
            } else {
                PseudoClass::NthChild(a, b)
            };
            assert_eq!(
                parsed.parts[0].1.simples[0],
                SimpleSelector::Pseudo(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn nth_child_allows_of_clause() {
        let parsed = selector(":nth-child(2n+1 of .item)");
        assert_eq!(
            parsed.parts[0].1.simples[0],
            SimpleSelector::Pseudo(PseudoClass::NthChildOf(
                2,
                1,
                vec![parse_selector(".item").unwrap()]
            ))
        );
        // :nth-child keeps its original shape when there is no `of` clause.
        assert_eq!(
            selector(":nth-child(2n+1)").parts[0].1.simples[0],
            SimpleSelector::Pseudo(PseudoClass::NthChild(2, 1))
        );
    }

    #[test]
    fn parses_nth_of_clause_variants() {
        let parsed = selector(":nth-last-child(2 of li.foo, div)");
        let SimpleSelector::Pseudo(PseudoClass::NthLastChildOf(a, b, list)) =
            &parsed.parts[0].1.simples[0]
        else {
            panic!("expected :nth-last-child of");
        };
        assert_eq!((*a, *b), (0, 2));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].text, "li.foo");
        assert_eq!(list[1].text, "div");
    }

    #[test]
    fn rejects_invalid_of_clause_and_direction() {
        assert!(parse_selector(":nth-child(2n of)").is_err());
        assert!(parse_selector(":dir(auto)").is_err());
        assert!(parse_selector(":dir(up)").is_err());
    }

    #[test]
    fn parses_new_pseudo_classes() {
        for (text, expected) in [
            (":focus-visible", PseudoClass::FocusVisible),
            (":focus-within", PseudoClass::FocusWithin),
            (":valid", PseudoClass::Valid),
            (":invalid", PseudoClass::Invalid),
            (":required", PseudoClass::Required),
            (":optional", PseudoClass::Optional),
            (":placeholder-shown", PseudoClass::PlaceholderShown),
            (":read-only", PseudoClass::ReadOnly),
            (":read-write", PseudoClass::ReadWrite),
            (":host", PseudoClass::Host),
            (":defined", PseudoClass::Defined),
            (":modal", PseudoClass::Modal),
            (":autofill", PseudoClass::Autofill),
            (":user-invalid", PseudoClass::UserInvalid),
            (":dir(ltr)", PseudoClass::Dir(Direction::Ltr)),
            (":dir(RTL)", PseudoClass::Dir(Direction::Rtl)),
        ] {
            assert_eq!(
                selector(text).parts[0].1.simples[0],
                SimpleSelector::Pseudo(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn nth_of_clause_contributes_to_specificity() {
        assert_eq!(
            selector(":nth-child(2n of .item)").specificity,
            Specificity::new(0, 2, 0)
        );
        assert_eq!(
            selector(":nth-child(2n)").specificity,
            Specificity::new(0, 1, 0)
        );
    }

    #[test]
    fn parses_not_is_where() {
        let parsed = selector(":not(.a, #b)");
        let SimpleSelector::Pseudo(PseudoClass::Not(list)) = &parsed.parts[0].1.simples[0] else {
            panic!("expected :not");
        };
        assert_eq!(list.len(), 2);
        assert!(matches!(
            selector(":is(a, b)").parts[0].1.simples[0],
            SimpleSelector::Pseudo(PseudoClass::Is(_))
        ));
        assert!(matches!(
            selector(":where(a)").parts[0].1.simples[0],
            SimpleSelector::Pseudo(PseudoClass::Where(_))
        ));
    }

    #[test]
    fn parses_has_relative_selectors() {
        let parsed = selector(":has(> .child)");
        let SimpleSelector::Pseudo(PseudoClass::Has(list)) = &parsed.parts[0].1.simples[0] else {
            panic!("expected :has");
        };
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].parts.len(), 1);
        assert_eq!(list[0].parts[0].0, Some(Combinator::Child));
        assert_eq!(
            list[0].parts[0].1.simples,
            vec![SimpleSelector::Class("child".into())]
        );

        let parsed = selector(":has(.a .b, + .next, ~ .later)");
        let SimpleSelector::Pseudo(PseudoClass::Has(list)) = &parsed.parts[0].1.simples[0] else {
            panic!("expected :has");
        };
        assert_eq!(list.len(), 3);
        // Implicit descendant leading combinator is stored as `None`.
        assert_eq!(list[0].parts[0].0, None);
        assert_eq!(list[0].parts.len(), 2);
        assert_eq!(list[0].parts[1].0, Some(Combinator::Descendant));
        assert_eq!(list[1].parts[0].0, Some(Combinator::NextSibling));
        assert_eq!(list[2].parts[0].0, Some(Combinator::SubsequentSibling));
    }

    #[test]
    fn parses_nth_of_type_formulas() {
        for (text, expected) in [
            (":nth-of-type(odd)", PseudoClass::NthOfType(2, 1)),
            (":nth-of-type(even)", PseudoClass::NthOfType(2, 0)),
            (":nth-of-type(2n+1)", PseudoClass::NthOfType(2, 1)),
            (":nth-of-type(0n+3)", PseudoClass::NthOfType(0, 3)),
            (":nth-of-type(4)", PseudoClass::NthOfType(0, 4)),
            (":nth-last-of-type(-n+2)", PseudoClass::NthLastOfType(-1, 2)),
            (
                ":nth-last-of-type(3n - 1)",
                PseudoClass::NthLastOfType(3, -1),
            ),
        ] {
            assert_eq!(
                selector(text).parts[0].1.simples[0],
                SimpleSelector::Pseudo(expected),
                "{text}"
            );
        }
    }

    #[test]
    fn parses_target_pseudo_class() {
        assert_eq!(
            selector("section:target").parts[0].1.simples[1],
            SimpleSelector::Pseudo(PseudoClass::Target)
        );
        assert_eq!(selector(":target").specificity, Specificity::new(0, 1, 0));
    }

    #[test]
    fn has_specificity_is_the_most_specific_argument() {
        assert_eq!(
            selector(":has(.a, #b)").specificity,
            Specificity::new(1, 0, 0)
        );
        assert_eq!(
            selector("div:has(> .a)").specificity,
            Specificity::new(0, 1, 1)
        );
        // :where always contributes nothing, even around :has.
        assert_eq!(
            selector(":where(:has(#a)) div").specificity,
            Specificity::new(0, 0, 1)
        );
    }

    #[test]
    fn rejects_invalid_relative_selectors() {
        for text in [":has(>)", ":has(a,)", ":has(+ )", ":has(> > a)"] {
            assert!(parse_selector(text).is_err(), "{text} unexpectedly parsed");
        }
    }

    #[test]
    fn parses_lang_pseudo_class() {
        assert_eq!(
            selector(":lang(en-US)").parts[0].1.simples[0],
            SimpleSelector::Pseudo(PseudoClass::Lang("en-US".into()))
        );
        assert_eq!(
            selector(":lang(\"fr\")").parts[0].1.simples[0],
            SimpleSelector::Pseudo(PseudoClass::Lang("fr".into()))
        );
    }

    #[test]
    fn parses_pseudo_elements() {
        assert_eq!(
            selector("p::before").parts[0].1.simples[1],
            SimpleSelector::PseudoElement("before".into())
        );
        assert_eq!(
            selector("p:after").parts[0].1.simples[1],
            SimpleSelector::PseudoElement("after".into())
        );
        assert_eq!(
            selector("p:first-line").parts[0].1.simples[1],
            SimpleSelector::PseudoElement("first-line".into())
        );
        assert_eq!(
            selector("::part(label)").parts[0].1.simples[0],
            SimpleSelector::PseudoElement("part".into())
        );
        assert_eq!(selector("p::before").specificity, Specificity::new(0, 0, 2));
    }

    #[test]
    fn stores_pseudo_element_flag_on_selector() {
        assert_eq!(
            selector("p::before").pseudo_element.as_deref(),
            Some("before")
        );
        assert_eq!(selector("p:after").pseudo_element.as_deref(), Some("after"));
        assert_eq!(
            selector("div.box::first-line").pseudo_element.as_deref(),
            Some("first-line")
        );
        assert_eq!(selector("p::before").pseudo_element, Some("before".into()));
        assert_eq!(selector("p.icon").pseudo_element, None);
        assert_eq!(selector("p:hover").pseudo_element, None);
    }

    #[test]
    fn decodes_escapes_in_identifiers() {
        assert_eq!(
            selector(".a\\.b").parts[0].1.simples[0],
            SimpleSelector::Class("a.b".into())
        );
        assert_eq!(
            selector("#\\31 23").parts[0].1.simples[0],
            SimpleSelector::Id("123".into())
        );
    }

    #[test]
    fn specificity_follows_level_4_rules() {
        assert_eq!(selector("#a.b c").specificity, Specificity::new(1, 1, 1));
        assert_eq!(
            selector("[a]::before").specificity,
            Specificity::new(0, 1, 1)
        );
        // :where contributes nothing.
        assert_eq!(
            selector(":where(#a, .b) div").specificity,
            Specificity::new(0, 0, 1)
        );
        // :is and :not take their most specific argument.
        assert_eq!(
            selector(":is(.a, #b)").specificity,
            Specificity::new(1, 0, 0)
        );
        assert_eq!(
            selector(":not(div, .a)").specificity,
            Specificity::new(0, 1, 0)
        );
        assert_eq!(
            selector(":nth-child(2n)").specificity,
            Specificity::new(0, 1, 0)
        );
    }

    #[test]
    fn parses_selector_lists() {
        let selectors = parse_selectors("a, .b, #c > d").unwrap();
        assert_eq!(selectors.len(), 3);
        assert_eq!(selectors[2].parts.len(), 2);
    }

    #[test]
    fn rejects_invalid_selectors_with_positions() {
        let cases = [
            ("", 0),
            ("   ", 0),
            ("> a", 0),
            ("a >", 3),
            ("a,,b", 2),
            ("[a", 2),
            ("[a=]", 3),
            ("[a=b", 4),
            (":unknown", 0),
            (":nth-child(foo)", 11),
            ("::", 2),
            ("a::before b", 10),
        ];
        for (text, position) in cases {
            let error = parse_selector(text).expect_err(text);
            assert_eq!(error.position, position, "{text}: {error}");
        }
    }

    #[test]
    fn parse_selector_rejects_lists() {
        let error = parse_selector("a, b").unwrap_err();
        assert_eq!(error.position, 1);
    }

    #[test]
    fn accepts_whitespace_around_attribute_parts() {
        assert!(parse_selector("[ data-x = \"a\" i ]").is_ok());
        assert!(parse_selector("a\t>\nb").is_ok());
    }
}
