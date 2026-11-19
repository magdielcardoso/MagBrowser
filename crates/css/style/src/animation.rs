//! Deterministic animation clocks and keyframe interpolation.
//!
//! Nothing in this module reads the wall clock: every value is a function of
//! an elapsed time supplied by the caller. That makes sampling reproducible
//! for both interactive frames and headless rendering.
//!
//! These types live in `browser-style` because the CSS-to-animation bridge
//! ([`crate::StyleEngine::animate`]) needs to build and sample them. The
//! `browser-compositor` crate re-exports them, so callers can keep importing
//! `AnimationClock`, `KeyframeAnimation`, `AnimatedValue`, and friends from
//! either crate.
//!
//! # Integration with CSS
//!
//! For each `animation` declaration the bridge parses it with
//! `browser_css::parse_animation`, finds the matching `@keyframes` rule,
//! resolves each keyframe declaration into an [`AnimatedValue`], then builds a
//! [`KeyframeAnimation`] with the parsed duration/delay (seconds), timing
//! function, iteration count, direction and fill mode and adds it to an
//! [`AnimationClock`]. Each frame the app supplies an elapsed time; the clock
//! is sampled and the resulting `(property, value)` pairs override the
//! computed style before paint. `transition` declarations work the same way:
//! a two-keyframe [`KeyframeAnimation`] interpolates from the old (host
//! supplied) value to the newly computed one.

use std::time::Duration;

/// Where a `steps()` timing function jumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepPosition {
    /// Jump at the beginning of each interval.
    Start,
    /// Jump at the end of each interval (the CSS default).
    #[default]
    End,
}

/// An easing (CSS timing) function.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Easing {
    /// Linear interpolation.
    #[default]
    Linear,
    /// CSS `ease` (`cubic-bezier(0.25, 0.1, 0.25, 1)`).
    Ease,
    /// CSS `ease-in` (`cubic-bezier(0.42, 0, 1, 1)`).
    EaseIn,
    /// CSS `ease-out` (`cubic-bezier(0, 0, 0.58, 1)`).
    EaseOut,
    /// CSS `ease-in-out` (`cubic-bezier(0.42, 0, 0.58, 1)`).
    EaseInOut,
    /// CSS `steps(count, position)`.
    Steps(u32, StepPosition),
    /// An arbitrary CSS `cubic-bezier(x1, y1, x2, y2)`.
    CubicBezier(f32, f32, f32, f32),
}

/// Evaluates a cubic Bezier with endpoints `(0, 0)` and `(1, 1)` at time `t`.
///
/// The x coordinate is solved by bisection and the matching y coordinate is
/// returned, which is what a CSS timing function does.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let sample = |u: f32, first: f32, second: f32| {
        let inverse = 1.0 - u;
        3.0 * inverse * inverse * u * first + 3.0 * inverse * u * u * second + u * u * u
    };
    let mut low = 0.0_f32;
    let mut high = 1.0_f32;
    let mut u = t;
    for _ in 0..24 {
        let x = sample(u, x1, x2);
        if (x - t).abs() < 1e-5 {
            break;
        }
        if x < t {
            low = u;
        } else {
            high = u;
        }
        u = (low + high) * 0.5;
    }
    sample(u, y1, y2)
}

impl Easing {
    /// Maps linear progress `t` in `0..=1` to eased progress.
    #[must_use]
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Ease => cubic_bezier(0.25, 0.1, 0.25, 1.0, t),
            Self::EaseIn => cubic_bezier(0.42, 0.0, 1.0, 1.0, t),
            Self::EaseOut => cubic_bezier(0.0, 0.0, 0.58, 1.0, t),
            Self::EaseInOut => cubic_bezier(0.42, 0.0, 0.58, 1.0, t),
            Self::Steps(count, position) => {
                let count = count.max(1) as f32;
                let step = (t * count).floor();
                let value = match position {
                    StepPosition::Start => (step + 1.0) / count,
                    StepPosition::End => step / count,
                };
                value.min(1.0)
            }
            Self::CubicBezier(x1, y1, x2, y2) => cubic_bezier(x1, y1, x2, y2, t),
        }
    }
}

/// An sRGB color with alpha, stored as 8-bit channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbaColor {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel (255 is opaque).
    pub a: u8,
}

impl RgbaColor {
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::rgb(0, 0, 0).with_alpha(0);
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    /// Creates a color from its channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Creates an opaque color.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::new(r, g, b, 255)
    }

    /// Replaces the alpha channel.
    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self::new(self.r, self.g, self.b, a)
    }

    /// Linearly interpolates each channel, including alpha.
    #[must_use]
    pub fn interpolate(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |from: u8, to: u8| {
            (f32::from(from) + (f32::from(to) - f32::from(from)) * t)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Self::new(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
            mix(self.a, other.a),
        )
    }
}

/// A property value that can be interpolated by an animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnimatedValue {
    /// A plain number (opacity, transform components, and so on).
    Number(f32),
    /// An sRGB color with alpha.
    Color(RgbaColor),
}

impl AnimatedValue {
    /// Interpolates two values of the same kind.
    ///
    /// Mismatched kinds cannot be interpolated, so the value closest to `t`
    /// is returned instead.
    #[must_use]
    pub fn interpolate(self, other: Self, t: f32) -> Self {
        match (self, other) {
            (Self::Number(from), Self::Number(to)) => Self::Number(from + (to - from) * t),
            (Self::Color(from), Self::Color(to)) => Self::Color(from.interpolate(to, t)),
            _ => {
                if t < 0.5 {
                    self
                } else {
                    other
                }
            }
        }
    }
}

/// A single keyframe: the property value at a normalized offset in `0..=1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Keyframe {
    /// Position within one iteration, from 0.0 to 1.0.
    pub offset: f32,
    /// The value at this offset.
    pub value: AnimatedValue,
}

impl Keyframe {
    /// Creates a keyframe, clamping its offset into `0..=1`.
    #[must_use]
    pub fn new(offset: f32, value: AnimatedValue) -> Self {
        Self {
            offset: offset.clamp(0.0, 1.0),
            value,
        }
    }
}

/// An `animation-iteration-count` value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IterationCount {
    /// A finite, non-negative number of iterations.
    Finite(f32),
    /// `infinite`.
    Infinite,
}

impl Default for IterationCount {
    fn default() -> Self {
        Self::Finite(1.0)
    }
}

impl IterationCount {
    /// A single iteration.
    pub const ONCE: Self = Self::Finite(1.0);

    /// The number of iterations, or `None` for `infinite`.
    #[must_use]
    pub fn count(self) -> Option<f32> {
        match self {
            Self::Finite(count) => Some(count),
            Self::Infinite => None,
        }
    }

    /// True when the animation repeats forever.
    #[must_use]
    pub fn is_infinite(self) -> bool {
        matches!(self, Self::Infinite)
    }
}

/// An `animation-direction` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationDirection {
    /// `normal`.
    #[default]
    Normal,
    /// `reverse`.
    Reverse,
    /// `alternate`.
    Alternate,
    /// `alternate-reverse`.
    AlternateReverse,
}

/// An `animation-fill-mode` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillMode {
    /// `none`.
    #[default]
    None,
    /// `forwards`.
    Forwards,
    /// `backwards`.
    Backwards,
    /// `both`.
    Both,
}

impl FillMode {
    /// Whether the first keyframe applies during the delay.
    #[must_use]
    pub fn fills_backwards(self) -> bool {
        matches!(self, Self::Backwards | Self::Both)
    }

    /// Whether the last keyframe applies after the animation ends.
    #[must_use]
    pub fn fills_forwards(self) -> bool {
        matches!(self, Self::Forwards | Self::Both)
    }
}

/// An `animation-play-state` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayState {
    /// `running`.
    #[default]
    Running,
    /// `paused`.
    Paused,
}

/// A keyframe animation driven by caller-supplied time.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframeAnimation {
    /// Property this animation drives (for example `opacity`).
    pub property: String,
    /// Keyframes in any order; they are sorted by offset when sampled.
    pub keyframes: Vec<Keyframe>,
    /// Length of one iteration.
    pub duration: Duration,
    /// Delay before the first iteration, in seconds (negative starts partway in).
    pub delay: f32,
    /// Easing applied inside each keyframe interval.
    pub easing: Easing,
    /// Number of iterations.
    pub iteration_count: IterationCount,
    /// Playback direction.
    pub direction: AnimationDirection,
    /// Values applied outside the active interval.
    pub fill_mode: FillMode,
    /// Whether the animation is paused.
    pub play_state: PlayState,
    /// Time at which a paused animation is frozen (`None` freezes at zero).
    pub paused_at: Option<Duration>,
}

impl KeyframeAnimation {
    /// Creates an animation from a property, its keyframes and one duration.
    #[must_use]
    pub fn new(property: impl Into<String>, keyframes: Vec<Keyframe>, duration: Duration) -> Self {
        Self {
            property: property.into(),
            keyframes,
            duration,
            delay: 0.0,
            easing: Easing::Linear,
            iteration_count: IterationCount::ONCE,
            direction: AnimationDirection::Normal,
            fill_mode: FillMode::None,
            play_state: PlayState::Running,
            paused_at: None,
        }
    }

    /// Sets the delay before the first iteration, in seconds.
    ///
    /// A negative delay starts the animation partway through its first
    /// iteration, matching CSS.
    #[must_use]
    pub fn with_delay(mut self, seconds: f32) -> Self {
        self.delay = seconds;
        self
    }

    /// Sets the easing function.
    #[must_use]
    pub fn with_easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self
    }

    /// Sets the iteration count.
    #[must_use]
    pub fn with_iteration_count(mut self, count: IterationCount) -> Self {
        self.iteration_count = count;
        self
    }

    /// Sets the playback direction.
    #[must_use]
    pub fn with_direction(mut self, direction: AnimationDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Sets the fill mode.
    #[must_use]
    pub fn with_fill_mode(mut self, fill_mode: FillMode) -> Self {
        self.fill_mode = fill_mode;
        self
    }

    /// Freezes the animation at `at`, regardless of the supplied time.
    #[must_use]
    pub fn paused_at(mut self, at: Duration) -> Self {
        self.play_state = PlayState::Paused;
        self.paused_at = Some(at);
        self
    }

    /// Samples the animation at an absolute elapsed time.
    ///
    /// Returns `None` when the animation has no value at this time (during a
    /// delay with no backwards fill, after finishing with no forwards fill, or
    /// when there are no keyframes), so the caller can fall back to base style.
    #[must_use]
    pub fn sample(&self, elapsed: Duration) -> Option<AnimatedValue> {
        if self.keyframes.is_empty() {
            return None;
        }
        let time = match self.play_state {
            PlayState::Paused => self.paused_at.unwrap_or(Duration::ZERO),
            PlayState::Running => elapsed,
        };
        let local = time.as_secs_f32() - self.delay;
        if local < 0.0 {
            return self
                .fill_mode
                .fills_backwards()
                .then(|| self.before_value());
        }
        let duration = self.duration.as_secs_f32();
        if duration <= 0.0 {
            return Some(self.after_value());
        }
        let count = self.iteration_count.count().unwrap_or(f32::INFINITY);
        if count.is_finite() && local >= duration * count {
            return self.fill_mode.fills_forwards().then(|| self.after_value());
        }
        let iteration = (local / duration).floor();
        let index = iteration as u64;
        let progress = (local / duration) - iteration;
        let forward = self.is_forward(index);
        let keyframe_progress = if forward { progress } else { 1.0 - progress };
        Some(self.interpolate_at(keyframe_progress))
    }

    /// Samples the animation at a number of seconds since it started.
    #[must_use]
    pub fn sample_secs(&self, seconds: f32) -> Option<AnimatedValue> {
        let whole = seconds.max(0.0) as u64;
        let fraction = seconds.max(0.0) - whole as f32;
        self.sample(Duration::new(whole, (fraction * 1_000_000_000.0) as u32))
    }

    fn is_forward(&self, index: u64) -> bool {
        match self.direction {
            AnimationDirection::Normal => true,
            AnimationDirection::Reverse => false,
            AnimationDirection::Alternate => index.is_multiple_of(2),
            AnimationDirection::AlternateReverse => !index.is_multiple_of(2),
        }
    }

    fn before_value(&self) -> AnimatedValue {
        let offset = if self.is_forward(0) { 0.0 } else { 1.0 };
        self.interpolate_at(offset)
    }

    fn after_value(&self) -> AnimatedValue {
        let Some(count) = self.iteration_count.count() else {
            return self.interpolate_at(self.after_offset(0.0));
        };
        self.interpolate_at(self.after_offset(count))
    }

    fn after_offset(&self, count: f32) -> f32 {
        if count <= 0.0 {
            return self.after_progress_for_index(0, 0.0);
        }
        let index = count.floor() as u64;
        let progress = if count.fract() == 0.0 {
            1.0
        } else {
            count.fract()
        };
        self.after_progress_for_index(index, progress)
    }

    fn after_progress_for_index(&self, index: u64, progress: f32) -> f32 {
        if self.is_forward(index) {
            progress
        } else {
            1.0 - progress
        }
    }

    fn interpolate_at(&self, progress: f32) -> AnimatedValue {
        let progress = progress.clamp(0.0, 1.0);
        let mut keyframes = self.keyframes.iter().collect::<Vec<_>>();
        keyframes.sort_by(|left, right| {
            left.offset
                .partial_cmp(&right.offset)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let first = keyframes[0].value;
        let last = keyframes[keyframes.len() - 1].value;
        if progress <= keyframes[0].offset {
            return first;
        }
        if progress >= keyframes[keyframes.len() - 1].offset {
            return last;
        }
        for window in keyframes.windows(2) {
            let (start, end) = (window[0], window[1]);
            if progress >= start.offset && progress <= end.offset {
                let span = end.offset - start.offset;
                let local = if span <= 0.0 {
                    0.0
                } else {
                    (progress - start.offset) / span
                };
                return start.value.interpolate(end.value, self.easing.apply(local));
            }
        }
        last
    }
}

/// A collection of keyframe animations sampled against one elapsed time.
#[derive(Debug, Clone, Default)]
pub struct AnimationClock {
    animations: Vec<KeyframeAnimation>,
}

impl AnimationClock {
    /// Creates an empty clock.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a keyframe animation.
    pub fn add(&mut self, animation: KeyframeAnimation) {
        self.animations.push(animation);
    }

    /// Adds several keyframe animations.
    pub fn extend(&mut self, animations: impl IntoIterator<Item = KeyframeAnimation>) {
        self.animations.extend(animations);
    }

    /// Samples every animation at `elapsed`, returning values keyed by property.
    ///
    /// Animations with no value at this time are omitted.
    #[must_use]
    pub fn sample(&self, elapsed: Duration) -> Vec<(String, AnimatedValue)> {
        self.animations
            .iter()
            .filter_map(|animation| {
                animation
                    .sample(elapsed)
                    .map(|value| (animation.property.clone(), value))
            })
            .collect()
    }

    /// True when the clock holds any animations.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.animations.is_empty()
    }

    /// Number of animations on the clock.
    #[must_use]
    pub fn len(&self) -> usize {
        self.animations.len()
    }

    /// True when the clock holds no animations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.animations.is_empty()
    }

    /// Removes all animations.
    pub fn clear(&mut self) {
        self.animations.clear();
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

    /// Interpolated value at an arbitrary absolute elapsed time.
    #[must_use]
    pub fn value_at(&self, elapsed: Duration) -> f32 {
        let progress = if self.duration.is_zero() {
            1.0
        } else {
            (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
        };
        let t = self.easing.apply(progress);
        self.from + (self.to - self.from) * t
    }

    /// Current interpolated value.
    pub fn value(&self) -> f32 {
        self.value_at(self.elapsed)
    }

    /// True when the animation reached its end (and is not repeating).
    pub fn is_finished(&self) -> bool {
        !self.repeating && self.progress() >= 1.0
    }
}

impl From<Animation> for KeyframeAnimation {
    fn from(animation: Animation) -> Self {
        Self::new(
            animation.property,
            vec![
                Keyframe::new(0.0, AnimatedValue::Number(animation.from)),
                Keyframe::new(1.0, AnimatedValue::Number(animation.to)),
            ],
            animation.duration,
        )
        .with_easing(animation.easing)
        .with_iteration_count(if animation.repeating {
            IterationCount::Infinite
        } else {
            IterationCount::ONCE
        })
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

    /// Deterministic sample of every animation at `elapsed`, without mutating.
    pub fn sample_at(&self, elapsed: Duration) -> Vec<(String, f32)> {
        self.animations
            .iter()
            .map(|a| (a.property.clone(), a.value_at(elapsed)))
            .collect()
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

    fn number_animation(from: f32, to: f32) -> KeyframeAnimation {
        KeyframeAnimation::new(
            "opacity",
            vec![
                Keyframe::new(0.0, AnimatedValue::Number(from)),
                Keyframe::new(1.0, AnimatedValue::Number(to)),
            ],
            Duration::from_secs(1),
        )
    }

    fn number(value: AnimatedValue) -> f32 {
        match value {
            AnimatedValue::Number(value) => value,
            AnimatedValue::Color(_) => panic!("expected a number"),
        }
    }

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
            Easing::EaseInOut,
        ] {
            assert_eq!(easing.apply(0.0), 0.0);
            assert_eq!(easing.apply(1.0), 1.0);
            assert!(easing.apply(0.5) > 0.0);
            assert!(easing.apply(0.5) < 1.0);
        }
    }

    #[test]
    fn named_easings_match_their_bezier_curves() {
        assert!((Easing::Ease.apply(0.5) - 0.8).abs() < 0.02);
        assert!((Easing::EaseIn.apply(0.5) - 0.3).abs() < 0.02);
        assert!((Easing::EaseOut.apply(0.5) - 0.7).abs() < 0.02);
        assert!((Easing::EaseInOut.apply(0.5) - 0.5).abs() < 0.02);
        assert!(Easing::EaseIn.apply(0.25) < Easing::Linear.apply(0.25));
        assert!(Easing::EaseOut.apply(0.25) > Easing::Linear.apply(0.25));
    }

    #[test]
    fn steps_sampling() {
        assert_eq!(Easing::Steps(4, StepPosition::End).apply(0.0), 0.0);
        assert_eq!(Easing::Steps(4, StepPosition::End).apply(0.5), 0.5);
        assert_eq!(Easing::Steps(4, StepPosition::End).apply(1.0), 1.0);
        assert_eq!(Easing::Steps(1, StepPosition::Start).apply(0.0), 1.0);
        assert_eq!(Easing::Steps(2, StepPosition::Start).apply(0.5), 1.0);
    }

    #[test]
    fn keyframes_are_sorted_and_interpolated() {
        let animation = KeyframeAnimation::new(
            "x",
            vec![
                Keyframe::new(1.0, AnimatedValue::Number(20.0)),
                Keyframe::new(0.0, AnimatedValue::Number(0.0)),
                Keyframe::new(0.5, AnimatedValue::Number(10.0)),
            ],
            Duration::from_secs(1),
        );
        assert_eq!(number(animation.sample(Duration::ZERO).unwrap()), 0.0);
        assert_eq!(
            number(animation.sample(Duration::from_millis(500)).unwrap()),
            10.0
        );
        assert_eq!(
            number(animation.sample(Duration::from_millis(250)).unwrap()),
            5.0
        );
        assert_eq!(
            number(animation.sample(Duration::from_millis(750)).unwrap()),
            15.0
        );
    }

    #[test]
    fn easing_applies_inside_keyframe_intervals() {
        let animation = number_animation(0.0, 1.0).with_easing(Easing::EaseIn);
        let value = number(animation.sample(Duration::from_millis(500)).unwrap());
        assert!((value - 0.3).abs() < 0.02, "value was {value}");
    }

    #[test]
    fn iteration_counts_and_finite_fill() {
        let twice = number_animation(0.0, 10.0)
            .with_iteration_count(IterationCount::Finite(2.0))
            .with_fill_mode(FillMode::Forwards);
        assert_eq!(
            number(twice.sample(Duration::from_millis(250)).unwrap()),
            2.5
        );
        assert_eq!(
            number(twice.sample(Duration::from_millis(1500)).unwrap()),
            5.0
        );
        assert_eq!(
            number(twice.sample(Duration::from_millis(2000)).unwrap()),
            10.0
        );
        assert_eq!(number(twice.sample(Duration::from_secs(5)).unwrap()), 10.0);

        let infinite = number_animation(0.0, 10.0)
            .with_iteration_count(IterationCount::Infinite)
            .with_fill_mode(FillMode::None);
        assert_eq!(
            number(infinite.sample(Duration::from_millis(3000)).unwrap()),
            0.0
        );
    }

    #[test]
    fn fractional_iteration_count_ends_partway() {
        let animation = number_animation(0.0, 10.0)
            .with_iteration_count(IterationCount::Finite(2.5))
            .with_fill_mode(FillMode::Forwards);
        assert_eq!(
            number(animation.sample(Duration::from_millis(2500)).unwrap()),
            5.0
        );
    }

    #[test]
    fn alternate_direction_reverses_odd_iterations() {
        let animation = number_animation(0.0, 10.0)
            .with_iteration_count(IterationCount::Infinite)
            .with_direction(AnimationDirection::Alternate);
        // Iteration 0 plays forward: 0 -> 10.
        assert_eq!(
            number(animation.sample(Duration::from_millis(250)).unwrap()),
            2.5
        );
        // Iteration 1 plays backwards: 10 -> 0.
        assert_eq!(
            number(animation.sample(Duration::from_millis(1250)).unwrap()),
            7.5
        );
        assert_eq!(
            number(animation.sample(Duration::from_millis(1500)).unwrap()),
            5.0
        );
        assert_eq!(
            number(animation.sample(Duration::from_millis(2000)).unwrap()),
            0.0
        );

        let reverse = number_animation(0.0, 10.0)
            .with_iteration_count(IterationCount::Infinite)
            .with_direction(AnimationDirection::Reverse);
        assert_eq!(
            number(reverse.sample(Duration::from_millis(250)).unwrap()),
            7.5
        );
    }

    #[test]
    fn fill_modes_cover_delay_and_after_phase() {
        let animation = number_animation(0.0, 10.0).with_delay(0.5);
        assert!(animation.sample(Duration::ZERO).is_none());
        assert!(animation.sample(Duration::from_millis(250)).is_none());
        assert!(animation.sample(Duration::from_millis(1500)).is_none());

        let backwards = number_animation(0.0, 10.0)
            .with_delay(0.5)
            .with_fill_mode(FillMode::Backwards);
        assert_eq!(number(backwards.sample(Duration::ZERO).unwrap()), 0.0);
        assert_eq!(
            number(backwards.sample(Duration::from_millis(250)).unwrap()),
            0.0
        );
        assert!(backwards.sample(Duration::from_millis(1500)).is_none());

        let both = number_animation(0.0, 10.0)
            .with_delay(0.5)
            .with_fill_mode(FillMode::Both);
        assert_eq!(number(both.sample(Duration::ZERO).unwrap()), 0.0);
        assert_eq!(
            number(both.sample(Duration::from_millis(2000)).unwrap()),
            10.0
        );

        let fill_reverse = number_animation(0.0, 10.0)
            .with_direction(AnimationDirection::Reverse)
            .with_fill_mode(FillMode::Both);
        assert_eq!(number(fill_reverse.sample(Duration::ZERO).unwrap()), 10.0);
        assert_eq!(
            number(fill_reverse.sample(Duration::from_millis(2000)).unwrap()),
            0.0
        );
    }

    #[test]
    fn negative_delay_starts_partway_in() {
        let animation = number_animation(0.0, 10.0).with_delay(-0.25);
        assert_eq!(number(animation.sample(Duration::ZERO).unwrap()), 2.5);
    }

    #[test]
    fn color_interpolation_channels_and_alpha() {
        let from = RgbaColor::rgb(0, 0, 0);
        let to = RgbaColor::new(200, 100, 50, 0);
        assert_eq!(from.interpolate(to, 0.0), from);
        assert_eq!(from.interpolate(to, 1.0), to);
        assert_eq!(from.interpolate(to, 0.5), RgbaColor::new(100, 50, 25, 128));
        assert_eq!(
            RgbaColor::rgb(255, 255, 255).interpolate(RgbaColor::rgb(0, 0, 0), 0.5),
            RgbaColor::rgb(128, 128, 128)
        );
    }

    #[test]
    fn keyframe_animation_interpolates_colors() {
        let animation = KeyframeAnimation::new(
            "background-color",
            vec![
                Keyframe::new(0.0, AnimatedValue::Color(RgbaColor::rgb(255, 0, 0))),
                Keyframe::new(1.0, AnimatedValue::Color(RgbaColor::rgb(0, 0, 255))),
            ],
            Duration::from_secs(1),
        );
        let Some(AnimatedValue::Color(value)) = animation.sample(Duration::from_millis(500)) else {
            panic!("expected a color");
        };
        assert_eq!(value, RgbaColor::rgb(128, 0, 128));
    }

    #[test]
    fn clock_samples_multiple_properties_without_wall_clock() {
        let mut clock = AnimationClock::new();
        clock.add(number_animation(0.0, 1.0));
        clock.add(KeyframeAnimation::new(
            "color",
            vec![
                Keyframe::new(0.0, AnimatedValue::Color(RgbaColor::BLACK)),
                Keyframe::new(1.0, AnimatedValue::Color(RgbaColor::rgb(100, 100, 100))),
            ],
            Duration::from_secs(1),
        ));
        let first = clock.sample(Duration::from_millis(500));
        let second = clock.sample(Duration::from_millis(500));
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].0, "opacity");
        assert_eq!(first[1].0, "color");
        assert_eq!(clock.len(), 2);
        assert!(clock.is_active());
        clock.clear();
        assert!(clock.is_empty());
    }

    #[test]
    fn paused_animation_freezes_at_recorded_time() {
        let animation = number_animation(0.0, 10.0).paused_at(Duration::from_millis(250));
        assert_eq!(
            number(animation.sample(Duration::from_millis(900)).unwrap()),
            2.5
        );
        assert_eq!(number(animation.sample(Duration::ZERO).unwrap()), 2.5);
    }

    #[test]
    fn simple_animation_converts_to_keyframes() {
        let animation = Animation::new("x", 5.0, 15.0, Duration::from_secs(2))
            .with_easing(Easing::EaseIn)
            .repeating();
        let keyframes: KeyframeAnimation = animation.into();
        assert_eq!(keyframes.property, "x");
        assert_eq!(keyframes.keyframes.len(), 2);
        assert_eq!(keyframes.easing, Easing::EaseIn);
        assert_eq!(keyframes.iteration_count, IterationCount::Infinite);
    }

    #[test]
    fn timeline_sample_at_is_deterministic() {
        let mut timeline = AnimationTimeline::new();
        timeline.add(Animation::new("a", 0.0, 1.0, Duration::from_secs(1)));
        assert_eq!(
            timeline.sample_at(Duration::from_millis(500)),
            vec![("a".to_string(), 0.5)]
        );
        assert_eq!(timeline.values(), vec![("a".to_string(), 0.0)]);
    }
}
