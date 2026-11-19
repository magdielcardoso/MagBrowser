//! The `document` global object.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::NodeId;
use browser_js::{JsError, Runtime, Value};

use crate::HostState;
use crate::element::{require_node, wrap};

/// Installs `document` and its DOM methods.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let document = runtime.new_object();
    runtime.set_global("document", document.clone());

    runtime.define_host_method(document.clone(), "getElementById", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let id = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("getElementById needs an id"))?;
            let document = state.borrow().document.clone();
            let found = document.borrow().get_element_by_id(&id);
            match found {
                Some(node) => Ok(wrap(runtime, &state, node)),
                None => Ok(Value::Null),
            }
        })
    });

    runtime.define_host_method(document.clone(), "querySelector", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let selector = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("querySelector needs a selector"))?;
            let document = state.borrow().document.clone();
            let result = {
                let document = document.borrow();
                browser_selectors::query_selector(&document, document.root(), &selector)
            };
            match result {
                Ok(Some(node)) => Ok(wrap(runtime, &state, node)),
                Ok(None) => Ok(Value::Null),
                Err(err) => Err(JsError::new(err.to_string())),
            }
        })
    });

    runtime.define_host_method(document.clone(), "querySelectorAll", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let selector = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("querySelectorAll needs a selector"))?;
            let document = state.borrow().document.clone();
            let nodes = {
                let document = document.borrow();
                browser_selectors::query_selector_all(&document, document.root(), &selector)
                    .map_err(|err| JsError::new(err.to_string()))?
            };
            let items = nodes
                .into_iter()
                .map(|node| wrap(runtime, &state, node))
                .collect();
            Ok(runtime.new_array(items))
        })
    });

    runtime.define_host_method(document.clone(), "createElement", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let tag = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("createElement needs a tag name"))?;
            let document = state.borrow().document.clone();
            let node = document.borrow_mut().create_element(&tag);
            Ok(wrap(runtime, &state, node))
        })
    });

    runtime.define_host_method(document.clone(), "createTextNode", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let text = args.first().map(Value::to_string_value).unwrap_or_default();
            let document = state.borrow().document.clone();
            let node = document.borrow_mut().create_text(&text);
            Ok(wrap(runtime, &state, node))
        })
    });

    runtime.define_host_method(document.clone(), "addEventListener", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            crate::events::add_listener(runtime, &state, NodeId(0), args, false);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(document.clone(), "removeEventListener", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            crate::events::remove_listener(&state, NodeId(0), args);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(document.clone(), "dispatchEvent", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let handled = match args.first() {
                Some(Value::Object(_)) => {
                    crate::events::dispatch_event_object(
                        runtime,
                        &state,
                        NodeId(0),
                        args[0].clone(),
                    )
                    .handled
                }
                _ => {
                    let event_type = args
                        .first()
                        .map(Value::to_string_value)
                        .unwrap_or_else(|| "custom".to_string());
                    crate::events::dispatch_event(runtime, &state, NodeId(0), &event_type)
                }
            };
            Ok(Value::Bool(handled))
        })
    });

    runtime.define_host_method(document.clone(), "getElementsByTagName", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let tag = args
                .first()
                .map(Value::to_string_value)
                .unwrap_or_else(|| "*".to_string());
            let document = state.borrow().document.clone();
            let nodes = document.borrow().get_elements_by_tag_name(&tag);
            let items = nodes
                .into_iter()
                .map(|node| wrap(runtime, &state, node))
                .collect();
            Ok(runtime.new_array(items))
        })
    });

    install_accessors(runtime, state, document);
}

fn install_accessors(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, object: Value) {
    #[allow(clippy::type_complexity)]
    let element_accessor =
        |name: &str, pick: fn((Option<NodeId>, Option<NodeId>)) -> Option<NodeId>| {
            let state = state.clone();
            let getter: browser_js::HostFunction = Box::new(move |runtime, _this, _args| {
                let document = state.borrow().document.clone();
                let node = {
                    let document = document.borrow();
                    pick(document.head_and_body())
                };
                match node {
                    Some(node) => Ok(wrap(runtime, &state, node)),
                    None => Ok(Value::Null),
                }
            });
            let setter: browser_js::HostFunction =
                Box::new(|_runtime, _this, _args| Err(JsError::type_error("read-only property")));
            (name.to_string(), getter, setter)
        };

    let (name, getter, setter) = element_accessor("body", |(_, body)| body);
    runtime.define_accessor(object.clone(), &name, getter, setter);
    let (name, getter, setter) = element_accessor("head", |(head, _)| head);
    runtime.define_accessor(object.clone(), &name, getter, setter);
    runtime.define_accessor(
        object.clone(),
        "documentElement",
        {
            let state = state.clone();
            Box::new(move |runtime, _this, _args| {
                let document = state.borrow().document.clone();
                let node = document.borrow().document_element();
                match node {
                    Some(node) => Ok(wrap(runtime, &state, node)),
                    None => Ok(Value::Null),
                }
            })
        },
        { Box::new(|_runtime, _this, _args| Err(JsError::type_error("read-only property"))) },
    );

    runtime.define_accessor(
        object.clone(),
        "activeElement",
        {
            let state = state.clone();
            Box::new(move |runtime, _this, _args| match state.borrow().focused {
                Some(node) => Ok(wrap(runtime, &state, node)),
                None => Ok(Value::Null),
            })
        },
        { Box::new(|_runtime, _this, _args| Err(JsError::type_error("read-only property"))) },
    );

    runtime.define_accessor(
        object.clone(),
        "title",
        {
            let state = state.clone();
            Box::new(move |_runtime, _this, _args| {
                let document = state.borrow().document.clone();
                let title = document.borrow().title.clone();
                Ok(Value::String(title))
            })
        },
        {
            let state = state.clone();
            Box::new(move |_runtime, _this, args| {
                let title = args.first().map(Value::to_string_value).unwrap_or_default();
                let document = state.borrow().document.clone();
                // If the document has a <title> element, updating document.title
                // is a characterData change on its text node. Record it so
                // MutationObservers observing the head see the update.
                let title_text = {
                    let document = document.borrow();
                    document
                        .get_elements_by_tag_name("title")
                        .into_iter()
                        .next()
                        .and_then(|node| document.first_child(node))
                };
                if let Some(text_node) = title_text {
                    let old_value = document.borrow().text_content(text_node);
                    document.borrow_mut().title = title;
                    crate::observers::record_mutation(
                        &state,
                        crate::observers::Mutation::character_data(text_node, Some(old_value)),
                    );
                } else {
                    document.borrow_mut().title = title;
                }
                Ok(Value::Undefined)
            })
        },
    );
}

/// Returns the body element id, when present.
pub fn body_node(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> Option<NodeId> {
    let _ = runtime;
    let document = state.borrow().document.clone();
    let document = document.borrow();
    document.head_and_body().1
}

/// Verifies a value refers to an element in the document.
pub fn is_element(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, value: &Value) -> bool {
    let Ok(node) = require_node(runtime, value) else {
        return false;
    };
    let document = state.borrow().document.clone();
    document.borrow().node(node).is_element()
}
