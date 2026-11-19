//! CSS tokenizer following CSS Syntax Level 3.
//!
//! The tokenizer never fails: malformed input (unterminated strings or
//! comments, stray backslashes, unterminated blocks) produces best-effort
//! tokens so the parser can recover and skip invalid rules.

/// A single CSS token.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// An identifier such as `color`.
    Ident(String),
    /// A function name including the opening parenthesis, such as `rgb(`.
    Function(String),
    /// An at-keyword without the `@`.
    AtKeyword(String),
    /// A hash token without the `#` (identifiers and hex colors).
    Hash(String),
    /// A quoted string with escapes resolved.
    String(String),
    /// An unquoted `url(...)` value.
    Url(String),
    /// A number such as `12` or `-3.5e2`.
    Number(f64),
    /// A percentage such as `50%`.
    Percentage(f32),
    /// A number with a unit such as `10px`.
    Dimension(f64, String),
    /// A single delimiter character.
    Delim(char),
    /// `,`.
    Comma,
    /// `:`.
    Colon,
    /// `;`.
    Semicolon,
    /// `{`.
    LeftBrace,
    /// `}`.
    RightBrace,
    /// `(`.
    LeftParen,
    /// `)`.
    RightParen,
    /// `[`.
    LeftBracket,
    /// `]`.
    RightBracket,
    /// A run of whitespace.
    Whitespace,
    /// `<!--`.
    Cdo,
    /// `-->`.
    Cdc,
}

/// A token together with its byte span in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    /// The token itself.
    pub token: Token,
    /// Byte offset of the first character.
    pub start: usize,
    /// Byte offset one past the last character.
    pub end: usize,
}

/// Tokenizes `input`, dropping comments.
#[must_use]
pub fn tokenize(input: &str) -> Vec<Token> {
    tokenize_spanned(input)
        .into_iter()
        .map(|spanned| spanned.token)
        .collect()
}

/// Tokenizes `input`, keeping the byte span of each token.
#[must_use]
pub fn tokenize_spanned(input: &str) -> Vec<SpannedToken> {
    let mut lexer = Lexer::new(input);
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token() {
        tokens.push(token);
    }
    tokens
}

/// Converts a token sequence into component values, resolving blocks and
/// functions into nested lists.
#[must_use]
pub fn to_component_values(tokens: &[Token]) -> Vec<crate::values::ComponentValue> {
    let mut index = 0;
    parse_component_list(tokens, &mut index, None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
    Paren,
    Bracket,
    Brace,
}

fn parse_component_list(
    tokens: &[Token],
    index: &mut usize,
    close: Option<Close>,
) -> Vec<crate::values::ComponentValue> {
    use crate::values::ComponentValue;

    let mut out = Vec::new();
    while *index < tokens.len() {
        let token = &tokens[*index];
        let closing = match token {
            Token::RightParen => Some((Close::Paren, ')')),
            Token::RightBracket => Some((Close::Bracket, ']')),
            Token::RightBrace => Some((Close::Brace, '}')),
            _ => None,
        };
        if let Some((kind, character)) = closing {
            *index += 1;
            if Some(kind) == close {
                return out;
            }
            out.push(ComponentValue::Delim(character));
            continue;
        }
        match token {
            Token::LeftParen => {
                *index += 1;
                let inner = parse_component_list(tokens, index, Some(Close::Paren));
                out.push(ComponentValue::Function(String::new(), inner));
            }
            Token::LeftBracket => {
                *index += 1;
                let inner = parse_component_list(tokens, index, Some(Close::Bracket));
                out.push(ComponentValue::Delim('['));
                out.extend(inner);
                out.push(ComponentValue::Delim(']'));
            }
            Token::LeftBrace => {
                *index += 1;
                let inner = parse_component_list(tokens, index, Some(Close::Brace));
                out.push(ComponentValue::Delim('{'));
                out.extend(inner);
                out.push(ComponentValue::Delim('}'));
            }
            Token::Function(name) => {
                *index += 1;
                let inner = parse_component_list(tokens, index, Some(Close::Paren));
                out.push(ComponentValue::Function(name.to_ascii_lowercase(), inner));
            }
            other => {
                *index += 1;
                push_component(other, &mut out);
            }
        }
    }
    out
}

fn push_component(token: &Token, out: &mut Vec<crate::values::ComponentValue>) {
    use crate::values::ComponentValue;

    match token {
        Token::Ident(name) => out.push(ComponentValue::Ident(name.to_ascii_lowercase())),
        Token::Number(value) => out.push(ComponentValue::Number(*value)),
        Token::Percentage(value) => out.push(ComponentValue::Percentage(*value)),
        Token::Dimension(value, unit) => {
            out.push(ComponentValue::Dimension(*value, unit.to_ascii_lowercase()))
        }
        Token::String(value) => out.push(ComponentValue::String(value.clone())),
        Token::Url(value) => out.push(ComponentValue::Url(value.clone())),
        Token::Hash(value) => out.push(ComponentValue::Hash(value.clone())),
        Token::Delim(character) => out.push(ComponentValue::Delim(*character)),
        Token::Comma => out.push(ComponentValue::Comma),
        Token::Whitespace => out.push(ComponentValue::Whitespace),
        Token::Colon => out.push(ComponentValue::Delim(':')),
        Token::Semicolon => out.push(ComponentValue::Delim(';')),
        Token::AtKeyword(name) => {
            out.push(ComponentValue::Delim('@'));
            out.push(ComponentValue::Ident(name.to_ascii_lowercase()));
        }
        // Structural tokens are handled by the caller; CDO/CDC are noise.
        Token::Function(_)
        | Token::LeftBrace
        | Token::RightBrace
        | Token::LeftParen
        | Token::RightParen
        | Token::LeftBracket
        | Token::RightBracket
        | Token::Cdo
        | Token::Cdc => {}
    }
}

fn is_name_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_' || !character.is_ascii()
}

fn is_name_char(character: char) -> bool {
    is_name_start(character) || character.is_ascii_digit() || character == '-'
}

fn is_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

struct Lexer<'a> {
    input: &'a str,
    chars: Vec<(usize, char)>,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.char_indices().collect(),
            pos: 0,
        }
    }

    fn offset(&self) -> usize {
        self.chars
            .get(self.pos)
            .map_or(self.input.len(), |(offset, _)| *offset)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).map(|(_, character)| *character)
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars
            .get(self.pos + offset)
            .map(|(_, character)| *character)
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.pos += 1;
        Some(character)
    }

    fn valid_escape_at(&self, offset: usize) -> bool {
        self.peek_at(offset) == Some('\\')
            && !matches!(self.peek_at(offset + 1), None | Some('\n' | '\r' | '\x0c'))
    }

    fn starts_ident(&self) -> bool {
        match self.peek() {
            Some('-') => match self.peek_at(1) {
                Some(character) if is_name_start(character) || character == '-' => true,
                Some('\\') => self.valid_escape_at(1),
                _ => false,
            },
            Some(character) if is_name_start(character) => true,
            Some('\\') => self.valid_escape_at(0),
            _ => false,
        }
    }

    fn starts_number(&self) -> bool {
        match self.peek() {
            Some(character) if character.is_ascii_digit() => true,
            Some('+') | Some('-') => match self.peek_at(1) {
                Some(character) if character.is_ascii_digit() => true,
                Some('.') => self.peek_at(2).is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            },
            Some('.') => self.peek_at(1).is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        }
    }

    fn next_token(&mut self) -> Option<SpannedToken> {
        loop {
            let start = self.offset();
            let character = self.peek()?;

            if character == '/' && self.peek_at(1) == Some('*') {
                self.pos += 2;
                while let Some(current) = self.peek() {
                    if current == '*' && self.peek_at(1) == Some('/') {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }

            let token = match character {
                c if is_whitespace(c) => {
                    while matches!(self.peek(), Some(current) if is_whitespace(current)) {
                        self.pos += 1;
                    }
                    Token::Whitespace
                }
                '<' if self.starts_with("<!--") => {
                    self.pos += 4;
                    Token::Cdo
                }
                '-' if self.starts_with("-->") => {
                    self.pos += 3;
                    Token::Cdc
                }
                '"' | '\'' => Token::String(self.consume_string()),
                '#' => {
                    if self
                        .peek_at(1)
                        .is_some_and(|c| is_name_char(c) || (c == '\\' && self.valid_escape_at(1)))
                    {
                        self.pos += 1;
                        Token::Hash(self.consume_name())
                    } else {
                        self.pos += 1;
                        Token::Delim('#')
                    }
                }
                '(' => {
                    self.pos += 1;
                    Token::LeftParen
                }
                ')' => {
                    self.pos += 1;
                    Token::RightParen
                }
                '[' => {
                    self.pos += 1;
                    Token::LeftBracket
                }
                ']' => {
                    self.pos += 1;
                    Token::RightBracket
                }
                '{' => {
                    self.pos += 1;
                    Token::LeftBrace
                }
                '}' => {
                    self.pos += 1;
                    Token::RightBrace
                }
                ',' => {
                    self.pos += 1;
                    Token::Comma
                }
                ':' => {
                    self.pos += 1;
                    Token::Colon
                }
                ';' => {
                    self.pos += 1;
                    Token::Semicolon
                }
                '@' => {
                    if self.starts_ident_at(1) {
                        self.pos += 1;
                        Token::AtKeyword(self.consume_name())
                    } else {
                        self.pos += 1;
                        Token::Delim('@')
                    }
                }
                '\\' => {
                    if self.valid_escape_at(0) {
                        self.consume_ident_like()
                    } else {
                        self.pos += 1;
                        Token::Delim('\\')
                    }
                }
                '+' | '.' => {
                    if self.starts_number() {
                        self.consume_numeric()
                    } else {
                        self.pos += 1;
                        Token::Delim(character)
                    }
                }
                '-' => {
                    if self.starts_number() {
                        self.consume_numeric()
                    } else if self.starts_ident() {
                        self.consume_ident_like()
                    } else {
                        self.pos += 1;
                        Token::Delim('-')
                    }
                }
                c if c.is_ascii_digit() => self.consume_numeric(),
                c if is_name_start(c) => self.consume_ident_like(),
                c => {
                    self.bump();
                    Token::Delim(c)
                }
            };

            let end = self.offset();
            return Some(SpannedToken { token, start, end });
        }
    }

    fn starts_with(&self, needle: &str) -> bool {
        let mut index = self.pos;
        for expected in needle.chars() {
            match self.chars.get(index) {
                Some((_, character)) if *character == expected => index += 1,
                _ => return false,
            }
        }
        true
    }

    fn starts_ident_at(&self, offset: usize) -> bool {
        match self.peek_at(offset) {
            Some(character) if is_name_start(character) => true,
            Some('-') => match self.peek_at(offset + 1) {
                Some(character) if is_name_start(character) || character == '-' => true,
                Some('\\') => self.valid_escape_at(offset + 1),
                _ => false,
            },
            Some('\\') => self.valid_escape_at(offset),
            _ => false,
        }
    }

    fn consume_ident_like(&mut self) -> Token {
        let name = self.consume_name();
        if self.peek() == Some('(') {
            self.pos += 1;
            if name.eq_ignore_ascii_case("url") {
                Token::Url(self.consume_url())
            } else {
                Token::Function(name)
            }
        } else {
            Token::Ident(name)
        }
    }

    fn consume_name(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek() {
                Some('\\') if self.valid_escape_at(0) => out.push(self.consume_escape()),
                Some(character) if is_name_char(character) => {
                    out.push(character);
                    self.pos += 1;
                }
                _ => break,
            }
        }
        out
    }

    fn consume_escape(&mut self) -> char {
        // The caller guarantees `self.pos` points at the backslash.
        self.pos += 1;
        let Some(character) = self.peek() else {
            return '\u{fffd}';
        };
        if character.is_ascii_hexdigit() {
            let mut hex = String::new();
            while hex.len() < 6 {
                match self.peek() {
                    Some(digit) if digit.is_ascii_hexdigit() => {
                        hex.push(digit);
                        self.pos += 1;
                    }
                    _ => break,
                }
            }
            if matches!(self.peek(), Some(current) if is_whitespace(current)) {
                self.pos += 1;
            }
            u32::from_str_radix(&hex, 16)
                .ok()
                .and_then(char::from_u32)
                .unwrap_or('\u{fffd}')
        } else {
            self.pos += 1;
            character
        }
    }

    fn consume_string(&mut self) -> String {
        let quote = self.bump().unwrap_or('"');
        let mut out = String::new();
        while let Some(character) = self.peek() {
            if character == quote {
                self.pos += 1;
                break;
            }
            if character == '\\' {
                match self.peek_at(1) {
                    None => {
                        self.pos += 1;
                        break;
                    }
                    Some('\n') => self.pos += 2,
                    Some('\r') => {
                        self.pos += 2;
                        if self.peek() == Some('\n') {
                            self.pos += 1;
                        }
                    }
                    Some('\x0c') => self.pos += 2,
                    Some(_) => out.push(self.consume_escape()),
                }
                continue;
            }
            if matches!(character, '\n' | '\r' | '\x0c') {
                break;
            }
            out.push(character);
            self.pos += 1;
        }
        out
    }

    fn consume_url(&mut self) -> String {
        while matches!(self.peek(), Some(current) if is_whitespace(current)) {
            self.pos += 1;
        }
        if matches!(self.peek(), Some('"' | '\'')) {
            let value = self.consume_string();
            while matches!(self.peek(), Some(current) if is_whitespace(current)) {
                self.pos += 1;
            }
            if self.peek() == Some(')') {
                self.pos += 1;
            }
            return value;
        }
        let mut out = String::new();
        loop {
            match self.peek() {
                None => break,
                Some(')') => {
                    self.pos += 1;
                    break;
                }
                Some(character) if is_whitespace(character) => {
                    while matches!(self.peek(), Some(current) if is_whitespace(current)) {
                        self.pos += 1;
                    }
                    if self.peek() == Some(')') {
                        self.pos += 1;
                        break;
                    }
                }
                Some('\\') if self.valid_escape_at(0) => out.push(self.consume_escape()),
                Some(character) => {
                    out.push(character);
                    self.pos += 1;
                }
            }
        }
        out
    }

    fn consume_numeric(&mut self) -> Token {
        let value = self.consume_number();
        if self.peek() == Some('%') {
            self.pos += 1;
            return Token::Percentage(value as f32);
        }
        if self.starts_ident() {
            let unit = self.consume_name();
            return Token::Dimension(value, unit);
        }
        Token::Number(value)
    }

    fn consume_number(&mut self) -> f64 {
        let mut repr = String::new();
        if let Some(character @ ('+' | '-')) = self.peek() {
            repr.push(character);
            self.pos += 1;
        }
        while let Some(character) = self.peek() {
            if character.is_ascii_digit() {
                repr.push(character);
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            repr.push('.');
            self.pos += 1;
            while let Some(character) = self.peek() {
                if character.is_ascii_digit() {
                    repr.push(character);
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let exponent_starts = match self.peek() {
            Some('e' | 'E') => {
                self.peek_at(1).is_some_and(|c| c.is_ascii_digit())
                    || (matches!(self.peek_at(1), Some('+' | '-'))
                        && self.peek_at(2).is_some_and(|c| c.is_ascii_digit()))
            }
            _ => false,
        };
        if exponent_starts {
            repr.push('e');
            self.pos += 1;
            if let Some(sign @ ('+' | '-')) = self.peek() {
                repr.push(sign);
                self.pos += 1;
            }
            while let Some(character) = self.peek() {
                if character.is_ascii_digit() {
                    repr.push(character);
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        repr.parse::<f64>().unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &str) -> Vec<Token> {
        tokenize(input)
    }

    #[test]
    fn tokenizes_basic_punctuation() {
        assert_eq!(
            kinds("a{b:c;}"),
            vec![
                Token::Ident("a".into()),
                Token::LeftBrace,
                Token::Ident("b".into()),
                Token::Colon,
                Token::Ident("c".into()),
                Token::Semicolon,
                Token::RightBrace,
            ]
        );
    }

    #[test]
    fn collapses_whitespace_runs() {
        assert_eq!(
            kinds("a \t\n  b"),
            vec![
                Token::Ident("a".into()),
                Token::Whitespace,
                Token::Ident("b".into()),
            ]
        );
    }

    #[test]
    fn drops_comments_but_keeps_boundaries() {
        assert_eq!(
            kinds("a/* x */b"),
            vec![Token::Ident("a".into()), Token::Ident("b".into())]
        );
        // An unterminated comment simply runs to the end.
        assert_eq!(
            kinds("a /* unterminated"),
            vec![Token::Ident("a".into()), Token::Whitespace]
        );
    }

    #[test]
    fn tokenizes_numbers_dimensions_and_percentages() {
        assert_eq!(
            kinds("1 -2.5 +.5 10px 50% 1e3 2.5e-2"),
            vec![
                Token::Number(1.0),
                Token::Whitespace,
                Token::Number(-2.5),
                Token::Whitespace,
                Token::Number(0.5),
                Token::Whitespace,
                Token::Dimension(10.0, "px".into()),
                Token::Whitespace,
                Token::Percentage(50.0),
                Token::Whitespace,
                Token::Number(1000.0),
                Token::Whitespace,
                Token::Number(0.025),
            ]
        );
    }

    #[test]
    fn distinguishes_ident_from_number_starts() {
        assert_eq!(kinds("-foo"), vec![Token::Ident("-foo".into())]);
        assert_eq!(kinds("-5px"), vec![Token::Dimension(-5.0, "px".into())]);
        assert_eq!(
            kinds(".a"),
            vec![Token::Delim('.'), Token::Ident("a".into())]
        );
    }

    #[test]
    fn tokenizes_strings_with_escapes() {
        assert_eq!(
            kinds("\"a\\\"b\" 'c\\64 '"),
            vec![
                Token::String("a\"b".into()),
                Token::Whitespace,
                Token::String("cd".into()),
            ]
        );
    }

    #[test]
    fn handles_line_continuations_in_strings() {
        assert_eq!(kinds("\"a\\\nb\""), vec![Token::String("ab".into())]);
    }

    #[test]
    fn unterminated_string_is_best_effort() {
        assert_eq!(kinds("\"abc"), vec![Token::String("abc".into())]);
    }

    #[test]
    fn tokenizes_url_tokens() {
        assert_eq!(
            kinds("url(foo/bar.png) url( 'x.png' )"),
            vec![
                Token::Url("foo/bar.png".into()),
                Token::Whitespace,
                Token::Url("x.png".into()),
            ]
        );
    }

    #[test]
    fn tokenizes_functions_and_at_keywords() {
        assert_eq!(
            kinds("@media calc(1px+2px)"),
            vec![
                Token::AtKeyword("media".into()),
                Token::Whitespace,
                Token::Function("calc".into()),
                Token::Dimension(1.0, "px".into()),
                Token::Dimension(2.0, "px".into()),
                Token::RightParen,
            ]
        );
    }

    #[test]
    fn tokenizes_hashes_and_escaped_idents() {
        assert_eq!(
            kinds("#abc \\31 23 .a\\.b"),
            vec![
                Token::Hash("abc".into()),
                Token::Whitespace,
                Token::Ident("123".into()),
                Token::Whitespace,
                Token::Delim('.'),
                Token::Ident("a.b".into()),
            ]
        );
    }

    #[test]
    fn escaped_newline_is_a_delim_backslash() {
        assert_eq!(
            kinds("a\\\nb"),
            vec![
                Token::Ident("a".into()),
                Token::Delim('\\'),
                Token::Whitespace,
                Token::Ident("b".into()),
            ]
        );
    }

    #[test]
    fn recognizes_cdo_and_cdc() {
        assert_eq!(
            kinds("<!-- -->"),
            vec![Token::Cdo, Token::Whitespace, Token::Cdc]
        );
    }

    #[test]
    fn arbitrary_input_never_panics() {
        for input in [
            "", "\\", "/*", "\"", "'", "@", "#", "url(", "(){}[]", "1e", "1e+",
        ] {
            let _ = tokenize(input);
        }
    }

    #[test]
    fn builds_nested_component_values() {
        let values = to_component_values(&tokenize("rgb(1, 2, 3) calc(1px + 2px)"));
        assert!(
            matches!(&values[0], crate::values::ComponentValue::Function(name, _) if name == "rgb")
        );
        assert!(
            matches!(&values[2], crate::values::ComponentValue::Function(name, _) if name == "calc")
        );
    }

    #[test]
    fn tokenizes_math_functions_as_generic_functions() {
        for name in ["min", "max", "clamp", "calc"] {
            assert_eq!(
                kinds(&format!("{name}(1, 2)")),
                vec![
                    Token::Function(name.into()),
                    Token::Number(1.0),
                    Token::Comma,
                    Token::Whitespace,
                    Token::Number(2.0),
                    Token::RightParen,
                ],
                "{name}"
            );
        }
    }

    #[test]
    fn builds_math_function_component_values() {
        let values = to_component_values(&tokenize(
            "MIN(1px, 2px) max(10%, 20px) clamp(1rem, 2vw, 3rem)",
        ));
        let names: Vec<&str> = values
            .iter()
            .filter_map(|component| match component {
                crate::values::ComponentValue::Function(name, _) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["min", "max", "clamp"]);

        let crate::values::ComponentValue::Function(_, args) = &values[2] else {
            panic!("expected a function");
        };
        assert!(args.contains(&crate::values::ComponentValue::Comma));
        assert!(args.contains(&crate::values::ComponentValue::Percentage(10.0)));
        assert!(args.contains(&crate::values::ComponentValue::Dimension(20.0, "px".into())));
    }

    #[test]
    fn preserves_calc_operators_and_nested_parentheses() {
        let tokens = tokenize("calc((100% - 10px) * 2 / 3)");
        assert_eq!(
            tokens,
            vec![
                Token::Function("calc".into()),
                Token::LeftParen,
                Token::Percentage(100.0),
                Token::Whitespace,
                Token::Delim('-'),
                Token::Whitespace,
                Token::Dimension(10.0, "px".into()),
                Token::RightParen,
                Token::Whitespace,
                Token::Delim('*'),
                Token::Whitespace,
                Token::Number(2.0),
                Token::Whitespace,
                Token::Delim('/'),
                Token::Whitespace,
                Token::Number(3.0),
                Token::RightParen,
            ]
        );

        let values = to_component_values(&tokens);
        let crate::values::ComponentValue::Function(name, args) = &values[0] else {
            panic!("expected a function");
        };
        assert_eq!(name, "calc");
        assert!(
            args.iter().any(|component| matches!(
                component,
                crate::values::ComponentValue::Function(inner_name, _) if inner_name.is_empty()
            )),
            "nested parentheses must become an empty-named function"
        );
        assert!(args.contains(&crate::values::ComponentValue::Delim('*')));
        assert!(args.contains(&crate::values::ComponentValue::Delim('/')));
    }
}
