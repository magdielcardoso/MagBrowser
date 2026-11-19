//! Observer APIs used by modern sites: `IntersectionObserver`, `ResizeObserver`
//! and `MutationObserver`.
//!
//! Observers are registered from JavaScript and stored on [`crate::HostState`]
//! in the [`Observers`] field. The embedder drives them in two phases:
//!
//! 1. After every layout/scroll pass it installs (once) a layout-box resolver
//!    with [`Observers::set_layout_box_resolver`] and calls [`notify_observers`].
//!    That function compares the new geometry (and the mutation queue) against
//!    each observer's previous state and *queues* deliveries.
//! 2. It calls [`deliver_observer_callbacks`] with a mutable reference to the
//!    JavaScript runtime, which builds the `*ObserverEntry` / `MutationRecord`
//!    objects and invokes the registered callbacks.
//!
//! Splitting the hook this way keeps [`notify_observers`] usable from the
//! browser-core layout code, which owns the document but borrows the runtime
//! mutably only while running scripts. It mirrors the existing
//! `drain_timers`/`dispatch_event` pattern used elsewhere in this crate.
//!
//! DOM mutations performed through this crate's bindings are recorded
//! automatically. A DOM implementation that mutates nodes directly can call
//! the public [`record_mutation`] function at the documented integration point.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use browser_dom::{Document, NodeId};
use browser_js::{JsError, Runtime, Value};

use crate::{HostState, element};

/// An axis-aligned rectangle in CSS pixels. Geometry mirrors
/// `browser_layout::Rect`, but lives here so this crate needs no layout
/// dependency; browser-core converts a layout box with
/// `Rect::new(box.x, box.y, box.width, box.height)` (or by reading the public
/// fields).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Rect {
    /// Creates a rectangle.
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// An empty rectangle at the origin.
    pub const ZERO: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    /// Right edge.
    pub fn right(self) -> f64 {
        self.x + self.width
    }

    /// Bottom edge.
    pub fn bottom(self) -> f64 {
        self.y + self.height
    }

    /// Whether the rectangle has no positive area.
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// The overlap with `other`, clamped so it never has negative extent.
    pub fn intersect(self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
    }
}

/// Resolves a node to its layout box, supplied by the embedder (browser-core)
/// once a layout tree exists. Returning `None` means "no box" (display:none,
/// detached, or not laid out yet).
pub type LayoutBoxResolver = Box<dyn Fn(NodeId) -> Option<Rect>>;

/// The kind of a DOM mutation record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationKind {
    /// Children were added or removed.
    ChildList,
    /// An attribute was added, changed or removed.
    Attributes,
    /// A text node's data changed.
    CharacterData,
}

impl MutationKind {
    /// The JavaScript `MutationRecord.type` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChildList => "childList",
            Self::Attributes => "attributes",
            Self::CharacterData => "characterData",
        }
    }
}

/// A DOM mutation, recorded by this crate's bindings or by the DOM crate via
/// [`record_mutation`].
#[derive(Clone, Debug)]
pub struct Mutation {
    /// What changed.
    pub kind: MutationKind,
    /// The node on which the change happened (`target`).
    pub target: NodeId,
    /// Nodes inserted as children.
    pub added_nodes: Vec<NodeId>,
    /// Nodes removed from children.
    pub removed_nodes: Vec<NodeId>,
    /// Previous sibling of an inserted/removed node.
    pub previous_sibling: Option<NodeId>,
    /// Next sibling of an inserted/removed node.
    pub next_sibling: Option<NodeId>,
    /// Changed attribute's name, for `attributes` records.
    pub attribute_name: Option<String>,
    /// Previous value, for `attributes` / `characterData` records.
    pub old_value: Option<String>,
}

impl Mutation {
    /// Builds a `childList` mutation.
    pub fn child_list(
        target: NodeId,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        previous_sibling: Option<NodeId>,
        next_sibling: Option<NodeId>,
    ) -> Self {
        Self {
            kind: MutationKind::ChildList,
            target,
            added_nodes: added,
            removed_nodes: removed,
            previous_sibling,
            next_sibling,
            attribute_name: None,
            old_value: None,
        }
    }

    /// Builds an `attributes` mutation.
    pub fn attributes(target: NodeId, name: impl Into<String>, old_value: Option<String>) -> Self {
        Self {
            kind: MutationKind::Attributes,
            target,
            added_nodes: Vec::new(),
            removed_nodes: Vec::new(),
            previous_sibling: None,
            next_sibling: None,
            attribute_name: Some(name.into()),
            old_value,
        }
    }

    /// Builds a `characterData` mutation.
    pub fn character_data(target: NodeId, old_value: Option<String>) -> Self {
        Self {
            kind: MutationKind::CharacterData,
            target,
            added_nodes: Vec::new(),
            removed_nodes: Vec::new(),
            previous_sibling: None,
            next_sibling: None,
            attribute_name: None,
            old_value,
        }
    }
}

/// Parsed `MutationObserver.observe` options.
#[derive(Clone, Default)]
struct MutationOptions {
    child_list: bool,
    attributes: bool,
    character_data: bool,
    subtree: bool,
    attribute_old_value: bool,
    character_data_old_value: bool,
    attribute_filter: Option<Vec<String>>,
}

impl MutationOptions {
    fn from_value(runtime: &mut Runtime, value: &Value) -> Self {
        let mut options = Self::default();
        if !matches!(value, Value::Object(_)) {
            return options;
        }
        options.child_list = read_bool(runtime, value, "childList", false);
        options.attributes = read_bool(runtime, value, "attributes", false);
        options.character_data = read_bool(runtime, value, "characterData", false);
        options.subtree = read_bool(runtime, value, "subtree", false);
        options.attribute_old_value = read_bool(runtime, value, "attributeOldValue", false);
        options.character_data_old_value =
            read_bool(runtime, value, "characterDataOldValue", false);
        let filter = runtime.get_property(value, "attributeFilter");
        if let Value::Object(_) = filter {
            let mut names = Vec::new();
            if let Value::Number(length) = runtime.get_property(&filter, "length") {
                for index in 0..length as usize {
                    names.push(
                        runtime
                            .get_property(&filter, &index.to_string())
                            .to_string_value(),
                    );
                }
            }
            options.attribute_filter = Some(names);
        }
        // Per spec these flags imply their observation type.
        if options.attribute_old_value || options.attribute_filter.is_some() {
            options.attributes = true;
        }
        if options.character_data_old_value {
            options.character_data = true;
        }
        options
    }

    fn accepts(&self, mutation: &Mutation) -> bool {
        match mutation.kind {
            MutationKind::ChildList => self.child_list,
            MutationKind::Attributes => {
                self.attributes
                    && self.attribute_filter.as_ref().is_none_or(|filter| {
                        mutation
                            .attribute_name
                            .as_deref()
                            .is_some_and(|name| filter.iter().any(|item| item == name))
                    })
            }
            MutationKind::CharacterData => self.character_data,
        }
    }

    fn wants_old_value(&self, mutation: &Mutation) -> bool {
        match mutation.kind {
            MutationKind::Attributes => self.attribute_old_value,
            MutationKind::CharacterData => self.character_data_old_value,
            MutationKind::ChildList => false,
        }
    }
}

#[derive(Clone)]
struct IntersectionState {
    id: u64,
    callback: Value,
    wrapper: Value,
    root: Option<NodeId>,
    root_margin: String,
    thresholds: Vec<f64>,
    observed: Vec<NodeId>,
    last_ratio: HashMap<NodeId, f64>,
}

#[derive(Clone)]
struct ResizeState {
    id: u64,
    callback: Value,
    wrapper: Value,
    observed: Vec<NodeId>,
    last_size: HashMap<NodeId, (f64, f64)>,
}

#[derive(Clone)]
struct MutationState {
    id: u64,
    callback: Value,
    wrapper: Value,
    targets: Vec<(NodeId, MutationOptions)>,
    queue: Vec<Mutation>,
}

#[derive(Clone)]
struct IntersectionEntryData {
    target: NodeId,
    is_intersecting: bool,
    intersection_ratio: f64,
    bounding_client_rect: Rect,
    intersection_rect: Rect,
}

#[derive(Clone)]
struct ResizeEntryData {
    target: NodeId,
    content_rect: Rect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeliveryKind {
    Intersection,
    Resize,
    Mutation,
}

struct PendingDelivery {
    id: u64,
    kind: DeliveryKind,
    intersections: Vec<IntersectionEntryData>,
    resizes: Vec<ResizeEntryData>,
}

impl PendingDelivery {
    fn intersection(id: u64, entries: Vec<IntersectionEntryData>) -> Self {
        Self {
            id,
            kind: DeliveryKind::Intersection,
            intersections: entries,
            resizes: Vec::new(),
        }
    }

    fn resize(id: u64, entries: Vec<ResizeEntryData>) -> Self {
        Self {
            id,
            kind: DeliveryKind::Resize,
            intersections: Vec::new(),
            resizes: entries,
        }
    }

    fn mutation(id: u64) -> Self {
        Self {
            id,
            kind: DeliveryKind::Mutation,
            intersections: Vec::new(),
            resizes: Vec::new(),
        }
    }
}

/// All observer state shared with the runtime. Stored on
/// [`crate::HostState::observers`].
#[derive(Default)]
pub struct Observers {
    /// Layout box resolver installed by browser-core.
    pub layout_boxes: Option<LayoutBoxResolver>,
    intersections: Vec<IntersectionState>,
    resizes: Vec<ResizeState>,
    mutations: Vec<MutationState>,
    pending: Vec<PendingDelivery>,
    next_id: u64,
}

impl Observers {
    /// Creates empty observer state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Installs the callback that maps a node to its current layout box.
    pub fn set_layout_box_resolver(&mut self, resolver: impl Fn(NodeId) -> Option<Rect> + 'static) {
        self.layout_boxes = Some(Box::new(resolver));
    }

    /// Removes all registered observers and queued notifications.
    pub fn clear(&mut self) {
        self.intersections.clear();
        self.resizes.clear();
        self.mutations.clear();
        self.pending.clear();
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

fn read_bool(runtime: &mut Runtime, object: &Value, key: &str, default: bool) -> bool {
    match runtime.get_property(object, key) {
        Value::Bool(value) => value,
        Value::Undefined | Value::Null => default,
        other => other.is_truthy(),
    }
}

fn read_string(runtime: &mut Runtime, object: &Value, key: &str, default: &str) -> String {
    match runtime.get_property(object, key) {
        Value::Undefined | Value::Null => default.to_owned(),
        other => other.to_string_value(),
    }
}

fn is_callable(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// Margin edges after percentage resolution, in CSS pixels. Positive values
/// expand the root box, negative values shrink it.
#[derive(Clone, Copy, Default)]
struct Edges {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

/// Parses the four-value `rootMargin` CSS shorthand. Unitless and `px` values
/// are pixels; `%` values resolve against the root box's width/height.
fn parse_margin(source: &str, root_width: f64, root_height: f64) -> Edges {
    let values: Vec<(f64, bool)> = source.split_whitespace().filter_map(parse_length).collect();
    let pick = |index: usize, fallback: usize, horizontal: bool| -> f64 {
        let Some(&(value, percent)) = values.get(index).or_else(|| values.get(fallback)) else {
            return 0.0;
        };
        if percent {
            value * if horizontal { root_width } else { root_height } / 100.0
        } else {
            value
        }
    };
    match values.len() {
        0 => Edges::default(),
        1 => {
            let value = pick(0, 0, true);
            Edges {
                top: value,
                right: value,
                bottom: value,
                left: value,
            }
        }
        2 => {
            let vertical = pick(0, 0, false);
            let horizontal = pick(1, 1, true);
            Edges {
                top: vertical,
                right: horizontal,
                bottom: vertical,
                left: horizontal,
            }
        }
        3 => Edges {
            top: pick(0, 0, false),
            right: pick(1, 1, true),
            bottom: pick(2, 2, false),
            left: pick(1, 1, true),
        },
        _ => Edges {
            top: pick(0, 0, false),
            right: pick(1, 1, true),
            bottom: pick(2, 2, false),
            left: pick(3, 3, true),
        },
    }
}

/// Returns `(number, is_percent)`.
fn parse_length(source: &str) -> Option<(f64, bool)> {
    let trimmed = source.trim();
    if let Some(number) = trimmed.strip_suffix('%') {
        return number.parse::<f64>().ok().map(|value| (value, true));
    }
    if let Some(number) = trimmed.strip_suffix("px") {
        return number.parse::<f64>().ok().map(|value| (value, false));
    }
    trimmed.parse::<f64>().ok().map(|value| (value, false))
}

fn expand(rect: Rect, margin: Edges) -> Rect {
    Rect::new(
        rect.x - margin.left,
        rect.y - margin.top,
        rect.width + margin.left + margin.right,
        rect.height + margin.top + margin.bottom,
    )
}

fn parse_thresholds(runtime: &mut Runtime, value: &Value) -> Vec<f64> {
    let mut raw = Vec::new();
    match value {
        Value::Object(_) => {
            if let Value::Number(length) = runtime.get_property(value, "length") {
                for index in 0..length as usize {
                    raw.push(runtime.get_property(value, &index.to_string()).to_number());
                }
            }
        }
        Value::Undefined | Value::Null => raw.push(0.0),
        other => raw.push(other.to_number()),
    }
    if raw.is_empty() {
        raw.push(0.0);
    }
    let mut thresholds: Vec<f64> = raw.into_iter().map(|value| value.clamp(0.0, 1.0)).collect();
    thresholds.sort_by(f64::total_cmp);
    thresholds.dedup_by(|a, b| (*a - *b).abs() < f64::EPSILON);
    thresholds
}

fn rect_object(runtime: &mut Runtime, rect: Rect) -> Value {
    let object = runtime.new_object();
    runtime.set_property(object.clone(), "x", Value::Number(rect.x));
    runtime.set_property(object.clone(), "y", Value::Number(rect.y));
    runtime.set_property(object.clone(), "width", Value::Number(rect.width));
    runtime.set_property(object.clone(), "height", Value::Number(rect.height));
    runtime.set_property(object.clone(), "top", Value::Number(rect.y));
    runtime.set_property(object.clone(), "left", Value::Number(rect.x));
    runtime.set_property(object.clone(), "right", Value::Number(rect.right()));
    runtime.set_property(object.clone(), "bottom", Value::Number(rect.bottom()));
    object
}

/// Installs the observer globals on `runtime`.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    define_intersection_observer(runtime, state);
    define_resize_observer(runtime, state);
    define_mutation_observer(runtime, state);
    define_entry_constructors(runtime);
}

fn install_observer_methods(
    runtime: &mut Runtime,
    object: Value,
    state: &Rc<RefCell<HostState>>,
    kind: &'static str,
) {
    runtime.set_property(
        object.clone(),
        "__observerKind",
        Value::String(kind.to_owned()),
    );
    runtime.define_host_method(object.clone(), "disconnect", {
        let state = state.clone();
        Box::new(move |runtime, this, _args| {
            let id = observer_id(runtime, &this);
            let mut state = state.borrow_mut();
            match kind {
                "intersection" => {
                    if let Some(observer) = state
                        .observers
                        .intersections
                        .iter_mut()
                        .find(|observer| observer.id == id)
                    {
                        observer.observed.clear();
                        observer.last_ratio.clear();
                    }
                }
                "resize" => {
                    if let Some(observer) = state
                        .observers
                        .resizes
                        .iter_mut()
                        .find(|observer| observer.id == id)
                    {
                        observer.observed.clear();
                        observer.last_size.clear();
                    }
                }
                "mutation" => {
                    if let Some(observer) = state
                        .observers
                        .mutations
                        .iter_mut()
                        .find(|observer| observer.id == id)
                    {
                        observer.targets.clear();
                        observer.queue.clear();
                    }
                }
                _ => {}
            }
            state.observers.pending.retain(|delivery| delivery.id != id);
            Ok(Value::Undefined)
        })
    });
}

fn observer_id(runtime: &mut Runtime, value: &Value) -> u64 {
    match runtime.get_property(value, "__observerId") {
        Value::Number(number) if number >= 0.0 => number as u64,
        _ => 0,
    }
}

fn this_or_new(runtime: &mut Runtime, this: Value) -> Value {
    match this {
        Value::Object(_) => this,
        _ => runtime.new_object(),
    }
}

fn define_intersection_observer(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function("IntersectionObserver", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let callback = args
                .first()
                .filter(|value| is_callable(value))
                .cloned()
                .ok_or_else(|| JsError::type_error("IntersectionObserver needs a callback"))?;
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let root_value = runtime.get_property(&options, "root");
            let root = element::node_id_of(runtime, &root_value);
            let root_margin = read_string(runtime, &options, "rootMargin", "0px");
            let threshold_value = runtime.get_property(&options, "threshold");
            let thresholds = parse_thresholds(runtime, &threshold_value);

            let object = this_or_new(runtime, this);
            let id = state.borrow_mut().observers.next_id();
            runtime.set_property(object.clone(), "__observerId", Value::Number(id as f64));
            runtime.set_property(object.clone(), "root", root_value);
            runtime.set_property(
                object.clone(),
                "rootMargin",
                Value::String(root_margin.clone()),
            );
            let threshold_values = thresholds
                .iter()
                .map(|value| Value::Number(*value))
                .collect();
            let thresholds_array = runtime.new_array(threshold_values);
            runtime.set_property(object.clone(), "thresholds", thresholds_array);

            runtime.define_host_method(object.clone(), "observe", {
                let state = state.clone();
                Box::new(move |runtime, this, args| {
                    let id = observer_id(runtime, &this);
                    let Ok(node) = args
                        .first()
                        .ok_or_else(|| JsError::type_error("observe needs a target"))
                        .and_then(|value| element::require_node(runtime, value))
                    else {
                        return Ok(Value::Undefined);
                    };
                    let mut state = state.borrow_mut();
                    if let Some(observer) = state
                        .observers
                        .intersections
                        .iter_mut()
                        .find(|observer| observer.id == id)
                        && !observer.observed.contains(&node)
                    {
                        observer.observed.push(node);
                    }
                    Ok(Value::Undefined)
                })
            });
            runtime.define_host_method(object.clone(), "unobserve", {
                let state = state.clone();
                Box::new(move |runtime, this, args| {
                    let id = observer_id(runtime, &this);
                    let Ok(node) = args
                        .first()
                        .ok_or_else(|| JsError::type_error("unobserve needs a target"))
                        .and_then(|value| element::require_node(runtime, value))
                    else {
                        return Ok(Value::Undefined);
                    };
                    let mut state = state.borrow_mut();
                    if let Some(observer) = state
                        .observers
                        .intersections
                        .iter_mut()
                        .find(|observer| observer.id == id)
                    {
                        observer.observed.retain(|observed| *observed != node);
                        observer.last_ratio.remove(&node);
                    }
                    Ok(Value::Undefined)
                })
            });
            install_observer_methods(runtime, object.clone(), &state, "intersection");

            let wrapper = object.clone();
            state
                .borrow_mut()
                .observers
                .intersections
                .push(IntersectionState {
                    id,
                    callback,
                    wrapper,
                    root,
                    root_margin,
                    thresholds,
                    observed: Vec::new(),
                    last_ratio: HashMap::new(),
                });
            Ok(object)
        })
    });
}

fn define_resize_observer(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function("ResizeObserver", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let callback = args
                .first()
                .filter(|value| is_callable(value))
                .cloned()
                .ok_or_else(|| JsError::type_error("ResizeObserver needs a callback"))?;
            let object = this_or_new(runtime, this);
            let id = state.borrow_mut().observers.next_id();
            runtime.set_property(object.clone(), "__observerId", Value::Number(id as f64));

            runtime.define_host_method(object.clone(), "observe", {
                let state = state.clone();
                Box::new(move |runtime, this, args| {
                    let id = observer_id(runtime, &this);
                    let Ok(node) = args
                        .first()
                        .ok_or_else(|| JsError::type_error("observe needs a target"))
                        .and_then(|value| element::require_node(runtime, value))
                    else {
                        return Ok(Value::Undefined);
                    };
                    let mut state = state.borrow_mut();
                    if let Some(observer) = state
                        .observers
                        .resizes
                        .iter_mut()
                        .find(|observer| observer.id == id)
                        && !observer.observed.contains(&node)
                    {
                        observer.observed.push(node);
                    }
                    Ok(Value::Undefined)
                })
            });
            runtime.define_host_method(object.clone(), "unobserve", {
                let state = state.clone();
                Box::new(move |runtime, this, args| {
                    let id = observer_id(runtime, &this);
                    let Ok(node) = args
                        .first()
                        .ok_or_else(|| JsError::type_error("unobserve needs a target"))
                        .and_then(|value| element::require_node(runtime, value))
                    else {
                        return Ok(Value::Undefined);
                    };
                    let mut state = state.borrow_mut();
                    if let Some(observer) = state
                        .observers
                        .resizes
                        .iter_mut()
                        .find(|observer| observer.id == id)
                    {
                        observer.observed.retain(|observed| *observed != node);
                        observer.last_size.remove(&node);
                    }
                    Ok(Value::Undefined)
                })
            });
            install_observer_methods(runtime, object.clone(), &state, "resize");

            let wrapper = object.clone();
            state.borrow_mut().observers.resizes.push(ResizeState {
                id,
                callback,
                wrapper,
                observed: Vec::new(),
                last_size: HashMap::new(),
            });
            Ok(object)
        })
    });
}

fn define_mutation_observer(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function("MutationObserver", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let callback = args
                .first()
                .filter(|value| is_callable(value))
                .cloned()
                .ok_or_else(|| JsError::type_error("MutationObserver needs a callback"))?;
            let object = this_or_new(runtime, this);
            let id = state.borrow_mut().observers.next_id();
            runtime.set_property(object.clone(), "__observerId", Value::Number(id as f64));

            runtime.define_host_method(object.clone(), "observe", {
                let state = state.clone();
                Box::new(move |runtime, this, args| {
                    let id = observer_id(runtime, &this);
                    let Ok(node) = args
                        .first()
                        .ok_or_else(|| JsError::type_error("observe needs a target"))
                        .and_then(|value| element::require_node(runtime, value))
                    else {
                        return Ok(Value::Undefined);
                    };
                    let options = MutationOptions::from_value(
                        runtime,
                        &args.get(1).cloned().unwrap_or(Value::Undefined),
                    );
                    let mut state = state.borrow_mut();
                    if let Some(observer) = state
                        .observers
                        .mutations
                        .iter_mut()
                        .find(|observer| observer.id == id)
                    {
                        observer.targets.retain(|(target, _)| *target != node);
                        observer.targets.push((node, options));
                    }
                    Ok(Value::Undefined)
                })
            });
            runtime.define_host_method(object.clone(), "takeRecords", {
                let state = state.clone();
                Box::new(move |runtime, this, _args| {
                    let id = observer_id(runtime, &this);
                    let records = {
                        let mut state = state.borrow_mut();
                        let taken = state
                            .observers
                            .mutations
                            .iter_mut()
                            .find(|observer| observer.id == id)
                            .map(|observer| std::mem::take(&mut observer.queue));
                        let Some(records) = taken else {
                            return Ok(runtime.new_array(Vec::new()));
                        };
                        state.observers.pending.retain(|delivery| {
                            !(delivery.id == id && delivery.kind == DeliveryKind::Mutation)
                        });
                        records
                    };
                    let items = records
                        .into_iter()
                        .map(|record| build_mutation_record(runtime, &state, record))
                        .collect();
                    Ok(runtime.new_array(items))
                })
            });
            install_observer_methods(runtime, object.clone(), &state, "mutation");

            let wrapper = object.clone();
            state.borrow_mut().observers.mutations.push(MutationState {
                id,
                callback,
                wrapper,
                targets: Vec::new(),
                queue: Vec::new(),
            });
            Ok(object)
        })
    });
}

fn define_entry_constructors(runtime: &mut Runtime) {
    runtime.define_global_function(
        "IntersectionObserverEntry",
        Box::new(move |runtime, this, args| {
            let object = this_or_new(runtime, this);
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            let time = runtime.get_property(&init, "time").to_number();
            runtime.set_property(object.clone(), "time", Value::Number(time));
            for key in [
                "target",
                "rootBounds",
                "boundingClientRect",
                "intersectionRect",
            ] {
                let value = runtime.get_property(&init, key);
                runtime.set_property(object.clone(), key, value);
            }
            let ratio = runtime.get_property(&init, "intersectionRatio").to_number();
            let intersecting = runtime.get_property(&init, "isIntersecting").is_truthy();
            runtime.set_property(object.clone(), "intersectionRatio", Value::Number(ratio));
            runtime.set_property(object.clone(), "isIntersecting", Value::Bool(intersecting));
            Ok(object)
        }),
    );

    runtime.define_global_function(
        "ResizeObserverEntry",
        Box::new(move |runtime, this, args| {
            let object = this_or_new(runtime, this);
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            for key in ["target", "contentRect", "borderBoxSize", "contentBoxSize"] {
                let value = runtime.get_property(&init, key);
                runtime.set_property(object.clone(), key, value);
            }
            Ok(object)
        }),
    );

    runtime.define_global_function(
        "MutationRecord",
        Box::new(move |runtime, this, args| {
            let object = this_or_new(runtime, this);
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            for key in [
                "type",
                "target",
                "addedNodes",
                "removedNodes",
                "previousSibling",
                "nextSibling",
                "attributeName",
                "oldValue",
            ] {
                let value = runtime.get_property(&init, key);
                runtime.set_property(object.clone(), key, value);
            }
            Ok(object)
        }),
    );
}

/// Records a DOM mutation so matching `MutationObserver`s can deliver it.
///
/// The crate's own DOM bindings call this automatically. A DOM crate that
/// mutates nodes directly can call it with the host handle at each mutation
/// site; it is safe to call while a `Document` borrow is active because the
/// host state and document are separate cells.
pub fn record_mutation(host: &Rc<RefCell<HostState>>, mutation: Mutation) {
    let mut state = host.borrow_mut();
    let document = state.document.clone();
    let document = document.borrow();
    let mut pending = Vec::new();
    for observer in state.observers.mutations.iter_mut() {
        let matches_target = observer.targets.iter().any(|(target, options)| {
            if !options.accepts(&mutation) {
                return false;
            }
            *target == mutation.target
                || (options.subtree && document.is_ancestor_of(*target, mutation.target))
        });
        if !matches_target {
            continue;
        }
        let was_empty = observer.queue.is_empty();
        observer.queue.push(mutation.clone());
        if was_empty {
            pending.push(PendingDelivery::mutation(observer.id));
        }
        // Keep oldValue only when the observing options requested it.
        if !observer
            .targets
            .iter()
            .any(|(_, options)| options.wants_old_value(&mutation))
            && let Some(record) = observer.queue.last_mut()
        {
            record.old_value = None;
        }
    }
    drop(document);
    state.observers.pending.extend(pending);
}

/// Recomputes intersection and resize observations after a layout/scroll pass.
///
/// `viewport` is the visible area in CSS pixels. `doc` is supplied so the hook
/// can resolve node ancestry; pass the same document used by `host`.
pub fn notify_observers(host: &Rc<RefCell<HostState>>, viewport: Rect, doc: &Document) {
    let mut state = host.borrow_mut();
    let mut nodes = HashSet::new();
    for observer in &state.observers.intersections {
        nodes.extend(observer.observed.iter().copied());
        if let Some(root) = observer.root {
            nodes.insert(root);
        }
    }
    for observer in &state.observers.resizes {
        nodes.extend(observer.observed.iter().copied());
    }
    let rects: HashMap<NodeId, Option<Rect>> = {
        let resolver = state.observers.layout_boxes.as_ref();
        nodes
            .into_iter()
            .map(|node| (node, resolver.and_then(|resolve| resolve(node))))
            .collect()
    };
    let rect_of =
        |node: NodeId| -> Rect { rects.get(&node).copied().flatten().unwrap_or(Rect::ZERO) };

    let mut intersection_deliveries = Vec::new();
    for observer in state.observers.intersections.iter_mut() {
        if observer.observed.is_empty() {
            continue;
        }
        let root_rect = observer.root.map(rect_of).unwrap_or(viewport);
        let margin = parse_margin(&observer.root_margin, root_rect.width, root_rect.height);
        let root_bounds = expand(root_rect, margin);
        let mut entries = Vec::new();
        for &target in &observer.observed {
            if !doc.contains(target) {
                continue;
            }
            let target_rect = rect_of(target);
            let intersection_rect = target_rect.intersect(root_bounds);
            let area = intersection_rect.width * intersection_rect.height;
            let target_area = target_rect.width * target_rect.height;
            let ratio = if target_area > 0.0 {
                (area / target_area).clamp(0.0, 1.0)
            } else if area > 0.0 {
                1.0
            } else {
                0.0
            };
            let is_intersecting = area > 0.0
                || target_rect.is_empty()
                    && root_bounds.contains_point(target_rect.x, target_rect.y);
            let should_fire = match observer.last_ratio.get(&target) {
                None => true,
                Some(&previous) => {
                    (previous == 0.0) != (ratio == 0.0)
                        || observer.thresholds.iter().any(|&threshold| {
                            (previous < threshold && ratio >= threshold)
                                || (previous >= threshold && ratio < threshold)
                        })
                }
            };
            if should_fire {
                observer.last_ratio.insert(target, ratio);
                entries.push(IntersectionEntryData {
                    target,
                    is_intersecting,
                    intersection_ratio: ratio,
                    bounding_client_rect: target_rect,
                    intersection_rect,
                });
            }
        }
        if !entries.is_empty() {
            intersection_deliveries.push(PendingDelivery::intersection(observer.id, entries));
        }
    }

    let mut resize_deliveries = Vec::new();
    for observer in state.observers.resizes.iter_mut() {
        let mut entries = Vec::new();
        for &target in &observer.observed {
            if !doc.contains(target) {
                continue;
            }
            let rect = rect_of(target);
            let size = (rect.width, rect.height);
            let changed = match observer.last_size.get(&target) {
                None => true,
                Some(&(width, height)) => (width, height) != size,
            };
            if changed {
                observer.last_size.insert(target, size);
                entries.push(ResizeEntryData {
                    target,
                    content_rect: Rect::new(0.0, 0.0, rect.width, rect.height),
                });
            }
        }
        if !entries.is_empty() {
            resize_deliveries.push(PendingDelivery::resize(observer.id, entries));
        }
    }

    state.observers.pending.extend(intersection_deliveries);
    state.observers.pending.extend(resize_deliveries);
}

impl Rect {
    /// Whether the point lies inside this rectangle.
    pub fn contains_point(self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

fn build_mutation_record(
    runtime: &mut Runtime,
    host: &Rc<RefCell<HostState>>,
    record: Mutation,
) -> Value {
    let object = runtime.new_object();
    let target = element::wrap(runtime, host, record.target);
    let added_nodes: Vec<Value> = record
        .added_nodes
        .iter()
        .map(|node| element::wrap(runtime, host, *node))
        .collect();
    let removed_nodes: Vec<Value> = record
        .removed_nodes
        .iter()
        .map(|node| element::wrap(runtime, host, *node))
        .collect();
    let previous = match record.previous_sibling {
        Some(node) => element::wrap(runtime, host, node),
        None => Value::Null,
    };
    let next = match record.next_sibling {
        Some(node) => element::wrap(runtime, host, node),
        None => Value::Null,
    };
    let added = runtime.new_array(added_nodes);
    let removed = runtime.new_array(removed_nodes);
    runtime.set_property(
        object.clone(),
        "type",
        Value::String(record.kind.as_str().to_owned()),
    );
    runtime.set_property(object.clone(), "target", target);
    runtime.set_property(object.clone(), "addedNodes", added);
    runtime.set_property(object.clone(), "removedNodes", removed);
    runtime.set_property(object.clone(), "previousSibling", previous);
    runtime.set_property(object.clone(), "nextSibling", next);
    runtime.set_property(
        object.clone(),
        "attributeName",
        match record.attribute_name {
            Some(name) => Value::String(name),
            None => Value::Null,
        },
    );
    runtime.set_property(
        object.clone(),
        "oldValue",
        match record.old_value {
            Some(value) => Value::String(value),
            None => Value::Null,
        },
    );
    object
}

/// Delivers all queued observer notifications by invoking the registered
/// JavaScript callbacks. Returns the number of callbacks invoked.
///
/// Call this after [`notify_observers`] with the runtime used to install the
/// bindings, then drain microtasks as usual.
pub fn deliver_observer_callbacks(runtime: &mut Runtime, host: &Rc<RefCell<HostState>>) -> usize {
    let pending = {
        let mut state = host.borrow_mut();
        std::mem::take(&mut state.observers.pending)
    };
    let mut delivered_mutations: HashSet<u64> = HashSet::new();
    let mut delivered = 0;
    for delivery in pending {
        match delivery.kind {
            DeliveryKind::Intersection => {
                let observer = {
                    let state = host.borrow();
                    state
                        .observers
                        .intersections
                        .iter()
                        .find(|observer| observer.id == delivery.id)
                        .map(|observer| (observer.callback.clone(), observer.wrapper.clone()))
                };
                let Some((callback, wrapper)) = observer else {
                    continue;
                };
                let entries = delivery
                    .intersections
                    .into_iter()
                    .map(|entry| {
                        let object = runtime.new_object();
                        let target = element::wrap(runtime, host, entry.target);
                        let bounding = rect_object(runtime, entry.bounding_client_rect);
                        let intersection = rect_object(runtime, entry.intersection_rect);
                        runtime.set_property(object.clone(), "target", target);
                        runtime.set_property(
                            object.clone(),
                            "isIntersecting",
                            Value::Bool(entry.is_intersecting),
                        );
                        runtime.set_property(
                            object.clone(),
                            "intersectionRatio",
                            Value::Number(entry.intersection_ratio),
                        );
                        runtime.set_property(object.clone(), "boundingClientRect", bounding);
                        runtime.set_property(object.clone(), "intersectionRect", intersection);
                        runtime.set_property(object.clone(), "rootBounds", Value::Null);
                        runtime.set_property(object.clone(), "time", Value::Number(0.0));
                        object
                    })
                    .collect();
                let entries = runtime.new_array(entries);
                let _ = runtime.call_function(&callback, wrapper.clone(), vec![entries, wrapper]);
                delivered += 1;
            }
            DeliveryKind::Resize => {
                let observer = {
                    let state = host.borrow();
                    state
                        .observers
                        .resizes
                        .iter()
                        .find(|observer| observer.id == delivery.id)
                        .map(|observer| (observer.callback.clone(), observer.wrapper.clone()))
                };
                let Some((callback, wrapper)) = observer else {
                    continue;
                };
                let entries = delivery
                    .resizes
                    .into_iter()
                    .map(|entry| {
                        let object = runtime.new_object();
                        let target = element::wrap(runtime, host, entry.target);
                        let content_rect = rect_object(runtime, entry.content_rect);
                        let size_object = runtime.new_object();
                        runtime.set_property(
                            size_object.clone(),
                            "inlineSize",
                            Value::Number(entry.content_rect.width),
                        );
                        runtime.set_property(
                            size_object.clone(),
                            "blockSize",
                            Value::Number(entry.content_rect.height),
                        );
                        let sizes = runtime.new_array(vec![size_object]);
                        runtime.set_property(object.clone(), "target", target);
                        runtime.set_property(object.clone(), "contentRect", content_rect);
                        runtime.set_property(object.clone(), "contentBoxSize", sizes.clone());
                        runtime.set_property(object.clone(), "borderBoxSize", sizes);
                        object
                    })
                    .collect();
                let entries = runtime.new_array(entries);
                let _ = runtime.call_function(&callback, wrapper.clone(), vec![entries, wrapper]);
                delivered += 1;
            }
            DeliveryKind::Mutation => {
                if !delivered_mutations.insert(delivery.id) {
                    continue;
                }
                let observer = {
                    let mut state = host.borrow_mut();
                    let Some(observer) = state
                        .observers
                        .mutations
                        .iter_mut()
                        .find(|observer| observer.id == delivery.id)
                    else {
                        continue;
                    };
                    (
                        observer.callback.clone(),
                        observer.wrapper.clone(),
                        std::mem::take(&mut observer.queue),
                    )
                };
                let (callback, wrapper, records) = observer;
                if records.is_empty() {
                    continue;
                }
                let records = records
                    .into_iter()
                    .map(|record| build_mutation_record(runtime, host, record))
                    .collect();
                let records = runtime.new_array(records);
                let _ = runtime.call_function(&callback, wrapper.clone(), vec![records, wrapper]);
                delivered += 1;
            }
        }
    }
    delivered
}

/// Convenience wrapper around [`deliver_observer_callbacks`] that also drains
/// microtasks, mirroring `drain_timers_now`.
pub fn deliver_observer_callbacks_now(
    runtime: &mut Runtime,
    host: &Rc<RefCell<HostState>>,
) -> usize {
    let delivered = deliver_observer_callbacks(runtime, host);
    let _ = runtime.run_microtasks();
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document as DomDocument;
    use browser_html_parser::parse_document;

    fn setup(html: &str) -> (Runtime, Rc<RefCell<DomDocument>>, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document(html)));
        let host = crate::install(&mut runtime, document.clone(), None);
        (runtime, document, host)
    }

    fn eval(runtime: &mut Runtime, source: &str) -> String {
        runtime
            .eval(source)
            .unwrap_or_else(|error| panic!("eval failed for {source:?}: {error}"))
            .to_string_value()
    }

    #[test]
    fn globals_are_installed() {
        let (mut runtime, _document, _host) = setup("<html><body></body></html>");
        for name in [
            "IntersectionObserver",
            "IntersectionObserverEntry",
            "ResizeObserver",
            "ResizeObserverEntry",
            "MutationObserver",
            "MutationRecord",
        ] {
            assert_eq!(eval(&mut runtime, &format!("typeof {name}")), "function");
        }
    }

    #[test]
    fn intersection_observer_fires_on_layout_change() {
        let (mut runtime, document, host) = setup("<html><body><div id='box'></div></body></html>");
        let doc_for_resolver = document.clone();
        host.borrow_mut()
            .observers
            .set_layout_box_resolver(move |node| {
                let document = doc_for_resolver.borrow();
                if document.get_element_by_id("box") == Some(node) {
                    Some(Rect::new(10.0, 10.0, 100.0, 100.0))
                } else {
                    None
                }
            });
        runtime
            .eval(
                "var hits = [];\n\
                 var io = new IntersectionObserver(function (entries, observer) {\n\
                     hits.push(entries[0].isIntersecting + ':' + entries[0].intersectionRatio);\n\
                 });\n\
                 io.observe(document.getElementById('box'));",
            )
            .unwrap();

        let doc = document.borrow();
        notify_observers(&host, Rect::new(0.0, 0.0, 200.0, 200.0), &doc);
        drop(doc);
        assert_eq!(deliver_observer_callbacks(&mut runtime, &host), 1);
        assert_eq!(eval(&mut runtime, "hits.join(',')"), "true:1");

        // Moving the box fully outside the viewport flips isIntersecting.
        let doc_for_resolver = document.clone();
        host.borrow_mut()
            .observers
            .set_layout_box_resolver(move |node| {
                let document = doc_for_resolver.borrow();
                if document.get_element_by_id("box") == Some(node) {
                    Some(Rect::new(500.0, 500.0, 100.0, 100.0))
                } else {
                    None
                }
            });
        let doc = document.borrow();
        notify_observers(&host, Rect::new(0.0, 0.0, 200.0, 200.0), &doc);
        drop(doc);
        assert_eq!(deliver_observer_callbacks(&mut runtime, &host), 1);
        assert_eq!(eval(&mut runtime, "hits.join(',')"), "true:1,false:0");
    }

    #[test]
    fn intersection_respects_threshold_and_root_margin() {
        let (mut runtime, document, host) = setup("<html><body><div id='box'></div></body></html>");
        let doc_for_resolver = document.clone();
        host.borrow_mut()
            .observers
            .set_layout_box_resolver(move |node| {
                let document = doc_for_resolver.borrow();
                if document.get_element_by_id("box") == Some(node) {
                    Some(Rect::new(0.0, 0.0, 100.0, 100.0))
                } else {
                    None
                }
            });
        runtime
            .eval(
                "var seen = [];\n\
                 var io = new IntersectionObserver(function (entries) {\n\
                     seen.push(entries[0].intersectionRatio);\n\
                 }, { threshold: [0, 0.5, 1] });\n\
                 io.observe(document.getElementById('box'));",
            )
            .unwrap();
        // Viewport height 50 => ratio 0.5, crossing the 0.5 threshold.
        let doc = document.borrow();
        notify_observers(&host, Rect::new(0.0, 0.0, 100.0, 50.0), &doc);
        drop(doc);
        deliver_observer_callbacks(&mut runtime, &host);
        assert_eq!(eval(&mut runtime, "seen.join(',')"), "0.5");
    }

    #[test]
    fn resize_observer_fires_when_size_changes() {
        let (mut runtime, document, host) = setup("<html><body><div id='box'></div></body></html>");
        let size = Rc::new(RefCell::new((100.0_f64, 50.0_f64)));
        let shared = size.clone();
        let doc_for_resolver = document.clone();
        host.borrow_mut()
            .observers
            .set_layout_box_resolver(move |node| {
                let document = doc_for_resolver.borrow();
                if document.get_element_by_id("box") == Some(node) {
                    let (width, height) = *shared.borrow();
                    Some(Rect::new(0.0, 0.0, width, height))
                } else {
                    None
                }
            });
        runtime
            .eval(
                "var sizes = [];\n\
                 var ro = new ResizeObserver(function (entries) {\n\
                     sizes.push(entries[0].contentRect.width + 'x' + entries[0].contentRect.height);\n\
                 });\n\
                 ro.observe(document.getElementById('box'));",
            )
            .unwrap();
        let doc = document.borrow();
        notify_observers(&host, Rect::ZERO, &doc);
        drop(doc);
        deliver_observer_callbacks(&mut runtime, &host);
        assert_eq!(eval(&mut runtime, "sizes.join(',')"), "100x50");

        // Same size: no new callback.
        let doc = document.borrow();
        notify_observers(&host, Rect::ZERO, &doc);
        drop(doc);
        assert_eq!(deliver_observer_callbacks(&mut runtime, &host), 0);

        *size.borrow_mut() = (200.0, 80.0);
        let doc = document.borrow();
        notify_observers(&host, Rect::ZERO, &doc);
        drop(doc);
        deliver_observer_callbacks(&mut runtime, &host);
        assert_eq!(eval(&mut runtime, "sizes.join(',')"), "100x50,200x80");
    }

    #[test]
    fn mutation_observer_records_child_list_and_attributes() {
        let (mut runtime, _document, host) =
            setup("<html><body><div id='root'></div></body></html>");
        runtime
            .eval(
                "var kinds = [];\n\
                 var mo = new MutationObserver(function (records) {\n\
                     for (var i = 0; i < records.length; i++) kinds.push(records[i].type);\n\
                 });\n\
                 var root = document.getElementById('root');\n\
                 mo.observe(root, { childList: true, attributes: true, subtree: true });\n\
                 var child = document.createElement('span');\n\
                 root.appendChild(child);\n\
                 child.setAttribute('data-x', '1');",
            )
            .unwrap();
        assert_eq!(deliver_observer_callbacks(&mut runtime, &host), 1);
        assert_eq!(
            eval(&mut runtime, "kinds.join(',')"),
            "childList,attributes"
        );
    }

    #[test]
    fn mutation_observer_take_records_drains_queue() {
        let (mut runtime, _document, host) =
            setup("<html><body><div id='root'></div></body></html>");
        runtime
            .eval(
                "var root = document.getElementById('root');\n\
                 var mo = new MutationObserver(function () {});\n\
                 mo.observe(root, { attributes: true });\n\
                 root.setAttribute('id', 'changed');\n\
                 var taken = mo.takeRecords();",
            )
            .unwrap();
        assert_eq!(eval(&mut runtime, "taken.length"), "1");
        assert_eq!(eval(&mut runtime, "taken[0].type"), "attributes");
        assert_eq!(eval(&mut runtime, "taken[0].oldValue"), "null");
        assert_eq!(eval(&mut runtime, "mo.takeRecords().length"), "0");
        assert_eq!(deliver_observer_callbacks(&mut runtime, &host), 0);
    }

    #[test]
    fn disconnect_stops_delivery() {
        let (mut runtime, _document, host) =
            setup("<html><body><div id='root'></div></body></html>");
        runtime
            .eval(
                "var count = 0;\n\
                 var mo = new MutationObserver(function () { count++; });\n\
                 mo.observe(document.getElementById('root'), { childList: true });\n\
                 document.getElementById('root').appendChild(document.createElement('b'));\n\
                 mo.disconnect();",
            )
            .unwrap();
        assert_eq!(deliver_observer_callbacks(&mut runtime, &host), 0);
        assert_eq!(eval(&mut runtime, "count"), "0");
    }
}
