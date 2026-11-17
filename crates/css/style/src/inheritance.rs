//! Inheritance and explicit `inherit`/`initial`/`unset` handling.

use browser_css::ComponentValue;
use browser_css::Value;

use crate::computed::ComputedStyle;
use crate::properties::{Property, initial_value};

/// Special CSS-wide keywords that must be resolved during the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WideKeyword {
    /// `inherit`
    Inherit,
    /// `initial`
    Initial,
    /// `unset`
    Unset,
    /// `revert` (treated as `unset` for a single-origin engine)
    Revert,
}

impl WideKeyword {
    /// Detects a CSS-wide keyword at the start of a value.
    pub fn detect(value: &Value) -> Option<Self> {
        if value.components.len() != 1 {
            return None;
        }
        match value.components.first() {
            Some(ComponentValue::Ident(ident)) => match ident.to_ascii_lowercase().as_str() {
                "inherit" => Some(Self::Inherit),
                "initial" => Some(Self::Initial),
                "unset" => Some(Self::Unset),
                "revert" => Some(Self::Revert),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Applies a CSS-wide keyword, returning the value to use.
pub fn apply_wide(
    keyword: WideKeyword,
    property: Property,
    parent: Option<&ComputedStyle>,
) -> Option<crate::computed::ComputedValue> {
    let inherited = property.is_inherited();
    match keyword {
        WideKeyword::Inherit => parent.and_then(|p| p.get(property).cloned()),
        WideKeyword::Initial => initial_value(property),
        WideKeyword::Unset | WideKeyword::Revert => {
            if inherited {
                parent.and_then(|p| p.get(property).cloned())
            } else {
                initial_value(property)
            }
        }
    }
}

/// Fills in inherited properties that were not declared.
pub fn inherit_missing(style: &mut ComputedStyle, parent: Option<&ComputedStyle>) {
    let Some(parent) = parent else { return };
    for property in Property::ALL {
        if !property.is_inherited() {
            continue;
        }
        if style.get(*property).is_none() {
            style.inherit_property(*property, parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computed::ComputedValue;

    #[test]
    fn detects_wide_keywords() {
        let value = browser_css::parse_declarations("color: inherit")
            .remove(0)
            .value;
        assert_eq!(WideKeyword::detect(&value), Some(WideKeyword::Inherit));
        let value = browser_css::parse_declarations("color: red")
            .remove(0)
            .value;
        assert_eq!(WideKeyword::detect(&value), None);
    }

    #[test]
    fn unset_inherits_inherited_properties_only() {
        let mut parent = ComputedStyle::initial();
        parent.set(
            Property::Color,
            ComputedValue::Color(browser_css::Color::rgb(1, 2, 3)),
        );
        parent.set(
            Property::Width,
            ComputedValue::Dim(crate::computed::Dim::Px(99.0)),
        );

        let inherited = apply_wide(WideKeyword::Unset, Property::Color, Some(&parent)).unwrap();
        assert_eq!(
            inherited,
            ComputedValue::Color(browser_css::Color::rgb(1, 2, 3))
        );
        let non_inherited = apply_wide(WideKeyword::Unset, Property::Width, Some(&parent)).unwrap();
        assert_eq!(non_inherited, initial_value(Property::Width).unwrap());
    }

    #[test]
    fn inherit_missing_copies_parent_inherited_props() {
        let mut parent = ComputedStyle::initial();
        parent.set(Property::FontSize, ComputedValue::Px(24.0));
        let mut child = ComputedStyle::default();
        child.set(
            Property::Width,
            ComputedValue::Dim(crate::computed::Dim::Px(10.0)),
        );
        inherit_missing(&mut child, Some(&parent));
        assert_eq!(child.font_size(), 24.0);
    }
}
