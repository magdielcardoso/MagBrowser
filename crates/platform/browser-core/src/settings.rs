//! Engine settings.

use std::time::Duration;

/// Configuration for a browser instance.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Viewport width in CSS pixels.
    pub viewport_width: f32,
    /// Viewport height in CSS pixels.
    pub viewport_height: f32,
    /// `User-Agent` sent with requests.
    pub user_agent: String,
    /// Whether `<script>` elements are executed.
    pub enable_scripting: bool,
    /// Whether network access is permitted at all.
    pub enable_network: bool,
    /// Page background colour used when no element paints the canvas.
    pub background: (u8, u8, u8),
    /// Maximum response body size in bytes.
    pub max_body_bytes: usize,
    /// Network timeout in seconds (bounds a single HTTP request).
    pub timeout_seconds: u64,
    /// Wall-clock deadline for an entire navigation pipeline: main document,
    /// stylesheets, images, scripts and layout/paint combined. Once it elapses
    /// the load stops gracefully and a usable partial page is shown.
    pub navigation_deadline: Duration,
    /// Preferred colour scheme reported to CSS media queries.
    pub prefers_color_scheme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            viewport_width: 1024.0,
            viewport_height: 768.0,
            user_agent: format!("Mag/{}", env!("CARGO_PKG_VERSION")),
            enable_scripting: false,
            enable_network: true,
            background: (255, 255, 255),
            max_body_bytes: 32 * 1024 * 1024,
            timeout_seconds: 20,
            navigation_deadline: Duration::from_secs(10),
            prefers_color_scheme: "light".to_string(),
        }
    }
}

impl Settings {
    /// A settings value tuned for a 1280×800 desktop window.
    pub fn desktop() -> Self {
        Self {
            viewport_width: 1280.0,
            viewport_height: 800.0,
            ..Self::default()
        }
    }

    /// Settings for a deterministic headless render.
    pub fn headless() -> Self {
        Self {
            enable_scripting: true,
            enable_network: false,
            ..Self::default()
        }
    }

    /// The viewport as a layout size.
    pub fn viewport(&self) -> browser_layout::Size {
        browser_layout::Size {
            width: self.viewport_width,
            height: self.viewport_height,
        }
    }

    /// The style engine viewport.
    pub fn style_viewport(&self) -> browser_style::Viewport {
        browser_style::Viewport {
            width: self.viewport_width,
            height: self.viewport_height,
        }
    }

    /// The background colour.
    pub fn background_color(&self) -> browser_css::Color {
        browser_css::Color::rgb(self.background.0, self.background.1, self.background.2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_consistent() {
        let settings = Settings::default();
        assert_eq!(settings.viewport().width, settings.viewport_width);
        assert_eq!(settings.style_viewport().width, settings.viewport_width);
        assert_eq!(settings.background_color().r, 255);
        assert!(!settings.enable_scripting);
        assert_eq!(settings.navigation_deadline, Duration::from_secs(10));
    }

    #[test]
    fn headless_is_scripting_enabled_offline() {
        let settings = Settings::headless();
        assert!(settings.enable_scripting);
        assert!(!settings.enable_network);
    }
}
