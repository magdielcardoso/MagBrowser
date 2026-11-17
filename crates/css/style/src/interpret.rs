//! Interpretation of parsed CSS values into computed values.

use browser_css::{Color, ComponentValue, Length, Value};

use crate::computed::{ComputedValue, Dim, LineHeight};
use crate::properties::Property;

/// Context needed to resolve relative units.
#[derive(Debug, Clone, Copy)]
pub struct InterpretContext {
    /// The element's computed font size (for `em`), already resolved.
    pub font_size: f32,
    /// The root element font size (for `rem`).
    pub root_font_size: f32,
    /// Parent font size, used while resolving `font-size` itself.
    pub parent_font_size: f32,
    /// Viewport width in CSS pixels (for `vw`).
    pub viewport_width: f32,
    /// Viewport height in CSS pixels (for `vh`).
    pub viewport_height: f32,
}

impl Default for InterpretContext {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            root_font_size: 16.0,
            parent_font_size: 16.0,
            viewport_width: 1024.0,
            viewport_height: 768.0,
        }
    }
}

/// Interprets one longhand declaration.
pub fn interpret(
    property: Property,
    value: &Value,
    ctx: &InterpretContext,
) -> Option<ComputedValue> {
    if value.is_empty() {
        return None;
    }
    let ident = value.ident();
    match property {
        Property::Display => {
            let keyword = match ident? {
                "none" | "block" | "inline" | "inline-block" | "flex" | "grid" | "table"
                | "table-row" | "table-cell" | "list-item" | "contents" => ident?,
                "table-row-group" | "table-header-group" | "table-footer-group" => "table-row",
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::Position => {
            let keyword = match ident? {
                "static" | "relative" | "absolute" | "fixed" | "sticky" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::Float => {
            let keyword = match ident? {
                "none" | "left" | "right" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::Clear => {
            let keyword = match ident? {
                "none" | "left" | "right" | "both" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::Visibility => {
            let keyword = match ident? {
                "visible" | "hidden" | "collapse" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
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
        | Property::FlexBasis => Some(ComputedValue::Dim(dim(value, ctx, None)?)),
        Property::MarginTop
        | Property::MarginRight
        | Property::MarginBottom
        | Property::MarginLeft => Some(ComputedValue::Dim(dim(value, ctx, None)?)),
        Property::PaddingTop
        | Property::PaddingRight
        | Property::PaddingBottom
        | Property::PaddingLeft
        | Property::RowGap
        | Property::ColumnGap => Some(ComputedValue::Px(length(value, ctx, None)?)),
        Property::LetterSpacing | Property::WordSpacing => {
            if ident == Some("normal") {
                return Some(ComputedValue::Px(0.0));
            }
            Some(ComputedValue::Px(length(value, ctx, None)?))
        }
        Property::BorderTopWidth
        | Property::BorderRightWidth
        | Property::BorderBottomWidth
        | Property::BorderLeftWidth => {
            if let Some(keyword) = ident {
                let px = match keyword {
                    "thin" => 1.0,
                    "medium" => 3.0,
                    "thick" => 5.0,
                    _ => return None,
                };
                return Some(ComputedValue::Px(px));
            }
            Some(ComputedValue::Px(length(value, ctx, None)?.max(0.0)))
        }
        Property::BorderTopStyle
        | Property::BorderRightStyle
        | Property::BorderBottomStyle
        | Property::BorderLeftStyle => {
            let keyword = match ident? {
                "none" | "hidden" | "solid" | "dashed" | "dotted" | "double" | "groove"
                | "ridge" | "inset" | "outset" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::Color
        | Property::BackgroundColor
        | Property::BorderTopColor
        | Property::BorderRightColor
        | Property::BorderBottomColor
        | Property::BorderLeftColor => Some(ComputedValue::Color(value.as_color()?)),
        Property::BackgroundImage => {
            if let Some(url) = value.as_url() {
                return Some(ComputedValue::Str(url.to_string()));
            }
            match ident? {
                "none" => Some(ComputedValue::Str("none".to_string())),
                _ => None,
            }
        }
        Property::FontSize => Some(ComputedValue::Px(font_size(value, ctx)?)),
        Property::FontWeight => {
            let weight = match ident {
                Some("normal") => 400,
                Some("bold") => 700,
                Some("bolder") => {
                    let base = ctx.parent_font_size.max(0.0);
                    let _ = base;
                    700
                }
                Some("lighter") => 100,
                _ => value.as_number()? as i32,
            };
            Some(ComputedValue::Integer(weight.clamp(1, 1000)))
        }
        Property::FontStyle => {
            let keyword = match ident? {
                "normal" | "italic" | "oblique" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::FontFamily => {
            let groups = value.split_commas();
            let families: Vec<String> = groups
                .iter()
                .filter_map(|group| {
                    let text: String = group
                        .iter()
                        .filter_map(|component| match component {
                            ComponentValue::Ident(s) => Some(s.clone()),
                            ComponentValue::String(s) => Some(format!("{s:?}")),
                            ComponentValue::Whitespace => Some(" ".to_string()),
                            _ => None,
                        })
                        .collect();
                    let text = text.trim().to_string();
                    if text.is_empty() { None } else { Some(text) }
                })
                .collect();
            if families.is_empty() {
                None
            } else {
                Some(ComputedValue::IdentList(families))
            }
        }
        Property::LineHeight => {
            if ident == Some("normal") {
                return Some(ComputedValue::LineHeight(LineHeight::Normal));
            }
            if let Some(ComponentValue::Percentage(p)) = value.first() {
                return Some(ComputedValue::LineHeight(LineHeight::Px(
                    ctx.font_size * p / 100.0,
                )));
            }
            if let Some(n) = value.as_number()
                && matches!(value.first(), Some(ComponentValue::Number(_)))
            {
                return Some(ComputedValue::LineHeight(LineHeight::Multiplier(n)));
            }
            Some(ComputedValue::LineHeight(LineHeight::Px(length(
                value, ctx, None,
            )?)))
        }
        Property::TextAlign => {
            let keyword = match ident? {
                "left" | "right" | "center" | "justify" | "start" | "end" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::TextDecoration => {
            let keyword = match ident? {
                "none" | "underline" | "line-through" | "overline" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::TextTransform => {
            let keyword = match ident? {
                "none" | "uppercase" | "lowercase" | "capitalize" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::WhiteSpace => {
            let keyword = match ident? {
                "normal" | "nowrap" | "pre" | "pre-wrap" | "pre-line" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::OverflowX | Property::OverflowY => {
            let keyword = match ident? {
                "visible" | "hidden" | "scroll" | "auto" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::BoxSizing => {
            let keyword = match ident? {
                "content-box" | "border-box" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::ZIndex => {
            if ident == Some("auto") {
                return Some(ComputedValue::Integer(0));
            }
            Some(ComputedValue::Integer(value.as_number()? as i32))
        }
        Property::Opacity => Some(ComputedValue::Px(value.as_number()?.clamp(0.0, 1.0))),
        Property::FlexDirection => {
            let keyword = match ident? {
                "row" | "row-reverse" | "column" | "column-reverse" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::FlexWrap => {
            let keyword = match ident? {
                "nowrap" | "wrap" | "wrap-reverse" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::JustifyContent => {
            let keyword = match ident? {
                "flex-start" | "flex-end" | "center" | "space-between" | "space-around"
                | "space-evenly" | "start" | "end" => match ident? {
                    "start" => "flex-start",
                    "end" => "flex-end",
                    other => other,
                },
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::AlignItems | Property::AlignSelf => {
            let keyword = match ident? {
                "stretch" | "flex-start" | "flex-end" | "center" | "baseline" | "auto" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::FlexGrow | Property::FlexShrink => {
            Some(ComputedValue::Number(value.as_number()?.max(0.0)))
        }
        Property::GridTemplateColumns | Property::GridTemplateRows => {
            // Kept as raw text so the layout engine can parse `fr`, `repeat()`
            // and `auto` track lists itself.
            Some(ComputedValue::Str(raw_value_text(value)))
        }
        Property::TableLayout => {
            let keyword = match ident? {
                "auto" | "fixed" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::BorderCollapse => {
            let keyword = match ident? {
                "separate" | "collapse" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::VerticalAlign => {
            let keyword = match ident? {
                "baseline" | "top" | "middle" | "bottom" | "sub" | "super" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::ListStyleType => {
            let keyword = match ident? {
                "none" | "disc" | "circle" | "square" | "decimal" | "lower-alpha"
                | "upper-alpha" | "lower-roman" | "upper-roman" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::Cursor => Some(ComputedValue::keyword(ident?)),
        Property::Direction => {
            let keyword = match ident? {
                "ltr" | "rtl" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
    }
}

/// Serializes component values back to a compact text form.
///
/// Used for properties whose grammar is left to the consumer (grid track
/// lists).
fn raw_value_text(value: &Value) -> String {
    let mut out = String::new();
    for component in &value.components {
        component_text(component, &mut out);
    }
    out.trim().to_string()
}

fn component_text(component: &ComponentValue, out: &mut String) {
    match component {
        ComponentValue::Ident(text) => out.push_str(text),
        ComponentValue::Number(number) => out.push_str(&format!("{number}")),
        ComponentValue::Percentage(percent) => out.push_str(&format!("{percent}%")),
        ComponentValue::Dimension(number, unit) => out.push_str(&format!("{number}{unit}")),
        ComponentValue::String(text) => {
            out.push('"');
            out.push_str(text);
            out.push('"');
        }
        ComponentValue::Url(url) => {
            out.push_str("url(");
            out.push_str(url);
            out.push(')');
        }
        ComponentValue::Hash(hash) => {
            out.push('#');
            out.push_str(hash);
        }
        ComponentValue::Function(name, arguments) => {
            out.push_str(name);
            out.push('(');
            for argument in arguments {
                component_text(argument, out);
            }
            out.push(')');
        }
        ComponentValue::Delim(character) => out.push(*character),
        ComponentValue::Comma => out.push(','),
        ComponentValue::Whitespace => out.push(' '),
    }
}

/// Resolves a dimension value (`auto`, length or percentage).
pub fn dim(value: &Value, ctx: &InterpretContext, percent_base: Option<f32>) -> Option<Dim> {
    if value.ident() == Some("auto") {
        return Some(Dim::Auto);
    }
    match value.first()? {
        ComponentValue::Percentage(p) => Some(Dim::Percent(*p)),
        ComponentValue::Number(n) if *n == 0.0 => Some(Dim::Px(0.0)),
        _ => Some(Dim::Px(length(value, ctx, percent_base)?)),
    }
}

/// Resolves a length to CSS pixels.
pub fn length(value: &Value, ctx: &InterpretContext, percent_base: Option<f32>) -> Option<f32> {
    match value.first()? {
        ComponentValue::Percentage(p) => Some(percent_base.unwrap_or(0.0) * p / 100.0),
        ComponentValue::Number(n) => Some(*n as f32),
        ComponentValue::Dimension(n, unit) => Some(match unit.to_ascii_lowercase().as_str() {
            "px" => *n as f32,
            "pt" => (*n as f32) * 4.0 / 3.0,
            "pc" => (*n as f32) * 16.0,
            "in" => (*n as f32) * 96.0,
            "cm" => (*n as f32) * 96.0 / 2.54,
            "mm" => (*n as f32) * 96.0 / 25.4,
            "q" => (*n as f32) * 96.0 / 101.6,
            "em" => (*n as f32) * ctx.font_size,
            "rem" => (*n as f32) * ctx.root_font_size,
            "ex" => (*n as f32) * ctx.font_size * 0.5,
            "ch" => (*n as f32) * ctx.font_size * 0.5,
            "vw" => (*n as f32) * ctx.viewport_width / 100.0,
            "vh" => (*n as f32) * ctx.viewport_height / 100.0,
            "vmin" => (*n as f32) * ctx.viewport_width.min(ctx.viewport_height) / 100.0,
            "vmax" => (*n as f32) * ctx.viewport_width.max(ctx.viewport_height) / 100.0,
            _ => return None,
        }),
        _ => {
            let length = value.as_length()?;
            Some(resolve_css_length(length, ctx, percent_base))
        }
    }
}

fn resolve_css_length(length: Length, ctx: &InterpretContext, percent_base: Option<f32>) -> f32 {
    match length {
        Length::Px(px) => px,
        Length::Em(em) => em * ctx.font_size,
        Length::Rem(rem) => rem * ctx.root_font_size,
        Length::Percent(p) => percent_base.unwrap_or(0.0) * p / 100.0,
        Length::Pt(pt) => pt * 4.0 / 3.0,
        Length::Vw(vw) => vw * ctx.viewport_width / 100.0,
        Length::Vh(vh) => vh * ctx.viewport_height / 100.0,
    }
}

/// Resolves `font-size`, where `em` and `%` are relative to the parent.
pub fn font_size(value: &Value, ctx: &InterpretContext) -> Option<f32> {
    if let Some(ident) = value.ident() {
        let px = match ident {
            "xx-small" => 9.0,
            "x-small" => 10.0,
            "small" => 13.0,
            "medium" => 16.0,
            "large" => 18.0,
            "x-large" => 24.0,
            "xx-large" => 32.0,
            "smaller" => ctx.parent_font_size * 0.833,
            "larger" => ctx.parent_font_size * 1.2,
            _ => return None,
        };
        return Some(px);
    }
    match value.first()? {
        ComponentValue::Percentage(p) => Some(ctx.parent_font_size * p / 100.0),
        ComponentValue::Dimension(n, unit) if unit.eq_ignore_ascii_case("em") => {
            Some((*n as f32) * ctx.parent_font_size)
        }
        _ => {
            let ctx = InterpretContext {
                font_size: ctx.parent_font_size,
                ..*ctx
            };
            length(value, &ctx, Some(ctx.parent_font_size)).map(|px| px.max(0.0))
        }
    }
}

/// A fully transparent style helper used for unsupported elements.
pub fn transparent_color() -> Color {
    Color::transparent()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computed::{Dim, LineHeight};

    fn value(css: &str) -> Value {
        browser_css::parse_declarations(&format!("x: {css}"))
            .into_iter()
            .next()
            .unwrap()
            .value
    }

    #[test]
    fn resolves_length_units() {
        let ctx = InterpretContext::default();
        assert_eq!(length(&value("10px"), &ctx, None), Some(10.0));
        assert_eq!(length(&value("1.5em"), &ctx, None), Some(24.0));
        assert_eq!(length(&value("2rem"), &ctx, None), Some(32.0));
        assert_eq!(length(&value("50%"), &ctx, Some(200.0)), Some(100.0));
        assert_eq!(length(&value("50vw"), &ctx, None), Some(512.0));
    }

    #[test]
    fn interprets_dims_and_auto() {
        let ctx = InterpretContext::default();
        assert_eq!(dim(&value("auto"), &ctx, None), Some(Dim::Auto));
        assert_eq!(dim(&value("12px"), &ctx, None), Some(Dim::Px(12.0)));
        assert_eq!(dim(&value("25%"), &ctx, None), Some(Dim::Percent(25.0)));
    }

    #[test]
    fn font_size_relative_to_parent() {
        let ctx = InterpretContext {
            parent_font_size: 20.0,
            ..InterpretContext::default()
        };
        assert_eq!(font_size(&value("150%"), &ctx), Some(30.0));
        assert_eq!(font_size(&value("2em"), &ctx), Some(40.0));
        assert_eq!(font_size(&value("large"), &ctx), Some(18.0));
    }

    #[test]
    fn line_height_variants() {
        let ctx = InterpretContext::default();
        assert_eq!(
            interpret(Property::LineHeight, &value("1.5"), &ctx),
            Some(ComputedValue::LineHeight(LineHeight::Multiplier(1.5)))
        );
        assert_eq!(
            interpret(Property::LineHeight, &value("normal"), &ctx),
            Some(ComputedValue::LineHeight(LineHeight::Normal))
        );
        assert_eq!(
            interpret(Property::LineHeight, &value("24px"), &ctx),
            Some(ComputedValue::LineHeight(LineHeight::Px(24.0)))
        );
    }

    #[test]
    fn rejects_invalid_keywords() {
        let ctx = InterpretContext::default();
        assert!(interpret(Property::Display, &value("banana"), &ctx).is_none());
        assert!(interpret(Property::Position, &value("flying"), &ctx).is_none());
    }
}
