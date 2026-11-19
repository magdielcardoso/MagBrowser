//! `fetch()`, `Response` and the host bridge used by the embedder.
//!
//! Networking is performed synchronously by the host callback installed on
//! [`HostState`], but the API is surfaced as Promises so scripts can `await` it
//! and call `.then()`. The bridge types are plain, `Send + Sync` data so the
//! embedder may dispatch the work to a worker thread if it wishes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use browser_js::{JsError, Runtime, Value};
use browser_url::Url;

use crate::HostState;

/// Error returned by the host fetch bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError {
    /// Human-readable failure reason.
    pub message: String,
}

impl FetchError {
    /// Creates a fetch error from any displayable message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FetchError {}

/// A request handed to the host bridge.
///
/// All fields are owned, making the struct `Send`-able.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchRequest {
    /// Absolute URL to fetch.
    pub url: String,
    /// Upper-cased HTTP method (defaults to `GET`).
    pub method: String,
    /// Request headers in insertion order.
    pub headers: Vec<(String, String)>,
    /// Request body bytes (empty for bodyless requests).
    pub body: Vec<u8>,
    /// Id of an `AbortSignal`, when the request was given one.
    pub signal: Option<u64>,
}

/// A response produced by the host bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchResponse {
    /// True for 2xx status codes.
    pub ok: bool,
    /// Numeric HTTP status.
    pub status: u16,
    /// HTTP reason phrase.
    pub status_text: String,
    /// Response headers in insertion order.
    pub headers: Vec<(String, String)>,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

impl FetchResponse {
    /// Builds a response, deriving `ok` from the status code.
    pub fn new(
        status: u16,
        status_text: impl Into<String>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            ok: (200..300).contains(&status),
            status,
            status_text: status_text.into(),
            headers,
            body,
        }
    }
}

/// Host callback installed by the embedder to service `fetch` / `XMLHttpRequest`.
pub type FetchHandler =
    Arc<dyn Fn(FetchRequest) -> Result<FetchResponse, FetchError> + Send + Sync>;

/// Runs the installed host bridge, if any.
pub fn perform_fetch(
    state: &HostState,
    request: FetchRequest,
) -> Result<FetchResponse, FetchError> {
    let Some(handler) = state.fetch_handler.as_ref() else {
        return Err(FetchError::new("no fetch handler installed by the host"));
    };
    handler(request)
}

/// Resolves a possibly-relative URL against the document URL in `state`.
pub fn resolve_url(state: &HostState, raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err("a URL is required".to_owned());
    }
    if let Ok(url) = Url::parse(raw) {
        return Ok(url.as_str().to_owned());
    }
    let base = state
        .url
        .as_deref()
        .ok_or_else(|| format!("cannot resolve relative URL '{raw}' without a document URL"))?;
    let base = Url::parse(base).map_err(|error| error.to_string())?;
    base.join(raw)
        .map(|url| url.as_str().to_owned())
        .map_err(|error| error.to_string())
}

/// JavaScript-side bodies of every `Response` created through `fetch`.
#[derive(Default)]
struct ResponseStore {
    next: u64,
    bodies: HashMap<u64, FetchResponse>,
}

/// Installs the `fetch`, `Response`, `Headers` and `URL`-adjacent globals.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    let store = Rc::new(RefCell::new(ResponseStore::default()));

    runtime.define_global_function("fetch", {
        let state = state.clone();
        let store = store.clone();
        Box::new(move |runtime, _this, args| fetch_impl(runtime, &state, &store, args))
    });

    runtime.define_global_function("Response", {
        let store = store.clone();
        Box::new(move |runtime, _this, args| {
            let body = args
                .first()
                .map(|value| to_js_string(runtime, value))
                .unwrap_or_default();
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            let status = property_number(runtime, &init, "status", 200.0) as u16;
            let status_text = property_string(runtime, &init, "statusText", "");
            let headers_value = property_value(runtime, &init, "headers");
            let headers = headers_from_value(runtime, &headers_value);
            let response = FetchResponse::new(status, status_text, headers, body.into_bytes());
            Ok(make_response(runtime, &store, response))
        })
    });

    runtime.define_global_function(
        "Headers",
        Box::new(move |runtime, this, args| {
            let object = match this {
                Value::Object(_) => this.clone(),
                _ => runtime.new_object(),
            };
            let initial = args.first().cloned().unwrap_or(Value::Undefined);
            let pairs = headers_from_value(runtime, &initial);
            install_headers_object(runtime, &object, pairs);
            Ok(object)
        }),
    );
}

fn fetch_impl(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    store: &Rc<RefCell<ResponseStore>>,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let raw_url = args
        .first()
        .map(|value| to_js_string(runtime, value))
        .unwrap_or_default();
    let init = args.get(1).cloned().unwrap_or(Value::Undefined);

    let method = property_string(runtime, &init, "method", "GET").to_ascii_uppercase();
    let headers_value = property_value(runtime, &init, "headers");
    let headers = headers_from_value(runtime, &headers_value);
    let mut headers = headers;
    let body = body_from_init(runtime, &init, &mut headers);
    let signal = signal_from_init(runtime, &init);

    let url = {
        let borrowed = state.borrow();
        resolve_url(&borrowed, &raw_url)
    };
    let url = match url {
        Ok(url) => url,
        Err(message) => {
            let error = type_error(runtime, &message);
            return Ok(rejected_promise(runtime, error));
        }
    };

    if signal.is_some_and(|id| {
        state
            .borrow()
            .abort_signals
            .get(&id)
            .is_some_and(|s| s.aborted)
    }) {
        let error = crate::abort::abort_error(runtime);
        return Ok(rejected_promise(runtime, error));
    }

    let request = FetchRequest {
        url,
        method,
        headers,
        body,
        signal,
    };
    let result = {
        let borrowed = state.borrow();
        perform_fetch(&borrowed, request)
    };
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            let rejection = type_error(runtime, &error.message);
            return Ok(rejected_promise(runtime, rejection));
        }
    };

    if signal.is_some_and(|id| {
        state
            .borrow()
            .abort_signals
            .get(&id)
            .is_some_and(|s| s.aborted)
    }) {
        let error = crate::abort::abort_error(runtime);
        return Ok(rejected_promise(runtime, error));
    }

    let response = make_response(runtime, store, response);
    Ok(resolved_promise(runtime, response))
}

fn make_response(
    runtime: &mut Runtime,
    store: &Rc<RefCell<ResponseStore>>,
    response: FetchResponse,
) -> Value {
    let (id, ok, status, status_text) = {
        let mut store = store.borrow_mut();
        let id = store.next;
        store.next += 1;
        store.bodies.insert(id, response);
        let record = &store.bodies[&id];
        (id, record.ok, record.status, record.status_text.clone())
    };

    let object = runtime.new_object();
    runtime.set_property(object.clone(), "__responseId", Value::Number(id as f64));
    runtime.set_property(object.clone(), "ok", Value::Bool(ok));
    runtime.set_property(object.clone(), "status", Value::Number(status as f64));
    runtime.set_property(object.clone(), "statusText", Value::String(status_text));
    let headers = make_headers_view(runtime, store, id);
    runtime.set_property(object.clone(), "headers", headers);

    runtime.define_host_method(object.clone(), "text", {
        let store = store.clone();
        Box::new(move |runtime, this, _args| {
            let bytes = response_bytes(&store, runtime, &this);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Ok(resolved_promise(runtime, Value::String(text)))
        })
    });

    runtime.define_host_method(object.clone(), "json", {
        let store = store.clone();
        Box::new(move |runtime, this, _args| {
            let bytes = response_bytes(&store, runtime, &this);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let json = runtime.get_global("JSON");
            let parse = runtime.get_property(&json, "parse");
            match runtime.call_function(&parse, Value::Undefined, vec![Value::String(text)]) {
                Ok(value) => Ok(resolved_promise(runtime, value)),
                Err(error) => {
                    let rejection = type_error(runtime, &error.message);
                    Ok(rejected_promise(runtime, rejection))
                }
            }
        })
    });

    runtime.define_host_method(object.clone(), "arrayBuffer", {
        let store = store.clone();
        Box::new(move |runtime, this, _args| {
            let bytes = response_bytes(&store, runtime, &this);
            let numbers = bytes
                .iter()
                .map(|byte| Value::Number(*byte as f64))
                .collect();
            let buffer = runtime.new_object();
            runtime.set_property(
                buffer.clone(),
                "byteLength",
                Value::Number(bytes.len() as f64),
            );
            let bytes_array = runtime.new_array(numbers);
            runtime.set_property(buffer.clone(), "bytes", bytes_array);
            runtime.set_property(buffer.clone(), "__isArrayBuffer", Value::Bool(true));
            Ok(resolved_promise(runtime, buffer))
        })
    });

    object
}

fn response_bytes(
    store: &Rc<RefCell<ResponseStore>>,
    runtime: &mut Runtime,
    this: &Value,
) -> Vec<u8> {
    let id = match runtime.get_property(this, "__responseId") {
        Value::Number(number) => number as u64,
        _ => return Vec::new(),
    };
    store
        .borrow()
        .bodies
        .get(&id)
        .map(|response| response.body.clone())
        .unwrap_or_default()
}

fn make_headers_view(runtime: &mut Runtime, store: &Rc<RefCell<ResponseStore>>, id: u64) -> Value {
    let object = runtime.new_object();
    runtime.set_property(object.clone(), "__responseId", Value::Number(id as f64));
    runtime.define_host_method(object.clone(), "get", {
        let store = store.clone();
        Box::new(move |runtime, this, args| {
            let Some(name) = args.first().map(|value| value.to_string_value()) else {
                return Ok(Value::Null);
            };
            let value = response_header(&store, runtime, &this, &name);
            Ok(value.map_or(Value::Null, Value::String))
        })
    });
    runtime.define_host_method(object.clone(), "has", {
        let store = store.clone();
        Box::new(move |runtime, this, args| {
            let Some(name) = args.first().map(|value| value.to_string_value()) else {
                return Ok(Value::Bool(false));
            };
            Ok(Value::Bool(
                response_header(&store, runtime, &this, &name).is_some(),
            ))
        })
    });
    object
}

fn response_header(
    store: &Rc<RefCell<ResponseStore>>,
    runtime: &mut Runtime,
    this: &Value,
    name: &str,
) -> Option<String> {
    let id = match runtime.get_property(this, "__responseId") {
        Value::Number(number) => number as u64,
        _ => return None,
    };
    store.borrow().bodies.get(&id).and_then(|response| {
        response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    })
}

fn install_headers_object(runtime: &mut Runtime, object: &Value, pairs: Vec<(String, String)>) {
    runtime.set_property(object.clone(), "__isHeaders", Value::Bool(true));
    set_headers_pairs(runtime, object, pairs);

    runtime.define_host_method(
        object.clone(),
        "get",
        Box::new(move |runtime, this, args| {
            let Some(name) = args.first().map(|value| value.to_string_value()) else {
                return Ok(Value::Null);
            };
            let value = read_headers_pairs(runtime, &this)
                .into_iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(&name))
                .map(|(_, value)| value);
            Ok(value.map_or(Value::Null, Value::String))
        }),
    );
    runtime.define_host_method(
        object.clone(),
        "set",
        Box::new(move |runtime, this, args| {
            if args.len() < 2 {
                return Err(JsError::type_error("Headers.set needs a name and value"));
            }
            let name = args[0].to_string_value();
            let value = args[1].to_string_value();
            let mut pairs = read_headers_pairs(runtime, &this);
            pairs.retain(|(key, _)| !key.eq_ignore_ascii_case(&name));
            pairs.push((name, value));
            set_headers_pairs(runtime, &this, pairs);
            Ok(Value::Undefined)
        }),
    );
    runtime.define_host_method(
        object.clone(),
        "append",
        Box::new(move |runtime, this, args| {
            if args.len() < 2 {
                return Err(JsError::type_error("Headers.append needs a name and value"));
            }
            let mut pairs = read_headers_pairs(runtime, &this);
            pairs.push((args[0].to_string_value(), args[1].to_string_value()));
            set_headers_pairs(runtime, &this, pairs);
            Ok(Value::Undefined)
        }),
    );
    runtime.define_host_method(
        object.clone(),
        "has",
        Box::new(move |runtime, this, args| {
            let name = args
                .first()
                .map_or(String::new(), |value| value.to_string_value());
            Ok(Value::Bool(
                read_headers_pairs(runtime, &this)
                    .iter()
                    .any(|(key, _)| key.eq_ignore_ascii_case(&name)),
            ))
        }),
    );
    runtime.define_host_method(
        object.clone(),
        "toString",
        Box::new(move |runtime, this, _args| {
            let text = read_headers_pairs(runtime, &this)
                .iter()
                .map(|(key, value)| format!("{key}: {value}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            Ok(Value::String(text))
        }),
    );
}

fn set_headers_pairs(runtime: &mut Runtime, object: &Value, pairs: Vec<(String, String)>) {
    let mut entries = Vec::with_capacity(pairs.len());
    for (name, value) in pairs {
        entries.push(runtime.new_array(vec![Value::String(name), Value::String(value)]));
    }
    let array = runtime.new_array(entries);
    runtime.set_property(object.clone(), "__pairs", array);
}

fn read_headers_pairs(runtime: &mut Runtime, object: &Value) -> Vec<(String, String)> {
    let pairs = runtime.get_property(object, "__pairs");
    array_of_pairs(runtime, &pairs)
}

fn array_of_pairs(runtime: &mut Runtime, value: &Value) -> Vec<(String, String)> {
    let Value::Object(_) = value else {
        return Vec::new();
    };
    let length = match runtime.get_property(value, "length") {
        Value::Number(number) => number as usize,
        _ => 0,
    };
    (0..length)
        .map(|index| {
            let pair = runtime.get_property(value, &index.to_string());
            (
                runtime.get_property(&pair, "0").to_string_value(),
                runtime.get_property(&pair, "1").to_string_value(),
            )
        })
        .collect()
}

fn headers_from_value(runtime: &mut Runtime, value: &Value) -> Vec<(String, String)> {
    let Value::Object(_) = value else {
        return Vec::new();
    };
    // Headers objects carry their pairs on a hidden array property.
    if runtime.get_property(value, "__isHeaders").is_truthy() {
        let pairs = runtime.get_property(value, "__pairs");
        return array_of_pairs(runtime, &pairs);
    }
    // Arrays of `[name, value]` pairs (also used by `URLSearchParams`-like data).
    let length = match runtime.get_property(value, "length") {
        Value::Number(number) => number as usize,
        _ => 0,
    };
    if length > 0 {
        return array_of_pairs(runtime, value);
    }
    // Plain objects (e.g. `{ 'X-Test': 'yes' }`).
    runtime
        .object_keys(value)
        .into_iter()
        .map(|key| {
            let value = runtime.get_property(value, &key).to_string_value();
            (key, value)
        })
        .collect()
}

fn body_from_init(
    runtime: &mut Runtime,
    init: &Value,
    headers: &mut Vec<(String, String)>,
) -> Vec<u8> {
    let body = property_value(runtime, init, "body");
    let text = match body {
        Value::Undefined | Value::Null => return Vec::new(),
        Value::Object(_) => {
            if runtime.get_property(&body, "__isHeaders").is_truthy()
                || runtime.get_property(&body, "__isSearchParams").is_truthy()
            {
                to_js_string(runtime, &body)
            } else {
                let json = runtime.get_global("JSON");
                let stringify = runtime.get_property(&json, "stringify");
                match runtime.call_function(&stringify, Value::Undefined, vec![body.clone()]) {
                    Ok(Value::String(text)) => text,
                    Ok(other) => other.to_string_value(),
                    Err(_) => return Vec::new(),
                }
            }
        }
        other => to_js_string(runtime, &other),
    };
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Content-Type"))
    {
        headers.push((
            "Content-Type".to_owned(),
            "text/plain;charset=UTF-8".to_owned(),
        ));
    }
    text.into_bytes()
}

fn signal_from_init(runtime: &mut Runtime, init: &Value) -> Option<u64> {
    let signal = property_value(runtime, init, "signal");
    if matches!(signal, Value::Object(_)) {
        crate::abort::signal_id(runtime, &signal)
    } else {
        None
    }
}

fn property_value(runtime: &mut Runtime, object: &Value, name: &str) -> Value {
    if matches!(object, Value::Object(_)) {
        runtime.get_property(object, name)
    } else {
        Value::Undefined
    }
}

fn property_string(runtime: &mut Runtime, object: &Value, name: &str, default: &str) -> String {
    let value = property_value(runtime, object, name);
    if matches!(value, Value::Undefined | Value::Null) {
        default.to_owned()
    } else {
        value.to_string_value()
    }
}

fn property_number(runtime: &mut Runtime, object: &Value, name: &str, default: f64) -> f64 {
    let value = property_value(runtime, object, name);
    if matches!(value, Value::Undefined | Value::Null) {
        default
    } else {
        value.to_number()
    }
}

/// Converts a value to a string, honouring custom `toString()` methods.
fn to_js_string(runtime: &mut Runtime, value: &Value) -> String {
    if let Value::Object(_) = value {
        let stringify = runtime.get_property(value, "toString");
        if matches!(stringify, Value::Object(_))
            && let Ok(result) = runtime.call_function(&stringify, value.clone(), Vec::new())
        {
            return result.to_string_value();
        }
    }
    value.to_string_value()
}

fn resolved_promise(runtime: &mut Runtime, value: Value) -> Value {
    let promise = runtime.get_global("Promise");
    let resolve = runtime.get_property(&promise, "resolve");
    runtime
        .call_function(&resolve, Value::Undefined, vec![value])
        .unwrap_or(Value::Undefined)
}

fn rejected_promise(runtime: &mut Runtime, value: Value) -> Value {
    let promise = runtime.get_global("Promise");
    let reject = runtime.get_property(&promise, "reject");
    runtime
        .call_function(&reject, Value::Undefined, vec![value])
        .unwrap_or(Value::Undefined)
}

fn type_error(runtime: &mut Runtime, message: &str) -> Value {
    let constructor = runtime.get_global("TypeError");
    if matches!(constructor, Value::Object(_))
        && let Ok(value) = runtime.call_function(
            &constructor,
            Value::Undefined,
            vec![Value::String(message.to_owned())],
        )
    {
        return value;
    }
    Value::String(message.to_owned())
}
