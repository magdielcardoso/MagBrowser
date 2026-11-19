//! Address bar model: the text shown to the user and the URL it resolves to.

use browser_core::Browser;
use browser_text::Font;

/// The visible slice of the address text, plus the caret column inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleAddress {
    /// Text that fits in the field (possibly prefixed with an ellipsis).
    pub text: String,
    /// Caret position in characters from the start of [`Self::text`].
    pub caret_col: usize,
}

/// The address bar of a browser window.
#[derive(Debug, Clone, Default)]
pub struct AddressBar {
    text: String,
    caret: usize,
    focused: bool,
}

#[cfg_attr(not(feature = "window"), allow(dead_code))]
impl AddressBar {
    /// Creates an empty address bar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the text (as typed by the user) and moves the caret to the end.
    pub fn set(&mut self, text: &str) {
        self.text = text.trim().to_string();
        self.caret = self.text.chars().count();
    }

    /// Replaces the text without trimming it (used for programmatic sync).
    pub fn set_exact(&mut self, text: &str) {
        self.text = text.to_string();
        self.caret = self.text.chars().count();
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True when the field accepts keyboard input.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Focuses the field and selects the end of the text.
    pub fn focus(&mut self) {
        self.focused = true;
        self.caret = self.text.chars().count();
    }

    /// Removes keyboard focus.
    pub fn blur(&mut self) {
        self.focused = false;
    }

    /// Syncs the bar with the active page URL.
    pub fn sync(&mut self, browser: &Browser) {
        if let Some(url) = &browser.page().url {
            self.text = url.as_str();
        } else {
            self.text = String::new();
        }
        self.caret = self.text.chars().count();
    }

    /// Inserts typed text at the caret.
    pub fn insert(&mut self, input: &str) {
        let mut chars: Vec<char> = self.text.chars().collect();
        let mut caret = self.caret.min(chars.len());
        for ch in input.chars().filter(|ch| !ch.is_control()) {
            chars.insert(caret, ch);
            caret += 1;
        }
        self.text = chars.into_iter().collect();
        self.caret = caret;
    }

    /// Deletes the character before the caret.
    pub fn backspace(&mut self) {
        let mut chars: Vec<char> = self.text.chars().collect();
        if self.caret == 0 || self.caret > chars.len() {
            return;
        }
        chars.remove(self.caret - 1);
        self.caret -= 1;
        self.text = chars.into_iter().collect();
    }

    /// Deletes the character after the caret.
    pub fn delete(&mut self) {
        let mut chars: Vec<char> = self.text.chars().collect();
        if self.caret >= chars.len() {
            return;
        }
        chars.remove(self.caret);
        self.text = chars.into_iter().collect();
    }

    /// Moves the caret by `delta` characters, clamped to the text.
    pub fn move_caret(&mut self, delta: i32) {
        let len = self.text.chars().count();
        let next = self.caret as i32 + delta;
        self.caret = next.clamp(0, len as i32) as usize;
    }

    /// Moves the caret to the start of the text.
    pub fn home(&mut self) {
        self.caret = 0;
    }

    /// Moves the caret to the end of the text.
    pub fn end(&mut self) {
        self.caret = self.text.chars().count();
    }

    /// Computes the visible slice that fits in `max_width` measured pixels.
    ///
    /// Uses the real (proportional) advances so a long URL never overflows the
    /// address field, and keeps the caret in view while typing.
    pub fn visible_width(&self, max_width: f32, font: &Font, px: f32) -> VisibleAddress {
        let chars: Vec<char> = self.text.chars().collect();
        let len = chars.len();
        let caret = self.caret.min(len);
        let advances: Vec<f32> = chars.iter().map(|&ch| font.advance(ch, px)).collect();
        let total: f32 = advances.iter().sum();
        if max_width <= 0.0 {
            return VisibleAddress {
                text: String::new(),
                caret_col: 0,
            };
        }
        if total <= max_width {
            return VisibleAddress {
                text: self.text.clone(),
                caret_col: caret,
            };
        }
        let ellipsis = font.measure("\u{2026}", px);
        let available = (max_width - ellipsis).max(0.0);
        let width_of =
            |from: usize, to: usize| -> f32 { advances[from.min(len)..to.min(len)].iter().sum() };

        let mut start = 0;
        let mut end = 0;
        while end < len && width_of(0, end + 1) <= available {
            end += 1;
        }
        if caret > end {
            // Keep the caret visible by trimming characters from the left.
            start = caret;
            while start > 0 && width_of(start - 1, caret) <= available {
                start -= 1;
            }
            end = caret;
            let mut room = available - width_of(start, end);
            while end < len && room >= advances[end] {
                room -= advances[end];
                end += 1;
            }
        }
        let prefix = if start > 0 { "\u{2026}" } else { "" };
        let slice: String = chars[start..end].iter().collect();
        VisibleAddress {
            text: format!("{prefix}{slice}"),
            caret_col: if start > 0 { 1 } else { 0 } + caret.saturating_sub(start),
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

    #[test]
    fn editing_respects_the_caret() {
        let mut bar = AddressBar::new();
        bar.set("ac");
        bar.home();
        bar.move_caret(1);
        bar.insert("b");
        assert_eq!(bar.text(), "abc");
        bar.backspace();
        assert_eq!(bar.text(), "ac");
        bar.delete();
        assert_eq!(bar.text(), "a");
        bar.end();
        bar.move_caret(-1);
        assert_eq!(bar.caret, 0);
    }

    #[test]
    fn long_addresses_trim_from_the_left() {
        let font = browser_text::Font::sans();
        let px = 14.0;
        let mut bar = AddressBar::new();
        bar.set("0123456789");
        let visible = bar.visible_width(font.measure("012", px), font, px);
        assert!(visible.text.starts_with('\u{2026}'));
        assert!(visible.text.len() < bar.text().len());
        assert!(font.measure(&visible.text, px) <= font.measure("0123", px) + 0.01);
    }

    #[test]
    fn short_addresses_are_shown_whole() {
        let font = browser_text::Font::sans();
        let px = 14.0;
        let mut bar = AddressBar::new();
        bar.set("mag.dev");
        let visible = bar.visible_width(200.0, font, px);
        assert_eq!(visible.text, "mag.dev");
        assert_eq!(visible.caret_col, 7);
    }
}
