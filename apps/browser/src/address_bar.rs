//! Address bar model: the text shown to the user and the URL it resolves to.

use browser_core::Browser;

/// The address bar of a browser window.
#[derive(Debug, Default)]
pub struct AddressBar {
    text: String,
}

impl AddressBar {
    /// Creates an empty address bar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the text (as typed by the user).
    pub fn set(&mut self, text: &str) {
        self.text = text.trim().to_string();
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Syncs the bar with the active page URL.
    pub fn sync(&mut self, browser: &Browser) {
        if let Some(url) = &browser.page().url {
            self.text = url.as_str();
        } else {
            self.text = String::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_text_and_submission() {
        let dir = std::env::temp_dir().join(format!("mag-bar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.html");
        std::fs::write(
            &path,
            "<html><head><title>T</title></head><body></body></html>",
        )
        .unwrap();
        let mut browser = browser_core::Browser::new(browser_core::Settings::headless());
        browser
            .navigate(&format!("file://{}", path.display()))
            .unwrap();
        let mut bar = AddressBar::new();
        bar.sync(&browser);
        assert!(bar.text().contains("p.html"));
        bar.set("https://example.com");
        assert_eq!(bar.text(), "https://example.com");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
