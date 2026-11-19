//! Structured values for the `animation` and `transition` shorthands.
//!
//! The declarations are kept as generic [`Value`]s by the parser; the helpers
//! here turn those values into typed descriptions that a style engine can use
//! without re-tokenising source text. Times are stored as seconds and keyframe
//! property values are left to the animation engine, which interpolates them.

use crate::values::{ComponentValue, Value};

/// Where a `steps()` timing function jumps: at the start or the end of each
/// interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StepPosition {
    /// The step changes at the beginning of each interval.
    Start,
    /// The step changes at the end of each interval (the CSS default).
    #[default]
    End,
}

/// A CSS `<easing-function>`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum TimingFunction {
    /// `linear`.
    #[default]
    Linear,
    /// `ease`.
    Ease,
    /// `ease-in`.
    EaseIn,
    /// `ease-out`.
    EaseOut,
    /// `ease-in-out`.
    EaseInOut,
    /// `steps(n)` or `steps(n, start|end)`.
    Steps(u32, StepPosition),
    /// `cubic-bezier(x1, y1, x2, y2)` control points.
    CubicBezier(f32, f32, f32, f32),
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

/// One `<single-animation>` from an `animation` shorthand.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    /// The animation name, or `None` for `none`.
    pub name: Option<String>,
    /// One iteration's duration, in seconds.
    pub duration: f32,
    /// The timing function applied to each keyframe interval.
    pub timing_function: TimingFunction,
    /// Delay before the first iteration, in seconds (may be negative).
    pub delay: f32,
    /// How many times the animation repeats.
    pub iteration_count: IterationCount,
    /// Which way the animation plays.
    pub direction: AnimationDirection,
    /// Which keyframes apply outside the active interval.
    pub fill_mode: FillMode,
    /// Whether the animation is running or paused.
    pub play_state: PlayState,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            name: None,
            duration: 0.0,
            timing_function: TimingFunction::Linear,
            delay: 0.0,
            iteration_count: IterationCount::default(),
            direction: AnimationDirection::Normal,
            fill_mode: FillMode::None,
            play_state: PlayState::Running,
        }
    }
}

impl Animation {
    /// Sets the animation name.
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Sets the duration in seconds.
    #[must_use]
    pub fn with_duration(mut self, seconds: f32) -> Self {
        self.duration = seconds;
        self
    }

    /// Sets the timing function.
    #[must_use]
    pub fn with_timing_function(mut self, timing_function: TimingFunction) -> Self {
        self.timing_function = timing_function;
        self
    }

    /// Sets the delay in seconds.
    #[must_use]
    pub fn with_delay(mut self, seconds: f32) -> Self {
        self.delay = seconds;
        self
    }

    /// Sets the iteration count.
    #[must_use]
    pub fn with_iteration_count(mut self, count: IterationCount) -> Self {
        self.iteration_count = count;
        self
    }

    /// Sets the direction.
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

    /// Sets the play state.
    #[must_use]
    pub fn with_play_state(mut self, play_state: PlayState) -> Self {
        self.play_state = play_state;
        self
    }
}

/// One `<single-transition>` from a `transition` shorthand.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    /// The property to transition (`all`, `none`, or a property name).
    pub property: String,
    /// The transition duration, in seconds.
    pub duration: f32,
    /// The timing function applied over the transition.
    pub timing_function: TimingFunction,
    /// Delay before the transition starts, in seconds (may be negative).
    pub delay: f32,
}

impl Default for Transition {
    fn default() -> Self {
        Self {
            property: "all".to_string(),
            duration: 0.0,
            timing_function: TimingFunction::Linear,
            delay: 0.0,
        }
    }
}

impl Transition {
    /// Sets the property to transition.
    #[must_use]
    pub fn with_property(mut self, property: &str) -> Self {
        self.property = property.to_string();
        self
    }

    /// Sets the duration in seconds.
    #[must_use]
    pub fn with_duration(mut self, seconds: f32) -> Self {
        self.duration = seconds;
        self
    }

    /// Sets the timing function.
    #[must_use]
    pub fn with_timing_function(mut self, timing_function: TimingFunction) -> Self {
        self.timing_function = timing_function;
        self
    }

    /// Sets the delay in seconds.
    #[must_use]
    pub fn with_delay(mut self, seconds: f32) -> Self {
        self.delay = seconds;
        self
    }
}

/// Components that are not whitespace or commas, in source order.
fn significant(components: &[ComponentValue]) -> Vec<&ComponentValue> {
    components
        .iter()
        .filter(|component| {
            !matches!(
                component,
                ComponentValue::Whitespace | ComponentValue::Comma
            )
        })
        .collect()
}

fn time_from_component(component: &ComponentValue) -> Option<f32> {
    let ComponentValue::Dimension(value, unit) = component else {
        return None;
    };
    match unit.as_str() {
        "s" => Some(*value as f32),
        "ms" => Some((*value / 1000.0) as f32),
        _ => None,
    }
}

fn numbers_from_components(components: &[&ComponentValue]) -> Option<Vec<f32>> {
    components
        .iter()
        .map(|component| match component {
            ComponentValue::Number(value) => Some(*value as f32),
            _ => None,
        })
        .collect()
}

fn timing_from_component(component: &ComponentValue) -> Option<TimingFunction> {
    match component {
        ComponentValue::Ident(name) => match name.as_str() {
            "linear" => Some(TimingFunction::Linear),
            "ease" => Some(TimingFunction::Ease),
            "ease-in" => Some(TimingFunction::EaseIn),
            "ease-out" => Some(TimingFunction::EaseOut),
            "ease-in-out" => Some(TimingFunction::EaseInOut),
            "step-start" => Some(TimingFunction::Steps(1, StepPosition::Start)),
            "step-end" => Some(TimingFunction::Steps(1, StepPosition::End)),
            _ => None,
        },
        ComponentValue::Function(name, args) => {
            let parts = significant(args);
            match name.as_str() {
                "steps" => {
                    let count = parts.first().and_then(|part| match part {
                        ComponentValue::Number(value) if *value >= 1.0 => {
                            Some((value.floor()) as u32)
                        }
                        _ => None,
                    })?;
                    let position = match parts.get(1) {
                        Some(ComponentValue::Ident(name)) if name == "start" => StepPosition::Start,
                        _ => StepPosition::End,
                    };
                    Some(TimingFunction::Steps(count, position))
                }
                "cubic-bezier" => {
                    let values = numbers_from_components(&parts)?;
                    let [x1, y1, x2, y2, ..] = values.as_slice() else {
                        return None;
                    };
                    if values.len() != 4 {
                        return None;
                    }
                    Some(TimingFunction::CubicBezier(*x1, *y1, *x2, *y2))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn is_css_wide_keyword(name: &str) -> bool {
    matches!(
        name,
        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    )
}

/// Parses a single `<time>` value, returning seconds.
///
/// Both `s` and `ms` units are accepted. Unitless values are not times in CSS,
/// so they return `None` (this also keeps iteration counts unambiguous in the
/// `animation` shorthand).
#[must_use]
pub fn parse_time(value: &Value) -> Option<f32> {
    let parts = significant(&value.components);
    let [component] = parts.as_slice() else {
        return None;
    };
    time_from_component(component)
}

/// Parses a single `<easing-function>` value.
#[must_use]
pub fn parse_timing_function(value: &Value) -> Option<TimingFunction> {
    let parts = significant(&value.components);
    let [component] = parts.as_slice() else {
        return None;
    };
    timing_from_component(component)
}

fn parse_single_animation(components: &[ComponentValue]) -> Option<Animation> {
    let mut animation = Animation::default();
    let mut times = 0;
    let mut has_name = false;

    for component in significant(components) {
        if let Some(time) = time_from_component(component) {
            if times == 0 {
                animation.duration = time;
            } else if times == 1 {
                animation.delay = time;
            }
            times += 1;
            continue;
        }
        if let Some(timing) = timing_from_component(component) {
            animation.timing_function = timing;
            continue;
        }
        match component {
            ComponentValue::Ident(name) | ComponentValue::String(name) => {
                if is_css_wide_keyword(name) {
                    return None;
                }
                match name.as_str() {
                    "none" => {
                        animation.name = None;
                        has_name = true;
                    }
                    "infinite" => animation.iteration_count = IterationCount::Infinite,
                    "normal" => animation.direction = AnimationDirection::Normal,
                    "reverse" => animation.direction = AnimationDirection::Reverse,
                    "alternate" => animation.direction = AnimationDirection::Alternate,
                    "alternate-reverse" => {
                        animation.direction = AnimationDirection::AlternateReverse;
                    }
                    "forwards" => animation.fill_mode = FillMode::Forwards,
                    "backwards" => animation.fill_mode = FillMode::Backwards,
                    "both" => animation.fill_mode = FillMode::Both,
                    "running" => animation.play_state = PlayState::Running,
                    "paused" => animation.play_state = PlayState::Paused,
                    other if !has_name => {
                        animation.name = Some(other.to_string());
                        has_name = true;
                    }
                    // An unrecognised extra keyword is ignored so one bad
                    // token cannot discard the whole animation.
                    _ => {}
                }
            }
            ComponentValue::Number(value) if *value >= 0.0 => {
                animation.iteration_count = IterationCount::Finite(*value as f32);
            }
            // Functions and other components that were not recognised as
            // timing functions are skipped without looking at their arguments.
            _ => {}
        }
    }
    Some(animation)
}

fn parse_single_transition(components: &[ComponentValue]) -> Option<Transition> {
    let mut transition = Transition::default();
    let mut times = 0;
    let mut has_property = false;

    for component in significant(components) {
        if let Some(time) = time_from_component(component) {
            if times == 0 {
                transition.duration = time;
            } else if times == 1 {
                transition.delay = time;
            }
            times += 1;
            continue;
        }
        if let Some(timing) = timing_from_component(component) {
            transition.timing_function = timing;
            continue;
        }
        match component {
            ComponentValue::Ident(name) | ComponentValue::String(name) => {
                if is_css_wide_keyword(name) {
                    return None;
                }
                match name.as_str() {
                    "none" => {
                        transition.property = "none".to_string();
                        has_property = true;
                    }
                    other if !has_property => {
                        transition.property = other.to_string();
                        has_property = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Some(transition)
}

/// Parses an `animation` shorthand value into one entry per comma-separated
/// animation.
#[must_use]
pub fn parse_animation(value: &Value) -> Vec<Animation> {
    value
        .split_commas()
        .iter()
        .filter_map(|components| parse_single_animation(components))
        .collect()
}

/// Parses a `transition` shorthand value into one entry per comma-separated
/// transition.
#[must_use]
pub fn parse_transition(value: &Value) -> Vec<Transition> {
    value
        .split_commas()
        .iter()
        .filter_map(|components| parse_single_transition(components))
        .collect()
}

impl Value {
    /// This value as a single `<time>` in seconds.
    #[must_use]
    pub fn as_time(&self) -> Option<f32> {
        parse_time(self)
    }

    /// This value as a single `<easing-function>`.
    #[must_use]
    pub fn as_timing_function(&self) -> Option<TimingFunction> {
        parse_timing_function(self)
    }

    /// This value as a list of animations (the `animation` shorthand).
    #[must_use]
    pub fn as_animations(&self) -> Vec<Animation> {
        parse_animation(self)
    }

    /// This value as a list of transitions (the `transition` shorthand).
    #[must_use]
    pub fn as_transitions(&self) -> Vec<Transition> {
        parse_transition(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(input: &str) -> Value {
        Value::parse(input)
    }

    #[test]
    fn parses_times_in_seconds_and_milliseconds() {
        assert_eq!(value("0.5s").as_time(), Some(0.5));
        assert_eq!(value("250ms").as_time(), Some(0.25));
        assert_eq!(value("-1.5s").as_time(), Some(-1.5));
        assert_eq!(value("2").as_time(), None);
        assert_eq!(value("2px").as_time(), None);
        assert_eq!(value("1s 2s").as_time(), None);
    }

    #[test]
    fn parses_easing_keywords_and_functions() {
        assert_eq!(
            value("linear").as_timing_function(),
            Some(TimingFunction::Linear)
        );
        assert_eq!(
            value("EASE-IN-OUT").as_timing_function(),
            Some(TimingFunction::EaseInOut)
        );
        assert_eq!(
            value("step-start").as_timing_function(),
            Some(TimingFunction::Steps(1, StepPosition::Start))
        );
        assert_eq!(
            value("steps(4)").as_timing_function(),
            Some(TimingFunction::Steps(4, StepPosition::End))
        );
        assert_eq!(
            value("steps(3, start)").as_timing_function(),
            Some(TimingFunction::Steps(3, StepPosition::Start))
        );
        assert_eq!(
            value("cubic-bezier(0.1, 0.7, 1, 0.1)").as_timing_function(),
            Some(TimingFunction::CubicBezier(0.1, 0.7, 1.0, 0.1))
        );
        assert_eq!(value("ease 1s").as_timing_function(), None);
    }

    #[test]
    fn parses_full_animation_shorthand_in_any_order() {
        let animations =
            value("slide 300ms ease-in-out 50ms 3 alternate both paused").as_animations();
        assert_eq!(animations.len(), 1);
        let animation = &animations[0];
        assert_eq!(animation.name.as_deref(), Some("slide"));
        assert_eq!(animation.duration, 0.3);
        assert_eq!(animation.timing_function, TimingFunction::EaseInOut);
        assert_eq!(animation.delay, 0.05);
        assert_eq!(animation.iteration_count, IterationCount::Finite(3.0));
        assert_eq!(animation.direction, AnimationDirection::Alternate);
        assert_eq!(animation.fill_mode, FillMode::Both);
        assert_eq!(animation.play_state, PlayState::Paused);
    }

    #[test]
    fn parses_multiple_animations_and_special_values() {
        let animations = value("fade 1s, none 2s linear infinite reverse forwards").as_animations();
        assert_eq!(animations.len(), 2);
        assert_eq!(animations[0].name.as_deref(), Some("fade"));
        assert_eq!(animations[0].duration, 1.0);
        assert_eq!(animations[1].name, None);
        assert_eq!(animations[1].duration, 2.0);
        assert!(animations[1].iteration_count.is_infinite());
        assert_eq!(animations[1].direction, AnimationDirection::Reverse);
        assert_eq!(animations[1].fill_mode, FillMode::Forwards);
    }

    #[test]
    fn animation_defaults_and_iteration_count() {
        let animations = value("pulse").as_animations();
        assert_eq!(animations.len(), 1);
        assert_eq!(animations[0], Animation::default().with_name("pulse"));
        assert_eq!(
            value("pulse 1s 2").as_animations()[0].iteration_count,
            IterationCount::Finite(2.0)
        );
        assert!(value("initial").as_animations().is_empty());
    }

    #[test]
    fn parses_transition_shorthand() {
        let transitions =
            value("opacity 200ms ease 1s, transform 0.5s steps(2, end)").as_transitions();
        assert_eq!(transitions.len(), 2);
        assert_eq!(transitions[0].property, "opacity");
        assert_eq!(transitions[0].duration, 0.2);
        assert_eq!(transitions[0].timing_function, TimingFunction::Ease);
        assert_eq!(transitions[0].delay, 1.0);
        assert_eq!(transitions[1].property, "transform");
        assert_eq!(transitions[1].duration, 0.5);
        assert_eq!(
            transitions[1].timing_function,
            TimingFunction::Steps(2, StepPosition::End)
        );
        assert_eq!(value("all 1s").as_transitions()[0].property, "all");
    }

    #[test]
    fn fill_mode_helpers() {
        assert!(FillMode::Backwards.fills_backwards());
        assert!(FillMode::Both.fills_backwards());
        assert!(!FillMode::Forwards.fills_backwards());
        assert!(FillMode::Forwards.fills_forwards());
        assert!(FillMode::Both.fills_forwards());
        assert!(!FillMode::Backwards.fills_forwards());
    }
}
