//! Component values, parsed values and typed lengths and colors.

/// A single component value inside a declaration value.
#[derive(Debug, Clone, PartialEq)]
pub enum ComponentValue {
    /// An identifier, stored lowercased (CSS keywords are ASCII
    /// case-insensitive).
    Ident(String),
    /// A number.
    Number(f64),
    /// A percentage.
    Percentage(f32),
    /// A number with a unit, stored with a lowercased unit.
    Dimension(f64, String),
    /// A string literal with escapes resolved.
    String(String),
    /// A `url(...)` token.
    Url(String),
    /// A hash token without the `#`.
    Hash(String),
    /// A function call, with its name lowercased and its arguments.
    Function(String, Vec<ComponentValue>),
    /// A single delimiter character.
    Delim(char),
    /// A comma.
    Comma,
    /// A run of whitespace.
    Whitespace,
}

/// Function names that produce a CSS `<math-function>` value.
///
/// The tokenizer turns any `ident(` sequence into a [`ComponentValue::Function`],
/// so these need no special casing during tokenization — they are only named
/// here so callers can recognise math expressions (for example to evaluate
/// them) after parsing.
const MATH_FUNCTION_NAMES: [&str; 4] = ["calc", "min", "max", "clamp"];

fn is_math_function_name(name: &str) -> bool {
    MATH_FUNCTION_NAMES.contains(&name)
}

impl ComponentValue {
    /// If this component is a function, returns its name and arguments.
    ///
    /// A plain parenthesised group (for example the `(...)` inside
    /// `calc((1px + 2px))`) is represented as a function with an empty name
    /// and is returned as `Some(("", args))`.
    #[must_use]
    pub fn as_function(&self) -> Option<(&str, &[ComponentValue])> {
        match self {
            ComponentValue::Function(name, args) => Some((name, args)),
            _ => None,
        }
    }

    /// If this component is a CSS math function (`calc`, `min`, `max` or
    /// `clamp`), returns its name and arguments.
    ///
    /// Parenthesised groups with an empty name are *not* returned here; use
    /// [`ComponentValue::as_function`] when those are needed too.
    #[must_use]
    pub fn as_math_function(&self) -> Option<(&str, &[ComponentValue])> {
        let (name, args) = self.as_function()?;
        is_math_function_name(name).then_some((name, args))
    }

    /// True when this component is a CSS math function.
    #[must_use]
    pub fn is_math_function(&self) -> bool {
        self.as_math_function().is_some()
    }
}

/// A parsed declaration value: a list of component values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Value {
    /// The component values in source order, whitespace included.
    pub components: Vec<ComponentValue>,
}

/// A length as understood by the style engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// Pixels.
    Px(f32),
    /// Relative to the parent font size.
    Em(f32),
    /// Relative to the root font size.
    Rem(f32),
    /// Relative to the containing block width.
    Percent(f32),
    /// Points (1/72 inch).
    Pt(f32),
    /// Relative to the viewport width.
    Vw(f32),
    /// Relative to the viewport height.
    Vh(f32),
}

/// An 8-bit-per-channel sRGB color with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel, 255 = opaque.
    pub a: u8,
}

impl Color {
    /// Black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// White.
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    /// Creates a color from channels.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Creates an opaque color.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Fully transparent black.
    #[must_use]
    pub const fn transparent() -> Self {
        Self::rgba(0, 0, 0, 0)
    }

    /// Parses a hexadecimal color of the form `#rgb`, `#rgba`, `#rrggbb` or
    /// `#rrggbbaa`. The leading `#` is optional and hex digits may have any
    /// case.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        parse_hex_color(hex.strip_prefix('#').unwrap_or(hex))
    }

    /// Parses the arguments of an `rgb()` or `rgba()` function.
    ///
    /// Both the legacy comma syntax (`rgb(255, 0, 0, 0.5)`) and the modern
    /// space-separated syntax with an optional `/` alpha
    /// (`rgb(255 0 0 / 50%)`) are accepted.
    #[must_use]
    pub fn from_rgb_function(args: &[ComponentValue]) -> Option<Self> {
        parse_rgb_function(args)
    }

    /// Parses the arguments of an `hsl()` or `hsla()` function.
    ///
    /// Both the legacy comma syntax and the modern space-separated syntax are
    /// accepted, including a `/` alpha and hue units (`deg`, `rad`, `grad`,
    /// `turn` or a percentage).
    #[must_use]
    pub fn from_hsl_function(args: &[ComponentValue]) -> Option<Self> {
        parse_hsl_function(args)
    }

    /// Parses the arguments of an `oklch()` function (Oklab Lightness,
    /// Chroma, Hue in the OKLab color space).
    ///
    /// Accepts the modern space-separated syntax `oklch(L C H / A)`. Lightness
    /// may be a number (`0..1`) or a percentage (`0%..100%`); hue accepts the
    /// same units as [`Color::from_hsl_function`]. Out-of-gamut results are
    /// clamped to sRGB after conversion.
    #[must_use]
    pub fn from_oklch_function(args: &[ComponentValue]) -> Option<Self> {
        parse_oklch_function(args)
    }

    /// Parses the arguments of an `oklab()` function.
    ///
    /// Accepts `oklab(L a b / A)`, where `L` is a number (`0..1`) or a
    /// percentage and `a`/`b` are numbers (percentages are scaled so `100%`
    /// equals `0.4`, matching CSS Color 4). Results are gamut-clamped to sRGB.
    #[must_use]
    pub fn from_oklab_function(args: &[ComponentValue]) -> Option<Self> {
        parse_oklab_function(args)
    }

    /// Parses the arguments of a CIE `lab()` function (D50-referenced).
    ///
    /// Accepts `lab(L a b / A)`, where `L` is `0..100` (or `0%..100%`) and
    /// `a`/`b` are signed numbers. The D50 result is Bradford-adapted to D65
    /// before conversion and gamut clamping to sRGB.
    #[must_use]
    pub fn from_lab_function(args: &[ComponentValue]) -> Option<Self> {
        parse_lab_function(args)
    }

    /// Parses the arguments of a CIE `lch()` function (D50-referenced).
    ///
    /// Accepts `lch(L C H / A)`. `L` is `0..100`, `C` a non-negative number and
    /// `H` a hue with the units accepted by [`Color::from_hsl_function`].
    #[must_use]
    pub fn from_lch_function(args: &[ComponentValue]) -> Option<Self> {
        parse_lch_function(args)
    }

    /// Parses the arguments of an `hwb()` function (Hue, Whiteness,
    /// Blackness).
    ///
    /// Accepts `hwb(H W B / A)`. Whiteness and blackness are numbers (`0..1`)
    /// or percentages (`0%..100%`); the pure hue is derived via HSV and then
    /// mixed with white and black.
    #[must_use]
    pub fn from_hwb_function(args: &[ComponentValue]) -> Option<Self> {
        parse_hwb_function(args)
    }

    /// Parses the arguments of a `color()` function.
    ///
    /// Supports the `srgb`, `srgb-linear` and `display-p3` color spaces, e.g.
    /// `color(display-p3 1 0 0 / 50%)`. Other spaces return `None`.
    #[must_use]
    pub fn from_color_function(args: &[ComponentValue]) -> Option<Self> {
        parse_color_function(args)
    }

    /// Parses the arguments of a `color-mix()` function.
    ///
    /// Supports `color-mix(in srgb, A x%, B y%)` (and `in srgb-linear`), where
    /// `A` and `B` are any colors this parser understands. Weight percentages
    /// are normalized; a missing weight uses the remainder, and both missing
    /// weights default to 50/50. Mixing uses premultiplied alpha.
    #[must_use]
    pub fn from_color_mix_function(args: &[ComponentValue]) -> Option<Self> {
        parse_color_mix_function(args)
    }

    /// Serializes the color as CSS (`#rrggbb` when opaque, `rgba()` otherwise).
    ///
    /// The `&self` receiver is part of the crate's public API.
    #[must_use]
    #[allow(clippy::wrong_self_convention)]
    pub fn to_css(&self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            let mut alpha = format!("{:.3}", f32::from(self.a) / 255.0);
            while alpha.ends_with('0') {
                alpha.pop();
            }
            if alpha.ends_with('.') {
                alpha.pop();
            }
            format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, alpha)
        }
    }
}

impl Value {
    /// Parses a declaration value from `input`, preserving functions and
    /// nested parentheses as component values.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        use crate::tokenizer::{to_component_values, tokenize};

        Self {
            components: to_component_values(&tokenize(input)),
        }
    }

    /// True when the value has no components other than whitespace.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self
            .components
            .iter()
            .any(|component| !matches!(component, ComponentValue::Whitespace))
    }

    /// The single identifier of the value, lowercased.
    #[must_use]
    pub fn ident(&self) -> Option<&str> {
        match self.single()? {
            ComponentValue::Ident(name) => Some(name),
            _ => None,
        }
    }

    /// All top-level identifiers, lowercased (whitespace skipped).
    #[must_use]
    pub fn as_ident_list(&self) -> Vec<String> {
        self.components
            .iter()
            .filter_map(|component| match component {
                ComponentValue::Ident(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// The value as a single length or percentage, or unitless zero.
    #[must_use]
    pub fn as_length(&self) -> Option<Length> {
        match self.single()? {
            ComponentValue::Percentage(value) => Some(Length::Percent(*value)),
            ComponentValue::Number(value) if *value == 0.0 => Some(Length::Px(0.0)),
            ComponentValue::Dimension(value, unit) => {
                let value = *value as f32;
                match unit.as_str() {
                    "px" => Some(Length::Px(value)),
                    "em" => Some(Length::Em(value)),
                    "rem" => Some(Length::Rem(value)),
                    "pt" => Some(Length::Pt(value)),
                    "vw" => Some(Length::Vw(value)),
                    "vh" => Some(Length::Vh(value)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The value as a single number.
    #[must_use]
    pub fn as_number(&self) -> Option<f32> {
        match self.single()? {
            ComponentValue::Number(value) => Some(*value as f32),
            _ => None,
        }
    }

    /// If the value consists of a single math function, returns its name and
    /// arguments. Whitespace around the function is ignored.
    #[must_use]
    pub fn as_math_function(&self) -> Option<(&str, &[ComponentValue])> {
        self.single()?.as_math_function()
    }

    /// The value as a single color.
    ///
    /// Accepts named colors, `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa` hex colors,
    /// and the color functions `rgb()`/`rgba()`, `hsl()`/`hsla()`, `oklch()`,
    /// `oklab()`, `lab()`, `lch()`, `hwb()`, `color()` and `color-mix()`. The
    /// legacy comma and modern space-separated forms are both understood where
    /// the function defines them.
    #[must_use]
    pub fn as_color(&self) -> Option<Color> {
        parse_color_component(self.single()?)
    }

    /// The value as a single string.
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        match self.single()? {
            ComponentValue::String(value) => Some(value),
            _ => None,
        }
    }

    /// The value as a single URL.
    #[must_use]
    pub fn as_url(&self) -> Option<&str> {
        match self.single()? {
            ComponentValue::Url(value) => Some(value),
            _ => None,
        }
    }

    /// The first non-whitespace component value.
    #[must_use]
    pub fn first(&self) -> Option<&ComponentValue> {
        self.components
            .iter()
            .find(|component| !matches!(component, ComponentValue::Whitespace))
    }

    /// Splits the value on top-level commas, trimming whitespace from groups.
    ///
    /// Empty groups (for example from a trailing comma) are dropped.
    #[must_use]
    pub fn split_commas(&self) -> Vec<Vec<ComponentValue>> {
        let mut groups = Vec::new();
        let mut current = Vec::new();
        for component in &self.components {
            match component {
                ComponentValue::Comma => {
                    groups.push(std::mem::take(&mut current));
                }
                _ => current.push(component.clone()),
            }
        }
        groups.push(current);
        groups
            .into_iter()
            .map(|mut group| {
                while matches!(group.first(), Some(ComponentValue::Whitespace)) {
                    group.remove(0);
                }
                while matches!(group.last(), Some(ComponentValue::Whitespace)) {
                    group.pop();
                }
                group
            })
            .filter(|group| !group.is_empty())
            .collect()
    }

    fn single(&self) -> Option<&ComponentValue> {
        let mut found = None;
        for component in &self.components {
            if matches!(component, ComponentValue::Whitespace) {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(component);
        }
        found
    }
}

fn named_color(name: &str) -> Option<Color> {
    let color = match name {
        "black" => Color::rgb(0, 0, 0),
        "white" => Color::rgb(255, 255, 255),
        "red" => Color::rgb(255, 0, 0),
        "green" => Color::rgb(0, 128, 0),
        "blue" => Color::rgb(0, 0, 255),
        "yellow" => Color::rgb(255, 255, 0),
        "gray" | "grey" => Color::rgb(128, 128, 128),
        "silver" => Color::rgb(192, 192, 192),
        "maroon" => Color::rgb(128, 0, 0),
        "olive" => Color::rgb(128, 128, 0),
        "lime" => Color::rgb(0, 255, 0),
        "aqua" | "cyan" => Color::rgb(0, 255, 255),
        "teal" => Color::rgb(0, 128, 128),
        "navy" => Color::rgb(0, 0, 128),
        "fuchsia" | "magenta" => Color::rgb(255, 0, 255),
        "purple" => Color::rgb(128, 0, 128),
        "orange" => Color::rgb(255, 165, 0),
        "transparent" => Color::transparent(),
        "brown" => Color::rgb(165, 42, 42),
        "pink" => Color::rgb(255, 192, 203),
        "gold" => Color::rgb(255, 215, 0),
        "violet" => Color::rgb(238, 130, 238),
        "indigo" => Color::rgb(75, 0, 130),
        "crimson" => Color::rgb(220, 20, 60),
        "darkgray" | "darkgrey" => Color::rgb(169, 169, 169),
        "lightgray" | "lightgrey" => Color::rgb(211, 211, 211),
        "currentcolor" => return None,
        _ => return None,
    };
    Some(color)
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let digits: Vec<u32> = hex
        .chars()
        .map(|character| character.to_digit(16))
        .collect::<Option<Vec<_>>>()?;
    match digits.len() {
        3 => Some(Color::rgb(
            (digits[0] * 17) as u8,
            (digits[1] * 17) as u8,
            (digits[2] * 17) as u8,
        )),
        4 => Some(Color::rgba(
            (digits[0] * 17) as u8,
            (digits[1] * 17) as u8,
            (digits[2] * 17) as u8,
            (digits[3] * 17) as u8,
        )),
        6 => Some(Color::rgb(
            ((digits[0] << 4) | digits[1]) as u8,
            ((digits[2] << 4) | digits[3]) as u8,
            ((digits[4] << 4) | digits[5]) as u8,
        )),
        8 => Some(Color::rgba(
            ((digits[0] << 4) | digits[1]) as u8,
            ((digits[2] << 4) | digits[3]) as u8,
            ((digits[4] << 4) | digits[5]) as u8,
            ((digits[6] << 4) | digits[7]) as u8,
        )),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Numeric {
    Number(f64),
    Percent(f64),
}

impl Numeric {
    fn parse(component: &ComponentValue) -> Option<Self> {
        match component {
            ComponentValue::Number(value) => Some(Self::Number(*value)),
            ComponentValue::Percentage(value) => Some(Self::Percent(f64::from(*value))),
            _ => None,
        }
    }

    fn value(self) -> f64 {
        match self {
            Self::Number(value) | Self::Percent(value) => value,
        }
    }

    fn is_percent(self) -> bool {
        matches!(self, Self::Percent(_))
    }

    fn alpha(self) -> u8 {
        let value = if self.is_percent() {
            self.value() / 100.0
        } else {
            self.value()
        };
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    }

    fn channel(self) -> u8 {
        let value = if self.is_percent() {
            self.value() / 100.0 * 255.0
        } else {
            self.value()
        };
        value.round().clamp(0.0, 255.0) as u8
    }
}

fn split_alpha(args: &[ComponentValue]) -> (&[ComponentValue], &[ComponentValue]) {
    for (index, component) in args.iter().enumerate() {
        if matches!(component, ComponentValue::Delim('/')) {
            return (&args[..index], &args[index + 1..]);
        }
    }
    (args, &[])
}

fn collect_numerics(components: &[ComponentValue]) -> Option<Vec<Numeric>> {
    let mut values = Vec::new();
    for component in components {
        match component {
            ComponentValue::Whitespace | ComponentValue::Comma => {}
            other => values.push(Numeric::parse(other)?),
        }
    }
    Some(values)
}

fn collect_alpha(components: &[ComponentValue]) -> Option<Option<Numeric>> {
    let values = collect_numerics(components)?;
    match values.len() {
        0 => Some(None),
        1 => Some(Some(values[0])),
        _ => None,
    }
}

fn parse_rgb_function(args: &[ComponentValue]) -> Option<Color> {
    let (main, alpha_part) = split_alpha(args);
    let mut numbers = collect_numerics(main)?;
    let mut alpha = collect_alpha(alpha_part)?;
    if numbers.len() == 4 && alpha.is_none() {
        alpha = Some(numbers.pop()?);
    }
    if numbers.len() != 3 {
        return None;
    }
    Some(Color::rgba(
        numbers[0].channel(),
        numbers[1].channel(),
        numbers[2].channel(),
        alpha.map_or(255, Numeric::alpha),
    ))
}

fn parse_hue(component: &ComponentValue) -> Option<f64> {
    match component {
        ComponentValue::Number(value) => Some(*value),
        // A percentage hue is a fraction of a full turn: 100% == 360deg.
        ComponentValue::Percentage(value) => Some(f64::from(*value) * 3.6),
        ComponentValue::Dimension(value, unit) => match unit.as_str() {
            "deg" => Some(*value),
            "turn" => Some(*value * 360.0),
            "rad" => Some(value.to_degrees()),
            "grad" => Some(*value * 0.9),
            _ => None,
        },
        _ => None,
    }
}

fn parse_hsl_function(args: &[ComponentValue]) -> Option<Color> {
    let (main, alpha_part) = split_alpha(args);
    let components: Vec<&ComponentValue> = main
        .iter()
        .filter(|component| {
            !matches!(
                component,
                ComponentValue::Whitespace | ComponentValue::Comma
            )
        })
        .collect();
    let alpha = collect_alpha(alpha_part)?;
    if components.len() == 4 && alpha.is_none() {
        let value = Numeric::parse(components[3])?;
        return hsl_to_color(
            parse_hue(components[0])?,
            Numeric::parse(components[1])?,
            Numeric::parse(components[2])?,
            Some(value),
        );
    }
    if components.len() != 3 {
        return None;
    }
    hsl_to_color(
        parse_hue(components[0])?,
        Numeric::parse(components[1])?,
        Numeric::parse(components[2])?,
        alpha,
    )
}

fn fraction(numeric: Numeric) -> f64 {
    if numeric.is_percent() || numeric.value() > 1.0 {
        numeric.value() / 100.0
    } else {
        numeric.value()
    }
}

fn hsl_to_color(
    hue: f64,
    saturation: Numeric,
    lightness: Numeric,
    alpha: Option<Numeric>,
) -> Option<Color> {
    let saturation = fraction(saturation).clamp(0.0, 1.0);
    let lightness = fraction(lightness).clamp(0.0, 1.0);
    let hue = hue.rem_euclid(360.0) / 360.0;
    let (red, green, blue) = if saturation == 0.0 {
        (lightness, lightness, lightness)
    } else {
        let q = if lightness < 0.5 {
            lightness * (1.0 + saturation)
        } else {
            lightness + saturation - lightness * saturation
        };
        let p = 2.0 * lightness - q;
        (
            hue_to_channel(p, q, hue + 1.0 / 3.0),
            hue_to_channel(p, q, hue),
            hue_to_channel(p, q, hue - 1.0 / 3.0),
        )
    };
    Some(Color::rgba(
        (red * 255.0).round().clamp(0.0, 255.0) as u8,
        (green * 255.0).round().clamp(0.0, 255.0) as u8,
        (blue * 255.0).round().clamp(0.0, 255.0) as u8,
        alpha.map_or(255, Numeric::alpha),
    ))
}

fn hue_to_channel(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

/// Parses a single component value as a color of any supported kind.
fn parse_color_component(component: &ComponentValue) -> Option<Color> {
    match component {
        ComponentValue::Ident(name) => named_color(name),
        ComponentValue::Hash(hex) => Color::from_hex(hex),
        ComponentValue::Function(name, args) => match name.as_str() {
            "rgb" | "rgba" => Color::from_rgb_function(args),
            "hsl" | "hsla" => Color::from_hsl_function(args),
            "oklch" => Color::from_oklch_function(args),
            "oklab" => Color::from_oklab_function(args),
            "lab" => Color::from_lab_function(args),
            "lch" => Color::from_lch_function(args),
            "hwb" => Color::from_hwb_function(args),
            "color" => Color::from_color_function(args),
            "color-mix" => Color::from_color_mix_function(args),
            _ => None,
        },
        _ => None,
    }
}

/// Components without separator whitespace or commas.
fn significant_components(components: &[ComponentValue]) -> Vec<&ComponentValue> {
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

/// Drops leading and trailing whitespace from a component slice.
fn trim_whitespace(mut components: &[ComponentValue]) -> &[ComponentValue] {
    while matches!(components.first(), Some(ComponentValue::Whitespace)) {
        components = &components[1..];
    }
    while matches!(components.last(), Some(ComponentValue::Whitespace)) {
        components = &components[..components.len() - 1];
    }
    components
}

/// Splits a component slice on top-level commas.
fn split_commas_slice(components: &[ComponentValue]) -> Vec<&[ComponentValue]> {
    let mut groups = Vec::new();
    let mut start = 0;
    for (index, component) in components.iter().enumerate() {
        if matches!(component, ComponentValue::Comma) {
            groups.push(trim_whitespace(&components[start..index]));
            start = index + 1;
        }
    }
    groups.push(trim_whitespace(&components[start..]));
    groups
        .into_iter()
        .filter(|group| !group.is_empty())
        .collect()
}

/// Parses a function with three numeric components and an optional alpha.
fn parse_triplet_function(
    args: &[ComponentValue],
    convert: impl Fn(Numeric, Numeric, Numeric, Option<Numeric>) -> Option<Color>,
) -> Option<Color> {
    let (main, alpha_part) = split_alpha(args);
    let components = significant_components(main);
    let alpha = collect_alpha(alpha_part)?;
    if components.len() == 4 && alpha.is_none() {
        let legacy_alpha = Numeric::parse(components[3])?;
        return convert(
            Numeric::parse(components[0])?,
            Numeric::parse(components[1])?,
            Numeric::parse(components[2])?,
            Some(legacy_alpha),
        );
    }
    if components.len() != 3 {
        return None;
    }
    convert(
        Numeric::parse(components[0])?,
        Numeric::parse(components[1])?,
        Numeric::parse(components[2])?,
        alpha,
    )
}

/// Parses a function of the form `A B H / alpha`, where the third component
/// is a hue (used by `oklch()` and `lch()`).
fn parse_hue_last_function(
    args: &[ComponentValue],
    convert: impl Fn(Numeric, Numeric, f64, Option<Numeric>) -> Option<Color>,
) -> Option<Color> {
    let (main, alpha_part) = split_alpha(args);
    let components = significant_components(main);
    let alpha = collect_alpha(alpha_part)?;
    if components.len() == 4 && alpha.is_none() {
        let legacy_alpha = Numeric::parse(components[3])?;
        return convert(
            Numeric::parse(components[0])?,
            Numeric::parse(components[1])?,
            parse_hue(components[2])?,
            Some(legacy_alpha),
        );
    }
    if components.len() != 3 {
        return None;
    }
    convert(
        Numeric::parse(components[0])?,
        Numeric::parse(components[1])?,
        parse_hue(components[2])?,
        alpha,
    )
}

/// Parses a function of the form `H A B / alpha`, where the first component
/// is a hue (used by `hwb()`).
fn parse_hue_first_function(
    args: &[ComponentValue],
    convert: impl Fn(f64, Numeric, Numeric, Option<Numeric>) -> Option<Color>,
) -> Option<Color> {
    let (main, alpha_part) = split_alpha(args);
    let components = significant_components(main);
    let alpha = collect_alpha(alpha_part)?;
    if components.len() == 4 && alpha.is_none() {
        let legacy_alpha = Numeric::parse(components[3])?;
        return convert(
            parse_hue(components[0])?,
            Numeric::parse(components[1])?,
            Numeric::parse(components[2])?,
            Some(legacy_alpha),
        );
    }
    if components.len() != 3 {
        return None;
    }
    convert(
        parse_hue(components[0])?,
        Numeric::parse(components[1])?,
        Numeric::parse(components[2])?,
        alpha,
    )
}

/// Scales an OKLab `a`/`b`/chroma component; CSS defines `100%` as `0.4`.
fn oklab_component(numeric: Numeric) -> f64 {
    if numeric.is_percent() {
        numeric.value() * 0.004
    } else {
        numeric.value()
    }
}

/// Converts a linear-light sRGB channel to an 8-bit gamma-encoded channel,
/// clamping out-of-gamut values into `0..=1`.
fn encode_srgb_channel(linear: f64) -> u8 {
    let encoded = if linear <= 0.0031308 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Inverse of the sRGB transfer function (encoded channel to linear light).
fn decode_srgb(encoded: f64) -> f64 {
    let encoded = encoded.clamp(0.0, 1.0);
    if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn channel_color(red: f64, green: f64, blue: f64, alpha: Option<Numeric>) -> Color {
    Color::rgba(
        encode_srgb_channel(red),
        encode_srgb_channel(green),
        encode_srgb_channel(blue),
        alpha.map_or(255, Numeric::alpha),
    )
}

/// The pure (fully saturated) sRGB hue for an angle in degrees.
fn hue_rgb(hue: f64) -> (f64, f64, f64) {
    let hue = hue.rem_euclid(360.0) / 360.0;
    (
        hue_to_channel(0.0, 1.0, hue + 1.0 / 3.0),
        hue_to_channel(0.0, 1.0, hue),
        hue_to_channel(0.0, 1.0, hue - 1.0 / 3.0),
    )
}

/// Converts OKLab coordinates to linear sRGB using Ottosson's matrices.
fn oklab_to_color(lightness: f64, a: f64, b: f64, alpha: Option<Numeric>) -> Color {
    let l_cube = (lightness + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_cube = (lightness - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_cube = (lightness - 0.0894841775 * a - 1.2914855480 * b).powi(3);

    let red = 4.0767416621 * l_cube - 3.3077115913 * m_cube + 0.2309699292 * s_cube;
    let green = -1.2684380046 * l_cube + 2.6097574011 * m_cube - 0.3413193965 * s_cube;
    let blue = -0.0041960863 * l_cube - 0.7034186147 * m_cube + 1.7076147010 * s_cube;
    channel_color(red, green, blue, alpha)
}

fn parse_oklch_function(args: &[ComponentValue]) -> Option<Color> {
    parse_hue_last_function(args, |lightness, chroma, hue, alpha| {
        let lightness = if lightness.is_percent() {
            lightness.value() / 100.0
        } else {
            lightness.value()
        };
        let chroma = oklab_component(chroma);
        let hue = hue.to_radians();
        Some(oklab_to_color(
            lightness,
            chroma * hue.cos(),
            chroma * hue.sin(),
            alpha,
        ))
    })
}

fn parse_oklab_function(args: &[ComponentValue]) -> Option<Color> {
    parse_triplet_function(args, |lightness, a, b, alpha| {
        let lightness = if lightness.is_percent() {
            lightness.value() / 100.0
        } else {
            lightness.value()
        };
        Some(oklab_to_color(
            lightness,
            oklab_component(a),
            oklab_component(b),
            alpha,
        ))
    })
}

/// Converts CIE L*a*b* (D50) to linear sRGB via Bradford D50->D65 adaptation.
fn lab_to_color(lightness: f64, a: f64, b: f64, alpha: Option<Numeric>) -> Color {
    const EPSILON: f64 = 216.0 / 24389.0;
    const KAPPA: f64 = 24389.0 / 27.0;
    const DELTA: f64 = 6.0 / 29.0;
    const D50_WHITE_X: f64 = 0.9642956764295677;
    const D50_WHITE_Z: f64 = 0.8251046025104602;

    let fy = (lightness + 16.0) / 116.0;
    let fx = a / 500.0 + fy;
    let fz = fy - b / 200.0;

    let x_ref = if fx.powi(3) > EPSILON {
        fx.powi(3)
    } else {
        (fx - 16.0 / 116.0) / DELTA
    };
    let y_ref = if lightness > KAPPA * EPSILON {
        fy.powi(3)
    } else {
        lightness / KAPPA
    };
    let z_ref = if fz.powi(3) > EPSILON {
        fz.powi(3)
    } else {
        (fz - 16.0 / 116.0) / DELTA
    };

    let x = x_ref * D50_WHITE_X;
    let y = y_ref;
    let z = z_ref * D50_WHITE_Z;

    // Bradford chromatic adaptation, D50 -> D65.
    let x_d65 = 0.9554734527042182 * x - 0.023098536874261423 * y + 0.0632593086610217 * z;
    let y_d65 = -0.028369706963208136 * x + 1.0099954580058226 * y + 0.021041398966943008 * z;
    let z_d65 = 0.012314001688319899 * x - 0.020507696433477912 * y + 1.330365926242124 * z;

    // XYZ (D65) -> linear sRGB.
    let red = 3.2409699419045226 * x_d65 - 1.537383177570094 * y_d65 - 0.4986107602930034 * z_d65;
    let green =
        -0.9692436362808796 * x_d65 + 1.8759675015077202 * y_d65 + 0.04155505740717559 * z_d65;
    let blue =
        0.05563007969699366 * x_d65 - 0.20397695888897652 * y_d65 + 1.0569715142428786 * z_d65;
    channel_color(red, green, blue, alpha)
}

fn parse_lab_function(args: &[ComponentValue]) -> Option<Color> {
    parse_triplet_function(args, |lightness, a, b, alpha| {
        Some(lab_to_color(lightness.value(), a.value(), b.value(), alpha))
    })
}

fn parse_lch_function(args: &[ComponentValue]) -> Option<Color> {
    parse_hue_last_function(args, |lightness, chroma, hue, alpha| {
        let hue = hue.to_radians();
        Some(lab_to_color(
            lightness.value(),
            chroma.value() * hue.cos(),
            chroma.value() * hue.sin(),
            alpha,
        ))
    })
}

fn parse_hwb_function(args: &[ComponentValue]) -> Option<Color> {
    parse_hue_first_function(args, |hue, whiteness, blackness, alpha| {
        let whiteness = fraction(whiteness).clamp(0.0, 1.0);
        let blackness = fraction(blackness).clamp(0.0, 1.0);
        let sum = whiteness + blackness;
        let (base_red, base_green, base_blue) = hue_rgb(hue);
        let (red, green, blue) = if sum >= 1.0 {
            let gray = whiteness / sum;
            (gray, gray, gray)
        } else {
            let shade = 1.0 - sum;
            (
                base_red * shade + whiteness,
                base_green * shade + whiteness,
                base_blue * shade + whiteness,
            )
        };
        Some(Color::rgba(
            (red.clamp(0.0, 1.0) * 255.0).round() as u8,
            (green.clamp(0.0, 1.0) * 255.0).round() as u8,
            (blue.clamp(0.0, 1.0) * 255.0).round() as u8,
            alpha.map_or(255, Numeric::alpha),
        ))
    })
}

fn parse_color_function(args: &[ComponentValue]) -> Option<Color> {
    let (main, alpha_part) = split_alpha(args);
    let components = significant_components(main);
    if components.len() != 4 {
        return None;
    }
    let ComponentValue::Ident(space) = components[0] else {
        return None;
    };
    let to_fraction = |numeric: Numeric| {
        if numeric.is_percent() {
            numeric.value() / 100.0
        } else {
            numeric.value()
        }
    };
    let red = to_fraction(Numeric::parse(components[1])?);
    let green = to_fraction(Numeric::parse(components[2])?);
    let blue = to_fraction(Numeric::parse(components[3])?);
    let alpha = collect_alpha(alpha_part)?;

    let (red_linear, green_linear, blue_linear) = match space.as_str() {
        "srgb" => (decode_srgb(red), decode_srgb(green), decode_srgb(blue)),
        "srgb-linear" => (red, green, blue),
        "display-p3" => {
            // Display P3 uses the sRGB transfer curve but wider primaries.
            let red_p3 = decode_srgb(red);
            let green_p3 = decode_srgb(green);
            let blue_p3 = decode_srgb(blue);
            (
                1.2249402022086958 * red_p3 - 0.22494020220869583 * green_p3,
                -0.04205695469410538 * red_p3 + 1.0420569546941055 * green_p3,
                -0.019637554587623643 * red_p3 - 0.07863604631606736 * green_p3
                    + 1.0982736009437914 * blue_p3,
            )
        }
        _ => return None,
    };
    Some(channel_color(red_linear, green_linear, blue_linear, alpha))
}

/// Parses one color argument of `color-mix()`, returning it with its optional
/// weight expressed as a `0..=1` fraction.
fn parse_mix_color(components: &[ComponentValue]) -> Option<(Color, Option<f64>)> {
    let components = trim_whitespace(components);
    let (rest, weight) = match components.last()? {
        ComponentValue::Percentage(percent) => (
            &components[..components.len() - 1],
            Some(f64::from(*percent)),
        ),
        _ => (components, None),
    };
    let rest = significant_components(rest);
    if rest.len() != 1 {
        return None;
    }
    let weight = weight.map(|percent| (percent / 100.0).clamp(0.0, 1.0));
    Some((parse_color_component(rest[0])?, weight))
}

fn parse_color_mix_function(args: &[ComponentValue]) -> Option<Color> {
    let groups = split_commas_slice(args);
    if groups.len() != 3 {
        return None;
    }

    let space_components = significant_components(groups[0]);
    if space_components.len() != 2 {
        return None;
    }
    let ComponentValue::Ident(prefix) = space_components[0] else {
        return None;
    };
    if prefix.as_str() != "in" {
        return None;
    }
    let ComponentValue::Ident(space) = space_components[1] else {
        return None;
    };
    let linear = match space.as_str() {
        "srgb" => false,
        "srgb-linear" => true,
        _ => return None,
    };

    let (first, first_weight) = parse_mix_color(groups[1])?;
    let (second, second_weight) = parse_mix_color(groups[2])?;

    let (first_weight, second_weight) = match (first_weight, second_weight) {
        (Some(first), Some(second)) => {
            let sum = first + second;
            if sum <= 0.0 {
                // CSS Color 5: mixing with two zero weights is transparent.
                return Some(Color::transparent());
            }
            (first / sum, second / sum)
        }
        (Some(first), None) => (first, 1.0 - first),
        (None, Some(second)) => (1.0 - second, second),
        (None, None) => (0.5, 0.5),
    };

    let alpha_first = f64::from(first.a) / 255.0;
    let alpha_second = f64::from(second.a) / 255.0;
    let alpha_mix = alpha_first * first_weight + alpha_second * second_weight;
    if alpha_mix <= 0.0 {
        return Some(Color::transparent());
    }

    let mix_channel = |first_channel: u8, second_channel: u8| -> u8 {
        let first_value = f64::from(first_channel) / 255.0;
        let second_value = f64::from(second_channel) / 255.0;
        let (first_value, second_value) = if linear {
            (decode_srgb(first_value), decode_srgb(second_value))
        } else {
            (first_value, second_value)
        };
        let premultiplied =
            first_value * alpha_first * first_weight + second_value * alpha_second * second_weight;
        let mixed = premultiplied / alpha_mix;
        if linear {
            encode_srgb_channel(mixed)
        } else {
            (mixed.clamp(0.0, 1.0) * 255.0).round() as u8
        }
    };

    Some(Color::rgba(
        mix_channel(first.r, second.r),
        mix_channel(first.g, second.g),
        mix_channel(first.b, second.b),
        (alpha_mix.clamp(0.0, 1.0) * 255.0).round() as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(input: &str) -> Value {
        Value::parse(input)
    }

    /// Returns the argument list of the first function in `input`.
    fn function_args(input: &str) -> Vec<ComponentValue> {
        match value(input).components.into_iter().next() {
            Some(ComponentValue::Function(_, args)) => args,
            other => panic!("expected a function, got {other:?}"),
        }
    }

    #[test]
    fn ident_is_single_lowercased_ident() {
        assert_eq!(value("  BLOCK ").ident(), Some("block"));
        assert_eq!(value("a b").ident(), None);
        assert_eq!(value("rgb(1,2,3)").ident(), None);
    }

    #[test]
    fn ident_list_collects_top_level_idents() {
        assert_eq!(value("A B  C").as_ident_list(), ["a", "b", "c"]);
        assert!(value("1px").as_ident_list().is_empty());
    }

    #[test]
    fn lengths_with_units() {
        assert_eq!(value("10px").as_length(), Some(Length::Px(10.0)));
        assert_eq!(value("-2.5em").as_length(), Some(Length::Em(-2.5)));
        assert_eq!(value("2rem").as_length(), Some(Length::Rem(2.0)));
        assert_eq!(value("50%").as_length(), Some(Length::Percent(50.0)));
        assert_eq!(value("12pt").as_length(), Some(Length::Pt(12.0)));
        assert_eq!(value("3vw").as_length(), Some(Length::Vw(3.0)));
        assert_eq!(value("4vh").as_length(), Some(Length::Vh(4.0)));
        assert_eq!(value("0").as_length(), Some(Length::Px(0.0)));
        assert_eq!(value("2").as_length(), None);
        assert_eq!(value("2ex").as_length(), None);
        assert_eq!(value("1px 2px").as_length(), None);
    }

    #[test]
    fn numbers() {
        assert_eq!(value("1.5").as_number(), Some(1.5));
        assert_eq!(value("-0.5").as_number(), Some(-0.5));
        assert_eq!(value("1px").as_number(), None);
    }

    #[test]
    fn named_colors() {
        assert_eq!(value("red").as_color(), Some(Color::rgb(255, 0, 0)));
        assert_eq!(value("GREY").as_color(), Some(Color::rgb(128, 128, 128)));
        assert_eq!(value("transparent").as_color(), Some(Color::transparent()));
        assert_eq!(value("notacolor").as_color(), None);
        assert_eq!(value("currentcolor").as_color(), None);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(value("#f00").as_color(), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            value("#ff000080").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(value("#1234").as_color(), Some(Color::rgba(17, 34, 51, 68)));
        assert_eq!(value("#12345").as_color(), None);
        assert_eq!(value("#ggg").as_color(), None);
    }

    #[test]
    fn rgb_and_rgba_functions() {
        assert_eq!(
            value("rgb(255, 0, 10)").as_color(),
            Some(Color::rgb(255, 0, 10))
        );
        assert_eq!(
            value("rgba(1, 2, 3, 0.5)").as_color(),
            Some(Color::rgba(1, 2, 3, 128))
        );
        assert_eq!(
            value("rgb(100% 0% 50%)").as_color(),
            Some(Color::rgb(255, 0, 128))
        );
        assert_eq!(
            value("rgb(255 0 0 / 50%)").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(value("rgb(1, 2)").as_color(), None);
    }

    #[test]
    fn hsl_functions_convert_to_rgb() {
        assert_eq!(
            value("hsl(0, 100%, 50%)").as_color(),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            value("hsl(120deg 100% 50%)").as_color(),
            Some(Color::rgb(0, 255, 0))
        );
        assert_eq!(
            value("hsla(240, 100%, 50%, 0.25)").as_color(),
            Some(Color::rgba(0, 0, 255, 64))
        );
        assert_eq!(
            value("hsl(0.5turn 100% 50%)").as_color(),
            Some(Color::rgb(0, 255, 255))
        );
        assert_eq!(value("hsl(0, 100%)").as_color(), None);
    }

    #[test]
    fn color_serialization() {
        assert_eq!(Color::rgb(255, 0, 0).to_css(), "#ff0000");
        assert_eq!(Color::rgba(0, 0, 0, 128).to_css(), "rgba(0, 0, 0, 0.502)");
        assert_eq!(Color::transparent().to_css(), "rgba(0, 0, 0, 0)");
        assert_eq!(Color::BLACK, Color::rgb(0, 0, 0));
        assert_eq!(Color::WHITE, Color::rgb(255, 255, 255));
    }

    #[test]
    fn strings_and_urls() {
        assert_eq!(value("\"hello\"").as_string(), Some("hello"));
        assert_eq!(value("url(a.png)").as_url(), Some("a.png"));
        assert_eq!(value("url(\"a.png\")").as_url(), Some("a.png"));
        assert_eq!(value("a.png").as_url(), None);
    }

    #[test]
    fn split_commas_trims_groups() {
        let groups = value(" Arial , \"Helvetica Neue\" , sans-serif ").split_commas();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0], vec![ComponentValue::Ident("arial".into())]);
        assert_eq!(
            groups[1],
            vec![ComponentValue::String("Helvetica Neue".into())]
        );
        // Commas inside functions do not split the value.
        assert_eq!(value("rgb(1, 2, 3), red").split_commas().len(), 2);
        assert!(value("").split_commas().is_empty());
    }

    #[test]
    fn first_skips_whitespace() {
        assert_eq!(
            value("  1px 2px").first(),
            Some(&ComponentValue::Dimension(1.0, "px".into()))
        );
        assert_eq!(value("   ").first(), None);
    }

    #[test]
    fn calc_is_preserved_as_function() {
        let parsed = value("calc(100% - 10px)");
        let Some(ComponentValue::Function(name, args)) = parsed.components.first().cloned() else {
            panic!("expected a function");
        };
        assert_eq!(name, "calc");
        assert!(args.contains(&ComponentValue::Percentage(100.0)));
        assert!(args.contains(&ComponentValue::Dimension(10.0, "px".into())));
    }

    #[test]
    fn whitespace_between_components_is_kept() {
        let parsed = value("1px   solid    red");
        assert_eq!(
            parsed
                .components
                .iter()
                .filter(|component| matches!(component, ComponentValue::Whitespace))
                .count(),
            2
        );
    }

    #[test]
    fn value_parse_matches_manual_component_conversion() {
        use crate::tokenizer::{to_component_values, tokenize};

        for input in ["red", "1px", "rgb(1, 2, 3)", "calc(100% - 10px)"] {
            assert_eq!(
                Value::parse(input),
                Value {
                    components: to_component_values(&tokenize(input)),
                },
                "{input}"
            );
        }
    }

    #[test]
    fn math_functions_are_preserved_as_functions() {
        let parsed = value("min(1px, 2px)");
        let Some(ComponentValue::Function(name, args)) = parsed.components.first().cloned() else {
            panic!("expected a function");
        };
        assert_eq!(name, "min");
        assert!(args.contains(&ComponentValue::Dimension(1.0, "px".into())));
        assert!(args.contains(&ComponentValue::Dimension(2.0, "px".into())));
        assert!(args.contains(&ComponentValue::Comma));

        let parsed = Value::parse("MAX(10%, 20px, 5em)");
        let (name, args) = parsed.as_math_function().expect("max math function");
        assert_eq!(name, "max");
        assert_eq!(
            args.iter()
                .filter(|component| **component == ComponentValue::Comma)
                .count(),
            2
        );
        assert!(args.contains(&ComponentValue::Percentage(10.0)));
        assert!(args.contains(&ComponentValue::Dimension(20.0, "px".into())));
        assert!(args.contains(&ComponentValue::Dimension(5.0, "em".into())));

        let parsed = value("clamp(1rem, 2vw + 1rem, 3rem)");
        let (name, args) = parsed.as_math_function().expect("clamp math function");
        assert_eq!(name, "clamp");
        assert_eq!(
            args.iter()
                .filter(|component| **component == ComponentValue::Comma)
                .count(),
            2
        );
        assert!(args.contains(&ComponentValue::Dimension(1.0, "rem".into())));
        assert!(args.contains(&ComponentValue::Dimension(2.0, "vw".into())));
        assert!(args.contains(&ComponentValue::Delim('+')));
        assert!(args.contains(&ComponentValue::Dimension(3.0, "rem".into())));
    }

    #[test]
    fn math_function_helpers_recognise_only_math_functions() {
        for name in ["calc", "min", "max", "clamp"] {
            let component = ComponentValue::Function(name.into(), Vec::new());
            assert!(component.is_math_function(), "{name}");
            assert_eq!(component.as_math_function().map(|(n, _)| n), Some(name));
            assert_eq!(component.as_function().map(|(n, _)| n), Some(name));
            assert!(
                Value::parse(&format!("{name}(1px)"))
                    .as_math_function()
                    .is_some(),
                "{name}"
            );
        }

        let rgb = ComponentValue::Function("rgb".into(), Vec::new());
        assert!(!rgb.is_math_function());
        assert_eq!(rgb.as_function().map(|(name, _)| name), Some("rgb"));
        assert_eq!(Value::parse("rgb(1, 2, 3)").as_math_function(), None);

        // A plain parenthesised group has an empty function name and is not a
        // math function, but `as_function` still exposes it.
        let group = ComponentValue::Function(String::new(), vec![ComponentValue::Number(1.0)]);
        assert_eq!(group.as_function().map(|(name, _)| name), Some(""));
        assert!(!group.is_math_function());

        assert!(!ComponentValue::Number(1.0).is_math_function());
        assert_eq!(ComponentValue::Number(1.0).as_function(), None);
        assert_eq!(Value::parse("1px").as_math_function(), None);
        assert_eq!(Value::parse("min(1px) max(2px)").as_math_function(), None);
    }

    #[test]
    fn calc_with_nested_parentheses_and_arithmetic_is_preserved() {
        let parsed = Value::parse("calc((100% - 10px) * 2 / 3)");
        let (name, args) = parsed.as_math_function().expect("calc");
        assert_eq!(name, "calc");

        let nested = args
            .iter()
            .find_map(ComponentValue::as_function)
            .expect("nested parenthesised group");
        assert_eq!(nested.0, "");
        assert!(nested.1.contains(&ComponentValue::Percentage(100.0)));
        assert!(nested.1.contains(&ComponentValue::Delim('-')));
        assert!(
            nested
                .1
                .contains(&ComponentValue::Dimension(10.0, "px".into()))
        );

        let operators: Vec<char> = args
            .iter()
            .filter_map(|component| match component {
                ComponentValue::Delim(character @ ('+' | '-' | '*' | '/')) => Some(*character),
                _ => None,
            })
            .collect();
        assert_eq!(operators, ['*', '/']);
        assert!(args.contains(&ComponentValue::Number(2.0)));
        assert!(args.contains(&ComponentValue::Number(3.0)));
    }

    #[test]
    fn nested_math_functions_are_preserved() {
        let parsed = Value::parse("calc(100% - min(2vw, 10px))");
        let (_, args) = parsed.as_math_function().expect("calc");
        let inner = args
            .iter()
            .find_map(ComponentValue::as_math_function)
            .expect("min");
        assert_eq!(inner.0, "min");
        assert!(
            inner
                .1
                .contains(&ComponentValue::Dimension(2.0, "vw".into()))
        );

        let parsed = Value::parse("clamp(1rem, calc(2vw + 1px), 3rem)");
        let (_, args) = parsed.as_math_function().expect("clamp");
        let inner = args
            .iter()
            .find_map(ComponentValue::as_math_function)
            .expect("calc");
        assert_eq!(inner.0, "calc");
        assert!(inner.1.contains(&ComponentValue::Delim('+')));
    }

    #[test]
    fn hex_colors_support_all_lengths_and_cases() {
        assert_eq!(Color::from_hex("#f00"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(Color::from_hex("F00"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            Color::from_hex("#ff000080"),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(
            Color::from_hex("#FF000080"),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(
            Color::from_hex("11223344"),
            Some(Color::rgba(17, 34, 51, 68))
        );
        assert_eq!(Color::from_hex("#00000000"), Some(Color::transparent()));
        assert_eq!(Color::from_hex("#12345"), None);
        assert_eq!(Color::from_hex("#gggggg"), None);

        assert_eq!(
            value("#abcd").as_color(),
            Some(Color::rgba(170, 187, 204, 221))
        );
        assert_eq!(value("#abcdef").as_color(), Some(Color::rgb(171, 205, 239)));
        assert_eq!(
            value(" #FF000080 ").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
    }

    #[test]
    fn rgb_functions_accept_legacy_and_modern_syntax() {
        let cases = [
            ("rgb(255, 0, 10)", Color::rgb(255, 0, 10)),
            ("rgba(1, 2, 3, 0.5)", Color::rgba(1, 2, 3, 128)),
            ("rgb(255 0 10)", Color::rgb(255, 0, 10)),
            ("rgb(255 0 10 / 0.5)", Color::rgba(255, 0, 10, 128)),
            ("rgba(255 0 10 / 50%)", Color::rgba(255, 0, 10, 128)),
            ("rgb(100% 0% 50%)", Color::rgb(255, 0, 128)),
            ("rgb(50% 0 0 / 25%)", Color::rgba(128, 0, 0, 64)),
            ("rgb(255, 0, 10, 0.5)", Color::rgba(255, 0, 10, 128)),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_rgb_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }

        assert_eq!(value("rgb(1, 2)").as_color(), None);
        assert_eq!(value("rgb(1 2 3 4 5)").as_color(), None);
    }

    #[test]
    fn hsl_functions_accept_legacy_and_modern_syntax() {
        let cases = [
            ("hsl(0, 100%, 50%)", Color::rgb(255, 0, 0)),
            ("hsl(120 100% 50%)", Color::rgb(0, 255, 0)),
            ("hsl(240deg, 100%, 50%)", Color::rgb(0, 0, 255)),
            ("hsla(240, 100%, 50%, 0.25)", Color::rgba(0, 0, 255, 64)),
            ("hsl(240 100% 50% / 25%)", Color::rgba(0, 0, 255, 64)),
            ("hsla(0, 100%, 50%, 50%)", Color::rgba(255, 0, 0, 128)),
            ("hsl(0, 100%, 50%, 0.5)", Color::rgba(255, 0, 0, 128)),
            ("hsl(0.5turn 100% 50% / 0.5)", Color::rgba(0, 255, 255, 128)),
            ("hsl(50% 100% 50%)", Color::rgb(0, 255, 255)),
            ("hsl(-120deg 100% 50%)", Color::rgb(0, 0, 255)),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_hsl_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }

        assert_eq!(value("hsl(0, 100%)").as_color(), None);
        assert_eq!(value("hsl(0 100% 50% / 20% / 30%)").as_color(), None);
    }

    #[test]
    fn oklch_functions_match_known_srgb_colors() {
        let cases = [
            ("oklch(0 0 0)", Color::rgb(0, 0, 0)),
            ("oklch(1 0 0)", Color::rgb(255, 255, 255)),
            ("oklch(0.627955 0.257683 29.2339)", Color::rgb(255, 0, 0)),
            ("oklch(0.86644 0.294827 142.4953)", Color::rgb(0, 255, 0)),
            ("oklch(0.452014 0.313214 264.052)", Color::rgb(0, 0, 255)),
            ("oklch(59.9871% 0 0)", Color::rgb(128, 128, 128)),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_oklch_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn oklch_supports_alpha_hue_units_and_gamut_clamping() {
        assert_eq!(
            value("oklch(0.627955 0.257683 29.2339 / 50%)").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(
            value("oklch(0.452014 0.313214 0.73348turn)").as_color(),
            Some(Color::rgb(0, 0, 255))
        );
        assert_eq!(
            value("oklch(0.627955 0.257683 0.510228rad)").as_color(),
            Some(Color::rgb(255, 0, 0))
        );
        // Chroma beyond the sRGB gamut is clamped channel by channel.
        assert_eq!(
            value("oklch(0.5 1 0)").as_color(),
            Some(Color::rgb(255, 0, 76))
        );
        assert_eq!(value("oklch(0.5 0.1)").as_color(), None);
        assert_eq!(value("oklch(0.5 0.1 20deg / 1 / 2)").as_color(), None);
    }

    #[test]
    fn oklab_functions_match_known_srgb_colors() {
        let cases = [
            ("oklab(0.627955 0.224863 0.125846)", Color::rgb(255, 0, 0)),
            ("oklab(0.86644 -0.233888 0.179498)", Color::rgb(0, 255, 0)),
            ("oklab(0.452014 -0.032457 -0.311528)", Color::rgb(0, 0, 255)),
            ("oklab(100% 0 0)", Color::rgb(255, 255, 255)),
            ("oklab(0 0 0)", Color::rgb(0, 0, 0)),
            ("oklab(59.9871% 0 0)", Color::rgb(128, 128, 128)),
            // In OKLab, 100% for a/b maps to 0.4.
            ("oklab(0.627955 56.21575% 31.4615%)", Color::rgb(255, 0, 0)),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_oklab_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }
        assert_eq!(
            value("oklab(0.627955 0.224863 0.125846 / 25%)").as_color(),
            Some(Color::rgba(255, 0, 0, 64))
        );
        assert_eq!(value("oklab(0.5 0.1)").as_color(), None);
    }

    #[test]
    fn lab_functions_match_known_srgb_colors() {
        let cases = [
            ("lab(54.291 80.805 69.891)", Color::rgb(255, 0, 0)),
            ("lab(87.819 -79.271 80.995)", Color::rgb(0, 255, 0)),
            ("lab(29.568 68.287 -112.03)", Color::rgb(0, 0, 255)),
            ("lab(100 0 0)", Color::rgb(255, 255, 255)),
            ("lab(0 0 0)", Color::rgb(0, 0, 0)),
            ("lab(53.585 0 0)", Color::rgb(128, 128, 128)),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_lab_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }
        assert_eq!(
            value("lab(54.291 80.805 69.891 / 0.5)").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(value("lab(50 0)").as_color(), None);
    }

    #[test]
    fn lch_functions_match_known_srgb_colors() {
        let cases = [
            ("lch(54.291 106.837 40.858)", Color::rgb(255, 0, 0)),
            ("lch(87.819 113.332 134.384)", Color::rgb(0, 255, 0)),
            ("lch(29.568 131.201 301.364)", Color::rgb(0, 0, 255)),
            ("lch(100 0 0)", Color::rgb(255, 255, 255)),
            ("lch(53.585 0 0)", Color::rgb(128, 128, 128)),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_lch_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }
        // Hue units all resolve to the same angle.
        assert_eq!(
            value("lch(54.291 106.837 0.113494turn)").as_color(),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            value("lch(54.291 106.837 45.398grad)").as_color(),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            value("lch(54.291 106.837 0.713139rad)").as_color(),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            value("lch(54.291 106.837 40.858 / 0.5)").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(value("lch(50 20)").as_color(), None);
    }

    #[test]
    fn hwb_functions_use_hsv_and_whiteness_blackness() {
        let cases = [
            ("hwb(0 0% 0%)", Color::rgb(255, 0, 0)),
            ("hwb(60 0% 0%)", Color::rgb(255, 255, 0)),
            ("hwb(120 0% 0%)", Color::rgb(0, 255, 0)),
            ("hwb(180 0% 0%)", Color::rgb(0, 255, 255)),
            ("hwb(240 0% 0%)", Color::rgb(0, 0, 255)),
            ("hwb(300 0% 0%)", Color::rgb(255, 0, 255)),
            ("hwb(0 50% 50%)", Color::rgb(128, 128, 128)),
            ("hwb(0 100% 0%)", Color::rgb(255, 255, 255)),
            ("hwb(0 0% 100%)", Color::rgb(0, 0, 0)),
            ("hwb(0 20% 30%)", Color::rgb(179, 51, 51)),
            // Whiteness + blackness >= 100% collapses to gray.
            ("hwb(0 60% 60%)", Color::rgb(128, 128, 128)),
            ("hwb(0.5turn 0% 0%)", Color::rgb(0, 255, 255)),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_hwb_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }
        assert_eq!(
            value("hwb(0 0 0 / 50%)").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
        assert_eq!(value("hwb(0 10%)").as_color(), None);
    }

    #[test]
    fn color_functions_support_srgb_and_display_p3() {
        let cases = [
            ("color(srgb 1 0 0)", Color::rgb(255, 0, 0)),
            ("color(srgb 0 1 0)", Color::rgb(0, 255, 0)),
            ("color(srgb 0 0 1)", Color::rgb(0, 0, 255)),
            ("color(srgb-linear 1 0 0)", Color::rgb(255, 0, 0)),
            ("color(display-p3 1 0 0)", Color::rgb(255, 0, 0)),
            ("color(display-p3 0 1 0)", Color::rgb(0, 255, 0)),
            ("color(display-p3 0 0 1)", Color::rgb(0, 0, 255)),
            ("color(display-p3 1 1 1)", Color::rgb(255, 255, 255)),
            (
                "color(display-p3 100% 0% 0% / 50%)",
                Color::rgba(255, 0, 0, 128),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_color_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }
        assert_eq!(value("color(lab 50% 0 0)").as_color(), None);
        assert_eq!(value("color(display-p3 1 0)").as_color(), None);
    }

    #[test]
    fn color_mix_interpolates_with_percentage_weights() {
        let cases = [
            ("color-mix(in srgb, red 50%, blue)", Color::rgb(128, 0, 128)),
            ("color-mix(in srgb, red, blue)", Color::rgb(128, 0, 128)),
            (
                "color-mix(in srgb, red 25%, blue 75%)",
                Color::rgb(64, 0, 191),
            ),
            (
                "color-mix(in srgb, red 80%, blue 20%)",
                Color::rgb(204, 0, 51),
            ),
            ("color-mix(in srgb, red 100%, blue)", Color::rgb(255, 0, 0)),
            (
                "color-mix(in srgb, #f00 50%, #00f)",
                Color::rgb(128, 0, 128),
            ),
            (
                "color-mix(in srgb, oklch(0.627955 0.257683 29.2339) 50%, blue)",
                Color::rgb(128, 0, 128),
            ),
            (
                "color-mix(in srgb-linear, red 50%, blue)",
                Color::rgb(188, 0, 188),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(value(input).as_color(), Some(expected), "{input}");
            assert_eq!(
                Color::from_color_mix_function(&function_args(input)),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn color_mix_premultiplies_alpha() {
        assert_eq!(
            value("color-mix(in srgb, rgb(255 0 0 / 0.5) 50%, blue)").as_color(),
            Some(Color::rgba(85, 0, 170, 192))
        );
        assert_eq!(
            value("color-mix(in srgb, transparent 50%, red)").as_color(),
            Some(Color::rgba(255, 0, 0, 128))
        );
        // Two zero weights are defined as transparent black.
        assert_eq!(
            value("color-mix(in srgb, red 0%, blue 0%)").as_color(),
            Some(Color::transparent())
        );
    }

    #[test]
    fn color_mix_rejects_unsupported_spaces_and_malformed_args() {
        assert_eq!(value("color-mix(in lab, red, blue)").as_color(), None);
        assert_eq!(value("color-mix(in srgb, red)").as_color(), None);
        assert_eq!(
            value("color-mix(in srgb, red, blue, green)").as_color(),
            None
        );
        assert_eq!(value("color-mix(in srgb, nope 50%, blue)").as_color(), None);
    }

    #[test]
    fn all_color_functions_are_reachable_from_value() {
        assert!(value("rgb(1 2 3)").as_color().is_some());
        assert!(value("hsl(0 100% 50%)").as_color().is_some());
        assert!(value("oklch(1 0 0)").as_color().is_some());
        assert!(value("oklab(1 0 0)").as_color().is_some());
        assert!(value("lab(100 0 0)").as_color().is_some());
        assert!(value("lch(100 0 0)").as_color().is_some());
        assert!(value("hwb(0 0 0)").as_color().is_some());
        assert!(value("color(srgb 1 1 1)").as_color().is_some());
        assert!(value("color-mix(in srgb, red, white)").as_color().is_some());
        assert_eq!(value("not-a-color(1 2 3)").as_color(), None);
    }
}
