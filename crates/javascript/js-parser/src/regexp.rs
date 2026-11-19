//! A small regular expression parser and backtracking matcher.
//!
//! The dialect is a deliberately small JavaScript-flavoured subset: literal
//! characters, `.`, character classes, the `\d \w \s` families (and their
//! negations), `\b`/`\B`, greedy and lazy quantifiers, anchors, alternation,
//! capturing and non-capturing groups, lookahead and lookbehind assertions,
//! and numeric (`\1` .. `\9`) and named (`\k<name>`) backreferences. The flags
//! `g`, `i`, `m`, `s`, `u`, `v`, `y` and `d` are understood. Character classes
//! follow Annex B web semantics: a hyphen next to a class escape is a literal
//! `-`.
//!
//! The `u` (unicode) and `v` (unicode sets) flags are accepted so real-world
//! scripts compile. Full Unicode mode is approximated: text is matched as a
//! sequence of Unicode scalar values (`char`), which is already how the engine
//! works, so astral code points are treated as single characters rather than
//! UTF-16 surrogate pairs.

use std::fmt;

/// Flags of a compiled regular expression.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegexFlags {
    /// `g`: search for every match.
    pub global: bool,
    /// `i`: case insensitive matching.
    pub ignore_case: bool,
    /// `m`: `^` and `$` also match beside line terminators.
    pub multiline: bool,
    /// `s`: `.` also matches line terminators.
    pub dot_all: bool,
    /// `u`: unicode mode. Code points are treated as single characters.
    pub unicode: bool,
    /// `v`: unicode sets mode (accepted; matching is the same as `u` here).
    pub unicode_sets: bool,
    /// `y`: match only at `lastIndex` (sticky matching).
    pub sticky: bool,
    /// `d`: produce the start/end indices of each captured group.
    pub has_indices: bool,
}

impl RegexFlags {
    /// Parse a flag string, rejecting unknown or duplicated flags.
    pub fn parse(flags: &str) -> Result<Self, RegexError> {
        let mut parsed = Self::default();
        for flag in flags.chars() {
            let slot = match flag {
                'g' => &mut parsed.global,
                'i' => &mut parsed.ignore_case,
                'm' => &mut parsed.multiline,
                's' => &mut parsed.dot_all,
                'u' => &mut parsed.unicode,
                'v' => &mut parsed.unicode_sets,
                'y' => &mut parsed.sticky,
                'd' => &mut parsed.has_indices,
                other => {
                    return Err(RegexError::new(format!(
                        "invalid regular expression flag `{other}`"
                    )));
                }
            };
            if *slot {
                return Err(RegexError::new(format!(
                    "duplicate regular expression flag `{flag}`"
                )));
            }
            *slot = true;
        }
        Ok(parsed)
    }

    /// The canonical flag string for these flags.
    pub fn as_string(&self) -> String {
        let mut text = String::new();
        if self.global {
            text.push('g');
        }
        if self.ignore_case {
            text.push('i');
        }
        if self.multiline {
            text.push('m');
        }
        if self.dot_all {
            text.push('s');
        }
        if self.unicode {
            text.push('u');
        }
        if self.unicode_sets {
            text.push('v');
        }
        if self.sticky {
            text.push('y');
        }
        if self.has_indices {
            text.push('d');
        }
        text
    }
}

/// An error produced while parsing a regular expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexError {
    /// Human readable message.
    pub message: String,
}

impl RegexError {
    /// Create an error from a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RegexError {}

/// A single successful match of a regular expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexMatch {
    /// Character offset of the first matched character.
    pub start: usize,
    /// Character offset just past the last matched character.
    pub end: usize,
    /// Captured groups. Index zero is the whole match; missing groups are `None`.
    pub groups: Vec<Option<String>>,
    /// Character offset ranges `(start, end)` for each captured group. Filled
    /// when the `d` flag is set; otherwise each range is `None`.
    pub ranges: Vec<Option<(usize, usize)>>,
}

/// A compiled regular expression.
#[derive(Clone, Debug)]
pub struct Regex {
    source: String,
    flags: RegexFlags,
    program: Vec<Inst>,
    capture_count: usize,
    /// Name of each capturing group, indexed by group number (group 0 is the
    /// whole match and is always `None`).
    group_names: Vec<Option<String>>,
}

#[derive(Clone, Debug)]
enum Inst {
    Char(char),
    Any,
    Class(Class),
    /// Match the text most recently captured by the given group. A group that
    /// did not participate in the match acts as an empty backreference.
    Backref(usize),
    /// A zero-width lookaround assertion. The nested program is run at (or,
    /// for lookbehind, just before) the current position. Every successful
    /// outcome becomes a continuation so the assertion can be backtracked
    /// into, just like in JavaScript.
    Lookaround {
        behind: bool,
        negative: bool,
        program: Vec<Inst>,
    },
    Save(usize),
    Split(usize, usize),
    Jump(usize),
    AssertStart,
    AssertEnd,
    WordBoundary(bool),
    Match,
}

#[derive(Clone, Debug)]
struct Class {
    negated: bool,
    items: Vec<ClassItem>,
}

#[derive(Clone, Debug)]
enum ClassItem {
    Range(char, char),
    Digit,
    NotDigit,
    Word,
    NotWord,
    Space,
    NotSpace,
}

impl Class {
    fn matches(&self, ch: char) -> bool {
        let found = self.items.iter().any(|item| match item {
            ClassItem::Range(start, end) => *start <= ch && ch <= *end,
            ClassItem::Digit => ch.is_ascii_digit(),
            ClassItem::NotDigit => !ch.is_ascii_digit(),
            ClassItem::Word => is_word_char(ch),
            ClassItem::NotWord => !is_word_char(ch),
            ClassItem::Space => is_space_char(ch),
            ClassItem::NotSpace => !is_space_char(ch),
        });
        found != self.negated
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn is_space_char(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\t' | '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

fn is_line_terminator(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(char),
    Any,
    Class(Class),
    Backref(usize),
    Lookaround {
        behind: bool,
        negative: bool,
        node: Box<Node>,
    },
    Seq(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    },
    Group {
        index: Option<usize>,
        node: Box<Node>,
    },
    AssertStart,
    AssertEnd,
    WordBoundary(bool),
}

impl Regex {
    /// Compile `source` with the given flag characters.
    pub fn new(source: &str, flags: &str) -> Result<Self, RegexError> {
        let flags = RegexFlags::parse(flags)?;
        let mut parser = RegexParser::new(source);
        let (node, capture_count, group_names) = parser.parse()?;
        let mut program = Vec::new();
        program.push(Inst::Save(0));
        compile_node(&node, &mut program);
        program.push(Inst::Save(1));
        program.push(Inst::Match);
        Ok(Self {
            source: source.to_owned(),
            flags,
            program,
            capture_count,
            group_names,
        })
    }

    /// The pattern text the expression was compiled from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The flags of the expression.
    pub fn flags(&self) -> RegexFlags {
        self.flags
    }

    /// The number of capturing groups, excluding the implicit whole match.
    pub fn capture_count(&self) -> usize {
        self.capture_count
    }

    /// Names of the capturing groups, indexed by group number. Index zero is
    /// the whole match and is always `None`.
    pub fn group_names(&self) -> &[Option<String>] {
        &self.group_names
    }

    /// Whether the expression matches anywhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        self.find_from(&chars, 0).is_some()
    }

    /// Find the leftmost match starting at or after `from`. With the `y`
    /// (sticky) flag the match is attempted only at `from` itself.
    pub fn find_from(&self, text: &[char], from: usize) -> Option<RegexMatch> {
        let start = from.min(text.len());
        let last = if self.flags.sticky { start } else { text.len() };
        for position in start..=last {
            if let Some(found) = self.match_at(text, position) {
                return Some(found);
            }
        }
        None
    }

    /// Match only at `position`.
    pub fn match_at(&self, text: &[char], position: usize) -> Option<RegexMatch> {
        if position > text.len() {
            return None;
        }
        let group_count = self.capture_count;
        let captures = vec![None; 2 * (group_count + 1)];
        let mut budget: usize = 400_000;
        let (matched_end, captures) = self
            .run_program(&self.program, text, position, captures, &mut budget)
            .into_iter()
            .next()?;
        let start = captures.first().copied().flatten().unwrap_or(position);
        let end = captures.get(1).copied().flatten().unwrap_or(matched_end);
        let mut groups = Vec::with_capacity(group_count + 1);
        for group in 0..=group_count {
            let from = captures.get(group * 2).copied().flatten();
            let to = captures.get(group * 2 + 1).copied().flatten();
            groups.push(match (from, to) {
                (Some(from), Some(to)) if from <= to && to <= text.len() => {
                    Some(text[from..to].iter().collect())
                }
                _ => None,
            });
        }
        let ranges = (0..=group_count)
            .map(|group| {
                let from = captures.get(group * 2).copied().flatten();
                let to = captures.get(group * 2 + 1).copied().flatten();
                match (from, to) {
                    (Some(from), Some(to)) if from <= to && to <= text.len() => Some((from, to)),
                    _ => None,
                }
            })
            .collect();
        Some(RegexMatch {
            start,
            end,
            groups,
            ranges,
        })
    }

    /// Run `program` from `start`, collecting every successful path in the
    /// engine's backtracking order. Nested lookaround programs share the
    /// global `budget` so a pathological pattern cannot spin forever.
    fn run_program(
        &self,
        program: &[Inst],
        text: &[char],
        start: usize,
        captures: Vec<Option<usize>>,
        budget: &mut usize,
    ) -> Vec<(usize, Vec<Option<usize>>)> {
        let mut stack = vec![Frame {
            pc: 0,
            pos: start,
            captures,
        }];
        let mut results = Vec::new();
        while let Some(mut frame) = stack.pop() {
            loop {
                if *budget == 0 {
                    return results;
                }
                *budget -= 1;
                let Some(inst) = program.get(frame.pc) else {
                    break;
                };
                match inst {
                    Inst::Char(expected) => {
                        let Some(actual) = text.get(frame.pos) else {
                            break;
                        };
                        if chars_equal(*expected, *actual, self.flags.ignore_case) {
                            frame.pos += 1;
                            frame.pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Any => {
                        let Some(actual) = text.get(frame.pos) else {
                            break;
                        };
                        if self.flags.dot_all || !is_line_terminator(*actual) {
                            frame.pos += 1;
                            frame.pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Class(class) => {
                        let Some(actual) = text.get(frame.pos) else {
                            break;
                        };
                        let matches = class.matches(*actual)
                            || (self.flags.ignore_case
                                && (*actual)
                                    .to_uppercase()
                                    .next()
                                    .is_some_and(|upper| class.matches(upper)));
                        if matches {
                            frame.pos += 1;
                            frame.pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Backref(group) => {
                        let from = frame.captures.get(group * 2).copied().flatten();
                        let to = frame.captures.get(group * 2 + 1).copied().flatten();
                        match (from, to) {
                            (Some(from), Some(to)) if from <= to && to <= text.len() => {
                                let captured = &text[from..to];
                                let remaining = text.get(frame.pos..).unwrap_or(&[]);
                                if captured.len() <= remaining.len()
                                    && captured
                                        .iter()
                                        .zip(remaining.iter())
                                        .all(|(a, b)| chars_equal(*a, *b, self.flags.ignore_case))
                                {
                                    frame.pos += captured.len();
                                    frame.pc += 1;
                                } else {
                                    break;
                                }
                            }
                            // Per ECMAScript semantics, a group that did not
                            // participate matches the empty string here.
                            _ => frame.pc += 1,
                        }
                    }
                    Inst::Lookaround {
                        behind,
                        negative,
                        program: inner,
                    } => {
                        let mut outcomes = Vec::new();
                        if *behind {
                            // A lookbehind succeeds when the nested program can
                            // match a span ending exactly at the current spot.
                            for candidate in 0..=frame.pos {
                                let mut found = self.run_program(
                                    inner,
                                    text,
                                    candidate,
                                    frame.captures.clone(),
                                    budget,
                                );
                                found.retain(|(end, _)| *end == frame.pos);
                                outcomes.extend(found);
                            }
                        } else {
                            outcomes = self.run_program(
                                inner,
                                text,
                                frame.pos,
                                frame.captures.clone(),
                                budget,
                            );
                        }
                        if *negative {
                            if outcomes.is_empty() {
                                frame.pc += 1;
                            } else {
                                break;
                            }
                        } else {
                            // The assertion is zero-width: each successful
                            // inner path resumes at the original position,
                            // carrying the captures it made. Pushing them all
                            // lets the caller backtrack into the assertion.
                            for (_, captures) in outcomes.into_iter().rev() {
                                stack.push(Frame {
                                    pc: frame.pc + 1,
                                    pos: frame.pos,
                                    captures,
                                });
                            }
                            break;
                        }
                    }
                    Inst::Save(slot) => {
                        if let Some(target) = frame.captures.get_mut(*slot) {
                            *target = Some(frame.pos);
                        }
                        frame.pc += 1;
                    }
                    Inst::Split(first, second) => {
                        stack.push(Frame {
                            pc: *second,
                            pos: frame.pos,
                            captures: frame.captures.clone(),
                        });
                        frame.pc = *first;
                    }
                    Inst::Jump(target) => {
                        frame.pc = *target;
                    }
                    Inst::AssertStart => {
                        let ok = frame.pos == 0
                            || (self.flags.multiline
                                && frame
                                    .pos
                                    .checked_sub(1)
                                    .and_then(|index| text.get(index))
                                    .is_some_and(|ch| is_line_terminator(*ch)));
                        if ok {
                            frame.pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::AssertEnd => {
                        let ok = frame.pos == text.len()
                            || (self.flags.multiline
                                && (text
                                    .get(frame.pos)
                                    .is_some_and(|ch| is_line_terminator(*ch))
                                    || (frame.pos + 1 == text.len()
                                        && text
                                            .get(frame.pos)
                                            .is_some_and(|ch| is_line_terminator(*ch)))))
                            || (frame.pos + 1 == text.len()
                                && text
                                    .get(frame.pos)
                                    .is_some_and(|ch| is_line_terminator(*ch)));
                        if ok {
                            frame.pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::WordBoundary(want_boundary) => {
                        let before = frame
                            .pos
                            .checked_sub(1)
                            .and_then(|index| text.get(index))
                            .is_some_and(|ch| is_word_char(*ch));
                        let after = text.get(frame.pos).is_some_and(|ch| is_word_char(*ch));
                        let is_boundary = before != after;
                        if is_boundary == *want_boundary {
                            frame.pc += 1;
                        } else {
                            break;
                        }
                    }
                    Inst::Match => {
                        results.push((frame.pos, frame.captures));
                        break;
                    }
                }
            }
        }
        results
    }
}

struct Frame {
    pc: usize,
    pos: usize,
    captures: Vec<Option<usize>>,
}

fn chars_equal(expected: char, actual: char, ignore_case: bool) -> bool {
    if !ignore_case {
        return expected == actual;
    }
    lower_char(expected) == lower_char(actual)
}

fn lower_char(ch: char) -> char {
    let mut lowered = ch.to_lowercase();
    match (lowered.next(), lowered.next()) {
        (Some(single), None) => single,
        _ => ch,
    }
}

struct RegexParser {
    chars: Vec<char>,
    pos: usize,
    capture_count: usize,
    depth: usize,
    group_names: Vec<Option<String>>,
}

impl RegexParser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            capture_count: 0,
            depth: 0,
            // Index zero reserves the whole match, which is never named.
            group_names: vec![None],
        }
    }

    fn parse(&mut self) -> Result<(Node, usize, Vec<Option<String>>), RegexError> {
        let node = self.parse_alternation()?;
        if self.pos < self.chars.len() {
            return Err(RegexError::new(format!(
                "unexpected `{}` in regular expression",
                self.chars[self.pos]
            )));
        }
        Ok((
            node,
            self.capture_count,
            std::mem::take(&mut self.group_names),
        ))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += 1;
        Some(ch)
    }

    fn parse_alternation(&mut self) -> Result<Node, RegexError> {
        let mut alternatives = vec![self.parse_sequence()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            alternatives.push(self.parse_sequence()?);
        }
        if alternatives.len() == 1 {
            Ok(alternatives.remove(0))
        } else {
            Ok(Node::Alt(alternatives))
        }
    }

    fn parse_sequence(&mut self) -> Result<Node, RegexError> {
        let mut items = Vec::new();
        while let Some(ch) = self.peek() {
            if ch == '|' || ch == ')' {
                break;
            }
            if matches!(ch, '*' | '+' | '?') {
                return Err(RegexError::new("nothing to repeat in regular expression"));
            }
            if ch == '{' {
                // A `{` that does not start a quantifier is a literal brace.
                if self.peek_at(1).is_some_and(|next| next.is_ascii_digit()) {
                    return Err(RegexError::new("nothing to repeat in regular expression"));
                }
                self.pos += 1;
                items.push(Node::Char('{'));
                continue;
            }
            let atom = self.parse_atom()?;
            let atom = self.parse_quantifier(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.remove(0),
            _ => Node::Seq(items),
        })
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn parse_quantifier(&mut self, atom: Node) -> Result<Node, RegexError> {
        let Some(ch) = self.peek() else {
            return Ok(atom);
        };
        let (min, max) = match ch {
            '*' => (0, None),
            '+' => (1, None),
            '?' => (0, Some(1)),
            '{' => {
                let saved = self.pos;
                match self.parse_braces() {
                    Some(bounds) => bounds,
                    None => {
                        self.pos = saved;
                        return Ok(atom);
                    }
                }
            }
            _ => return Ok(atom),
        };
        if let Some(max) = max
            && max < min
        {
            return Err(RegexError::new(format!(
                "numbers out of order in `{{{min},{max}}}` quantifier"
            )));
        }
        if min > 1000 || max.is_some_and(|max| max > 1000) {
            return Err(RegexError::new(
                "quantifier bounds are too large in regular expression",
            ));
        }
        self.pos += 1;
        let greedy = if self.peek() == Some('?') {
            self.pos += 1;
            false
        } else {
            true
        };
        if self
            .peek()
            .is_some_and(|next| matches!(next, '*' | '+' | '?'))
        {
            return Err(RegexError::new("nothing to repeat in regular expression"));
        }
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    /// Parse `{n}`, `{n,}` or `{n,m}` with the leading `{` still current.
    /// Returns `None` when the text is not a valid quantifier, in which case the
    /// brace is treated as a literal by the caller.
    fn parse_braces(&mut self) -> Option<(usize, Option<usize>)> {
        let mut cursor = self.pos + 1;
        let digits_start = cursor;
        while self.chars.get(cursor).is_some_and(|ch| ch.is_ascii_digit()) {
            cursor += 1;
        }
        if cursor == digits_start {
            return None;
        }
        let min: usize = self.chars[digits_start..cursor]
            .iter()
            .collect::<String>()
            .parse()
            .ok()?;
        match self.chars.get(cursor) {
            Some('}') => {
                self.pos = cursor;
                Some((min, Some(min)))
            }
            Some(',') => {
                cursor += 1;
                let max_start = cursor;
                while self.chars.get(cursor).is_some_and(|ch| ch.is_ascii_digit()) {
                    cursor += 1;
                }
                if self.chars.get(cursor) != Some(&'}') {
                    return None;
                }
                let max = if cursor == max_start {
                    None
                } else {
                    let value: usize = self.chars[max_start..cursor]
                        .iter()
                        .collect::<String>()
                        .parse()
                        .ok()?;
                    Some(value)
                };
                self.pos = cursor;
                Some((min, max))
            }
            _ => None,
        }
    }

    fn parse_atom(&mut self) -> Result<Node, RegexError> {
        let ch = self
            .next()
            .ok_or_else(|| RegexError::new("unexpected end of regular expression"))?;
        Ok(match ch {
            '.' => Node::Any,
            '^' => Node::AssertStart,
            '$' => Node::AssertEnd,
            '[' => Node::Class(self.parse_class()?),
            '(' => {
                self.depth += 1;
                if self.depth > 200 {
                    return Err(RegexError::new(
                        "regular expression groups are nested too deeply",
                    ));
                }
                let group = if self.peek() == Some('?') {
                    self.pos += 1;
                    if self.peek() == Some(':') {
                        self.pos += 1;
                        let node = self.parse_alternation()?;
                        self.expect_close_group()?;
                        Node::Group {
                            index: None,
                            node: Box::new(node),
                        }
                    } else if self.peek().is_some_and(|ch| matches!(ch, '=' | '!')) {
                        let negative = self.next() == Some('!');
                        let node = self.parse_alternation()?;
                        self.expect_close_group()?;
                        Node::Lookaround {
                            behind: false,
                            negative,
                            node: Box::new(node),
                        }
                    } else if self.peek() == Some('<')
                        && matches!(self.peek_at(1), Some('=') | Some('!'))
                    {
                        self.pos += 1; // `<`
                        let negative = self.next() == Some('!');
                        let node = self.parse_alternation()?;
                        self.expect_close_group()?;
                        Node::Lookaround {
                            behind: true,
                            negative,
                            node: Box::new(node),
                        }
                    } else if self.peek() == Some('<') {
                        let name = self.parse_group_name()?;
                        self.capture_count += 1;
                        let index = self.capture_count;
                        if self.group_names.len() <= index {
                            self.group_names.push(None);
                        }
                        self.group_names[index] = Some(name);
                        let node = self.parse_alternation()?;
                        self.expect_close_group()?;
                        Node::Group {
                            index: Some(index),
                            node: Box::new(node),
                        }
                    } else {
                        let modifier = self.peek().map(|ch| ch.to_string()).unwrap_or_default();
                        return Err(RegexError::new(format!(
                            "unsupported group modifier `(?{modifier}` in regular expression"
                        )));
                    }
                } else {
                    self.capture_count += 1;
                    let index = self.capture_count;
                    self.group_names.push(None);
                    let node = self.parse_alternation()?;
                    self.expect_close_group()?;
                    Node::Group {
                        index: Some(index),
                        node: Box::new(node),
                    }
                };
                self.depth -= 1;
                group
            }
            '\\' => self.parse_escape()?,
            ')' => return Err(RegexError::new("unmatched `)` in regular expression")),
            other => Node::Char(other),
        })
    }

    fn expect_close_group(&mut self) -> Result<(), RegexError> {
        if self.peek() == Some(')') {
            self.pos += 1;
            Ok(())
        } else {
            Err(RegexError::new("unterminated group in regular expression"))
        }
    }

    /// Parse the name of a `(?<name>...)` group. The leading `?<` is still
    /// current; on return the closing `>` has been consumed.
    fn parse_group_name(&mut self) -> Result<String, RegexError> {
        self.pos += 1; // `<`
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '>' {
                break;
            }
            if !(ch.is_alphanumeric() || ch == '_' || ch == '$') {
                return Err(RegexError::new("invalid character in named capture group"));
            }
            self.pos += 1;
        }
        if self.pos == start || self.peek() != Some('>') {
            return Err(RegexError::new("unterminated named capture group"));
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        self.pos += 1; // `>`
        Ok(name)
    }

    fn parse_escape(&mut self) -> Result<Node, RegexError> {
        let ch = self
            .next()
            .ok_or_else(|| RegexError::new("unterminated escape in regular expression"))?;
        Ok(match ch {
            'd' => Node::Class(Class {
                negated: false,
                items: vec![ClassItem::Digit],
            }),
            'D' => Node::Class(Class {
                negated: false,
                items: vec![ClassItem::NotDigit],
            }),
            'w' => Node::Class(Class {
                negated: false,
                items: vec![ClassItem::Word],
            }),
            'W' => Node::Class(Class {
                negated: false,
                items: vec![ClassItem::NotWord],
            }),
            's' => Node::Class(Class {
                negated: false,
                items: vec![ClassItem::Space],
            }),
            'S' => Node::Class(Class {
                negated: false,
                items: vec![ClassItem::NotSpace],
            }),
            'b' => Node::WordBoundary(true),
            'B' => Node::WordBoundary(false),
            'n' => Node::Char('\n'),
            'r' => Node::Char('\r'),
            't' => Node::Char('\t'),
            'f' => Node::Char('\u{000C}'),
            'v' => Node::Char('\u{000B}'),
            '0' => Node::Char('\0'),
            'x' => Node::Char(self.parse_hex(2)?),
            'u' => Node::Char(self.parse_unicode_escape()?),
            'c' => {
                let letter = self
                    .next()
                    .ok_or_else(|| RegexError::new("unterminated control escape"))?;
                if letter.is_ascii_alphabetic() {
                    Node::Char(char::from_u32(u32::from(letter) % 32).unwrap_or(letter))
                } else {
                    Node::Char('c')
                }
            }
            'k' => {
                if self.peek() == Some('<') {
                    let name = self.parse_group_name()?;
                    let index = self
                        .group_names
                        .iter()
                        .position(|existing| existing.as_deref() == Some(name.as_str()))
                        .ok_or_else(|| {
                            RegexError::new(format!(
                                "invalid named backreference `\\k<{name}>` in regular expression"
                            ))
                        })?;
                    Node::Backref(index)
                } else {
                    // Annex B identity escape when `\k` is not a named
                    // backreference.
                    Node::Char('k')
                }
            }
            digit if digit.is_ascii_digit() => {
                // `\0` is handled above; here we have `1` .. `9`. Read the
                // whole run of digits so `\10` can refer to group ten when it
                // exists. Otherwise only the first digit is a backreference
                // and the rest are literal characters.
                let first = self.pos - 1;
                let mut end = self.pos;
                while self.chars.get(end).is_some_and(|ch| ch.is_ascii_digit()) {
                    end += 1;
                }
                let digits: String = self.chars[first..end].iter().collect();
                let whole: usize = digits.parse().unwrap_or(usize::MAX);
                if whole >= 1 && whole <= self.capture_count {
                    self.pos = end;
                    Node::Backref(whole)
                } else {
                    let single = digit.to_digit(10).unwrap() as usize;
                    if single <= self.capture_count {
                        self.pos = first + 1;
                        Node::Backref(single)
                    } else {
                        return Err(RegexError::new(format!(
                            "invalid backreference `\\{digits}` in regular expression"
                        )));
                    }
                }
            }
            other => Node::Char(other),
        })
    }

    fn parse_hex(&mut self, digits: usize) -> Result<char, RegexError> {
        let mut value: u32 = 0;
        for _ in 0..digits {
            let digit = self
                .next()
                .and_then(|ch| ch.to_digit(16))
                .ok_or_else(|| RegexError::new("invalid hexadecimal escape"))?;
            value = value * 16 + digit;
        }
        Ok(char::from_u32(value).unwrap_or('\u{FFFD}'))
    }

    fn parse_unicode_escape(&mut self) -> Result<char, RegexError> {
        if self.peek() == Some('{') {
            self.pos += 1;
            let mut value: u32 = 0;
            let mut any = false;
            while let Some(ch) = self.peek() {
                if ch == '}' {
                    break;
                }
                let digit = ch
                    .to_digit(16)
                    .ok_or_else(|| RegexError::new("invalid unicode escape"))?;
                value = value.saturating_mul(16).saturating_add(digit);
                any = true;
                self.pos += 1;
            }
            if !any || self.peek() != Some('}') {
                return Err(RegexError::new("invalid unicode escape"));
            }
            self.pos += 1;
            return Ok(char::from_u32(value).unwrap_or('\u{FFFD}'));
        }
        self.parse_hex(4)
    }

    fn parse_class(&mut self) -> Result<Class, RegexError> {
        let mut negated = false;
        if self.peek() == Some('^') {
            negated = true;
            self.pos += 1;
        }
        let mut items = Vec::new();
        if self.peek() == Some(']') {
            // `[]` is an empty class that matches nothing.
            self.pos += 1;
            return Ok(Class { negated, items });
        }
        loop {
            let Some(ch) = self.peek() else {
                return Err(RegexError::new(
                    "unterminated character class in regular expression",
                ));
            };
            if ch == ']' {
                self.pos += 1;
                break;
            }
            let start = self.parse_class_atom()?;
            if self.peek() == Some('-') && self.peek_at(1).is_some_and(|next| next != ']') {
                self.pos += 1;
                let end = self.parse_class_atom()?;
                match (start, end) {
                    (ClassAtom::Char(from), ClassAtom::Char(to)) => {
                        if from > to {
                            return Err(RegexError::new("invalid character class range"));
                        }
                        items.push(ClassItem::Range(from, to));
                    }
                    // Annex B / web reality: a `-` that cannot form a range
                    // because one endpoint is a class escape (`\w`, `\d`,
                    // `\s`, ...) is just a literal hyphen. Escaped characters
                    // such as `\.` are still ordinary endpoints, so ranges
                    // between them keep working.
                    (start, end) => {
                        items.push(start.into_item());
                        items.push(ClassItem::Range('-', '-'));
                        items.push(end.into_item());
                    }
                }
            } else {
                items.push(start.into_item());
            }
        }
        Ok(Class { negated, items })
    }

    fn parse_class_atom(&mut self) -> Result<ClassAtom, RegexError> {
        let ch = self
            .next()
            .ok_or_else(|| RegexError::new("unterminated character class"))?;
        if ch != '\\' {
            return Ok(ClassAtom::Char(ch));
        }
        let escaped = self
            .next()
            .ok_or_else(|| RegexError::new("unterminated escape in character class"))?;
        Ok(match escaped {
            'd' => ClassAtom::Item(ClassItem::Digit),
            'D' => ClassAtom::Item(ClassItem::NotDigit),
            'w' => ClassAtom::Item(ClassItem::Word),
            'W' => ClassAtom::Item(ClassItem::NotWord),
            's' => ClassAtom::Item(ClassItem::Space),
            'S' => ClassAtom::Item(ClassItem::NotSpace),
            'b' => ClassAtom::Char('\u{0008}'),
            'n' => ClassAtom::Char('\n'),
            'r' => ClassAtom::Char('\r'),
            't' => ClassAtom::Char('\t'),
            'f' => ClassAtom::Char('\u{000C}'),
            'v' => ClassAtom::Char('\u{000B}'),
            '0' => ClassAtom::Char('\0'),
            'x' => ClassAtom::Char(self.parse_hex(2)?),
            'u' => ClassAtom::Char(self.parse_unicode_escape()?),
            other => ClassAtom::Char(other),
        })
    }
}

enum ClassAtom {
    Char(char),
    Item(ClassItem),
}

impl ClassAtom {
    fn into_item(self) -> ClassItem {
        match self {
            Self::Item(item) => item,
            Self::Char(ch) => ClassItem::Range(ch, ch),
        }
    }
}

fn compile_node(node: &Node, program: &mut Vec<Inst>) {
    match node {
        Node::Empty => {}
        Node::Char(ch) => program.push(Inst::Char(*ch)),
        Node::Any => program.push(Inst::Any),
        Node::Class(class) => program.push(Inst::Class(class.clone())),
        Node::Backref(group) => program.push(Inst::Backref(*group)),
        Node::Lookaround {
            behind,
            negative,
            node,
        } => {
            let mut inner = Vec::new();
            compile_node(node, &mut inner);
            inner.push(Inst::Match);
            program.push(Inst::Lookaround {
                behind: *behind,
                negative: *negative,
                program: inner,
            });
        }
        Node::Seq(items) => {
            for item in items {
                compile_node(item, program);
            }
        }
        Node::Alt(alternatives) => {
            let mut end_jumps = Vec::new();
            for (index, alternative) in alternatives.iter().enumerate() {
                if index + 1 < alternatives.len() {
                    let split = program.len();
                    program.push(Inst::Split(0, 0));
                    let body = program.len();
                    compile_node(alternative, program);
                    let jump = program.len();
                    program.push(Inst::Jump(0));
                    end_jumps.push(jump);
                    let next = program.len();
                    if let Some(Inst::Split(first, second)) = program.get_mut(split) {
                        *first = body;
                        *second = next;
                    }
                } else {
                    compile_node(alternative, program);
                }
            }
            let end = program.len();
            for jump in end_jumps {
                if let Some(Inst::Jump(target)) = program.get_mut(jump) {
                    *target = end;
                }
            }
        }
        Node::Group { index, node } => {
            if let Some(index) = index {
                program.push(Inst::Save(index * 2));
                compile_node(node, program);
                program.push(Inst::Save(index * 2 + 1));
            } else {
                compile_node(node, program);
            }
        }
        Node::Repeat {
            node,
            min,
            max,
            greedy,
        } => compile_repeat(node, *min, *max, *greedy, program),
        Node::AssertStart => program.push(Inst::AssertStart),
        Node::AssertEnd => program.push(Inst::AssertEnd),
        Node::WordBoundary(want) => program.push(Inst::WordBoundary(*want)),
    }
}

fn compile_star(node: &Node, greedy: bool, program: &mut Vec<Inst>) {
    let split = program.len();
    program.push(Inst::Split(0, 0));
    let body = program.len();
    compile_node(node, program);
    program.push(Inst::Jump(split));
    let end = program.len();
    if let Some(Inst::Split(first, second)) = program.get_mut(split) {
        if greedy {
            *first = body;
            *second = end;
        } else {
            *first = end;
            *second = body;
        }
    }
}

fn compile_optional(node: &Node, greedy: bool, program: &mut Vec<Inst>) {
    let split = program.len();
    program.push(Inst::Split(0, 0));
    let body = program.len();
    compile_node(node, program);
    let end = program.len();
    if let Some(Inst::Split(first, second)) = program.get_mut(split) {
        if greedy {
            *first = body;
            *second = end;
        } else {
            *first = end;
            *second = body;
        }
    }
}

fn compile_repeat(
    node: &Node,
    min: usize,
    max: Option<usize>,
    greedy: bool,
    program: &mut Vec<Inst>,
) {
    for _ in 0..min {
        compile_node(node, program);
    }
    match max {
        None => compile_star(node, greedy, program),
        Some(max) => {
            for _ in min..max {
                compile_optional(node, greedy, program);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_match(pattern: &str, flags: &str, text: &str) -> bool {
        Regex::new(pattern, flags).expect("compile").is_match(text)
    }

    fn matched(pattern: &str, flags: &str, text: &str) -> RegexMatch {
        let chars: Vec<char> = text.chars().collect();
        Regex::new(pattern, flags)
            .expect("compile")
            .find_from(&chars, 0)
            .expect("match")
    }

    #[test]
    fn matches_literals_and_dot() {
        assert!(is_match("abc", "", "xxabcxx"));
        assert!(!is_match("abc", "", "ab"));
        assert!(is_match("a.c", "", "abc"));
        assert!(!is_match("a.c", "", "a\nc"));
        assert!(is_match("a.c", "s", "a\nc"));
    }

    #[test]
    fn matches_classes_and_shorthands() {
        assert!(is_match(r"^\d{3}$", "", "123"));
        assert!(!is_match(r"^\d{3}$", "", "12a"));
        assert!(is_match(r"^\w+$", "", "hello_1"));
        assert!(is_match(r"^\s+$", "", " \t\n"));
        assert!(is_match(r"^[a-z0-9_]+$", "", "abc_09"));
        assert!(is_match(r"^[^0-9]+$", "", "abc"));
        assert!(!is_match(r"^[^0-9]+$", "", "ab1"));
        assert!(is_match(r"^\D+$", "", "abc"));
        assert!(is_match(r"^\S+$", "", "abc"));
    }

    #[test]
    fn quantifiers_and_alternation() {
        assert!(is_match(r"^ab*c$", "", "ac"));
        assert!(is_match(r"^ab*c$", "", "abbbc"));
        assert!(is_match(r"^ab+c$", "", "abc"));
        assert!(!is_match(r"^ab+c$", "", "ac"));
        assert!(is_match(r"^ab?c$", "", "ac"));
        assert!(is_match(r"^a{2,3}$", "", "aa"));
        assert!(is_match(r"^a{2,3}$", "", "aaa"));
        assert!(!is_match(r"^a{2,3}$", "", "a"));
        assert!(is_match(r"^(cat|dog)$", "", "dog"));
    }

    #[test]
    fn captures_and_anchors() {
        let found = matched(r"(\d+)-(\d+)", "", "x 12-34 y");
        assert_eq!(found.start, 2);
        assert_eq!(found.groups[0].as_deref(), Some("12-34"));
        assert_eq!(found.groups[1].as_deref(), Some("12"));
        assert_eq!(found.groups[2].as_deref(), Some("34"));
        assert!(is_match("^b", "m", "a\nb"));
        assert!(is_match("a$", "m", "a\nb"));
        assert!(!is_match("^b", "", "a\nb"));
    }

    #[test]
    fn ignore_case_and_word_boundaries() {
        assert!(is_match("^abc$", "i", "AbC"));
        assert!(is_match(r"\bcat\b", "", "a cat here"));
        assert!(!is_match(r"\bcat\b", "", "scatter"));
        assert!(is_match(r"\Bcat", "", "scatter"));
    }

    #[test]
    fn rejects_invalid_patterns() {
        assert!(Regex::new("a(", "").is_err());
        assert!(Regex::new("[a", "").is_err());
        assert!(Regex::new("*a", "").is_err());
        assert!(Regex::new("a{3,2}", "").is_err());
        assert!(Regex::new("a", "q").is_err());
        assert!(Regex::new("a", "gg").is_err());
        assert!(Regex::new(r"\1", "").is_err());
        assert!(Regex::new("a{1001}", "").is_err());
    }

    #[test]
    fn non_capturing_groups_do_not_capture() {
        let found = matched(r"(?:ab)+c", "", "ababc");
        assert_eq!(found.groups.len(), 1);
        assert_eq!(found.groups[0].as_deref(), Some("ababc"));
    }

    #[test]
    fn sticky_flag_only_matches_at_last_index() {
        let regex = Regex::new("ab", "y").expect("compile");
        let chars: Vec<char> = "xxab".chars().collect();
        assert!(regex.find_from(&chars, 2).is_some());
        assert!(regex.find_from(&chars, 0).is_none());
        assert!(regex.find_from(&chars, 1).is_none());
        assert_eq!(Regex::new("ab", "y").unwrap().flags().as_string(), "y");
    }

    #[test]
    fn named_capture_groups_record_their_names() {
        let regex = Regex::new(r"(?<year>\d{4})-(?<month>\d{2})", "").expect("compile");
        assert_eq!(
            regex.group_names(),
            &[None, Some("year".to_owned()), Some("month".to_owned())]
        );
        let chars: Vec<char> = "date 2024-07".chars().collect();
        let found = regex.find_from(&chars, 0).expect("match");
        assert_eq!(found.groups[1].as_deref(), Some("2024"));
        assert_eq!(found.groups[2].as_deref(), Some("07"));
    }

    #[test]
    fn indices_flag_reports_group_ranges() {
        let regex = Regex::new(r"(\d+)-(\d+)", "d").expect("compile");
        let chars: Vec<char> = "xx12-34yy".chars().collect();
        let found = regex.find_from(&chars, 0).expect("match");
        assert_eq!(found.start, 2);
        assert_eq!(found.end, 7);
        assert_eq!(found.ranges[0], Some((2, 7)));
        assert_eq!(found.ranges[1], Some((2, 4)));
        assert_eq!(found.ranges[2], Some((5, 7)));
        assert_eq!(Regex::new("a", "dg").unwrap().flags().as_string(), "gd");
    }

    #[test]
    fn rejects_bad_named_groups_and_group_modifiers() {
        assert!(Regex::new("(?<a", "").is_err());
        assert!(Regex::new("(?<a-b>x)", "").is_err());
        assert!(Regex::new("(?>a)", "").is_err());
        let error = Regex::new("(?>a)", "").expect_err("unsupported modifier");
        assert!(error.message.contains("unsupported group modifier"));
    }

    #[test]
    fn numeric_backreferences_match_previous_captures() {
        let found = matched(r"(\w+)\s+\1", "", "say go go now");
        assert_eq!(found.start, 4);
        assert_eq!(found.end, 9);
        assert_eq!(found.groups[0].as_deref(), Some("go go"));
        assert_eq!(found.groups[1].as_deref(), Some("go"));

        assert!(is_match(r"^(\d)\1$", "", "77"));
        assert!(!is_match(r"^(\d)\1$", "", "78"));
        assert!(is_match(r"^(\d)(\d)\2\1$", "", "1221"));
        // A group that did not participate matches the empty string.
        assert!(is_match(r"^(a)?b\1$", "", "b"));
        // Multi-digit backreferences resolve to the group with that number.
        assert!(is_match(
            r"^(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\10$",
            "",
            "abcdefghijj"
        ));
    }

    #[test]
    fn named_backreferences_match_previous_captures() {
        let found = matched(r"(?<word>\w+)\s+\k<word>", "", "echo hi hi there");
        assert_eq!(found.groups[0].as_deref(), Some("hi hi"));
        assert_eq!(found.groups[1].as_deref(), Some("hi"));

        assert!(is_match(r"^(?<x>a)(?<y>b)\k<y>\k<x>$", "", "abba"));
        assert!(is_match(r"^(?<tag>[a-z]+)<\/\k<tag>>$", "", "p</p>"));
        // Named and numeric backreferences can point at the same group.
        assert!(is_match(r"^(?<x>a)\1\k<x>$", "", "aaa"));
        // The `i` flag applies to the backreference as well.
        assert!(is_match(r"^(?<x>a)\k<x>$", "i", "aA"));
    }

    #[test]
    fn backreferences_honour_ignore_case_sticky_and_indices() {
        assert!(is_match(r"^(a)\1$", "i", "aA"));
        assert!(!is_match(r"^(a)\1$", "", "aA"));

        let regex = Regex::new(r"(?<a>ab)\k<a>", "dy").expect("compile");
        let chars: Vec<char> = "zzabab".chars().collect();
        assert!(regex.find_from(&chars, 0).is_none());
        let found = regex.find_from(&chars, 2).expect("match");
        assert_eq!(found.groups[1].as_deref(), Some("ab"));
        assert_eq!(found.ranges[0], Some((2, 6)));
        assert_eq!(found.ranges[1], Some((2, 4)));
    }

    #[test]
    fn invalid_backreferences_are_syntax_errors() {
        assert!(Regex::new(r"\1", "").is_err()); // no groups at all
        assert!(Regex::new(r"(a)\2", "").is_err()); // out of range
        assert!(Regex::new(r"\1(a)", "").is_err()); // forward reference
        assert!(Regex::new(r"\k<x>", "").is_err()); // name not defined
        assert!(Regex::new(r"(a)\k<other>", "").is_err()); // unknown name
        assert!(Regex::new(r"\k<x>(?<x>a)", "").is_err()); // forward named reference
        assert!(Regex::new(r"(?<x>a)\k<x", "").is_err()); // unterminated name
    }

    #[test]
    fn youtube_script_tag_pattern_with_numeric_backreference() {
        // Exact pattern from spf.js line 53 and network.js line 29:
        // /\x3c(script|style)([\s\S]*?)\x3e([\s\S]*?)\x3c\/\1\x3e/ig
        let regex = Regex::new(
            r"\x3c(script|style)([\s\S]*?)\x3e([\s\S]*?)\x3c\/\1\x3e",
            "ig",
        )
        .expect("compile");
        assert_eq!(regex.capture_count(), 3);

        let chars: Vec<char> = "<script>var x=1;</script><style>a{}</style>"
            .chars()
            .collect();
        let first = regex.find_from(&chars, 0).expect("first match");
        assert_eq!(
            first.groups[0].as_deref(),
            Some("<script>var x=1;</script>")
        );
        assert_eq!(first.groups[1].as_deref(), Some("script"));
        assert_eq!(first.groups[2].as_deref(), Some(""));
        assert_eq!(first.groups[3].as_deref(), Some("var x=1;"));

        let second = regex.find_from(&chars, first.end).expect("second match");
        assert_eq!(second.groups[1].as_deref(), Some("style"));
        assert_eq!(second.groups[3].as_deref(), Some("a{}"));

        // The backreference ties the closing tag to the opening tag.
        assert!(!regex.is_match("<script>x</style>"));
        // With `i`, the backreference comparison is case insensitive too.
        assert!(regex.is_match("<SCRIPT>x</ScRiPt>"));
    }

    #[test]
    fn youtube_web_animations_pattern_with_numeric_backreference() {
        // From web-animations-next-lite.min.js line 80: /(N|D)O\1/g
        let regex = Regex::new(r"(N|D)O\1", "g").expect("compile");
        assert!(regex.is_match("NON"));
        assert!(regex.is_match("DOD"));
        assert!(!regex.is_match("NOX"));
    }

    #[test]
    fn positive_and_negative_lookahead_are_zero_width() {
        assert!(is_match(r"^(?=a)a$", "", "a"));
        assert!(!is_match(r"^(?=a)b$", "", "b"));
        assert!(is_match(r"^(?!a)b$", "", "b"));
        assert!(!is_match(r"^(?!a)a$", "", "a"));

        let found = matched(r"(?=b)b", "", "abc");
        assert_eq!(found.start, 1);
        assert_eq!(found.end, 2);
        assert_eq!(found.groups[0].as_deref(), Some("b"));

        // Captures made inside a positive lookahead are kept.
        let found = matched(r"(?=(a))a", "", "a");
        assert_eq!(found.groups[1].as_deref(), Some("a"));
    }

    #[test]
    fn lookahead_can_be_backtracked_into() {
        // Greedy `a+` inside the assertion first consumes both `a`s; the
        // engine must backtrack inside the lookahead to leave one for `a`.
        let found = matched(r"(?=(a+)a)a+b", "", "aab");
        assert_eq!(found.groups[0].as_deref(), Some("aab"));
        assert_eq!(found.groups[1].as_deref(), Some("a"));
    }

    #[test]
    fn lookbehind_assertions() {
        let found = matched(r"(?<=ab)c", "", "abc");
        assert_eq!(found.start, 2);
        assert_eq!(found.end, 3);
        assert_eq!(found.groups[0].as_deref(), Some("c"));
        assert!(!is_match(r"(?<=ab)c", "", "ac"));

        assert!(is_match(r"(?<!ab)c", "", "ac"));
        assert!(!is_match(r"(?<!ab)c", "", "abc"));

        // Variable-length lookbehind is supported by trying each start.
        assert!(is_match(r"(?<=a+)b", "", "aaab"));
        assert!(!is_match(r"(?<=a+)b", "", "cb"));

        // Captures made inside a lookbehind are kept too.
        let found = matched(r"(?<=(ab))c", "", "abc");
        assert_eq!(found.groups[1].as_deref(), Some("ab"));

        // Variable-length lookbehind prefers the longest match, like V8.
        let found = matched(r"(?<=(a+))b", "", "aaab");
        assert_eq!(found.groups[1].as_deref(), Some("aaa"));
    }

    #[test]
    fn youtube_css_custom_property_pattern_with_lookahead() {
        // Exact pattern from webcomponents-sd.js line 149:
        // /(?:^|[;\s{]\s*)(--[\w-]*?)\s*:\s*(?:((?:'(?:\\'|.)*?'|"(?:\\"|.)*?"|\([^)]*?\)|[^};{])+)|\{([^}]*)\}(?:(?=[;\s}])|$))/gi
        let regex = Regex::new(
            r#"(?:^|[;\s{]\s*)(--[\w-]*?)\s*:\s*(?:((?:'(?:\\'|.)*?'|"(?:\\"|.)*?"|\([^)]*?\)|[^};{])+)|\{([^}]*)\}(?:(?=[;\s}])|$))"#,
            "gi",
        )
        .expect("compile");
        assert_eq!(regex.capture_count(), 3);

        let chars: Vec<char> = "--brand: red;".chars().collect();
        let found = regex.find_from(&chars, 0).expect("custom property");
        assert_eq!(found.groups[1].as_deref(), Some("--brand"));
        assert_eq!(found.groups[2].as_deref(), Some("red"));

        let chars: Vec<char> = ":root { --brand: #f00; }".chars().collect();
        let found = regex.find_from(&chars, 0).expect("inside a rule");
        assert_eq!(found.start, 6);
        assert_eq!(found.groups[1].as_deref(), Some("--brand"));
        assert_eq!(found.groups[2].as_deref(), Some("#f00"));

        // The `{ ... }` value branch relies on `(?=[;\s}])`.
        let chars: Vec<char> = "--bg: { color: red };".chars().collect();
        let found = regex.find_from(&chars, 0).expect("brace value");
        assert_eq!(found.groups[1].as_deref(), Some("--bg"));
        assert_eq!(found.groups[2], None);
        assert_eq!(found.groups[3].as_deref(), Some(" color: red "));
    }

    #[test]
    fn lookaround_composes_with_backreferences_and_named_groups() {
        let found = matched(r"(?<w>\w+)(?=\s+\k<w>)", "", "go go");
        assert_eq!(found.groups[1].as_deref(), Some("go"));
        assert_eq!(found.end, 2);
        assert!(is_match(r"(?<w>\w+)\s+(?=\k<w>)\k<w>", "", "go go"));
    }

    #[test]
    fn youtube_main_module_pattern_with_class_escape_hyphen() {
        // Exact pattern from YouTube's main module:
        // /(^\/[^\/])|(^#)|(^[\w-\d]*:)/
        let regex = Regex::new(r"(^\/[^\/])|(^#)|(^[\w-\d]*:)", "").expect("compile");

        let found = matched(r"(^\/[^\/])|(^#)|(^[\w-\d]*:)", "", "/path");
        assert_eq!(found.groups[0].as_deref(), Some("/p"));
        assert_eq!(found.groups[1].as_deref(), Some("/p"));

        let found = matched(r"(^\/[^\/])|(^#)|(^[\w-\d]*:)", "", "#frag");
        assert_eq!(found.groups[0].as_deref(), Some("#"));
        assert_eq!(found.groups[2].as_deref(), Some("#"));

        for text in ["scheme:", "a-b:", "123:"] {
            assert!(regex.is_match(text), "{text} should match");
        }
        // The hyphen between `\w` and `\d` is a literal class member.
        assert!(is_match(r"^[\w-\d]*$", "", "-"));
    }

    #[test]
    fn youtube_main_module_direction_pattern_with_class_escape_hyphen() {
        // Exact pattern from YouTube's main module:
        // /([\s\w-#\.\[\]\*]*):dir\((ltr|rtl)\)/g
        let regex = Regex::new(r"([\s\w-#\.\[\]\*]*):dir\((ltr|rtl)\)", "g").expect("compile");
        assert_eq!(regex.capture_count(), 2);

        let found = matched(
            r"([\s\w-#\.\[\]\*]*):dir\((ltr|rtl)\)",
            "",
            "width:dir(ltr)",
        );
        assert_eq!(found.groups[0].as_deref(), Some("width:dir(ltr)"));
        assert_eq!(found.groups[1].as_deref(), Some("width"));
        assert_eq!(found.groups[2].as_deref(), Some("ltr"));

        let found = matched(
            r"([\s\w-#\.\[\]\*]*):dir\((ltr|rtl)\)",
            "",
            "a-b[0].x *:dir(rtl)",
        );
        assert_eq!(found.groups[1].as_deref(), Some("a-b[0].x *"));
        assert_eq!(found.groups[2].as_deref(), Some("rtl"));

        // The literal hyphen is part of the class.
        assert!(is_match(r"^[\s\w-#\.\[\]\*]+$", "", "-"));
    }

    #[test]
    fn youtube_main_module_unicode_flag() {
        // YouTube's main module contains regex literals with the `u` (unicode)
        // flag (e.g. patterns using `\u{...}` code-point escapes). The module is
        // versioned, so the exact line/column of a given pattern moves between
        // releases; this representative pattern exercises the same flag and
        // the same approximation of Unicode semantics.
        let regex = Regex::new(r"^(?:\w|\u{1F600})+$", "gu").expect("compile");
        assert!(regex.flags().unicode);
        assert!(regex.flags().global);
        assert_eq!(regex.flags().as_string(), "gu");

        // Astral code points are matched as single characters, not as UTF-16
        // surrogate pairs, and the pattern above compiles and runs with `u`.
        assert!(regex.is_match("abc\u{1F600}"));
        assert!(regex.is_match("\u{1F600}\u{1F600}"));
        assert!(!regex.is_match("abc!"));

        // `u` composes with every previously supported flag.
        let combined = Regex::new(r"^[a-z]+$", "gimsuyd").expect("compile");
        assert!(combined.flags().global);
        assert!(combined.flags().ignore_case);
        assert!(combined.flags().multiline);
        assert!(combined.flags().dot_all);
        assert!(combined.flags().sticky);
        assert!(combined.flags().unicode);
        assert!(combined.flags().has_indices);

        // `v` (unicode sets) is accepted as well, and duplicates are still
        // rejected for both new flags.
        let sets = Regex::new(r"\w+", "v").expect("compile");
        assert!(sets.flags().unicode_sets);
        assert_eq!(sets.flags().as_string(), "v");
        assert!(Regex::new("a", "uu").is_err());
        assert!(Regex::new("a", "vv").is_err());
    }

    #[test]
    fn character_class_ranges_still_work() {
        assert!(is_match(r"^[a-z]+$", "", "hello"));
        assert!(!is_match(r"^[a-z]+$", "", "HELLO"));
        assert!(is_match(r"^[\x30-\x39]+$", "", "0123"));
        assert!(is_match(r"^[\.-a]+$", "", "/0."));
        assert!(is_match(r"^[a-]+$", "", "-"));
        assert!(is_match(r"^[-a]+$", "", "-"));
        // Real out-of-order ranges are still rejected.
        assert!(Regex::new(r"[z-a]", "").is_err());
        assert!(Regex::new(r"[a-\.]", "").is_err());
        // A hyphen at the edge of a class escape is literal in both positions.
        assert!(is_match(r"^[\d-]+$", "", "-7"));
        assert!(is_match(r"^[a-\w]+$", "", "a-9"));
    }
}
