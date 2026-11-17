//! A fetched resource with its decoded metadata.

use browser_url::Url;

/// A resource fetched from the network or the filesystem.
#[derive(Debug, Clone)]
pub struct Resource {
    /// Final URL after redirects.
    pub url: Url,
    /// HTTP status (200 for local files).
    pub status: u16,
    /// Detected media type.
    pub mime: String,
    /// Raw bytes.
    pub bytes: Vec<u8>,
    /// Declared charset, when known.
    pub charset: Option<String>,
}

impl Resource {
    /// Creates a resource.
    pub fn new(url: Url, status: u16, mime: &str, bytes: Vec<u8>) -> Self {
        Self {
            url,
            status,
            mime: mime.to_string(),
            bytes,
            charset: None,
        }
    }

    /// Decodes the body as text using the declared charset (UTF-8 or Latin-1).
    pub fn text(&self) -> String {
        match self.charset.as_deref() {
            Some("iso-8859-1" | "latin1" | "windows-1252") => {
                self.bytes.iter().map(|b| char::from(*b)).collect()
            }
            _ => String::from_utf8_lossy(&self.bytes).into_owned(),
        }
    }

    /// True when the resource is HTML.
    pub fn is_html(&self) -> bool {
        self.mime.contains("html")
    }

    /// True when the resource is CSS.
    pub fn is_css(&self) -> bool {
        self.mime.contains("css")
    }

    /// True when the resource is an image.
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_text_with_charset() {
        let mut resource = Resource::new(
            Url::parse("https://a.test/").unwrap(),
            200,
            "text/plain",
            vec![0x63, 0x61, 0x66, 0xe9],
        );
        resource.charset = Some("iso-8859-1".to_string());
        assert_eq!(resource.text(), "café");
        assert!(!resource.is_html());
        assert!(!resource.is_css());
    }
}
