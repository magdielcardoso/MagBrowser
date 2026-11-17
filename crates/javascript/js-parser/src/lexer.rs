//! Hand written JavaScript lexer.

use crate::ast::Position;
use crate::parser::ParseError;
use crate::syntax::Keyword;

/// A punctuator or operator token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Punct {
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semi,
    Comma,
    Dot,
    Colon,
    Question,
    Arrow,
    Ellipsis,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    Bang,
    Tilde,
    Amp,
    Pipe,
    Caret,
    Lt,
    Gt,
    LtEq,
    GtEq,
    EqEq,
    BangEq,
    EqEqEq,
    BangEqEq,
    AmpAmp,
    PipePipe,
    QuestionQuestion,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    StarStarEq,
    AmpEq,
    PipeEq,
    CaretEq,
    LtLt,
    GtGt,
    GtGtGt,
    LtLtEq,
    GtGtEq,
    GtGtGtEq,
    PlusPlus,
    MinusMinus,
}

impl Punct {
    /// The source text of the punctuator.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::Semi => ";",
            Self::Comma => ",",
            Self::Dot => ".",
            Self::Colon => ":",
            Self::Question => "?",
            Self::Arrow => "=>",
            Self::Ellipsis => "...",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Slash => "/",
            Self::Percent => "%",
            Self::StarStar => "**",
            Self::Bang => "!",
            Self::Tilde => "~",
            Self::Amp => "&",
            Self::Pipe => "|",
            Self::Caret => "^",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::LtEq => "<=",
            Self::GtEq => ">=",
            Self::EqEq => "==",
            Self::BangEq => "!=",
            Self::EqEqEq => "===",
            Self::BangEqEq => "!==",
            Self::AmpAmp => "&&",
            Self::PipePipe => "||",
            Self::QuestionQuestion => "??",
            Self::Eq => "=",
            Self::PlusEq => "+=",
            Self::MinusEq => "-=",
            Self::StarEq => "*=",
            Self::SlashEq => "/=",
            Self::PercentEq => "%=",
            Self::StarStarEq => "**=",
            Self::AmpEq => "&=",
            Self::PipeEq => "|=",
            Self::CaretEq => "^=",
            Self::LtLt => "<<",
            Self::GtGt => ">>",
            Self::GtGtGt => ">>>",
            Self::LtLtEq => "<<=",
            Self::GtGtEq => ">>=",
            Self::GtGtGtEq => ">>>=",
            Self::PlusPlus => "++",
            Self::MinusMinus => "--",
        }
    }
}

/// A lexical token.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    /// Numeric literal.
    Number(f64),
    /// String literal with escapes already processed.
    String(String),
    /// A template literal chunk. `tail` is set when the chunk ends the literal.
    TemplatePart {
        /// Cooked text of the chunk.
        cooked: String,
        /// Whether this chunk ends the template literal.
        tail: bool,
    },
    /// Identifier or contextual keyword.
    Identifier(String),
    /// Regular expression literal.
    Regex {
        /// Pattern text without the surrounding slashes.
        pattern: String,
        /// Flag characters.
        flags: String,
    },
    /// Reserved word.
    Keyword(Keyword),
    /// Punctuator or operator.
    Punct(Punct),
    /// End of input.
    Eof,
}

impl TokenKind {
    /// Human readable name used in error messages.
    pub fn describe(&self) -> String {
        match self {
            Self::Number(n) => format!("number `{n}`"),
            Self::String(s) => format!("string `{s}`"),
            Self::TemplatePart { .. } => "template literal".to_owned(),
            Self::Identifier(name) => format!("identifier `{name}`"),
            Self::Regex { pattern, .. } => format!("regular expression `/{pattern}/`"),
            Self::Keyword(kw) => format!("keyword `{kw}`"),
            Self::Punct(p) => format!("`{}`", p.as_str()),
            Self::Eof => "end of input".to_owned(),
        }
    }
}

/// A token together with its source position.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    /// The token payload.
    pub kind: TokenKind,
    /// Position of the first character of the token.
    pub position: Position,
    /// Whether a line terminator appeared before the token.
    pub newline_before: bool,
}

impl Token {
    /// Whether the token is the given punctuator.
    pub fn is_punct(&self, punct: Punct) -> bool {
        self.kind == TokenKind::Punct(punct)
    }
}

/// A streaming JavaScript lexer.
pub struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    column: usize,
    newline_before: bool,
    /// Brace depth per open template interpolation.
    template_stack: Vec<usize>,
    /// Whether a `/` at the current position would start a regular expression.
    regex_allowed: bool,
    /// For every open parenthesis, whether it follows a control keyword. The
    /// closing `)` of such a parenthesis ends a statement header, after which an
    /// expression (and so a regular expression) may start.
    paren_stack: Vec<bool>,
    /// The most recently produced token kind, used for regex disambiguation.
    last_kind: Option<TokenKind>,
    source_len: usize,
    _source: &'a str,
}

impl<'a> Lexer<'a> {
    /// Create a lexer over `source`.
    pub fn new(source: &'a str) -> Self {
        let chars: Vec<char> = source.chars().collect();
        let source_len = chars.len();
        Self {
            chars,
            pos: 0,
            line: 1,
            column: 1,
            newline_before: false,
            template_stack: Vec::new(),
            regex_allowed: true,
            paren_stack: Vec::new(),
            last_kind: None,
            source_len,
            _source: source,
        }
    }

    /// Current line number.
    pub fn line(&self) -> usize {
        self.line
    }

    /// Current column number.
    pub fn column(&self) -> usize {
        self.column
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn position(&self) -> Position {
        Position {
            line: self.line,
            column: self.column,
            offset: self.pos,
        }
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError::new(message, self.line, self.column)
    }

    fn skip_trivia(&mut self) -> Result<(), ParseError> {
        loop {
            match self.peek_char() {
                Some('\n') | Some('\r') => {
                    self.newline_before = true;
                    self.bump();
                }
                Some(ch) if ch == ' ' || ch == '\t' || ch == '\u{000B}' || ch == '\u{000C}' => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    while let Some(ch) = self.peek_char() {
                        if ch == '\n' || ch == '\r' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    let start_line = self.line;
                    let start_column = self.column;
                    self.bump();
                    self.bump();
                    let mut closed = false;
                    while let Some(ch) = self.bump() {
                        if ch == '\n' {
                            self.newline_before = true;
                        }
                        if ch == '*' && self.peek_char() == Some('/') {
                            self.bump();
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return Err(ParseError::new(
                            "unterminated block comment",
                            start_line,
                            start_column,
                        ));
                    }
                }
                Some(ch) if ch.is_whitespace() => {
                    self.bump();
                }
                Some('\u{FEFF}') => {
                    self.bump();
                }
                _ => return Ok(()),
            }
        }
    }

    /// Produce the next token.
    pub fn next_token(&mut self) -> Result<Token, ParseError> {
        self.skip_trivia()?;
        let position = self.position();
        let newline_before = std::mem::take(&mut self.newline_before);
        if self.pos >= self.source_len {
            return Ok(Token {
                kind: TokenKind::Eof,
                position,
                newline_before,
            });
        }
        let kind = self.scan_token()?;
        self.update_regex_context(&kind);
        self.last_kind = Some(kind.clone());
        Ok(Token {
            kind,
            position,
            newline_before,
        })
    }

    /// Decide whether the token just scanned ends an operand. After an operand a
    /// `/` is division; anywhere else it starts a regular expression literal.
    fn update_regex_context(&mut self, kind: &TokenKind) {
        self.regex_allowed = match kind {
            TokenKind::Number(_) | TokenKind::String(_) | TokenKind::Regex { .. } => false,
            TokenKind::Identifier(_) => false,
            TokenKind::Keyword(keyword) => !matches!(
                keyword,
                Keyword::This | Keyword::True | Keyword::False | Keyword::Null
            ),
            TokenKind::TemplatePart { tail, .. } => !tail,
            TokenKind::Punct(punct) => match punct {
                Punct::RParen => self.paren_stack.pop().unwrap_or(false),
                Punct::RBracket => false,
                // A closing brace usually ends a block, after which an
                // expression may start.
                Punct::RBrace => true,
                Punct::PlusPlus | Punct::MinusMinus => false,
                _ => true,
            },
            TokenKind::Eof => false,
        };
    }

    fn scan_token(&mut self) -> Result<TokenKind, ParseError> {
        let ch = self.peek_char().expect("caller checked for end of input");
        if ch.is_ascii_digit() || (ch == '.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()))
        {
            return self.scan_number();
        }
        if ch == '"' || ch == '\'' {
            return self.scan_string(ch);
        }
        if ch == '`' {
            self.bump();
            return self.scan_template_part();
        }
        if is_ident_start(ch) {
            return Ok(self.scan_identifier());
        }
        if ch == '/' && self.regex_allowed {
            return self.scan_regex();
        }
        self.scan_punct()
    }

    fn scan_regex(&mut self) -> Result<TokenKind, ParseError> {
        let start_line = self.line;
        let start_column = self.column;
        self.bump();
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            let ch = match self.bump() {
                Some(ch) => ch,
                None => {
                    return Err(ParseError::new(
                        "unterminated regular expression literal",
                        start_line,
                        start_column,
                    ));
                }
            };
            if ch == '\n' || ch == '\r' {
                return Err(ParseError::new(
                    "unterminated regular expression literal",
                    start_line,
                    start_column,
                ));
            }
            if ch == '\\' {
                let escaped = self.bump().ok_or_else(|| {
                    ParseError::new(
                        "unterminated regular expression literal",
                        start_line,
                        start_column,
                    )
                })?;
                if escaped == '\n' || escaped == '\r' {
                    return Err(ParseError::new(
                        "unterminated regular expression literal",
                        start_line,
                        start_column,
                    ));
                }
                pattern.push('\\');
                pattern.push(escaped);
                continue;
            }
            if ch == '[' {
                in_class = true;
                pattern.push(ch);
                continue;
            }
            if ch == ']' {
                in_class = false;
                pattern.push(ch);
                continue;
            }
            if ch == '/' && !in_class {
                break;
            }
            pattern.push(ch);
        }
        let mut flags = String::new();
        while let Some(ch) = self.peek_char() {
            if ch.is_alphabetic() {
                flags.push(ch);
                self.bump();
            } else {
                break;
            }
        }
        Ok(TokenKind::Regex { pattern, flags })
    }

    fn scan_identifier(&mut self) -> TokenKind {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if is_ident_continue(ch) {
                self.bump();
            } else {
                break;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        match Keyword::from_text(&text) {
            Some(keyword) => TokenKind::Keyword(keyword),
            None => TokenKind::Identifier(text),
        }
    }

    fn scan_number(&mut self) -> Result<TokenKind, ParseError> {
        let start = self.pos;
        let mut radix = 10;
        if self.peek_char() == Some('0') {
            match self.peek_at(1) {
                Some('x') | Some('X') => {
                    radix = 16;
                    self.bump();
                    self.bump();
                }
                Some('b') | Some('B') => {
                    radix = 2;
                    self.bump();
                    self.bump();
                }
                Some('o') | Some('O') => {
                    radix = 8;
                    self.bump();
                    self.bump();
                }
                _ => {}
            }
        }
        if radix == 10 {
            while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                self.bump();
            }
            if self.peek_char() == Some('.') {
                self.bump();
                while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                    self.bump();
                }
            }
            if matches!(self.peek_char(), Some('e') | Some('E')) {
                let save_pos = self.pos;
                let save_line = self.line;
                let save_column = self.column;
                self.bump();
                if matches!(self.peek_char(), Some('+') | Some('-')) {
                    self.bump();
                }
                if self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                    while self.peek_char().is_some_and(|c| c.is_ascii_digit()) {
                        self.bump();
                    }
                } else {
                    self.pos = save_pos;
                    self.line = save_line;
                    self.column = save_column;
                }
            }
            let text: String = self.chars[start..self.pos].iter().collect();
            let value = text
                .parse::<f64>()
                .map_err(|_| self.error(format!("invalid numeric literal `{text}`")))?;
            return Ok(TokenKind::Number(value));
        }
        let digits_start = self.pos;
        while let Some(ch) = self.peek_char() {
            if ch.is_digit(radix) || ch == '_' {
                self.bump();
            } else {
                break;
            }
        }
        let text: String = self.chars[digits_start..self.pos]
            .iter()
            .filter(|c| **c != '_')
            .collect();
        if text.is_empty() {
            return Err(self.error("missing digits in numeric literal"));
        }
        let value = u64::from_str_radix(&text, radix)
            .map_err(|_| self.error(format!("invalid numeric literal `0x{text}`")))?;
        Ok(TokenKind::Number(value as f64))
    }

    fn scan_escape(&mut self) -> Result<Option<char>, ParseError> {
        // Called with the backslash already consumed.
        let ch = match self.bump() {
            Some(ch) => ch,
            None => return Err(self.error("unterminated escape sequence")),
        };
        let value = match ch {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'b' => '\u{0008}',
            'f' => '\u{000C}',
            'v' => '\u{000B}',
            '0' => '\0',
            'x' => {
                let mut code = 0u32;
                for _ in 0..2 {
                    let digit = self
                        .bump()
                        .and_then(|c| c.to_digit(16))
                        .ok_or_else(|| self.error("invalid hexadecimal escape sequence"))?;
                    code = code * 16 + digit;
                }
                char::from_u32(code).unwrap_or('\u{FFFD}')
            }
            'u' => {
                if self.peek_char() == Some('{') {
                    self.bump();
                    let mut code = 0u32;
                    let mut any = false;
                    while let Some(c) = self.peek_char() {
                        if c == '}' {
                            break;
                        }
                        let digit = c
                            .to_digit(16)
                            .ok_or_else(|| self.error("invalid unicode escape sequence"))?;
                        code = code * 16 + digit;
                        any = true;
                        self.bump();
                    }
                    if !any || self.peek_char() != Some('}') {
                        return Err(self.error("invalid unicode escape sequence"));
                    }
                    self.bump();
                    char::from_u32(code).unwrap_or('\u{FFFD}')
                } else {
                    let mut code = 0u32;
                    for _ in 0..4 {
                        let digit = self
                            .bump()
                            .and_then(|c| c.to_digit(16))
                            .ok_or_else(|| self.error("invalid unicode escape sequence"))?;
                        code = code * 16 + digit;
                    }
                    char::from_u32(code).unwrap_or('\u{FFFD}')
                }
            }
            '\n' => return Ok(None),
            '\r' => {
                if self.peek_char() == Some('\n') {
                    self.bump();
                }
                return Ok(None);
            }
            other => other,
        };
        Ok(Some(value))
    }

    fn scan_string(&mut self, quote: char) -> Result<TokenKind, ParseError> {
        let start_line = self.line;
        let start_column = self.column;
        self.bump();
        let mut out = String::new();
        loop {
            let ch = match self.bump() {
                Some(ch) => ch,
                None => {
                    return Err(ParseError::new(
                        "unterminated string literal",
                        start_line,
                        start_column,
                    ));
                }
            };
            if ch == quote {
                break;
            }
            if ch == '\\' {
                if let Some(escaped) = self.scan_escape()? {
                    out.push(escaped);
                }
                continue;
            }
            if ch == '\n' || ch == '\r' {
                return Err(ParseError::new(
                    "unterminated string literal",
                    start_line,
                    start_column,
                ));
            }
            out.push(ch);
        }
        Ok(TokenKind::String(out))
    }

    fn scan_template_part(&mut self) -> Result<TokenKind, ParseError> {
        let start_line = self.line;
        let start_column = self.column;
        let mut out = String::new();
        loop {
            let ch = match self.bump() {
                Some(ch) => ch,
                None => {
                    return Err(ParseError::new(
                        "unterminated template literal",
                        start_line,
                        start_column,
                    ));
                }
            };
            if ch == '`' {
                return Ok(TokenKind::TemplatePart {
                    cooked: out,
                    tail: true,
                });
            }
            if ch == '$' && self.peek_char() == Some('{') {
                self.bump();
                self.template_stack.push(0);
                return Ok(TokenKind::TemplatePart {
                    cooked: out,
                    tail: false,
                });
            }
            if ch == '\\' {
                if let Some(escaped) = self.scan_escape()? {
                    out.push(escaped);
                }
                continue;
            }
            if ch == '\r' {
                if self.peek_char() == Some('\n') {
                    self.bump();
                }
                out.push('\n');
                continue;
            }
            out.push(ch);
        }
    }

    fn scan_punct(&mut self) -> Result<TokenKind, ParseError> {
        let position = self.position();
        let ch = self.bump().expect("caller checked for end of input");
        let next = self.peek_char();
        let next2 = self.peek_at(1);
        let (punct, len) = match ch {
            '(' => {
                let control = matches!(
                    self.last_kind,
                    Some(TokenKind::Keyword(
                        Keyword::If | Keyword::While | Keyword::For
                    ))
                );
                self.paren_stack.push(control);
                (Punct::LParen, 0)
            }
            ')' => (Punct::RParen, 0),
            '{' => {
                if let Some(depth) = self.template_stack.last_mut() {
                    *depth += 1;
                }
                (Punct::LBrace, 0)
            }
            '}' => {
                if let Some(depth) = self.template_stack.last_mut() {
                    if *depth == 0 {
                        self.template_stack.pop();
                        let kind = self.scan_template_part()?;
                        return Ok(kind);
                    }
                    *depth -= 1;
                }
                (Punct::RBrace, 0)
            }
            '[' => (Punct::LBracket, 0),
            ']' => (Punct::RBracket, 0),
            ';' => (Punct::Semi, 0),
            ',' => (Punct::Comma, 0),
            ':' => (Punct::Colon, 0),
            '?' => match next {
                Some('?') => (Punct::QuestionQuestion, 1),
                _ => (Punct::Question, 0),
            },
            '.' => {
                if next == Some('.') && next2 == Some('.') {
                    (Punct::Ellipsis, 2)
                } else {
                    (Punct::Dot, 0)
                }
            }
            '+' => match next {
                Some('+') => (Punct::PlusPlus, 1),
                Some('=') => (Punct::PlusEq, 1),
                _ => (Punct::Plus, 0),
            },
            '-' => match next {
                Some('-') => (Punct::MinusMinus, 1),
                Some('=') => (Punct::MinusEq, 1),
                _ => (Punct::Minus, 0),
            },
            '*' => match next {
                Some('*') => {
                    if next2 == Some('=') {
                        (Punct::StarStarEq, 2)
                    } else {
                        (Punct::StarStar, 1)
                    }
                }
                Some('=') => (Punct::StarEq, 1),
                _ => (Punct::Star, 0),
            },
            '/' => match next {
                Some('=') => (Punct::SlashEq, 1),
                _ => (Punct::Slash, 0),
            },
            '%' => match next {
                Some('=') => (Punct::PercentEq, 1),
                _ => (Punct::Percent, 0),
            },
            '!' => match next {
                Some('=') => {
                    if next2 == Some('=') {
                        (Punct::BangEqEq, 2)
                    } else {
                        (Punct::BangEq, 1)
                    }
                }
                _ => (Punct::Bang, 0),
            },
            '~' => (Punct::Tilde, 0),
            '&' => match next {
                Some('&') => (Punct::AmpAmp, 1),
                Some('=') => (Punct::AmpEq, 1),
                _ => (Punct::Amp, 0),
            },
            '|' => match next {
                Some('|') => (Punct::PipePipe, 1),
                Some('=') => (Punct::PipeEq, 1),
                _ => (Punct::Pipe, 0),
            },
            '^' => match next {
                Some('=') => (Punct::CaretEq, 1),
                _ => (Punct::Caret, 0),
            },
            '<' => match next {
                Some('<') => {
                    if next2 == Some('=') {
                        (Punct::LtLtEq, 2)
                    } else {
                        (Punct::LtLt, 1)
                    }
                }
                Some('=') => (Punct::LtEq, 1),
                _ => (Punct::Lt, 0),
            },
            '>' => match next {
                Some('>') => match next2 {
                    Some('>') => {
                        if self.peek_at(2) == Some('=') {
                            (Punct::GtGtGtEq, 3)
                        } else {
                            (Punct::GtGtGt, 2)
                        }
                    }
                    Some('=') => (Punct::GtGtEq, 2),
                    _ => (Punct::GtGt, 1),
                },
                Some('=') => (Punct::GtEq, 1),
                _ => (Punct::Gt, 0),
            },
            '=' => match next {
                Some('=') => {
                    if next2 == Some('=') {
                        (Punct::EqEqEq, 2)
                    } else {
                        (Punct::EqEq, 1)
                    }
                }
                Some('>') => (Punct::Arrow, 1),
                _ => (Punct::Eq, 0),
            },
            other => {
                return Err(ParseError::new(
                    format!("unexpected character `{other}`"),
                    position.line,
                    position.column,
                ));
            }
        };
        for _ in 0..len {
            self.bump();
        }
        Ok(TokenKind::Punct(punct))
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '$' || ch == '_' || ch.is_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '$' || ch == '_' || ch.is_alphanumeric() || ch == '\u{200C}' || ch == '\u{200D}'
}

/// Tokenize a complete source string.
pub fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token()?;
        let done = token.kind == TokenKind::Eof;
        tokens.push(token);
        if done {
            return Ok(tokens);
        }
    }
}
