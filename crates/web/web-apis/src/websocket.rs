//! The `WebSocket` global, backed by [`browser_event_loop::WebSocketManager`].
//!
//! The manager owns the connection state machine; this module supplies the
//! JavaScript wrapper, the event-handler bridge and a minimal real TCP
//! transport. The default transport is in-memory so the API is deterministic in
//! tests — embedders can install [`TcpWebSocketTransport`] (or their own) with
//! [`crate::set_websocket_transport`].

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use browser_event_loop::{
    TransportError, WebSocketEvent, WebSocketId, WebSocketMessage, WebSocketState,
    WebSocketTransport,
};
use browser_js::{JsError, Runtime, Value};

use crate::HostState;

/// Shared state of the default, host-driven WebSocket transport.
#[derive(Default)]
pub struct HostWebSocketState {
    /// Inbound events queued by the host.
    pub inbound: VecDeque<(WebSocketId, WebSocketEvent)>,
    /// URL of each connection opened through the transport.
    pub connections: Vec<(WebSocketId, String)>,
    /// Frames sent by scripts.
    pub sent: Vec<(WebSocketId, WebSocketMessage)>,
    /// Close requests sent by scripts.
    pub closed: Vec<(WebSocketId, u16, String)>,
    /// When true, `connect` fails.
    pub fail_connect: bool,
}

/// Shared handle to [`HostWebSocketState`].
pub type HostWebSocketQueue = Arc<Mutex<HostWebSocketState>>;

/// A `WebSocketTransport` whose inbound events are injected through shared
/// state. This is the deterministic default; embedders may replace it with
/// [`TcpWebSocketTransport`] or their own implementation.
#[derive(Clone)]
pub struct HostWebSocketTransport {
    /// Shared state used by the host and the transport.
    pub shared: HostWebSocketQueue,
}

impl WebSocketTransport for HostWebSocketTransport {
    fn connect(&mut self, id: WebSocketId, url: &str) -> Result<(), TransportError> {
        if self.shared.lock().is_ok_and(|state| state.fail_connect) {
            return Err(TransportError::new("simulated WebSocket connect failure"));
        }
        if let Ok(mut state) = self.shared.lock() {
            state.connections.push((id, url.to_owned()));
        }
        Ok(())
    }

    fn send(&mut self, id: WebSocketId, message: WebSocketMessage) -> Result<(), TransportError> {
        if let Ok(mut state) = self.shared.lock() {
            state.sent.push((id, message));
        }
        Ok(())
    }

    fn close(&mut self, id: WebSocketId, code: u16, reason: &str) -> Result<(), TransportError> {
        if let Ok(mut state) = self.shared.lock() {
            state.closed.push((id, code, reason.to_owned()));
        }
        Ok(())
    }

    fn poll(&mut self) -> Vec<(WebSocketId, WebSocketEvent)> {
        self.shared.lock().map_or_else(
            |_| Vec::new(),
            |mut state| state.inbound.drain(..).collect(),
        )
    }
}

/// JavaScript event handlers and wrapper for one managed connection.
#[derive(Clone)]
pub struct WebSocketJsHandlers {
    /// The wrapper object handed to scripts.
    pub wrapper: Value,
    /// `onopen` handler.
    pub open: Option<Value>,
    /// `onmessage` handler.
    pub message: Option<Value>,
    /// `onclose` handler.
    pub close: Option<Value>,
    /// `onerror` handler.
    pub error: Option<Value>,
}

impl WebSocketJsHandlers {
    fn new(wrapper: Value) -> Self {
        Self {
            wrapper,
            open: None,
            message: None,
            close: None,
            error: None,
        }
    }
}

/// Installs the `WebSocket` global constructor.
pub fn install(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) {
    runtime.define_global_function("WebSocket", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let url = args
                .first()
                .map(Value::to_string_value)
                .ok_or_else(|| JsError::type_error("WebSocket needs a URL"))?;
            let id = {
                let mut borrowed = state.borrow_mut();
                borrowed
                    .websocket_manager
                    .connect(url.clone())
                    .map_err(|error| JsError::new(error.to_string()))?
            };

            let object = if matches!(this, Value::Object(_)) {
                this
            } else {
                runtime.new_object()
            };
            runtime.set_property(object.clone(), "__wsId", Value::Number(id.0 as f64));
            runtime.set_property(object.clone(), "url", Value::String(url));
            runtime.set_property(object.clone(), "bufferedAmount", Value::Number(0.0));
            install_socket_members(runtime, &state, &object, id);
            state
                .borrow_mut()
                .websocket_handlers
                .insert(id.0, WebSocketJsHandlers::new(object.clone()));
            Ok(object)
        })
    });
}

fn socket_id(runtime: &mut Runtime, value: &Value) -> Result<WebSocketId, JsError> {
    match runtime.get_property(value, "__wsId") {
        Value::Number(number) if number >= 0.0 => Ok(WebSocketId(number as u64)),
        _ => Err(JsError::type_error("expected a WebSocket object")),
    }
}

fn install_socket_members(
    runtime: &mut Runtime,
    state: &Rc<RefCell<HostState>>,
    object: &Value,
    id: WebSocketId,
) {
    runtime.define_host_method(object.clone(), "send", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let id = socket_id(runtime, &this)?;
            let value = args
                .first()
                .cloned()
                .ok_or_else(|| JsError::type_error("send needs data"))?;
            let message = if let Value::String(text) = &value {
                WebSocketMessage::Text(text.clone())
            } else {
                WebSocketMessage::Binary(value.to_string_value().into_bytes())
            };
            state
                .borrow_mut()
                .websocket_manager
                .send(id, message)
                .map_err(|error| JsError::new(error.to_string()))?;
            Ok(Value::Undefined)
        })
    });

    runtime.define_host_method(object.clone(), "close", {
        let state = state.clone();
        Box::new(move |runtime, this, args| {
            let id = socket_id(runtime, &this)?;
            let code = args
                .first()
                .map(|value| value.to_number() as u16)
                .unwrap_or(1000);
            let reason = args.get(1).map(Value::to_string_value).unwrap_or_default();
            state
                .borrow_mut()
                .websocket_manager
                .close(id, code, &reason)
                .map_err(|error| JsError::new(error.to_string()))?;
            Ok(Value::Undefined)
        })
    });

    let handler_names = [
        ("onopen", "open"),
        ("onmessage", "message"),
        ("onclose", "close"),
        ("onerror", "error"),
    ];
    for (js_name, field) in handler_names {
        runtime.define_accessor(
            object.clone(),
            js_name,
            {
                let state = state.clone();
                Box::new(move |_runtime, _this, _args| {
                    Ok(state
                        .borrow()
                        .websocket_handlers
                        .get(&id.0)
                        .and_then(|handlers| match field {
                            "open" => handlers.open.clone(),
                            "message" => handlers.message.clone(),
                            "close" => handlers.close.clone(),
                            "error" => handlers.error.clone(),
                            _ => None,
                        })
                        .unwrap_or(Value::Null))
                })
            },
            {
                let state = state.clone();
                Box::new(move |_runtime, _this, args| {
                    let handler = args.first().cloned().unwrap_or(Value::Null);
                    let handler = if matches!(handler, Value::Null) {
                        None
                    } else {
                        Some(handler)
                    };
                    if let Some(handlers) = state.borrow_mut().websocket_handlers.get_mut(&id.0) {
                        match field {
                            "open" => handlers.open = handler,
                            "message" => handlers.message = handler,
                            "close" => handlers.close = handler,
                            "error" => handlers.error = handler,
                            _ => {}
                        }
                    }
                    Ok(Value::Undefined)
                })
            },
        );
    }

    runtime.define_accessor(
        object.clone(),
        "readyState",
        {
            let state = state.clone();
            Box::new(move |_runtime, this, _args| {
                let id = {
                    // `socket_id` needs &mut Runtime; read the hidden id directly.
                    match _runtime.get_property(&this, "__wsId") {
                        Value::Number(n) if n >= 0.0 => WebSocketId(n as u64),
                        _ => return Ok(Value::Number(3.0)),
                    }
                };
                let state = state.borrow();
                let numeric = match state.websocket_manager.state(id) {
                    Some(WebSocketState::Connecting) => 0.0,
                    Some(WebSocketState::Open) => 1.0,
                    Some(WebSocketState::Closing) => 2.0,
                    Some(WebSocketState::Closed) | None => 3.0,
                };
                Ok(Value::Number(numeric))
            })
        },
        { Box::new(|_runtime, _this, _args| Err(JsError::type_error("readyState is read-only"))) },
    );

    let _ = id;
}

/// Polls the transport and schedules JavaScript event delivery as microtasks.
pub fn pump(runtime: &mut Runtime, state: &Rc<RefCell<HostState>>) -> usize {
    let applied = state.borrow_mut().websocket_manager.pump();
    let ids: Vec<WebSocketId> = state.borrow().websocket_manager.ids().collect();
    for id in ids {
        let events = state
            .borrow_mut()
            .websocket_manager
            .take_events(id)
            .unwrap_or_default();
        let handlers = state.borrow().websocket_handlers.get(&id.0).cloned();
        let Some(handlers) = handlers else { continue };
        for event in events {
            let (kind, handler) = match &event {
                WebSocketEvent::Open => ("open", handlers.open.clone()),
                WebSocketEvent::Message(_) => ("message", handlers.message.clone()),
                WebSocketEvent::Close { .. } => ("close", handlers.close.clone()),
                WebSocketEvent::Error(_) => ("error", handlers.error.clone()),
            };
            let ready = match &event {
                WebSocketEvent::Close { .. } | WebSocketEvent::Error(_) => 3.0,
                WebSocketEvent::Open | WebSocketEvent::Message(_) => 1.0,
            };
            let wrapper = handlers.wrapper.clone();
            let (message, code, reason, error) = match &event {
                WebSocketEvent::Message(WebSocketMessage::Text(text)) => {
                    (Some(text.clone()), None, None, None)
                }
                WebSocketEvent::Message(WebSocketMessage::Binary(bytes)) => (
                    Some(String::from_utf8_lossy(bytes).into_owned()),
                    None,
                    None,
                    None,
                ),
                WebSocketEvent::Close { code, reason } => {
                    (None, Some(*code), Some(reason.clone()), None)
                }
                WebSocketEvent::Error(message) => (None, None, None, Some(message.clone())),
                WebSocketEvent::Message(_) | WebSocketEvent::Open => (None, None, None, None),
            };
            if let Some(handler) = handler {
                runtime.enqueue_microtask(Box::new(move |runtime, _this, _args| {
                    runtime.set_property(wrapper.clone(), "readyState", Value::Number(ready));
                    let event_object = runtime.new_object();
                    runtime.set_property(
                        event_object.clone(),
                        "type",
                        Value::String(kind.to_owned()),
                    );
                    if let Some(data) = message.clone() {
                        runtime.set_property(event_object.clone(), "data", Value::String(data));
                    }
                    if let Some(code) = code {
                        runtime.set_property(
                            event_object.clone(),
                            "code",
                            Value::Number(f64::from(code)),
                        );
                    }
                    if let Some(reason) = reason.clone() {
                        runtime.set_property(event_object.clone(), "reason", Value::String(reason));
                    }
                    if let Some(error) = error.clone() {
                        runtime.set_property(event_object.clone(), "message", Value::String(error));
                    }
                    runtime.call_function(&handler, wrapper.clone(), vec![event_object])?;
                    Ok(Value::Undefined)
                }));
            }
        }
    }
    applied
}

// ---------------------------------------------------------------------------
// Minimal real TCP transport (ws:// only; wss needs TLS supplied by the host).
// ---------------------------------------------------------------------------

/// A `WebSocketTransport` that speaks RFC 6455 text frames over a plain TCP
/// socket. Secure (`wss://`) URLs are rejected; a host may supply a TLS-capable
/// transport instead.
pub struct TcpWebSocketTransport {
    writers: HashMap<WebSocketId, TcpStream>,
    readers: HashMap<WebSocketId, TcpStream>,
    buffers: HashMap<WebSocketId, Vec<u8>>,
    pending: VecDeque<(WebSocketId, WebSocketEvent)>,
}

impl Default for TcpWebSocketTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl TcpWebSocketTransport {
    /// Creates an empty transport.
    pub fn new() -> Self {
        Self {
            writers: HashMap::new(),
            readers: HashMap::new(),
            buffers: HashMap::new(),
            pending: VecDeque::new(),
        }
    }
}

fn parse_ws_url(url: &str) -> Result<(String, u16, String, bool), TransportError> {
    let parsed =
        browser_url::Url::parse(url).map_err(|error| TransportError::new(error.to_string()))?;
    let secure = matches!(parsed.scheme(), "wss");
    if !matches!(parsed.scheme(), "ws" | "wss") {
        return Err(TransportError::new(
            "WebSocket URL must use ws:// or wss://",
        ));
    }
    let host = parsed
        .host()
        .ok_or_else(|| TransportError::new("WebSocket URL has no host"))?
        .to_owned();
    let port = parsed
        .port_or_default()
        .unwrap_or(if secure { 443 } else { 80 });
    if secure {
        return Err(TransportError::new(
            "wss:// requires a TLS transport supplied by the host",
        ));
    }
    Ok((host, port, parsed.request_target(), secure))
}

impl WebSocketTransport for TcpWebSocketTransport {
    fn connect(&mut self, id: WebSocketId, url: &str) -> Result<(), TransportError> {
        let (host, port, target, _) = parse_ws_url(url)?;
        let mut stream = TcpStream::connect((host.as_str(), port))
            .map_err(|error| TransportError::new(error.to_string()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let request = format!(
            "GET {target} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| TransportError::new(error.to_string()))?;

        // Read the handshake response one byte at a time so no frame bytes are
        // swallowed by a buffered reader.
        let mut response = Vec::new();
        let mut byte = [0_u8; 1];
        while !response.ends_with(b"\r\n\r\n") {
            let read = stream
                .read(&mut byte)
                .map_err(|error| TransportError::new(error.to_string()))?;
            if read == 0 {
                return Err(TransportError::new("WebSocket handshake closed early"));
            }
            response.push(byte[0]);
            if response.len() > 16 * 1024 {
                return Err(TransportError::new("WebSocket handshake too large"));
            }
        }
        let head = String::from_utf8_lossy(&response);
        if !head.contains(" 101") {
            return Err(TransportError::new("WebSocket upgrade was rejected"));
        }
        let reader = stream
            .try_clone()
            .map_err(|error| TransportError::new(error.to_string()))?;
        reader
            .set_nonblocking(true)
            .map_err(|error| TransportError::new(error.to_string()))?;
        self.writers.insert(id, stream);
        self.readers.insert(id, reader);
        self.buffers.insert(id, Vec::new());
        self.pending.push_back((id, WebSocketEvent::Open));
        Ok(())
    }

    fn send(&mut self, id: WebSocketId, message: WebSocketMessage) -> Result<(), TransportError> {
        let stream = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| TransportError::new("unknown WebSocket connection"))?;
        let payload = match message {
            WebSocketMessage::Text(text) => text.into_bytes(),
            WebSocketMessage::Binary(bytes)
            | WebSocketMessage::Ping(bytes)
            | WebSocketMessage::Pong(bytes) => bytes,
        };
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x81);
        let mask = [0x12, 0x34, 0x56, 0x78];
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        stream
            .write_all(&frame)
            .map_err(|error| TransportError::new(error.to_string()))
    }

    fn close(&mut self, id: WebSocketId, code: u16, reason: &str) -> Result<(), TransportError> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        payload.truncate(125);
        let stream = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| TransportError::new("unknown WebSocket connection"))?;
        let mut frame = vec![0x88, 0x80 | payload.len() as u8];
        let mask = [0x21, 0x43, 0x65, 0x87];
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        stream
            .write_all(&frame)
            .map_err(|error| TransportError::new(error.to_string()))?;
        Ok(())
    }

    fn poll(&mut self) -> Vec<(WebSocketId, WebSocketEvent)> {
        let ids: Vec<WebSocketId> = self.readers.keys().copied().collect();
        for id in ids {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = self.readers.get_mut(&id).unwrap().read(&mut chunk) {
                if read == 0 {
                    break;
                }
                self.buffers
                    .entry(id)
                    .or_default()
                    .extend_from_slice(&chunk[..read]);
            }
            let complete = self.parse_frames(id);
            self.pending.extend(complete);
        }
        self.pending.drain(..).collect()
    }
}

impl TcpWebSocketTransport {
    fn parse_frames(&mut self, id: WebSocketId) -> Vec<(WebSocketId, WebSocketEvent)> {
        let mut events = Vec::new();
        let buffer = self.buffers.entry(id).or_default();
        loop {
            if buffer.len() < 2 {
                break;
            }
            let opcode = buffer[0] & 0x0f;
            let masked = buffer[1] & 0x80 != 0;
            let mut length = (buffer[1] & 0x7f) as usize;
            let mut offset = 2;
            if length == 126 {
                if buffer.len() < offset + 2 {
                    break;
                }
                length = u16::from_be_bytes([buffer[offset], buffer[offset + 1]]) as usize;
                offset += 2;
            } else if length == 127 {
                if buffer.len() < offset + 8 {
                    break;
                }
                let bytes: [u8; 8] = buffer[offset..offset + 8].try_into().unwrap();
                length = u64::from_be_bytes(bytes) as usize;
                offset += 8;
            }
            let mask = if masked {
                if buffer.len() < offset + 4 {
                    break;
                }
                let mask = [
                    buffer[offset],
                    buffer[offset + 1],
                    buffer[offset + 2],
                    buffer[offset + 3],
                ];
                offset += 4;
                Some(mask)
            } else {
                None
            };
            if buffer.len() < offset + length {
                break;
            }
            let mut payload = buffer[offset..offset + length].to_vec();
            if let Some(mask) = mask {
                for (index, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[index % 4];
                }
            }
            buffer.drain(..offset + length);
            match opcode {
                1 => events.push((
                    id,
                    WebSocketEvent::Message(WebSocketMessage::Text(
                        String::from_utf8_lossy(&payload).into_owned(),
                    )),
                )),
                2 => events.push((
                    id,
                    WebSocketEvent::Message(WebSocketMessage::Binary(payload)),
                )),
                8 => {
                    let code = if payload.len() >= 2 {
                        u16::from_be_bytes([payload[0], payload[1]])
                    } else {
                        1000
                    };
                    let reason = if payload.len() >= 2 {
                        String::from_utf8_lossy(&payload[2..]).into_owned()
                    } else {
                        String::new()
                    };
                    events.push((id, WebSocketEvent::Close { code, reason }));
                }
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_html_parser::parse_document;

    fn setup() -> (Runtime, Rc<RefCell<HostState>>) {
        let mut runtime = Runtime::new();
        let document = Rc::new(RefCell::new(parse_document("<html><body></body></html>")));
        let state = crate::install(&mut runtime, document, None);
        (runtime, state)
    }

    #[test]
    fn connect_send_close_and_events_via_microtasks() {
        let (mut runtime, state) = setup();
        runtime
            .eval(
                "var ws = new WebSocket('ws://example.com/socket'); \
                 var log = []; \
                 ws.onopen = function () { log.push('open'); ws.send('hello'); }; \
                 ws.onmessage = function (e) { log.push('message:' + e.data); }; \
                 ws.onclose = function (e) { log.push('close:' + e.code); };",
            )
            .unwrap();
        assert_eq!(runtime.eval("ws.readyState").unwrap().to_number(), 0.0);

        // Drive the host-injected transport on the host side.
        {
            let id = WebSocketId(1);
            let queue = state.borrow().websocket_queue.clone();
            let mut queue = queue.lock().unwrap();
            queue.inbound.push_back((id, WebSocketEvent::Open));
        }
        assert_eq!(crate::pump_websockets(&mut runtime, &state), 1);
        runtime.run_microtasks().unwrap();
        assert_eq!(
            runtime.eval("log.join(',')").unwrap().to_string_value(),
            "open"
        );
        assert_eq!(runtime.eval("ws.readyState").unwrap().to_number(), 1.0);

        {
            let id = WebSocketId(1);
            let queue = state.borrow().websocket_queue.clone();
            let mut queue = queue.lock().unwrap();
            queue.inbound.push_back((
                id,
                WebSocketEvent::Message(WebSocketMessage::Text("hi".to_owned())),
            ));
            queue.inbound.push_back((
                id,
                WebSocketEvent::Close {
                    code: 1000,
                    reason: "done".to_owned(),
                },
            ));
        }
        assert_eq!(crate::pump_websockets(&mut runtime, &state), 2);
        runtime.run_microtasks().unwrap();
        assert_eq!(
            runtime.eval("log.join(',')").unwrap().to_string_value(),
            "open,message:hi,close:1000"
        );
        assert_eq!(runtime.eval("ws.readyState").unwrap().to_number(), 3.0);
        let queue = state.borrow().websocket_queue.clone();
        assert_eq!(
            queue.lock().unwrap().sent,
            vec![(WebSocketId(1), WebSocketMessage::Text("hello".to_owned()))]
        );
    }

    #[test]
    fn ready_state_and_handlers_round_trip() {
        let (mut runtime, _state) = setup();
        runtime
            .eval("var ws = new WebSocket('ws://x'); var fn = function(){}; ws.onmessage = fn;")
            .unwrap();
        assert!(matches!(
            runtime.eval("ws.onmessage === fn").unwrap(),
            Value::Bool(true)
        ));
        runtime.eval("ws.onmessage = null;").unwrap();
        assert!(matches!(
            runtime.eval("ws.onmessage === null").unwrap(),
            Value::Bool(true)
        ));
    }

    #[test]
    fn tcp_url_parsing_rejects_invalid_schemes() {
        assert!(parse_ws_url("http://example.com").is_err());
        assert!(parse_ws_url("wss://example.com").is_err());
        assert!(parse_ws_url("not a url").is_err());
        let (host, port, target, secure) = parse_ws_url("ws://example.com:8080/path?q=1").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8080);
        assert_eq!(target, "/path?q=1");
        assert!(!secure);
    }
}
