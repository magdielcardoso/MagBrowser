//! Painting: display lists and the software rasterizer.
//!
//! The engine builds a display list from the layout tree, then either executes it
//! with the software rasterizer (headless, deterministic screenshots) or hands it
//! to the GPU backend in `browser-gpu`. PNG encoding is implemented here so the
//! headless renderer needs no image library.
//!
//! ```
//! use browser_dom::Document;
//! use browser_style::{StyleEngine, Viewport};
//! use browser_layout::{Size, layout};
//! use browser_paint::render;
//!
//! let mut doc = Document::new();
//! let html = doc.create_element("html");
//! let body = doc.create_element("body");
//! doc.append_child(doc.root(), html);
//! doc.append_child(html, body);
//! doc.append_text(body, "render me");
//! let styled = StyleEngine::new(Viewport::default()).compute(&doc);
//! let tree = layout(&doc, &styled, Size::default());
//! let image = render(&tree, Size::default());
//! assert!(image.width > 0);
//! ```

#![forbid(unsafe_code)]

pub mod background;
pub mod borders;
pub mod commands;
pub mod decode;
pub mod display_list;
pub mod images;
pub mod png;
pub mod raster;

pub use commands::{build, build_with_images, content_bounds, render, render_with_background};
pub use decode::{DecodeError, ImageFormat, decode, decode_png, detect_format};
pub use display_list::{DisplayCommand, DisplayList};
pub use images::ImageData;
pub use raster::{ImageBuffer, Rasterizer};
