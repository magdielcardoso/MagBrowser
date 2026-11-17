//! Lightweight preload scanning of HTML for subresources.
//!
//! This is intentionally not the HTML parser: it scans the raw markup for
//! `src`/`href` attributes on the tags that matter, so the network layer can start
//! fetching before the tree builder finishes.

use browser_url::Url;

use crate::mime;

/// The kind of subresource to preload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreloadKind {
    /// Stylesheet (`<link rel=stylesheet>`).
    Style,
    /// Script.
    Script,
    /// Image.
    Image,
    /// Font.
    Font,
    /// Other resource.
    Other,
}

/// A resource that should be fetched early.
#[derive(Debug, Clone, PartialEq)]
pub struct PreloadRequest {
    /// Absolute URL resolved against the document URL.
    pub url: Url,
    /// Resource kind.
    pub kind: PreloadKind,
}

/// Scans `html` for preloadable resources.
pub fn scan(html: &str, base: Option<&Url>) -> Vec<PreloadRequest> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else { break };
        let tag = &rest[1..tag_end];
        rest = &rest[tag_end + 1..];
        if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        let mut parts = tag.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let name = name.to_ascii_lowercase();
        let kind = match name.as_str() {
            "link" => PreloadKind::Style,
            "script" => PreloadKind::Script,
            "img" => PreloadKind::Image,
            "source" | "video" | "audio" => PreloadKind::Other,
            _ => continue,
        };
        let attrs = parse_attributes(tag);
        if name == "link" {
            let rel = attr(&attrs, "rel").unwrap_or_default().to_ascii_lowercase();
            if !rel.contains("stylesheet") && !rel.contains("preload") && !rel.contains("icon") {
                continue;
            }
        }
        let Some(href) = attr(&attrs, "src").or_else(|| attr(&attrs, "href")) else {
            continue;
        };
        if href.starts_with('#') || href.starts_with("data:") || href.starts_with("javascript:") {
            continue;
        }
        let url = match base {
            Some(base) => base.join(href),
            None => Url::parse(href),
        };
        if let Ok(url) = url {
            let kind = match kind {
                PreloadKind::Style => {
                    let _ = mime::from_path(&url.path());
                    PreloadKind::Style
                }
                other => other,
            };
            out.push(PreloadRequest { url, kind });
        }
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn parse_attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = tag.as_bytes();
    let mut i = tag.find(char::is_whitespace).unwrap_or(tag.len());
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        if start == i {
            break;
        }
        let key = tag[start..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let value_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                out.push((key, tag[value_start..i].to_string()));
                i += 1;
            } else {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                out.push((key, tag[value_start..i].to_string()));
            }
        } else {
            out.push((key, String::new()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_styles_scripts_and_images() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/a.css">
            <script src="app.js"></script>
          </head><body><img src="pic.png"><a href="/skip">x</a></body></html>"#;
        let base = Url::parse("https://example.com/dir/page").unwrap();
        let requests = scan(html, Some(&base));
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].kind, PreloadKind::Style);
        assert_eq!(requests[0].url.as_str(), "https://example.com/a.css");
        assert_eq!(requests[1].url.as_str(), "https://example.com/dir/app.js");
        assert_eq!(requests[2].kind, PreloadKind::Image);
    }

    #[test]
    fn ignores_data_and_fragment_urls() {
        let html = "<img src='data:image/png;base64,xx'><img src='#x'>";
        assert!(scan(html, None).is_empty());
    }

    #[test]
    fn parses_unquoted_and_boolean_attributes() {
        let attrs = parse_attributes("input disabled value=abc");
        assert_eq!(attr(&attrs, "disabled"), Some(""));
        assert_eq!(attr(&attrs, "value"), Some("abc"));
    }
}
