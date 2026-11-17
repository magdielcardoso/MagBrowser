//! Media query evaluation for the `screen` medium.

/// Viewport and environment used to evaluate media queries.
#[derive(Debug, Clone)]
pub struct MediaContext {
    /// Viewport width in CSS pixels.
    pub width: f32,
    /// Viewport height in CSS pixels.
    pub height: f32,
    /// Media type, usually `screen`.
    pub media_type: String,
    /// Preferred colour scheme, `light` or `dark`.
    pub prefers_color_scheme: String,
}

impl Default for MediaContext {
    fn default() -> Self {
        Self {
            width: 1024.0,
            height: 768.0,
            media_type: "screen".to_string(),
            prefers_color_scheme: "light".to_string(),
        }
    }
}

/// Evaluates a media query list (comma separated, OR).
pub fn media_matches(query: &str, ctx: &MediaContext) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    split_top_level(query, ',')
        .into_iter()
        .any(|part| single_query_matches(part.trim(), ctx))
}

fn single_query_matches(query: &str, ctx: &MediaContext) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut negate = false;
    let mut rest = query.trim();
    if let Some(stripped) = rest.strip_prefix("not ") {
        negate = true;
        rest = stripped.trim();
    } else if let Some(stripped) = rest.strip_prefix("only ") {
        rest = stripped.trim();
    }

    let mut result = true;
    let mut saw_type = false;
    for token in split_conditions(rest) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if let Some(feature) = token.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            result &= feature_matches(feature, ctx);
        } else if token.eq_ignore_ascii_case("and") {
            continue;
        } else {
            saw_type = true;
            result &= match token.to_ascii_lowercase().as_str() {
                "all" => true,
                "screen" => ctx.media_type.eq_ignore_ascii_case("screen"),
                "print" => ctx.media_type.eq_ignore_ascii_case("print"),
                _ => false,
            };
        }
    }
    if !saw_type && !rest.contains('(') {
        result = false;
    }
    if negate { !result } else { result }
}

fn feature_matches(feature: &str, ctx: &MediaContext) -> bool {
    if let Some((name, value)) = feature.split_once(':') {
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        let wanted = parse_length(value);
        match name.as_str() {
            "min-width" => wanted.is_some_and(|v| ctx.width >= v),
            "max-width" => wanted.is_some_and(|v| ctx.width <= v),
            "width" => wanted.is_some_and(|v| (ctx.width - v).abs() < 0.5),
            "min-height" => wanted.is_some_and(|v| ctx.height >= v),
            "max-height" => wanted.is_some_and(|v| ctx.height <= v),
            "height" => wanted.is_some_and(|v| (ctx.height - v).abs() < 0.5),
            "orientation" => match value.to_ascii_lowercase().as_str() {
                "landscape" => ctx.width >= ctx.height,
                "portrait" => ctx.height > ctx.width,
                _ => false,
            },
            "prefers-color-scheme" => ctx.prefers_color_scheme.eq_ignore_ascii_case(value),
            "min-resolution" | "max-resolution" | "resolution" => true,
            _ => false,
        }
    } else {
        // Boolean form: `(color)`, `(hover)` etc. Unknown features are false.
        !matches!(feature.trim().to_ascii_lowercase().as_str(), "unknown")
            && feature.trim().is_empty()
    }
}

fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(px) = value.strip_suffix("px") {
        return px.trim().parse().ok();
    }
    if let Some(em) = value.strip_suffix("em") {
        return em.trim().parse::<f32>().ok().map(|v| v * 16.0);
    }
    if let Some(rem) = value.strip_suffix("rem") {
        return rem.trim().parse::<f32>().ok().map(|v| v * 16.0);
    }
    value.parse().ok()
}

fn split_top_level(input: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    for (index, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&input[start..index]);
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&input[start..]);
    out
}

fn split_conditions(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut depth = 0_i32;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 => {
                let word = &input[start..i];
                if !word.trim().is_empty() {
                    out.push(word);
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start <= input.len() {
        let tail = &input[start..];
        if !tail.trim().is_empty() {
            out.push(tail);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(width: f32, height: f32) -> MediaContext {
        MediaContext {
            width,
            height,
            ..MediaContext::default()
        }
    }

    #[test]
    fn empty_query_always_matches() {
        assert!(media_matches("", &ctx(800.0, 600.0)));
        assert!(media_matches("all", &ctx(800.0, 600.0)));
        assert!(media_matches("screen", &ctx(800.0, 600.0)));
        assert!(!media_matches("print", &ctx(800.0, 600.0)));
    }

    #[test]
    fn width_ranges() {
        let c = ctx(800.0, 600.0);
        assert!(media_matches("(min-width: 600px)", &c));
        assert!(!media_matches("(min-width: 900px)", &c));
        assert!(media_matches("(max-width: 800px)", &c));
        assert!(media_matches(
            "screen and (min-width: 600px) and (max-width: 1000px)",
            &c
        ));
        assert!(!media_matches("screen and (orientation: portrait)", &c));
        assert!(media_matches("screen and (orientation: landscape)", &c));
    }

    #[test]
    fn comma_separated_queries_or() {
        let c = ctx(800.0, 600.0);
        assert!(media_matches("print, screen and (min-width: 100px)", &c));
        assert!(!media_matches("print, (min-width: 2000px)", &c));
    }

    #[test]
    fn not_negates() {
        let c = ctx(800.0, 600.0);
        assert!(media_matches("not print", &c));
        assert!(!media_matches("not screen", &c));
    }
}
