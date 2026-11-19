//! Compositor: layer tree, tiling, damage tracking and animation clocks.
//!
//! The compositor never touches pixels itself. It decides *what* needs repainting
//! and in which order layers are composited, and hands that information to the
//! paint/raster or GPU backend.
//!
//! ```
//! use browser_layout::Size;
//! use browser_compositor::{Damage, LayerTree};
//!
//! let layout = browser_layout::LayoutTree::new();
//! let mut layers = LayerTree::build(&layout, Size::default());
//! layers.resize(Size { width: 800.0, height: 600.0 });
//! let damage: Damage = layers.take_damage();
//! assert!(damage.is_empty());
//! ```

#![forbid(unsafe_code)]

pub mod animations;
pub mod clipping;
pub mod damage;
pub mod layers;
pub mod tiles;

pub use animations::{
    AnimatedValue, Animation, AnimationClock, AnimationDirection, AnimationTimeline, Easing,
    FillMode, IterationCount, Keyframe, KeyframeAnimation, PlayState, RgbaColor, StepPosition,
};
/// Re-exported so paint can ask which properties a style is currently animating
/// without depending on `browser-style` directly.
pub use browser_style::animated_properties;
pub use clipping::ClipStack;
pub use damage::Damage;
pub use layers::{Layer, LayerComposite, LayerId, LayerTree};
pub use tiles::{MAX_TILES, TileGrid};
