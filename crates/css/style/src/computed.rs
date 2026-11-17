//! Computed style values and the typed accessors used by layout and paint.

use std::collections::BTreeMap;

use browser_css::Color;

use crate::properties::Property;

/// A horizontal or vertical box side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// Top edge.
    Top,
    /// Right edge.
    Right,
    /// Bottom edge.
    Bottom,
    /// Left edge.
    Left,
}

impl Side {
    /// All sides in CSS order (top, right, bottom, left).
    pub const ALL: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

    /// Suffix used in property names.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
        }
    }
}

/// A length that may be `auto` or a percentage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dim {
    /// `auto`
    Auto,
    /// Absolute length in CSS pixels.
    Px(f32),
    /// Percentage of the containing block.
    Percent(f32),
}

impl Dim {
    /// Resolves against `base`, returning `None` for `auto`.
    pub fn resolve(self, base: f32) -> Option<f32> {
        match self {
            Self::Auto => None,
            Self::Px(px) => Some(px),
            Self::Percent(p) => Some(base * p / 100.0),
        }
    }

    /// True for `auto`.
    pub fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }
}

/// `display` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    /// `none`
    None,
    /// `block`
    Block,
    /// `inline`
    #[default]
    Inline,
    /// `inline-block`
    InlineBlock,
    /// `flex`
    Flex,
    /// `grid`
    Grid,
    /// `table`
    Table,
    /// `table-row`
    TableRow,
    /// `table-cell`
    TableCell,
    /// `list-item`
    ListItem,
    /// `contents`
    Contents,
}

/// `position` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// `static`
    #[default]
    Static,
    /// `relative`
    Relative,
    /// `absolute`
    Absolute,
    /// `fixed`
    Fixed,
    /// `sticky`
    Sticky,
}

/// `float` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Float {
    /// `none`
    #[default]
    None,
    /// `left`
    Left,
    /// `right`
    Right,
}

/// `visibility` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// `visible`
    #[default]
    Visible,
    /// `hidden`
    Hidden,
    /// `collapse`
    Collapse,
}

/// `white-space` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhiteSpace {
    /// `normal`
    #[default]
    Normal,
    /// `nowrap`
    Nowrap,
    /// `pre`
    Pre,
    /// `pre-wrap`
    PreWrap,
    /// `pre-line`
    PreLine,
}

/// `overflow` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overflow {
    /// `visible`
    #[default]
    Visible,
    /// `hidden`
    Hidden,
    /// `scroll`
    Scroll,
    /// `auto`
    Auto,
}

/// `text-align` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    /// `left`
    #[default]
    Left,
    /// `right`
    Right,
    /// `center`
    Center,
    /// `justify`
    Justify,
}

/// `border-style` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderStyle {
    /// `none`
    #[default]
    None,
    /// `hidden`
    Hidden,
    /// `solid`
    Solid,
    /// `dashed`
    Dashed,
    /// `dotted`
    Dotted,
    /// `double`
    Double,
    /// `groove`
    Groove,
    /// `ridge`
    Ridge,
    /// `inset`
    Inset,
    /// `outset`
    Outset,
}

impl BorderStyle {
    /// True when the style paints a border.
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::None | Self::Hidden)
    }
}

/// `box-sizing` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxSizing {
    /// `content-box`
    #[default]
    ContentBox,
    /// `border-box`
    BorderBox,
}

/// `flex-direction` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexDirection {
    /// `row`
    #[default]
    Row,
    /// `row-reverse`
    RowReverse,
    /// `column`
    Column,
    /// `column-reverse`
    ColumnReverse,
}

/// `justify-content` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JustifyContent {
    /// `flex-start`
    #[default]
    FlexStart,
    /// `flex-end`
    FlexEnd,
    /// `center`
    Center,
    /// `space-between`
    SpaceBetween,
    /// `space-around`
    SpaceAround,
    /// `space-evenly`
    SpaceEvenly,
}

/// `align-items` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignItems {
    /// `stretch`
    #[default]
    Stretch,
    /// `flex-start`
    FlexStart,
    /// `flex-end`
    FlexEnd,
    /// `center`
    Center,
    /// `baseline`
    Baseline,
}

/// `line-height` values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// `normal` (1.2 × font size)
    Normal,
    /// Absolute pixels.
    Px(f32),
    /// Unitless multiplier of the font size.
    Multiplier(f32),
}

impl LineHeight {
    /// Resolves to pixels for a given font size.
    pub fn resolve(self, font_size: f32) -> f32 {
        match self {
            Self::Normal => font_size * 1.2,
            Self::Px(px) => px,
            Self::Multiplier(m) => font_size * m,
        }
    }
}

/// `text-decoration` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextDecoration {
    /// `none`
    #[default]
    None,
    /// `underline`
    Underline,
    /// `line-through`
    LineThrough,
    /// `overline`
    Overline,
}

/// A computed property value.
#[derive(Debug, Clone, PartialEq)]
pub enum ComputedValue {
    /// Length, percentage or auto.
    Dim(Dim),
    /// Absolute pixels.
    Px(f32),
    /// Unitless number.
    Number(f32),
    /// Integer (z-index, font-weight).
    Integer(i32),
    /// A colour.
    Color(Color),
    /// A lowercased keyword.
    Keyword(Box<str>),
    /// A line height.
    LineHeight(LineHeight),
    /// A list of identifiers (font families).
    IdentList(Vec<String>),
    /// Raw string (background image URL, cursor).
    Str(String),
}

impl ComputedValue {
    /// Creates a keyword value.
    pub fn keyword(value: &str) -> Self {
        Self::Keyword(value.to_ascii_lowercase().into_boxed_str())
    }

    /// The keyword text, if this is a keyword value.
    pub fn as_keyword(&self) -> Option<&str> {
        match self {
            Self::Keyword(k) => Some(k),
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The dimension, defaulting to `auto`.
    pub fn as_dim(&self) -> Dim {
        match self {
            Self::Dim(d) => *d,
            Self::Px(px) => Dim::Px(*px),
            _ => Dim::Auto,
        }
    }

    /// The pixel value.
    pub fn as_px(&self) -> f32 {
        match self {
            Self::Px(px) => *px,
            Self::Dim(Dim::Px(px)) => *px,
            Self::Number(n) => *n,
            _ => 0.0,
        }
    }

    /// The colour, defaulting to transparent.
    pub fn as_color(&self) -> Color {
        match self {
            Self::Color(c) => *c,
            _ => Color::transparent(),
        }
    }
}

/// The computed style of one element.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComputedStyle {
    values: BTreeMap<Property, ComputedValue>,
}

impl ComputedStyle {
    /// Creates a style with all properties at their initial values.
    pub fn initial() -> Self {
        let mut style = Self::default();
        for prop in Property::ALL {
            if let Some(value) = crate::properties::initial_value(*prop) {
                style.values.insert(*prop, value);
            }
        }
        style
    }

    /// Reads a raw computed value.
    pub fn get(&self, property: Property) -> Option<&ComputedValue> {
        self.values.get(&property)
    }

    /// Writes a computed value.
    pub fn set(&mut self, property: Property, value: ComputedValue) {
        self.values.insert(property, value);
    }

    /// Copies a property from another style.
    pub fn inherit_property(&mut self, property: Property, parent: &Self) {
        if let Some(value) = parent.values.get(&property).cloned() {
            self.values.insert(property, value);
        }
    }

    /// All explicitly computed properties.
    pub fn iter(&self) -> impl Iterator<Item = (&Property, &ComputedValue)> {
        self.values.iter()
    }

    /// `display`
    pub fn display(&self) -> Display {
        match self.keyword(Property::Display) {
            "none" => Display::None,
            "block" => Display::Block,
            "inline-block" => Display::InlineBlock,
            "flex" => Display::Flex,
            "grid" => Display::Grid,
            "table" => Display::Table,
            "table-row" => Display::TableRow,
            "table-cell" => Display::TableCell,
            "list-item" => Display::ListItem,
            "contents" => Display::Contents,
            _ => Display::Inline,
        }
    }

    /// True when `display: none`.
    pub fn is_display_none(&self) -> bool {
        self.display() == Display::None
    }

    /// `position`
    pub fn position(&self) -> Position {
        match self.keyword(Property::Position) {
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            "sticky" => Position::Sticky,
            _ => Position::Static,
        }
    }

    /// `float`
    pub fn float(&self) -> Float {
        match self.keyword(Property::Float) {
            "left" => Float::Left,
            "right" => Float::Right,
            _ => Float::None,
        }
    }

    /// `visibility`
    pub fn visibility(&self) -> Visibility {
        match self.keyword(Property::Visibility) {
            "hidden" => Visibility::Hidden,
            "collapse" => Visibility::Collapse,
            _ => Visibility::Visible,
        }
    }

    /// `white-space`
    pub fn white_space(&self) -> WhiteSpace {
        match self.keyword(Property::WhiteSpace) {
            "nowrap" => WhiteSpace::Nowrap,
            "pre" => WhiteSpace::Pre,
            "pre-wrap" => WhiteSpace::PreWrap,
            "pre-line" => WhiteSpace::PreLine,
            _ => WhiteSpace::Normal,
        }
    }

    /// `overflow-x`
    pub fn overflow_x(&self) -> Overflow {
        keyword_overflow(self.keyword(Property::OverflowX))
    }

    /// `overflow-y`
    pub fn overflow_y(&self) -> Overflow {
        keyword_overflow(self.keyword(Property::OverflowY))
    }

    /// `text-align`
    pub fn text_align(&self) -> TextAlign {
        match self.keyword(Property::TextAlign) {
            "right" | "end" => TextAlign::Right,
            "center" => TextAlign::Center,
            "justify" => TextAlign::Justify,
            _ => TextAlign::Left,
        }
    }

    /// `box-sizing`
    pub fn box_sizing(&self) -> BoxSizing {
        match self.keyword(Property::BoxSizing) {
            "border-box" => BoxSizing::BorderBox,
            _ => BoxSizing::ContentBox,
        }
    }

    /// `flex-direction`
    pub fn flex_direction(&self) -> FlexDirection {
        match self.keyword(Property::FlexDirection) {
            "row-reverse" => FlexDirection::RowReverse,
            "column" => FlexDirection::Column,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => FlexDirection::Row,
        }
    }

    /// `justify-content`
    pub fn justify_content(&self) -> JustifyContent {
        match self.keyword(Property::JustifyContent) {
            "flex-end" => JustifyContent::FlexEnd,
            "center" => JustifyContent::Center,
            "space-between" => JustifyContent::SpaceBetween,
            "space-around" => JustifyContent::SpaceAround,
            "space-evenly" => JustifyContent::SpaceEvenly,
            _ => JustifyContent::FlexStart,
        }
    }

    /// `align-items`
    pub fn align_items(&self) -> AlignItems {
        match self.keyword(Property::AlignItems) {
            "flex-start" => AlignItems::FlexStart,
            "flex-end" => AlignItems::FlexEnd,
            "center" => AlignItems::Center,
            "baseline" => AlignItems::Baseline,
            _ => AlignItems::Stretch,
        }
    }

    /// `text-decoration`
    pub fn text_decoration(&self) -> TextDecoration {
        match self.keyword(Property::TextDecoration) {
            "underline" => TextDecoration::Underline,
            "line-through" => TextDecoration::LineThrough,
            "overline" => TextDecoration::Overline,
            _ => TextDecoration::None,
        }
    }

    /// `width` or a side property by name.
    pub fn dim(&self, property: Property) -> Dim {
        match self.values.get(&property) {
            Some(value) => value.as_dim(),
            None => Dim::Auto,
        }
    }

    /// Width.
    pub fn width(&self) -> Dim {
        self.dim(Property::Width)
    }

    /// Height.
    pub fn height(&self) -> Dim {
        self.dim(Property::Height)
    }

    /// Margin for a side.
    pub fn margin(&self, side: Side) -> Dim {
        self.dim(margin_property(side))
    }

    /// Padding for a side.
    pub fn padding(&self, side: Side) -> Dim {
        self.dim(padding_property(side))
    }

    /// Border width for a side (0 when the style is not visible).
    pub fn border_width(&self, side: Side) -> f32 {
        let style = self.border_style(side);
        if style.is_visible() {
            self.values
                .get(&border_width_property(side))
                .map_or(0.0, ComputedValue::as_px)
        } else {
            0.0
        }
    }

    /// Border style for a side.
    pub fn border_style(&self, side: Side) -> BorderStyle {
        match self.keyword(border_style_property(side)) {
            "hidden" => BorderStyle::Hidden,
            "solid" => BorderStyle::Solid,
            "dashed" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            "double" => BorderStyle::Double,
            "groove" => BorderStyle::Groove,
            "ridge" => BorderStyle::Ridge,
            "inset" => BorderStyle::Inset,
            "outset" => BorderStyle::Outset,
            _ => BorderStyle::None,
        }
    }

    /// Border colour for a side.
    pub fn border_color(&self, side: Side) -> Color {
        self.values
            .get(&border_color_property(side))
            .map_or_else(Color::transparent, ComputedValue::as_color)
    }

    /// Text colour.
    pub fn color(&self) -> Color {
        self.values
            .get(&Property::Color)
            .map_or_else(|| Color::rgb(0, 0, 0), ComputedValue::as_color)
    }

    /// Background colour.
    pub fn background_color(&self) -> Color {
        self.values
            .get(&Property::BackgroundColor)
            .map_or_else(Color::transparent, ComputedValue::as_color)
    }

    /// Background image URL, when set.
    pub fn background_image(&self) -> Option<&str> {
        match self.values.get(&Property::BackgroundImage) {
            Some(ComputedValue::Str(url)) if !url.is_empty() && url != "none" => Some(url),
            _ => None,
        }
    }

    /// Font size in pixels.
    pub fn font_size(&self) -> f32 {
        self.values
            .get(&Property::FontSize)
            .map_or(16.0, ComputedValue::as_px)
    }

    /// Font weight (100..=900).
    pub fn font_weight(&self) -> u16 {
        match self.values.get(&Property::FontWeight) {
            Some(ComputedValue::Integer(w)) => (*w).clamp(100, 900) as u16,
            Some(ComputedValue::Number(n)) => (*n as u16).clamp(100, 900),
            _ => 400,
        }
    }

    /// True when italic.
    pub fn is_italic(&self) -> bool {
        matches!(self.keyword(Property::FontStyle), "italic" | "oblique")
    }

    /// True when the font is monospace.
    pub fn is_monospace(&self) -> bool {
        self.font_family().contains("mono")
    }

    /// First font family name.
    pub fn font_family(&self) -> &str {
        match self.values.get(&Property::FontFamily) {
            Some(ComputedValue::IdentList(list)) => {
                list.first().map_or("sans-serif", String::as_str)
            }
            Some(ComputedValue::Str(s)) => s,
            _ => "sans-serif",
        }
    }

    /// Line height.
    pub fn line_height(&self) -> LineHeight {
        match self.values.get(&Property::LineHeight) {
            Some(ComputedValue::LineHeight(lh)) => *lh,
            Some(ComputedValue::Number(n)) => LineHeight::Multiplier(*n),
            Some(ComputedValue::Px(px)) => LineHeight::Px(*px),
            _ => LineHeight::Normal,
        }
    }

    /// Resolved line height in pixels.
    pub fn line_height_px(&self) -> f32 {
        self.line_height().resolve(self.font_size())
    }

    /// `z-index`.
    pub fn z_index(&self) -> i32 {
        match self.values.get(&Property::ZIndex) {
            Some(ComputedValue::Integer(i)) => *i,
            _ => 0,
        }
    }

    /// `opacity` in 0..=1.
    pub fn opacity(&self) -> f32 {
        match self.values.get(&Property::Opacity) {
            Some(value) => value.as_px().clamp(0.0, 1.0),
            None => 1.0,
        }
    }

    /// `flex-grow`.
    pub fn flex_grow(&self) -> f32 {
        self.values
            .get(&Property::FlexGrow)
            .map_or(0.0, ComputedValue::as_px)
    }

    /// `flex-shrink`.
    pub fn flex_shrink(&self) -> f32 {
        self.values
            .get(&Property::FlexShrink)
            .map_or(1.0, ComputedValue::as_px)
    }

    /// `flex-basis`.
    pub fn flex_basis(&self) -> Dim {
        self.dim(Property::FlexBasis)
    }

    /// Row gap in pixels.
    pub fn row_gap(&self) -> f32 {
        self.values
            .get(&Property::RowGap)
            .map_or(0.0, ComputedValue::as_px)
    }

    /// Column gap in pixels.
    pub fn column_gap(&self) -> f32 {
        self.values
            .get(&Property::ColumnGap)
            .map_or(0.0, ComputedValue::as_px)
    }

    /// `grid-template-columns` as raw text, or `None` when `none`.
    pub fn grid_template_columns(&self) -> Option<String> {
        match self.values.get(&Property::GridTemplateColumns) {
            Some(ComputedValue::Str(value)) if !value.is_empty() && value != "none" => {
                Some(value.clone())
            }
            _ => None,
        }
    }

    /// `grid-template-rows` as raw text, or `None` when `none`.
    pub fn grid_template_rows(&self) -> Option<String> {
        match self.values.get(&Property::GridTemplateRows) {
            Some(ComputedValue::Str(value)) if !value.is_empty() && value != "none" => {
                Some(value.clone())
            }
            _ => None,
        }
    }

    /// `letter-spacing` in pixels.
    pub fn letter_spacing(&self) -> f32 {
        self.values
            .get(&Property::LetterSpacing)
            .map_or(0.0, ComputedValue::as_px)
    }

    fn keyword(&self, property: Property) -> &str {
        match self.values.get(&property) {
            Some(ComputedValue::Keyword(k)) => k,
            Some(ComputedValue::Str(s)) => s,
            _ => "",
        }
    }
}

fn keyword_overflow(keyword: &str) -> Overflow {
    match keyword {
        "hidden" => Overflow::Hidden,
        "scroll" => Overflow::Scroll,
        "auto" => Overflow::Auto,
        _ => Overflow::Visible,
    }
}

/// Margin property for a side.
pub fn margin_property(side: Side) -> Property {
    match side {
        Side::Top => Property::MarginTop,
        Side::Right => Property::MarginRight,
        Side::Bottom => Property::MarginBottom,
        Side::Left => Property::MarginLeft,
    }
}

/// Padding property for a side.
pub fn padding_property(side: Side) -> Property {
    match side {
        Side::Top => Property::PaddingTop,
        Side::Right => Property::PaddingRight,
        Side::Bottom => Property::PaddingBottom,
        Side::Left => Property::PaddingLeft,
    }
}

/// Border width property for a side.
pub fn border_width_property(side: Side) -> Property {
    match side {
        Side::Top => Property::BorderTopWidth,
        Side::Right => Property::BorderRightWidth,
        Side::Bottom => Property::BorderBottomWidth,
        Side::Left => Property::BorderLeftWidth,
    }
}

/// Border style property for a side.
pub fn border_style_property(side: Side) -> Property {
    match side {
        Side::Top => Property::BorderTopStyle,
        Side::Right => Property::BorderRightStyle,
        Side::Bottom => Property::BorderBottomStyle,
        Side::Left => Property::BorderLeftStyle,
    }
}

/// Border colour property for a side.
pub fn border_color_property(side: Side) -> Property {
    match side {
        Side::Top => Property::BorderTopColor,
        Side::Right => Property::BorderRightColor,
        Side::Bottom => Property::BorderBottomColor,
        Side::Left => Property::BorderLeftColor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_values_are_sensible() {
        let s = ComputedStyle::initial();
        assert_eq!(s.display(), Display::Inline);
        assert_eq!(s.width(), Dim::Auto);
        assert_eq!(s.font_size(), 16.0);
        assert_eq!(s.color(), Color::rgb(0, 0, 0));
        assert_eq!(s.margin(Side::Top), Dim::Px(0.0));
        assert_eq!(s.line_height_px(), 19.2);
        assert_eq!(s.opacity(), 1.0);
    }

    #[test]
    fn dims_resolve_against_base() {
        assert_eq!(Dim::Px(10.0).resolve(100.0), Some(10.0));
        assert_eq!(Dim::Percent(50.0).resolve(100.0), Some(50.0));
        assert!(Dim::Auto.resolve(100.0).is_none());
    }

    #[test]
    fn keyword_getters_map_values() {
        let mut s = ComputedStyle::initial();
        s.set(Property::Display, ComputedValue::keyword("flex"));
        s.set(Property::Position, ComputedValue::keyword("absolute"));
        s.set(Property::OverflowY, ComputedValue::keyword("scroll"));
        s.set(
            Property::JustifyContent,
            ComputedValue::keyword("space-between"),
        );
        assert_eq!(s.display(), Display::Flex);
        assert_eq!(s.position(), Position::Absolute);
        assert_eq!(s.overflow_y(), Overflow::Scroll);
        assert_eq!(s.justify_content(), JustifyContent::SpaceBetween);
    }
}
