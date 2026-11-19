//! Element wrapper objects and their DOM methods.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::{Document, Namespace, NodeData, NodeId, serialize_children};
use browser_js::{JsError, Runtime, Value};

use crate::HostState;
use crate::form::FormSubmission;

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
    crate::cssom::install_element_cssom(runtime, state, object.clone(), node);
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

/// True when `node` is a `<form>` element.
fn is_form(state: &Rc<RefCell<HostState>>, node: NodeId) -> bool {
    let document = document_of(state);
    document
        .borrow()
        .element_name(node)
        .is_some_and(|name| name.eq_ignore_ascii_case("form"))
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
            let handled = match args.first() {
                Some(Value::Object(_)) => {
                    crate::events::dispatch_event_object(runtime, &state, node, args[0].clone())
                        .handled
                }
                _ => {
                    let event_type = args
                        .first()
                        .map(Value::to_string_value)
                        .unwrap_or_else(|| "custom".to_string());
                    crate::events::dispatch_event(runtime, &state, node, &event_type)
                }
            };
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

    // Form submission. The bindings only record the request; the embedder
    // drains it from `HostState` and performs the actual navigation, which keeps
    // the network and history logic in `browser-core`.
    runtime.define_host_method(object.clone(), "submit", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let node = require_node(runtime, &this)?;
            if !is_form(&state, node) {
                return Err(JsError::type_error("submit() is only available on forms"));
            }
            state.borrow_mut().form_submission = Some(FormSubmission {
                form: node,
                submitter: None,
            });
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "requestSubmit", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let node = require_node(runtime, &this)?;
            if !is_form(&state, node) {
                return Err(JsError::type_error(
                    "requestSubmit() is only available on forms",
                ));
            }
            let submitter = match args.first() {
                None | Some(Value::Undefined) | Some(Value::Null) => None,
                Some(value) => {
                    let candidate = require_node(runtime, value)?;
                    let document = document_of(&state);
                    let document = document.borrow();
                    if !document.contains(candidate) || !document.is_ancestor_of(node, candidate) {
                        return Err(JsError::type_error(
                            "requestSubmit() submitter must be a descendant of the form",
                        ));
                    }
                    Some(candidate)
                }
            };
            state.borrow_mut().form_submission = Some(FormSubmission {
                form: node,
                submitter,
            });
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "focus", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let node = require_node(runtime, &this)?;
            state.borrow_mut().focused = Some(node);
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "blur", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let node = require_node(runtime, &this)?;
            let mut state = state.borrow_mut();
            if state.focused == Some(node) {
                state.focused = None;
            }
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
        "checked",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                Ok(Value::Bool(document.borrow().has_attr(node, "checked")))
            })
        },
        {
            let state = state.clone();
            Box::new(move |runtime, this, args| {
                let node = require_node(runtime, &this)?;
                let checked = args.first().is_some_and(Value::is_truthy);
                let document = document_of(&state);
                if checked {
                    document.borrow_mut().set_attr(node, "checked", "");
                } else {
                    document.borrow_mut().remove_attr(node, "checked");
                }
                Ok(Value::Undefined)
            })
        },
    );

    runtime.define_accessor(
        object.clone(),
        "disabled",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                Ok(Value::Bool(document.borrow().has_attr(node, "disabled")))
            })
        },
        {
            let state = state.clone();
            Box::new(move |runtime, this, args| {
                let node = require_node(runtime, &this)?;
                let disabled = args.first().is_some_and(Value::is_truthy);
                let document = document_of(&state);
                if disabled {
                    document.borrow_mut().set_attr(node, "disabled", "");
                } else {
                    document.borrow_mut().remove_attr(node, "disabled");
                }
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

    runtime.define_accessor(
        object.clone(),
        "childNodes",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let ids = {
                    let document = document.borrow();
                    document.children(node).to_vec()
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

    runtime.define_accessor(
        object.clone(),
        "parentNode",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let node = require_node(runtime, &this)?;
                let document = document_of(&state);
                let parent = document.borrow().parent(node);
                match parent {
                    Some(parent) => Ok(wrap(runtime, &state, parent)),
                    None => Ok(Value::Null),
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;
    use browser_html_parser::parse_document;

    fn setup(html: &str) -> (Runtime, Rc<RefCell<Document>>, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document(html)));
        let state = crate::install(&mut runtime, document.clone(), None);
        (runtime, document, state)
    }

    fn eval(runtime: &mut Runtime, source: &str) -> String {
        runtime.eval(source).unwrap().to_string_value()
    }

    #[test]
    fn create_append_and_attribute_roundtrip() {
        let (mut runtime, document, _state) =
            setup("<html><body><div id='root'></div></body></html>");
        let root = document.borrow().get_element_by_id("root").unwrap();

        runtime
            .eval(
                "var root = document.getElementById('root');\n\
                 var el = document.createElement('span');\n\
                 el.setAttribute('data-x', 'hello');\n\
                 el.appendChild(document.createTextNode('text'));\n\
                 root.appendChild(el);",
            )
            .unwrap();

        assert_eq!(eval(&mut runtime, "el.getAttribute('data-x')"), "hello");
        assert_eq!(eval(&mut runtime, "el.tagName"), "SPAN");
        assert_eq!(eval(&mut runtime, "el.textContent"), "text");
        assert_eq!(eval(&mut runtime, "root.children.length"), "1");
        assert_eq!(eval(&mut runtime, "root.childNodes.length"), "1");
        assert_eq!(eval(&mut runtime, "el.parentNode.id"), "root");
        assert!(document.borrow().parent(root).is_some());

        runtime.eval("el.removeAttribute('data-x')").unwrap();
        assert_eq!(eval(&mut runtime, "el.getAttribute('data-x')"), "null");

        runtime.eval("root.removeChild(el)").unwrap();
        assert_eq!(eval(&mut runtime, "root.children.length"), "0");
    }

    #[test]
    fn child_nodes_include_text_while_children_does_not() {
        let (mut runtime, _document, _state) =
            setup("<html><body><div id='root'>before<span></span>after</div></body></html>");
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('root').childNodes.length"
            ),
            "3"
        );
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('root').children.length"
            ),
            "1"
        );
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('root').childNodes[0].nodeType"
            ),
            "3"
        );
    }

    #[test]
    fn text_content_setter_replaces_children() {
        let (mut runtime, _document, _state) =
            setup("<html><body><div id='root'></div></body></html>");
        runtime
            .eval(
                "var root = document.getElementById('root');\n\
                 root.textContent = 'replaced';",
            )
            .unwrap();
        assert_eq!(eval(&mut runtime, "root.textContent"), "replaced");
        assert_eq!(eval(&mut runtime, "root.childNodes.length"), "1");
    }

    #[test]
    fn forms_expose_submit_and_request_submit() {
        let (mut runtime, document, state) = setup(
            "<html><body><form id='f'><input name='q'>\
             <button id='go' type='submit' name='go' value='1'>Go</button></form>\
             <div id='not-a-form'></div></body></html>",
        );
        let form = document.borrow().get_element_by_id("f").unwrap();
        let button = document.borrow().get_element_by_id("go").unwrap();

        runtime
            .eval("document.getElementById('f').submit()")
            .unwrap();
        let submission = state.borrow_mut().take_form_submission().unwrap();
        assert_eq!(submission.form, form);
        assert_eq!(submission.submitter, None);

        runtime
            .eval("document.getElementById('f').requestSubmit(document.getElementById('go'))")
            .unwrap();
        let submission = state.borrow_mut().take_form_submission().unwrap();
        assert_eq!(submission.form, form);
        assert_eq!(submission.submitter, Some(button));

        runtime
            .eval("document.getElementById('f').requestSubmit()")
            .unwrap();
        assert_eq!(
            state.borrow_mut().take_form_submission().unwrap().submitter,
            None
        );
    }

    #[test]
    fn focus_and_blur_track_the_active_element() {
        let (mut runtime, document, state) = setup("<html><body><input id='q'></body></html>");
        let input = document.borrow().get_element_by_id("q").unwrap();
        runtime
            .eval("document.getElementById('q').focus()")
            .unwrap();
        assert_eq!(state.borrow().focused, Some(input));
        assert_eq!(eval(&mut runtime, "document.activeElement.id"), "q");
        runtime.eval("document.getElementById('q').blur()").unwrap();
        assert_eq!(state.borrow().focused, None);
        assert_eq!(eval(&mut runtime, "document.activeElement"), "null");
    }

    #[test]
    fn query_selectors_work_from_document_and_element() {
        let (mut runtime, _document, _state) = setup(
            "<html><body><div id='root'><p class='item'>a</p><p class='item'>b</p></div></body></html>",
        );
        assert_eq!(
            eval(&mut runtime, "document.querySelectorAll('p.item').length"),
            "2"
        );
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('root').querySelector('p').textContent"
            ),
            "a"
        );
        assert_eq!(
            eval(
                &mut runtime,
                "document.getElementById('root').querySelectorAll('p')[1].textContent"
            ),
            "b"
        );
    }
}
