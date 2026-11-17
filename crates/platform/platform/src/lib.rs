//! OS integration: windows, input, clipboard, filesystem.
//!
//! The engine talks to the operating system only through the abstractions in
//! this crate, so the rest of the codebase compiles and runs headless. Every
//! abstraction has a working `std`-backed implementation:
//!
//! * [`window`] — [`WindowBackend`] with a virtual backend used by headless mode.
//! * [`input`] — modifier state, key events and click tracking.
//! * [`clipboard`] — in-memory and file-backed clipboards.
//! * [`filesystem`] — direct and sandboxed filesystem access.
//!
//! ```
//! use browser_platform::{Clipboard, MemoryClipboard, VirtualWindowBackend, WindowBackend,
//!     WindowOptions};
//!
//! let mut clipboard = MemoryClipboard::new();
//! clipboard.set_text("copied").unwrap();
//! assert_eq!(clipboard.get_text().as_deref(), Some("copied"));
//!
//! let mut backend = VirtualWindowBackend::new();
//! let id = backend.create(WindowOptions::default()).unwrap();
//! assert_eq!(backend.size(id), Some((1024, 768)));
//! ```

#![forbid(unsafe_code)]

pub mod clipboard;
pub mod filesystem;
pub mod input;
pub mod window;

pub use clipboard::{Clipboard, FileClipboard, MemoryClipboard};
pub use filesystem::{FileSystem, SandboxedFileSystem, StdFileSystem};
pub use input::{InputState, Key, KeyEvent, Modifiers, NamedKey};
pub use window::{
    BackendRequest, ButtonState, MouseButton, VirtualWindowBackend, WindowBackend, WindowEvent,
    WindowId, WindowOptions,
};

use std::fmt;
use std::path::PathBuf;

/// Errors raised by platform abstractions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// A window id is not known to the backend (or was already closed).
    UnknownWindow(WindowId),
    /// An operating-system I/O operation failed; the string is the OS message.
    Io(String),
    /// The sandbox rejected a path that escaped its root.
    PermissionDenied(PathBuf),
    /// An operation was given invalid arguments.
    InvalidInput(String),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownWindow(id) => write!(f, "unknown window {}", id.0),
            Self::Io(message) => write!(f, "I/O error: {message}"),
            Self::PermissionDenied(path) => {
                write!(f, "permission denied: {}", path.display())
            }
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
        }
    }
}

impl std::error::Error for PlatformError {}

impl From<std::io::Error> for PlatformError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}
