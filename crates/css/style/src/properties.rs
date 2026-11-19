//! Longhand CSS properties known to the engine, with initial values.

use browser_css::Color;

use crate::computed::{
    AspectRatio, BorderRadius, ComputedValue, Dim, FilterList, LineHeight, ObjectFit, ShadowList,
    TransformList,
};

/// A CSS longhand property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Property {
    /// `display`
    Display,
    /// `position`
    Position,
    /// `float`
    Float,
    /// `clear`
    Clear,
    /// `visibility`
    Visibility,
    /// `width`
    Width,
    /// `height`
    Height,
    /// `min-width`
    MinWidth,
    /// `max-width`
    MaxWidth,
    /// `min-height`
    MinHeight,
    /// `max-height`
    MaxHeight,
    /// `margin-top`
    MarginTop,
    /// `margin-right`
    MarginRight,
    /// `margin-bottom`
    MarginBottom,
    /// `margin-left`
    MarginLeft,
    /// `padding-top`
    PaddingTop,
    /// `padding-right`
    PaddingRight,
    /// `padding-bottom`
    PaddingBottom,
    /// `padding-left`
    PaddingLeft,
    /// `border-top-width`
    BorderTopWidth,
    /// `border-right-width`
    BorderRightWidth,
    /// `border-bottom-width`
    BorderBottomWidth,
    /// `border-left-width`
    BorderLeftWidth,
    /// `border-top-style`
    BorderTopStyle,
    /// `border-right-style`
    BorderRightStyle,
    /// `border-bottom-style`
    BorderBottomStyle,
    /// `border-left-style`
    BorderLeftStyle,
    /// `border-top-color`
    BorderTopColor,
    /// `border-right-color`
    BorderRightColor,
    /// `border-bottom-color`
    BorderBottomColor,
    /// `border-left-color`
    BorderLeftColor,
    /// `color`
    Color,
    /// `background-color`
    BackgroundColor,
    /// `background-image`
    BackgroundImage,
    /// `font-size`
    FontSize,
    /// `font-weight`
    FontWeight,
    /// `font-style`
    FontStyle,
    /// `font-family`
    FontFamily,
    /// `line-height`
    LineHeight,
    /// `text-align`
    TextAlign,
    /// `text-decoration`
    TextDecoration,
    /// `text-transform`
    TextTransform,
    /// `letter-spacing`
    LetterSpacing,
    /// `word-spacing`
    WordSpacing,
    /// `white-space`
    WhiteSpace,
    /// `overflow-x`
    OverflowX,
    /// `overflow-y`
    OverflowY,
    /// `box-sizing`
    BoxSizing,
    /// `top`
    Top,
    /// `right`
    Right,
    /// `bottom`
    Bottom,
    /// `left`
    Left,
    /// `z-index`
    ZIndex,
    /// `opacity`
    Opacity,
    /// `flex-direction`
    FlexDirection,
    /// `flex-wrap`
    FlexWrap,
    /// `justify-content`
    JustifyContent,
    /// `align-items`
    AlignItems,
    /// `align-self`
    AlignSelf,
    /// `flex-grow`
    FlexGrow,
    /// `flex-shrink`
    FlexShrink,
    /// `flex-basis`
    FlexBasis,
    /// `row-gap`
    RowGap,
    /// `column-gap`
    ColumnGap,
    /// `grid-template-columns`
    GridTemplateColumns,
    /// `grid-template-rows`
    GridTemplateRows,
    /// `grid-template-areas`
    GridTemplateAreas,
    /// `grid-column`
    GridColumn,
    /// `grid-row`
    GridRow,
    /// `grid-area`
    GridArea,
    /// `grid-auto-rows`
    GridAutoRows,
    /// `grid-auto-columns`
    GridAutoColumns,
    /// `grid-auto-flow`
    GridAutoFlow,
    /// `justify-items`
    JustifyItems,
    /// `justify-self`
    JustifySelf,
    /// `align-content`
    AlignContent,
    /// `table-layout`
    TableLayout,
    /// `border-collapse`
    BorderCollapse,
    /// `vertical-align`
    VerticalAlign,
    /// `list-style-type`
    ListStyleType,
    /// `cursor`
    Cursor,
    /// `direction`
    Direction,
    /// `border-radius` (kept as one compound value: four corners, each with an
    /// optional second radius after `/`).
    BorderRadius,
    /// `box-shadow` (a comma-separated list of shadow layers).
    BoxShadow,
    /// `transform` (a space-separated list of transform functions).
    Transform,
    /// `aspect-ratio` (`auto` or a width / height ratio).
    AspectRatio,
    /// `object-fit` (how replaced content is fitted to its box).
    ObjectFit,
    /// `filter` (a space-separated list of filter functions, or `none`).
    Filter,
    /// `text-shadow` (a comma-separated list of shadow layers).
    TextShadow,
}

impl Property {
    /// Every supported longhand.
    pub const ALL: &'static [Property] = &[
        Property::Display,
        Property::Position,
        Property::Float,
        Property::Clear,
        Property::Visibility,
        Property::Width,
        Property::Height,
        Property::MinWidth,
        Property::MaxWidth,
        Property::MinHeight,
        Property::MaxHeight,
        Property::MarginTop,
        Property::MarginRight,
        Property::MarginBottom,
        Property::MarginLeft,
        Property::PaddingTop,
        Property::PaddingRight,
        Property::PaddingBottom,
        Property::PaddingLeft,
        Property::BorderTopWidth,
        Property::BorderRightWidth,
        Property::BorderBottomWidth,
        Property::BorderLeftWidth,
        Property::BorderTopStyle,
        Property::BorderRightStyle,
        Property::BorderBottomStyle,
        Property::BorderLeftStyle,
        Property::BorderTopColor,
        Property::BorderRightColor,
        Property::BorderBottomColor,
        Property::BorderLeftColor,
        Property::Color,
        Property::BackgroundColor,
        Property::BackgroundImage,
        Property::FontSize,
        Property::FontWeight,
        Property::FontStyle,
        Property::FontFamily,
        Property::LineHeight,
        Property::TextAlign,
        Property::TextDecoration,
        Property::TextTransform,
        Property::LetterSpacing,
        Property::WordSpacing,
        Property::WhiteSpace,
        Property::OverflowX,
        Property::OverflowY,
        Property::BoxSizing,
        Property::Top,
        Property::Right,
        Property::Bottom,
        Property::Left,
        Property::ZIndex,
        Property::Opacity,
        Property::FlexDirection,
        Property::FlexWrap,
        Property::JustifyContent,
        Property::AlignItems,
        Property::AlignSelf,
        Property::FlexGrow,
        Property::FlexShrink,
        Property::FlexBasis,
        Property::RowGap,
        Property::ColumnGap,
        Property::GridTemplateColumns,
        Property::GridTemplateRows,
        Property::GridTemplateAreas,
        Property::GridColumn,
        Property::GridRow,
        Property::GridArea,
        Property::GridAutoRows,
        Property::GridAutoColumns,
        Property::GridAutoFlow,
        Property::JustifyItems,
        Property::JustifySelf,
        Property::AlignContent,
        Property::TableLayout,
        Property::BorderCollapse,
        Property::VerticalAlign,
        Property::ListStyleType,
        Property::Cursor,
        Property::Direction,
        Property::BorderRadius,
        Property::BoxShadow,
        Property::Transform,
        Property::AspectRatio,
        Property::ObjectFit,
        Property::Filter,
        Property::TextShadow,
    ];

    /// The CSS name of the property.
    pub fn name(self) -> &'static str {
        match self {
            Property::Display => "display",
            Property::Position => "position",
            Property::Float => "float",
            Property::Clear => "clear",
            Property::Visibility => "visibility",
            Property::Width => "width",
            Property::Height => "height",
            Property::MinWidth => "min-width",
            Property::MaxWidth => "max-width",
            Property::MinHeight => "min-height",
            Property::MaxHeight => "max-height",
            Property::MarginTop => "margin-top",
            Property::MarginRight => "margin-right",
            Property::MarginBottom => "margin-bottom",
            Property::MarginLeft => "margin-left",
            Property::PaddingTop => "padding-top",
            Property::PaddingRight => "padding-right",
            Property::PaddingBottom => "padding-bottom",
            Property::PaddingLeft => "padding-left",
            Property::BorderTopWidth => "border-top-width",
            Property::BorderRightWidth => "border-right-width",
            Property::BorderBottomWidth => "border-bottom-width",
            Property::BorderLeftWidth => "border-left-width",
            Property::BorderTopStyle => "border-top-style",
            Property::BorderRightStyle => "border-right-style",
            Property::BorderBottomStyle => "border-bottom-style",
            Property::BorderLeftStyle => "border-left-style",
            Property::BorderTopColor => "border-top-color",
            Property::BorderRightColor => "border-right-color",
            Property::BorderBottomColor => "border-bottom-color",
            Property::BorderLeftColor => "border-left-color",
            Property::Color => "color",
            Property::BackgroundColor => "background-color",
            Property::BackgroundImage => "background-image",
            Property::FontSize => "font-size",
            Property::FontWeight => "font-weight",
            Property::FontStyle => "font-style",
            Property::FontFamily => "font-family",
            Property::LineHeight => "line-height",
            Property::TextAlign => "text-align",
            Property::TextDecoration => "text-decoration",
            Property::TextTransform => "text-transform",
            Property::LetterSpacing => "letter-spacing",
            Property::WordSpacing => "word-spacing",
            Property::WhiteSpace => "white-space",
            Property::OverflowX => "overflow-x",
            Property::OverflowY => "overflow-y",
            Property::BoxSizing => "box-sizing",
            Property::Top => "top",
            Property::Right => "right",
            Property::Bottom => "bottom",
            Property::Left => "left",
            Property::ZIndex => "z-index",
            Property::Opacity => "opacity",
            Property::FlexDirection => "flex-direction",
            Property::FlexWrap => "flex-wrap",
            Property::JustifyContent => "justify-content",
            Property::AlignItems => "align-items",
            Property::AlignSelf => "align-self",
            Property::FlexGrow => "flex-grow",
            Property::FlexShrink => "flex-shrink",
            Property::FlexBasis => "flex-basis",
            Property::RowGap => "row-gap",
            Property::ColumnGap => "column-gap",
            Property::GridTemplateColumns => "grid-template-columns",
            Property::GridTemplateRows => "grid-template-rows",
            Property::GridTemplateAreas => "grid-template-areas",
            Property::GridColumn => "grid-column",
            Property::GridRow => "grid-row",
            Property::GridArea => "grid-area",
            Property::GridAutoRows => "grid-auto-rows",
            Property::GridAutoColumns => "grid-auto-columns",
            Property::GridAutoFlow => "grid-auto-flow",
            Property::JustifyItems => "justify-items",
            Property::JustifySelf => "justify-self",
            Property::AlignContent => "align-content",
            Property::TableLayout => "table-layout",
            Property::BorderCollapse => "border-collapse",
            Property::VerticalAlign => "vertical-align",
            Property::ListStyleType => "list-style-type",
            Property::Cursor => "cursor",
            Property::Direction => "direction",
            Property::BorderRadius => "border-radius",
            Property::BoxShadow => "box-shadow",
            Property::Transform => "transform",
            Property::AspectRatio => "aspect-ratio",
            Property::ObjectFit => "object-fit",
            Property::Filter => "filter",
            Property::TextShadow => "text-shadow",
        }
    }

    /// Resolves a CSS property name.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Property::ALL.iter().copied().find(|p| p.name() == name)
    }

    /// True when the property is inherited by default.
    pub fn is_inherited(self) -> bool {
        matches!(
            self,
            Property::Color
                | Property::FontSize
                | Property::FontWeight
                | Property::FontStyle
                | Property::FontFamily
                | Property::LineHeight
                | Property::TextAlign
                | Property::WhiteSpace
                | Property::LetterSpacing
                | Property::WordSpacing
                | Property::Visibility
                | Property::ListStyleType
                | Property::Cursor
                | Property::TextTransform
                | Property::BorderCollapse
                | Property::Direction
        )
    }
}

/// The initial (spec default) value of a property.
pub fn initial_value(property: Property) -> Option<ComputedValue> {
    let value = match property {
        Property::Display => ComputedValue::keyword("inline"),
        Property::Position => ComputedValue::keyword("static"),
        Property::Float => ComputedValue::keyword("none"),
        Property::Clear => ComputedValue::keyword("none"),
        Property::Visibility => ComputedValue::keyword("visible"),
        Property::Width
        | Property::Height
        | Property::MinWidth
        | Property::MaxWidth
        | Property::MinHeight
        | Property::MaxHeight
        | Property::Top
        | Property::Right
        | Property::Bottom
        | Property::Left
        | Property::FlexBasis => ComputedValue::Dim(Dim::Auto),
        Property::MarginTop
        | Property::MarginRight
        | Property::MarginBottom
        | Property::MarginLeft
        | Property::PaddingTop
        | Property::PaddingRight
        | Property::PaddingBottom
        | Property::PaddingLeft
        | Property::RowGap
        | Property::ColumnGap
        | Property::LetterSpacing
        | Property::WordSpacing => ComputedValue::Px(0.0),
        Property::BorderTopWidth
        | Property::BorderRightWidth
        | Property::BorderBottomWidth
        | Property::BorderLeftWidth => ComputedValue::Px(3.0),
        Property::BorderTopStyle
        | Property::BorderRightStyle
        | Property::BorderBottomStyle
        | Property::BorderLeftStyle => ComputedValue::keyword("none"),
        Property::BorderTopColor
        | Property::BorderRightColor
        | Property::BorderBottomColor
        | Property::BorderLeftColor => ComputedValue::Color(Color::rgb(0, 0, 0)),
        Property::Color => ComputedValue::Color(Color::rgb(0, 0, 0)),
        Property::BackgroundColor => ComputedValue::Color(Color::transparent()),
        Property::BackgroundImage => ComputedValue::Str("none".to_string()),
        Property::FontSize => ComputedValue::Px(16.0),
        Property::FontWeight => ComputedValue::Integer(400),
        Property::FontStyle => ComputedValue::keyword("normal"),
        Property::FontFamily => ComputedValue::IdentList(vec!["sans-serif".to_string()]),
        Property::LineHeight => ComputedValue::LineHeight(LineHeight::Normal),
        Property::TextAlign => ComputedValue::keyword("left"),
        Property::TextDecoration => ComputedValue::keyword("none"),
        Property::TextTransform => ComputedValue::keyword("none"),
        Property::WhiteSpace => ComputedValue::keyword("normal"),
        Property::OverflowX | Property::OverflowY => ComputedValue::keyword("visible"),
        Property::BoxSizing => ComputedValue::keyword("content-box"),
        Property::ZIndex => ComputedValue::Integer(0),
        Property::Opacity => ComputedValue::Px(1.0),
        Property::FlexDirection => ComputedValue::keyword("row"),
        Property::FlexWrap => ComputedValue::keyword("nowrap"),
        Property::JustifyContent => ComputedValue::keyword("flex-start"),
        Property::AlignItems => ComputedValue::keyword("stretch"),
        Property::AlignSelf => ComputedValue::keyword("auto"),
        Property::FlexGrow => ComputedValue::Number(0.0),
        Property::FlexShrink => ComputedValue::Number(1.0),
        Property::GridTemplateColumns | Property::GridTemplateRows => {
            ComputedValue::keyword("none")
        }
        // Placement values are kept as raw text so the grid layout engine can
        // parse line numbers, `span`, named lines and `/` separators itself.
        // `auto` is the spec initial value for every placement longhand.
        Property::GridColumn
        | Property::GridRow
        | Property::GridArea
        | Property::GridAutoRows
        | Property::GridAutoColumns
        | Property::JustifySelf => ComputedValue::keyword("auto"),
        Property::GridTemplateAreas => ComputedValue::keyword("none"),
        Property::GridAutoFlow => ComputedValue::keyword("row"),
        Property::JustifyItems | Property::AlignContent => ComputedValue::keyword("normal"),
        Property::TableLayout => ComputedValue::keyword("auto"),
        Property::BorderCollapse => ComputedValue::keyword("separate"),
        Property::VerticalAlign => ComputedValue::keyword("baseline"),
        Property::ListStyleType => ComputedValue::keyword("disc"),
        Property::Cursor => ComputedValue::keyword("auto"),
        Property::Direction => ComputedValue::keyword("ltr"),
        Property::BorderRadius => ComputedValue::BorderRadius(BorderRadius::default()),
        Property::BoxShadow | Property::TextShadow => ComputedValue::Shadows(ShadowList::default()),
        Property::Transform => ComputedValue::Transform(TransformList::default()),
        Property::AspectRatio => ComputedValue::AspectRatio(AspectRatio::Auto),
        Property::ObjectFit => ComputedValue::ObjectFit(ObjectFit::default()),
        Property::Filter => ComputedValue::Filter(FilterList::default()),
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for property in Property::ALL {
            assert_eq!(Property::from_name(property.name()), Some(*property));
        }
        assert_eq!(Property::from_name("DISPLAY"), Some(Property::Display));
        assert_eq!(Property::from_name("nope"), None);
    }

    #[test]
    fn inherited_set_is_correct() {
        assert!(Property::Color.is_inherited());
        assert!(Property::FontSize.is_inherited());
        assert!(!Property::MarginTop.is_inherited());
        assert!(!Property::Width.is_inherited());
    }

    #[test]
    fn all_properties_have_initial_values() {
        for property in Property::ALL {
            assert!(initial_value(*property).is_some(), "{property:?}");
        }
    }

    #[test]
    fn grid_properties_are_recognised_and_not_inherited() {
        let grid = [
            ("grid-column", Property::GridColumn),
            ("grid-row", Property::GridRow),
            ("grid-area", Property::GridArea),
            ("grid-template-areas", Property::GridTemplateAreas),
            ("grid-auto-rows", Property::GridAutoRows),
            ("grid-auto-columns", Property::GridAutoColumns),
            ("grid-auto-flow", Property::GridAutoFlow),
            ("justify-content", Property::JustifyContent),
            ("justify-items", Property::JustifyItems),
            ("justify-self", Property::JustifySelf),
            ("align-self", Property::AlignSelf),
            ("align-content", Property::AlignContent),
        ];
        for (name, property) in grid {
            assert_eq!(Property::from_name(name), Some(property), "{name}");
            assert_eq!(property.name(), name);
            assert!(!property.is_inherited(), "{name}");
            assert!(initial_value(property).is_some(), "{name}");
        }
    }
}
