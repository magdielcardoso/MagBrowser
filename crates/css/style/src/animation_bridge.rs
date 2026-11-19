//! Bridge from parsed CSS `animation`/`transition` declarations to the
//! deterministic keyframe clocks in [`crate::animation`].
//!
//! The whole path is a pure function of the document, the stylesheets, and an
//! `elapsed` time supplied by the caller. No wall clock is read, so the same
//! inputs always produce the same [`crate::StyledTree`].

use std::collections::HashMap;
use std::time::Duration;

use browser_css::{
    Animation as CssAnimation, ComponentValue, Declaration, Keyframe as CssKeyframe, Length,
    Origin, Rule, TimingFunction, Value, parse_animation, parse_time, parse_timing_function,
    parse_transition,
};
use browser_dom::{Document, NodeId};
use browser_selectors::{ElementState, Specificity};

use crate::animation::{
    AnimatedValue, AnimationClock, AnimationDirection, Easing, FillMode, IterationCount, Keyframe,
    KeyframeAnimation, PlayState, RgbaColor, StepPosition,
};
use crate::cascade::{Candidate, NO_SCOPE, UNLAYERED, pack_order};
use crate::computed::{ComputedStyle, ComputedValue, Dim, TransformFunction, TransformList};
use crate::conditions::{ContainerContext, ContainerSize};
use crate::media_queries::MediaContext;
use crate::selector_index::{NodeKeys, RuleKind};
use crate::{Color, Property, StyleEngine, StyledTree, selector_matches, specificity};

/// Properties the bridge knows how to interpolate.
///
/// Keyframe and transition declarations targeting any other property are left
/// at their cascaded (base) value.
fn is_animatable(property: Property) -> bool {
    matches!(
        property,
        Property::Opacity
            | Property::Transform
            | Property::FontSize
            | Property::Width
            | Property::Height
            | Property::MinWidth
            | Property::MinHeight
            | Property::MaxWidth
            | Property::MaxHeight
            | Property::Top
            | Property::Right
            | Property::Bottom
            | Property::Left
            | Property::MarginTop
            | Property::MarginRight
            | Property::MarginBottom
            | Property::MarginLeft
            | Property::PaddingTop
            | Property::PaddingRight
            | Property::PaddingBottom
            | Property::PaddingLeft
            | Property::RowGap
            | Property::ColumnGap
            | Property::FlexBasis
            | Property::Color
            | Property::BackgroundColor
            | Property::BorderTopColor
            | Property::BorderRightColor
            | Property::BorderBottomColor
            | Property::BorderLeftColor
            | Property::BorderTopWidth
            | Property::BorderRightWidth
            | Property::BorderBottomWidth
            | Property::BorderLeftWidth
            | Property::LetterSpacing
            | Property::WordSpacing
    )
}

fn is_color_property(property: Property) -> bool {
    matches!(
        property,
        Property::Color
            | Property::BackgroundColor
            | Property::BorderTopColor
            | Property::BorderRightColor
            | Property::BorderBottomColor
            | Property::BorderLeftColor
    )
}

fn is_dim_property(property: Property) -> bool {
    matches!(
        property,
        Property::Width
            | Property::Height
            | Property::MinWidth
            | Property::MinHeight
            | Property::MaxWidth
            | Property::MaxHeight
            | Property::Top
            | Property::Right
            | Property::Bottom
            | Property::Left
            | Property::MarginTop
            | Property::MarginRight
            | Property::MarginBottom
            | Property::MarginLeft
            | Property::PaddingTop
            | Property::PaddingRight
            | Property::PaddingBottom
            | Property::PaddingLeft
            | Property::RowGap
            | Property::ColumnGap
            | Property::FlexBasis
            | Property::LetterSpacing
            | Property::WordSpacing
    )
}

/// The transform function a numeric animation value stands for.
///
/// `AnimatedValue` only carries numbers and colours, so a transform animation
/// is encoded as a single number plus the function it belongs to. The kind is
/// stored in the clock's property string (`transform@scale`, for example) and
/// stripped again when the sample is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransformKind {
    Scale,
    ScaleX,
    ScaleY,
    Translate,
    TranslateX,
    TranslateY,
    Rotate,
}

impl TransformKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "scale" => Some(Self::Scale),
            "scalex" => Some(Self::ScaleX),
            "scaley" => Some(Self::ScaleY),
            "translate" => Some(Self::Translate),
            "translatex" => Some(Self::TranslateX),
            "translatey" => Some(Self::TranslateY),
            "rotate" => Some(Self::Rotate),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Scale => "scale",
            Self::ScaleX => "scalex",
            Self::ScaleY => "scaley",
            Self::Translate => "translate",
            Self::TranslateX => "translatex",
            Self::TranslateY => "translatey",
            Self::Rotate => "rotate",
        }
    }

    fn function(self, value: f32) -> TransformFunction {
        match self {
            Self::Scale => TransformFunction::Scale(value, value),
            Self::ScaleX => TransformFunction::ScaleX(value),
            Self::ScaleY => TransformFunction::ScaleY(value),
            Self::Translate => TransformFunction::Translate(Dim::Px(value), Dim::Px(0.0)),
            Self::TranslateX => TransformFunction::TranslateX(Dim::Px(value)),
            Self::TranslateY => TransformFunction::TranslateY(Dim::Px(value)),
            Self::Rotate => TransformFunction::Rotate(value),
        }
    }
}

fn transform_function_value(function: &TransformFunction) -> Option<(f32, TransformKind)> {
    match *function {
        TransformFunction::Scale(x, _) => Some((x, TransformKind::Scale)),
        TransformFunction::ScaleX(x) => Some((x, TransformKind::ScaleX)),
        TransformFunction::ScaleY(y) => Some((y, TransformKind::ScaleY)),
        TransformFunction::TranslateX(dim) => dim_px(dim).map(|v| (v, TransformKind::TranslateX)),
        TransformFunction::TranslateY(dim) => dim_px(dim).map(|v| (v, TransformKind::TranslateY)),
        TransformFunction::Translate(x, _) => dim_px(x).map(|v| (v, TransformKind::Translate)),
        TransformFunction::Rotate(deg) => Some((deg, TransformKind::Rotate)),
        _ => None,
    }
}

fn dim_px(dim: Dim) -> Option<f32> {
    match dim {
        Dim::Px(px) => Some(px),
        Dim::Percent(percent) => Some(percent),
        Dim::Calc { percent, px } => Some(percent + px),
        Dim::Auto => None,
    }
}

fn length_px(length: Length) -> f32 {
    match length {
        Length::Px(px) => px,
        Length::Percent(percent) => percent,
        Length::Em(em) | Length::Rem(em) => em * 16.0,
        Length::Pt(pt) => pt * 96.0 / 72.0,
        Length::Vw(vw) => vw * 10.24,
        Length::Vh(vh) => vh * 7.68,
    }
}

/// First numeric argument of a transform function, with units stripped.
fn first_number(components: &[ComponentValue]) -> Option<f32> {
    components.iter().find_map(|component| match component {
        ComponentValue::Number(value) => Some(*value as f32),
        ComponentValue::Percentage(value) => Some(*value),
        ComponentValue::Dimension(value, _) => Some(*value as f32),
        _ => None,
    })
}

/// Resolves a CSS keyframe/transition value into an interpolatable value.
///
/// Returns the transform function kind as well, for `transform` values whose
/// number needs to be written back as the right `TransformFunction`.
fn parse_animated(value: &Value) -> Option<(AnimatedValue, Option<TransformKind>)> {
    if let Some(color) = value.as_color() {
        return Some((
            AnimatedValue::Color(RgbaColor::new(color.r, color.g, color.b, color.a)),
            None,
        ));
    }
    for component in &value.components {
        if let ComponentValue::Function(name, args) = component
            && let Some(kind) = TransformKind::from_name(name)
            && let Some(number) = first_number(args)
        {
            return Some((AnimatedValue::Number(number), Some(kind)));
        }
    }
    if let Some(number) = value.as_number() {
        return Some((AnimatedValue::Number(number), None));
    }
    if let Some(length) = value.as_length() {
        return Some((AnimatedValue::Number(length_px(length)), None));
    }
    None
}

fn computed_animated_value(
    style: &ComputedStyle,
    property: Property,
) -> Option<(AnimatedValue, Option<TransformKind>)> {
    match style.get(property)? {
        ComputedValue::Color(color) => Some((
            AnimatedValue::Color(RgbaColor::new(color.r, color.g, color.b, color.a)),
            None,
        )),
        ComputedValue::Px(px) | ComputedValue::Number(px) => {
            Some((AnimatedValue::Number(*px), None))
        }
        ComputedValue::Integer(value) => Some((AnimatedValue::Number(*value as f32), None)),
        ComputedValue::Dim(dim) => dim_px(*dim).map(|px| (AnimatedValue::Number(px), None)),
        ComputedValue::Transform(list) => list
            .first()
            .and_then(transform_function_value)
            .map(|(value, kind)| (AnimatedValue::Number(value), Some(kind))),
        _ => None,
    }
}

/// The clock's property name for `property`, including the transform kind when
/// it can be recovered from `style`.
fn property_key(property: Property, style: &ComputedStyle) -> String {
    if property == Property::Transform
        && let Some(ComputedValue::Transform(list)) = style.get(Property::Transform)
        && let Some(function) = list.first()
        && let Some((_, kind)) = transform_function_value(function)
    {
        return format!("transform@{}", kind.as_str());
    }
    property.name().to_string()
}

/// Applies a sampled value to a computed style and records the property as
/// animated so paint can find it with [`animated_properties`].
fn apply_sample(style: &mut ComputedStyle, name: &str, value: AnimatedValue) {
    let (property_name, kind) = match name.split_once('@') {
        Some((base, kind)) => (base, TransformKind::from_name(kind)),
        None => (name, None),
    };
    let Some(property) = Property::from_name(property_name) else {
        return;
    };
    if property == Property::Transform {
        let number = match value {
            AnimatedValue::Number(number) => number,
            AnimatedValue::Color(_) => return,
        };
        let function = kind.unwrap_or(TransformKind::Scale).function(number);
        let list: TransformList = vec![function];
        style.set(Property::Transform, ComputedValue::Transform(list));
        style.mark_animated(Property::Transform);
        return;
    }
    if is_color_property(property) {
        if let AnimatedValue::Color(color) = value {
            style.set(
                property,
                ComputedValue::Color(Color::rgba(color.r, color.g, color.b, color.a)),
            );
            style.mark_animated(property);
        }
        return;
    }
    if let AnimatedValue::Number(number) = value {
        let computed = if is_dim_property(property) {
            ComputedValue::Dim(Dim::Px(number))
        } else {
            ComputedValue::Px(number)
        };
        style.set(property, computed);
        style.mark_animated(property);
    }
}

// ---------------------------------------------------------------------------
// CSS enum mapping
// ---------------------------------------------------------------------------

fn map_easing(function: &TimingFunction) -> Easing {
    match *function {
        TimingFunction::Linear => Easing::Linear,
        TimingFunction::Ease => Easing::Ease,
        TimingFunction::EaseIn => Easing::EaseIn,
        TimingFunction::EaseOut => Easing::EaseOut,
        TimingFunction::EaseInOut => Easing::EaseInOut,
        TimingFunction::Steps(count, position) => Easing::Steps(
            count,
            match position {
                browser_css::StepPosition::Start => StepPosition::Start,
                browser_css::StepPosition::End => StepPosition::End,
            },
        ),
        TimingFunction::CubicBezier(x1, y1, x2, y2) => Easing::CubicBezier(x1, y1, x2, y2),
    }
}

fn map_direction(direction: browser_css::AnimationDirection) -> AnimationDirection {
    match direction {
        browser_css::AnimationDirection::Normal => AnimationDirection::Normal,
        browser_css::AnimationDirection::Reverse => AnimationDirection::Reverse,
        browser_css::AnimationDirection::Alternate => AnimationDirection::Alternate,
        browser_css::AnimationDirection::AlternateReverse => AnimationDirection::AlternateReverse,
    }
}

fn map_fill(fill: browser_css::FillMode) -> FillMode {
    match fill {
        browser_css::FillMode::None => FillMode::None,
        browser_css::FillMode::Forwards => FillMode::Forwards,
        browser_css::FillMode::Backwards => FillMode::Backwards,
        browser_css::FillMode::Both => FillMode::Both,
    }
}

fn map_play(play: browser_css::PlayState) -> PlayState {
    match play {
        browser_css::PlayState::Running => PlayState::Running,
        browser_css::PlayState::Paused => PlayState::Paused,
    }
}

fn map_iteration(count: browser_css::IterationCount) -> IterationCount {
    match count {
        browser_css::IterationCount::Finite(value) => IterationCount::Finite(value),
        browser_css::IterationCount::Infinite => IterationCount::Infinite,
    }
}

// ---------------------------------------------------------------------------
// Collecting `@keyframes` and the element's animation declarations
// ---------------------------------------------------------------------------

fn collect_keyframes_from_rules(rules: &[Rule], out: &mut HashMap<String, Vec<CssKeyframe>>) {
    for rule in rules {
        match rule {
            Rule::Keyframes(keyframes) => {
                out.entry(keyframes.name.clone())
                    .or_default()
                    .extend(keyframes.keyframes.iter().cloned());
            }
            Rule::Media(rule) => collect_keyframes_from_rules(&rule.rules, out),
            Rule::Supports(rule) => collect_keyframes_from_rules(&rule.rules, out),
            Rule::Container(rule) => collect_keyframes_from_rules(&rule.rules, out),
            Rule::LayerBlock(rule) => collect_keyframes_from_rules(&rule.rules, out),
            Rule::Scope(rule) => collect_keyframes_from_rules(&rule.rules, out),
            Rule::StartingStyle(rule) => collect_keyframes_from_rules(&rule.rules, out),
            _ => {}
        }
    }
}

fn collect_keyframes(engine: &StyleEngine) -> HashMap<String, Vec<CssKeyframe>> {
    let mut out = HashMap::new();
    for rules in [
        std::slice::from_ref(&engine.user_agent),
        engine.user.as_slice(),
        engine.author.as_slice(),
    ] {
        for sheet in rules {
            collect_keyframes_from_rules(&sheet.rules, &mut out);
        }
    }
    out
}

fn is_anim_declaration(name: &str) -> bool {
    matches!(
        name,
        "animation"
            | "animation-name"
            | "animation-duration"
            | "animation-delay"
            | "animation-iteration-count"
            | "animation-direction"
            | "animation-fill-mode"
            | "animation-play-state"
            | "animation-timing-function"
            | "transition"
            | "transition-property"
            | "transition-duration"
            | "transition-timing-function"
            | "transition-delay"
    )
}

type CascadeWeight = (u8, u8, u8, u32, u32, u32, u32, u32, usize);

fn weight(
    declaration: &Declaration,
    origin: Origin,
    inline: bool,
    specificity: Specificity,
    order: usize,
) -> CascadeWeight {
    Candidate {
        property: Property::Opacity,
        value: declaration.value.clone(),
        important: declaration.important,
        origin,
        specificity,
        inline,
        order,
    }
    .weight()
}

/// The winning value of each animation/transition declaration on `node`.
///
/// This mirrors the main cascade (origin, importance, layers, scope proximity,
/// specificity, inline flag and source order) using the same `Candidate`
/// weight function, so the values seen by the bridge are the ones that won.
fn anim_declarations(
    engine: &StyleEngine,
    doc: &Document,
    node: NodeId,
    media_ok: &[bool],
    containers: &ContainerContext,
    candidates: &mut Vec<usize>,
) -> HashMap<String, Value> {
    let keys = NodeKeys::new(doc, node);
    let state = ElementState::default();
    engine
        .index
        .lookup(RuleKind::Normal, &keys, media_ok, containers, candidates);
    let mut winners: HashMap<String, (Value, CascadeWeight)> = HashMap::new();
    let mut order = 0_usize;
    for &index in candidates.iter() {
        let rule = engine.index.rule(index);
        let proximity = match &rule.scope {
            Some(scope) => match scope.proximity(doc, node, &state) {
                Some(proximity) => proximity,
                None => continue,
            },
            None => NO_SCOPE,
        };
        if !selector_matches(doc, node, &rule.selector, &state) {
            continue;
        }
        for declaration in &rule.declarations {
            order += 1;
            if !is_anim_declaration(&declaration.name) {
                continue;
            }
            let candidate_weight = weight(
                declaration,
                rule.origin,
                false,
                rule.selector.specificity,
                pack_order(order, rule.layer, proximity),
            );
            consider(&mut winners, declaration, candidate_weight);
        }
    }
    if let Some(css) = doc.attr(node, "style") {
        for declaration in browser_css::parse_inline_style(css) {
            order += 1;
            if !is_anim_declaration(&declaration.name) {
                continue;
            }
            let candidate_weight = weight(
                &declaration,
                Origin::Author,
                true,
                specificity::inline_specificity(),
                pack_order(order, UNLAYERED, NO_SCOPE),
            );
            consider(&mut winners, &declaration, candidate_weight);
        }
    }
    winners
        .into_iter()
        .map(|(name, (value, _))| (name, value))
        .collect()
}

fn consider(
    winners: &mut HashMap<String, (Value, CascadeWeight)>,
    declaration: &Declaration,
    candidate_weight: CascadeWeight,
) {
    match winners.get(&declaration.name) {
        Some((_, current)) if candidate_weight < *current => {}
        _ => {
            winners.insert(
                declaration.name.clone(),
                (declaration.value.clone(), candidate_weight),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Longhands
// ---------------------------------------------------------------------------

fn comma_values(value: &Value) -> Vec<Value> {
    value
        .split_commas()
        .into_iter()
        .map(|components| Value { components })
        .collect()
}

fn ensure_len(animations: &mut Vec<CssAnimation>, len: usize) {
    while animations.len() < len {
        animations.push(CssAnimation::default());
    }
}

/// Applies `animation-*` longhands on top of the `animation` shorthand.
fn apply_animation_longhands(animations: &mut Vec<CssAnimation>, decls: &HashMap<String, Value>) {
    if let Some(value) = decls.get("animation-name") {
        let names: Vec<Option<String>> = comma_values(value)
            .iter()
            .map(|part| match part.ident() {
                Some("none") | None => None,
                Some(name) => Some(name.to_string()),
            })
            .collect();
        ensure_len(animations, names.len());
        for (animation, name) in animations.iter_mut().zip(names) {
            animation.name = name;
        }
    }
    if let Some(value) = decls.get("animation-duration") {
        let values: Vec<f32> = comma_values(value).iter().filter_map(parse_time).collect();
        ensure_len(animations, values.len());
        for (animation, duration) in animations.iter_mut().zip(values) {
            animation.duration = duration;
        }
    }
    if let Some(value) = decls.get("animation-delay") {
        let values: Vec<f32> = comma_values(value).iter().filter_map(parse_time).collect();
        ensure_len(animations, values.len());
        for (animation, delay) in animations.iter_mut().zip(values) {
            animation.delay = delay;
        }
    }
    if let Some(value) = decls.get("animation-iteration-count") {
        let values: Vec<browser_css::IterationCount> = comma_values(value)
            .iter()
            .map(|part| {
                if part.ident() == Some("infinite") {
                    browser_css::IterationCount::Infinite
                } else {
                    browser_css::IterationCount::Finite(part.as_number().unwrap_or(1.0).max(0.0))
                }
            })
            .collect();
        ensure_len(animations, values.len());
        for (animation, count) in animations.iter_mut().zip(values) {
            animation.iteration_count = count;
        }
    }
    if let Some(value) = decls.get("animation-direction") {
        let values: Vec<browser_css::AnimationDirection> = comma_values(value)
            .iter()
            .filter_map(|part| match part.ident()? {
                "normal" => Some(browser_css::AnimationDirection::Normal),
                "reverse" => Some(browser_css::AnimationDirection::Reverse),
                "alternate" => Some(browser_css::AnimationDirection::Alternate),
                "alternate-reverse" => Some(browser_css::AnimationDirection::AlternateReverse),
                _ => None,
            })
            .collect();
        ensure_len(animations, values.len());
        for (animation, direction) in animations.iter_mut().zip(values) {
            animation.direction = direction;
        }
    }
    if let Some(value) = decls.get("animation-fill-mode") {
        let values: Vec<browser_css::FillMode> = comma_values(value)
            .iter()
            .filter_map(|part| match part.ident()? {
                "none" => Some(browser_css::FillMode::None),
                "forwards" => Some(browser_css::FillMode::Forwards),
                "backwards" => Some(browser_css::FillMode::Backwards),
                "both" => Some(browser_css::FillMode::Both),
                _ => None,
            })
            .collect();
        ensure_len(animations, values.len());
        for (animation, fill) in animations.iter_mut().zip(values) {
            animation.fill_mode = fill;
        }
    }
    if let Some(value) = decls.get("animation-play-state") {
        let values: Vec<browser_css::PlayState> = comma_values(value)
            .iter()
            .filter_map(|part| match part.ident()? {
                "running" => Some(browser_css::PlayState::Running),
                "paused" => Some(browser_css::PlayState::Paused),
                _ => None,
            })
            .collect();
        ensure_len(animations, values.len());
        for (animation, play) in animations.iter_mut().zip(values) {
            animation.play_state = play;
        }
    }
    if let Some(value) = decls.get("animation-timing-function") {
        let values: Vec<TimingFunction> = comma_values(value)
            .iter()
            .filter_map(parse_timing_function)
            .collect();
        ensure_len(animations, values.len());
        for (animation, timing) in animations.iter_mut().zip(values) {
            animation.timing_function = timing;
        }
    }
}

/// Applies `transition-*` longhands on top of the `transition` shorthand.
fn apply_transition_longhands(
    transitions: &mut Vec<browser_css::Transition>,
    decls: &HashMap<String, Value>,
) {
    if let Some(value) = decls.get("transition-property") {
        let values: Vec<String> = comma_values(value)
            .iter()
            .filter_map(|part| part.ident().map(str::to_string))
            .collect();
        while transitions.len() < values.len() {
            transitions.push(browser_css::Transition::default());
        }
        for (transition, property) in transitions.iter_mut().zip(values) {
            transition.property = property;
        }
    }
    if let Some(value) = decls.get("transition-duration") {
        let values: Vec<f32> = comma_values(value).iter().filter_map(parse_time).collect();
        while transitions.len() < values.len() {
            transitions.push(browser_css::Transition::default());
        }
        for (transition, duration) in transitions.iter_mut().zip(values) {
            transition.duration = duration;
        }
    }
    if let Some(value) = decls.get("transition-delay") {
        let values: Vec<f32> = comma_values(value).iter().filter_map(parse_time).collect();
        while transitions.len() < values.len() {
            transitions.push(browser_css::Transition::default());
        }
        for (transition, delay) in transitions.iter_mut().zip(values) {
            transition.delay = delay;
        }
    }
    if let Some(value) = decls.get("transition-timing-function") {
        let values: Vec<TimingFunction> = comma_values(value)
            .iter()
            .filter_map(parse_timing_function)
            .collect();
        while transitions.len() < values.len() {
            transitions.push(browser_css::Transition::default());
        }
        for (transition, timing) in transitions.iter_mut().zip(values) {
            transition.timing_function = timing;
        }
    }
}

// ---------------------------------------------------------------------------
// Building the clocks
// ---------------------------------------------------------------------------

fn build_keyframe_animations(
    decls: &HashMap<String, Value>,
    keyframes: &HashMap<String, Vec<CssKeyframe>>,
    node: NodeId,
    elapsed: Duration,
    paused: &mut HashMap<(NodeId, String), Duration>,
    clock: &mut AnimationClock,
) {
    let mut animations = decls
        .get("animation")
        .map(parse_animation)
        .unwrap_or_default();
    apply_animation_longhands(&mut animations, decls);

    for animation in animations {
        let Some(name) = animation.name.as_deref() else {
            continue;
        };
        let Some(frames) = keyframes.get(name) else {
            continue;
        };

        // Properties mentioned by the rule, in first-appearance order.
        let mut properties: Vec<String> = Vec::new();
        for frame in frames {
            for declaration in &frame.declarations {
                let property = declaration.name.to_ascii_lowercase();
                if Property::from_name(&property).is_some() && !properties.contains(&property) {
                    properties.push(property);
                }
            }
        }

        for property in properties {
            let mut values: Vec<Keyframe> = Vec::new();
            let mut transform_kind = None;
            for frame in frames {
                let declaration = frame
                    .declarations
                    .iter()
                    .find(|declaration| declaration.name.eq_ignore_ascii_case(&property));
                let Some(declaration) = declaration else {
                    continue;
                };
                let Some((value, kind)) = parse_animated(&declaration.value) else {
                    continue;
                };
                if transform_kind.is_none() {
                    transform_kind = kind;
                }
                values.push(Keyframe::new(frame.offset, value));
            }
            if values.is_empty() {
                continue;
            }
            let clock_property = if property == "transform" {
                match transform_kind {
                    Some(kind) => format!("transform@{}", kind.as_str()),
                    None => property.clone(),
                }
            } else {
                property.clone()
            };
            let built = KeyframeAnimation::new(
                clock_property.clone(),
                values,
                Duration::from_secs_f32(animation.duration.max(0.0)),
            )
            .with_delay(animation.delay)
            .with_easing(map_easing(&animation.timing_function))
            .with_iteration_count(map_iteration(animation.iteration_count))
            .with_direction(map_direction(animation.direction))
            .with_fill_mode(map_fill(animation.fill_mode));
            let pause_key = (node, clock_property);
            let built = if map_play(animation.play_state) == PlayState::Paused {
                // Freeze at the first elapsed time at which the animation was
                // seen paused, so it stays put even as the host advances the
                // clock each frame.
                let at = *paused.entry(pause_key).or_insert(elapsed);
                built.paused_at(at)
            } else {
                paused.remove(&pause_key);
                built
            };
            clock.add(built);
        }
    }
}

fn build_transitions(
    decls: &HashMap<String, Value>,
    old: Option<&ComputedStyle>,
    new: &ComputedStyle,
    clock: &mut AnimationClock,
) {
    let Some(old) = old else {
        return;
    };
    let mut transitions = decls
        .get("transition")
        .map(parse_transition)
        .unwrap_or_default();
    apply_transition_longhands(&mut transitions, decls);

    for transition in transitions {
        let properties: Vec<Property> = if transition.property == "all" {
            Property::ALL
                .iter()
                .copied()
                .filter(|property| is_animatable(*property))
                .collect()
        } else if transition.property == "none" {
            Vec::new()
        } else {
            Property::from_name(&transition.property)
                .filter(|property| is_animatable(*property))
                .into_iter()
                .collect()
        };
        for property in properties {
            let Some((from, _)) = computed_animated_value(old, property) else {
                continue;
            };
            let Some((to, _)) = computed_animated_value(new, property) else {
                continue;
            };
            if from == to {
                continue;
            }
            let property_name = property_key(property, old);
            let animation = KeyframeAnimation::new(
                property_name,
                vec![Keyframe::new(0.0, from), Keyframe::new(1.0, to)],
                Duration::from_secs_f32(transition.duration.max(0.0)),
            )
            .with_delay(transition.delay)
            .with_easing(map_easing(&transition.timing_function));
            clock.add(animation);
        }
    }
}

impl StyleEngine {
    /// Computes styles and applies CSS animations and transitions at `elapsed`.
    ///
    /// `base` is the *old* computed style tree supplied by the host — normally
    /// the tree from before the style change that started a transition. A
    /// transition interpolates each changed property from its value in `base`
    /// to the newly computed value; `animation` declarations are sampled from
    /// the matching `@keyframes` rule. Properties that are neither animated
    /// nor transitioning keep exactly the values produced by [`Self::compute`].
    ///
    /// `elapsed` is an absolute time chosen by the caller; this method never
    /// reads the wall clock, so sampling is deterministic. A paused animation
    /// freezes at the first `elapsed` at which it is observed paused and stays
    /// there until it becomes `running` again.
    ///
    /// Use [`animated_properties`] to find out which properties of a returned
    /// style are currently driven by an animation or transition.
    pub fn animate(&mut self, doc: &Document, elapsed: Duration, base: &StyledTree) -> StyledTree {
        let mut tree = self.compute(doc);
        let keyframes = collect_keyframes(self);
        let media = MediaContext {
            width: self.viewport.width,
            height: self.viewport.height,
            media_type: self.media_type.clone(),
            prefers_color_scheme: self.prefers_color_scheme.clone(),
        };
        let media_ok = self.index.media_ok(&media);
        let containers = ContainerContext::new(ContainerSize {
            width: self.viewport.width,
            height: self.viewport.height,
        });
        let mut candidates = Vec::new();

        for node in doc.descendants(doc.root()) {
            if !doc.node(node).is_element() {
                continue;
            }
            let decls = anim_declarations(self, doc, node, &media_ok, &containers, &mut candidates);
            if decls.is_empty() {
                continue;
            }

            let mut clock = AnimationClock::new();
            build_keyframe_animations(
                &decls,
                &keyframes,
                node,
                elapsed,
                &mut self.paused_animations,
                &mut clock,
            );
            let new_style = tree.style(node);
            build_transitions(&decls, base.get(node), &new_style, &mut clock);
            if clock.is_empty() {
                continue;
            }

            let Some(style) = tree.styles.get_mut(&node) else {
                continue;
            };
            style.clear_animated();
            for (name, value) in clock.sample(elapsed) {
                apply_sample(style, &name, value);
            }
        }
        tree
    }
}

/// The properties of `style` currently overridden by an animation or
/// transition, in canonical order.
///
/// Paint can use this to limit damage to the properties that actually change
/// from frame to frame. Always empty for a plain [`StyleEngine::compute`]
/// result.
#[must_use]
pub fn animated_properties(style: &ComputedStyle) -> Vec<Property> {
    style.animated_properties()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use browser_css::{Origin, Stylesheet};
    use browser_dom::Document;

    use super::*;
    use crate::Viewport;

    fn setup(css: &str) -> (StyleEngine, Document, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_stylesheet(Stylesheet::parse(css, Origin::Author));
        (engine, doc, body)
    }

    fn opacity(style: &ComputedStyle) -> f32 {
        match style.get(Property::Opacity) {
            Some(ComputedValue::Px(value)) => *value,
            other => panic!("expected an opacity value, got {other:?}"),
        }
    }

    fn sample(css: &str, millis: u64) -> (StyledTree, NodeId) {
        let (mut engine, doc, body) = setup(css);
        let base = engine.compute(&doc);
        (
            engine.animate(&doc, Duration::from_millis(millis), &base),
            body,
        )
    }

    #[test]
    fn opacity_animates_from_zero_to_one_at_half() {
        let css = "@keyframes fade { from { opacity: 0; } to { opacity: 1; } } \
                   body { animation: fade 1s linear; opacity: 0; }";
        let (tree, body) = sample(css, 500);
        let style = tree.get(body).unwrap();
        assert!((opacity(style) - 0.5).abs() < 0.001, "{}", opacity(style));
        assert_eq!(animated_properties(style), vec![Property::Opacity]);

        let (tree, body) = sample(css, 0);
        assert!((opacity(tree.get(body).unwrap()) - 0.0).abs() < 0.001);
        let (tree, body) = sample(css, 999);
        assert!((opacity(tree.get(body).unwrap()) - 1.0).abs() < 0.001);
    }

    #[test]
    fn alternate_direction_reverses_odd_iterations() {
        let css = "@keyframes fade { from { opacity: 0; } to { opacity: 1; } } \
                   body { animation: fade 1s linear infinite alternate; opacity: 0; }";
        let (tree, body) = sample(css, 250);
        assert!((opacity(tree.get(body).unwrap()) - 0.25).abs() < 0.001);
        let (tree, body) = sample(css, 1250);
        assert!((opacity(tree.get(body).unwrap()) - 0.75).abs() < 0.001);
        let (tree, body) = sample(css, 2000);
        assert!((opacity(tree.get(body).unwrap()) - 0.0).abs() < 0.001);
    }

    #[test]
    fn fill_mode_forwards_after_end() {
        let css = "@keyframes fade { from { opacity: 0; } to { opacity: 1; } } \
                   body { animation: fade 1s linear forwards; opacity: 0; }";
        let (tree, body) = sample(css, 5000);
        assert!((opacity(tree.get(body).unwrap()) - 1.0).abs() < 0.001);

        let css = "@keyframes fade { from { opacity: 0; } to { opacity: 1; } } \
                   body { animation: fade 1s linear; opacity: 0; }";
        let (tree, body) = sample(css, 5000);
        assert!((opacity(tree.get(body).unwrap()) - 0.0).abs() < 0.001);
    }

    #[test]
    fn no_animation_is_identity() {
        let (mut engine, doc, body) = setup("body { color: red; opacity: 0.4; }");
        let base = engine.compute(&doc);
        let animated = engine.animate(&doc, Duration::from_secs(1), &base);
        assert_eq!(base.get(body), animated.get(body));
        assert!(animated_properties(animated.get(body).unwrap()).is_empty());
    }

    #[test]
    fn colors_widths_font_sizes_and_transforms_interpolate() {
        let css = "@keyframes grow { \
                     from { background-color: #ff0000; width: 10px; font-size: 10px; transform: scale(0); } \
                     to { background-color: #0000ff; width: 110px; font-size: 20px; transform: scale(2); } \
                   } \
                   body { animation: grow 1s linear; }";
        let (tree, body) = sample(css, 500);
        let style = tree.get(body).unwrap();
        assert_eq!(style.background_color(), Color::rgb(128, 0, 128));
        assert_eq!(style.width(), Dim::Px(60.0));
        assert!((style.font_size() - 15.0).abs() < 0.001);
        assert_eq!(style.transform_raw(), &[TransformFunction::Scale(1.0, 1.0)]);
        assert_eq!(
            animated_properties(style),
            vec![
                Property::Width,
                Property::BackgroundColor,
                Property::FontSize,
                Property::Transform,
            ]
        );
    }

    #[test]
    fn transition_uses_old_styles_from_base() {
        let (mut engine, doc, body) = setup("body { opacity: 0; }");
        // Host keeps the style tree from before the change as `base`.
        let base = engine.compute(&doc);
        assert!((opacity(base.get(body).unwrap()) - 0.0).abs() < 0.001);

        // The new style has opacity 1 and a transition; at half time the
        // sampled value must be halfway between the old and the new value.
        engine.add_css(
            "body { opacity: 1; transition: opacity 1s linear; }",
            Origin::Author,
        );
        let animated = engine.animate(&doc, Duration::from_millis(500), &base);
        let style = animated.get(body).unwrap();
        assert!((opacity(style) - 0.5).abs() < 0.001, "{}", opacity(style));
        assert_eq!(animated_properties(style), vec![Property::Opacity]);
    }

    #[test]
    fn paused_animation_stays_frozen_while_time_advances() {
        let css = "@keyframes fade { from { opacity: 0; } to { opacity: 1; } } \
                   body { animation: fade 1s linear paused; opacity: 0; }";
        let (mut engine, doc, body) = setup(css);
        let base = engine.compute(&doc);
        let first = engine.animate(&doc, Duration::from_millis(500), &base);
        let second = engine.animate(&doc, Duration::from_millis(900), &base);
        assert!((opacity(first.get(body).unwrap()) - 0.5).abs() < 0.001);
        assert_eq!(first.get(body), second.get(body));
    }

    #[test]
    fn longhand_animation_properties_are_honoured() {
        let css = "@keyframes fade { from { opacity: 0; } to { opacity: 1; } } \
                   body { \
                     animation-name: fade; animation-duration: 1s; \
                     animation-timing-function: linear; animation-fill-mode: forwards; \
                   }";
        let (tree, body) = sample(css, 2000);
        assert!((opacity(tree.get(body).unwrap()) - 1.0).abs() < 0.001);
    }
}
