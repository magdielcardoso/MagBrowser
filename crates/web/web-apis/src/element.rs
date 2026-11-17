//! Element wrapper objects and their DOM methods.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::{Document, Namespace, NodeData, NodeId, serialize_children};
use browser_js::{JsError, Runtime, Value};

use crate::HostState;

/// Reads the hidden node id of a wrapper object.
pub fn node_id_of(runtime: &mut Runtime, value: &Value) -> Option<NodeId> {
    let raw = runtime.get_property(value, "__node");
    match raw {
        Value::Number(number) if number >= 0.0 => Some(NodeId(number as usize)),
        _ => None,
    }
}

/// Borrow helper that returns an error when the value is not an element wrapper.
pub fn require_node(runtime: &mut Runtime, value: &Value) -> Result<NodeId, JsError> {
    node_id_of(runtime, value).ok_or_else(|| JsError::type_error("expected a DOM element"))
}

/// Wraps a DOM node into a JavaScript object, reusing cached wrappers.
pub fn wrap(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, node: NodeId) -> Value {
    if let Some(existing) = state.borrow().elements.get(&node.0).cloned() {
        return existing;
    }
    let object = runtime.new_object();
    let node_type = node_type(state, node);
    runtime.set_property(object.clone(), "__node", Value::Number(node.0 as f64));
    runtime.set_property(object.clone(), "nodeType", Value::Number(node_type as f64));
    define_element_members(runtime, state, object.clone());
    state.borrow_mut().elements.insert(node.0, object.clone());
    object
}

fn node_type(state: &Rc<RefCell<HostState>>, node: NodeId) -> u8 {
    let document = state.borrow().document.clone();
    let document = document.borrow();
    match document.node(node).data {
        NodeData::Element(_) => 1,
        NodeData::Text(_) => 3,
        NodeData::Comment(_) => 8,
        NodeData::Document => 9,
        NodeData::Doctype { .. } => 10,
    }
}

fn document_of(state: &Rc<RefCell<HostState>>) -> Rc<RefCell<Document>> {
    state.borrow().document.clone()
}

fn define_element_members(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, object: Value) {
    runtime.define_host_method(object.clone(), "getAttribute", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("getAttribute needs a name"))?;
            let document = document_of(&state);
            let document = document.borrow();
            match document.attr(node, &name) {
                Some(value) => Ok(Value::String(value.to_string())),
                None => Ok(Value::Null),
            }
        })
    });

    runtime.define_host_method(object.clone(), "setAttribute", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            if args.len() < 2 {
                return Err(JsError::type_error("setAttribute needs a name and a value"));
            }
            let name = args[0].to_string_value();
            let value = args[1].to_string_value();
            let document = document_of(&state);
            document.borrow_mut().set_attr(node, &name, &value);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "removeAttribute", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("removeAttribute needs a name"))?;
            let document = document_of(&state);
            document.borrow_mut().remove_attr(node, &name);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "hasAttribute", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            let name = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("hasAttribute needs a name"))?;
            let document = document_of(&state);
            let has = document.borrow().has_attr(node, &name);
            Ok(Value::Bool(has))
        })
    });

    runtime.define_host_method(object.clone(), "appendChild", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let parent = require_node(runtime, &this)?;
            let child = args
                .first()
                .ok_or_else(|| JsError::type_error("appendChild needs a node"))
                .and_then(|v| require_node(runtime, v))?;
            let document = document_of(&state);
            if !document.borrow_mut().append_child(parent, child) {
                return Err(JsError::new("appendChild failed (cycle or unknown node)"));
            }
            Ok(args[0].clone())
        })
    });

    runtime.define_host_method(object.clone(), "removeChild", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let parent = require_node(runtime, &this)?;
            let child = args
                .first()
                .ok_or_else(|| JsError::type_error("removeChild needs a node"))
                .and_then(|v| require_node(runtime, v))?;
            let document = document_of(&state);
            if !document.borrow_mut().remove_child(parent, child) {
                return Err(JsError::new("removeChild failed"));
            }
            Ok(Value::Null)
        })
    });

    runtime.define_host_method(object.clone(), "insertBefore", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let parent = require_node(runtime, &this)?;
            if args.len() < 2 {
                return Err(JsError::type_error("insertBefore needs two nodes"));
            }
            let new_node = require_node(runtime, &args[0])?;
            let reference = require_node(runtime, &args[1])?;
            let document = document_of(&state);
            if !document
                .borrow_mut()
                .insert_before(parent, new_node, reference)
            {
                return Err(JsError::new("insertBefore failed"));
            }
            Ok(args[0].clone())
        })
    });

    runtime.define_host_method(object.clone(), "querySelector", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let root = require_node(runtime, &this)?;
            let selector = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("querySelector needs a selector"))?;
            let document = document_of(&state);
            let document = document.borrow();
            match browser_selectors::query_selector(&document, root, &selector) {
                Ok(Some(node)) => {
                    drop(document);
                    Ok(wrap(runtime, &state, node))
                }
                Ok(None) => Ok(Value::Null),
                Err(err) => Err(JsError::new(err.to_string())),
            }
        })
    });

    runtime.define_host_method(object.clone(), "querySelectorAll", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let root = require_node(runtime, &this)?;
            let selector = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("querySelectorAll needs a selector"))?;
            let document = document_of(&state);
            let nodes = {
                let document = document.borrow();
                browser_selectors::query_selector_all(&document, root, &selector)
                    .map_err(|err| JsError::new(err.to_string()))?
            };
            let items = nodes
                .into_iter()
                .map(|node| wrap(runtime, &state, node))
                .collect();
            Ok(runtime.new_array(items))
        })
    });

    runtime.define_host_method(object.clone(), "addEventListener", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            crate::events::add_listener(runtime, &state, node, args, false);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "removeEventListener", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            crate::events::remove_listener(&state, node, args);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "dispatchEvent", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            let event_type = args
                .first()
                .filter(|value| value.type_of() == "object")
                .map(|value| runtime.get_property(value, "type").to_string_value())
                .unwrap_or_else(|| "custom".to_string());
            let handled = crate::events::dispatch_event(runtime, &state, node, &event_type);
            Ok(Value::Bool(handled))
        })
    });

    runtime.define_host_method(object.clone(), "click", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let node = require_node(runtime, &this)?;
            crate::events::dispatch_event(runtime, &state, node, "click");
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "getElementsByTagName", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let _root = require_node(runtime, &this)?;
            let tag = args
                .first()
                .map(Value::to_string_value)
                .unwrap_or_else(|| "*".to_string());
            let document = document_of(&state);
            let nodes = document.borrow().get_elements_by_tag_name(&tag);
            let items = nodes
                .into_iter()
                .map(|node| wrap(runtime, &state, node))
                .collect();
            Ok(runtime.new_array(items))
        })
    });

    install_accessors(runtime, state, object);
}

fn install_accessors(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, object: Value) {
    runtime.define_accessor(
        object.clone(),
        "id",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let document = document.borrow();
                Ok(match document.id_attr(node) {
                    Some(value) => Value::String(value.to_string()),
                    None => Value::String(String::new()),
                })
            })
        },
        {
            let state = state.clone();
            Box::new(move |runtime, this, args| {
                let node = require_node(runtime, &this)?;
                let value = args.first().map(Value::to_string_value).unwrap_or_default();
                let document = document_of(&state);
                document.borrow_mut().set_attr(node, "id", &value);
                Ok(Value::Undefined)
            })
        },
    );

    runtime.define_accessor(
        object.clone(),
        "className",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let document = document.borrow();
                Ok(match document.attr(node, "class") {
                    Some(value) => Value::String(value.to_string()),
                    None => Value::String(String::new()),
                })
            })
        },
        {
            let state = state.clone();
            Box::new(move |runtime, this, args| {
                let node = require_node(runtime, &this)?;
                let value = args.first().map(Value::to_string_value).unwrap_or_default();
                let document = document_of(&state);
                document.borrow_mut().set_attr(node, "class", &value);
                Ok(Value::Undefined)
            })
        },
    );

    runtime.define_accessor(
        object.clone(),
        "textContent",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let text = document.borrow().text_content(node);
                Ok(Value::String(text))
            })
        },
        {
            let state = state.clone();
            Box::new(move |runtime, this, args| {
                let node = require_node(runtime, &this)?;
                let text = args.first().map(Value::to_string_value).unwrap_or_default();
                let document = document_of(&state);
                document.borrow_mut().set_text_content(node, &text);
                Ok(Value::Undefined)
            })
        },
    );

    runtime.define_accessor(
        object.clone(),
        "innerHTML",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let html = {
                    let document = document.borrow();
                    serialize_children(&document, node)
                };
                Ok(Value::String(html))
            })
        },
        {
            let state = state.clone();
            Box::new(move |runtime, this, args| {
                let node = require_node(runtime, &this)?;
                let html = args.first().map(Value::to_string_value).unwrap_or_default();
                let document = document_of(&state);
                {
                    let mut document = document.borrow_mut();
                    let fragment = browser_html_parser::parse_fragment(&mut document, node, &html);
                    let children: Vec<NodeId> = document.children(node).to_vec();
                    for child in children {
                        document.remove_child(node, child);
                    }
                    for child in fragment {
                        document.append_child(node, child);
                    }
                }
                Ok(Value::Undefined)
            })
        },
    );

    runtime.define_accessor(
        object.clone(),
        "value",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let document = document.borrow();
                Ok(match document.attr(node, "value") {
                    Some(value) => Value::String(value.to_string()),
                    None => Value::String(String::new()),
                })
            })
        },
        {
            let state = state.clone();
            Box::new(move |runtime, this, args| {
                let node = require_node(runtime, &this)?;
                let value = args.first().map(Value::to_string_value).unwrap_or_default();
                let document = document_of(&state);
                document.borrow_mut().set_attr(node, "value", &value);
                Ok(Value::Undefined)
            })
        },
    );

    runtime.define_accessor(
        object.clone(),
        "tagName",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let document = document.borrow();
                Ok(match document.element_name(node) {
                    Some(name) if document.namespace(node) == Some(Namespace::Html) => {
                        Value::String(name.to_ascii_uppercase())
                    }
                    Some(name) => Value::String(name.to_string()),
                    None => Value::String(String::new()),
                })
            })
        },
        { Box::new(|_runtime, _this, _args| Err(JsError::type_error("tagName is read-only"))) },
    );

    runtime.define_accessor(
        object.clone(),
        "children",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let ids = {
                    let document = document.borrow();
                    document.element_children(node)
                };
                let items = ids
                    .into_iter()
                    .map(|child| wrap(runtime, &state, child))
                    .collect();
                Ok(runtime.new_array(items))
            })
        },
        { Box::new(|_runtime, _this, _args| Ok(Value::Undefined)) },
    );
}

/// Returns `true` when the node is an element, text or document node.
pub fn is_node(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>, value: &Value) -> bool {
    match node_id_of(runtime, value) {
        Some(node) => {
            let document = document_of(state);
            let document = document.borrow();
            node.0 < document.node_count()
        }
        None => false,
    }
}
