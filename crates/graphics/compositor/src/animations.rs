//! Keyframe animation timeline used by the compositor.

use std::time::Duration;

/// An easing function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Easing {
    /// Linear interpolation.
    #[default]
    Linear,
    /// CSS `ease` approximation (cubic bezier 0.25, 0.1, 0.25, 1).
    Ease,
    /// CSS `ease-in` approximation.
    EaseIn,
    /// CSS `ease-out` approximation.
    EaseOut,
}

impl Easing {
    /// Maps linear progress `t` in 0..=1 to eased progress.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Ease => t * t * (3.0 - 2.0 * t),
            Self::EaseIn => t * t,
            Self::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
        }
    }
}

/// A running animation over a numeric property.
#[derive(Debug, Clone)]
pub struct Animation {
    /// Property name this animation drives (for example `opacity`).
    pub property: String,
    /// Start value.
    pub from: f32,
    /// End value.
    pub to: f32,
    /// Total duration.
    pub duration: Duration,
    /// Time already elapsed.
    pub elapsed: Duration,
    /// Easing function.
    pub easing: Easing,
    /// Whether the animation repeats forever.
    pub repeating: bool,
}

impl Animation {
    /// Creates a new animation from zero elapsed time.
    pub fn new(property: &str, from: f32, to: f32, duration: Duration) -> Self {
        Self {
            property: property.to_string(),
            from,
            to,
            duration,
            elapsed: Duration::ZERO,
            easing: Easing::Linear,
            repeating: false,
        }
    }

    /// Sets the easing function.
    pub fn with_easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self
    }

    /// Makes the animation repeat.
    pub fn repeating(mut self) -> Self {
        self.repeating = true;
        self
    }

    /// Advances the animation clock.
    pub fn advance(&mut self, delta: Duration) {
        self.elapsed += delta;
        if self.repeating && !self.duration.is_zero() {
            let total = self.duration.as_nanos();
            let elapsed = self.elapsed.as_nanos();
            if total > 0 {
                self.elapsed = Duration::from_nanos((elapsed % total) as u64);
            }
        }
    }

    /// Linear progress in 0..=1.
    pub fn progress(&self) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (self.elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Current interpolated value.
    pub fn value(&self) -> f32 {
        let t = self.easing.apply(self.progress());
        self.from + (self.to - self.from) * t
    }

    /// True when the animation reached its end (and is not repeating).
    pub fn is_finished(&self) -> bool {
        !self.repeating && self.progress() >= 1.0
    }
}

/// A collection of animations sharing a clock.
#[derive(Debug, Clone, Default)]
pub struct AnimationTimeline {
    animations: Vec<Animation>,
}

impl AnimationTimeline {
    /// Creates an empty timeline.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an animation.
    pub fn add(&mut self, animation: Animation) {
        self.animations.push(animation);
    }

    /// Advances every animation and drops finished ones.
    pub fn advance(&mut self, delta: Duration) -> Vec<(String, f32)> {
        for animation in &mut self.animations {
            animation.advance(delta);
        }
        self.animations.retain(|a| !a.is_finished());
        self.values()
    }

    /// Current values keyed by property.
    pub fn values(&self) -> Vec<(String, f32)> {
        self.animations
            .iter()
            .map(|a| (a.property.clone(), a.value()))
            .collect()
    }

    /// True when animations are running.
    pub fn is_active(&self) -> bool {
        !self.animations.is_empty()
    }

    /// Number of running animations.
    pub fn len(&self) -> usize {
        self.animations.len()
    }

    /// True when no animation is running.
    pub fn is_empty(&self) -> bool {
        self.animations.is_empty()
    }

    /// Clear all animations.
    pub fn clear(&mut self) {
        self.animations.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_linearly() {
        let mut animation = Animation::new("opacity", 0.0, 1.0, Duration::from_millis(1000));
        assert_eq!(animation.value(), 0.0);
        animation.advance(Duration::from_millis(500));
        assert!((animation.value() - 0.5).abs() < 0.001);
        animation.advance(Duration::from_millis(500));
        assert_eq!(animation.value(), 1.0);
        assert!(animation.is_finished());
    }

    #[test]
    fn repeats_with_modulo_time() {
        let mut animation = Animation::new("x", 0.0, 10.0, Duration::from_millis(100)).repeating();
        animation.advance(Duration::from_millis(250));
        assert!((animation.progress() - 0.5).abs() < 0.01);
        assert!(!animation.is_finished());
    }

    #[test]
    fn timeline_drops_finished() {
        let mut timeline = AnimationTimeline::new();
        timeline.add(Animation::new("a", 0.0, 1.0, Duration::from_millis(10)));
        timeline.advance(Duration::from_millis(20));
        assert!(!timeline.is_active());
    }

    #[test]
    fn easing_is_monotonic() {
        for easing in [
            Easing::Linear,
            Easing::Ease,
            Easing::EaseIn,
            Easing::EaseOut,
        ] {
            assert_eq!(easing.apply(0.0), 0.0);
            assert_eq!(easing.apply(1.0), 1.0);
            assert!(easing.apply(0.5) > 0.0);
        }
    }
}
