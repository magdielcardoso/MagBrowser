//! A WHATWG-style HTML tokenizer.
//!
//! The tokenizer walks the input once and emits [`Token`]s. It covers the
//! data, tag, attribute, comment, doctype, RCDATA/rawtext and script-data
//! states. Because rawtext end tags can only be recognised after the start tag
//! is known, the tokenizer switches to rawtext mode automatically for the
//! standard rawtext/RCDATA elements (`script`, `style`, `title`, `textarea`,
//! `xmp`, `iframe`, `noembed`, `noframes`, and `noscript` when scripting is
//! enabled).
//!
//! Character references are decoded with [`crate::entities`]. The tokenizer
//! never panics: malformed input is recovered from by emitting the literal
//! characters seen so far or by ending input.

use crate::entities;
use crate::tokens::Token;

/// Input preprocessing replaces CRLF/CR with LF and NUL with U+FFFD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    Rcdata,
    Rawtext,
    ScriptData,
    TagOpen,
    EndTagOpen,
    TagName,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDouble,
    AttributeValueSingle,
    AttributeValueUnquoted,
    SelfClosingStartTag,
    MarkupDeclarationOpen,
    BogusComment,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    DoctypeName,
    AfterDoctypeName,
    BogusDoctype,
    /// Character-reference consumption. References are decoded by the
    /// [`entities`] subroutine, which is entered from this state.
    CharacterReference,
}

/// Raw text family selected after a rawtext start tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Rcdata,
    Rawtext,
    ScriptData,
}

#[derive(Debug, Default)]
struct TagBuilder {
    name: String,
    attributes: Vec<(String, String)>,
    attr_name: Option<String>,
    attr_value: String,
    self_closing: bool,
    is_end: bool,
}

impl TagBuilder {
    fn start(is_end: bool) -> Self {
        Self {
            is_end,
            ..Self::default()
        }
    }

    fn finish_attr(&mut self) {
        if let Some(name) = self.attr_name.take() {
            // Duplicate attributes in the source are ignored; the first wins.
            if !self
                .attributes
                .iter()
                .any(|(existing, _)| *existing == name)
            {
                self.attributes
                    .push((name, std::mem::take(&mut self.attr_value)));
            } else {
                self.attr_value.clear();
            }
        }
    }
}

#[derive(Debug, Default)]
struct DoctypeBuilder {
    name: String,
    public_id: String,
    system_id: String,
}

/// Streaming HTML tokenizer.
///
/// ```
/// use browser_html_parser::{Tokenizer, Token};
/// let tokens: Vec<Token> = Tokenizer::new("<p>hi</p>").collect();
/// assert!(matches!(tokens[0], Token::StartTag { .. }));
/// ```
pub struct Tokenizer<'a> {
    _source: &'a str,
    chars: Vec<char>,
    pos: usize,
    state: State,
    tag: Option<TagBuilder>,
    doctype: Option<DoctypeBuilder>,
    comment: String,
    raw_tag: Option<String>,
    scripting: bool,
    eof_emitted: bool,
    /// Set during preprocessing when input was cut off at `max_bytes`.
    input_truncated: bool,
    /// Number of character references decoded in the current tag's attributes.
    attr_refs: usize,
}

/// Backstop for text runs consisting almost entirely of character references.
/// Past this budget references are emitted in their literal form, keeping work
/// bounded even when a `Tokenizer` is used directly without the input-size cap.
/// The public parse entry points cap input well below this, so normal pages
/// (including large, entity-heavy ones) are unaffected.
const MAX_REFERENCES_PER_RUN: usize = 8 * 1024 * 1024;

impl<'a> Tokenizer<'a> {
    /// Creates a tokenizer over `input` with scripting enabled and no input cap.
    pub fn new(input: &'a str) -> Self {
        Self::with_max_input_bytes(input, usize::MAX)
    }

    /// Creates a tokenizer that preprocesses at most `max_bytes` bytes of
    /// `input`. Longer input is truncated at a character boundary and can be
    /// detected with [`Tokenizer::input_truncated`].
    pub fn with_max_input_bytes(input: &'a str, max_bytes: usize) -> Self {
        let mut chars = Vec::with_capacity(input.len().min(max_bytes));
        let mut used = 0usize;
        let mut iter = input.chars().peekable();
        let mut truncated = false;
        while let Some(c) = iter.next() {
            let width = c.len_utf8();
            if used.saturating_add(width) > max_bytes {
                truncated = true;
                break;
            }
            used += width;
            match c {
                '\r' => {
                    if iter.peek() == Some(&'\n') {
                        iter.next();
                    }
                    chars.push('\n');
                }
                '\0' => chars.push('\u{fffd}'),
                _ => chars.push(c),
            }
        }
        Self {
            _source: input,
            chars,
            pos: 0,
            state: State::Data,
            tag: None,
            doctype: None,
            comment: String::new(),
            raw_tag: None,
            scripting: true,
            eof_emitted: false,
            input_truncated: truncated,
            attr_refs: 0,
        }
    }

    /// Whether preprocessing truncated the source at the configured byte cap.
    #[must_use]
    pub fn input_truncated(&self) -> bool {
        self.input_truncated
    }

    /// Sets whether `noscript` content is tokenized as rawtext.
    #[must_use]
    pub fn with_scripting(mut self, scripting: bool) -> Self {
        self.scripting = scripting;
        self
    }

    /// Character offset of the tokenizer within the (preprocessed) input.
    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn eof(&mut self) -> Token {
        self.state = State::Data;
        self.eof_emitted = true;
        Token::Eof
    }

    fn starts_with_ascii_ci(&self, text: &str) -> bool {
        let mut actual_chars = self.chars.iter().skip(self.pos);
        for expected in text.chars() {
            let Some(actual) = actual_chars.next() else {
                return false;
            };
            if !actual.eq_ignore_ascii_case(&expected) {
                return false;
            }
        }
        true
    }

    fn markup_start_ahead(&self) -> bool {
        match self.chars.get(self.pos + 1) {
            Some(c) => c.is_ascii_alphabetic() || matches!(c, '!' | '/' | '?'),
            None => false,
        }
    }

    fn raw_kind_for(&self, name: &str) -> Option<RawKind> {
        match name {
            "title" | "textarea" => Some(RawKind::Rcdata),
            "script" => Some(RawKind::ScriptData),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(RawKind::Rawtext),
            "noscript" if self.scripting => Some(RawKind::Rawtext),
            _ => None,
        }
    }

    fn raw_end_ahead(&self) -> bool {
        if self.chars.get(self.pos) != Some(&'<') || self.chars.get(self.pos + 1) != Some(&'/') {
            return false;
        }
        let Some(raw_tag) = self.raw_tag.as_deref() else {
            return false;
        };
        let mut i = self.pos + 2;
        for expected in raw_tag.chars() {
            let Some(actual) = self.chars.get(i) else {
                return true;
            };
            if !actual.eq_ignore_ascii_case(&expected) {
                return false;
            }
            i += 1;
        }
        match self.chars.get(i) {
            None => true,
            Some(c) => c.is_ascii_whitespace() || matches!(c, '/' | '>'),
        }
    }

    /// Consumes a character reference through the `CharacterReference` state.
    ///
    /// The state is transient because the decoding subroutine consumes the
    /// complete reference; the caller's state is restored before returning.
    fn consume_reference(&mut self) -> String {
        let return_state = self.state;
        self.state = State::CharacterReference;
        let (decoded, next) = entities::consume_char_ref(&self.chars, self.pos);
        self.pos = next;
        self.state = return_state;
        decoded
    }

    /// Decodes a reference into the current attribute value, respecting the
    /// per-tag reference budget.
    fn consume_attr_reference(&mut self) {
        let decoded = self.consume_reference();
        if let Some(tag) = &mut self.tag {
            tag.attr_value.push_str(&decoded);
        }
        self.attr_refs += 1;
    }

    /// Consumes the whole text run starting at the current position.
    fn consume_text(&mut self, decode: bool) -> Token {
        let mut out = String::new();
        let mut refs = 0usize;
        while let Some(c) = self.peek() {
            if c == '<' && self.markup_start_ahead() {
                break;
            }
            if decode && c == '&' && refs < MAX_REFERENCES_PER_RUN {
                out.push_str(&self.consume_reference());
                refs += 1;
            } else {
                out.push(c);
                self.pos += 1;
            }
        }
        Token::Text(out)
    }

    /// Consumes rawtext/RCDATA content up to the matching end tag.
    fn consume_raw_text(&mut self, decode: bool) -> Token {
        if self.at_end() {
            return self.eof();
        }
        if self.raw_end_ahead() {
            return self.consume_raw_end_tag();
        }
        let mut out = String::new();
        let mut refs = 0usize;
        while let Some(c) = self.peek() {
            if c == '<' && self.raw_end_ahead() {
                break;
            }
            if decode && c == '&' && refs < MAX_REFERENCES_PER_RUN {
                out.push_str(&self.consume_reference());
                refs += 1;
            } else {
                out.push(c);
                self.pos += 1;
            }
        }
        if out.is_empty() {
            // Only possible when the end tag starts here; keep forward progress.
            self.consume_raw_end_tag()
        } else {
            Token::Text(out)
        }
    }

    fn consume_raw_end_tag(&mut self) -> Token {
        self.pos += 2; // `</`
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() || matches!(c, '/' | '>') {
                break;
            }
            name.push(c.to_ascii_lowercase());
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '>' {
                break;
            }
        }
        self.raw_tag = None;
        self.state = State::Data;
        Token::EndTag { name }
    }

    fn emit_tag(&mut self) -> Token {
        self.state = State::Data;
        self.attr_refs = 0;
        let mut tag = self.tag.take().unwrap_or_else(|| TagBuilder::start(false));
        tag.finish_attr();
        if tag.is_end {
            return Token::EndTag { name: tag.name };
        }
        let token = Token::StartTag {
            name: tag.name.clone(),
            attributes: tag.attributes,
            self_closing: tag.self_closing,
        };
        if !tag.self_closing
            && let Some(kind) = self.raw_kind_for(&tag.name)
        {
            self.state = match kind {
                RawKind::Rcdata => State::Rcdata,
                RawKind::Rawtext => State::Rawtext,
                RawKind::ScriptData => State::ScriptData,
            };
            self.raw_tag = Some(tag.name);
        }
        token
    }

    fn emit_comment(&mut self) -> Token {
        self.state = State::Data;
        Token::Comment(std::mem::take(&mut self.comment))
    }

    fn emit_doctype(&mut self) -> Token {
        self.state = State::Data;
        let builder = self.doctype.take().unwrap_or_default();
        Token::Doctype {
            name: builder.name,
            public_id: builder.public_id,
            system_id: builder.system_id,
        }
    }

    fn consume_bogus_comment(&mut self, prefix: &str) -> Token {
        let mut data = prefix.to_string();
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '>' {
                break;
            }
            data.push(c);
        }
        self.state = State::Data;
        Token::Comment(data)
    }

    fn consume_quoted(&mut self) -> String {
        let mut out = String::new();
        let Some(quote) = self.peek() else {
            return out;
        };
        if quote == '"' || quote == '\'' {
            self.pos += 1;
            while let Some(c) = self.peek() {
                self.pos += 1;
                if c == quote {
                    return out;
                }
                out.push(c);
            }
        } else {
            while let Some(c) = self.peek() {
                if c.is_ascii_whitespace() || c == '>' {
                    break;
                }
                out.push(c);
                self.pos += 1;
            }
        }
        out
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        if !self.starts_with_ascii_ci(keyword) {
            return false;
        }
        self.pos += keyword.len();
        true
    }

    fn consume_after_doctype_name(&mut self) -> Token {
        self.skip_whitespace();
        let mut builder = self.doctype.take().unwrap_or_default();
        if self.peek() == Some('>') {
            self.pos += 1;
            self.state = State::Data;
            return Token::Doctype {
                name: builder.name,
                public_id: builder.public_id,
                system_id: builder.system_id,
            };
        }
        if self.consume_keyword("PUBLIC") {
            self.skip_whitespace();
            builder.public_id = self.consume_quoted();
            self.skip_whitespace();
            if matches!(self.peek(), Some('"' | '\'')) {
                builder.system_id = self.consume_quoted();
            }
        } else if self.consume_keyword("SYSTEM") {
            self.skip_whitespace();
            builder.system_id = self.consume_quoted();
        } else {
            self.state = State::BogusDoctype;
            self.doctype = Some(builder);
            return self.consume_bogus_doctype();
        }
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '>' {
                break;
            }
        }
        self.state = State::Data;
        Token::Doctype {
            name: builder.name,
            public_id: builder.public_id,
            system_id: builder.system_id,
        }
    }

    fn consume_bogus_doctype(&mut self) -> Token {
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == '>' {
                break;
            }
        }
        self.emit_doctype()
    }

    /// Produces the next token. Returns [`Token::Eof`] once input is exhausted.
    pub fn next_token(&mut self) -> Token {
        loop {
            match self.state {
                State::Data => {
                    if self.at_end() {
                        return self.eof();
                    }
                    if self.chars[self.pos] == '<' && self.markup_start_ahead() {
                        self.pos += 1;
                        self.state = State::TagOpen;
                    } else {
                        return self.consume_text(true);
                    }
                }
                State::Rcdata | State::Rawtext | State::ScriptData => {
                    let decode = self.state == State::Rcdata;
                    return self.consume_raw_text(decode);
                }
                State::TagOpen => {
                    let Some(c) = self.peek() else {
                        self.state = State::Data;
                        return Token::Text("<".to_string());
                    };
                    if c == '!' {
                        self.pos += 1;
                        self.state = State::MarkupDeclarationOpen;
                    } else if c == '/' {
                        self.pos += 1;
                        self.state = State::EndTagOpen;
                    } else if c == '?' {
                        self.state = State::BogusComment;
                    } else if c.is_ascii_alphabetic() {
                        self.tag = Some(TagBuilder::start(false));
                        self.state = State::TagName;
                    } else {
                        self.state = State::Data;
                        return Token::Text("<".to_string());
                    }
                }
                State::EndTagOpen => match self.peek() {
                    None => return self.eof(),
                    Some('>') => {
                        self.pos += 1;
                        self.state = State::Data;
                    }
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.tag = Some(TagBuilder::start(true));
                        self.state = State::TagName;
                    }
                    Some(_) => {
                        self.state = State::BogusComment;
                    }
                },
                State::TagName => {
                    let Some(c) = self.peek() else {
                        self.tag = None;
                        return self.eof();
                    };
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                        self.state = State::BeforeAttributeName;
                    } else if c == '/' {
                        self.pos += 1;
                        self.state = State::SelfClosingStartTag;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_tag();
                    } else {
                        if let Some(tag) = &mut self.tag {
                            tag.name.push(c.to_ascii_lowercase());
                        }
                        self.pos += 1;
                    }
                }
                State::BeforeAttributeName => {
                    let Some(c) = self.peek() else {
                        self.tag = None;
                        return self.eof();
                    };
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                    } else if c == '/' {
                        self.pos += 1;
                        self.state = State::SelfClosingStartTag;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_tag();
                    } else {
                        // Normally the pending attribute was already committed on
                        // entering this state, but it can still be live when we
                        // arrive from SelfClosingStartTag. Commit before reuse.
                        if let Some(tag) = &mut self.tag {
                            tag.finish_attr();
                            tag.attr_name = Some(c.to_ascii_lowercase().to_string());
                            tag.attr_value.clear();
                        }
                        self.pos += 1;
                        self.state = State::AttributeName;
                    }
                }
                State::AttributeName => {
                    let Some(c) = self.peek() else {
                        self.tag = None;
                        return self.eof();
                    };
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                        self.state = State::AfterAttributeName;
                    } else if c == '/' {
                        self.pos += 1;
                        if let Some(tag) = &mut self.tag {
                            tag.finish_attr();
                        }
                        self.state = State::SelfClosingStartTag;
                    } else if c == '=' {
                        self.pos += 1;
                        self.state = State::BeforeAttributeValue;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_tag();
                    } else {
                        if let Some(tag) = &mut self.tag
                            && let Some(name) = &mut tag.attr_name
                        {
                            name.push(c.to_ascii_lowercase());
                        }
                        self.pos += 1;
                    }
                }
                State::AfterAttributeName => {
                    let Some(c) = self.peek() else {
                        self.tag = None;
                        return self.eof();
                    };
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                    } else if c == '/' {
                        self.pos += 1;
                        if let Some(tag) = &mut self.tag {
                            tag.finish_attr();
                        }
                        self.state = State::SelfClosingStartTag;
                    } else if c == '=' {
                        self.pos += 1;
                        self.state = State::BeforeAttributeValue;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_tag();
                    } else {
                        // A new attribute name starts here. The previous attribute
                        // (possibly a valueless boolean such as `hidden`) is still
                        // pending, so commit it before overwriting `attr_name`.
                        if let Some(tag) = &mut self.tag {
                            tag.finish_attr();
                            tag.attr_name = Some(c.to_ascii_lowercase().to_string());
                            tag.attr_value.clear();
                        }
                        self.pos += 1;
                        self.state = State::AttributeName;
                    }
                }
                State::BeforeAttributeValue => {
                    let Some(c) = self.peek() else {
                        self.tag = None;
                        return self.eof();
                    };
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                    } else if c == '"' {
                        self.pos += 1;
                        self.state = State::AttributeValueDouble;
                    } else if c == '\'' {
                        self.pos += 1;
                        self.state = State::AttributeValueSingle;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_tag();
                    } else {
                        self.state = State::AttributeValueUnquoted;
                    }
                }
                State::AttributeValueDouble | State::AttributeValueSingle => {
                    let quote = if self.state == State::AttributeValueDouble {
                        '"'
                    } else {
                        '\''
                    };
                    match self.peek() {
                        None => {
                            self.tag = None;
                            return self.eof();
                        }
                        Some(c) if c == quote => {
                            self.pos += 1;
                            if let Some(tag) = &mut self.tag {
                                tag.finish_attr();
                            }
                            self.state = State::BeforeAttributeName;
                        }
                        Some('&') if self.attr_refs < MAX_REFERENCES_PER_RUN => {
                            self.consume_attr_reference();
                        }
                        Some(c) => {
                            if let Some(tag) = &mut self.tag {
                                tag.attr_value.push(c);
                            }
                            self.pos += 1;
                        }
                    }
                }
                State::AttributeValueUnquoted => {
                    let Some(c) = self.peek() else {
                        self.tag = None;
                        return self.eof();
                    };
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                        if let Some(tag) = &mut self.tag {
                            tag.finish_attr();
                        }
                        self.state = State::BeforeAttributeName;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_tag();
                    } else if c == '&' && self.attr_refs < MAX_REFERENCES_PER_RUN {
                        self.consume_attr_reference();
                    } else {
                        if let Some(tag) = &mut self.tag {
                            tag.attr_value.push(c);
                        }
                        self.pos += 1;
                    }
                }
                State::SelfClosingStartTag => match self.peek() {
                    None => {
                        self.tag = None;
                        return self.eof();
                    }
                    Some('>') => {
                        self.pos += 1;
                        if let Some(tag) = &mut self.tag {
                            tag.self_closing = true;
                        }
                        return self.emit_tag();
                    }
                    Some(_) => {
                        self.state = State::BeforeAttributeName;
                    }
                },
                State::MarkupDeclarationOpen => {
                    if self.at_end() {
                        return self.eof();
                    }
                    if self.starts_with_ascii_ci("--") {
                        self.pos += 2;
                        self.comment.clear();
                        self.state = State::CommentStart;
                    } else if self.starts_with_ascii_ci("doctype") {
                        self.pos += 7;
                        self.doctype = Some(DoctypeBuilder::default());
                        self.state = State::Doctype;
                    } else if self.starts_with_ascii_ci("[CDATA[") {
                        self.pos += 7;
                        return self.consume_bogus_comment("[CDATA[");
                    } else {
                        self.state = State::BogusComment;
                    }
                }
                State::BogusComment => {
                    return self.consume_bogus_comment("");
                }
                State::CommentStart => match self.peek() {
                    None => return self.emit_comment(),
                    Some('>') => {
                        self.pos += 1;
                        return self.emit_comment();
                    }
                    Some('-') => {
                        self.pos += 1;
                        self.state = State::CommentStartDash;
                    }
                    Some(_) => {
                        self.state = State::Comment;
                    }
                },
                State::CommentStartDash => match self.peek() {
                    None => return self.emit_comment(),
                    Some('>') => {
                        self.pos += 1;
                        return self.emit_comment();
                    }
                    Some('-') => {
                        self.pos += 1;
                        self.state = State::CommentEnd;
                    }
                    Some(_) => {
                        self.comment.push('-');
                        self.state = State::Comment;
                    }
                },
                State::Comment => match self.peek() {
                    None => return self.emit_comment(),
                    Some('-') => {
                        self.pos += 1;
                        self.state = State::CommentEndDash;
                    }
                    Some(c) => {
                        self.comment.push(c);
                        self.pos += 1;
                    }
                },
                State::CommentEndDash => match self.peek() {
                    None => return self.emit_comment(),
                    Some('-') => {
                        self.pos += 1;
                        self.state = State::CommentEnd;
                    }
                    Some(c) => {
                        self.comment.push('-');
                        self.comment.push(c);
                        self.pos += 1;
                        self.state = State::Comment;
                    }
                },
                State::CommentEnd => match self.peek() {
                    None => return self.emit_comment(),
                    Some('>') => {
                        self.pos += 1;
                        return self.emit_comment();
                    }
                    Some('!') => {
                        self.pos += 1;
                        self.state = State::CommentEndBang;
                    }
                    Some('-') => {
                        self.comment.push('-');
                        self.pos += 1;
                    }
                    Some(_) => {
                        self.comment.push_str("--");
                        self.state = State::Comment;
                    }
                },
                State::CommentEndBang => match self.peek() {
                    None => {
                        self.comment.push_str("--!");
                        return self.emit_comment();
                    }
                    Some('>') => {
                        self.pos += 1;
                        self.comment.push_str("--!");
                        return self.emit_comment();
                    }
                    Some('-') => {
                        self.comment.push_str("--!");
                        self.pos += 1;
                        self.state = State::CommentEndDash;
                    }
                    Some(_) => {
                        self.comment.push_str("--!");
                        self.state = State::Comment;
                    }
                },
                State::Doctype => {
                    if self.at_end() {
                        return self.emit_doctype();
                    }
                    let c = self.chars[self.pos];
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_doctype();
                    } else {
                        if let Some(doctype) = &mut self.doctype {
                            doctype.name.push(c.to_ascii_lowercase());
                        }
                        self.pos += 1;
                        self.state = State::DoctypeName;
                    }
                }
                State::DoctypeName => {
                    let Some(c) = self.peek() else {
                        return self.emit_doctype();
                    };
                    if c.is_ascii_whitespace() {
                        self.pos += 1;
                        self.state = State::AfterDoctypeName;
                    } else if c == '>' {
                        self.pos += 1;
                        return self.emit_doctype();
                    } else {
                        if let Some(doctype) = &mut self.doctype {
                            doctype.name.push(c.to_ascii_lowercase());
                        }
                        self.pos += 1;
                    }
                }
                State::AfterDoctypeName => {
                    return self.consume_after_doctype_name();
                }
                State::BogusDoctype => {
                    return self.consume_bogus_doctype();
                }
                State::CharacterReference => {
                    let (decoded, next) = entities::consume_char_ref(&self.chars, self.pos);
                    self.pos = next;
                    self.state = State::Data;
                    return Token::Text(decoded);
                }
            }
        }
    }
}

impl Iterator for Tokenizer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof_emitted {
            return None;
        }
        Some(self.next_token())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token> {
        Tokenizer::new(input).collect()
    }

    fn tag(name: &str, attributes: &[(&str, &str)], self_closing: bool) -> Token {
        Token::StartTag {
            name: name.to_string(),
            attributes: attributes
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            self_closing,
        }
    }

    #[test]
    fn tokenizes_attributes_and_self_closing() {
        let out = tokens("<img src=a.png alt='x y' data-a=\"1\" />");
        assert_eq!(
            out[0],
            tag(
                "img",
                &[("src", "a.png"), ("alt", "x y"), ("data-a", "1")],
                true
            )
        );
        assert_eq!(out[1], Token::Eof);
    }

    #[test]
    fn valueless_attribute_before_another_attribute_is_kept() {
        let out = tokens("<div id=x hidden data-foo=1>");
        assert_eq!(
            out[0],
            tag(
                "div",
                &[("id", "x"), ("hidden", ""), ("data-foo", "1")],
                false
            )
        );
    }

    #[test]
    fn trailing_valueless_attribute_is_kept() {
        let out = tokens("<div hidden>");
        assert_eq!(out[0], tag("div", &[("hidden", "")], false));
    }

    #[test]
    fn consecutive_valueless_attributes_are_kept() {
        let out = tokens("<input disabled readonly required>");
        assert_eq!(
            out[0],
            tag(
                "input",
                &[("disabled", ""), ("readonly", ""), ("required", "")],
                false
            )
        );
    }

    #[test]
    fn valueless_attribute_reconsumed_after_self_close_slash_is_kept() {
        let out = tokens("<div hidden / class=x>");
        assert_eq!(out[0], tag("div", &[("hidden", ""), ("class", "x")], false));
    }

    #[test]
    fn tokenizes_comments_and_doctypes() {
        let out = tokens("<!DOCTYPE html><!-- hi --><!doctype html PUBLIC \"a\" \"b\">");
        assert_eq!(
            out[0],
            Token::Doctype {
                name: "html".to_string(),
                public_id: String::new(),
                system_id: String::new(),
            }
        );
        assert_eq!(out[1], Token::Comment(" hi ".to_string()));
        assert_eq!(
            out[2],
            Token::Doctype {
                name: "html".to_string(),
                public_id: "a".to_string(),
                system_id: "b".to_string(),
            }
        );
    }

    #[test]
    fn tokenizes_rcdata_and_rawtext() {
        let out = tokens("<title>a &amp; b</title><style>a < b</style>");
        assert_eq!(out[0], tag("title", &[], false));
        assert_eq!(out[1], Token::Text("a & b".to_string()));
        assert_eq!(
            out[2],
            Token::EndTag {
                name: "title".to_string()
            }
        );
        assert_eq!(out[3], tag("style", &[], false));
        assert_eq!(out[4], Token::Text("a < b".to_string()));
    }

    #[test]
    fn script_can_contain_less_than_and_end_tag_text() {
        let out = tokens("<script>if (a < b) { document.write('</scr' + 'ipt>'); }</script>");
        let Token::Text(script) = &out[1] else {
            panic!("expected script text");
        };
        assert!(script.contains("a < b"));
        assert!(script.contains("</scr"));
        assert_eq!(
            out[2],
            Token::EndTag {
                name: "script".to_string()
            }
        );
    }

    #[test]
    fn bogus_comments_and_cdata() {
        let out = tokens("<?xml version=\"1.0\"?><![CDATA[x<y]]>");
        assert_eq!(out[0], Token::Comment("?xml version=\"1.0\"?".to_string()));
        assert_eq!(out[1], Token::Comment("[CDATA[x<y]]".to_string()));
    }

    #[test]
    fn unterminated_constructs_end_cleanly() {
        assert_eq!(tokens("<div"), vec![Token::Eof]);
        assert_eq!(
            tokens("<!-- x"),
            vec![Token::Comment(" x".to_string()), Token::Eof]
        );
        assert_eq!(tokens("<"), vec![Token::Text("<".to_string()), Token::Eof]);
        assert_eq!(
            tokens("a < b"),
            vec![Token::Text("a < b".to_string()), Token::Eof]
        );
    }
}
