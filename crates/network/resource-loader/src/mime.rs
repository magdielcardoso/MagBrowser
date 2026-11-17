//! MIME type detection by extension and content sniffing.

/// Guesses a MIME type from a path or URL.
pub fn from_path(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let extension = lower.rsplit('.').next().unwrap_or("");
    match extension {
        "html" | "htm" | "xhtml" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
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
    None
}

/// Sniffs whether content looks like HTML.
pub fn looks_like_html(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(256)];
    let text = String::from_utf8_lossy(head)
        .trim_start()
        .to_ascii_lowercase();
    text.starts_with("<!doctype html") || text.starts_with("<html") || text.starts_with('<')
}

/// True when the MIME type is textual.
pub fn is_textual(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || mime.contains("javascript")
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
    fn sniffs_images() {
        assert_eq!(
            sniff_image(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]),
            Some("image/png")
        );
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"hello"), None);
    }

    #[test]
    fn detects_html_and_text() {
        assert!(looks_like_html(b"<!DOCTYPE html><html>"));
        assert!(looks_like_html(b"<p>hi"));
        assert!(!looks_like_html(b"\x89PNG"));
        assert!(is_textual("text/html"));
        assert!(!is_textual("image/png"));
    }
}
