//! Browser globals that third-party scripts expect but which the platform had
//! not installed yet.
//!
//! Two kinds of globals live here:
//!
//! * DOM constructors (`Element`, `HTMLElement`, …) with real `prototype`
//!   objects. The prototypes are chained (`HTMLElement` → `Element` → `Node` →
//!   `EventTarget` → `Object`) so `instanceof` and feature probes such as
//!   `"matches" in Element.prototype` work. New instances reuse the existing
//!   element wrapper convention (`__node`) used by the rest of `web-apis`.
//! * `navigator` and its `userAgentData` companion, which analytics and
//!   feature-detection scripts read before deciding how to bootstrap.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::{NodeData, NodeId};
use browser_js::{JsError, Runtime, Value};

use crate::{HostState, deferred, element};

/// DOM classes in dependency order: every parent is created before its child.
const DOM_CLASSES: &[(&str, Option<&str>)] = &[
    ("EventTarget", None),
    ("Node", Some("EventTarget")),
    ("Element", Some("Node")),
    ("HTMLElement", Some("Element")),
    ("HTMLInputElement", Some("HTMLElement")),
    ("HTMLScriptElement", Some("HTMLElement")),
    ("Document", Some("Node")),
    ("ShadowRoot", Some("Node")),
];

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                         AppleWebKit/537.36 (KHTML, like Gecko) Mag/1.0 \
                         Chrome/120.0.0.0 Safari/537.36";

/// Installs the missing browser globals.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    install_window_aliases(runtime);
    install_dom_constructors(runtime, state);
    install_navigator(runtime);
}

/// Changes an object's `[[Prototype]]` using the runtime's `Reflect` global.
fn set_prototype(runtime: &mut Runtime, object: &Value, prototype: &Value) {
    let reflect = runtime.get_global("Reflect");
    let set = runtime.get_property(&reflect, "setPrototypeOf");
    if matches!(set, Value::Object(_)) {
        let _ = runtime.call_function(
            &set,
            Value::Undefined,
            vec![object.clone(), prototype.clone()],
        );
    }
}

/// Installs one constructible global and returns its constructor/prototype pair.
///
/// Host functions are callable *and* constructible, so `new Element()` gives a
/// fresh instance whose prototype is the function's `prototype` property. A
/// plain `Element()` call returns a detached object instead of clobbering the
/// global `this`.
fn make_constructor(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    name: &str,
    parent: Option<&str>,
) -> (Value, Value) {
    let element_class = matches!(
        name,
        "Element" | "HTMLElement" | "HTMLInputElement" | "HTMLScriptElement"
    );
    let name_owned = name.to_string();
    let state = state.clone();
    runtime.define_global_function(
        name,
        Box::new(move |runtime, this, _args| {
            let global = runtime.global();
            let is_instance = matches!(this, Value::Object(id) if Value::Object(id) != global);
            if !is_instance {
                if element_class {
                    let tag = if name_owned == "HTMLInputElement" {
                        "input"
                    } else if name_owned == "HTMLScriptElement" {
                        "script"
                    } else {
                        "div"
                    };
                    let document = state.borrow().document.clone();
                    let node = document.borrow_mut().create_element(tag);
                    return Ok(element::wrap(runtime, &state, node));
                }
                return Ok(runtime.new_object());
            }
            Ok(this.clone())
        }),
    );
    let constructor = runtime.get_global(name);
    let prototype = runtime.new_object();
    if let Some(parent) = parent {
        let parent_constructor = runtime.get_global(parent);
        let parent_prototype = runtime.get_property(&parent_constructor, "prototype");
        if matches!(parent_prototype, Value::Object(_)) {
            set_prototype(runtime, &prototype, &parent_prototype);
        }
    }
    runtime.set_property(constructor.clone(), "prototype", prototype.clone());
    runtime.set_property(prototype.clone(), "constructor", constructor.clone());
    (constructor, prototype)
}

fn install_dom_constructors(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let mut prototypes = std::collections::HashMap::new();
    for &(name, parent) in DOM_CLASSES {
        let (_constructor, prototype) = make_constructor(runtime, state, name, parent);
        prototypes.insert(name, prototype);
    }

    if let Some(prototype) = prototypes.get("Element") {
        // Reuse the wrapper's own members so methods such as `matches` and
        // `getAttribute` behave identically on a prototype and on a wrapper.
        // `define_element_members` also installs the accessors, so this single
        // call supplies the full element surface (`matches`, `getAttribute`,
        // `appendChild`, `children`, `parentElement`, `ownerDocument`, …).
        element::define_element_members(runtime, state, prototype.clone());
        install_style_and_class_list(runtime, prototype);
        install_dataset(runtime, state, prototype);
    }

    // The live `document` object is a Document instance, so probes like
    // `document instanceof Document` succeed.
    let global = runtime.global();
    let document = runtime.get_property(&global, "document");
    if let (Some(prototype), Value::Object(_)) = (prototypes.get("Document"), &document) {
        set_prototype(runtime, &document, prototype);
    }
}

fn install_style_and_class_list(runtime: &mut Runtime, prototype: &Value) {
    // These are plain data properties: a real wrapper receives its own live
    // `style` / `classList` objects from `cssom`, and an inherited *accessor*
    // here would intercept `set_property` when the wrapper is built. A data
    // property on the prototype is shadowed cleanly by the wrapper's own one.
    let style = runtime.new_object();
    runtime.set_property(prototype.clone(), "style", style);
    let class_list = runtime.new_object();
    runtime.set_property(prototype.clone(), "classList", class_list);
}

fn install_dataset(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, prototype: &Value) {
    runtime.define_accessor(
        prototype.clone(),
        "dataset",
        {
            let state = state.clone();
            Box::new(
                move |runtime, this, _args| match element::node_id_of(runtime, &this) {
                    Some(node) => Ok(make_dataset(runtime, &state, node)),
                    None => Ok(runtime.new_object()),
                },
            )
        },
        Box::new(|_runtime, _this, _args| Err(JsError::type_error("dataset is read-only"))),
    );
}

/// Maps a `data-foo-bar` attribute name to the `fooBar` dataset key.
fn dataset_key(attribute: &str) -> String {
    let mut key = String::new();
    let mut capitalize = false;
    for character in attribute.chars() {
        if character == '-' {
            capitalize = true;
            continue;
        }
        if capitalize {
            key.extend(character.to_uppercase());
            capitalize = false;
        } else {
            key.push(character);
        }
    }
    key
}

fn make_dataset(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, node: NodeId) -> Value {
    let object = runtime.new_object();
    let entries: Vec<(String, String)> = {
        let document = state.borrow().document.clone();
        let document = document.borrow();
        document
            .attributes(node)
            .map(|attributes| {
                attributes
                    .iter()
                    .filter(|(name, _)| name.to_ascii_lowercase().starts_with("data-"))
                    .map(|(name, _value)| (dataset_key(&name[5..]), name.to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    };
    for (key, attribute) in entries {
        runtime.define_accessor(
            object.clone(),
            &key,
            {
                let state = state.clone();
                let attribute = attribute.clone();
                Box::new(move |_runtime, _this, _args| {
                    let document = state.borrow().document.clone();
                    let value = document
                        .borrow()
                        .attr(node, &attribute)
                        .unwrap_or_default()
                        .to_owned();
                    Ok(Value::String(value))
                })
            },
            {
                let state = state.clone();
                Box::new(move |_runtime, _this, args| {
                    let value = args.first().map(Value::to_string_value).unwrap_or_default();
                    let document = state.borrow().document.clone();
                    document.borrow_mut().set_attr(node, &attribute, &value);
                    Ok(Value::Undefined)
                })
            },
        );
    }
    object
}

/// Points a freshly wrapped DOM node at the most specific DOM prototype so
/// `instanceof` works for real elements too, not only for `new Element()`.
pub(crate) fn apply_element_prototype(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    object: &Value,
    node: NodeId,
) {
    let class = {
        let document = state.borrow().document.clone();
        let document = document.borrow();
        match document.node(node).data {
            NodeData::Element(_) => {
                match document
                    .element_name(node)
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                {
                    Some("input") => "HTMLInputElement",
                    Some("script") => "HTMLScriptElement",
                    _ => "HTMLElement",
                }
            }
            _ => "Node",
        }
    };
    let constructor = runtime.get_global(class);
    let prototype = runtime.get_property(&constructor, "prototype");
    if matches!(prototype, Value::Object(_)) {
        set_prototype(runtime, object, &prototype);
    }
}

fn install_window_aliases(runtime: &mut Runtime) {
    let global = runtime.global();
    runtime.set_global("window", global.clone());
    runtime.set_global("self", global.clone());
    runtime.set_global("globalThis", global);
}

fn make_brand(runtime: &mut Runtime, brand: &str, version: &str) -> Value {
    let object = runtime.new_object();
    runtime.set_property(object.clone(), "brand", Value::String(brand.to_owned()));
    runtime.set_property(object.clone(), "version", Value::String(version.to_owned()));
    object
}

fn install_navigator(runtime: &mut Runtime) {
    let navigator = match runtime.get_global("navigator") {
        Value::Object(_) => runtime.get_global("navigator"),
        _ => {
            let object = runtime.new_object();
            runtime.set_global("navigator", object.clone());
            object
        }
    };

    runtime.set_property(
        navigator.clone(),
        "userAgent",
        Value::String(USER_AGENT.to_owned()),
    );
    runtime.set_property(
        navigator.clone(),
        "language",
        Value::String("en-US".to_owned()),
    );
    let languages = runtime.new_array(vec![
        Value::String("en-US".to_owned()),
        Value::String("en".to_owned()),
    ]);
    runtime.set_property(navigator.clone(), "languages", languages);
    runtime.set_property(
        navigator.clone(),
        "platform",
        Value::String("MacIntel".to_owned()),
    );
    runtime.set_property(navigator.clone(), "onLine", Value::Bool(true));
    runtime.set_property(
        navigator.clone(),
        "vendor",
        Value::String("Google Inc.".to_owned()),
    );
    runtime.set_property(navigator.clone(), "hardwareConcurrency", Value::Number(8.0));
    runtime.set_property(navigator.clone(), "deviceMemory", Value::Number(8.0));
    runtime.set_property(navigator.clone(), "maxTouchPoints", Value::Number(0.0));

    let chromium = make_brand(runtime, "Chromium", "120");
    let chrome = make_brand(runtime, "Google Chrome", "120");
    let other = make_brand(runtime, "Not?A_Brand", "24");
    let brands = runtime.new_array(vec![chromium, chrome, other]);
    let user_agent_data = runtime.new_object();
    runtime.set_property(user_agent_data.clone(), "brands", brands.clone());
    runtime.set_property(user_agent_data.clone(), "mobile", Value::Bool(false));
    runtime.set_property(
        user_agent_data.clone(),
        "platform",
        Value::String("macOS".to_owned()),
    );
    runtime.define_host_method(
        user_agent_data.clone(),
        "getHighEntropyValues",
        Box::new(move |runtime, _this, _args| {
            let hints = runtime.new_object();
            runtime.set_property(
                hints.clone(),
                "architecture",
                Value::String("x86".to_owned()),
            );
            runtime.set_property(hints.clone(), "bitness", Value::String("64".to_owned()));
            runtime.set_property(hints.clone(), "brands", brands.clone());
            runtime.set_property(hints.clone(), "mobile", Value::Bool(false));
            runtime.set_property(hints.clone(), "model", Value::String(String::new()));
            runtime.set_property(hints.clone(), "platform", Value::String("macOS".to_owned()));
            runtime.set_property(
                hints.clone(),
                "platformVersion",
                Value::String("13.0.0".to_owned()),
            );
            runtime.set_property(
                hints.clone(),
                "uaFullVersion",
                Value::String("120.0.0.0".to_owned()),
            );
            Ok(deferred::resolved(runtime, hints))
        }),
    );
    runtime.set_property(navigator.clone(), "userAgentData", user_agent_data);

    // `window` is the global object, so this also installs `window.navigator`.
    let global = runtime.global();
    runtime.set_property(global, "navigator", navigator.clone());
    runtime.set_global("navigator", navigator);
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse_document;

    fn setup() -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document(
            "<html><body><input id='q' data-foo-bar='baz'></body></html>",
        )));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    fn eval(runtime: &mut Runtime, source: &str) -> Value {
        runtime.eval(source).unwrap()
    }

    #[test]
    fn dom_constructors_are_real_constructors() {
        let (mut runtime, _state) = setup();
        for expression in [
            "typeof Element === 'function'",
            "typeof HTMLElement === 'function'",
            "typeof Node === 'function'",
            "typeof Document === 'function'",
            "typeof EventTarget === 'function'",
            "typeof ShadowRoot === 'function'",
            "typeof HTMLInputElement === 'function'",
            "typeof HTMLScriptElement === 'function'",
            "Element.prototype.constructor === Element",
            "HTMLElement.prototype instanceof Element",
            "HTMLInputElement.prototype instanceof HTMLElement",
            "document instanceof Document",
            "document.createElement('div') instanceof HTMLElement",
            "document.getElementById('q') instanceof HTMLInputElement",
            "new Element() instanceof Element",
            "'matches' in Element.prototype",
            "'getAttribute' in Element.prototype",
            "'setAttribute' in Element.prototype",
            "'appendChild' in Element.prototype",
            "'insertBefore' in Element.prototype",
            "'remove' in Element.prototype",
            "'classList' in Element.prototype",
            "'dataset' in Element.prototype",
            "'style' in Element.prototype",
            "'children' in Element.prototype",
            "'parentElement' in Element.prototype",
            "'ownerDocument' in Element.prototype",
        ] {
            assert_eq!(
                eval(&mut runtime, expression),
                Value::Bool(true),
                "{expression}"
            );
        }
        // Calling a constructor with `new` (the classic misuse case) must not
        // throw, even when no node is attached to the instance.
        eval(&mut runtime, "var detached = new Element();");
        assert_eq!(
            eval(&mut runtime, "detached instanceof Element"),
            Value::Bool(true)
        );
    }

    #[test]
    fn navigator_exposes_the_expected_fields() {
        let (mut runtime, _state) = setup();
        for expression in [
            "typeof navigator.userAgent === 'string'",
            "navigator.userAgent.indexOf('Chrome') > -1",
            "navigator.language === 'en-US'",
            "navigator.languages.length === 2",
            "navigator.platform === 'MacIntel'",
            "navigator.onLine === true",
            "navigator.vendor === 'Google Inc.'",
            "navigator.hardwareConcurrency === 8",
            "navigator.deviceMemory === 8",
            "navigator.maxTouchPoints === 0",
            "Array.isArray(navigator.userAgentData.brands)",
            "navigator.userAgentData.brands.length === 3",
            "navigator === window.navigator",
            "'navigator' in window",
            "'location' in window",
            "!('x' in window)",
            "!('x' in globalThis)",
        ] {
            assert_eq!(
                eval(&mut runtime, expression),
                Value::Bool(true),
                "{expression}"
            );
        }
        // The high-entropy API is asynchronous.
        eval(
            &mut runtime,
            "var entropy = navigator.userAgentData.getHighEntropyValues(['brands']);",
        );
        assert_eq!(
            eval(&mut runtime, "typeof entropy.then === 'function'"),
            Value::Bool(true)
        );
        eval(
            &mut runtime,
            "var seenEntropy = null; entropy.then(function (v) { seenEntropy = v; });",
        );
        runtime.run_microtasks().unwrap();
        assert_eq!(
            eval(&mut runtime, "seenEntropy.platform"),
            Value::String("macOS".to_owned())
        );
    }

    /// Third-party bootstraps probe feature support through `window.*` and
    /// `navigator.userAgentData`, then call the resulting methods. Make sure
    /// those shapes support `in` and direct method calls.
    #[test]
    fn window_and_user_agent_data_support_feature_probes() {
        let (mut runtime, _state) = setup();
        for expression in [
            "window === globalThis",
            "window.document === document",
            "typeof window.Element === 'function'",
            "window.Element === Element",
            "window.Element.prototype === Element.prototype",
            "'getAttribute' in window.Element.prototype",
            "window.Element.prototype.getAttribute === Element.prototype.getAttribute",
            "typeof window.navigator.userAgentData === 'object'",
            "'getHighEntropyValues' in navigator.userAgentData",
            "typeof navigator.userAgentData.getHighEntropyValues === 'function'",
            "'brands' in navigator.userAgentData",
            "navigator.userAgentData.brands[0].brand === 'Chromium'",
        ] {
            assert_eq!(
                eval(&mut runtime, expression),
                Value::Bool(true),
                "{expression}"
            );
        }
    }

    #[test]
    fn element_prototype_helpers_operate_on_wrappers() {
        let (mut runtime, _state) = setup();
        assert_eq!(
            eval(&mut runtime, "document.getElementById('q').dataset.fooBar"),
            Value::String("baz".to_owned())
        );
        eval(&mut runtime, "document.getElementById('q').remove()");
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('q').parentNode === null"
            ),
            Value::Bool(true)
        );
    }
}
