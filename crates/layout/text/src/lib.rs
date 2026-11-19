//! Font metrics, text rasterisation, shaping and line breaking.
//!
//! The text layer ships an embedded, open-licensed Inter outline face (SIL OFL
//! 1.1) with proportional advances. Glyphs are anti-aliased by the in-engine
//! rasterizer, so both page text and window chrome are smooth at any size.
//!
//! ```
//! use browser_text::{Font, line_break, shape, rasterize_cached};
//!
//! let font = Font::sans();
//! assert!(font.advance('M', 16.0) > font.advance('i', 16.0));
//! assert!(font.measure("abc", 16.0) > 0.0);
//!
//! let run = shape(font, "abc", 16.0);
//! assert_eq!(run.glyphs.len(), 3);
//!
//! let glyph = rasterize_cached(font, 'A', 16.0, false, false).unwrap();
//! assert!(!glyph.coverage.is_empty());
//!
//! let broken = line_break("hello world", 40.0, 16.0);
//! assert_eq!(broken.lines, vec![(0, 5), (6, 11)]);
//! ```

#![forbid(unsafe_code)]

mod glyphs;
mod line_break;
mod raster;
mod shaping;
mod truetype;

pub use glyphs::{Font, Glyph};
pub use line_break::{LineBreakResult, line_break};
pub use raster::{RasterGlyph, quantize, rasterize, rasterize_cached};
pub use shaping::{PositionedGlyph, ShapedRun, shape};
pub use truetype::{Face, OutlineGlyph, OutlinePoint};
