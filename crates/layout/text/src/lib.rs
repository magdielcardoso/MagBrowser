//! Font metrics, text shaping and line breaking.
//!
//! This crate is the engine's text layer. It owns the single built-in typeface
//! — a monospace 8x16 bitmap face covering printable ASCII — and exposes the
//! metrics and line-breaking primitives that layout and paint depend on. There
//! is no TrueType/OpenType loading or shaping yet: a non-ASCII character is
//! measured and drawn with a fallback box glyph.
//!
//! ```
//! use browser_text::{Font, line_break, shape};
//!
//! let font = Font::monospace();
//! assert_eq!(font.measure("abc", 16.0), 24.0);
//! assert_eq!(font.advance('a', 16.0), 8.0);
//!
//! let run = shape(font, "abc", 16.0);
//! assert_eq!(run.glyphs.len(), 3);
//! assert_eq!(run.glyphs[2].x, 16.0);
//!
//! let broken = line_break("hello world", 40.0, 16.0);
//! assert_eq!(broken.lines, vec![(0, 5), (6, 11)]);
//! ```

#![forbid(unsafe_code)]

mod fonts;
mod glyphs;
mod line_break;
mod shaping;

pub use glyphs::{Font, Glyph};
pub use line_break::{LineBreakResult, line_break};
pub use shaping::{PositionedGlyph, ShapedRun, shape};
