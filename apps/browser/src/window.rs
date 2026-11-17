//! Frame presentation for the browser shell.
//!
//! The default build presents frames as PNG files (deterministic, works over SSH
//! and in CI). A native window backend (winit + wgpu) plugs in behind the same
//! [`FramePresenter`] trait; see `docs/roadmap.md`.

use std::path::PathBuf;

use browser_core::Browser;

/// Where rendered frames go.
pub trait FramePresenter {
    /// Presents one frame and returns a human-readable description.
    fn present(&mut self, browser: &Browser) -> Result<String, String>;
}

/// Writes each frame to a numbered PNG file in a directory.
#[derive(Debug, Clone)]
pub struct PngPresenter {
    directory: PathBuf,
    frame: u32,
}

impl PngPresenter {
    /// Creates a presenter writing into `directory` (created on first frame).
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            frame: 0,
        }
    }

    /// The path of the next frame.
    pub fn next_path(&self) -> PathBuf {
        self.directory.join(format!("frame-{:04}.png", self.frame))
    }
}

impl FramePresenter for PngPresenter {
    fn present(&mut self, browser: &Browser) -> Result<String, String> {
        std::fs::create_dir_all(&self.directory)
            .map_err(|err| format!("could not create {}: {err}", self.directory.display()))?;
        let path = self.next_path();
        let image = browser.render();
        image
            .save_png(&path)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
        self.frame += 1;
        Ok(format!(
            "frame saved to {} ({}x{})",
            path.display(),
            image.width,
            image.height
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::{Browser, Settings};

    #[test]
    fn presents_frames_with_sequential_names() {
        let dir = std::env::temp_dir().join(format!("mag-frames-{}", std::process::id()));
        let mut browser = Browser::new(Settings::headless());
        browser
            .load_html("<html><body><p>frame</p></body></html>", None)
            .unwrap();
        let mut presenter = PngPresenter::new(&dir);
        assert!(
            presenter
                .present(&browser)
                .unwrap()
                .contains("frame-0000.png")
        );
        assert!(
            presenter
                .present(&browser)
                .unwrap()
                .contains("frame-0001.png")
        );
        assert!(dir.join("frame-0000.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
