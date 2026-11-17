//! Rule-level and stylesheet-level parsing with error recovery.

use crate::ast::{
    FontFaceRule, ImportRule, Keyframe, KeyframesRule, MediaRule, Origin, Rule, StyleRule,
    Stylesheet,
};
use crate::declarations::parse_declarations;
use crate::tokenizer::{SpannedToken, Token, tokenize, tokenize_spanned};

impl Stylesheet {
    /// Parses a stylesheet. This never fails: invalid rules are skipped.
    #[must_use]
    pub fn parse(css: &str, origin: Origin) -> Self {
        Self {
            rules: parse_rules(css),
            origin,
        }
    }

    /// Returns the rules that apply for a given document media query list.
    ///
    /// Media types are matched loosely (`screen`, `print`, `all`); media
    /// features are not evaluated (they are treated as matching).
    #[must_use]
    pub fn declarations_of_matching_media(&self, media_query_list: &str) -> Vec<&Rule> {
        let mut applicable = Vec::new();
        collect_matching(&self.rules, media_query_list, &mut applicable);
        applicable
    }
}

fn collect_matching<'a>(rules: &'a [Rule], media: &str, out: &mut Vec<&'a Rule>) {
    for rule in rules {
        match rule {
            Rule::Media(nested) => {
                if media_query_matches(&nested.query, media) {
                    collect_matching(&nested.rules, media, out);
                }
            }
            other => out.push(other),
        }
    }
}

fn media_query_matches(query: &str, target: &str) -> bool {
    let query = query.trim();
    let target = target.trim();
    if query.is_empty() || target.is_empty() {
        return true;
    }
    let targets = media_types(target);
    query.split(',').any(|part| {
        let types = media_types(part);
        if types.is_empty() {
            // Only media features: assume they match.
            return true;
        }
        types
            .iter()
            .any(|kind| kind == "all" || targets.iter().any(|target| target == kind))
    })
}

fn media_types(list: &str) -> Vec<String> {
    list.split_whitespace()
        .map(|word| word.trim().to_ascii_lowercase())
        .filter(|word| !matches!(word.as_str(), "only" | "not" | "and" | "or"))
        .filter(|word| word.chars().all(|c| c.is_ascii_alphabetic() || c == '-'))
        .collect()
}

fn parse_rules(css: &str) -> Vec<Rule> {
    Parser::new(css).parse_rule_list()
}

struct Parser<'a> {
    css: &'a str,
    tokens: Vec<SpannedToken>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(css: &'a str) -> Self {
        Self {
            css,
            tokens: tokenize_spanned(css),
            pos: 0,
        }
    }

    fn skip_noise(&mut self) {
        while matches!(
            self.tokens.get(self.pos).map(|spanned| &spanned.token),
            Some(Token::Whitespace | Token::Semicolon | Token::Cdo | Token::Cdc)
        ) {
            self.pos += 1;
        }
    }

    fn byte_offset(&self, index: usize) -> usize {
        self.tokens
            .get(index)
            .map_or(self.css.len(), |spanned| spanned.start)
    }

    fn slice(&self, start: usize, end: usize) -> &'a str {
        if end <= start {
            return "";
        }
        let first = self.byte_offset(start);
        let last = self
            .tokens
            .get(end - 1)
            .map_or(self.css.len(), |spanned| spanned.end);
        if first <= last && last <= self.css.len() {
            &self.css[first..last]
        } else {
            ""
        }
    }

    fn parse_rule_list(&mut self) -> Vec<Rule> {
        let mut rules = Vec::new();
        loop {
            self.skip_noise();
            let Some(spanned) = self.tokens.get(self.pos) else {
                break;
            };
            match &spanned.token {
                Token::RightBrace => {
                    self.pos += 1;
                    break;
                }
                Token::AtKeyword(name) => {
                    let name = name.clone();
                    self.pos += 1;
                    if let Some(rule) = self.parse_at_rule(&name) {
                        rules.push(rule);
                    }
                }
                _ => {
                    if let Some(rule) = self.parse_qualified_rule() {
                        rules.push(rule);
                    }
                }
            }
        }
        rules
    }

    fn parse_qualified_rule(&mut self) -> Option<Rule> {
        let prelude_start = self.pos;
        let mut depth = 0i32;
        let mut block_at = None;
        while let Some(spanned) = self.tokens.get(self.pos) {
            match &spanned.token {
                Token::LeftParen | Token::LeftBracket => depth += 1,
                Token::RightParen | Token::RightBracket => depth = (depth - 1).max(0),
                Token::LeftBrace if depth == 0 => {
                    block_at = Some(self.pos);
                    break;
                }
                Token::Semicolon if depth == 0 => break,
                _ => {}
            }
            self.pos += 1;
        }
        let prelude_end = self.pos;
        let Some(open) = block_at else {
            if self.pos < self.tokens.len() {
                self.pos += 1;
            }
            return None;
        };

        self.pos = open + 1;
        let body = self.consume_block();
        let selectors = split_top_level_commas(self.slice(prelude_start, prelude_end));
        if selectors.is_empty() {
            return None;
        }
        Some(Rule::Style(StyleRule {
            selectors,
            declarations: parse_declarations(body),
        }))
    }

    fn parse_at_rule(&mut self, name: &str) -> Option<Rule> {
        let prelude_start = self.pos;
        let mut depth = 0i32;
        let mut block_at = None;
        while let Some(spanned) = self.tokens.get(self.pos) {
            match &spanned.token {
                Token::LeftParen | Token::LeftBracket => depth += 1,
                Token::RightParen | Token::RightBracket => depth = (depth - 1).max(0),
                Token::LeftBrace if depth == 0 => {
                    block_at = Some(self.pos);
                    break;
                }
                Token::Semicolon if depth == 0 => break,
                _ => {}
            }
            self.pos += 1;
        }
        let prelude_end = self.pos;
        let body = if let Some(open) = block_at {
            self.pos = open + 1;
            Some(self.consume_block())
        } else {
            if self.pos < self.tokens.len() {
                self.pos += 1;
            }
            None
        };
        let prelude = self.slice(prelude_start, prelude_end);

        match name.to_ascii_lowercase().as_str() {
            "media" => Some(Rule::Media(MediaRule {
                query: prelude.trim().to_string(),
                rules: body.map_or_else(Vec::new, parse_rules),
            })),
            "import" => parse_import(prelude).map(Rule::Import),
            "font-face" => body.map(|body| {
                Rule::FontFace(FontFaceRule {
                    declarations: parse_declarations(body),
                })
            }),
            "keyframes" | "-webkit-keyframes" | "-moz-keyframes" | "-o-keyframes" => {
                body.map(|body| Rule::Keyframes(parse_keyframes(prelude, body)))
            }
            _ => None,
        }
    }

    /// Consumes a `{ ... }` block and returns the text between the braces.
    fn consume_block(&mut self) -> &'a str {
        let start = self.byte_offset(self.pos);
        let mut depth = 0i32;
        while let Some(spanned) = self.tokens.get(self.pos) {
            match &spanned.token {
                Token::LeftBrace => depth += 1,
                Token::RightBrace => {
                    if depth == 0 {
                        let end = spanned.start;
                        self.pos += 1;
                        return &self.css[start..end];
                    }
                    depth -= 1;
                }
                _ => {}
            }
            self.pos += 1;
        }
        &self.css[start..]
    }
}

fn parse_import(prelude: &str) -> Option<ImportRule> {
    let tokens = tokenize_spanned(prelude);
    let first = tokens
        .iter()
        .find(|spanned| !matches!(spanned.token, Token::Whitespace))?;
    let (url, after) = match &first.token {
        Token::Url(url) | Token::String(url) => (url.clone(), first.end),
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            let string = tokens
                .iter()
                .skip_while(|spanned| spanned.start < first.end)
                .find(|spanned| !matches!(spanned.token, Token::Whitespace))?;
            match &string.token {
                Token::String(url) => (url.clone(), string.end),
                _ => return None,
            }
        }
        _ => return None,
    };
    let media = prelude[after.min(prelude.len())..].trim();
    Some(ImportRule {
        url,
        media: if media.is_empty() {
            None
        } else {
            Some(media.to_string())
        },
    })
}

fn parse_keyframes(prelude: &str, body: &str) -> KeyframesRule {
    let name = keyframes_name(prelude);
    let tokens = tokenize_spanned(body);
    let mut keyframes = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        while matches!(
            tokens.get(index).map(|spanned| &spanned.token),
            Some(Token::Whitespace | Token::Semicolon | Token::Cdo | Token::Cdc)
        ) {
            index += 1;
        }
        let start = index;
        let mut block_at = None;
        while index < tokens.len() {
            match &tokens[index].token {
                Token::LeftBrace => {
                    block_at = Some(index);
                    break;
                }
                Token::Semicolon => break,
                _ => index += 1,
            }
        }
        let Some(open) = block_at else {
            if index < tokens.len() {
                index += 1;
            }
            continue;
        };

        let declarations_start = tokens[open].end;
        index = open + 1;
        let mut depth = 0i32;
        let mut declarations_end = body.len();
        while index < tokens.len() {
            match &tokens[index].token {
                Token::LeftBrace => depth += 1,
                Token::RightBrace => {
                    if depth == 0 {
                        declarations_end = tokens[index].start;
                        index += 1;
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
            index += 1;
        }

        let selector_text = if start < open {
            &body[tokens[start].start..tokens[open - 1].end]
        } else {
            ""
        };
        let offsets: Vec<f32> = split_top_level_commas(selector_text)
            .iter()
            .filter_map(|selector| keyframe_offset(selector))
            .collect();
        if offsets.is_empty() {
            continue;
        }
        let declarations =
            parse_declarations(&body[declarations_start.min(body.len())..declarations_end]);
        for offset in offsets {
            keyframes.push(Keyframe {
                offset,
                declarations: declarations.clone(),
            });
        }
    }

    KeyframesRule { name, keyframes }
}

fn keyframes_name(prelude: &str) -> String {
    tokenize(prelude)
        .into_iter()
        .find_map(|token| match token {
            Token::Ident(name) | Token::String(name) => Some(name),
            _ => None,
        })
        .unwrap_or_else(|| prelude.trim().to_string())
}

fn keyframe_offset(selector: &str) -> Option<f32> {
    let selector = selector.trim().to_ascii_lowercase();
    match selector.as_str() {
        "from" => return Some(0.0),
        "to" => return Some(1.0),
        _ => {}
    }
    let number = selector.strip_suffix('%')?.trim().parse::<f32>().ok()?;
    Some((number / 100.0).clamp(0.0, 1.0))
}

/// Splits text on commas that are not nested inside blocks, strings or
/// escapes. Empty parts are dropped.
fn split_top_level_commas(text: &str) -> Vec<String> {
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
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            ',' if depth == 0 => {
                parts.push(text[start..index].trim().to_string());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim().to_string());
    parts.retain(|part| !part.is_empty());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::Length;

    fn style_rules(sheet: &Stylesheet) -> Vec<&StyleRule> {
        sheet
            .rules
            .iter()
            .filter_map(|rule| match rule {
                Rule::Style(style) => Some(style),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_stylesheet_has_no_rules() {
        let sheet = Stylesheet::parse("", Origin::Author);
        assert!(sheet.rules.is_empty());
        assert_eq!(sheet.origin, Origin::Author);
        assert!(
            Stylesheet::parse("   /* nothing */  ", Origin::UserAgent)
                .rules
                .is_empty()
        );
    }

    #[test]
    fn parses_a_simple_rule() {
        let sheet = Stylesheet::parse("p { color: red; }", Origin::Author);
        let rules = style_rules(&sheet);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].selectors, ["p"]);
        assert_eq!(rules[0].declarations.len(), 1);
        assert_eq!(rules[0].declarations[0].value.ident(), Some("red"));
    }

    #[test]
    fn splits_selector_lists_on_top_level_commas() {
        let sheet = Stylesheet::parse(
            " h1 , .a:not(.b, .c), [title=\"a,b\"] { color: blue } ",
            Origin::Author,
        );
        let rules = style_rules(&sheet);
        assert_eq!(
            rules[0].selectors,
            ["h1", ".a:not(.b, .c)", "[title=\"a,b\"]"]
        );
    }

    #[test]
    fn keeps_selector_text_raw() {
        let sheet = Stylesheet::parse("div.a > p + span ~ a:hover { }", Origin::Author);
        assert_eq!(
            style_rules(&sheet)[0].selectors,
            ["div.a > p + span ~ a:hover"]
        );
    }

    #[test]
    fn parses_multiple_rules_and_the_origin_is_kept() {
        let sheet = Stylesheet::parse("a { color: red } b { color: blue }", Origin::User);
        assert_eq!(sheet.origin, Origin::User);
        assert_eq!(style_rules(&sheet).len(), 2);
    }

    #[test]
    fn declarations_survive_invalid_entries() {
        let sheet = Stylesheet::parse("p { color: red; broken; width: 2px }", Origin::Author);
        let declarations = &style_rules(&sheet)[0].declarations;
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[1].value.as_length(), Some(Length::Px(2.0)));
    }

    #[test]
    fn parses_important_flag_inside_rules() {
        let sheet = Stylesheet::parse("p { color: red !important; }", Origin::Author);
        assert!(style_rules(&sheet)[0].declarations[0].important);
    }

    #[test]
    fn skips_rules_without_blocks() {
        let sheet = Stylesheet::parse("junk; p { color: red } more junk", Origin::Author);
        assert_eq!(style_rules(&sheet).len(), 1);
        assert_eq!(style_rules(&sheet)[0].selectors, ["p"]);
    }

    #[test]
    fn skips_rules_with_empty_selector() {
        let sheet = Stylesheet::parse("{ color: red } p { color: blue }", Origin::Author);
        assert_eq!(style_rules(&sheet).len(), 1);
    }

    #[test]
    fn parses_media_rules_with_nesting() {
        let sheet = Stylesheet::parse(
            "@media screen and (max-width: 600px) { p { color: red } } q { color: blue }",
            Origin::Author,
        );
        let media = sheet.rules.iter().find_map(|rule| match rule {
            Rule::Media(media) => Some(media),
            _ => None,
        });
        let media = media.expect("media rule");
        assert_eq!(media.query, "screen and (max-width: 600px)");
        assert_eq!(media.rules.len(), 1);
        assert!(matches!(media.rules[0], Rule::Style(_)));
        assert_eq!(style_rules(&sheet).len(), 1);
    }

    #[test]
    fn parses_nested_media_rules() {
        let sheet = Stylesheet::parse(
            "@media screen { @media (min-width: 1px) { p { color: red } } }",
            Origin::Author,
        );
        let Rule::Media(outer) = &sheet.rules[0] else {
            panic!("expected media");
        };
        let Rule::Media(inner) = &outer.rules[0] else {
            panic!("expected nested media");
        };
        assert_eq!(inner.query, "(min-width: 1px)");
        assert_eq!(inner.rules.len(), 1);
    }

    #[test]
    fn parses_empty_media_block() {
        let sheet = Stylesheet::parse("@media print {}", Origin::Author);
        let Rule::Media(media) = &sheet.rules[0] else {
            panic!("expected media");
        };
        assert!(media.rules.is_empty());
    }

    #[test]
    fn parses_import_forms() {
        let sheet = Stylesheet::parse(
            "@import \"a.css\";\n@import url(b.css) screen;\n@import url(\"c.css\") print and (color);",
            Origin::Author,
        );
        let imports: Vec<&ImportRule> = sheet
            .rules
            .iter()
            .filter_map(|rule| match rule {
                Rule::Import(import) => Some(import),
                _ => None,
            })
            .collect();
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].url, "a.css");
        assert_eq!(imports[0].media, None);
        assert_eq!(imports[1].url, "b.css");
        assert_eq!(imports[1].media.as_deref(), Some("screen"));
        assert_eq!(imports[2].url, "c.css");
        assert_eq!(imports[2].media.as_deref(), Some("print and (color)"));
    }

    #[test]
    fn ignores_invalid_import() {
        let sheet = Stylesheet::parse("@import foo bar;", Origin::Author);
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn parses_keyframes_offsets() {
        let sheet = Stylesheet::parse(
            "@keyframes spin { from { transform: none } 50% { opacity: 0.5 } to { opacity: 1 } }",
            Origin::Author,
        );
        let Rule::Keyframes(frames) = &sheet.rules[0] else {
            panic!("expected keyframes");
        };
        assert_eq!(frames.name, "spin");
        assert_eq!(frames.keyframes.len(), 3);
        assert_eq!(frames.keyframes[0].offset, 0.0);
        assert_eq!(frames.keyframes[1].offset, 0.5);
        assert_eq!(frames.keyframes[2].offset, 1.0);
        assert_eq!(frames.keyframes[1].declarations[0].name, "opacity");
    }

    #[test]
    fn keyframe_selector_lists_repeat_declarations() {
        let sheet = Stylesheet::parse("@keyframes k { 0%, 100% { color: red } }", Origin::Author);
        let Rule::Keyframes(frames) = &sheet.rules[0] else {
            panic!("expected keyframes");
        };
        assert_eq!(frames.keyframes.len(), 2);
        assert_eq!(frames.keyframes[0].offset, 0.0);
        assert_eq!(frames.keyframes[1].offset, 1.0);
        assert_eq!(
            frames.keyframes[0].declarations,
            frames.keyframes[1].declarations
        );
    }

    #[test]
    fn parses_font_face() {
        let sheet = Stylesheet::parse(
            "@font-face { font-family: \"My Font\"; src: url(x.woff2) }",
            Origin::Author,
        );
        let Rule::FontFace(font) = &sheet.rules[0] else {
            panic!("expected font-face");
        };
        assert_eq!(font.declarations.len(), 2);
        assert_eq!(font.declarations[0].name, "font-family");
        assert_eq!(font.declarations[0].value.as_string(), Some("My Font"));
    }

    #[test]
    fn skips_unknown_at_rules_with_blocks() {
        let sheet = Stylesheet::parse(
            "@supports (display: grid) { p { color: red } } q { color: blue }",
            Origin::Author,
        );
        assert_eq!(style_rules(&sheet).len(), 1);
        assert_eq!(style_rules(&sheet)[0].selectors, ["q"]);
    }

    #[test]
    fn skips_unknown_at_rules_without_blocks() {
        let sheet = Stylesheet::parse("@charset \"utf-8\"; p { color: red }", Origin::Author);
        assert_eq!(style_rules(&sheet).len(), 1);
    }

    #[test]
    fn recovers_from_unclosed_blocks() {
        let sheet = Stylesheet::parse("p { color: red; q { color: blue }", Origin::Author);
        assert_eq!(sheet.rules.len(), 1);
        let rules = style_rules(&sheet);
        assert_eq!(rules[0].declarations.len(), 1);
        assert_eq!(rules[0].declarations[0].value.ident(), Some("red"));
    }

    #[test]
    fn nested_braces_inside_declarations_are_matched() {
        let sheet = Stylesheet::parse("p { --data: {a: b}; color: red }", Origin::Author);
        let rules = style_rules(&sheet);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].declarations.len(), 2);
    }

    #[test]
    fn comments_are_ignored_between_rules() {
        let sheet = Stylesheet::parse("/* head */ p { color: red } /* tail */", Origin::Author);
        assert_eq!(style_rules(&sheet).len(), 1);
    }

    #[test]
    fn media_helper_returns_applicable_rules() {
        let sheet = Stylesheet::parse(
            "p { color: red } @media screen { q { color: green } } @media print { r { color: blue } }",
            Origin::Author,
        );
        let rules = sheet.declarations_of_matching_media("screen");
        let selectors: Vec<&str> = rules
            .iter()
            .filter_map(|rule| match rule {
                Rule::Style(style) => Some(style.selectors[0].as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(selectors, ["p", "q"]);
        let all: Vec<&Rule> = sheet.declarations_of_matching_media("print");
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn media_queries_with_only_features_match() {
        let sheet = Stylesheet::parse(
            "@media (min-width: 600px) { p { color: red } }",
            Origin::Author,
        );
        assert_eq!(sheet.declarations_of_matching_media("screen").len(), 1);
        assert_eq!(sheet.declarations_of_matching_media("").len(), 1);
    }

    #[test]
    fn malformed_input_never_panics() {
        for css in [
            "@",
            "@media",
            "@media {",
            "}",
            "{{{{",
            "p {",
            "p { color",
            "p { color:",
            "p { : }",
            "@import",
            "@keyframes",
            "@font-face",
            "&&&",
            "p,,q{}",
            "\u{0}",
            "p { color: red } @media screen {",
        ] {
            let sheet = Stylesheet::parse(css, Origin::Author);
            let _ = sheet.declarations_of_matching_media("screen");
        }
    }
}
