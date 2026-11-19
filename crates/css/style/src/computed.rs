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
    /// A `calc()` expression mixing a percentage and absolute pixels, e.g.
    /// `calc(100% - 20px)`. Resolved by layout against the containing block.
    Calc {
        /// Percentage coefficient.
        percent: f32,
        /// Absolute pixel offset.
        px: f32,
    },
}

impl Dim {
    /// Resolves against `base`, returning `None` for `auto`.
    pub fn resolve(self, base: f32) -> Option<f32> {
        match self {
            Self::Auto => None,
            Self::Px(px) => Some(px),
            Self::Percent(p) => Some(base * p / 100.0),
            Self::Calc { percent, px } => Some(base * percent / 100.0 + px),
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
    /// `inline-flex`
    InlineFlex,
    /// `grid`
    Grid,
    /// `inline-grid`
    InlineGrid,
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

impl Display {
    /// True when this display value establishes a flex formatting context.
    ///
    /// Both `display: flex` and `display: inline-flex` make the element a flex
    /// container; they differ only in the element's own outer display type.
    pub fn is_flex_container(self) -> bool {
        matches!(self, Self::Flex | Self::InlineFlex)
    }

    /// True when this display value establishes a grid formatting context.
    ///
    /// Both `display: grid` and `display: inline-grid` make the element a grid
    /// container; they differ only in the element's own outer display type.
    pub fn is_grid_container(self) -> bool {
        matches!(self, Self::Grid | Self::InlineGrid)
    }

    /// True for inline-level display values, including the inline variants of
    /// flex and grid containers.
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Self::Inline | Self::InlineBlock | Self::InlineFlex | Self::InlineGrid
        )
    }
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

/// `flex-wrap` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexWrap {
    /// `nowrap`
    #[default]
    NoWrap,
    /// `wrap`
    Wrap,
    /// `wrap-reverse`
    WrapReverse,
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

/// One corner radius: a horizontal radius and an optional vertical radius.
///
/// When `vertical` is `None` the vertical radius equals the horizontal one,
/// which is what CSS means when only one value is supplied for a corner.
/// Percentages are kept as `Dim::Percent` so layout can resolve them against
/// the box dimensions (usually the smaller of width and height).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CornerRadius {
    /// Horizontal radius.
    pub horizontal: Dim,
    /// Vertical radius, or `None` when it equals `horizontal`.
    pub vertical: Option<Dim>,
}

impl CornerRadius {
    /// A square (zero-radius) corner.
    pub const ZERO: Self = Self {
        horizontal: Dim::Px(0.0),
        vertical: None,
    };
}

/// The four corner radii in CSS order: top-left, top-right, bottom-right,
/// bottom-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderRadius {
    /// Corner radii, in top-left, top-right, bottom-right, bottom-left order.
    pub corners: [CornerRadius; 4],
}

impl Default for BorderRadius {
    fn default() -> Self {
        Self {
            corners: [CornerRadius::ZERO; 4],
        }
    }
}

/// One shadow layer, shared by `box-shadow` and `text-shadow`.
#[derive(Debug, Clone, PartialEq)]
pub struct Shadow {
    /// Horizontal offset in px (may be negative).
    pub offset_x: f32,
    /// Vertical offset in px (may be negative).
    pub offset_y: f32,
    /// Blur radius in px (0 when omitted).
    pub blur: f32,
    /// Spread radius in px (0 when omitted; always 0 for text shadows).
    pub spread: f32,
    /// Shadow colour. Opaque black when the declaration omits it (CSS
    /// `currentColor` cannot be resolved at compute time).
    pub color: Color,
    /// `inset` keyword (box shadows only; always false for text shadows).
    pub inset: bool,
}

/// A list of shadow layers (`box-shadow` or `text-shadow`).
pub type ShadowList = Vec<Shadow>;

/// A single transform function with resolved arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum TransformFunction {
    /// `translate(x, y)`; percentages stay as `Dim::Percent` for layout to resolve.
    Translate(Dim, Dim),
    /// `translateX(x)`.
    TranslateX(Dim),
    /// `translateY(y)`.
    TranslateY(Dim),
    /// `scale(x, y)`.
    Scale(f32, f32),
    /// `scaleX(x)`.
    ScaleX(f32),
    /// `scaleY(y)`.
    ScaleY(f32),
    /// `rotate(a)`, in degrees.
    Rotate(f32),
    /// `skew(x, y)`, in degrees.
    Skew(f32, f32),
    /// `matrix(a, b, c, d, e, f)`.
    Matrix([f32; 6]),
    /// Any other transform function (e.g. `perspective(...)`), kept as raw text.
    Raw(String),
}

/// A list of transform functions, applied in order (left to right).
pub type TransformList = Vec<TransformFunction>;

/// A single filter function with resolved arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterFunction {
    /// `blur(<length>)` in px.
    Blur(f32),
    /// `brightness(<number|percentage>)`.
    Brightness(f32),
    /// `contrast(<number|percentage>)`.
    Contrast(f32),
    /// `grayscale(<number|percentage>)`.
    Grayscale(f32),
    /// `hue-rotate(<angle>)`, in degrees.
    HueRotate(f32),
    /// `invert(<number|percentage>)`.
    Invert(f32),
    /// `opacity(<number|percentage>)`.
    Opacity(f32),
    /// `saturate(<number|percentage>)`.
    Saturate(f32),
    /// `sepia(<number|percentage>)`.
    Sepia(f32),
    /// Any other filter function (e.g. `url(...)`), kept as raw text.
    Raw(String),
}

/// A list of filter functions, applied in order (left to right).
pub type FilterList = Vec<FilterFunction>;

/// The value of `aspect-ratio`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AspectRatio {
    /// `auto`: no preferred ratio.
    Auto,
    /// A width / height ratio (e.g. `16 / 9`).
    Ratio(f32, f32),
}

/// The value of `object-fit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectFit {
    /// Stretch the content to fill the box (initial value).
    #[default]
    Fill,
    /// Scale to fit inside the box, preserving the ratio.
    Contain,
    /// Scale to cover the box, preserving the ratio.
    Cover,
    /// Render at its natural size.
    None,
    /// Like `contain`, but never scale up.
    ScaleDown,
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
    /// `border-radius`.
    BorderRadius(BorderRadius),
    /// `box-shadow` or `text-shadow` (the owning property identifies which).
    Shadows(ShadowList),
    /// `transform`.
    Transform(TransformList),
    /// `aspect-ratio`.
    AspectRatio(AspectRatio),
    /// `object-fit`.
    ObjectFit(ObjectFit),
    /// `filter`.
    Filter(FilterList),
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
            "inline-flex" => Display::InlineFlex,
            "grid" => Display::Grid,
            "inline-grid" => Display::InlineGrid,
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

    /// `flex-wrap`
    pub fn flex_wrap(&self) -> FlexWrap {
        match self.keyword(Property::FlexWrap) {
            "wrap" => FlexWrap::Wrap,
            "wrap-reverse" => FlexWrap::WrapReverse,
            _ => FlexWrap::NoWrap,
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

    /// Raw background-image text when set (an image URL, or a gradient such
    /// as `linear-gradient(...)`, or a comma-joined list of layers).
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

    /// Reads a raw-text property, mapping the unset/initial keyword to `None`.
    ///
    /// Grid placement and alignment grammars are parsed by the layout engine,
    /// so the computed style stores them verbatim. Returning `None` for the
    /// initial value mirrors "no declaration" and lets layout apply its own
    /// defaults without having to special-case initial keywords.
    fn raw_text(&self, property: Property, initial: &str) -> Option<String> {
        match self
            .values
            .get(&property)
            .and_then(ComputedValue::as_keyword)
        {
            Some(text) if !text.is_empty() && text != initial => Some(text.to_string()),
            _ => None,
        }
    }

    /// `grid-template-areas` as raw text (quoted rows), or `None` when `none`.
    pub fn grid_template_areas(&self) -> Option<String> {
        self.raw_text(Property::GridTemplateAreas, "none")
    }

    /// `grid-column` as raw text (e.g. `span 2`, `1 / 3`), or `None` for `auto`.
    pub fn grid_column(&self) -> Option<String> {
        self.raw_text(Property::GridColumn, "auto")
    }

    /// `grid-row` as raw text (e.g. `span 2`, `1 / 3`), or `None` for `auto`.
    pub fn grid_row(&self) -> Option<String> {
        self.raw_text(Property::GridRow, "auto")
    }

    /// `grid-area` as raw text (a name or `row / col / row / col`), or `None`
    /// for `auto`.
    pub fn grid_area(&self) -> Option<String> {
        self.raw_text(Property::GridArea, "auto")
    }

    /// `grid-auto-rows` as raw text, or `None` for `auto`.
    pub fn grid_auto_rows(&self) -> Option<String> {
        self.raw_text(Property::GridAutoRows, "auto")
    }

    /// `grid-auto-columns` as raw text, or `None` for `auto`.
    pub fn grid_auto_columns(&self) -> Option<String> {
        self.raw_text(Property::GridAutoColumns, "auto")
    }

    /// `grid-auto-flow` as raw text (e.g. `column`), or `None` for `row`.
    pub fn grid_auto_flow(&self) -> Option<String> {
        self.raw_text(Property::GridAutoFlow, "row")
    }

    /// `justify-items` as raw text (e.g. `center`), or `None` for `normal`.
    pub fn justify_items(&self) -> Option<String> {
        self.raw_text(Property::JustifyItems, "normal")
    }

    /// `justify-self` as raw text (e.g. `end`), or `None` for `auto`.
    pub fn justify_self(&self) -> Option<String> {
        self.raw_text(Property::JustifySelf, "auto")
    }

    /// `align-self` as raw text (e.g. `start`), or `None` for `auto`.
    ///
    /// Unlike `align-items`, this stays raw so layout-specific keywords such
    /// as `start` and `end` are preserved verbatim.
    pub fn align_self(&self) -> Option<String> {
        self.raw_text(Property::AlignSelf, "auto")
    }

    /// `align-content` as raw text (e.g. `space-between`), or `None` for
    /// `normal`.
    pub fn align_content(&self) -> Option<String> {
        self.raw_text(Property::AlignContent, "normal")
    }

    /// `justify-content` as raw text, or `None` for `flex-start`.
    ///
    /// The typed [`Self::justify_content`] accessor is preferred for flexbox;
    /// this one is for grid content distribution helpers that need the exact
    /// declared keyword.
    pub fn justify_content_raw(&self) -> Option<String> {
        self.raw_text(Property::JustifyContent, "flex-start")
    }

    /// `letter-spacing` in pixels.
    pub fn letter_spacing(&self) -> f32 {
        self.values
            .get(&Property::LetterSpacing)
            .map_or(0.0, ComputedValue::as_px)
    }

    /// `border-radius` as four raw numbers in CSS corner order (top-left,
    /// top-right, bottom-right, bottom-left).
    ///
    /// A length is returned in px; a percentage is returned as its raw number
    /// (for example `50.0` for `50%`). The caller must resolve percentages
    /// against the box size, which is not available at compute time.
    pub fn border_radius(&self) -> [f32; 4] {
        self.border_radius_corners()
            .corners
            .map(|corner| match corner.horizontal {
                Dim::Px(px) => px,
                Dim::Percent(percent) => percent,
                Dim::Calc { percent, px } => percent + px,
                Dim::Auto => 0.0,
            })
    }

    /// `border-radius` with the full horizontal/vertical radii per corner.
    pub fn border_radius_corners(&self) -> BorderRadius {
        match self.values.get(&Property::BorderRadius) {
            Some(ComputedValue::BorderRadius(radius)) => *radius,
            _ => BorderRadius::default(),
        }
    }

    /// `box-shadow` layers, or an empty slice when there is no shadow.
    pub fn box_shadow_raw(&self) -> &[Shadow] {
        match self.values.get(&Property::BoxShadow) {
            Some(ComputedValue::Shadows(shadows)) => shadows,
            _ => &[],
        }
    }

    /// `text-shadow` layers, or an empty slice when there is no shadow.
    pub fn text_shadow_raw(&self) -> &[Shadow] {
        match self.values.get(&Property::TextShadow) {
            Some(ComputedValue::Shadows(shadows)) => shadows,
            _ => &[],
        }
    }

    /// `transform` functions in application order, or an empty slice.
    pub fn transform_raw(&self) -> &[TransformFunction] {
        match self.values.get(&Property::Transform) {
            Some(ComputedValue::Transform(transform)) => transform,
            _ => &[],
        }
    }

    /// `filter` functions in application order, or an empty slice.
    pub fn filter_raw(&self) -> &[FilterFunction] {
        match self.values.get(&Property::Filter) {
            Some(ComputedValue::Filter(filter)) => filter,
            _ => &[],
        }
    }

    /// `aspect-ratio`.
    pub fn aspect_ratio(&self) -> AspectRatio {
        match self.values.get(&Property::AspectRatio) {
            Some(ComputedValue::AspectRatio(ratio)) => *ratio,
            _ => AspectRatio::Auto,
        }
    }

    /// `object-fit`.
    pub fn object_fit(&self) -> ObjectFit {
        match self.values.get(&Property::ObjectFit) {
            Some(ComputedValue::ObjectFit(fit)) => *fit,
            _ => ObjectFit::default(),
        }
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

    #[test]
    fn inline_flex_and_grid_map_to_container_variants() {
        let display = |keyword: &str| {
            let mut s = ComputedStyle::initial();
            s.set(Property::Display, ComputedValue::keyword(keyword));
            s.display()
        };

        assert_eq!(display("inline-flex"), Display::InlineFlex);
        assert_eq!(display("inline-grid"), Display::InlineGrid);

        // Both inline and block flavours remain flex/grid containers.
        for container in [
            Display::Flex,
            Display::InlineFlex,
            Display::Grid,
            Display::InlineGrid,
        ] {
            assert_eq!(
                container.is_flex_container(),
                matches!(container, Display::Flex | Display::InlineFlex)
            );
            assert_eq!(
                container.is_grid_container(),
                matches!(container, Display::Grid | Display::InlineGrid)
            );
        }

        // Only the inline flavours are inline-level.
        assert!(Display::InlineFlex.is_inline_level());
        assert!(Display::InlineGrid.is_inline_level());
        assert!(!Display::Flex.is_inline_level());
        assert!(!Display::Grid.is_inline_level());
    }

    #[test]
    fn modern_property_accessors_have_safe_defaults() {
        let s = ComputedStyle::initial();
        assert_eq!(s.border_radius(), [0.0; 4]);
        assert!(s.box_shadow_raw().is_empty());
        assert!(s.text_shadow_raw().is_empty());
        assert!(s.transform_raw().is_empty());
        assert!(s.filter_raw().is_empty());
        assert_eq!(s.aspect_ratio(), AspectRatio::Auto);
        assert_eq!(s.object_fit(), ObjectFit::Fill);

        // Defaults are also returned for an entirely empty style.
        let empty = ComputedStyle::default();
        assert_eq!(empty.border_radius(), [0.0; 4]);
        assert_eq!(empty.object_fit(), ObjectFit::Fill);
    }

    #[test]
    fn modern_property_accessors_read_computed_values() {
        let mut s = ComputedStyle::default();
        s.set(
            Property::BorderRadius,
            ComputedValue::BorderRadius(BorderRadius {
                corners: [
                    CornerRadius {
                        horizontal: Dim::Px(4.0),
                        vertical: Some(Dim::Percent(10.0)),
                    },
                    CornerRadius::ZERO,
                    CornerRadius::ZERO,
                    CornerRadius::ZERO,
                ],
            }),
        );
        assert_eq!(s.border_radius(), [4.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            s.border_radius_corners().corners[0].vertical,
            Some(Dim::Percent(10.0))
        );

        s.set(
            Property::BoxShadow,
            ComputedValue::Shadows(vec![Shadow {
                offset_x: 1.0,
                offset_y: 2.0,
                blur: 3.0,
                spread: 0.0,
                color: Color::rgb(0, 0, 0),
                inset: false,
            }]),
        );
        assert_eq!(s.box_shadow_raw()[0].blur, 3.0);
    }

    #[test]
    fn grid_accessors_default_to_none() {
        let s = ComputedStyle::initial();
        assert_eq!(s.grid_template_areas(), None);
        assert_eq!(s.grid_column(), None);
        assert_eq!(s.grid_row(), None);
        assert_eq!(s.grid_area(), None);
        assert_eq!(s.grid_auto_rows(), None);
        assert_eq!(s.grid_auto_columns(), None);
        assert_eq!(s.grid_auto_flow(), None);
        assert_eq!(s.justify_items(), None);
        assert_eq!(s.justify_self(), None);
        assert_eq!(s.align_self(), None);
        assert_eq!(s.align_content(), None);
        assert_eq!(s.justify_content_raw(), None);

        // An entirely empty style behaves the same way.
        let empty = ComputedStyle::default();
        assert_eq!(empty.grid_column(), None);
        assert_eq!(empty.justify_self(), None);
    }

    #[test]
    fn grid_accessors_read_raw_values() {
        let mut s = ComputedStyle::default();
        let raw = |text: &str| ComputedValue::Str(text.to_string());
        s.set(Property::GridColumn, raw("span 2"));
        s.set(Property::GridRow, raw("1 / 3"));
        s.set(Property::GridArea, raw("head"));
        s.set(
            Property::GridTemplateAreas,
            raw("\"head head\" \"side main\""),
        );
        s.set(Property::GridAutoRows, raw("25px"));
        s.set(Property::GridAutoColumns, raw("10px"));
        s.set(Property::GridAutoFlow, raw("column"));
        s.set(Property::JustifyItems, raw("center"));
        s.set(Property::JustifySelf, raw("end"));
        s.set(Property::AlignSelf, raw("start"));
        s.set(Property::AlignContent, raw("space-between"));
        s.set(Property::JustifyContent, raw("center"));

        assert_eq!(s.grid_column().as_deref(), Some("span 2"));
        assert_eq!(s.grid_row().as_deref(), Some("1 / 3"));
        assert_eq!(s.grid_area().as_deref(), Some("head"));
        assert_eq!(
            s.grid_template_areas().as_deref(),
            Some("\"head head\" \"side main\"")
        );
        assert_eq!(s.grid_auto_rows().as_deref(), Some("25px"));
        assert_eq!(s.grid_auto_columns().as_deref(), Some("10px"));
        assert_eq!(s.grid_auto_flow().as_deref(), Some("column"));
        assert_eq!(s.justify_items().as_deref(), Some("center"));
        assert_eq!(s.justify_self().as_deref(), Some("end"));
        assert_eq!(s.align_self().as_deref(), Some("start"));
        assert_eq!(s.align_content().as_deref(), Some("space-between"));
        assert_eq!(s.justify_content_raw().as_deref(), Some("center"));
    }

    #[test]
    fn stylesheet_grid_properties_survive_compute() {
        use browser_dom::Document;

        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let a = doc.create_element("div");
        doc.set_attr(a, "id", "a");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, a);

        let mut engine = crate::StyleEngine::new(crate::Viewport::default());
        engine.add_css(
            "#a {
                grid-column: span 2;
                grid-row: 1 / 3;
                grid-area: main;
                grid-template-areas: \"head head\" \"side main\";
                grid-auto-rows: 25px;
                grid-auto-columns: 10px;
                grid-auto-flow: column;
                justify-items: center;
                justify-self: center;
                align-self: end;
                align-content: space-between;
            }",
            crate::Origin::Author,
        );
        let styled = engine.compute(&doc);
        let style = styled.get(a).unwrap();

        assert_eq!(style.grid_column().as_deref(), Some("span 2"));
        assert_eq!(style.grid_row().as_deref(), Some("1 / 3"));
        assert_eq!(style.grid_area().as_deref(), Some("main"));
        assert_eq!(
            style.grid_template_areas().as_deref(),
            Some("\"head head\" \"side main\"")
        );
        assert_eq!(style.grid_auto_rows().as_deref(), Some("25px"));
        assert_eq!(style.grid_auto_columns().as_deref(), Some("10px"));
        assert_eq!(style.grid_auto_flow().as_deref(), Some("column"));
        assert_eq!(style.justify_items().as_deref(), Some("center"));
        assert_eq!(style.justify_self().as_deref(), Some("center"));
        assert_eq!(style.align_self().as_deref(), Some("end"));
        assert_eq!(style.align_content().as_deref(), Some("space-between"));
    }

    #[test]
    fn stylesheet_linear_gradient_background_survives_compute() {
        use browser_dom::Document;

        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let div = doc.create_element("div");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, div);

        let mut engine = crate::StyleEngine::new(crate::Viewport::default());
        engine.add_css(
            "div { background-image: linear-gradient(90deg, red, blue) }",
            crate::Origin::Author,
        );
        let styled = engine.compute(&doc);
        let style = styled.get(div).unwrap();

        // This is exactly the form `browser_paint::commands::parse_linear_gradient`
        // accepts: it starts with `linear-gradient(` and splits the arguments
        // on top-level commas into an angle and two colour stops.
        let image = style.background_image().expect("gradient should survive");
        assert_eq!(image, "linear-gradient(90deg, red, blue)");
        assert!(image.to_ascii_lowercase().starts_with("linear-gradient("));
    }
}
