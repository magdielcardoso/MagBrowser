//! Minimal CSS Object Model (CSSOM) bindings used by real-world scripts.
//!
//! * `element.style` is backed by the element's `style` attribute, so mutations
//!   made from JavaScript are immediately visible to the style engine.
//! * `element.classList` is backed by the element's `class` attribute.
//! * `window.getComputedStyle(element)` resolves values through an optional
//!   resolver installed on [`HostState`] by the embedder. When no computed
//!   style tree is available it returns empty strings instead of throwing.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::{Document, NodeId};
use browser_js::{JsError, Runtime, Value};

use crate::HostState;
use crate::element::require_node;

/// The CSS properties the computed-style object is expected to expose.
///
/// The resolver on [`HostState`] may answer any property name; these are the
/// ones scripts most commonly read, and the ones the built-in tests cover.
pub const COMPUTED_PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "display",
    "width",
    "height",
    "font-size",
    "opacity",
];

/// Converts a script-facing property name (`backgroundColor`, `FONT-SIZE`)
/// into the lowercase dashed form CSS uses internally.
pub fn normalize_property(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

/// Splits a style attribute into `(property, value)` pairs. Later declarations
/// win when a property appears more than once.
fn declarations(css: &str) -> Vec<(String, String)> {
    css.split(';')
        .filter_map(|part| {
            let (name, value) = part.split_once(':')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((normalize_property(name), value.trim().to_string()))
        })
        .collect()
}

fn inline_value(document: &Document, node: NodeId, name: &str) -> Option<String> {
    let css = document.attr(node, "style")?.to_string();
    declarations(&css)
        .into_iter()
        .rev()
        .find(|(property, _)| property == name)
        .map(|(_, value)| value)
}

fn set_inline_value(document: &Rc<RefCell<Document>>, node: NodeId, name: &str, value: &str) {
    let mut props = {
        let document = document.borrow();
        document
            .attr(node, "style")
            .map(declarations)
            .unwrap_or_default()
    };
    props.retain(|(property, _)| property != name);
    let value = value.trim();
    if !value.is_empty() {
        props.push((name.to_string(), value.to_string()));
    }
    let css = props
        .into_iter()
        .map(|(property, value)| format!("{property}: {value}"))
        .collect::<Vec<_>>()
        .join("; ");
    document.borrow_mut().set_attr(node, "style", &css);
}

fn class_tokens(document: &Document, node: NodeId) -> Vec<String> {
    document
        .attr(node, "class")
        .map(|value| value.split_ascii_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

fn set_class_tokens(document: &Rc<RefCell<Document>>, node: NodeId, tokens: &[String]) {
    document
        .borrow_mut()
        .set_attr(node, "class", &tokens.join(" "));
}

/// Builds the object returned by `element.style`.
fn make_style_declaration(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
) -> Value {
    let object = runtime.new_object();

    runtime.define_host_method(object.clone(), "getPropertyValue", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("getPropertyValue needs a name"))?;
            let name = normalize_property(&name);
            let document = state.borrow().document.clone();
            Ok(Value::String(
                inline_value(&document.borrow(), node, &name).unwrap_or_default(),
            ))
        })
    });

    runtime.define_host_method(object.clone(), "setProperty", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            if args.len() < 2 {
                return Err(JsError::type_error("setProperty needs a name and a value"));
            }
            let name = normalize_property(&args[0].to_string_value());
            let value = args[1].to_string_value();
            let document = state.borrow().document.clone();
            set_inline_value(&document, node, &name, &value);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "removeProperty", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("removeProperty needs a name"))?;
            let name = normalize_property(&name);
            let document = state.borrow().document.clone();
            let removed = inline_value(&document.borrow(), node, &name).unwrap_or_default();
            if !removed.is_empty() {
                set_inline_value(&document, node, &name, "");
            }
            Ok(Value::String(removed))
        })
    });

    runtime.define_accessor(
        object.clone(),
        "cssText",
        {
            let state = state.clone();
            Box::new(move |_runtime, _this, _args| {
                let document = state.borrow().document.clone();
                let text = document
                    .borrow()
                    .attr(node, "style")
                    .unwrap_or_default()
                    .to_string();
                Ok(Value::String(text))
            })
        },
        {
            let state = state.clone();
            Box::new(move |_runtime, _this, args| {
                let text = args.first().map(Value::to_string_value).unwrap_or_default();
                let document = state.borrow().document.clone();
                document.borrow_mut().set_attr(node, "style", &text);
                Ok(Value::Undefined)
            })
        },
    );

    object
}

/// Builds the object returned by `element.classList`.
fn make_token_list(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, node: NodeId) -> Value {
    let object = runtime.new_object();

    runtime.define_host_method(object.clone(), "contains", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let token = args.first().map(Value::to_string_value).unwrap_or_default();
            let document = state.borrow().document.clone();
            let contains = class_tokens(&document.borrow(), node)
                .iter()
                .any(|existing| existing == &token);
            Ok(Value::Bool(contains))
        })
    });

    runtime.define_host_method(object.clone(), "add", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let document = state.borrow().document.clone();
            let mut tokens = class_tokens(&document.borrow(), node);
            for arg in args {
                let token = arg.to_string_value();
                if token.is_empty() || tokens.iter().any(|existing| existing == &token) {
                    continue;
                }
                tokens.push(token);
            }
            set_class_tokens(&document, node, &tokens);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "remove", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let document = state.borrow().document.clone();
            let mut tokens = class_tokens(&document.borrow(), node);
            for arg in args {
                let token = arg.to_string_value();
                tokens.retain(|existing| existing != &token);
            }
            set_class_tokens(&document, node, &tokens);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "toggle", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let token = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("toggle needs a token"))?;
            let force = args.get(1).map(Value::is_truthy);
            let document = state.borrow().document.clone();
            let mut tokens = class_tokens(&document.borrow(), node);
            let present = tokens.iter().any(|existing| existing == &token);
            let add = force.unwrap_or(!present);
            if add {
                if !present {
                    tokens.push(token);
                }
            } else {
                tokens.retain(|existing| existing != &token);
            }
            set_class_tokens(&document, node, &tokens);
            Ok(Value::Bool(add))
        })
    });

    object
}

/// Attaches `style` and `classList` objects to a freshly wrapped element.
///
/// Called once per element wrapper, so repeated reads of `element.style` and
/// `element.classList` return the same objects, as in a browser.
pub fn install_element_cssom(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    object: Value,
    node: NodeId,
) {
    let style = make_style_declaration(runtime, state, node);
    runtime.set_property(object.clone(), "style", style);
    let class_list = make_token_list(runtime, state, node);
    runtime.set_property(object, "classList", class_list);
}

/// Builds a `CSSStyleDeclaration`-like object for `window.getComputedStyle`.
fn make_computed_style(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
) -> Value {
    let object = runtime.new_object();
    runtime.define_host_method(object.clone(), "getPropertyValue", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("getPropertyValue needs a name"))?;
            let name = normalize_property(&name);
            let state = state.borrow();
            let value = state
                .computed_style
                .as_ref()
                .and_then(|resolve| resolve(node, &name))
                .unwrap_or_default();
            Ok(Value::String(value))
        })
    });
    object
}

/// Installs `window.getComputedStyle` on the global object.
pub fn install_global(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let global = runtime.global();
    runtime.define_host_method(global, "getComputedStyle", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let value = args
                .first()
                .ok_or_else(|| JsError::type_error("getComputedStyle needs an element"))?;
            let node = require_node(runtime, value)?;
            Ok(make_computed_style(runtime, &state, node))
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;

    fn runtime_with(html: &str) -> (Runtime, Rc<RefCell<Document>>, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(browser_html_parser::parse_document(html)));
        let host = crate::install(&mut runtime, document.clone(), None);
        (runtime, document, host)
    }

    fn eval(runtime: &mut Runtime, source: &str) -> String {
        runtime.eval(source).unwrap().to_string_value()
    }

    #[test]
    fn style_property_roundtrip_uses_the_style_attribute() {
        let (mut runtime, document, _host) = runtime_with(
            "<html><body><div id='x' style='color: red; margin: 4px'></div></body></html>",
        );
        let node = document.borrow().get_element_by_id("x").unwrap();

        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('x').style.getPropertyValue('color')"
            ),
            "red"
        );
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('x').style.getPropertyValue('margin')"
            ),
            "4px"
        );

        runtime
            .eval("document.getElementById('x').style.setProperty('color', 'blue')")
            .unwrap();

        let css = document.borrow().attr(node, "style").unwrap().to_string();
        assert!(css.contains("color: blue"), "{css}");
        assert!(css.contains("margin: 4px"), "{css}");
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('x').style.getPropertyValue('color')"
            ),
            "blue"
        );
    }

    #[test]
    fn style_set_property_normalizes_and_removes_properties() {
        let (mut runtime, document, _host) =
            runtime_with("<html><body><div id='x'></div></body></html>");
        let node = document.borrow().get_element_by_id("x").unwrap();

        runtime
            .eval("document.getElementById('x').style.setProperty('backgroundColor', '#123456')")
            .unwrap();
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('x').style.getPropertyValue('background-color')"
            ),
            "#123456"
        );

        runtime
            .eval("document.getElementById('x').style.removeProperty('background-color')")
            .unwrap();
        assert_eq!(
            document.borrow().attr(node, "style").unwrap_or_default(),
            ""
        );
    }

    #[test]
    fn style_object_identity_is_stable() {
        let (mut runtime, _document, _host) =
            runtime_with("<html><body><div id='x'></div></body></html>");
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('x').style === document.getElementById('x').style"
            ),
            "true"
        );
    }

    #[test]
    fn classlist_add_remove_toggle_contains() {
        let (mut runtime, document, _host) =
            runtime_with("<html><body><div id='x' class='foo bar'></div></body></html>");
        let node = document.borrow().get_element_by_id("x").unwrap();
        let class = |document: &Rc<RefCell<Document>>| {
            document
                .borrow()
                .attr(node, "class")
                .unwrap_or_default()
                .to_string()
        };

        runtime
            .eval("var el = document.getElementById('x'); el.classList.add('baz', 'foo')")
            .unwrap();
        assert_eq!(class(&document), "foo bar baz");

        assert_eq!(eval(&mut runtime, "el.classList.contains('bar')"), "true");
        assert_eq!(eval(&mut runtime, "el.classList.contains('nope')"), "false");

        assert_eq!(eval(&mut runtime, "el.classList.toggle('bar')"), "false");
        assert_eq!(class(&document), "foo baz");
        assert_eq!(eval(&mut runtime, "el.classList.toggle('bar')"), "true");
        assert_eq!(class(&document), "foo baz bar");

        assert_eq!(
            eval(&mut runtime, "el.classList.toggle('foo', false)"),
            "false"
        );
        assert_eq!(class(&document), "baz bar");

        runtime.eval("el.classList.remove('baz')").unwrap();
        assert_eq!(class(&document), "bar");
    }

    #[test]
    fn computed_style_uses_the_registered_resolver() {
        let (mut runtime, document, host) =
            runtime_with("<html><body><div id='x'></div></body></html>");
        let node = document.borrow().get_element_by_id("x").unwrap();

        // Without a computed style tree every property is an empty string.
        assert_eq!(
            eval(
                &mut runtime,
                "window.getComputedStyle(document.getElementById('x')).getPropertyValue('color')"
            ),
            ""
        );

        host.borrow_mut().computed_style = Some(Box::new(move |resolved, property| {
            if resolved != node {
                return None;
            }
            match property {
                "color" => Some("rgb(255, 0, 0)".to_string()),
                "background-color" => Some("rgba(0, 0, 0, 0)".to_string()),
                "display" => Some("block".to_string()),
                "width" => Some("320px".to_string()),
                "height" => Some("48px".to_string()),
                "font-size" => Some("16px".to_string()),
                "opacity" => Some("1".to_string()),
                _ => None,
            }
        }));

        let get = |property: &str| {
            format!(
                "window.getComputedStyle(document.getElementById('x')).getPropertyValue('{property}')"
            )
        };
        assert_eq!(eval(&mut runtime, &get("color")), "rgb(255, 0, 0)");
        assert_eq!(
            eval(&mut runtime, &get("backgroundColor")),
            "rgba(0, 0, 0, 0)"
        );
        assert_eq!(eval(&mut runtime, &get("display")), "block");
        assert_eq!(eval(&mut runtime, &get("width")), "320px");
        assert_eq!(eval(&mut runtime, &get("height")), "48px");
        assert_eq!(eval(&mut runtime, &get("fontSize")), "16px");
        assert_eq!(eval(&mut runtime, &get("opacity")), "1");
        assert_eq!(eval(&mut runtime, &get("unknown")), "");
    }
}
