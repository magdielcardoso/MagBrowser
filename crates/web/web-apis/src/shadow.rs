//! Shadow DOM bindings: `Element.attachShadow()`, `Element.shadowRoot` and the
//! `ShadowRoot` wrapper.
//!
//! A shadow root is an ordinary arena node in [`browser_dom::Document`] whose
//! child list is separate from the host's light DOM. The wrapper reuses
//! [`crate::element::wrap`], so `innerHTML`, `appendChild` and
//! `querySelector(All)` operate directly on the shadow tree.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::{NodeId, ShadowRootMode};
use browser_js::{JsError, Runtime, Value};

use crate::{HostState, element};

/// Installs the `ShadowRoot` global constructor shim.
pub fn install(runtime: &mut Runtime, _state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function(
        "ShadowRoot",
        Box::new(|runtime, this, _args| {
            Ok(if matches!(this, Value::Object(_)) {
                this
            } else {
                runtime.new_object()
            })
        }),
    );
}

/// Adds the shadow-DOM members to an element wrapper object. Called by
/// [`crate::element::wrap`] for every wrapped node.
pub fn install_element_shadow(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    object: Value,
) {
    runtime.define_host_method(object.clone(), "attachShadow", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let host = element::require_node(runtime, &this)?;
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            let raw_mode = runtime.get_property(&options, "mode").to_string_value();
            let mode = match raw_mode.as_str() {
                "closed" => ShadowRootMode::Closed,
                "open" | "" => ShadowRootMode::Open,
                other => {
                    return Err(JsError::type_error(format!(
                        "attachShadow mode must be 'open' or 'closed', got '{other}'"
                    )));
                }
            };
            let document = state.borrow().document.clone();
            let root = document
                .borrow_mut()
                .attach_shadow(host, mode)
                .ok_or_else(|| {
                    JsError::new(
                        "attachShadow failed (not an element or already has a shadow root)",
                    )
                })?;
            let wrapper = element::wrap(runtime, &state, root);
            runtime.set_property(
                wrapper.clone(),
                "__shadowHost",
                Value::Number(host.0 as f64),
            );
            runtime.set_property(
                wrapper.clone(),
                "__shadowMode",
                Value::String(mode.as_str().to_owned()),
            );
            runtime.set_property(
                wrapper.clone(),
                "mode",
                Value::String(mode.as_str().to_owned()),
            );
            let host_wrapper = element::wrap(runtime, &state, host);
            runtime.set_property(wrapper.clone(), "host", host_wrapper);
            Ok(wrapper)
        })
    });

    runtime.define_accessor(
        object.clone(),
        "shadowRoot",
        {
            let state = state.clone();
            Box::new(move |runtime, this, _args| {
                let host = element::require_node(runtime, &this)?;
                let document = state.borrow().document.clone();
                let root = {
                    let document = document.borrow();
                    document
                        .shadow_root(host)
                        .filter(|shadow| shadow.mode == ShadowRootMode::Open)
                        .map(|shadow| shadow.root)
                };
                Ok(root.map_or(Value::Null, |root| element::wrap(runtime, &state, root)))
            })
        },
        { Box::new(|_runtime, _this, _args| Err(JsError::type_error("shadowRoot is read-only"))) },
    );

    runtime.define_host_method(object.clone(), "assignedNodes", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let node = element::require_node(runtime, &this)?;
            let document = state.borrow().document.clone();
            let (host, assigned) = {
                let document = document.borrow();
                if !document.is_tag(node, "slot") {
                    (None, Vec::new())
                } else {
                    // A slot's parent is the shadow-root arena node; the host is
                    // reached through the shadow-root side table.
                    let host = document
                        .parent(node)
                        .and_then(|parent| document.shadow_root_host(parent));
                    let nodes =
                        host.map_or_else(Vec::new, |host| document.assigned_nodes(host, node));
                    (host, nodes)
                }
            };
            let _ = host;
            let items = assigned
                .into_iter()
                .map(|node| element::wrap(runtime, &state, node))
                .collect();
            Ok(runtime.new_array(items))
        })
    });

    runtime.define_host_method(object.clone(), "assignedElements", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let node = element::require_node(runtime, &this)?;
            let document = state.borrow().document.clone();
            let assigned = {
                let document = document.borrow();
                if !document.is_tag(node, "slot") {
                    Vec::new()
                } else {
                    document
                        .parent(node)
                        .and_then(|parent| document.shadow_root_host(parent))
                        .map_or_else(Vec::new, |host| document.assigned_nodes(host, node))
                }
            };
            let items = assigned
                .into_iter()
                .filter(|node| {
                    let document = state.borrow().document.clone();
                    document.borrow().node(*node).is_element()
                })
                .map(|node| element::wrap(runtime, &state, node))
                .collect();
            Ok(runtime.new_array(items))
        })
    });
}

/// True when `node` is a slot in a shadow tree, used by selector helpers.
pub fn is_slotted(state: &Rc<RefCell<HostState>>, node: NodeId) -> bool {
    let document = state.borrow().document.clone();
    document.borrow().is_slotted(node)
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

    #[test]
    fn attach_shadow_exposes_a_separate_tree() {
        let (mut runtime, document, _state) =
            setup("<html><body><div id='host'><p class='light'>light</p></div></body></html>");
        let host = document.borrow().get_element_by_id("host").unwrap();
        runtime
            .eval(
                "var host = document.getElementById('host'); \
                 var root = host.attachShadow({mode: 'open'}); \
                 root.innerHTML = '<span class=\"shadow\">shadow</span>';",
            )
            .unwrap();

        assert!(matches!(
            runtime.eval("host.shadowRoot === root").unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            runtime
                .eval("host.querySelector('.light') !== null")
                .unwrap(),
            Value::Bool(true)
        ));
        assert_eq!(
            runtime
                .eval("root.querySelector('.shadow').textContent")
                .unwrap()
                .to_string_value(),
            "shadow"
        );
        assert_eq!(
            runtime
                .eval("root.querySelectorAll('span').length")
                .unwrap()
                .to_number(),
            1.0
        );
        // Shadow content is invisible to a host-scoped query.
        assert!(matches!(
            runtime
                .eval("host.querySelector('.shadow') === null")
                .unwrap(),
            Value::Bool(true)
        ));
        let shadow_root = document.borrow().shadow_root(host).cloned().unwrap();
        assert_eq!(shadow_root.mode, ShadowRootMode::Open);
    }

    #[test]
    fn closed_shadow_root_is_hidden_but_tree_still_works() {
        let (mut runtime, _document, _state) =
            setup("<html><body><div id='host'></div></body></html>");
        runtime
            .eval(
                "var host = document.getElementById('host'); \
                 var root = host.attachShadow({mode: 'closed'}); \
                 root.innerHTML = '<b id=\"inside\">hi</b>';",
            )
            .unwrap();
        assert!(matches!(
            runtime.eval("host.shadowRoot === null").unwrap(),
            Value::Bool(true)
        ));
        assert_eq!(
            runtime
                .eval("root.querySelector('#inside').textContent")
                .unwrap()
                .to_string_value(),
            "hi"
        );
        assert!(runtime.eval("host.attachShadow({mode:'open'})").is_err());
    }

    #[test]
    fn slot_assignment_is_visible_from_js() {
        let (mut runtime, _document, _state) = setup(
            "<html><body><div id='host'><h1 slot='title'>Title</h1><p>Other</p></div></body></html>",
        );
        runtime
            .eval(
                "var host = document.getElementById('host'); \
                 var root = host.attachShadow({mode:'open'}); \
                 root.innerHTML = '<slot name=\"title\"></slot><slot></slot>'; \
                 var named = root.querySelector('slot[name=title]'); \
                 var def = root.querySelectorAll('slot')[1];",
            )
            .unwrap();
        assert_eq!(
            runtime
                .eval("named.assignedElements().length")
                .unwrap()
                .to_number(),
            1.0
        );
        assert_eq!(
            runtime
                .eval("named.assignedElements()[0].tagName")
                .unwrap()
                .to_string_value(),
            "H1"
        );
        assert_eq!(
            runtime
                .eval("def.assignedNodes().length")
                .unwrap()
                .to_number(),
            1.0
        );
    }
}
