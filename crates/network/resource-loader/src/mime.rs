//! MIME type detection by extension and content sniffing.
//!
//! The public entry point is [`sniff`], which recognises the binary and text
//! formats a web page commonly serves. The narrower helpers ([`sniff_image`],
//! [`sniff_font`], [`looks_like_html`], …) are kept since callers use them
//! directly, and [`sniff`] composes them in priority order.

const SNIFF_WINDOW: usize = 1024;
const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];

/// Guesses a MIME type from a path or URL.
///
/// Query strings and fragments are ignored, so `style.css?v=2` still maps to
/// `text/css`.
pub fn from_path(path: &str) -> &'static str {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    let extension = lower.rsplit('.').next().unwrap_or("");
    match extension {
        "html" | "htm" | "xhtml" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" | "svgz" => "image/svg+xml",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Sniffs a MIME type from leading bytes, falling back to the path extension.
pub fn detect(bytes: &[u8], path: &str) -> &'static str {
    sniff(bytes).unwrap_or_else(|| from_path(path))
}

/// Sniffs the MIME type of a response from its leading bytes.
///
/// Binary signatures are checked first, then text formats in the order SVG,
/// HTML, JSON, CSS and JavaScript. Returns `None` when nothing is recognised
/// so the caller can fall back to the `Content-Type` header or the URL path.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if let Some(mime) = sniff_image(bytes) {
        return Some(mime);
    }
    if let Some(mime) = sniff_font(bytes) {
        return Some(mime);
    }
    if bytes.starts_with(b"\0asm") {
        return Some("application/wasm");
    }
    if bytes.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if strip_bom(bytes).is_empty() {
        return None;
    }

    if let Some(mime) = sniff_svg(bytes) {
        return Some(mime);
    }
    if looks_like_html(bytes) {
        return Some("text/html");
    }
    if let Some(mime) = sniff_json(bytes) {
        return Some(mime);
    }
    if let Some(mime) = sniff_css(bytes) {
        return Some(mime);
    }
    sniff_javascript(bytes)
}

/// Sniffs an image format from its leading bytes.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some("image/x-icon");
    }
    if bytes.len() > 12 && &bytes[4..8] == b"ftyp" && bytes[8..12].starts_with(b"avif") {
        return Some("image/avif");
    }
    None
}

/// Sniffs a web font from its leading bytes (WOFF, WOFF2, TTF and OTF).
pub fn sniff_font(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"wOFF") {
        return Some("font/woff");
    }
    if bytes.starts_with(b"wOF2") {
        return Some("font/woff2");
    }
    if bytes.starts_with(b"OTTO") {
        return Some("font/otf");
    }
    if bytes.starts_with(&[0x00, 0x01, 0x00, 0x00]) || bytes.starts_with(b"true") {
        return Some("font/ttf");
    }
    None
}

/// True when a byte slice starts with an SVG root element, optionally after an
/// XML declaration, comments or a doctype.
pub fn sniff_svg(bytes: &[u8]) -> Option<&'static str> {
    let head = text_head(bytes);
    let text = skip_markup_prefix(&head);
    if starts_with_tag(text, "svg") {
        Some("image/svg+xml")
    } else {
        None
    }
}

/// Sniffs whether content looks like HTML.
///
/// Leading whitespace, comments, processing instructions and a doctype are
/// skipped, then a known HTML start tag must follow. This avoids classifying
/// arbitrary XML (or a lone `<`) as HTML.
pub fn looks_like_html(bytes: &[u8]) -> bool {
    let text = text_head(bytes);
    if text.starts_with("<!doctype html") {
        return true;
    }
    const HTML_TAGS: &[&str] = &[
        "html",
        "head",
        "body",
        "title",
        "meta",
        "link",
        "base",
        "style",
        "script",
        "noscript",
        "div",
        "span",
        "p",
        "a",
        "img",
        "br",
        "hr",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "ul",
        "ol",
        "li",
        "table",
        "thead",
        "tbody",
        "tr",
        "td",
        "th",
        "form",
        "input",
        "button",
        "select",
        "textarea",
        "label",
        "main",
        "section",
        "article",
        "nav",
        "header",
        "footer",
        "aside",
        "iframe",
        "canvas",
        "video",
        "audio",
        "picture",
        "figure",
        "blockquote",
        "pre",
        "code",
        "strong",
        "em",
        "b",
        "i",
        "small",
        "template",
    ];
    let text = skip_markup_prefix(&text);
    HTML_TAGS.iter().any(|tag| starts_with_tag(text, tag))
}

/// True when the slice starts with a valid JSON value.
pub fn sniff_json(bytes: &[u8]) -> Option<&'static str> {
    let bytes = strip_bom(bytes);
    let mut parser = JsonParser {
        bytes,
        pos: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    if !matches!(parser.peek(), Some(b'{' | b'[')) {
        return None;
    }
    parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos == bytes.len() {
        Some("application/json")
    } else {
        None
    }
}

/// True when content looks like a stylesheet.
pub fn sniff_css(bytes: &[u8]) -> Option<&'static str> {
    let text = text_head(bytes);
    if text.is_empty() {
        return None;
    }
    // An at-rule at the start is unambiguous CSS.
    if text.starts_with('@') {
        return Some("text/css");
    }
    let open = text.find('{')?;
    let selector = text[..open].trim();
    // JS objects and assignments are not selectors.
    if selector.is_empty()
        || selector.len() > 512
        || selector.contains('=')
        || selector.starts_with("return ")
        || selector.starts_with("function")
    {
        return None;
    }
    let body = &text[open + 1..];
    let close = body.find('}')?;
    let declarations = &body[..close];
    let has_declaration = declarations.split(';').any(|decl| {
        let Some((name, value)) = decl.trim().split_once(':') else {
            return false;
        };
        let name = name.trim();
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            && !value.trim().is_empty()
    });
    if has_declaration {
        Some("text/css")
    } else {
        None
    }
}

/// True when content looks like a JavaScript module or script.
pub fn sniff_javascript(bytes: &[u8]) -> Option<&'static str> {
    let text = text_head(bytes);
    if text.is_empty() {
        return None;
    }
    const STRONG: &[&str] = &[
        "=>",
        "function",
        "use strict",
        "import",
        "export",
        "document",
        "window",
        "console",
        "module.exports",
        "require",
        "async function",
    ];
    const WEAK: &[&str] = &[
        "var", "let", "const", "class", "return", "if", "for", "while", "try", "new", "typeof",
        "await", "delete", "this",
    ];
    let strong = STRONG.iter().any(|token| contains_js_token(&text, token));
    let weak = WEAK
        .iter()
        .map(|token| count_js_token(&text, token))
        .sum::<usize>();
    if strong || weak >= 2 {
        Some("text/javascript")
    } else {
        None
    }
}

/// True when the MIME type is textual.
pub fn is_textual(mime: &str) -> bool {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("text/")
        || essence.contains("json")
        || essence.contains("xml")
        || essence.contains("javascript")
        || essence == "image/svg+xml"
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes)
}

fn text_head(bytes: &[u8]) -> String {
    let bytes = strip_bom(bytes);
    String::from_utf8_lossy(&bytes[..bytes.len().min(SNIFF_WINDOW)])
        .trim_start()
        .to_ascii_lowercase()
}

fn starts_with_tag(text: &str, tag: &str) -> bool {
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(tag) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_whitespace() || matches!(c, '>' | '/'))
}

fn skip_markup_prefix(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix("<!--") {
            if let Some(end) = rest.find("-->") {
                text = &rest[end + 3..];
                continue;
            }
            return text;
        }
        if text.starts_with("<!doctype") {
            if let Some(end) = text.find('>') {
                text = &text[end + 1..];
                continue;
            }
            return text;
        }
        if let Some(rest) = text.strip_prefix("<?") {
            if let Some(end) = rest.find("?>") {
                text = &rest[end + 2..];
                continue;
            }
            return text;
        }
        return text;
    }
}

fn contains_js_token(text: &str, token: &str) -> bool {
    count_js_token(text, token) > 0
}

fn count_js_token(text: &str, token: &str) -> usize {
    let mut count = 0;
    let mut offset = 0;
    while let Some(index) = text[offset..].find(token) {
        let start = offset + index;
        let end = start + token.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let is_ident = |c: Option<char>| {
            c.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
        };
        if !is_ident(before) && !is_ident(after) {
            count += 1;
        }
        offset = end;
    }
    count
}

/// A small, allocation-free JSON validator used for content sniffing.
struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl JsonParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.pos += 1;
        }
    }

    fn enter(&mut self) -> Option<()> {
        if self.depth >= 64 {
            return None;
        }
        self.depth += 1;
        Some(())
    }

    fn parse_value(&mut self) -> Option<()> {
        self.skip_whitespace();
        match self.peek()? {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string(),
            b't' => self.parse_literal(b"true"),
            b'f' => self.parse_literal(b"false"),
            b'n' => self.parse_literal(b"null"),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_object(&mut self) -> Option<()> {
        self.enter()?;
        self.pos += 1; // `{`
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Some(());
        }
        loop {
            self.skip_whitespace();
            self.parse_string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return None;
            }
            self.pos += 1;
            self.parse_value()?;
            self.skip_whitespace();
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    break;
                }
                _ => return None,
            }
        }
        self.depth -= 1;
        Some(())
    }

    fn parse_array(&mut self) -> Option<()> {
        self.enter()?;
        self.pos += 1; // `[`
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Some(());
        }
        loop {
            self.parse_value()?;
            self.skip_whitespace();
            match self.peek()? {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    break;
                }
                _ => return None,
            }
        }
        self.depth -= 1;
        Some(())
    }

    fn parse_string(&mut self) -> Option<()> {
        if self.peek() != Some(b'"') {
            return None;
        }
        self.pos += 1;
        while let Some(byte) = self.peek() {
            self.pos += 1;
            match byte {
                b'"' => return Some(()),
                b'\\' => match self.peek()? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => self.pos += 1,
                    b'u' => {
                        self.pos += 1;
                        for _ in 0..4 {
                            if !self.peek()?.is_ascii_hexdigit() {
                                return None;
                            }
                            self.pos += 1;
                        }
                    }
                    _ => return None,
                },
                0..=0x1f => return None,
                _ => {}
            }
        }
        None
    }

    fn parse_literal(&mut self, literal: &[u8]) -> Option<()> {
        if self.bytes.get(self.pos..self.pos + literal.len()) == Some(literal) {
            self.pos += literal.len();
            Some(())
        } else {
            None
        }
    }

    fn parse_number(&mut self) -> Option<()> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek()? {
            b'0' => self.pos += 1,
            b'1'..=b'9' => self.consume_digits(),
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.consume_digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !self.peek()?.is_ascii_digit() {
                return None;
            }
            self.consume_digits();
        }
        Some(())
    }

    fn consume_digits(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_types() {
        assert_eq!(from_path("a/b/page.HTML"), "text/html");
        assert_eq!(from_path("/x/style.css"), "text/css");
        assert_eq!(from_path("image.png"), "image/png");
        assert_eq!(from_path("unknown.bin"), "application/octet-stream");
    }

    #[test]
    fn extension_lookup_ignores_query_and_fragment() {
        assert_eq!(from_path("/a/app.js?v=2#x"), "text/javascript");
        assert_eq!(from_path("/f/font.WOFF2"), "font/woff2");
        assert_eq!(from_path("/data.JSON"), "application/json");
        assert_eq!(from_path("/pic.webp?size=1"), "image/webp");
    }

    #[test]
    fn sniffs_images() {
        assert_eq!(
            sniff_image(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]),
            Some("image/png")
        );
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a........"), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF????WEBPVP8"), Some("image/webp"));
        assert_eq!(sniff_image(b"hello"), None);
    }

    #[test]
    fn sniffs_fonts_and_binary_formats() {
        assert_eq!(sniff_font(b"wOF2data"), Some("font/woff2"));
        assert_eq!(sniff_font(b"wOFFdata"), Some("font/woff"));
        assert_eq!(sniff_font(b"OTTOdata"), Some("font/otf"));
        assert_eq!(sniff_font(&[0, 1, 0, 0]), Some("font/ttf"));
        assert_eq!(sniff(b"\0asm\x01\0\0\0"), Some("application/wasm"));
        assert_eq!(sniff(b"%PDF-1.7"), Some("application/pdf"));
    }

    #[test]
    fn detects_html_and_text() {
        assert!(looks_like_html(b"<!DOCTYPE html><html>"));
        assert!(looks_like_html(b"<p>hi"));
        assert!(looks_like_html(b"<!-- c --><div>x</div>"));
        assert!(looks_like_html(b"\xef\xbb\xbf<html lang=en>"));
        assert!(!looks_like_html(b"\x89PNG"));
        assert!(!looks_like_html(b"<feed><title>x</title></feed>"));
        assert!(!looks_like_html(b"<not-an-html-tag>"));
        assert!(is_textual("text/html"));
        assert!(is_textual("text/css; charset=utf-8"));
        assert!(is_textual("image/svg+xml"));
        assert!(is_textual("application/json"));
        assert!(!is_textual("image/png"));
    }

    #[test]
    fn sniffs_svg() {
        assert_eq!(
            sniff_svg(b"<svg viewBox='0 0 1 1'><rect/></svg>"),
            Some("image/svg+xml")
        );
        assert_eq!(
            sniff_svg(b"<?xml version='1.0'?><svg xmlns='http://www.w3.org/2000/svg'/>"),
            Some("image/svg+xml")
        );
        assert_eq!(sniff_svg(b"<feed><title>x</title></feed>"), None);
        assert_eq!(sniff(b"<!-- x --><svg></svg>"), Some("image/svg+xml"));
    }

    #[test]
    fn sniffs_json() {
        for input in [
            r#"{"name":"value","n":1.5e3,"ok":true,"none":null}"#,
            r#"[1, 2, {"a":["x","y"]}]"#,
            "  \n{\"escaped\":\"a\\\"b\\u0041\"}  ",
        ] {
            assert_eq!(
                sniff_json(input.as_bytes()),
                Some("application/json"),
                "{input}"
            );
        }
        assert_eq!(sniff_json(b"{\"a\":}"), None);
        assert_eq!(sniff_json(b"[1,]"), None);
        assert_eq!(sniff_json(b"not json"), None);
        assert_eq!(sniff(b"\xef\xbb\xbf{\"a\":1}"), Some("application/json"));
    }

    #[test]
    fn sniffs_css() {
        assert_eq!(
            sniff_css(b"body { color: red; margin: 0 }"),
            Some("text/css")
        );
        assert_eq!(
            sniff_css(b"@media screen { a { color: blue } }"),
            Some("text/css")
        );
        assert_eq!(
            sniff_css(b".x::after { content: \"a:b\" }"),
            Some("text/css")
        );
        assert_eq!(sniff_css(b"var x = {a: 1};"), None);
        assert_eq!(sniff_css(b"hello world"), None);
        assert_eq!(sniff(b"h1 { font-size: 2rem }"), Some("text/css"));
    }

    #[test]
    fn sniffs_javascript() {
        assert_eq!(
            sniff_javascript(b"function hello() { return 1; }"),
            Some("text/javascript")
        );
        assert_eq!(
            sniff_javascript(b"const x = () => 42"),
            Some("text/javascript")
        );
        assert_eq!(sniff_javascript(b"'use strict';"), Some("text/javascript"));
        assert_eq!(
            sniff_javascript(b"document.getElementById('x')"),
            Some("text/javascript")
        );
        assert_eq!(
            sniff_javascript(b"let a = 1; let b = 2;"),
            Some("text/javascript")
        );
        assert_eq!(sniff_javascript(b"hello world"), None);
        assert_eq!(
            sniff(b"export default function () {}"),
            Some("text/javascript")
        );
    }

    #[test]
    fn sniff_prioritises_specific_formats() {
        assert_eq!(sniff(b"<html><body>hi</body></html>"), Some("text/html"));
        assert_eq!(sniff(b"<svg></svg>"), Some("image/svg+xml"));
        assert_eq!(sniff(b"{\"a\":1}"), Some("application/json"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn detect_combines_sniffing_and_path() {
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n", "/x/unknown"), "image/png");
        assert_eq!(detect(b"whatever", "/x/page.css"), "text/css");
    }
}
