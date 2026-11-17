//! Declaration cascade: specificity, origins, importance and shorthand expansion.

use browser_css::{ComponentValue, Declaration, Origin, Value};
use browser_selectors::Specificity;

use crate::computed::{
    Side, border_color_property, border_style_property, border_width_property, margin_property,
    padding_property,
};
use crate::properties::Property;

/// A declaration that matched an element, with its cascade weight inputs.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Longhand property.
    pub property: Property,
    /// Declared value (already expanded from shorthands).
    pub value: Value,
    /// `!important`
    pub important: bool,
    /// Stylesheet origin.
    pub origin: Origin,
    /// Selector specificity (inline styles use the inline flag instead).
    pub specificity: Specificity,
    /// True for `style=""` declarations.
    pub inline: bool,
    /// Document order of the declaration.
    pub order: usize,
}

impl Candidate {
    /// The sort key; larger keys win.
    pub fn weight(&self) -> (u8, u8, u8, u32, u32, u32, usize) {
        let important = u8::from(self.important);
        let origin = match (self.important, self.origin) {
            (false, Origin::UserAgent) | (true, Origin::Author) => 0,
            (_, Origin::User) => 1,
            (false, Origin::Author) | (true, Origin::UserAgent) => 2,
        };
        (
            important,
            origin,
            u8::from(self.inline),
            self.specificity.a,
            self.specificity.b,
            self.specificity.c,
            self.order,
        )
    }
}

/// Picks the winning declaration per property.
pub fn cascade(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut winners: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        match winners
            .iter_mut()
            .find(|w| w.property == candidate.property)
        {
            Some(current) => {
                if candidate.weight() >= current.weight() {
                    *current = candidate;
                }
            }
            None => winners.push(candidate),
        }
    }
    winners
}

/// Expands a declaration into longhand properties.
pub fn expand(declaration: &Declaration) -> Vec<(Property, Value)> {
    let name = declaration.name.to_ascii_lowercase();
    let value = &declaration.value;
    let groups = component_groups(value);

    let sides = |props: [Property; 4]| -> Option<Vec<(Property, Value)>> {
        let values = match groups.len() {
            1 => vec![groups[0].clone(); 4],
            2 => vec![
                groups[0].clone(),
                groups[1].clone(),
                groups[0].clone(),
                groups[1].clone(),
            ],
            3 => vec![
                groups[0].clone(),
                groups[1].clone(),
                groups[2].clone(),
                groups[1].clone(),
            ],
            4 => groups.clone(),
            _ => return None,
        };
        Some(props.into_iter().zip(values).collect())
    };

    let all_sides = |template: fn(Side) -> Property| {
        [
            template(Side::Top),
            template(Side::Right),
            template(Side::Bottom),
            template(Side::Left),
        ]
    };

    match name.as_str() {
        "margin" => sides(all_sides(margin_property)).unwrap_or_default(),
        "padding" => sides(all_sides(padding_property)).unwrap_or_default(),
        "inset" => sides([
            Property::Top,
            Property::Right,
            Property::Bottom,
            Property::Left,
        ])
        .unwrap_or_default(),
        "border-width" => sides(all_sides(border_width_property)).unwrap_or_default(),
        "border-style" => sides(all_sides(border_style_property)).unwrap_or_default(),
        "border-color" => sides(all_sides(border_color_property)).unwrap_or_default(),
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let (width, style, color) = parse_border(&groups);
            let targets: Vec<Side> = match name.as_str() {
                "border-top" => vec![Side::Top],
                "border-right" => vec![Side::Right],
                "border-bottom" => vec![Side::Bottom],
                "border-left" => vec![Side::Left],
                _ => vec![Side::Top, Side::Right, Side::Bottom, Side::Left],
            };
            let mut out = Vec::new();
            for side in targets {
                if let Some(width) = &width {
                    out.push((border_width_property(side), width.clone()));
                }
                if let Some(style) = &style {
                    out.push((border_style_property(side), style.clone()));
                }
                if let Some(color) = &color {
                    out.push((border_color_property(side), color.clone()));
                }
            }
            out
        }
        "background" => {
            let color = groups
                .iter()
                .find(|g| g.as_color().is_some())
                .map(|g| (Property::BackgroundColor, g.clone()));
            let image = groups
                .iter()
                .find(|g| g.as_url().is_some() || g.ident() == Some("none"))
                .map(|g| (Property::BackgroundImage, g.clone()));
            color.into_iter().chain(image).collect()
        }
        "overflow" => {
            let first = groups.first().cloned().unwrap_or_default();
            let second = groups.get(1).cloned().unwrap_or_else(|| first.clone());
            vec![(Property::OverflowX, first), (Property::OverflowY, second)]
        }
        "gap" | "grid-gap" => {
            let first = groups.first().cloned().unwrap_or_default();
            let second = groups.get(1).cloned().unwrap_or_else(|| first.clone());
            vec![(Property::RowGap, first), (Property::ColumnGap, second)]
        }
        "flex" => {
            let mut out = Vec::new();
            for group in &groups {
                if let Some(n) = group.as_number() {
                    if out.is_empty() {
                        out.push((Property::FlexGrow, group.clone()));
                        let _ = n;
                    } else {
                        out.push((Property::FlexShrink, group.clone()));
                    }
                } else if group.ident() == Some("auto") {
                    out.push((Property::FlexBasis, group.clone()));
                } else if group.ident() == Some("none") {
                    out.push((Property::FlexGrow, Value::default()));
                } else {
                    out.push((Property::FlexBasis, group.clone()));
                }
            }
            out
        }
        "flex-flow" => groups
            .iter()
            .map(|g| {
                if matches!(
                    g.ident(),
                    Some("wrap") | Some("nowrap") | Some("wrap-reverse")
                ) {
                    (Property::FlexWrap, g.clone())
                } else {
                    (Property::FlexDirection, g.clone())
                }
            })
            .collect(),
        "list-style" => groups
            .iter()
            .filter(|g| g.as_url().is_none())
            .map(|g| (Property::ListStyleType, g.clone()))
            .collect(),
        "text-decoration" => groups
            .iter()
            .find(|g| {
                matches!(
                    g.ident(),
                    Some("none" | "underline" | "line-through" | "overline")
                )
            })
            .map(|g| vec![(Property::TextDecoration, g.clone())])
            .unwrap_or_default(),
        "font" => groups
            .iter()
            .find(|g| g.as_length().is_some())
            .map(|g| vec![(Property::FontSize, g.clone())])
            .unwrap_or_default(),
        _ => Property::from_name(&name)
            .map(|property| vec![(property, value.clone())])
            .unwrap_or_default(),
    }
}

fn parse_border(groups: &[Value]) -> (Option<Value>, Option<Value>, Option<Value>) {
    let mut width = None;
    let mut style = None;
    let mut color = None;
    for group in groups {
        if let Some(ident) = group.ident() {
            if matches!(
                ident,
                "thin" | "medium" | "thick" | "0" | "1px" | "2px" | "3px"
            ) && group.as_length().is_some()
            {
                width = Some(group.clone());
                continue;
            }
            if matches!(
                ident,
                "none"
                    | "hidden"
                    | "solid"
                    | "dashed"
                    | "dotted"
                    | "double"
                    | "groove"
                    | "ridge"
                    | "inset"
                    | "outset"
            ) {
                style = Some(group.clone());
                continue;
            }
        }
        if group.as_color().is_some() {
            color = Some(group.clone());
        } else if group.as_length().is_some() {
            width = Some(group.clone());
        }
    }
    (width, style, color)
}

/// Splits a value into whitespace-separated top-level components.
pub fn component_groups(value: &Value) -> Vec<Value> {
    let mut groups = Vec::new();
    let mut current: Vec<ComponentValue> = Vec::new();
    for component in &value.components {
        if *component == ComponentValue::Whitespace {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(css: &str) -> Declaration {
        browser_css::parse_declarations(css)
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn expands_four_side_shorthands() {
        let expanded = expand(&decl("margin: 1px 2px 3px"));
        assert_eq!(expanded.len(), 4);
        let map: std::collections::HashMap<_, _> = expanded.into_iter().collect();
        assert_eq!(
            map[&Property::MarginTop].as_length().unwrap(),
            browser_css::Length::Px(1.0)
        );
        assert_eq!(
            map[&Property::MarginRight].as_length().unwrap(),
            browser_css::Length::Px(2.0)
        );
        assert_eq!(
            map[&Property::MarginBottom].as_length().unwrap(),
            browser_css::Length::Px(3.0)
        );
        assert_eq!(
            map[&Property::MarginLeft].as_length().unwrap(),
            browser_css::Length::Px(2.0)
        );
    }

    #[test]
    fn expands_border_shorthand_to_all_sides() {
        let expanded = expand(&decl("border: 2px solid red"));
        assert!(expanded.iter().any(|(p, _)| *p == Property::BorderTopWidth));
        assert!(
            expanded
                .iter()
                .any(|(p, v)| *p == Property::BorderLeftStyle && v.ident() == Some("solid"))
        );
        assert!(
            expanded
                .iter()
                .any(|(p, v)| *p == Property::BorderBottomColor && v.as_color().is_some())
        );
    }

    #[test]
    fn cascade_respects_specificity_and_order() {
        let specificity = |a, b, c| Specificity::new(a, b, c);
        let value = |s: &str| {
            browser_css::parse_declarations(&format!("color: {s}"))
                .into_iter()
                .next()
                .unwrap()
                .value
        };
        let candidates = vec![
            Candidate {
                property: Property::Color,
                value: value("red"),
                important: false,
                origin: Origin::Author,
                specificity: specificity(0, 1, 0),
                inline: false,
                order: 5,
            },
            Candidate {
                property: Property::Color,
                value: value("blue"),
                important: false,
                origin: Origin::Author,
                specificity: specificity(1, 0, 0),
                inline: false,
                order: 1,
            },
        ];
        let winners = cascade(candidates);
        assert_eq!(
            winners[0].value.as_color().unwrap(),
            browser_css::Color::rgb(0, 0, 255)
        );
    }

    #[test]
    fn important_beats_specificity() {
        let value = |s: &str| {
            browser_css::parse_declarations(&format!("color: {s} !important"))
                .into_iter()
                .next()
                .unwrap()
                .value
        };
        let candidates = vec![
            Candidate {
                property: Property::Color,
                value: value("red"),
                important: true,
                origin: Origin::Author,
                specificity: Specificity::new(0, 0, 1),
                inline: false,
                order: 0,
            },
            Candidate {
                property: Property::Color,
                value: browser_css::parse_declarations("color: blue")
                    .remove(0)
                    .value,
                important: false,
                origin: Origin::Author,
                specificity: Specificity::new(9, 9, 9),
                inline: true,
                order: 9,
            },
        ];
        let winners = cascade(candidates);
        assert_eq!(
            winners[0].value.as_color().unwrap(),
            browser_css::Color::rgb(255, 0, 0)
        );
    }
}
