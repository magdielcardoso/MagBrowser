//! Interpretation of parsed CSS values into computed values.

use browser_css::{Color, ComponentValue, Length, Value};

use crate::computed::{
    AspectRatio, BorderRadius, ComputedValue, CornerRadius, Dim, FilterFunction, FilterList,
    LineHeight, ObjectFit, Shadow, ShadowList, TransformFunction, TransformList,
};
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

/// Interprets a lone `env()` or `attr()` call as an opaque string.
///
/// The engine does not resolve environment variables or attribute references
/// into typed values. Rather than dropping such declarations, callers can use
/// this helper to expose the authored text as a [`ComputedValue::Str`] on
/// properties that accept raw text. Returns `None` for every other value.
#[must_use]
pub fn env_or_attr(value: &Value) -> Option<ComputedValue> {
    let [ComponentValue::Function(name, _)] = value.components.as_slice() else {
        return None;
    };
    if !matches!(name.to_ascii_lowercase().as_str(), "env" | "attr") {
        return None;
    }
    Some(ComputedValue::Str(raw_value_text(value)))
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
        | Property::FlexBasis => {
            let text = raw_value_text(value);
            if is_intrinsic_sizing(&text) {
                // No `Dim` variant exists for intrinsic sizing, so keep the
                // authored keyword as raw text. Layout recognises it through
                // `is_intrinsic_sizing` (e.g. so `width: fit-content` does not
                // stretch) while `as_dim()` still safely reads it as `auto`.
                Some(ComputedValue::Str(text))
            } else {
                Some(ComputedValue::Dim(dim(value, ctx, None)?))
            }
        }
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
            // A lone url() and the `none` keyword keep their historical
            // representation (just the URL, or the literal "none").
            if let Some(url) = value.as_url() {
                return Some(ComputedValue::Str(url.to_string()));
            }
            if value.ident() == Some("none") {
                return Some(ComputedValue::Str("none".to_string()));
            }
            // Gradients (and lists of background layers) have no dedicated
            // computed variant, so retain them as raw CSS text. This lets the
            // paint crate recover e.g. `linear-gradient(...)` with
            // `parse_linear_gradient`. Commas between layers are top-level
            // components, so they survive serialization / rejoining.
            let layers = value.split_commas();
            if layers.iter().all(|layer| is_background_image_layer(layer)) {
                let text = layers
                    .iter()
                    .map(|layer| components_text(layer))
                    .collect::<Vec<_>>()
                    .join(", ");
                if !text.is_empty() {
                    return Some(ComputedValue::Str(text));
                }
            }
            None
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
        Property::AlignItems => {
            let keyword = match ident? {
                "stretch" | "flex-start" | "flex-end" | "center" | "baseline" | "auto" => ident?,
                _ => return None,
            };
            Some(ComputedValue::keyword(keyword))
        }
        Property::FlexGrow | Property::FlexShrink => {
            Some(ComputedValue::Number(value.as_number()?.max(0.0)))
        }
        // Grid track lists, placements and item alignment are kept as raw text
        // so the grid layout engine can parse `fr`, `repeat()`, `span`, `/`,
        // named lines and quoted area rows itself. It already parses these
        // grammars defensively, so no strict validation happens here.
        Property::GridTemplateColumns
        | Property::GridTemplateRows
        | Property::GridTemplateAreas
        | Property::GridColumn
        | Property::GridRow
        | Property::GridArea
        | Property::GridAutoRows
        | Property::GridAutoColumns
        | Property::GridAutoFlow
        | Property::JustifyItems
        | Property::JustifySelf
        | Property::AlignSelf
        | Property::AlignContent => Some(ComputedValue::Str(raw_value_text(value))),
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
        Property::BorderRadius => Some(ComputedValue::BorderRadius(parse_border_radius(
            value, ctx,
        )?)),
        Property::BoxShadow => Some(ComputedValue::Shadows(parse_shadow_list(
            value, ctx, true, true,
        )?)),
        Property::TextShadow => Some(ComputedValue::Shadows(parse_shadow_list(
            value, ctx, false, false,
        )?)),
        Property::Transform => Some(ComputedValue::Transform(parse_transform(value, ctx)?)),
        Property::AspectRatio => Some(ComputedValue::AspectRatio(parse_aspect_ratio(value)?)),
        Property::ObjectFit => {
            let fit = match ident? {
                "fill" => ObjectFit::Fill,
                "contain" => ObjectFit::Contain,
                "cover" => ObjectFit::Cover,
                "none" => ObjectFit::None,
                "scale-down" => ObjectFit::ScaleDown,
                _ => return None,
            };
            Some(ComputedValue::ObjectFit(fit))
        }
        Property::Filter => Some(ComputedValue::Filter(parse_filter(value, ctx)?)),
        // Logical and scrollbar properties are resolved to physical sides by
        // the layout layer; keep their authored text here.
        Property::MarginInlineStart
        | Property::MarginInlineEnd
        | Property::PaddingInlineStart
        | Property::PaddingInlineEnd
        | Property::BorderInlineStartWidth
        | Property::BorderInlineEndWidth
        | Property::InsetInlineStart
        | Property::InsetInlineEnd
        | Property::InlineSize
        | Property::BlockSize
        | Property::ScrollbarWidth => Some(ComputedValue::Str(raw_value_text(value))),
    }
}

/// Serializes component values back to a compact text form.
///
/// Used for properties whose grammar is left to the consumer (grid track
/// lists).
fn raw_value_text(value: &Value) -> String {
    components_text(&value.components)
}

/// Serializes a slice of component values (e.g. one background layer) to
/// compact text, normalizing whitespace runs to a single space.
fn components_text(components: &[ComponentValue]) -> String {
    let mut out = String::new();
    for component in components {
        component_text(component, &mut out);
    }
    out.trim().to_string()
}

/// True when a component list looks like a `<bg-image>` layer we can retain:
/// an image URL, a gradient function, or the `none` keyword. Used to avoid
/// storing arbitrary declarations as `background-image` text.
fn is_background_image_layer(components: &[ComponentValue]) -> bool {
    components.iter().any(|component| match component {
        ComponentValue::Url(_) => true,
        ComponentValue::Function(name, _) => matches!(
            name.as_str(),
            "linear-gradient"
                | "radial-gradient"
                | "conic-gradient"
                | "repeating-linear-gradient"
                | "repeating-radial-gradient"
                | "repeating-conic-gradient"
        ),
        ComponentValue::Ident(name) => name == "none",
        _ => false,
    })
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

/// Splits component values on a top-level delimiter character (for example
/// the `/` in `border-radius` or `aspect-ratio`). Delimiters nested inside
/// functions are not top-level components, so they are left untouched.
fn split_top_level_delim(
    components: &[ComponentValue],
    delimiter: char,
) -> Vec<Vec<ComponentValue>> {
    let mut parts: Vec<Vec<ComponentValue>> = vec![Vec::new()];
    for component in components {
        if matches!(component, ComponentValue::Delim(c) if *c == delimiter) {
            parts.push(Vec::new());
        } else {
            parts
                .last_mut()
                .expect("at least one part")
                .push(component.clone());
        }
    }
    parts
}

/// Groups components by whitespace into owned values.
fn whitespace_groups(components: &[ComponentValue]) -> Vec<Value> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    for component in components {
        if matches!(component, ComponentValue::Whitespace) {
            if !current.is_empty() {
                groups.push(Value {
                    components: std::mem::take(&mut current),
                });
            }
        } else {
            current.push(component.clone());
        }
    }
    if !current.is_empty() {
        groups.push(Value {
            components: current,
        });
    }
    groups
}

/// Interprets one `border-radius` radius (a length or percentage).
fn radius_dim(group: &Value, ctx: &InterpretContext) -> Option<Dim> {
    match dim(group, ctx, None)? {
        Dim::Auto => Some(Dim::Px(0.0)),
        other => Some(other),
    }
}

/// Expands one `border-radius` group (1-4 values) into the four corners.
fn radius_group(components: &[ComponentValue], ctx: &InterpretContext) -> Option<[Dim; 4]> {
    let groups = whitespace_groups(components);
    if groups.is_empty() || groups.len() > 4 {
        return None;
    }
    let values: Vec<Dim> = groups
        .iter()
        .map(|group| radius_dim(group, ctx))
        .collect::<Option<Vec<_>>>()?;
    let at = |index: usize| -> Dim {
        match values.len() {
            1 => values[0],
            2 => values[index % 2],
            3 => match index {
                0 => values[0],
                2 => values[2],
                _ => values[1],
            },
            _ => values[index],
        }
    };
    Some([at(0), at(1), at(2), at(3)])
}

fn parse_border_radius(value: &Value, ctx: &InterpretContext) -> Option<BorderRadius> {
    let parts = split_top_level_delim(&value.components, '/');
    if parts.len() > 2 {
        return None;
    }
    let horizontal = radius_group(&parts[0], ctx)?;
    let vertical = match parts.get(1) {
        Some(part) => radius_group(part, ctx)?,
        None => [Dim::Px(0.0); 4],
    };
    let has_vertical = parts.len() == 2;
    let corners = std::array::from_fn(|index| CornerRadius {
        horizontal: horizontal[index],
        vertical: has_vertical.then_some(vertical[index]),
    });
    Some(BorderRadius { corners })
}

/// Parses one comma-separated shadow layer.
fn parse_shadow(
    components: &[ComponentValue],
    ctx: &InterpretContext,
    allow_inset: bool,
    allow_spread: bool,
) -> Option<Shadow> {
    let mut inset = false;
    let mut color = None;
    let mut lengths = Vec::new();
    for component in components {
        if matches!(component, ComponentValue::Whitespace) {
            continue;
        }
        if let ComponentValue::Ident(name) = component {
            if allow_inset && name == "inset" {
                inset = true;
                continue;
            }
            // `currentColor` cannot be resolved here; fall back to the same
            // opaque-black default used when a shadow omits its colour.
            if name == "currentcolor" {
                color = Some(Color::rgb(0, 0, 0));
                continue;
            }
        }
        let single = Value {
            components: vec![component.clone()],
        };
        if let Some(parsed) = single.as_color() {
            color = Some(parsed);
            continue;
        }
        lengths.push(length(&single, ctx, None)?);
    }
    let maximum = if allow_spread { 4 } else { 3 };
    if lengths.len() < 2 || lengths.len() > maximum {
        return None;
    }
    let mut lengths = lengths.into_iter();
    Some(Shadow {
        offset_x: lengths.next()?,
        offset_y: lengths.next()?,
        blur: lengths.next().unwrap_or(0.0),
        spread: if allow_spread {
            lengths.next().unwrap_or(0.0)
        } else {
            0.0
        },
        color: color.unwrap_or_else(|| Color::rgb(0, 0, 0)),
        inset,
    })
}

fn parse_shadow_list(
    value: &Value,
    ctx: &InterpretContext,
    allow_inset: bool,
    allow_spread: bool,
) -> Option<ShadowList> {
    if value.ident() == Some("none") {
        return Some(Vec::new());
    }
    value
        .split_commas()
        .iter()
        .map(|group| parse_shadow(group, ctx, allow_inset, allow_spread))
        .collect()
}

fn arg_dim(arg: &ComponentValue, ctx: &InterpretContext) -> Option<Dim> {
    dim(
        &Value {
            components: vec![arg.clone()],
        },
        ctx,
        None,
    )
}

fn arg_length(arg: &ComponentValue, ctx: &InterpretContext) -> Option<f32> {
    length(
        &Value {
            components: vec![arg.clone()],
        },
        ctx,
        None,
    )
}

fn arg_number(arg: &ComponentValue) -> Option<f32> {
    match arg {
        ComponentValue::Number(number) => Some(*number as f32),
        _ => None,
    }
}

fn arg_factor(arg: &ComponentValue) -> Option<f32> {
    match arg {
        ComponentValue::Number(number) => Some(*number as f32),
        ComponentValue::Percentage(percent) => Some(*percent / 100.0),
        _ => None,
    }
}

fn arg_angle(arg: &ComponentValue) -> Option<f32> {
    match arg {
        ComponentValue::Number(number) => Some(*number as f32),
        ComponentValue::Dimension(number, unit) => match unit.as_str() {
            "deg" => Some(*number as f32),
            "rad" => Some((*number as f32).to_degrees()),
            "grad" => Some(*number as f32 * 0.9),
            "turn" => Some(*number as f32 * 360.0),
            _ => None,
        },
        _ => None,
    }
}

/// The non-whitespace, non-comma arguments of a function.
fn function_args(args: &[ComponentValue]) -> Vec<&ComponentValue> {
    args.iter()
        .filter(|component| {
            !matches!(
                component,
                ComponentValue::Whitespace | ComponentValue::Comma
            )
        })
        .collect()
}

fn parse_transform(value: &Value, ctx: &InterpretContext) -> Option<TransformList> {
    if value.ident() == Some("none") {
        return Some(Vec::new());
    }
    let mut functions = Vec::new();
    for component in &value.components {
        if matches!(component, ComponentValue::Whitespace) {
            continue;
        }
        let ComponentValue::Function(name, arguments) = component else {
            return None;
        };
        let args = function_args(arguments);
        let function = match name.as_str() {
            "translate" => {
                let x = arg_dim(args.first()?, ctx)?;
                let y = args
                    .get(1)
                    .map_or(Some(Dim::Px(0.0)), |arg| arg_dim(arg, ctx))?;
                TransformFunction::Translate(x, y)
            }
            "translatex" => TransformFunction::TranslateX(arg_dim(args.first()?, ctx)?),
            "translatey" => TransformFunction::TranslateY(arg_dim(args.first()?, ctx)?),
            "translate3d" => {
                // The z component has no 2D effect; keep x and y only.
                let x = arg_dim(args.first()?, ctx)?;
                let y = args
                    .get(1)
                    .map_or(Some(Dim::Px(0.0)), |arg| arg_dim(arg, ctx))?;
                TransformFunction::Translate(x, y)
            }
            "scale" => {
                let x = arg_number(args.first()?)?;
                let y = args.get(1).map_or(Some(x), |arg| arg_number(arg))?;
                TransformFunction::Scale(x, y)
            }
            "scalex" => TransformFunction::ScaleX(arg_number(args.first()?)?),
            "scaley" => TransformFunction::ScaleY(arg_number(args.first()?)?),
            "scale3d" => {
                let x = arg_number(args.first()?)?;
                let y = args.get(1).map_or(Some(x), |arg| arg_number(arg))?;
                TransformFunction::Scale(x, y)
            }
            "rotate" => TransformFunction::Rotate(arg_angle(args.first()?)?),
            "skew" => {
                let x = arg_angle(args.first()?)?;
                let y = args.get(1).map_or(Some(0.0), |arg| arg_angle(arg))?;
                TransformFunction::Skew(x, y)
            }
            "skewx" => TransformFunction::Skew(arg_angle(args.first()?)?, 0.0),
            "skewy" => TransformFunction::Skew(0.0, arg_angle(args.first()?)?),
            "matrix" => {
                if args.len() != 6 {
                    return None;
                }
                let mut matrix = [0.0; 6];
                for (slot, arg) in matrix.iter_mut().zip(args.iter().take(6)) {
                    *slot = arg_number(arg)?;
                }
                TransformFunction::Matrix(matrix)
            }
            _ => TransformFunction::Raw(raw_value_text(&Value {
                components: vec![component.clone()],
            })),
        };
        functions.push(function);
    }
    Some(functions)
}

fn parse_filter(value: &Value, ctx: &InterpretContext) -> Option<FilterList> {
    if value.ident() == Some("none") {
        return Some(Vec::new());
    }
    let mut functions = Vec::new();
    for component in &value.components {
        if matches!(component, ComponentValue::Whitespace) {
            continue;
        }
        let ComponentValue::Function(name, arguments) = component else {
            return None;
        };
        let args = function_args(arguments);
        let function = match name.as_str() {
            "blur" => FilterFunction::Blur(arg_length(args.first()?, ctx)?),
            "brightness" => FilterFunction::Brightness(arg_factor(args.first()?)?),
            "contrast" => FilterFunction::Contrast(arg_factor(args.first()?)?),
            "grayscale" => FilterFunction::Grayscale(arg_factor(args.first()?)?),
            "hue-rotate" => FilterFunction::HueRotate(arg_angle(args.first()?)?),
            "invert" => FilterFunction::Invert(arg_factor(args.first()?)?),
            "opacity" => FilterFunction::Opacity(arg_factor(args.first()?)?),
            "saturate" => FilterFunction::Saturate(arg_factor(args.first()?)?),
            "sepia" => FilterFunction::Sepia(arg_factor(args.first()?)?),
            _ => FilterFunction::Raw(raw_value_text(&Value {
                components: vec![component.clone()],
            })),
        };
        functions.push(function);
    }
    Some(functions)
}

fn parse_aspect_ratio(value: &Value) -> Option<AspectRatio> {
    if value.ident() == Some("auto") {
        return Some(AspectRatio::Auto);
    }
    let parts = split_top_level_delim(&value.components, '/');
    if parts.len() > 2 {
        return None;
    }
    let first_number = |part: &[ComponentValue]| -> Option<f32> {
        part.iter().find_map(|component| match component {
            ComponentValue::Number(number) => Some(*number as f32),
            _ => None,
        })
    };
    let width = first_number(&parts[0])?;
    let height = parts.get(1).map_or(Some(1.0), |part| first_number(part))?;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some(AspectRatio::Ratio(width, height))
}

/// True when CSS `text` is an intrinsic sizing keyword: `fit-content`,
/// `min-content` or `max-content` (including `fit-content(<length>)`).
///
/// These keywords have no `Dim` variant, so [`interpret`] stores them as
/// `ComputedValue::Str` and layout reads them back with this helper — for
/// example to size a `width: fit-content` box to its content instead of
/// stretching it.
pub fn is_intrinsic_sizing(text: &str) -> bool {
    let text = text.trim().to_ascii_lowercase();
    matches!(text.as_str(), "fit-content" | "min-content" | "max-content")
        || text.starts_with("fit-content(")
}

/// Resolves a dimension value (`auto`, length or percentage).
pub fn dim(value: &Value, ctx: &InterpretContext, percent_base: Option<f32>) -> Option<Dim> {
    if value.ident() == Some("auto") {
        return Some(Dim::Auto);
    }
    // Intrinsic sizing keywords have no `Dim` variant; `interpret` stores them
    // as raw text instead so layout can recognise them via `is_intrinsic_sizing`.
    if is_intrinsic_sizing(&raw_value_text(value)) {
        return None;
    }
    match value.first()? {
        ComponentValue::Function(name, args) if is_math_fn_name(name) => {
            // `calc()` and the comparison functions (`min`/`max`/`clamp`) all
            // reduce to an affine `percent * base + px` expression. With a
            // known base we evaluate directly; otherwise keep the percentage
            // symbolic so layout can resolve it against the containing block
            // (e.g. `min(1240px, 100% - 48px)` becomes 100% + (-48px)).
            let evaluate = |base| eval_math_function(name, args, ctx, base);
            if let Some(base) = percent_base {
                Some(Dim::Px(evaluate(Some(base))?))
            } else if contains_percentage(args) {
                let px = evaluate(Some(0.0))?;
                let at_hundred = evaluate(Some(100.0))?;
                Some(Dim::Calc {
                    percent: at_hundred - px,
                    px,
                })
            } else {
                Some(Dim::Px(evaluate(None)?))
            }
        }
        ComponentValue::Percentage(p) => Some(Dim::Percent(*p)),
        ComponentValue::Number(n) if *n == 0.0 => Some(Dim::Px(0.0)),
        _ => Some(Dim::Px(length(value, ctx, percent_base)?)),
    }
}

fn contains_percentage(components: &[ComponentValue]) -> bool {
    components.iter().any(|component| match component {
        ComponentValue::Percentage(_) => true,
        ComponentValue::Function(_, args) => contains_percentage(args),
        _ => false,
    })
}

/// True for a CSS math function we can evaluate: `calc`, `min`, `max` or
/// `clamp` (matched case-insensitively).
fn is_math_fn_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("calc")
        || name.eq_ignore_ascii_case("min")
        || name.eq_ignore_ascii_case("max")
        || name.eq_ignore_ascii_case("clamp")
}

/// Evaluates a `min()` / `max()` / `clamp()` function to CSS pixels.
///
/// Each comma-separated argument is a `<calc-sum>` evaluated with [`eval_calc`],
/// so percentages resolve against `percent_base` and nested `calc()` (or other
/// math functions) work naturally. `calc()` itself is handled by delegating to
/// [`eval_calc`].
#[allow(clippy::manual_clamp)]
fn eval_math_function(
    name: &str,
    args: &[ComponentValue],
    ctx: &InterpretContext,
    percent_base: Option<f32>,
) -> Option<f32> {
    if name.eq_ignore_ascii_case("calc") {
        return eval_calc(args, ctx, percent_base);
    }

    let groups = Value {
        components: args.to_vec(),
    }
    .split_commas();
    let values = groups
        .iter()
        .map(|group| eval_calc(group, ctx, percent_base))
        .collect::<Option<Vec<_>>>()?;

    if name.eq_ignore_ascii_case("min") {
        values.into_iter().min_by(f32::total_cmp)
    } else if name.eq_ignore_ascii_case("max") {
        values.into_iter().max_by(f32::total_cmp)
    } else if name.eq_ignore_ascii_case("clamp") && values.len() == 3 {
        // clamp(MIN, VAL, MAX) == max(MIN, min(VAL, MAX)). This ordering also
        // handles MIN > MAX (CSS ignores MAX then) without panicking.
        Some(values[0].max(values[1].min(values[2])))
    } else {
        None
    }
}

/// Resolves a length to CSS pixels.
pub fn length(value: &Value, ctx: &InterpretContext, percent_base: Option<f32>) -> Option<f32> {
    match value.first()? {
        ComponentValue::Function(name, args) if is_math_fn_name(name) => {
            eval_math_function(name, args, ctx, percent_base)
        }
        ComponentValue::Percentage(p) => Some(percent_base.unwrap_or(0.0) * p / 100.0),
        ComponentValue::Number(n) => Some(*n as f32),
        ComponentValue::Dimension(n, unit) => dimension_px(*n, unit, ctx),
        _ => {
            let length = value.as_length()?;
            Some(resolve_css_length(length, ctx, percent_base))
        }
    }
}

fn dimension_px(value: f64, unit: &str, ctx: &InterpretContext) -> Option<f32> {
    let value = value as f32;
    Some(match unit.to_ascii_lowercase().as_str() {
        "px" => value,
        "pt" => value * 4.0 / 3.0,
        "pc" => value * 16.0,
        "in" => value * 96.0,
        "cm" => value * 96.0 / 2.54,
        "mm" => value * 96.0 / 25.4,
        "q" => value * 96.0 / 101.6,
        "em" => value * ctx.font_size,
        "rem" => value * ctx.root_font_size,
        "ex" => value * ctx.font_size * 0.5,
        "ch" => value * ctx.font_size * 0.5,
        "vw" => value * ctx.viewport_width / 100.0,
        "vh" => value * ctx.viewport_height / 100.0,
        "vmin" => value * ctx.viewport_width.min(ctx.viewport_height) / 100.0,
        "vmax" => value * ctx.viewport_width.max(ctx.viewport_height) / 100.0,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
enum CalcToken {
    Number(f32),
    Op(char),
    Paren(bool), // true = open
}

/// Flattens calc arguments into arithmetic tokens, resolving each term to
/// pixels immediately. Percentages resolve against `percent_base`, so
/// `calc(100% - 10px)` works for widths and paddings alike.
fn calc_tokens(
    components: &[ComponentValue],
    ctx: &InterpretContext,
    percent_base: Option<f32>,
    out: &mut Vec<CalcToken>,
) -> bool {
    for component in components {
        match component {
            ComponentValue::Whitespace => {}
            ComponentValue::Number(n) => out.push(CalcToken::Number(*n as f32)),
            ComponentValue::Percentage(p) => {
                out.push(CalcToken::Number(percent_base.unwrap_or(0.0) * p / 100.0));
            }
            ComponentValue::Dimension(n, unit) => {
                let Some(pixels) = dimension_px(*n, unit, ctx) else {
                    return false;
                };
                out.push(CalcToken::Number(pixels));
            }
            ComponentValue::Delim(ch) if matches!(ch, '+' | '-' | '*' | '/') => {
                out.push(CalcToken::Op(*ch));
            }
            ComponentValue::Function(name, args)
                if name.is_empty() || name.eq_ignore_ascii_case("calc") =>
            {
                out.push(CalcToken::Paren(true));
                if !calc_tokens(args, ctx, percent_base, out) {
                    return false;
                }
                out.push(CalcToken::Paren(false));
            }
            ComponentValue::Function(name, args)
                if name.eq_ignore_ascii_case("min")
                    || name.eq_ignore_ascii_case("max")
                    || name.eq_ignore_ascii_case("clamp") =>
            {
                // A comparison function nested inside a calc-sum evaluates to
                // a single number, just like a nested `calc()`.
                match eval_math_function(name, args, ctx, percent_base) {
                    Some(value) => out.push(CalcToken::Number(value)),
                    None => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

/// Recursive-descent evaluator for the token stream produced by [`calc_tokens`].
fn eval_calc(
    components: &[ComponentValue],
    ctx: &InterpretContext,
    percent_base: Option<f32>,
) -> Option<f32> {
    let mut tokens = Vec::new();
    if !calc_tokens(components, ctx, percent_base, &mut tokens) {
        return None;
    }
    let mut parser = CalcParser {
        tokens: &tokens,
        position: 0,
    };
    let result = parser.expr()?;
    (parser.position == tokens.len()).then_some(result)
}

struct CalcParser<'a> {
    tokens: &'a [CalcToken],
    position: usize,
}

impl CalcParser<'_> {
    fn expr(&mut self) -> Option<f32> {
        let mut value = self.term()?;
        while let Some(CalcToken::Op(op @ ('+' | '-'))) = self.tokens.get(self.position) {
            let op = *op;
            self.position += 1;
            let rhs = self.term()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Some(value)
    }

    fn term(&mut self) -> Option<f32> {
        let mut value = self.factor()?;
        while let Some(CalcToken::Op(op @ ('*' | '/'))) = self.tokens.get(self.position) {
            let op = *op;
            self.position += 1;
            let rhs = self.factor()?;
            value = if op == '*' {
                value * rhs
            } else {
                if rhs == 0.0 {
                    return None;
                }
                value / rhs
            };
        }
        Some(value)
    }

    fn factor(&mut self) -> Option<f32> {
        match self.tokens.get(self.position)? {
            CalcToken::Op('+') => {
                self.position += 1;
                self.factor()
            }
            CalcToken::Op('-') => {
                self.position += 1;
                Some(-self.factor()?)
            }
            CalcToken::Number(n) => {
                let n = *n;
                self.position += 1;
                Some(n)
            }
            CalcToken::Paren(true) => {
                self.position += 1;
                let value = self.expr()?;
                if !matches!(
                    self.tokens.get(self.position),
                    Some(CalcToken::Paren(false))
                ) {
                    return None;
                }
                self.position += 1;
                Some(value)
            }
            _ => None,
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
    fn evaluates_calc_lengths() {
        let ctx = InterpretContext::default();
        assert_eq!(
            length(&value("calc(100% - 10px)"), &ctx, Some(200.0)),
            Some(190.0)
        );
        assert_eq!(length(&value("calc(2em + 4px)"), &ctx, None), Some(36.0));
        assert_eq!(
            length(&value("calc((10px + 5px) * 2)"), &ctx, None),
            Some(30.0)
        );
        assert_eq!(
            length(&value("calc(100% / 4)"), &ctx, Some(200.0)),
            Some(50.0)
        );
    }

    #[test]
    fn evaluates_min_max_clamp_lengths() {
        let ctx = InterpretContext::default();

        // Modern container-style width: min(1240px, 100% - 48px).
        assert_eq!(
            length(&value("min(1240px, 100% - 48px)"), &ctx, Some(800.0)),
            Some(752.0)
        );
        // Percentages resolve against the supplied base for max() too.
        assert_eq!(
            length(&value("max(100px, 10%)"), &ctx, Some(200.0)),
            Some(100.0)
        );
        assert_eq!(
            length(&value("max(100px, 10%)"), &ctx, Some(2000.0)),
            Some(200.0)
        );
        // clamp(MIN, VAL, MAX) with mixed absolute/viewport units: 2vw is
        // 20.48px with the default 1024px viewport, inside [1rem, 3rem].
        assert_eq!(
            length(&value("clamp(1rem, 2vw, 3rem)"), &ctx, None),
            Some(2.0 * 1024.0 / 100.0)
        );
        // Nested calc() inside a comparison function is reused.
        assert_eq!(
            length(&value("min(calc(100% - 10px), 500px)"), &ctx, Some(200.0)),
            Some(190.0)
        );
    }

    #[test]
    fn min_max_clamp_dims_keep_symbolic_percentage() {
        let ctx = InterpretContext::default();

        // With no base, a mixed percentage/px expression stays a Dim::Calc.
        let width = dim(&value("min(1240px, 100% - 48px)"), &ctx, None);
        assert_eq!(
            width,
            Some(Dim::Calc {
                percent: 100.0,
                px: -48.0,
            })
        );
        assert_eq!(width.unwrap().resolve(800.0), Some(752.0));

        // With a known base, dim resolves straight to pixels.
        assert_eq!(
            dim(&value("max(100px, 10%)"), &ctx, Some(2000.0)),
            Some(Dim::Px(200.0))
        );

        // clamp with a percentage and no base stays symbolic...
        assert!(matches!(
            dim(&value("clamp(1rem, 50%, 3rem)"), &ctx, None),
            Some(Dim::Calc { .. })
        ));
        // ...while a pure-absolute math function is just pixels.
        assert_eq!(
            dim(&value("clamp(1rem, 2vw, 3rem)"), &ctx, None),
            Some(Dim::Px(2.0 * 1024.0 / 100.0))
        );
    }

    #[test]
    fn rejects_invalid_calc() {
        let ctx = InterpretContext::default();
        assert_eq!(length(&value("calc(10px / 0)"), &ctx, None), None);
        assert_eq!(length(&value("calc(10px + )"), &ctx, None), None);
        // clamp() needs exactly three arguments.
        assert_eq!(length(&value("clamp(1px, 2px)"), &ctx, None), None);
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
    fn intrinsic_sizing_keywords_are_kept_as_raw_text() {
        let ctx = InterpretContext::default();
        for css in [
            "fit-content",
            "max-content",
            "min-content",
            "fit-content(200px)",
        ] {
            assert!(is_intrinsic_sizing(css), "{css} should be recognised");
            assert_eq!(
                interpret(Property::Width, &value(css), &ctx),
                Some(ComputedValue::Str(css.to_string())),
                "{css}"
            );
            // There is no `Dim` variant, so the dimension helper declines them.
            assert_eq!(dim(&value(css), &ctx, None), None, "{css}");
        }
        assert!(!is_intrinsic_sizing("auto"));
        assert!(!is_intrinsic_sizing("100px"));
        assert_eq!(
            interpret(Property::Width, &value("auto"), &ctx),
            Some(ComputedValue::Dim(Dim::Auto))
        );
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

    #[test]
    fn interprets_border_radius() {
        let ctx = InterpretContext::default();
        let radius = parse_border_radius(&value("10px 20px 30px 40px / 5px 10px"), &ctx).unwrap();
        assert_eq!(
            radius.corners[0],
            CornerRadius {
                horizontal: Dim::Px(10.0),
                vertical: Some(Dim::Px(5.0)),
            }
        );
        assert_eq!(
            radius.corners[1],
            CornerRadius {
                horizontal: Dim::Px(20.0),
                vertical: Some(Dim::Px(10.0)),
            }
        );
        assert_eq!(
            radius.corners[2],
            CornerRadius {
                horizontal: Dim::Px(30.0),
                vertical: Some(Dim::Px(5.0)),
            }
        );
        assert_eq!(
            radius.corners[3],
            CornerRadius {
                horizontal: Dim::Px(40.0),
                vertical: Some(Dim::Px(10.0)),
            }
        );

        // Three values: top-left, top-right/bottom-left, bottom-right.
        let radius = parse_border_radius(&value("1px 2px 3px"), &ctx).unwrap();
        assert_eq!(radius.corners[0].horizontal, Dim::Px(1.0));
        assert_eq!(radius.corners[1].horizontal, Dim::Px(2.0));
        assert_eq!(radius.corners[2].horizontal, Dim::Px(3.0));
        assert_eq!(radius.corners[3].horizontal, Dim::Px(2.0));

        // Percentages are preserved for layout to resolve.
        let radius = parse_border_radius(&value("50%"), &ctx).unwrap();
        assert_eq!(radius.corners[0].horizontal, Dim::Percent(50.0));
        assert_eq!(radius.corners[0].vertical, None);
    }

    #[test]
    fn interprets_shadow_lists() {
        let ctx = InterpretContext::default();
        let shadows = parse_shadow_list(
            &value("0 2px 4px 1px rgba(0, 0, 0, 0.5), inset 0 0 10px red"),
            &ctx,
            true,
            true,
        )
        .unwrap();
        assert_eq!(shadows.len(), 2);
        assert_eq!(shadows[0].offset_x, 0.0);
        assert_eq!(shadows[0].offset_y, 2.0);
        assert_eq!(shadows[0].blur, 4.0);
        assert_eq!(shadows[0].spread, 1.0);
        assert_eq!(shadows[0].color, Color::rgba(0, 0, 0, 128));
        assert!(!shadows[0].inset);
        assert!(shadows[1].inset);
        assert_eq!(shadows[1].color, Color::rgb(255, 0, 0));

        // `none` yields an empty list, and text shadows have no spread/inset.
        assert!(
            parse_shadow_list(&value("none"), &ctx, true, true)
                .unwrap()
                .is_empty()
        );
        let text = parse_shadow_list(&value("1px 1px 2px blue"), &ctx, false, false).unwrap();
        assert_eq!(text[0].spread, 0.0);
        assert!(!text[0].inset);
    }

    #[test]
    fn interprets_transform_and_filter() {
        let ctx = InterpretContext::default();
        let transform =
            parse_transform(&value("translate(-50%, 10px) rotate(45deg) scale(2)"), &ctx).unwrap();
        assert_eq!(transform.len(), 3);
        assert_eq!(
            transform[0],
            TransformFunction::Translate(Dim::Percent(-50.0), Dim::Px(10.0))
        );
        assert_eq!(transform[1], TransformFunction::Rotate(45.0));
        assert_eq!(transform[2], TransformFunction::Scale(2.0, 2.0));
        assert!(parse_transform(&value("none"), &ctx).unwrap().is_empty());

        let filter = parse_filter(
            &value("blur(2px) brightness(120%) hue-rotate(0.5turn)"),
            &ctx,
        )
        .unwrap();
        assert_eq!(filter.len(), 3);
        assert_eq!(filter[0], FilterFunction::Blur(2.0));
        assert_eq!(filter[1], FilterFunction::Brightness(1.2));
        assert_eq!(filter[2], FilterFunction::HueRotate(180.0));
        assert!(parse_filter(&value("none"), &ctx).unwrap().is_empty());
    }

    #[test]
    fn interprets_grid_properties_as_raw_text() {
        let ctx = InterpretContext::default();
        let cases = [
            (Property::GridColumn, "span 2"),
            (Property::GridRow, "1 / 3"),
            (Property::GridArea, "head"),
            (Property::GridTemplateAreas, "\"head head\" \"side main\""),
            (Property::GridAutoRows, "25px"),
            (Property::GridAutoColumns, "minmax(10px, 1fr)"),
            (Property::GridAutoFlow, "column"),
            (Property::JustifyItems, "center"),
            (Property::JustifySelf, "end"),
            (Property::AlignSelf, "start"),
            (Property::AlignContent, "space-between"),
        ];
        for (property, css) in cases {
            assert_eq!(
                interpret(property, &value(css), &ctx),
                Some(ComputedValue::Str(css.to_string())),
                "{property:?} / {css}"
            );
        }
    }

    #[test]
    fn interprets_aspect_ratio_and_object_fit() {
        let ctx = InterpretContext::default();
        assert_eq!(
            parse_aspect_ratio(&value("16 / 9")),
            Some(AspectRatio::Ratio(16.0, 9.0))
        );
        assert_eq!(parse_aspect_ratio(&value("auto")), Some(AspectRatio::Auto));
        assert_eq!(
            interpret(Property::ObjectFit, &value("cover"), &ctx),
            Some(ComputedValue::ObjectFit(ObjectFit::Cover))
        );
        assert!(interpret(Property::ObjectFit, &value("banana"), &ctx).is_none());
    }

    #[test]
    fn background_image_preserves_urls_none_and_gradients() {
        let ctx = InterpretContext::default();
        // url() and `none` keep their original representation.
        assert_eq!(
            interpret(Property::BackgroundImage, &value("url(a.png)"), &ctx),
            Some(ComputedValue::Str("a.png".to_string()))
        );
        assert_eq!(
            interpret(Property::BackgroundImage, &value("none"), &ctx),
            Some(ComputedValue::Str("none".to_string()))
        );
        // Gradients survive as raw CSS text the paint crate can parse.
        assert_eq!(
            interpret(
                Property::BackgroundImage,
                &value("linear-gradient(90deg, red, blue)"),
                &ctx
            ),
            Some(ComputedValue::Str(
                "linear-gradient(90deg, red, blue)".to_string()
            ))
        );
        assert_eq!(
            interpret(
                Property::BackgroundImage,
                &value("radial-gradient(circle, red, blue)"),
                &ctx
            ),
            Some(ComputedValue::Str(
                "radial-gradient(circle, red, blue)".to_string()
            ))
        );
        // Whitespace runs are normalized to single spaces.
        assert_eq!(
            interpret(
                Property::BackgroundImage,
                &value("linear-gradient(  90deg ,  red , blue  )"),
                &ctx
            ),
            Some(ComputedValue::Str(
                "linear-gradient( 90deg , red , blue )".to_string()
            ))
        );
        // Arbitrary values are still rejected.
        assert!(interpret(Property::BackgroundImage, &value("red"), &ctx).is_none());
        assert!(interpret(Property::BackgroundImage, &value("10px"), &ctx).is_none());
    }

    #[test]
    fn background_image_joins_multiple_layers() {
        let ctx = InterpretContext::default();
        assert_eq!(
            interpret(
                Property::BackgroundImage,
                &value("url(a.png), linear-gradient(90deg, red, blue)"),
                &ctx
            ),
            Some(ComputedValue::Str(
                "url(a.png), linear-gradient(90deg, red, blue)".to_string()
            ))
        );
        assert_eq!(
            interpret(
                Property::BackgroundImage,
                &value("linear-gradient(red, blue), radial-gradient(circle, black, white)"),
                &ctx
            ),
            Some(ComputedValue::Str(
                "linear-gradient(red, blue), radial-gradient(circle, black, white)".to_string()
            ))
        );
    }
}
