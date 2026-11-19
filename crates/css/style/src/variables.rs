//! CSS custom properties and `var()` substitution.
//!
//! Substitution works on the parsed component stream rather than on source
//! text. That matters for real sites: a referenced value may contain nested
//! `var()` calls, commas inside functions (`rgb(1, 2, 3)`), or several tokens
//! that need to stay intact when they are substituted into a shorthand such
//! as `margin: var(--m)` or into another function such as
//! `rgb(var(--r), 0, 0)`.
//!
//! The low-level entry point is [`substitute`], which keeps the original API.
//! Callers that need `@property` registrations or a chain of inherited scopes
//! can use [`substitute_scoped`] or [`substitute_with_registry`].

use std::collections::HashMap;

use browser_css::{ComponentValue, Value};

/// Maximum number of nested `var()`/custom-property expansions performed
/// before substitution is abandoned.
///
/// This is a safety net for pathologically deep values. Ordinary cycles are
/// detected earlier by tracking the property names currently being expanded
/// (see [`SubstitutionInfo::cycle_detected`]).
pub const MAX_VAR_DEPTH: usize = 64;

/// True when `name` is a custom property (`--foo`).
///
/// Custom property names are case-sensitive; only the leading `--` matters
/// here.
pub fn is_custom_property(name: &str) -> bool {
    name.starts_with("--")
}

/// Splits a component list on top-level commas.
///
/// Commas belonging to nested functions or parenthesised groups are kept
/// inside their component, so `rgb(1, 2, 3), red` splits into two groups while
/// the `rgb()` arguments stay together. This is what makes a `var()` fallback
/// such as `var(--shadow, 0 2px rgba(0, 0, 0, 0.3))` parse correctly.
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

/// Rejoins comma-separated groups into a single component list.
fn join_with_commas(groups: &[Vec<ComponentValue>]) -> Vec<ComponentValue> {
    let mut out = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            out.push(ComponentValue::Comma);
        }
        out.extend(group.iter().cloned());
    }
    out
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

/// Registration metadata for a custom property declared with `@property`.
///
/// These descriptors let the resolver supply a *typed fallback*: the
/// `initial-value` of a registered property is used when a `var()` reference
/// cannot be resolved to a usable value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PropertyRegistration {
    /// The registered `syntax` descriptor (e.g. `"<color>"`), when present.
    pub syntax: Option<String>,
    /// The registered `inherits` descriptor. Non-inheriting registered
    /// properties are looked up in the nearest scope only.
    pub inherits: bool,
    /// The registered `initial-value`, used as a typed fallback.
    pub initial_value: Option<Value>,
}

/// Registered custom properties, keyed by their (case-sensitive) name.
pub type PropertyRegistry = HashMap<String, PropertyRegistration>;

/// Diagnostics collected while resolving a value.
///
/// All flags default to `false`. They let a caller distinguish "the value was
/// simply missing" from "a reference cycle had to be broken" without changing
/// the `Option<Value>`-based public API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubstitutionInfo {
    /// A `var()` fallback (the part after the first top-level comma) was used.
    pub used_fallback: bool,
    /// A registered property's `initial-value` was used as a typed fallback.
    pub used_typed_fallback: bool,
    /// A custom property referenced itself, directly or indirectly.
    pub cycle_detected: bool,
    /// A `var()` had no usable value, fallback or typed fallback; the
    /// declaration is invalid at computed-value time.
    pub unresolved: bool,
}

/// The result of a detailed substitution.
#[derive(Debug, Clone, PartialEq)]
pub struct Substitution {
    /// The resolved value, or `None` when the declaration is invalid at
    /// computed-value time.
    pub value: Option<Value>,
    /// What happened during resolution.
    pub info: SubstitutionInfo,
}

/// Resolves `var()` references inside `value` using `customs`.
///
/// Returns `None` when a referenced property is missing (and has no fallback
/// or typed fallback), which makes the declaration invalid at computed-value
/// time, as CSS requires. Reference cycles never loop: they are treated like
/// missing properties so a fallback can take over.
pub fn substitute(value: &Value, customs: &HashMap<String, Value>) -> Option<Value> {
    let registry = PropertyRegistry::new();
    let scopes = [customs];
    substitute_scoped(value, &scopes, &registry).value
}

/// Like [`substitute`], but also honours `@property` registrations.
///
/// A registered `initial-value` is used when a `var()` reference cannot be
/// resolved to a usable value. An explicit fallback in the `var()` still takes
/// precedence over the typed fallback.
pub fn substitute_with_registry(
    value: &Value,
    customs: &HashMap<String, Value>,
    registry: &PropertyRegistry,
) -> Option<Value> {
    let scopes = [customs];
    substitute_scoped(value, &scopes, registry).value
}

/// Resolves `var()` references using an ordered chain of custom-property
/// scopes plus optional `@property` registrations.
///
/// `scopes` is ordered from the current element outwards (nearest ancestor
/// first). Looking a property up walks every scope, so inheritance works along
/// any element path. A registered property with `inherits: false` is looked up
/// in the nearest scope only, matching `@property` semantics.
pub fn substitute_scoped<'a>(
    value: &Value,
    scopes: &'a [&'a HashMap<String, Value>],
    registry: &'a PropertyRegistry,
) -> Substitution {
    let mut resolver = Resolver {
        scopes,
        registry,
        info: SubstitutionInfo::default(),
        stack: Vec::new(),
    };
    let components = resolver.components(&value.components, 0);
    let value = components.map(|components| Value {
        components: trim_whitespace(components),
    });
    Substitution {
        value,
        info: resolver.info,
    }
}

/// Merges custom-property scopes into one map, with later scopes winning.
///
/// Pass scopes from the root element down to the current element so that every
/// element path inherits correctly. This is a convenience for callers that
/// need a flat map (for example to store on a styled node).
pub fn inherit_customs<'a>(
    scopes: impl IntoIterator<Item = &'a HashMap<String, Value>>,
) -> HashMap<String, Value> {
    let mut merged = HashMap::new();
    for scope in scopes {
        for (name, value) in scope {
            merged.insert(name.clone(), value.clone());
        }
    }
    merged
}

struct Resolver<'a> {
    scopes: &'a [&'a HashMap<String, Value>],
    registry: &'a PropertyRegistry,
    info: SubstitutionInfo,
    /// Property names currently being expanded, used to break cycles.
    stack: Vec<String>,
}

impl<'a> Resolver<'a> {
    /// Looks a property up in the nearest scope that declares it.
    fn find(&self, name: &str) -> Option<&'a Value> {
        if self
            .registry
            .get(name)
            .is_some_and(|registration| !registration.inherits)
        {
            return self.scopes.first().and_then(|scope| scope.get(name));
        }
        self.scopes.iter().find_map(|scope| scope.get(name))
    }

    fn components(
        &mut self,
        components: &[ComponentValue],
        depth: usize,
    ) -> Option<Vec<ComponentValue>> {
        if depth > MAX_VAR_DEPTH {
            return None;
        }
        let mut out = Vec::with_capacity(components.len());
        for component in components {
            match component {
                ComponentValue::Function(name, args) if name.eq_ignore_ascii_case("var") => {
                    out.extend(self.expand_var(args, depth)?);
                }
                ComponentValue::Function(name, args) => {
                    let inner = self.components(args, depth + 1)?;
                    out.push(ComponentValue::Function(name.clone(), inner));
                }
                other => out.push(other.clone()),
            }
        }
        Some(out)
    }

    fn expand_var(&mut self, args: &[ComponentValue], depth: usize) -> Option<Vec<ComponentValue>> {
        let groups = split_on_commas(args);
        let name_components = trim_whitespace(groups.first().cloned().unwrap_or_default());
        let [ComponentValue::Ident(property)] = name_components.as_slice() else {
            self.info.unresolved = true;
            return None;
        };
        if !is_custom_property(property) {
            self.info.unresolved = true;
            return None;
        }
        let property = property.clone();

        // A property that is already being expanded forms a cycle. Treat it as
        // missing so an explicit fallback or a typed fallback can take over.
        let referenced = if self.stack.iter().any(|name| name == &property) {
            self.info.cycle_detected = true;
            None
        } else {
            self.find(&property)
                // A value with no non-whitespace tokens is the
                // guaranteed-invalid value, so it behaves like a missing one.
                .filter(|value| !value.is_empty())
        };

        if let Some(value) = referenced {
            self.stack.push(property.clone());
            let resolved = self.components(&value.components, depth + 1);
            self.stack.pop();
            if let Some(components) = resolved {
                return Some(components);
            }
            // The referenced value itself failed to resolve; fall through to
            // the fallback rather than invalidating the whole declaration.
        }

        // Everything after the first top-level comma is the fallback. It is
        // rejoined with commas so `var(--font, Arial, sans-serif)` keeps both
        // identifiers. The presence of the group matters even when it is
        // empty: `var(--x, )` is a valid way to specify an empty fallback.
        if groups.len() > 1 {
            self.info.used_fallback = true;
            let fallback = join_with_commas(&groups[1..]);
            return self.components(&fallback, depth + 1);
        }

        // Registered custom properties supply a typed fallback when they have
        // an `initial-value` descriptor and no explicit fallback was given.
        let initial = self
            .registry
            .get(&property)
            .and_then(|registration| registration.initial_value.as_ref())
            .filter(|value| !value.is_empty());
        if let Some(initial) = initial {
            self.info.used_typed_fallback = true;
            return self.components(&initial.components, depth + 1);
        }

        self.info.unresolved = true;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(input: &str) -> Value {
        Value::parse(input)
    }

    fn customs(entries: &[(&str, &str)]) -> HashMap<String, Value> {
        entries
            .iter()
            .map(|(name, val)| ((*name).to_string(), value(val)))
            .collect()
    }

    fn ident(s: &str) -> ComponentValue {
        ComponentValue::Ident(s.to_string())
    }

    fn expanded(input: &str, declarations: &[(&str, &str)]) -> Option<Value> {
        substitute(&value(input), &customs(declarations))
    }

    fn expanded_scoped(
        input: &str,
        scopes: &[&HashMap<String, Value>],
        registry: &PropertyRegistry,
    ) -> Substitution {
        substitute_scoped(&value(input), scopes, registry)
    }

    #[test]
    fn detects_custom_properties() {
        assert!(is_custom_property("--brand"));
        assert!(is_custom_property("---x"));
        assert!(!is_custom_property("color"));
        assert!(!is_custom_property("-x"));
    }

    #[test]
    fn split_on_commas_keeps_nested_functions_together() {
        let groups = split_on_commas(&value("rgb(1, 2, 3), red, var(--a, blue)").components);
        let meaningful = |components: &Vec<ComponentValue>| -> Vec<ComponentValue> {
            components
                .iter()
                .filter(|component| !matches!(component, ComponentValue::Whitespace))
                .cloned()
                .collect()
        };
        assert_eq!(groups.len(), 3);
        assert!(matches!(
            meaningful(&groups[0]).as_slice(),
            [ComponentValue::Function(name, _)] if name == "rgb"
        ));
        assert_eq!(meaningful(&groups[1]), vec![ident("red")]);
        assert!(matches!(
            meaningful(&groups[2]).as_slice(),
            [ComponentValue::Function(name, _)] if name == "var"
        ));
    }

    #[test]
    fn substitutes_custom_property() {
        let custom = customs(&[("--brand", "#ff0000")]);
        let resolved = substitute(&value("var(--brand) solid"), &custom).unwrap();
        assert_eq!(
            resolved.components,
            vec![
                ComponentValue::Hash("ff0000".to_string()),
                ComponentValue::Whitespace,
                ident("solid")
            ]
        );
    }

    #[test]
    fn uses_fallback_when_missing() {
        assert_eq!(
            expanded("var(--missing, blue)", &[]).unwrap().components,
            vec![ident("blue")]
        );
    }

    #[test]
    fn missing_without_fallback_is_invalid() {
        assert!(expanded("var(--missing)", &[]).is_none());
        let result = expanded_scoped(
            "var(--missing)",
            &[&HashMap::new()],
            &PropertyRegistry::new(),
        );
        assert!(result.value.is_none());
        assert!(result.info.unresolved);
        assert!(!result.info.cycle_detected);
    }

    #[test]
    fn nested_var_resolves() {
        let resolved = expanded("var(--a)", &[("--a", "var(--b)"), ("--b", "10%")]).unwrap();
        assert_eq!(resolved.components, vec![ComponentValue::Percentage(10.0)]);
    }

    #[test]
    fn nested_fallback_chain_resolves_innermost_value() {
        // --a is missing, --b is present: the inner fallback supplies the value.
        let resolved = expanded("var(--a, var(--b, red))", &[("--b", "green")]).unwrap();
        assert_eq!(resolved.components, vec![ident("green")]);

        // Both are missing: the innermost fallback is the final answer.
        let resolved = expanded("var(--a, var(--b, red))", &[]).unwrap();
        assert_eq!(resolved.components, vec![ident("red")]);

        // --a exists: its value wins and the fallbacks are never consulted.
        let resolved = expanded("var(--a, var(--b, red))", &[("--a", "blue")]).unwrap();
        assert_eq!(resolved.components, vec![ident("blue")]);
    }

    #[test]
    fn nested_fallback_inside_custom_property_resolves() {
        // A custom property whose value contains a var() is resolved against
        // its own fallback when that nested reference is missing.
        let custom = customs(&[("--color", "var(--primary, teal)")]);
        let resolved = substitute(&value("var(--color)"), &custom).unwrap();
        assert_eq!(resolved.components, vec![ident("teal")]);
    }

    #[test]
    fn fallback_with_top_level_commas_is_preserved() {
        let resolved = expanded("var(--font, Arial, sans-serif)", &[]).unwrap();
        let meaningful: Vec<ComponentValue> = resolved
            .components
            .iter()
            .filter(|component| !matches!(component, ComponentValue::Whitespace))
            .cloned()
            .collect();
        assert_eq!(
            meaningful,
            vec![ident("arial"), ComponentValue::Comma, ident("sans-serif")]
        );
        let resolved = expanded("var(--font, Arial, sans-serif)", &[("--font", "serif")]).unwrap();
        assert_eq!(resolved.components, vec![ident("serif")]);
    }

    #[test]
    fn fallback_with_functions_and_commas_is_preserved() {
        let custom = customs(&[("--shadow", "0 2px rgba(0, 0, 0, 0.3)")]);
        // The custom property wins, and its nested rgba() function (with its
        // own commas) is preserved intact through substitution.
        let resolved = substitute(&value("var(--shadow, 0 0 red)"), &custom).unwrap();
        let function = resolved
            .components
            .iter()
            .find_map(ComponentValue::as_function)
            .expect("rgba should be preserved as a function");
        assert_eq!(function.0, "rgba");
        assert_eq!(
            function
                .1
                .iter()
                .filter(|component| **component == ComponentValue::Comma)
                .count(),
            3,
            "rgba(0, 0, 0, 0.3) keeps all three of its commas"
        );
    }

    #[test]
    fn fallback_function_when_property_missing() {
        let resolved = expanded("var(--c, rgb(255, 0, 0))", &[]).unwrap();
        let resolved = resolved
            .as_color()
            .expect("rgb fallback should parse as a color");
        assert_eq!(resolved, browser_css::Color::rgb(255, 0, 0));
    }

    #[test]
    fn empty_custom_property_uses_fallback() {
        let resolved = expanded("var(--empty, red)", &[("--empty", " ")]).unwrap();
        assert_eq!(resolved.components, vec![ident("red")]);
    }

    #[test]
    fn empty_fallback_is_a_valid_value() {
        let resolved = expanded("var(--missing, )", &[]).unwrap();
        assert!(resolved.components.is_empty());
    }

    #[test]
    fn whitespace_around_var_name_is_accepted() {
        let resolved = expanded("var( --brand , red )", &[("--brand", "blue")]).unwrap();
        assert_eq!(resolved.components, vec![ident("blue")]);
    }

    #[test]
    fn invalid_var_name_is_rejected() {
        assert!(expanded("var(blue, red)", &[]).is_none());
        assert!(expanded("var(--a --b, red)", &[]).is_none());
        assert!(expanded("var()", &[]).is_none());
    }

    #[test]
    fn cyclic_reference_is_detected_and_invalid_without_fallback() {
        let custom = customs(&[("--a", "var(--b)"), ("--b", "var(--a)")]);
        let result = substitute_scoped(&value("var(--a)"), &[&custom], &PropertyRegistry::new());
        assert!(result.value.is_none());
        assert!(result.info.cycle_detected);
    }

    #[test]
    fn cyclic_reference_uses_fallback_gracefully() {
        let custom = customs(&[("--a", "var(--a, red)")]);
        let result = substitute_scoped(&value("var(--a)"), &[&custom], &PropertyRegistry::new());
        assert_eq!(
            result.value.unwrap().components,
            vec![ident("red")],
            "the inner var(--a, red) should break its own cycle"
        );
        assert!(result.info.cycle_detected);
        assert!(result.info.used_fallback);
    }

    #[test]
    fn indirect_cycle_breaks_at_the_repeated_property() {
        let custom = customs(&[
            ("--a", "var(--b)"),
            ("--b", "var(--c)"),
            ("--c", "var(--a, orange)"),
        ]);
        let result = substitute_scoped(&value("var(--a)"), &[&custom], &PropertyRegistry::new());
        assert_eq!(result.value.unwrap().components, vec![ident("orange")]);
        assert!(result.info.cycle_detected);
    }

    #[test]
    fn deep_reference_chain_terminates_at_depth_limit() {
        let mut declarations = Vec::new();
        for index in 0..(MAX_VAR_DEPTH + 8) {
            let name = format!("--p{index}");
            let next = format!("var(--p{})", index + 1);
            declarations.push((name, next));
        }
        let entries: Vec<(&str, &str)> = declarations
            .iter()
            .map(|(name, val)| (name.as_str(), val.as_str()))
            .collect();
        let custom = customs(&entries);
        let value = value("var(--p0)");
        assert!(
            substitute(&value, &custom).is_none(),
            "a chain longer than MAX_VAR_DEPTH must terminate"
        );
    }

    #[test]
    fn resolves_through_all_scope_paths() {
        let root = customs(&[("--brand", "navy"), ("--root-only", "10px")]);
        let parent = customs(&[("--parent-only", "2em")]);
        let element = HashMap::new();
        let scopes = [&element, &parent, &root];

        assert_eq!(
            expanded_scoped("var(--brand)", &scopes, &PropertyRegistry::new())
                .value
                .unwrap()
                .components,
            vec![ident("navy")]
        );
        assert_eq!(
            expanded_scoped("var(--parent-only)", &scopes, &PropertyRegistry::new())
                .value
                .unwrap()
                .components,
            vec![ComponentValue::Dimension(2.0, "em".into())]
        );
        assert_eq!(
            expanded_scoped("var(--root-only)", &scopes, &PropertyRegistry::new())
                .value
                .unwrap()
                .components,
            vec![ComponentValue::Dimension(10.0, "px".into())]
        );
    }

    #[test]
    fn nearest_scope_overrides_ancestor() {
        let root = customs(&[("--brand", "red")]);
        let element = customs(&[("--brand", "blue")]);
        let scopes = [&element, &root];
        let resolved = expanded_scoped("var(--brand)", &scopes, &PropertyRegistry::new())
            .value
            .unwrap();
        assert_eq!(resolved.components, vec![ident("blue")]);

        let merged = inherit_customs([&root, &element]);
        assert_eq!(merged["--brand"].components, vec![ident("blue")]);
    }

    #[test]
    fn registered_initial_value_is_a_typed_fallback() {
        let mut registry = PropertyRegistry::new();
        registry.insert(
            "--brand".to_string(),
            PropertyRegistration {
                syntax: Some("<color>".to_string()),
                inherits: true,
                initial_value: Some(value("#eeeeee")),
            },
        );
        let empty = HashMap::new();
        let result = substitute_scoped(&value("var(--brand)"), &[&empty], &registry);
        assert_eq!(
            result.value.unwrap().components,
            vec![ComponentValue::Hash("eeeeee".into())]
        );
        assert!(result.info.used_typed_fallback);
        assert!(!result.info.unresolved);
    }

    #[test]
    fn typed_fallback_is_used_for_cycles_and_empty_values() {
        let mut registry = PropertyRegistry::new();
        registry.insert(
            "--brand".to_string(),
            PropertyRegistration {
                syntax: Some("<color>".to_string()),
                inherits: true,
                initial_value: Some(value("red")),
            },
        );
        let cyclic = customs(&[("--brand", "var(--brand)")]);
        let result = substitute_scoped(&value("var(--brand)"), &[&cyclic], &registry);
        assert_eq!(result.value.unwrap().components, vec![ident("red")]);
        assert!(result.info.cycle_detected);
        assert!(result.info.used_typed_fallback);

        let empty = customs(&[("--brand", "")]);
        let result = substitute_scoped(&value("var(--brand)"), &[&empty], &registry);
        assert_eq!(result.value.unwrap().components, vec![ident("red")]);
        assert!(result.info.used_typed_fallback);
    }

    #[test]
    fn explicit_fallback_takes_precedence_over_typed_fallback() {
        let mut registry = PropertyRegistry::new();
        registry.insert(
            "--brand".to_string(),
            PropertyRegistration {
                initial_value: Some(value("red")),
                ..PropertyRegistration::default()
            },
        );
        let empty = HashMap::new();
        let result = substitute_scoped(&value("var(--brand, blue)"), &[&empty], &registry);
        assert_eq!(result.value.unwrap().components, vec![ident("blue")]);
        assert!(result.info.used_fallback);
        assert!(!result.info.used_typed_fallback);
    }

    #[test]
    fn non_inheriting_registration_ignores_ancestor_scopes() {
        let mut registry = PropertyRegistry::new();
        registry.insert(
            "--brand".to_string(),
            PropertyRegistration {
                initial_value: Some(value("white")),
                inherits: false,
                ..PropertyRegistration::default()
            },
        );
        let root = customs(&[("--brand", "black")]);
        let element = HashMap::new();
        let scopes = [&element, &root];
        let result = expanded_scoped("var(--brand)", &scopes, &registry);
        assert_eq!(result.value.unwrap().components, vec![ident("white")]);
        assert!(result.info.used_typed_fallback);
    }

    #[test]
    fn substitutes_inside_shorthand_values() {
        let declarations = customs(&[("--m", "10px"), ("--w", "2px"), ("--c", "red")]);

        // A single custom property used as an entire shorthand. The resolved
        // component is kept intact for the longhand expander to interpret.
        let resolved = substitute(&value("margin: var(--m)"), &declarations).unwrap();
        assert_eq!(
            resolved.components,
            vec![
                ident("margin"),
                ComponentValue::Delim(':'),
                ComponentValue::Whitespace,
                ComponentValue::Dimension(10.0, "px".into())
            ]
        );

        // A shorthand whose individual tokens are substituted in place.
        let resolved = substitute(&value("var(--w) solid var(--c)"), &declarations).unwrap();
        assert_eq!(
            resolved.components,
            vec![
                ComponentValue::Dimension(2.0, "px".into()),
                ComponentValue::Whitespace,
                ident("solid"),
                ComponentValue::Whitespace,
                ident("red")
            ]
        );
    }

    #[test]
    fn substitutes_inside_nested_functions() {
        let declarations = customs(&[("--r", "255"), ("--fn", "rgb")]);
        let resolved = substitute(&value("rgb(var(--r), 0, 0)"), &declarations).unwrap();
        let function = resolved
            .components
            .first()
            .and_then(ComponentValue::as_function)
            .expect("rgb function");
        assert_eq!(function.0, "rgb");
        assert_eq!(
            function.1,
            vec![
                ComponentValue::Number(255.0),
                ComponentValue::Comma,
                ComponentValue::Whitespace,
                ComponentValue::Number(0.0),
                ComponentValue::Comma,
                ComponentValue::Whitespace,
                ComponentValue::Number(0.0)
            ]
        );

        // A function name can itself come from a custom property.
        let resolved = substitute(&value("var(--fn)(1, 2, 3)"), &declarations).unwrap();
        assert_eq!(resolved.components.first(), Some(&ident("rgb")));
    }

    #[test]
    fn substitute_with_registry_matches_convenience_api() {
        let mut registry = PropertyRegistry::new();
        registry.insert(
            "--brand".to_string(),
            PropertyRegistration {
                initial_value: Some(value("teal")),
                ..PropertyRegistration::default()
            },
        );
        let declarations = customs(&[("--size", "12px")]);
        let result = substitute_with_registry(&value("var(--size)"), &declarations, &registry);
        assert_eq!(
            result.unwrap().components,
            vec![ComponentValue::Dimension(12.0, "px".into())]
        );
        let result = substitute_with_registry(&value("var(--brand)"), &declarations, &registry);
        assert_eq!(result.unwrap().components, vec![ident("teal")]);
    }
}
