//! Declaration-list parsing, shared by rule blocks, `style` attributes and
//! `@font-face` bodies.

use crate::Token;
use crate::ast::Declaration;
use crate::tokenizer::{to_component_values, tokenize};
use crate::values::{ComponentValue, Value};

/// Parses a semicolon-separated declaration list.
///
/// Invalid declarations are skipped individually; they never invalidate the
/// rest of the list.
#[must_use]
pub fn parse_declarations(input: &str) -> Vec<Declaration> {
    let mut result = Vec::new();
    let mut segment: Vec<Token> = Vec::new();
    let mut depth: i32 = 0;

    for token in tokenize(input) {
        match &token {
            Token::LeftParen | Token::LeftBracket | Token::LeftBrace => depth += 1,
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = (depth - 1).max(0);
            }
            Token::Semicolon if depth == 0 => {
                if let Some(declaration) = parse_one(&segment) {
                    result.push(declaration);
                }
                segment.clear();
                continue;
            }
            _ => {}
        }
        segment.push(token);
    }

    if let Some(declaration) = parse_one(&segment) {
        result.push(declaration);
    }
    result
}

/// Parses the contents of a `style` attribute.
#[must_use]
pub fn parse_inline_style(input: &str) -> Vec<Declaration> {
    parse_declarations(input)
}

fn parse_one(tokens: &[Token]) -> Option<Declaration> {
    let mut start = 0;
    while matches!(tokens.get(start), Some(Token::Whitespace)) {
        start += 1;
    }

    let name = match tokens.get(start)? {
        Token::Ident(name) => name.clone(),
        _ => return None,
    };

    let mut index = start + 1;
    while matches!(tokens.get(index), Some(Token::Whitespace)) {
        index += 1;
    }
    if !matches!(tokens.get(index), Some(Token::Colon)) {
        return None;
    }
    index += 1;

    let mut end = tokens.len();
    while end > index && matches!(tokens.get(end - 1), Some(Token::Whitespace)) {
        end -= 1;
    }

    let mut important = false;
    if let Some(Token::Ident(word)) = tokens.get(end.checked_sub(1)?)
        && word.eq_ignore_ascii_case("important")
    {
        let mut bang = end - 1;
        while bang > index && matches!(tokens.get(bang - 1), Some(Token::Whitespace)) {
            bang -= 1;
        }
        if matches!(tokens.get(bang.checked_sub(1)?), Some(Token::Delim('!'))) {
            important = true;
            end = bang - 1;
            while end > index && matches!(tokens.get(end - 1), Some(Token::Whitespace)) {
                end -= 1;
            }
        }
    }

    if end <= index {
        return None;
    }

    let mut components = to_component_values(&tokens[index..end]);
    while matches!(components.first(), Some(ComponentValue::Whitespace)) {
        components.remove(0);
    }
    while matches!(components.last(), Some(ComponentValue::Whitespace)) {
        components.pop();
    }
    if components.is_empty() {
        return None;
    }

    let name = if name.starts_with("--") {
        name
    } else {
        name.to_ascii_lowercase()
    };

    Some(Declaration {
        name,
        value: Value { components },
        important,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::Length;

    #[test]
    fn parses_multiple_declarations() {
        let declarations = parse_declarations("color: red; width: 10px;");
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0].name, "color");
        assert_eq!(declarations[1].name, "width");
        assert_eq!(declarations[1].value.as_length(), Some(Length::Px(10.0)));
    }

    #[test]
    fn missing_final_semicolon_is_fine() {
        let declarations = parse_declarations("color: red");
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].value.ident(), Some("red"));
    }

    #[test]
    fn detects_important_in_all_spellings() {
        for input in [
            "color: red !important",
            "color: red!important",
            "color: red ! important",
            "color: red !IMPORTANT",
        ] {
            let declarations = parse_declarations(input);
            assert_eq!(declarations.len(), 1, "{input}");
            assert!(declarations[0].important, "{input}");
        }
    }

    #[test]
    fn important_requires_bang() {
        let declarations = parse_declarations("color: red important");
        assert_eq!(declarations.len(), 1);
        assert!(!declarations[0].important);
    }

    #[test]
    fn skips_invalid_declarations_but_keeps_valid_ones() {
        let declarations =
            parse_declarations("nonsense; color: blue; : bad; width:; height: 2px; 4px: red");
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0].name, "color");
        assert_eq!(declarations[1].name, "height");
    }

    #[test]
    fn lowercases_names_except_custom_properties() {
        let declarations = parse_declarations("COLOR: red; --My-Var: 1px;");
        assert_eq!(declarations[0].name, "color");
        assert_eq!(declarations[1].name, "--My-Var");
    }

    #[test]
    fn values_preserve_functions_and_shorthands() {
        let declarations = parse_declarations(
            "background: rgb(1, 2, 3) url(x.png) no-repeat; margin: 1px 2px 3px 4px",
        );
        assert_eq!(declarations[0].value.components.len(), 5);
        assert_eq!(declarations[1].value.split_commas().len(), 1);
        assert_eq!(declarations[1].value.as_ident_list(), Vec::<String>::new());
        assert_eq!(declarations[1].tokens().len(), 7);
    }

    #[test]
    fn comments_inside_values_are_dropped() {
        let declarations = parse_declarations("color: /* note */ red;");
        assert_eq!(declarations[0].value.ident(), Some("red"));
    }

    #[test]
    fn semicolons_inside_functions_do_not_split() {
        let declarations = parse_declarations("background: url(\"a;b.png\"); color: red");
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0].value.as_url(), Some("a;b.png"));
    }

    #[test]
    fn inline_style_matches_declaration_list() {
        let inline = parse_inline_style("color: blue; margin: 0");
        assert_eq!(inline.len(), 2);
        assert_eq!(inline[0].value.ident(), Some("blue"));
        assert_eq!(inline[1].value.as_length(), Some(Length::Px(0.0)));
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(parse_declarations("").is_empty());
        assert!(parse_declarations("   ;;  ").is_empty());
        assert!(parse_inline_style("/* only a comment */").is_empty());
    }
}
