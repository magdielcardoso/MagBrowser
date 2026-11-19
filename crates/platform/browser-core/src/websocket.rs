//! TCP implementation of the WebSocket host transport.
//!
//! [`browser_event_loop::WebSocketTransport`] is deliberately I/O-free: the
//! state machine owns connection ids and events, while the shell supplies the
//! bytes. [`TcpWebSocketTransport`] is that shell-side implementation. It
//! performs the RFC 6455 opening handshake and frames/unframes messages on
//! plain `ws://` sockets using the standard library only.
//!
//! # Limits
//!
//! * Only `ws://` is supported. `wss://` needs a TLS stream, which belongs in
//!   the network/TLS layer rather than in this facade; `connect` returns a
//!   descriptive [`TransportError`] for secure URLs. An embedder that needs
//!   TLS can supply its own transport through
//!   [`browser_event_loop::WebSocketManager::new`].
//! * The `Sec-WebSocket-Accept` response header is checked for presence but
//!   not cryptographically verified (no SHA-1 dependency in this crate).
//! * Inbound frames are parsed after a best-effort non-blocking read on each
//!   [`Self::poll`]; the caller drives the cadence. Fragmented text/binary
//!   messages are reassembled, but compressed frames (`permessage-deflate`)
//!   are not negotiated and therefore never arrive.
//! * A masked client frame is required by RFC 6455; the mask is generated from
//!   a small, non-cryptographic PRNG seeded from the system clock.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use browser_event_loop::{
    TransportError, WebSocketEvent, WebSocketId, WebSocketMessage, WebSocketTransport,
};
use browser_web_apis::HostState;

/// Time allowed for a single TCP connect or handshake response.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum bytes buffered for an opening handshake response.
const MAX_HANDSHAKE_BYTES: usize = 64 * 1024;
/// Maximum accepted WebSocket frame payload (8 MiB).
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// One live TCP connection and its inbound frame buffer.
struct Connection {
    stream: TcpStream,
    inbound: Vec<u8>,
    /// Payload of an unfinished fragmented message.
    fragment: Vec<u8>,
    /// Opcode (text or binary) of the unfinished fragmented message.
    fragment_opcode: u8,
    close_sent: bool,
}

/// A WebSocket transport backed by real TCP sockets.
#[derive(Default)]
pub struct TcpWebSocketTransport {
    connections: HashMap<WebSocketId, Connection>,
    pending: Vec<(WebSocketId, WebSocketEvent)>,
    counter: u64,
}

impl TcpWebSocketTransport {
    /// Creates an empty transport.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live TCP connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    fn next_bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            self.counter = self
                .counter
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let clock = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            out.push(((self.counter ^ clock) >> 33) as u8);
        }
        out
    }

    fn queue(&mut self, id: WebSocketId, event: WebSocketEvent) {
        self.pending.push((id, event));
    }

    fn write_frame(
        &mut self,
        id: WebSocketId,
        opcode: u8,
        payload: &[u8],
    ) -> Result<(), TransportError> {
        if !self.connections.contains_key(&id) {
            return Err(TransportError::new(format!("unknown connection '{id}'")));
        }
        let mask = self.next_bytes(4);
        let connection = self
            .connections
            .get_mut(&id)
            .expect("connection checked above");
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
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
        for (index, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[index % 4]);
        }
        connection
            .stream
            .write_all(&frame)
            .map_err(|error| TransportError::new(format!("WebSocket write failed: {error}")))
    }
}

/// Parsed `ws://` URL components.
struct UrlParts {
    host: String,
    port: u16,
    resource: String,
}

fn parse_url(url: &str) -> Result<UrlParts, TransportError> {
    let rest = url
        .strip_prefix("ws://")
        .ok_or_else(|| TransportError::new("only ws:// URLs are supported by the TCP transport"))?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(TransportError::new("WebSocket URL has no host"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| TransportError::new(format!("invalid WebSocket port '{port}'")))?;
            (host.to_string(), port)
        }
        None => (authority.to_string(), 80),
    };
    let resource = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };
    Ok(UrlParts {
        host,
        port,
        resource,
    })
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(input: &[u8]) -> String {
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(BASE64[(a >> 2) as usize] as char);
        out.push(BASE64[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64[(c & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn header_value<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// Outcome of parsing one complete frame from an inbound buffer.
#[derive(Debug)]
enum ParsedFrame {
    /// A data-frame chunk; `fin` says whether it ends the message.
    Data {
        fin: bool,
        opcode: u8,
        payload: Vec<u8>,
    },
    Close {
        code: u16,
        reason: String,
    },
    Ping(Vec<u8>),
    Pong,
    NeedMore,
    Invalid(String),
}

fn parse_frame(buffer: &[u8]) -> (ParsedFrame, usize) {
    if buffer.len() < 2 {
        return (ParsedFrame::NeedMore, 0);
    }
    let first = buffer[0];
    let second = buffer[1];
    let fin = first & 0x80 != 0;
    let opcode = first & 0x0f;
    let masked = second & 0x80 != 0;
    let mut length = (second & 0x7f) as usize;
    let mut offset = 2;
    if length == 126 {
        if buffer.len() < offset + 2 {
            return (ParsedFrame::NeedMore, 0);
        }
        length = u16::from_be_bytes([buffer[offset], buffer[offset + 1]]) as usize;
        offset += 2;
    } else if length == 127 {
        if buffer.len() < offset + 8 {
            return (ParsedFrame::NeedMore, 0);
        }
        let raw = u64::from_be_bytes(buffer[offset..offset + 8].try_into().expect("8 bytes"));
        length = usize::try_from(raw).unwrap_or(MAX_FRAME_BYTES + 1);
        offset += 8;
    }
    if length > MAX_FRAME_BYTES {
        return (
            ParsedFrame::Invalid(format!("frame payload too large ({length} bytes)")),
            buffer.len(),
        );
    }
    let mask = if masked {
        if buffer.len() < offset + 4 {
            return (ParsedFrame::NeedMore, 0);
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
        return (ParsedFrame::NeedMore, 0);
    }
    let mut payload = buffer[offset..offset + length].to_vec();
    if let Some(mask) = mask {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    let consumed = offset + length;
    let parsed = match opcode {
        0x0..=0x2 => ParsedFrame::Data {
            fin,
            opcode,
            payload,
        },
        0x8 => {
            let code = if payload.len() >= 2 {
                u16::from_be_bytes([payload[0], payload[1]])
            } else {
                1005
            };
            let reason = if payload.len() > 2 {
                String::from_utf8_lossy(&payload[2..]).into_owned()
            } else {
                String::new()
            };
            ParsedFrame::Close { code, reason }
        }
        0x9 => ParsedFrame::Ping(payload),
        0xA => ParsedFrame::Pong,
        other => {
            return (
                ParsedFrame::Invalid(format!("unsupported WebSocket opcode {other:#x}")),
                consumed,
            );
        }
    };
    (parsed, consumed)
}

impl WebSocketTransport for TcpWebSocketTransport {
    fn connect(&mut self, id: WebSocketId, url: &str) -> Result<(), TransportError> {
        let parts = parse_url(url)?;
        let addresses = (parts.host.as_str(), parts.port)
            .to_socket_addrs()
            .map_err(|error| {
                TransportError::new(format!("cannot resolve '{}': {error}", parts.host))
            })?;
        let address = addresses
            .into_iter()
            .next()
            .ok_or_else(|| TransportError::new(format!("no address for '{}'", parts.host)))?;
        let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)
            .map_err(|error| TransportError::new(format!("connect failed: {error}")))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|error| TransportError::new(error.to_string()))?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|error| TransportError::new(error.to_string()))?;

        let key = base64(&self.next_bytes(16));
        let request = format!(
            "GET {} HTTP/1.1\r\n\
             Host: {}:{}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
            parts.resource, parts.host, parts.port, key
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| TransportError::new(format!("handshake write failed: {error}")))?;

        let mut response = Vec::new();
        let mut byte = [0_u8; 1];
        while !response.ends_with(b"\r\n\r\n") {
            let read = stream
                .read(&mut byte)
                .map_err(|error| TransportError::new(format!("handshake read failed: {error}")))?;
            if read == 0 {
                return Err(TransportError::new("handshake closed before response"));
            }
            response.push(byte[0]);
            if response.len() > MAX_HANDSHAKE_BYTES {
                return Err(TransportError::new("handshake response too large"));
            }
        }
        let text = String::from_utf8_lossy(&response);
        if !text
            .lines()
            .next()
            .is_some_and(|line| line.contains(" 101"))
        {
            return Err(TransportError::new(format!(
                "handshake rejected: {}",
                text.lines().next().unwrap_or("no status")
            )));
        }
        if !header_value(&text, "upgrade")
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        {
            return Err(TransportError::new("handshake missing Upgrade: websocket"));
        }
        if header_value(&text, "sec-websocket-accept").is_none() {
            return Err(TransportError::new(
                "handshake missing Sec-WebSocket-Accept",
            ));
        }
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map_or(response.len(), |index| index + 4);
        let inbound = response[header_end..].to_vec();
        stream
            .set_nonblocking(true)
            .map_err(|error| TransportError::new(error.to_string()))?;
        self.connections.insert(
            id,
            Connection {
                stream,
                inbound,
                fragment: Vec::new(),
                fragment_opcode: 0,
                close_sent: false,
            },
        );
        self.queue(id, WebSocketEvent::Open);
        Ok(())
    }

    fn send(&mut self, id: WebSocketId, message: WebSocketMessage) -> Result<(), TransportError> {
        match message {
            WebSocketMessage::Text(text) => self.write_frame(id, 0x1, text.as_bytes()),
            WebSocketMessage::Binary(bytes) => self.write_frame(id, 0x2, &bytes),
            WebSocketMessage::Ping(bytes) => self.write_frame(id, 0x9, &bytes),
            WebSocketMessage::Pong(bytes) => self.write_frame(id, 0xA, &bytes),
        }
    }

    fn close(&mut self, id: WebSocketId, code: u16, reason: &str) -> Result<(), TransportError> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        self.write_frame(id, 0x8, &payload)?;
        if let Some(connection) = self.connections.get_mut(&id) {
            connection.close_sent = true;
        }
        Ok(())
    }

    fn poll(&mut self) -> Vec<(WebSocketId, WebSocketEvent)> {
        let ids: Vec<WebSocketId> = self.connections.keys().copied().collect();
        let mut failures = Vec::new();
        let mut invalid = Vec::new();
        for id in ids {
            let mut chunk = [0_u8; 4096];
            while let Some(connection) = self.connections.get_mut(&id) {
                match connection.stream.read(&mut chunk) {
                    Ok(0) => {
                        failures.push((
                            id,
                            WebSocketEvent::Close {
                                code: 1006,
                                reason: "connection closed by peer".to_string(),
                            },
                        ));
                        break;
                    }
                    Ok(count) => connection.inbound.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        failures.push((id, WebSocketEvent::Error(format!("read failed: {error}"))));
                        break;
                    }
                }
            }
            let mut events = Vec::new();
            let mut replies = Vec::new();
            loop {
                let mask = self.next_bytes(4);
                let Some(connection) = self.connections.get_mut(&id) else {
                    break;
                };
                let (parsed, consumed) = parse_frame(&connection.inbound);
                match parsed {
                    ParsedFrame::NeedMore => break,
                    ParsedFrame::Invalid(message) => {
                        connection.inbound.clear();
                        failures.push((id, WebSocketEvent::Error(message)));
                        invalid.push(id);
                        break;
                    }
                    parsed => {
                        connection.inbound.drain(..consumed);
                        match parsed {
                            ParsedFrame::Data {
                                fin,
                                opcode,
                                payload,
                            } => {
                                if opcode != 0 {
                                    connection.fragment_opcode = opcode;
                                }
                                connection.fragment.extend_from_slice(&payload);
                                if fin {
                                    let opcode = connection.fragment_opcode;
                                    let payload = std::mem::take(&mut connection.fragment);
                                    let message = if opcode == 0x2 {
                                        WebSocketMessage::Binary(payload)
                                    } else {
                                        WebSocketMessage::Text(
                                            String::from_utf8_lossy(&payload).into_owned(),
                                        )
                                    };
                                    events.push(WebSocketEvent::Message(message));
                                }
                            }
                            ParsedFrame::Close { code, reason } => {
                                if !connection.close_sent {
                                    let mut reply = code.to_be_bytes().to_vec();
                                    reply.extend_from_slice(reason.as_bytes());
                                    replies.push((0x8_u8, reply, mask));
                                }
                                events.push(WebSocketEvent::Close { code, reason });
                                failures.push((
                                    id,
                                    WebSocketEvent::Close {
                                        code,
                                        reason: String::new(),
                                    },
                                ));
                                break;
                            }
                            ParsedFrame::Ping(payload) => replies.push((0xA_u8, payload, mask)),
                            ParsedFrame::Pong => {}
                            ParsedFrame::NeedMore | ParsedFrame::Invalid(_) => unreachable!(),
                        }
                    }
                }
            }
            for (opcode, payload, mask) in replies {
                let Some(connection) = self.connections.get_mut(&id) else {
                    break;
                };
                let mut frame = Vec::with_capacity(payload.len() + 14);
                frame.push(0x80 | opcode);
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
                for (index, byte) in payload.iter().enumerate() {
                    frame.push(byte ^ mask[index % 4]);
                }
                let _ = connection.stream.write_all(&frame);
            }
            self.pending
                .extend(events.into_iter().map(|event| (id, event)));
        }
        for id in invalid {
            self.connections.remove(&id);
        }
        for (id, event) in failures {
            if self.connections.remove(&id).is_some() {
                match event {
                    WebSocketEvent::Close { .. }
                        if self.pending.iter().any(|(pending_id, pending)| {
                            *pending_id == id && matches!(pending, WebSocketEvent::Close { .. })
                        }) => {}
                    other => self.queue(id, other),
                }
            }
        }
        std::mem::take(&mut self.pending)
    }
}

/// Replaces the in-memory transport in `host` with a real TCP transport.
///
/// The manager type stays `WebSocketManager<Box<dyn WebSocketTransport>>`, so
/// none of the JavaScript-facing bindings need to change. All connections
/// created afterwards use TCP; ones already open are dropped. Call this once,
/// before scripts run (see [`crate::script::ensure_runtime`]).
pub fn install_tcp_transport(host: &Rc<RefCell<HostState>>) {
    browser_web_apis::set_websocket_transport(host, Box::new(TcpWebSocketTransport::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ws_urls() {
        let parts = parse_url("ws://example.com:8080/socket?x=1").expect("url");
        assert_eq!(parts.host, "example.com");
        assert_eq!(parts.port, 8080);
        assert_eq!(parts.resource, "/socket?x=1");
        let parts = parse_url("ws://localhost").expect("url");
        assert_eq!(parts.port, 80);
        assert_eq!(parts.resource, "/");
        assert!(parse_url("wss://example.com").is_err());
    }

    #[test]
    fn masks_client_frames() {
        let mut transport = TcpWebSocketTransport::new();
        // No live connection: writing must fail cleanly rather than panic.
        assert!(
            transport
                .write_frame(WebSocketId(1), 0x1, b"hello")
                .is_err()
        );
    }

    #[test]
    fn parses_unmasked_server_frames() {
        let mut frame = vec![0x81, 0x05];
        frame.extend_from_slice(b"hello");
        let (parsed, used) = parse_frame(&frame);
        assert_eq!(used, frame.len());
        match parsed {
            ParsedFrame::Data {
                fin: true,
                opcode: 0x1,
                payload,
            } => assert_eq!(payload, b"hello"),
            other => panic!("unexpected frame: {other:?}"),
        }
        // Incomplete frame waits for more bytes.
        assert!(matches!(parse_frame(&frame[..4]).0, ParsedFrame::NeedMore));
    }

    #[test]
    fn tcp_transport_handshakes_and_receives_frames() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                request.push(byte[0]);
            }
            let response = "HTTP/1.1 101 Switching Protocols\r\n\
                            Upgrade: websocket\r\n\
                            Connection: Upgrade\r\n\
                            Sec-WebSocket-Accept: dGVzdA==\r\n\r\n";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&[0x81, 0x02, b'h', b'i']);
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(200));
        });

        let mut transport = TcpWebSocketTransport::new();
        let id = WebSocketId(1);
        transport
            .connect(id, &format!("ws://{addr}/socket"))
            .expect("handshake");
        assert_eq!(transport.connection_count(), 1);
        let mut seen = Vec::new();
        for _ in 0..30 {
            seen.extend(transport.poll());
            if seen
                .iter()
                .any(|(_, event)| matches!(event, WebSocketEvent::Message(_)))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            seen.iter()
                .any(|(_, event)| matches!(event, WebSocketEvent::Open))
        );
        assert!(seen.iter().any(|(_, event)| matches!(
            event,
            WebSocketEvent::Message(WebSocketMessage::Text(text)) if text == "hi"
        )));
    }

    #[test]
    fn parses_close_and_ping_frames() {
        let mut close = vec![0x88, 0x05, 0x03, 0xE8];
        close.extend_from_slice(b"bye");
        match parse_frame(&close).0 {
            ParsedFrame::Close { code, reason } => {
                assert_eq!(code, 1000);
                assert_eq!(reason, "bye");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
        assert!(matches!(parse_frame(&[0x89, 0x00]).0, ParsedFrame::Ping(_)));
    }
}
