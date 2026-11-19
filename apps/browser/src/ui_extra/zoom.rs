//! Logical zoom levels and the scale factor fed to `render_window`.
//!
//! The browser exposes a fixed ladder of logical zoom levels. [`ZoomController`]
//! walks that ladder and computes the factor that multiplies the device pixel
//! ratio before rendering. It is pure: no winit or wgpu types appear here.

// A framework-agnostic model: not every accessor is used by the window shell.
#![allow(dead_code)]

/// The supported logical zoom levels, ascending.
pub const ZOOM_LEVELS: [f32; 11] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

/// Index of the 1.0 (100%) zoom level in [`ZOOM_LEVELS`].
pub const DEFAULT_INDEX: usize = 5;

/// Smallest logical zoom level.
pub const MIN_LEVEL: f32 = ZOOM_LEVELS[0];
/// Largest logical zoom level.
pub const MAX_LEVEL: f32 = ZOOM_LEVELS[ZOOM_LEVELS.len() - 1];
/// Default logical zoom level (100%).
pub const DEFAULT_LEVEL: f32 = ZOOM_LEVELS[DEFAULT_INDEX];

/// A zoom level selected from [`ZOOM_LEVELS`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomController {
    index: usize,
}

impl Default for ZoomController {
    fn default() -> Self {
        Self {
            index: DEFAULT_INDEX,
        }
    }
}

impl ZoomController {
    /// Creates a controller at the default 100% level.
    pub fn new() -> Self {
        Self::default()
    }

    /// Index into [`ZOOM_LEVELS`] of the current level.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The current logical zoom level (e.g. `1.25`).
    pub fn level(&self) -> f32 {
        ZOOM_LEVELS[self.index]
    }

    /// True when the controller is at the smallest level.
    pub fn is_min(&self) -> bool {
        self.index == 0
    }

    /// True when the controller is at the largest level.
    pub fn is_max(&self) -> bool {
        self.index + 1 == ZOOM_LEVELS.len()
    }

    /// Moves one step toward a larger level.
    ///
    /// Returns `true` if the level changed, `false` if already at the maximum.
    pub fn zoom_in(&mut self) -> bool {
        if self.index + 1 < ZOOM_LEVELS.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    /// Moves one step toward a smaller level.
    ///
    /// Returns `true` if the level changed, `false` if already at the minimum.
    pub fn zoom_out(&mut self) -> bool {
        if self.index > 0 {
            self.index -= 1;
            true
        } else {
            false
        }
    }

    /// Resets to the default 100% level.
    pub fn reset(&mut self) {
        self.index = DEFAULT_INDEX;
    }

    /// Sets the level by index, clamping to the valid range.
    pub fn set_index(&mut self, index: usize) {
        self.index = index.min(ZOOM_LEVELS.len() - 1);
    }

    /// Sets the level to the nearest entry in [`ZOOM_LEVELS`].
    pub fn set_level(&mut self, level: f32) {
        let mut best = 0usize;
        let mut best_diff = (ZOOM_LEVELS[0] - level).abs();
        for (i, &candidate) in ZOOM_LEVELS.iter().enumerate().skip(1) {
            let diff = (candidate - level).abs();
            if diff < best_diff {
                best = i;
                best_diff = diff;
            }
        }
        self.index = best;
    }

    /// Scale factor for rendering: the logical zoom level times the device
    /// pixel ratio passed to `render_window`.
    pub fn scale(&self, device_pixel_ratio: f32) -> f32 {
        self.level() * device_pixel_ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn starts_at_default() {
        let z = ZoomController::new();
        assert_eq!(z.index(), DEFAULT_INDEX);
        assert!(close(z.level(), 1.0));
        assert_eq!(z.level(), DEFAULT_LEVEL);
    }

    #[test]
    fn clamps_at_maximum() {
        let mut z = ZoomController::new();
        for _ in 0..100 {
            z.zoom_in();
        }
        assert_eq!(z.index(), ZOOM_LEVELS.len() - 1);
        assert!(close(z.level(), MAX_LEVEL));
        assert!(z.is_max());
        assert!(!z.is_min());
        assert!(!z.zoom_in(), "zoom_in at max must report no change");
        assert_eq!(z.index(), ZOOM_LEVELS.len() - 1);
    }

    #[test]
    fn clamps_at_minimum() {
        let mut z = ZoomController::new();
        for _ in 0..100 {
            z.zoom_out();
        }
        assert_eq!(z.index(), 0);
        assert!(close(z.level(), MIN_LEVEL));
        assert!(z.is_min());
        assert!(!z.is_max());
        assert!(!z.zoom_out(), "zoom_out at min must report no change");
        assert_eq!(z.index(), 0);
    }

    #[test]
    fn steps_through_levels_and_reports_change() {
        let mut z = ZoomController::new();
        assert!(z.zoom_in());
        assert!(close(z.level(), 1.1));
        assert!(z.zoom_out());
        assert!(close(z.level(), 1.0));
        assert!(z.zoom_out());
        assert!(close(z.level(), 0.9));
    }

    #[test]
    fn reset_returns_to_default() {
        let mut z = ZoomController::new();
        z.zoom_in();
        z.zoom_in();
        z.reset();
        assert!(close(z.level(), 1.0));
        assert_eq!(z.index(), DEFAULT_INDEX);
    }

    #[test]
    fn scale_multiplies_device_pixel_ratio() {
        let z = ZoomController::new();
        assert!(close(z.scale(1.0), 1.0));
        assert!(close(z.scale(2.0), 2.0));

        let mut z = ZoomController::new();
        z.zoom_in(); // 1.1
        assert!(close(z.scale(2.0), 2.2));

        let mut z = ZoomController::new();
        z.zoom_out(); // 0.9
        assert!(close(z.scale(3.0), 2.7));
    }

    #[test]
    fn setters_clamp_and_snap() {
        let mut z = ZoomController::new();
        z.set_index(999);
        assert!(z.is_max());
        z.set_index(0);
        assert!(z.is_min());

        z.set_level(1.3); // nearer to 1.25 than 1.5
        assert!(close(z.level(), 1.25));
        z.set_level(0.1); // below the minimum -> clamps
        assert!(close(z.level(), MIN_LEVEL));
        z.set_level(9.0); // above the maximum -> clamps
        assert!(close(z.level(), MAX_LEVEL));
    }
}
