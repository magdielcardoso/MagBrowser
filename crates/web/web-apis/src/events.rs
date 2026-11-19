//! DOM events: listener registry, dispatch, bubbling and event objects.
//!
//! Listeners always receive a real event object: besides `type`, `target` and
//! `currentTarget` it carries `preventDefault()` / `stopPropagation()` methods
//! and the optional mouse (`clientX` / `clientY`) or keyboard (`key` / `code`)
//! details supplied by the host or by a `MouseEvent` / `KeyboardEvent`
//! constructor in script.

use std::cell::RefCell;
use std::rc::Rc;

use browser_dom::NodeId;
use browser_js::{HostFunction, JsError, Runtime, Value};

use crate::HostState;
use crate::element::wrap;
use crate::form::{FormSubmission, default_submit_button, form_for_input, single_text_control};

/// Initialization data for a synthetic event.
///
/// The host uses this to dispatch events with mouse or keyboard details; script
/// uses the `Event`, `MouseEvent` and `KeyboardEvent` globals which build the
/// same set of properties.
#[derive(Clone, Debug, Default)]
pub struct EventInit {
    /// The event's `type` (for example `click`).
    pub event_type: String,
    /// Whether the event bubbles to ancestors. Defaults to `true` for host
    /// dispatches so existing callers keep their bubbling behaviour.
    pub bubbles: bool,
    /// Whether `preventDefault()` is meaningful for this event.
    pub cancelable: bool,
    /// Mouse horizontal coordinate.
    pub client_x: f64,
    /// Mouse vertical coordinate.
    pub client_y: f64,
    /// `KeyboardEvent.key` value.
    pub key: String,
    /// `KeyboardEvent.code` value.
    pub code: String,
}

impl EventInit {
    /// Creates a bubbling, cancelable event of the given type.
    pub fn new(event_type: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            bubbles: true,
            cancelable: true,
            ..Self::default()
        }
    }

    /// Creates an event carrying mouse coordinates.
    pub fn mouse(event_type: impl Into<String>, client_x: f64, client_y: f64) -> Self {
        Self {
            client_x,
            client_y,
            ..Self::new(event_type)
        }
    }

    /// Creates an event carrying keyboard `key` and `code`.
    pub fn keyboard(
        event_type: impl Into<String>,
        key: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            code: code.into(),
            ..Self::new(event_type)
        }
    }
}

/// Result of dispatching an event.
#[derive(Clone, Copy, Debug, Default)]
pub struct DispatchOutcome {
    /// Whether at least one listener ran.
    pub handled: bool,
    /// Whether a listener called `preventDefault()`.
    pub default_prevented: bool,
}

#[derive(Default)]
struct EventFlags {
    default_prevented: bool,
    propagation_stopped: bool,
}

/// Installs `window.addEventListener`-style globals on the runtime.
pub fn install_global(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let global = runtime.global();
    runtime.set_global("window", global.clone());
    runtime.set_global("self", global.clone());

    runtime.define_host_method(global.clone(), "addEventListener", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            add_listener(runtime, &state, NodeId(0), args, true);
            Ok(Value::Undefined)
        })
    });
    runtime.define_host_method(global.clone(), "removeEventListener", {
        let state = state.clone();
        Box::new(move |_runtime, _this, args| {
            remove_listener(&state, NodeId(0), args);
            Ok(Value::Undefined)
        })
    });
    runtime.define_host_method(global.clone(), "dispatchEvent", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let handled = match args.first() {
                Some(Value::Object(_)) => {
                    dispatch_event_object(runtime, &state, NodeId(0), args[0].clone()).handled
                }
                _ => {
                    let event_type = args
                        .first()
                        .map(Value::to_string_value)
                        .unwrap_or_else(|| "custom".to_string());
                    dispatch_event(runtime, &state, NodeId(0), &event_type)
                }
            };
            Ok(Value::Bool(handled))
        })
    });

    define_event_constructor(runtime, "Event", EventKind::Base);
    define_event_constructor(runtime, "MouseEvent", EventKind::Mouse);
    define_event_constructor(runtime, "KeyboardEvent", EventKind::Keyboard);
}

#[derive(Clone, Copy)]
enum EventKind {
    Base,
    Mouse,
    Keyboard,
}

fn define_event_constructor(runtime: &mut Runtime, name: &'static str, kind: EventKind) {
    runtime.define_global_function(
        name,
        Box::new(move |runtime, this, args| {
            let event_type = args.first().map(Value::to_string_value).unwrap_or_default();
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            let bubbles = read_bool(runtime, &init, "bubbles", false);
            let cancelable = read_bool(runtime, &init, "cancelable", false);
            let client_x = read_number(runtime, &init, "clientX");
            let client_y = read_number(runtime, &init, "clientY");
            let key = read_string(runtime, &init, "key");
            let code = read_string(runtime, &init, "code");

            runtime.set_property(this.clone(), "type", Value::String(event_type));
            runtime.set_property(this.clone(), "bubbles", Value::Bool(bubbles));
            runtime.set_property(this.clone(), "cancelable", Value::Bool(cancelable));
            runtime.set_property(this.clone(), "defaultPrevented", Value::Bool(false));
            match kind {
                EventKind::Base => {}
                EventKind::Mouse => {
                    runtime.set_property(this.clone(), "clientX", Value::Number(client_x));
                    runtime.set_property(this.clone(), "clientY", Value::Number(client_y));
                }
                EventKind::Keyboard => {
                    runtime.set_property(this.clone(), "key", Value::String(key));
                    runtime.set_property(this.clone(), "code", Value::String(code));
                }
            }
            let flags = Rc::new(RefCell::new(EventFlags::default()));
            install_event_methods(runtime, this, &flags);
            Ok(Value::Undefined)
        }),
    );
}

fn read_string(runtime: &mut Runtime, object: &Value, key: &str) -> String {
    match runtime.get_property(object, key) {
        Value::String(value) => value,
        Value::Undefined | Value::Null => String::new(),
        other => other.to_string_value(),
    }
}

fn read_number(runtime: &mut Runtime, object: &Value, key: &str) -> f64 {
    match runtime.get_property(object, key) {
        Value::Number(value) => value,
        Value::Undefined | Value::Null => 0.0,
        other => other.to_number(),
    }
}

fn read_bool(runtime: &mut Runtime, object: &Value, key: &str, default: bool) -> bool {
    match runtime.get_property(object, key) {
        Value::Bool(value) => value,
        Value::Undefined | Value::Null => default,
        other => other.is_truthy(),
    }
}

fn event_init_from_value(runtime: &mut Runtime, event: &Value) -> EventInit {
    let event_type = read_string(runtime, event, "type");
    EventInit {
        event_type: if event_type.is_empty() {
            "custom".to_string()
        } else {
            event_type
        },
        bubbles: read_bool(runtime, event, "bubbles", true),
        cancelable: read_bool(runtime, event, "cancelable", true),
        client_x: read_number(runtime, event, "clientX"),
        client_y: read_number(runtime, event, "clientY"),
        key: read_string(runtime, event, "key"),
        code: read_string(runtime, event, "code"),
    }
}

/// Installs the functional `preventDefault` / `stopPropagation` methods on an
/// event object, sharing the flags the dispatch loop inspects.
fn install_event_methods(runtime: &mut Runtime, event: Value, flags: &Rc<RefCell<EventFlags>>) {
    let prevent_event = event.clone();
    let prevent_flags = flags.clone();
    let prevent_default: HostFunction = Box::new(move |runtime, _this, _args| {
        prevent_flags.borrow_mut().default_prevented = true;
        runtime.set_property(prevent_event.clone(), "defaultPrevented", Value::Bool(true));
        Ok(Value::Undefined)
    });
    runtime.define_host_method(event.clone(), "preventDefault", prevent_default);

    let stop_flags = flags.clone();
    let stop_propagation: HostFunction = Box::new(move |_runtime, _this, _args| {
        stop_flags.borrow_mut().propagation_stopped = true;
        Ok(Value::Undefined)
    });
    runtime.define_host_method(event, "stopPropagation", stop_propagation);
}

fn write_event_properties(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    event: Value,
    target: NodeId,
    init: &EventInit,
) {
    runtime.set_property(
        event.clone(),
        "type",
        Value::String(init.event_type.clone()),
    );
    runtime.set_property(event.clone(), "bubbles", Value::Bool(init.bubbles));
    runtime.set_property(event.clone(), "cancelable", Value::Bool(init.cancelable));
    runtime.set_property(event.clone(), "defaultPrevented", Value::Bool(false));
    let target_wrapper = wrap(runtime, state, target);
    runtime.set_property(event.clone(), "target", target_wrapper);
    runtime.set_property(event.clone(), "currentTarget", Value::Null);
    runtime.set_property(event.clone(), "clientX", Value::Number(init.client_x));
    runtime.set_property(event.clone(), "clientY", Value::Number(init.client_y));
    runtime.set_property(event.clone(), "key", Value::String(init.key.clone()));
    runtime.set_property(event, "code", Value::String(init.code.clone()));
}

fn make_event(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    target: NodeId,
    init: &EventInit,
) -> (Value, Rc<RefCell<EventFlags>>) {
    let event = runtime.new_object();
    write_event_properties(runtime, state, event.clone(), target, init);
    let flags = Rc::new(RefCell::new(EventFlags::default()));
    install_event_methods(runtime, event.clone(), &flags);
    (event, flags)
}

/// Runs the listener chain for `event_type`, bubbling to ancestors when the
/// event allows it and stopping early on `stopPropagation()`.
#[allow(clippy::too_many_arguments)]
fn run_dispatch(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event_type: &str,
    bubbles: bool,
    event: Value,
    flags: &Rc<RefCell<EventFlags>>,
) -> DispatchOutcome {
    let mut outcome = DispatchOutcome::default();
    let mut current = Some(node);
    let mut depth = 0;
    while let Some(target) = current {
        if depth > 256 {
            break;
        }
        depth += 1;
        let handlers = state
            .borrow()
            .events
            .get(&(target.0, event_type.to_string()))
            .cloned()
            .unwrap_or_default();
        if !handlers.is_empty() {
            let this = wrap(runtime, state, target);
            runtime.set_property(event.clone(), "currentTarget", this.clone());
            for handler in handlers {
                let _ = runtime.call_function(&handler, this.clone(), vec![event.clone()]);
                outcome.handled = true;
            }
        }
        // stopPropagation prevents the event from reaching ancestors, but the
        // remaining listeners on this node have already run (as in the DOM).
        if !bubbles || flags.borrow().propagation_stopped {
            break;
        }
        current = {
            let document = state.borrow().document.clone();
            document.borrow().parent(target)
        };
    }
    runtime.set_property(event, "currentTarget", Value::Null);
    outcome.default_prevented = flags.borrow().default_prevented;
    outcome
}

/// Registers a listener from an `addEventListener(type, handler)` call.
pub fn add_listener(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    args: Vec<Value>,
    _global: bool,
) {
    let Some(event_type) = args.first().map(Value::to_string_value) else {
        return;
    };
    let Some(handler) = args.get(1).cloned() else {
        return;
    };
    if handler.type_of() != "function" && handler.type_of() != "object" {
        return;
    }
    let _ = runtime;
    state
        .borrow_mut()
        .events
        .entry((node.0, event_type))
        .or_default()
        .push(handler);
}

/// Removes a listener registered with the same function.
pub fn remove_listener(state: &Rc<RefCell<HostState>>, node: NodeId, args: Vec<Value>) {
    let Some(event_type) = args.first().map(Value::to_string_value) else {
        return;
    };
    let Some(handler) = args.get(1) else {
        return;
    };
    let mut state = state.borrow_mut();
    if let Some(list) = state.events.get_mut(&(node.0, event_type)) {
        list.retain(|existing| !existing.strict_equals(handler));
    }
}

/// Implicit form submission for Enter in a focused, single-text-control form.
fn maybe_submit_on_enter(state: &Rc<RefCell<HostState>>, node: NodeId) {
    let (document, focused, pending) = {
        let state = state.borrow();
        (
            state.document.clone(),
            state.focused,
            state.form_submission.is_some() || state.navigation.is_some(),
        )
    };
    if pending || focused.is_some_and(|focused| focused != node) {
        return;
    }
    let document = document.borrow();
    let Some(form) = form_for_input(&document, node) else {
        return;
    };
    if single_text_control(&document, form) != Some(node) {
        return;
    }
    let submitter = default_submit_button(&document, form);
    drop(document);
    state.borrow_mut().form_submission = Some(FormSubmission { form, submitter });
}

/// Dispatches an event described by [`EventInit`] at `node`.
///
/// Returns whether a listener ran and whether any listener called
/// `preventDefault()`.
pub fn dispatch_event_with(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    init: EventInit,
) -> DispatchOutcome {
    let (event, flags) = make_event(runtime, state, node, &init);
    let outcome = run_dispatch(
        runtime,
        state,
        node,
        &init.event_type,
        init.bubbles,
        event,
        &flags,
    );
    // Enter in a single-line text input submits the form, unless a listener
    // cancelled the keydown. The embedder drains `form_submission` and performs
    // the real navigation.
    if init.event_type.eq_ignore_ascii_case("keydown")
        && init.key == "Enter"
        && !outcome.default_prevented
    {
        maybe_submit_on_enter(state, node);
    }
    outcome
}

/// Dispatches a mouse event with host-provided coordinates.
pub fn dispatch_mouse_event(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event_type: &str,
    client_x: f64,
    client_y: f64,
) -> DispatchOutcome {
    dispatch_event_with(
        runtime,
        state,
        node,
        EventInit::mouse(event_type, client_x, client_y),
    )
}

/// Dispatches a keyboard event with host-provided `key` and `code`.
pub fn dispatch_keyboard_event(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event_type: &str,
    key: &str,
    code: &str,
) -> DispatchOutcome {
    dispatch_event_with(
        runtime,
        state,
        node,
        EventInit::keyboard(event_type, key, code),
    )
}

/// Dispatches an event object created in script (for example with
/// `new MouseEvent(...)`) at `node`, reusing that object for every listener.
pub fn dispatch_event_object(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event: Value,
) -> DispatchOutcome {
    let init = event_init_from_value(runtime, &event);
    write_event_properties(runtime, state, event.clone(), node, &init);
    let flags = Rc::new(RefCell::new(EventFlags::default()));
    install_event_methods(runtime, event.clone(), &flags);
    run_dispatch(
        runtime,
        state,
        node,
        &init.event_type,
        init.bubbles,
        event,
        &flags,
    )
}

/// Dispatches `event_type` at `node`, bubbling to ancestors.
///
/// Returns `true` when at least one listener ran. This is the long-standing
/// host-facing API used by `browser-core`; richer data is available through
/// [`dispatch_event_with`].
pub fn dispatch_event(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event_type: &str,
) -> bool {
    dispatch_event_with(runtime, state, node, EventInit::new(event_type)).handled
}

/// Count of registered listeners (for tests and diagnostics).
pub fn listener_count(state: &Rc<RefCell<HostState>>) -> usize {
    state.borrow().events.values().map(Vec::len).sum()
}

/// Runs listeners for a node without constructing a bubbling chain.
pub fn dispatch_direct(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    node: NodeId,
    event_type: &str,
) -> Result<bool, JsError> {
    Ok(dispatch_event(runtime, state, node, event_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;
    use browser_html_parser::parse_document;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn setup() -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(Document::new()));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    fn setup_html(html: &str) -> (Runtime, Rc<RefCell<Document>>, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document(html)));
        let state = crate::install(&mut runtime, document.clone(), None);
        (runtime, document, state)
    }

    fn number(runtime: &mut Runtime, source: &str) -> f64 {
        runtime.eval(source).unwrap().to_number()
    }

    #[test]
    fn listeners_are_registered_and_dispatched() {
        let (mut runtime, state) = setup();
        runtime
            .eval("var hits = 0; document.addEventListener('click', function(e) { hits = hits + 1; });")
            .unwrap();
        let ran = dispatch_event(&mut runtime, &state, NodeId(0), "click");
        assert!(ran);
        let hits = runtime.eval("hits").unwrap();
        assert_eq!(hits.to_number(), 1.0);
    }

    #[test]
    fn removal_stops_dispatch() {
        let (mut runtime, state) = setup();
        runtime
            .eval(
                "var hits = 0; var fn = function() { hits = hits + 1; };\n\
                 document.addEventListener('x', fn);\n\
                 document.removeEventListener('x', fn);",
            )
            .unwrap();
        assert!(!dispatch_event(&mut runtime, &state, NodeId(0), "x"));
        assert_eq!(runtime.eval("hits").unwrap().to_number(), 0.0);
    }

    #[test]
    fn events_bubble_from_element_to_document() {
        let (mut runtime, state) = setup();
        let node = {
            let document = state.borrow().document.clone();
            let mut document = document.borrow_mut();
            let body = document.create_element("body");
            let html = document.create_element("html");
            let root = document.root();
            document.append_child(root, html);
            document.append_child(html, body);
            body
        };
        runtime
            .eval("var bubbled = false; document.addEventListener('click', function() { bubbled = true; });")
            .unwrap();
        assert!(dispatch_event(&mut runtime, &state, node, "click"));
        assert!(runtime.eval("bubbled").unwrap().is_truthy());
    }

    #[test]
    fn event_object_exposes_type_target_and_host_mouse_details() {
        let (mut runtime, _document, state) =
            setup_html("<html><body><div id='box'></div></body></html>");
        let node = state
            .borrow()
            .document
            .borrow()
            .get_element_by_id("box")
            .unwrap();
        runtime
            .eval(
                "var seen = {}; document.getElementById('box').addEventListener('mousemove', function(e) {\n\
                     seen.type = e.type; seen.x = e.clientX; seen.y = e.clientY;\n\
                     seen.targetId = e.target.id; seen.prevent = typeof e.preventDefault;\n\
                     seen.stop = typeof e.stopPropagation;\n\
                 });",
            )
            .unwrap();

        let outcome = dispatch_mouse_event(&mut runtime, &state, node, "mousemove", 12.5, 34.0);
        assert!(outcome.handled);
        assert!(!outcome.default_prevented);
        assert_eq!(
            runtime.eval("seen.type").unwrap().to_string_value(),
            "mousemove"
        );
        assert_eq!(number(&mut runtime, "seen.x"), 12.5);
        assert_eq!(number(&mut runtime, "seen.y"), 34.0);
        assert_eq!(
            runtime.eval("seen.targetId").unwrap().to_string_value(),
            "box"
        );
        assert_eq!(
            runtime.eval("seen.prevent").unwrap().to_string_value(),
            "function"
        );
        assert_eq!(
            runtime.eval("seen.stop").unwrap().to_string_value(),
            "function"
        );
    }

    #[test]
    fn keyboard_dispatch_carries_key_and_code() {
        let (mut runtime, _document, state) =
            setup_html("<html><body><input id='field'></body></html>");
        let node = state
            .borrow()
            .document
            .borrow()
            .get_element_by_id("field")
            .unwrap();
        runtime
            .eval(
                "var seen = {}; document.getElementById('field').addEventListener('keydown', function(e) {\n\
                     seen.key = e.key; seen.code = e.code;\n\
                 });",
            )
            .unwrap();

        dispatch_keyboard_event(&mut runtime, &state, node, "keydown", "a", "KeyA");
        assert_eq!(runtime.eval("seen.key").unwrap().to_string_value(), "a");
        assert_eq!(runtime.eval("seen.code").unwrap().to_string_value(), "KeyA");
    }

    #[test]
    fn prevent_default_is_observable_on_the_event() {
        let (mut runtime, _document, state) =
            setup_html("<html><body><div id='box'></div></body></html>");
        let node = state
            .borrow()
            .document
            .borrow()
            .get_element_by_id("box")
            .unwrap();
        runtime
            .eval(
                "var during = null; document.getElementById('box').addEventListener('submit', function(e) {\n\
                     e.preventDefault(); during = e.defaultPrevented;\n\
                 });",
            )
            .unwrap();

        let outcome = dispatch_event_with(&mut runtime, &state, node, EventInit::new("submit"));
        assert!(outcome.handled);
        assert!(outcome.default_prevented);
        assert!(runtime.eval("during").unwrap().is_truthy());
    }

    #[test]
    fn stop_propagation_stops_bubbling() {
        let (mut runtime, _document, state) =
            setup_html("<html><body><div id='box'></div></body></html>");
        let node = state
            .borrow()
            .document
            .borrow()
            .get_element_by_id("box")
            .unwrap();
        runtime
            .eval(
                "var child = 0, doc = 0;\n\
                 document.getElementById('box').addEventListener('click', function(e) {\n\
                     child++; e.stopPropagation();\n\
                 });\n\
                 document.addEventListener('click', function() { doc++; });",
            )
            .unwrap();

        dispatch_event(&mut runtime, &state, node, "click");
        assert_eq!(number(&mut runtime, "child"), 1.0);
        assert_eq!(number(&mut runtime, "doc"), 0.0);
    }

    #[test]
    fn stop_propagation_keeps_same_node_listeners() {
        let (mut runtime, _document, state) =
            setup_html("<html><body><div id='box'></div></body></html>");
        let node = state
            .borrow()
            .document
            .borrow()
            .get_element_by_id("box")
            .unwrap();
        runtime
            .eval(
                "var first = 0, second = 0, doc = 0;\n\
                 var el = document.getElementById('box');\n\
                 el.addEventListener('click', function(e) { first++; e.stopPropagation(); });\n\
                 el.addEventListener('click', function() { second++; });\n\
                 document.addEventListener('click', function() { doc++; });",
            )
            .unwrap();

        dispatch_event(&mut runtime, &state, node, "click");
        assert_eq!(number(&mut runtime, "first"), 1.0);
        assert_eq!(number(&mut runtime, "second"), 1.0);
        assert_eq!(number(&mut runtime, "doc"), 0.0);
    }

    #[test]
    fn script_event_constructors_dispatch_with_details() {
        let (mut runtime, _document, _state) =
            setup_html("<html><body><div id='box'></div></body></html>");
        runtime
            .eval(
                "var seen = {}; var el = document.getElementById('box');\n\
                 el.addEventListener('click', function(e) {\n\
                     seen.type = e.type; seen.x = e.clientX; seen.y = e.clientY;\n\
                     seen.targetId = e.target.id;\n\
                 });\n\
                 var evt = new MouseEvent('click', { clientX: 7, clientY: 9 });\n\
                 var handled = el.dispatchEvent(evt);\n\
                 seen.handled = handled;",
            )
            .unwrap();
        assert_eq!(
            runtime.eval("seen.type").unwrap().to_string_value(),
            "click"
        );
        assert_eq!(number(&mut runtime, "seen.x"), 7.0);
        assert_eq!(number(&mut runtime, "seen.y"), 9.0);
        assert_eq!(
            runtime.eval("seen.targetId").unwrap().to_string_value(),
            "box"
        );
        assert!(runtime.eval("seen.handled").unwrap().is_truthy());
    }

    #[test]
    fn keyboard_event_constructor_reaches_listeners() {
        let (mut runtime, _document, _state) =
            setup_html("<html><body><input id='field'></body></html>");
        runtime
            .eval(
                "var seen = {}; document.getElementById('field').addEventListener('keydown', function(e) {\n\
                     seen.key = e.key; seen.code = e.code;\n\
                 });\n\
                 document.getElementById('field').dispatchEvent(\n\
                     new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter' }));",
            )
            .unwrap();
        assert_eq!(runtime.eval("seen.key").unwrap().to_string_value(), "Enter");
        assert_eq!(
            runtime.eval("seen.code").unwrap().to_string_value(),
            "Enter"
        );
    }

    #[test]
    fn enter_in_single_text_input_requests_form_submission() {
        let (mut runtime, document, state) = setup_html(
            "<html><body><form id='f' action='/search'>\
             <input id='q' name='q'><button type='submit' name='go' value='1'>Go</button>\
             </form></body></html>",
        );
        let input = document.borrow().get_element_by_id("q").unwrap();
        let form = document.borrow().get_element_by_id("f").unwrap();
        runtime
            .eval("document.getElementById('q').focus();")
            .unwrap();
        assert_eq!(state.borrow().focused, Some(input));

        dispatch_keyboard_event(&mut runtime, &state, input, "keydown", "Enter", "Enter");
        let submission = state.borrow_mut().take_form_submission();
        assert_eq!(submission.map(|s| s.form), Some(form));
        assert!(submission.unwrap().submitter.is_some());

        // A cancelled keydown must not submit.
        runtime
            .eval(
                "document.getElementById('q').addEventListener('keydown', function(e) { e.preventDefault(); });",
            )
            .unwrap();
        dispatch_keyboard_event(&mut runtime, &state, input, "keydown", "Enter", "Enter");
        assert!(state.borrow().form_submission.is_none());
    }

    #[test]
    fn event_object_default_prevented_after_script_dispatch() {
        let (mut runtime, _document, _state) =
            setup_html("<html><body><div id='box'></div></body></html>");
        runtime
            .eval(
                "var el = document.getElementById('box');\n\
                 el.addEventListener('x', function(e) { e.preventDefault(); });\n\
                 var evt = new Event('x');\n\
                 el.dispatchEvent(evt);\n\
                 var prevented = evt.defaultPrevented;",
            )
            .unwrap();
        assert!(runtime.eval("prevented").unwrap().is_truthy());
    }
}
