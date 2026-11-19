//! Animation types, re-exported from `browser-style`.
//!
//! The deterministic clock and keyframe interpolation types were moved into
//! `browser-style` so that `StyleEngine::animate` can build and sample them
//! directly. `browser-compositor` depends on `browser-style` already, so the
//! public API path `browser_compositor::KeyframeAnimation` (and friends) is
//! unchanged: this module simply forwards those types. See
//! [`browser_style::animation`] for the implementation and
//! [`browser_style::StyleEngine::animate`] for the CSS bridge.

pub use browser_style::animation::{
    AnimatedValue, Animation, AnimationClock, AnimationDirection, AnimationTimeline, Easing,
    FillMode, IterationCount, Keyframe, KeyframeAnimation, PlayState, RgbaColor, StepPosition,
};
