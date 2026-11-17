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

    /// The value as a single color.
    #[must_use]
    pub fn as_color(&self) -> Option<Color> {
        match self.single()? {
            ComponentValue::Ident(name) => named_color(name),
            ComponentValue::Hash(hex) => parse_hex_color(hex),
            ComponentValue::Function(name, args) => match name.as_str() {
                "rgb" | "rgba" => parse_rgb_function(args),
                "hsl" | "hsla" => parse_hsl_function(args),
                _ => None,
            },
            _ => None,
        }
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
        ComponentValue::Percentage(value) => Some(f64::from(*value)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::{to_component_values, tokenize};

    fn value(input: &str) -> Value {
        Value {
            components: to_component_values(&tokenize(input)),
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
}
