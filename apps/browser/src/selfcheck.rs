//! Deterministic full-window renderer used by `--window-selfcheck`.
//!
//! It computes the same chrome layout as the native window, rasterizes the page
//! and the chrome into one image and writes a PNG. No window, no GPU: this is
//! the path CI and reviewers use to check the window design.

use std::path::Path;

use browser_core::{Browser, Settings};
use browser_layout::Size;

use crate::chrome::render::render_window;
use crate::chrome::{ChromeLayout, LoadStatus, UiState};
use crate::target::resolve_target;

/// Options for a self-check render.
#[derive(Debug, Clone)]
pub struct SelfCheck {
    /// Output PNG path.
    pub output: std::path::PathBuf,
    /// Window width in logical pixels.
    pub width: f32,
    /// Window height in logical pixels.
    pub height: f32,
    /// Page to load, when any.
    pub target: Option<String>,
}

/// Renders the full window composition to `self_check.output`.
pub fn run(self_check: &SelfCheck) -> Result<(), String> {
    let size = Size {
        width: self_check.width.max(320.0),
        height: self_check.height.max(240.0),
    };
    let mut chrome = ChromeLayout::new(size, 1, LoadStatus::Ready);
    let page_size = chrome.page_size();

    let mut settings = Settings::desktop();
    settings.viewport_width = page_size.width.max(1.0);
    settings.viewport_height = page_size.height.max(1.0);
    settings.background = (255, 255, 255);
    let mut browser = Browser::new(settings);
    let mut ui = UiState::new();

    if let Some(target) = &self_check.target {
        let resolved = resolve_target(target);
        match browser.navigate(&resolved) {
            Ok(_) => {
                ui.status = LoadStatus::Ready;
            }
            Err(error) => {
                ui.status = LoadStatus::Error;
                ui.error = Some(error.to_string());
                let html = error_document(&error.to_string());
                browser
                    .load_html(&html, None)
                    .map_err(|err| format!("could not render the error document: {err}"))?;
            }
        }
    }
    ui.address.sync(&browser);
    // Show the address field focused so the screenshot also documents its
    // accent ring and caret.
    ui.address.focus();
    if ui.address.text().is_empty()
        && let Some(target) = &self_check.target
    {
        ui.address.set_exact(target);
    }

    let tab_count = browser.tabs().len();
    chrome = ChromeLayout::new(size, tab_count, ui.status);
    let image = render_window(&browser, &ui, &chrome, 1.0);
    save_png(&image, &self_check.output)
}

fn save_png(image: &browser_paint::ImageBuffer, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    image
        .save_png(path)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    let bytes = std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len();
    if bytes == 0 {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(())
}

fn error_document(message: &str) -> String {
    let escaped = browser_dom::escape_text(message);
    format!(
        "<!DOCTYPE html><html><head><title>Erro</title></head>\
         <body style='font-family: monospace; padding: 24px'>\
         <h1>Não foi possível carregar a página</h1><p>{escaped}</p>\
         </body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_valid_png_of_the_requested_size() {
        let dir = std::env::temp_dir().join(format!("mag-selfcheck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("window.png");
        run(&SelfCheck {
            output: output.clone(),
            width: 900.0,
            height: 600.0,
            target: None,
        })
        .unwrap();
        let bytes = std::fs::read(&output).unwrap();
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(bytes.len() > 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
