//! Flattens CSS nesting into the flat rule list the parser understands.
//!
//! The stylesheet parser accepts only flat style rules; a nested rule such as
//! `.card { &:hover { color: red } }` would otherwise be treated as an invalid
//! declaration and dropped. This module rewrites the raw CSS *before* parsing:
//! it hoists nested rules to the top level, expands `&` against the parent
//! selector (or inserts a descendant combinator when `&` is absent), and lets
//! the normal selector parser compute specificity from the flattened text.
//!
//! When the input contains no nested style rules the original string is
//! returned unchanged, so the cascade for non-nested stylesheets is exactly
//! the same as before.

/// A parsed item inside a rule list.
#[derive(Debug, Clone)]
enum Item {
    /// Something with a `{ … }` block: a style rule or a block at-rule.
    Block { prelude: String, body: String },
    /// A declaration or statement, including its trailing `;` when present.
    Statement(String),
}

/// Scans a rule list into items, respecting strings, comments and brackets.
fn parse_items(text: &str) -> Vec<Item> {
    let bytes = text.as_bytes();
    let mut items = Vec::new();
    let mut pos = 0usize;

    while pos < bytes.len() {
        skip_trivia(text, &mut pos);
        if pos >= bytes.len() {
            break;
        }
        let start = pos;
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut quote = None::<u8>;
        let mut escaped = false;
        let mut consumed = false;

        while pos < bytes.len() {
            let ch = bytes[pos];
            if escaped {
                escaped = false;
                pos += 1;
                continue;
            }
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
                pos += 1;
                continue;
            }
            if ch == b'\\' {
                escaped = true;
                pos += 1;
                continue;
            }
            if ch == b'"' || ch == b'\'' {
                quote = Some(ch);
                pos += 1;
                continue;
            }
            if ch == b'/' && bytes.get(pos + 1) == Some(&b'*') {
                pos += 2;
                while pos + 1 < bytes.len() && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                    pos += 1;
                }
                pos = (pos + 2).min(bytes.len());
                continue;
            }
            match ch {
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'[' => bracket += 1,
                b']' => bracket -= 1,
                b'{' if paren == 0 && bracket == 0 => {
                    let prelude = text[start..pos].trim().to_string();
                    let close = matching_brace(bytes, pos + 1);
                    let body = text[pos + 1..close.min(bytes.len())].to_string();
                    pos = (close + 1).min(bytes.len());
                    items.push(Item::Block { prelude, body });
                    consumed = true;
                    break;
                }
                b';' if paren == 0 && bracket == 0 => {
                    let statement = text[start..=pos].trim().to_string();
                    pos += 1;
                    items.push(Item::Statement(statement));
                    consumed = true;
                    break;
                }
                _ => {}
            }
            pos += 1;
        }

        if !consumed && start < bytes.len() {
            let statement = text[start..].trim().to_string();
            if !statement.is_empty() {
                items.push(Item::Statement(statement));
            }
        }
    }
    items
}

fn skip_trivia(text: &str, pos: &mut usize) {
    let bytes = text.as_bytes();
    loop {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        if bytes.get(*pos) == Some(&b'/') && bytes.get(*pos + 1) == Some(&b'*') {
            *pos += 2;
            while *pos + 1 < bytes.len() && !(bytes[*pos] == b'*' && bytes[*pos + 1] == b'/') {
                *pos += 1;
            }
            *pos = (*pos + 2).min(bytes.len());
            continue;
        }
        break;
    }
}

/// Finds the byte index just past the brace matching an opening brace at
/// `start`, ignoring braces in strings and comments.
fn matching_brace(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0i32;
    let mut pos = start;
    let mut quote = None::<u8>;
    let mut escaped = false;
    while pos < bytes.len() {
        let ch = bytes[pos];
        if escaped {
            escaped = false;
            pos += 1;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            pos += 1;
            continue;
        }
        if ch == b'\\' {
            escaped = true;
            pos += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' {
            quote = Some(ch);
            pos += 1;
            continue;
        }
        if ch == b'/' && bytes.get(pos + 1) == Some(&b'*') {
            pos += 2;
            while pos + 1 < bytes.len() && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                pos += 1;
            }
            pos = (pos + 2).min(bytes.len());
            continue;
        }
        match ch {
            b'{' => depth += 1,
            b'}' if depth == 0 => return pos,
            b'}' => depth -= 1,
            _ => {}
        }
        pos += 1;
    }
    bytes.len()
}

/// Splits a selector prelude on top-level commas.
fn split_selectors(prelude: &str) -> Vec<String> {
    let bytes = prelude.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut quote = None::<u8>;
    let mut escaped = false;
    for (index, &ch) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            b'\\' => escaped = true,
            b'"' | b'\'' => quote = Some(ch),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                let selector = prelude[start..index].trim();
                if !selector.is_empty() {
                    out.push(selector.to_string());
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let selector = prelude[start..].trim();
    if !selector.is_empty() {
        out.push(selector.to_string());
    }
    out
}

/// Rewrites nested style rules, returning the flattened CSS.
#[must_use]
pub fn flatten(input: &str) -> String {
    let items = parse_items(input);
    let mut out = String::new();
    let mut nested = false;
    flatten_items(&items, &[], &mut out, &mut nested);
    if nested { out } else { input.to_string() }
}

fn flatten_items(items: &[Item], parents: &[String], out: &mut String, nested: &mut bool) {
    for item in items {
        match item {
            Item::Statement(text) => {
                out.push_str(text);
                out.push('\n');
            }
            Item::Block { prelude, body } => {
                if prelude.trim_start().starts_with('@') {
                    flatten_at_rule(prelude, body, parents, out, nested);
                } else {
                    flatten_style_rule(prelude, body, parents, out, nested);
                }
            }
        }
    }
}

fn flatten_at_rule(
    prelude: &str,
    body: &str,
    parents: &[String],
    out: &mut String,
    nested: &mut bool,
) {
    // Only at-rules that contain style rules need recursion. Descriptor-style
    // blocks (@font-face, @property, @keyframes) are emitted verbatim.
    let name = prelude
        .trim_start()
        .trim_start_matches('@')
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        name.as_str(),
        "media" | "supports" | "container" | "layer" | "scope" | "starting-style"
    ) {
        out.push_str(prelude.trim());
        out.push_str(" {\n");
        out.push_str(body);
        out.push_str("\n}\n");
        return;
    }

    let inner = parse_items(body);
    let mut flattened = String::new();
    flatten_items(&inner, parents, &mut flattened, nested);
    out.push_str(prelude.trim());
    out.push_str(" {\n");
    out.push_str(&flattened);
    out.push_str("}\n");
}

fn flatten_style_rule(
    prelude: &str,
    body: &str,
    parents: &[String],
    out: &mut String,
    nested: &mut bool,
) {
    let inner = parse_items(body);
    let (declarations, children): (Vec<_>, Vec<_>) = inner
        .into_iter()
        .partition(|item| matches!(item, Item::Statement(_)));
    let declarations: Vec<String> = declarations
        .into_iter()
        .map(|item| match item {
            Item::Statement(text) => text,
            _ => unreachable!(),
        })
        .collect();
    let children: Vec<Item> = children;

    if parents.is_empty() && children.is_empty() {
        // Flat rule: emit exactly the original shape.
        out.push_str(prelude.trim());
        out.push_str(" {\n");
        for declaration in declarations {
            out.push_str(&declaration);
            out.push('\n');
        }
        out.push_str("}\n");
        return;
    }

    if !children.is_empty() && !parents.is_empty() {
        *nested = true;
    }

    let selectors = split_selectors(prelude);
    let expanded = expand_selectors(&selectors, parents);

    for selector in expanded {
        out.push_str(&selector);
        out.push_str(" {\n");
        for declaration in &declarations {
            out.push_str(declaration);
            out.push('\n');
        }
        out.push_str("}\n");
        if !children.is_empty() {
            *nested = true;
            flatten_items(&children, std::slice::from_ref(&selector), out, nested);
        }
    }
}

fn expand_selectors(selectors: &[String], parents: &[String]) -> Vec<String> {
    if parents.is_empty() {
        return selectors.to_vec();
    }
    let mut out = Vec::new();
    for selector in selectors {
        if selector.contains('&') {
            for parent in parents {
                out.push(selector.replace('&', parent));
            }
        } else {
            for parent in parents {
                out.push(format!("{parent} {selector}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_flat_css_unchanged() {
        let css = "p { color: red; }\n@media screen { a { color: blue } }";
        assert_eq!(flatten(css), css);
    }

    #[test]
    fn flattens_ampersand_nesting() {
        let css = ".card { color: red; &:hover { color: blue; } }";
        let flat = flatten(css);
        assert!(flat.contains(".card:hover"), "{flat}");
        assert!(flat.contains("color: red"), "{flat}");
        assert!(flat.contains("color: blue"), "{flat}");
    }

    #[test]
    fn flattens_implicit_descendant_nesting() {
        let css = ".card { .title { color: red; } }";
        let flat = flatten(css);
        assert!(flat.contains(".card .title"), "{flat}");
    }

    #[test]
    fn flattens_multiple_levels_and_groups() {
        let css = ".a, .b { &:hover { .c { color: red; } } }";
        let flat = flatten(css);
        assert!(flat.contains(".a:hover .c"), "{flat}");
        assert!(flat.contains(".b:hover .c"), "{flat}");
    }

    #[test]
    fn hoists_nested_at_rules() {
        let css = ".a { @media (min-width: 10px) { &:hover { color: red; } } }";
        let flat = flatten(css);
        assert!(flat.contains("@media (min-width: 10px)"), "{flat}");
        assert!(flat.contains(".a:hover"), "{flat}");
    }
}
