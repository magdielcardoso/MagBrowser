//! Clipboard access.
//!
//! Clipboard contents are plain text. [`MemoryClipboard`] is process-local and
//! used by headless mode and tests; [`FileClipboard`] persists to a file so a
//! clipboard survives across short-lived processes without linking a native
//! clipboard API.

use std::fs;
use std::path::{Path, PathBuf};

use crate::PlatformError;

/// Text clipboard contract.
pub trait Clipboard {
    /// Returns the current clipboard text, or `None` when it is empty or
    /// unreadable.
    fn get_text(&mut self) -> Option<String>;

    /// Replaces the clipboard text.
    fn set_text(&mut self, text: &str) -> Result<(), PlatformError>;
}

/// An in-memory clipboard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryClipboard {
    text: Option<String>,
}

impl MemoryClipboard {
    /// Creates an empty clipboard.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a clipboard that already holds `text`.
    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
        }
    }

    /// Empties the clipboard.
    pub fn clear(&mut self) {
        self.text = None;
    }
}

impl Clipboard for MemoryClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.text.clone()
    }

    fn set_text(&mut self, text: &str) -> Result<(), PlatformError> {
        self.text = Some(text.to_owned());
        Ok(())
    }
}

/// A clipboard persisted in a UTF-8 text file.
///
/// `get_text` returns `None` if the file does not exist or is not valid UTF-8;
/// `set_text` creates the parent directory when needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileClipboard {
    path: PathBuf,
}

impl FileClipboard {
    /// Creates a file-backed clipboard writing to `path`.
    ///
    /// The file is created on the first [`Clipboard::set_text`] call.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path of the backing file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Clipboard for FileClipboard {
    fn get_text(&mut self) -> Option<String> {
        fs::read_to_string(&self.path).ok()
    }

    fn set_text(&mut self, text: &str) -> Result<(), PlatformError> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, text.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mag-clipboard-{}-{unique}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn memory_clipboard_round_trips() {
        let mut clipboard = MemoryClipboard::new();
        assert_eq!(clipboard.get_text(), None);
        clipboard.set_text("hello").unwrap();
        assert_eq!(clipboard.get_text().as_deref(), Some("hello"));
        clipboard.set_text("replaced").unwrap();
        assert_eq!(clipboard.get_text().as_deref(), Some("replaced"));
    }

    #[test]
    fn memory_clipboard_clear_and_with_text() {
        let mut clipboard = MemoryClipboard::with_text("seed");
        assert_eq!(clipboard.get_text().as_deref(), Some("seed"));
        clipboard.clear();
        assert_eq!(clipboard.get_text(), None);
        clipboard.set_text("").unwrap();
        assert_eq!(clipboard.get_text().as_deref(), Some(""));
    }

    #[test]
    fn memory_clipboards_are_independent() {
        let mut first = MemoryClipboard::new();
        let mut second = MemoryClipboard::with_text("second");
        first.set_text("first").unwrap();
        assert_eq!(first.get_text().as_deref(), Some("first"));
        assert_eq!(second.get_text().as_deref(), Some("second"));
    }

    #[test]
    fn file_clipboard_round_trips() {
        let path = temp_path("roundtrip.txt");
        let _ = fs::remove_file(&path);
        let mut clipboard = FileClipboard::new(&path);
        assert_eq!(clipboard.get_text(), None);
        clipboard.set_text("persisted ✓").unwrap();
        assert_eq!(clipboard.path(), path.as_path());
        // A fresh instance sees the same data, as another process would.
        let mut reopened = FileClipboard::new(&path);
        assert_eq!(reopened.get_text().as_deref(), Some("persisted ✓"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn file_clipboard_creates_parent_directories() {
        let dir = temp_path("nested");
        let path = dir.join("deeper").join("clip.txt");
        let _ = fs::remove_dir_all(&dir);
        let mut clipboard = FileClipboard::new(&path);
        clipboard.set_text("nested").unwrap();
        assert!(path.exists());
        assert_eq!(clipboard.get_text().as_deref(), Some("nested"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_clipboard_overwrites() {
        let path = temp_path("overwrite.txt");
        let mut clipboard = FileClipboard::new(&path);
        clipboard.set_text("one").unwrap();
        clipboard.set_text("two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn file_clipboard_rejects_invalid_utf8_as_none() {
        let path = temp_path("binary.bin");
        fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        let mut clipboard = FileClipboard::new(&path);
        assert_eq!(clipboard.get_text(), None);
        let _ = fs::remove_file(&path);
    }
}
