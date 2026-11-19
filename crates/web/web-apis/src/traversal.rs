//! `NodeFilter`, `TreeWalker` and `NodeIterator` bindings.
//!
//! Third-party bootstraps (notably YouTube's webcomponents polyfill) feature
//! detect `NodeFilter` and call `document.createTreeWalker` /
//! `document.createNodeIterator` while scanning the DOM. The engine's DOM is a
//! flat arena with `first_child` / `next_sibling` / `parent` primitives, so the
//! walkers snapshot the subtree once per navigation and drive those primitives
//! through a `NodeFilter`-compatible decision pass.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use browser_dom::{Document, NodeData, NodeId};
use browser_js::{HostFunction, JsError, Runtime, Value};

use crate::{HostState, element};

/// Returned by a filter's `acceptNode` when the node should be visited.
pub const FILTER_ACCEPT: u32 = 1;
/// Returned by a filter's `acceptNode` when the node and its subtree are skipped.
pub const FILTER_REJECT: u32 = 2;
/// Returned by a filter's `acceptNode` when only the node itself is skipped.
pub const FILTER_SKIP: u32 = 3;

/// Show every node type.
pub const SHOW_ALL: u32 = u32::MAX;
pub const SHOW_ELEMENT: u32 = 0x1;
pub const SHOW_ATTRIBUTE: u32 = 0x2;
pub const SHOW_TEXT: u32 = 0x4;
pub const SHOW_CDATA_SECTION: u32 = 0x8;
pub const SHOW_PROCESSING_INSTRUCTION: u32 = 0x10;
pub const SHOW_COMMENT: u32 = 0x80;
pub const SHOW_DOCUMENT: u32 = 0x100;
pub const SHOW_DOCUMENT_TYPE: u32 = 0x200;
pub const SHOW_DOCUMENT_FRAGMENT: u32 = 0x400;
pub const SHOW_NOTATION: u32 = 0x800;

const FILTER_CONSTANTS: &[(&str, u32)] = &[
    ("FILTER_ACCEPT", FILTER_ACCEPT),
    ("FILTER_REJECT", FILTER_REJECT),
    ("FILTER_SKIP", FILTER_SKIP),
    ("SHOW_ALL", SHOW_ALL),
    ("SHOW_ELEMENT", SHOW_ELEMENT),
    ("SHOW_ATTRIBUTE", SHOW_ATTRIBUTE),
    ("SHOW_TEXT", SHOW_TEXT),
    ("SHOW_CDATA_SECTION", SHOW_CDATA_SECTION),
    ("SHOW_PROCESSING_INSTRUCTION", SHOW_PROCESSING_INSTRUCTION),
    ("SHOW_COMMENT", SHOW_COMMENT),
    ("SHOW_DOCUMENT", SHOW_DOCUMENT),
    ("SHOW_DOCUMENT_TYPE", SHOW_DOCUMENT_TYPE),
    ("SHOW_DOCUMENT_FRAGMENT", SHOW_DOCUMENT_FRAGMENT),
    ("SHOW_NOTATION", SHOW_NOTATION),
];

/// Installs the traversal globals and the corresponding `document` methods.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    install_node_filter(runtime);
    install_document_methods(runtime, state);
}

fn install_node_filter(runtime: &mut Runtime) {
    runtime.define_global_function(
        "NodeFilter",
        Box::new(|runtime, this, _args| {
            let global = runtime.global();
            match this {
                Value::Object(id) if Value::Object(id) != global => Ok(this),
                _ => Ok(runtime.new_object()),
            }
        }),
    );

    let constructor = runtime.get_global("NodeFilter");
    let prototype = runtime.get_property(&constructor, "prototype");
    runtime.define_host_method(
        prototype.clone(),
        "acceptNode",
        Box::new(|_runtime, _this, _args| Ok(Value::Number(FILTER_ACCEPT as f64))),
    );
    for &(name, value) in FILTER_CONSTANTS {
        let number = Value::Number(value as f64);
        runtime.set_property(constructor.clone(), name, number.clone());
        runtime.set_property(prototype.clone(), name, number);
    }
    // `window` aliases the global object, so `window.NodeFilter` resolves too.
}

struct WalkerState {
    root: NodeId,
    what_to_show: u32,
    filter: Option<Value>,
    current: NodeId,
}

struct IteratorState {
    root: NodeId,
    what_to_show: u32,
    filter: Option<Value>,
    current: Option<NodeId>,
}

fn node_type_mask(document: &Document, node: NodeId) -> u32 {
    match &document.node(node).data {
        NodeData::Element(_) => SHOW_ELEMENT,
        NodeData::Text(_) => SHOW_TEXT,
        NodeData::Comment(_) => SHOW_COMMENT,
        NodeData::Document => SHOW_DOCUMENT,
        NodeData::Doctype { .. } => SHOW_DOCUMENT_TYPE,
    }
}

struct SnapshotNode {
    id: NodeId,
    mask: u32,
    children: Vec<usize>,
}

fn snapshot_subtree(document: &Document, root: NodeId) -> Vec<SnapshotNode> {
    fn visit(document: &Document, id: NodeId, out: &mut Vec<SnapshotNode>) -> usize {
        let index = out.len();
        let mask = node_type_mask(document, id);
        let children: Vec<NodeId> = document.children(id).to_vec();
        out.push(SnapshotNode {
            id,
            mask,
            children: Vec::new(),
        });
        let indices: Vec<usize> = children
            .into_iter()
            .map(|child| visit(document, child, out))
            .collect();
        out[index].children = indices;
        index
    }

    let mut out = Vec::new();
    if document.contains(root) {
        visit(document, root, &mut out);
    }
    out
}

fn call_filter(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    filter: &Value,
    node: NodeId,
) -> Result<u32, JsError> {
    let accept = runtime.get_property(filter, "acceptNode");
    let wrapped = element::wrap(runtime, state, node);
    let result = if matches!(accept, Value::Object(_)) {
        runtime.call_function(&accept, filter.clone(), vec![wrapped])?
    } else {
        // Some polyfills pass a bare function instead of a NodeFilter object.
        match runtime.call_function(filter, Value::Undefined, vec![wrapped]) {
            Ok(result) => result,
            Err(_) => return Ok(FILTER_ACCEPT),
        }
    };
    Ok(result.to_number() as u32)
}

#[derive(Default)]
struct Traversal {
    /// Snapshot nodes in document (pre-)order.
    order: Vec<NodeId>,
    /// Nodes that passed both `whatToShow` and the filter.
    accepted: HashSet<NodeId>,
    parent: HashMap<NodeId, NodeId>,
    children: HashMap<NodeId, Vec<NodeId>>,
}

impl Traversal {
    fn position(&self, node: NodeId) -> Option<usize> {
        self.order.iter().position(|id| *id == node)
    }

    fn next_node(&self, current: NodeId) -> Option<NodeId> {
        let position = self.position(current)?;
        self.order[position + 1..]
            .iter()
            .copied()
            .find(|id| self.accepted.contains(id))
    }

    fn previous_node(&self, current: NodeId) -> Option<NodeId> {
        let position = self.position(current)?;
        self.order[..position]
            .iter()
            .rev()
            .copied()
            .find(|id| self.accepted.contains(id))
    }

    fn parent_node(&self, current: NodeId) -> Option<NodeId> {
        let mut candidate = self.parent.get(&current).copied();
        while let Some(id) = candidate {
            if self.accepted.contains(&id) {
                return Some(id);
            }
            candidate = self.parent.get(&id).copied();
        }
        None
    }

    fn descendants(&self, current: NodeId) -> Vec<NodeId> {
        fn visit(id: NodeId, traversal: &Traversal, out: &mut Vec<NodeId>) {
            if let Some(children) = traversal.children.get(&id) {
                for &child in children {
                    out.push(child);
                    visit(child, traversal, out);
                }
            }
        }
        let mut out = Vec::new();
        visit(current, self, &mut out);
        out
    }

    fn first_child(&self, current: NodeId) -> Option<NodeId> {
        self.descendants(current)
            .into_iter()
            .find(|id| self.accepted.contains(id))
    }

    fn last_child(&self, current: NodeId) -> Option<NodeId> {
        self.descendants(current)
            .into_iter()
            .rev()
            .find(|id| self.accepted.contains(id))
    }

    fn sibling(&self, current: NodeId, next: bool) -> Option<NodeId> {
        let parent = self.parent.get(&current)?;
        let siblings = self.children.get(parent)?;
        let position = siblings.iter().position(|id| *id == current)?;
        if next {
            siblings[position + 1..]
                .iter()
                .copied()
                .find(|id| self.accepted.contains(id))
        } else {
            siblings[..position]
                .iter()
                .rev()
                .copied()
                .find(|id| self.accepted.contains(id))
        }
    }

    fn first_accepted(&self) -> Option<NodeId> {
        self.order
            .iter()
            .copied()
            .find(|id| self.accepted.contains(id))
    }

    fn accepted_before(&self, position: usize) -> Option<NodeId> {
        self.order[..position]
            .iter()
            .rev()
            .copied()
            .find(|id| self.accepted.contains(id))
    }

    fn accepted_after(&self, position: usize) -> Option<NodeId> {
        self.order[position + 1..]
            .iter()
            .copied()
            .find(|id| self.accepted.contains(id))
    }
}

fn build_traversal(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    root: NodeId,
    what_to_show: u32,
    filter: &Option<Value>,
) -> Result<Traversal, JsError> {
    let document = state.borrow().document.clone();
    let snapshot = {
        let document = document.borrow();
        snapshot_subtree(&document, root)
    };

    let mut traversal = Traversal::default();
    for node in &snapshot {
        traversal.order.push(node.id);
        let children: Vec<NodeId> = node
            .children
            .iter()
            .map(|&index| snapshot[index].id)
            .collect();
        for child in &children {
            traversal.parent.insert(*child, node.id);
        }
        traversal.children.insert(node.id, children);
    }

    fn evaluate(
        index: usize,
        snapshot: &[SnapshotNode],
        runtime: &mut Runtime,
        state: &Rc<RefCell<HostState>>,
        what_to_show: u32,
        filter: &Option<Value>,
        traversal: &mut Traversal,
    ) -> Result<(), JsError> {
        let node = &snapshot[index];
        let decision = if node.mask & what_to_show == 0 {
            FILTER_SKIP
        } else if let Some(filter) = filter {
            call_filter(runtime, state, filter, node.id)?
        } else {
            FILTER_ACCEPT
        };
        if decision == FILTER_ACCEPT {
            traversal.accepted.insert(node.id);
        }
        // FILTER_REJECT prunes the subtree; FILTER_SKIP still visits children.
        if decision != FILTER_REJECT {
            for &child in &node.children {
                evaluate(
                    child,
                    snapshot,
                    runtime,
                    state,
                    what_to_show,
                    filter,
                    traversal,
                )?;
            }
        }
        Ok(())
    }

    if !snapshot.is_empty() {
        evaluate(
            0,
            &snapshot,
            runtime,
            state,
            what_to_show,
            filter,
            &mut traversal,
        )?;
    }
    Ok(traversal)
}

#[derive(Clone, Copy)]
enum TreeNav {
    Parent,
    FirstChild,
    LastChild,
    NextSibling,
    PreviousSibling,
    NextNode,
    PreviousNode,
}

fn tree_walker_method(
    state: &Rc<RefCell<HostState>>,
    walker: &Rc<RefCell<WalkerState>>,
    nav: TreeNav,
) -> HostFunction {
    let state = state.clone();
    let walker = walker.clone();
    Box::new(move |runtime, _this, _args| {
        let (root, what_to_show, filter, current) = {
            let walker = walker.borrow();
            (
                walker.root,
                walker.what_to_show,
                walker.filter.clone(),
                walker.current,
            )
        };
        let traversal = build_traversal(runtime, &state, root, what_to_show, &filter)?;
        let next = match nav {
            TreeNav::Parent => traversal.parent_node(current),
            TreeNav::FirstChild => traversal.first_child(current),
            TreeNav::LastChild => traversal.last_child(current),
            TreeNav::NextSibling => traversal.sibling(current, true),
            TreeNav::PreviousSibling => traversal.sibling(current, false),
            TreeNav::NextNode => traversal.next_node(current),
            TreeNav::PreviousNode => traversal.previous_node(current),
        };
        if let Some(next) = next {
            walker.borrow_mut().current = next;
        }
        Ok(match next {
            Some(node) => element::wrap(runtime, &state, node),
            None => Value::Null,
        })
    })
}

#[derive(Clone, Copy)]
enum IteratorNav {
    NextNode,
    PreviousNode,
}

fn node_iterator_method(
    state: &Rc<RefCell<HostState>>,
    iterator: &Rc<RefCell<IteratorState>>,
    nav: IteratorNav,
) -> HostFunction {
    let state = state.clone();
    let iterator = iterator.clone();
    Box::new(move |runtime, _this, _args| {
        let (root, what_to_show, filter, current) = {
            let iterator = iterator.borrow();
            (
                iterator.root,
                iterator.what_to_show,
                iterator.filter.clone(),
                iterator.current,
            )
        };
        let traversal = build_traversal(runtime, &state, root, what_to_show, &filter)?;
        let next = match nav {
            IteratorNav::NextNode => match current {
                None => traversal.first_accepted(),
                Some(current) => match traversal.position(current) {
                    Some(position) => traversal.accepted_after(position),
                    None => traversal.first_accepted(),
                },
            },
            IteratorNav::PreviousNode => match current {
                None => None,
                Some(current) => match traversal.position(current) {
                    Some(position) => traversal.accepted_before(position),
                    None => None,
                },
            },
        };
        if let Some(next) = next {
            iterator.borrow_mut().current = Some(next);
        }
        Ok(match next {
            Some(node) => element::wrap(runtime, &state, node),
            None => Value::Null,
        })
    })
}

fn install_document_methods(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let global = runtime.global();
    let document = runtime.get_property(&global, "document");

    runtime.define_host_method(document.clone(), "createTreeWalker", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let root = args
                .first()
                .ok_or_else(|| JsError::type_error("createTreeWalker needs a root node"))
                .and_then(|value| element::require_node(runtime, value))?;
            let what_to_show = match args.get(1) {
                None | Some(Value::Undefined) => SHOW_ALL,
                Some(value) => value.to_number() as u32,
            };
            let filter = args
                .get(2)
                .filter(|value| matches!(value, Value::Object(_)))
                .cloned();
            let expand = match args.get(3) {
                None | Some(Value::Undefined) => true,
                Some(value) => value.is_truthy(),
            };
            if let Some(filter) = &filter {
                // The filter lives in a Rust closure the JS collector cannot see.
                runtime.add_gc_root(filter.clone());
            }

            let inner = Rc::new(RefCell::new(WalkerState {
                root,
                what_to_show,
                filter: filter.clone(),
                current: root,
            }));
            let object = runtime.new_object();
            runtime.add_gc_root(object.clone());
            let root_wrapper = element::wrap(runtime, &state, root);
            runtime.set_property(object.clone(), "root", root_wrapper);
            runtime.set_property(
                object.clone(),
                "whatToShow",
                Value::Number(what_to_show as f64),
            );
            runtime.set_property(
                object.clone(),
                "filter",
                filter.clone().unwrap_or(Value::Null),
            );
            runtime.set_property(
                object.clone(),
                "expandEntityReferences",
                Value::Bool(expand),
            );

            runtime.define_accessor(
                object.clone(),
                "currentNode",
                {
                    let state = state.clone();
                    let inner = inner.clone();
                    Box::new(move |runtime, _this, _args| {
                        let current = inner.borrow().current;
                        Ok(element::wrap(runtime, &state, current))
                    })
                },
                {
                    let state = state.clone();
                    let inner = inner.clone();
                    Box::new(move |runtime, _this, args| {
                        let Some(node) = args
                            .first()
                            .and_then(|value| element::node_id_of(runtime, value))
                        else {
                            return Ok(Value::Undefined);
                        };
                        let document = state.borrow().document.clone();
                        if document.borrow().contains(node) {
                            inner.borrow_mut().current = node;
                        }
                        Ok(Value::Undefined)
                    })
                },
            );

            for (name, nav) in [
                ("parentNode", TreeNav::Parent),
                ("firstChild", TreeNav::FirstChild),
                ("lastChild", TreeNav::LastChild),
                ("nextSibling", TreeNav::NextSibling),
                ("previousSibling", TreeNav::PreviousSibling),
                ("nextNode", TreeNav::NextNode),
                ("previousNode", TreeNav::PreviousNode),
            ] {
                runtime.define_host_method(
                    object.clone(),
                    name,
                    tree_walker_method(&state, &inner, nav),
                );
            }
            Ok(object)
        })
    });

    runtime.define_host_method(document, "createNodeIterator", {
        let state = state.clone();
        Box::new(move |runtime, _this, args| {
            let root = args
                .first()
                .ok_or_else(|| JsError::type_error("createNodeIterator needs a root node"))
                .and_then(|value| element::require_node(runtime, value))?;
            let what_to_show = match args.get(1) {
                None | Some(Value::Undefined) => SHOW_ALL,
                Some(value) => value.to_number() as u32,
            };
            let filter = args
                .get(2)
                .filter(|value| matches!(value, Value::Object(_)))
                .cloned();
            if let Some(filter) = &filter {
                runtime.add_gc_root(filter.clone());
            }

            let inner = Rc::new(RefCell::new(IteratorState {
                root,
                what_to_show,
                filter: filter.clone(),
                current: None,
            }));
            let object = runtime.new_object();
            runtime.add_gc_root(object.clone());
            let root_wrapper = element::wrap(runtime, &state, root);
            runtime.set_property(object.clone(), "root", root_wrapper);
            runtime.set_property(
                object.clone(),
                "whatToShow",
                Value::Number(what_to_show as f64),
            );
            runtime.set_property(object.clone(), "filter", filter.unwrap_or(Value::Null));
            runtime.set_property(object.clone(), "expandEntityReferences", Value::Bool(true));

            runtime.define_host_method(
                object.clone(),
                "nextNode",
                node_iterator_method(&state, &inner, IteratorNav::NextNode),
            );
            runtime.define_host_method(
                object.clone(),
                "previousNode",
                node_iterator_method(&state, &inner, IteratorNav::PreviousNode),
            );
            runtime.define_host_method(
                object.clone(),
                "detach",
                Box::new(|_r, _t, _a| Ok(Value::Undefined)),
            );
            Ok(object)
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse_document;

    fn setup(html: &str) -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document(html)));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    fn eval(runtime: &mut Runtime, source: &str) -> Value {
        runtime.eval(source).unwrap()
    }

    fn is_true(runtime: &mut Runtime, source: &str) {
        assert_eq!(eval(runtime, source), Value::Bool(true), "{source}");
    }

    #[test]
    fn create_element_ns_delegates_to_create_element() {
        let (mut runtime, _state) = setup("<html><body></body></html>");
        runtime
            .eval(
                "const svg = 'http://www.w3.org/2000/svg';\n\
                 const el = document.createElementNS(svg, 'circle');\n\
                 el.setAttribute('r', '2');",
            )
            .unwrap();
        assert_eq!(
            eval(&mut runtime, "el instanceof Element"),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&mut runtime, "el.getAttribute('xmlns')"),
            Value::String("http://www.w3.org/2000/svg".into())
        );
        assert_eq!(
            eval(&mut runtime, "el.getAttribute('r')"),
            Value::String("2".into())
        );
        runtime
            .eval("const plain = document.createElementNS(null, 'div');")
            .unwrap();
        assert_eq!(
            eval(&mut runtime, "plain.getAttribute('xmlns')"),
            Value::String(String::new())
        );
    }

    #[test]
    fn node_filter_is_a_global_constructor_with_the_expected_constants() {
        let (mut runtime, _state) = setup("<html><body></body></html>");
        for expression in [
            "typeof NodeFilter === 'function'",
            "typeof window.NodeFilter === 'function'",
            "window.NodeFilter === NodeFilter",
            "typeof new NodeFilter() === 'object'",
            "typeof NodeFilter.prototype.acceptNode === 'function'",
            "typeof NodeFilter.prototype.FILTER_ACCEPT === 'number'",
        ] {
            is_true(&mut runtime, expression);
        }
        for (name, value) in FILTER_CONSTANTS {
            assert_eq!(
                eval(&mut runtime, &format!("NodeFilter.{name}")),
                Value::Number(*value as f64),
                "{name} on constructor"
            );
            assert_eq!(
                eval(&mut runtime, &format!("NodeFilter.prototype.{name}")),
                Value::Number(*value as f64),
                "{name} on prototype"
            );
        }
        assert_eq!(
            eval(&mut runtime, "new NodeFilter().acceptNode(document.body)"),
            Value::Number(FILTER_ACCEPT as f64)
        );
    }

    #[test]
    fn tree_walker_traverses_with_next_previous_parent_and_children() {
        let (mut runtime, _state) = setup(
            "<html><body><div id='root'>head<span id='a'>one</span>tail<span id='b'>two</span></div></body></html>",
        );
        runtime
            .eval(
                "var root = document.getElementById('root');\n\
                 var walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);\n\
                 var seen = [];\n\
                 for (var node = walker.currentNode; node; node = walker.nextNode()) seen.push(node.id || node.tagName);\n\
                 var visited = seen.join(',');",
            )
            .unwrap();
        // Elements only: the root, then its two <span> children.
        assert_eq!(
            eval(&mut runtime, "visited"),
            Value::String("root,a,b".into())
        );

        runtime
            .eval(
                "walker.currentNode = root;\n\
                 var first = walker.firstChild();\n\
                 var lastIsB = (walker.currentNode = root, walker.lastChild().id === 'b');\n\
                 var parentIsRoot = (walker.currentNode = document.getElementById('a'), walker.parentNode().id === 'root');\n\
                 var siblingIsB = (walker.currentNode = document.getElementById('a'), walker.nextSibling().id === 'b');\n\
                 var previous = (walker.currentNode = document.getElementById('b'), walker.previousNode().id === 'a');\n\
                 var above = (walker.currentNode = document.getElementById('a'), walker.parentNode().id === 'root');",
            )
            .unwrap();
        assert_eq!(eval(&mut runtime, "first.id"), Value::String("a".into()));
        for expression in ["lastIsB", "parentIsRoot", "siblingIsB", "previous", "above"] {
            is_true(&mut runtime, expression);
        }
    }

    #[test]
    fn tree_walker_honours_a_node_filter() {
        let (mut runtime, _state) = setup(
            "<html><body><div id='root'><p class='keep'></p><p class='skip'></p><p class='keep2'></p></div></body></html>",
        );
        runtime
            .eval(
                "var root = document.getElementById('root');\n\
                 var filter = { acceptNode: function (node) {\n\
                     return (node.getAttribute('class') === 'skip') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT;\n\
                 } };\n\
                 var walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, filter);\n\
                 var seen = [];\n\
                 for (var node = walker.currentNode; node; node = walker.nextNode()) seen.push(node.getAttribute('class') || 'root');\n\
                 var visited = seen.join(',');",
            )
            .unwrap();
        assert_eq!(
            eval(&mut runtime, "visited"),
            Value::String("root,keep,keep2".into())
        );
    }

    #[test]
    fn tree_walker_accepts_a_bare_function_filter() {
        let (mut runtime, _state) = setup(
            "<html><body><div id='root'><span id='a'></span><span id='b'></span></div></body></html>",
        );
        runtime
            .eval(
                "var root = document.getElementById('root');\n\
                 var fn = function (node) { return node.id === 'b' ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT; };\n\
                 var walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, fn);\n\
                 var seen = [];\n\
                 for (var node = walker.currentNode; node; node = walker.nextNode()) seen.push(node.id || 'root');\n\
                 var visited = seen.join(',');",
            )
            .unwrap();
        assert_eq!(
            eval(&mut runtime, "visited"),
            Value::String("root,a".into())
        );
    }

    #[test]
    fn node_iterator_walks_in_both_directions() {
        let (mut runtime, _state) = setup(
            "<html><body><div id='root'>a<span id='one'>1</span>b<span id='two'>2</span>c</div></body></html>",
        );
        runtime
            .eval(
                "var root = document.getElementById('root');\n\
                 var walker = document.createTreeWalker(root, NodeFilter.SHOW_ALL);\n\
                 var all = [];\n\
                 for (var n = walker.currentNode; n; n = walker.nextNode()) all.push(n.nodeType + ':' + (n.id || (n.textContent || '').trim()));\n\
                 var allJoined = all.join(',');\n\
                 var it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT);\n\
                 var beforeStart = it.previousNode();\n\
                 var first = it.nextNode();\n\
                 var second = it.nextNode();\n\
                 var back = it.previousNode();\n\
                 var result = first.id + ',' + second.id + ',' + back.id;",
            )
            .unwrap();
        // Per the DOM spec the root belongs to a NodeIterator's node set, so
        // the first nextNode() returns it and previousNode() starts at null.
        assert_eq!(
            eval(&mut runtime, "result"),
            Value::String("root,one,root".into())
        );
        assert_eq!(eval(&mut runtime, "beforeStart"), Value::Null);
        assert!(
            eval(&mut runtime, "allJoined")
                .to_string_value()
                .contains("3:a")
        );
        assert_eq!(
            eval(
                &mut runtime,
                "typeof it.nextNode === 'function' && typeof it.previousNode === 'function'"
            ),
            Value::Bool(true)
        );
    }
}
