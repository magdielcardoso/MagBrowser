//! CSS custom properties and `var()` substitution.

use std::collections::HashMap;

use browser_css::{ComponentValue, Value};

/// True when `name` is a custom property (`--foo`).
pub fn is_custom_property(name: &str) -> bool {
    name.starts_with("--")
}

/// Splits a component list on top-level commas.
pub fn split_on_commas(components: &[ComponentValue]) -> Vec<Vec<ComponentValue>> {
    let mut groups = vec![Vec::new()];
    for component in components {
        if *component == ComponentValue::Comma {
            groups.push(Vec::new());
        } else {
            groups
                .last_mut()
                .expect("groups always has one element")
                .push(component.clone());
        }
    }
    groups
}

fn trim_whitespace(mut components: Vec<ComponentValue>) -> Vec<ComponentValue> {
    while matches!(components.first(), Some(ComponentValue::Whitespace)) {
        components.remove(0);
    }
    while matches!(components.last(), Some(ComponentValue::Whitespace)) {
        components.pop();
    }
    components
}

/// Resolves `var()` references inside `value` using `customs`.
///
/// Returns `None` when a referenced property is missing and has no fallback,
/// which makes the declaration invalid at computed-value time (as CSS requires).
pub fn substitute(value: &Value, customs: &HashMap<String, Value>) -> Option<Value> {
    let components = substitute_components(&value.components, customs, 0)?;
    Some(Value {
        components: trim_whitespace(components),
    })
}

fn substitute_components(
    components: &[ComponentValue],
    customs: &HashMap<String, Value>,
    depth: usize,
) -> Option<Vec<ComponentValue>> {
    if depth > 32 {
        return None;
    }
    let mut out = Vec::with_capacity(components.len());
    for component in components {
        match component {
            ComponentValue::Function(name, args) if name.eq_ignore_ascii_case("var") => {
                let groups = split_on_commas(args);
                let name_components = trim_whitespace(groups.first().cloned().unwrap_or_default());
                let property = match name_components.first() {
                    Some(ComponentValue::Ident(ident)) if is_custom_property(ident) => {
                        ident.clone()
                    }
                    _ => return None,
                };
                if let Some(resolved) = customs.get(&property) {
                    out.extend(substitute_components(
                        &resolved.components,
                        customs,
                        depth + 1,
                    )?);
                } else {
                    let fallback = groups.get(1)?;
                    out.extend(substitute_components(fallback, customs, depth + 1)?);
                }
            }
            ComponentValue::Function(name, args) => {
                let inner = substitute_components(args, customs, depth + 1)?;
                out.push(ComponentValue::Function(name.clone(), inner));
            }
            other => out.push(other.clone()),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(s: &str) -> ComponentValue {
        ComponentValue::Ident(s.to_string())
    }

    #[test]
    fn substitutes_custom_property() {
        let mut customs = HashMap::new();
        customs.insert(
            "--brand".to_string(),
            Value {
                components: vec![ComponentValue::Hash("ff0000".to_string())],
            },
        );
        let value = Value {
            components: vec![
                ComponentValue::Function("var".to_string(), vec![ident("--brand")]),
                ComponentValue::Whitespace,
                ident("solid"),
            ],
        };
        let resolved = substitute(&value, &customs).unwrap();
        assert_eq!(resolved.components.len(), 3);
        assert_eq!(
            resolved.components[0],
            ComponentValue::Hash("ff0000".into())
        );
    }

    #[test]
    fn uses_fallback_when_missing() {
        let customs = HashMap::new();
        let value = Value {
            components: vec![ComponentValue::Function(
                "var".to_string(),
                vec![ident("--missing"), ComponentValue::Comma, ident("blue")],
            )],
        };
        let resolved = substitute(&value, &customs).unwrap();
        assert_eq!(resolved.components, vec![ident("blue")]);
    }

    #[test]
    fn missing_without_fallback_is_invalid() {
        let value = Value {
            components: vec![ComponentValue::Function(
                "var".to_string(),
                vec![ident("--missing")],
            )],
        };
        assert!(substitute(&value, &HashMap::new()).is_none());
    }

    #[test]
    fn nested_var_resolves() {
        let mut customs = HashMap::new();
        customs.insert(
            "--a".to_string(),
            Value {
                components: vec![ComponentValue::Function(
                    "var".to_string(),
                    vec![ident("--b")],
                )],
            },
        );
        customs.insert(
            "--b".to_string(),
            Value {
                components: vec![ComponentValue::Percentage(10.0)],
            },
        );
        let value = Value {
            components: vec![ComponentValue::Function(
                "var".to_string(),
                vec![ident("--a")],
            )],
        };
        let resolved = substitute(&value, &customs).unwrap();
        assert_eq!(resolved.components, vec![ComponentValue::Percentage(10.0)]);
    }
}
