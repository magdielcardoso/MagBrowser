//! WebSocket connection state machine with a pluggable transport.
//!
//! [`WebSocketManager`] owns a set of connections, enforces the
//! `Connecting → Open → Closing → Closed` state machine and buffers inbound
//! events for the host to drain. It never touches a socket itself: the
//! [`WebSocketTransport`] trait is the host hook for the real network layer.
//! A [`MemoryTransport`] is provided for deterministic tests and embedders that
//! want to drive the state machine without I/O.
//!
//! ```
//! use browser_event_loop::{MemoryTransport, WebSocketEvent, WebSocketManager};
//!
//! let mut manager = WebSocketManager::new(MemoryTransport::new());
//! let id = manager.connect("wss://example.com/socket").unwrap();
//! manager.transport_mut().push_event(id, WebSocketEvent::Open);
//! assert_eq!(manager.pump(), 1);
//! assert_eq!(manager.state(id).unwrap().as_str(), "open");
//! ```

use std::collections::{HashMap, VecDeque};
use std::fmt;

/// Identifier of one managed WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WebSocketId(pub u64);

impl fmt::Display for WebSocketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ws-{}", self.0)
    }
}

/// State of a managed WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebSocketState {
    /// Handshake in progress; `send` is not allowed yet.
    Connecting,
    /// Connection established; frames may be sent.
    Open,
    /// Close frame sent; waiting for the peer or transport confirmation.
    Closing,
    /// Connection finished. Terminal.
    Closed,
}

impl WebSocketState {
    /// Stable lowercase name of the state.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Open => "open",
            Self::Closing => "closing",
            Self::Closed => "closed",
        }
    }

    /// True when application data may be sent.
    pub const fn can_send(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// An application frame sent or received over a WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketMessage {
    /// UTF-8 text frame.
    Text(String),
    /// Arbitrary binary frame.
    Binary(Vec<u8>),
    /// Protocol ping frame.
    Ping(Vec<u8>),
    /// Protocol pong frame.
    Pong(Vec<u8>),
}

/// An event reported by the transport for one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketEvent {
    /// The handshake completed.
    Open,
    /// A frame arrived.
    Message(WebSocketMessage),
    /// The connection closed, with the close code and reason.
    Close {
        /// WebSocket close status code.
        code: u16,
        /// Human-readable close reason.
        reason: String,
    },
    /// The transport failed the connection.
    Error(String),
}

/// Error reported by a [`WebSocketTransport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    /// Human-readable failure reason.
    pub message: String,
}

impl TransportError {
    /// Creates a transport error from any displayable message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

/// Host hook for the actual WebSocket transport.
///
/// The manager calls the three mutating methods in response to shell requests.
/// Inbound frames are surfaced through [`Self::poll`], which the shell calls from
/// [`WebSocketManager::pump`]. Keeping inbound events pull-based (rather than a
/// callback) makes the state machine deterministic and free of re-entrancy.
pub trait WebSocketTransport: Send {
    /// Begins opening a connection for `id`.
    fn connect(&mut self, id: WebSocketId, url: &str) -> Result<(), TransportError>;

    /// Sends one frame on `id`.
    fn send(&mut self, id: WebSocketId, message: WebSocketMessage) -> Result<(), TransportError>;

    /// Begins the closing handshake on `id`.
    fn close(&mut self, id: WebSocketId, code: u16, reason: &str) -> Result<(), TransportError>;

    /// Returns inbound events received since the previous poll.
    fn poll(&mut self) -> Vec<(WebSocketId, WebSocketEvent)>;
}

impl<T: WebSocketTransport + ?Sized> WebSocketTransport for Box<T> {
    fn connect(&mut self, id: WebSocketId, url: &str) -> Result<(), TransportError> {
        (**self).connect(id, url)
    }

    fn send(&mut self, id: WebSocketId, message: WebSocketMessage) -> Result<(), TransportError> {
        (**self).send(id, message)
    }

    fn close(&mut self, id: WebSocketId, code: u16, reason: &str) -> Result<(), TransportError> {
        (**self).close(id, code, reason)
    }

    fn poll(&mut self) -> Vec<(WebSocketId, WebSocketEvent)> {
        (**self).poll()
    }
}

/// One managed connection and its undelivered inbound events.
#[derive(Debug)]
pub struct WebSocket {
    /// Identifier of this connection.
    pub id: WebSocketId,
    /// URL the connection was opened with.
    pub url: String,
    /// Current connection state.
    pub state: WebSocketState,
    inbound: Vec<WebSocketEvent>,
}

impl WebSocket {
    /// Removes and returns all inbound events buffered for this connection.
    pub fn take_events(&mut self) -> Vec<WebSocketEvent> {
        std::mem::take(&mut self.inbound)
    }

    /// Inbound events buffered for this connection.
    pub fn events(&self) -> &[WebSocketEvent] {
        &self.inbound
    }
}

/// Errors from [`WebSocketManager`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketError {
    /// No connection exists for the identifier.
    NotFound(WebSocketId),
    /// The connection is not in a state that allows the operation.
    InvalidState {
        /// Connection identifier.
        id: WebSocketId,
        /// Current state.
        state: WebSocketState,
        /// Operation that was attempted.
        action: &'static str,
    },
    /// The transport failed the operation.
    Transport(TransportError),
}

impl From<TransportError> for WebSocketError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

impl fmt::Display for WebSocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "no WebSocket connection '{id}'"),
            Self::InvalidState { id, state, action } => {
                write!(f, "cannot {action} WebSocket '{id}' while {state:?}")
            }
            Self::Transport(error) => write!(f, "WebSocket transport error: {error}"),
        }
    }
}

impl std::error::Error for WebSocketError {}

/// Owns WebSocket connections and drives their state machine.
///
/// `T` defaults to a boxed transport so embedders can store one manager type with
/// any concrete transport. Tests usually instantiate `WebSocketManager<MemoryTransport>`.
pub struct WebSocketManager<T: WebSocketTransport = Box<dyn WebSocketTransport>> {
    transport: T,
    sockets: HashMap<WebSocketId, WebSocket>,
    next_id: u64,
}

impl<T: WebSocketTransport> WebSocketManager<T> {
    /// Creates a manager that delegates network work to `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            sockets: HashMap::new(),
            next_id: 0,
        }
    }

    /// The transport used by this manager.
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Mutable access to the transport, for pumping or test injection.
    pub const fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Opens a connection and returns its identifier.
    ///
    /// The connection starts in [`WebSocketState::Connecting`]; call
    /// [`Self::pump`] after the transport reports an `Open` event.
    pub fn connect(&mut self, url: impl Into<String>) -> Result<WebSocketId, TransportError> {
        let url = url.into();
        self.next_id += 1;
        let id = WebSocketId(self.next_id);
        self.transport.connect(id, &url)?;
        self.sockets.insert(
            id,
            WebSocket {
                id,
                url,
                state: WebSocketState::Connecting,
                inbound: Vec::new(),
            },
        );
        Ok(id)
    }

    /// Sends a frame on an open connection.
    pub fn send(
        &mut self,
        id: WebSocketId,
        message: WebSocketMessage,
    ) -> Result<(), WebSocketError> {
        let socket = self.sockets.get(&id).ok_or(WebSocketError::NotFound(id))?;
        let state = socket.state;
        if !state.can_send() {
            return Err(WebSocketError::InvalidState {
                id,
                state,
                action: "send",
            });
        }
        self.transport.send(id, message)?;
        Ok(())
    }

    /// Begins the closing handshake on a connection.
    ///
    /// Closing an already `Closing` or `Closed` connection succeeds without
    /// asking the transport again.
    pub fn close(
        &mut self,
        id: WebSocketId,
        code: u16,
        reason: &str,
    ) -> Result<(), WebSocketError> {
        let state = self
            .sockets
            .get(&id)
            .ok_or(WebSocketError::NotFound(id))?
            .state;
        match state {
            WebSocketState::Closing | WebSocketState::Closed => return Ok(()),
            WebSocketState::Connecting | WebSocketState::Open => {}
        }
        self.transport.close(id, code, reason)?;
        if let Some(socket) = self.sockets.get_mut(&id) {
            socket.state = WebSocketState::Closing;
        }
        Ok(())
    }

    /// Requests a normal close on every live connection.
    ///
    /// Continues past individual failures and returns the first one encountered.
    pub fn close_all(&mut self, code: u16, reason: &str) -> Result<(), WebSocketError> {
        let ids: Vec<WebSocketId> = self.sockets.keys().copied().collect();
        let mut first_error = None;
        for id in ids {
            if let Err(error) = self.close(id, code, reason)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Polls the transport and applies every inbound event to its connection.
    ///
    /// `Open` moves `Connecting` to `Open`; `Close` and `Error` move a connection
    /// to `Closed`; `Message` only buffers. Events are also buffered so the host
    /// can drain them with [`Self::take_events`]. Returns the number applied.
    pub fn pump(&mut self) -> usize {
        let events = self.transport.poll();
        let mut applied = 0;
        for (id, event) in events {
            let Some(socket) = self.sockets.get_mut(&id) else {
                continue;
            };
            match &event {
                WebSocketEvent::Open => {
                    if socket.state == WebSocketState::Connecting {
                        socket.state = WebSocketState::Open;
                    }
                }
                WebSocketEvent::Close { .. } | WebSocketEvent::Error(_) => {
                    socket.state = WebSocketState::Closed;
                }
                WebSocketEvent::Message(_) => {}
            }
            socket.inbound.push(event);
            applied += 1;
        }
        applied
    }

    /// State of a connection, if it exists.
    pub fn state(&self, id: WebSocketId) -> Option<WebSocketState> {
        self.sockets.get(&id).map(|socket| socket.state)
    }

    /// Read-only access to a connection.
    pub fn socket(&self, id: WebSocketId) -> Option<&WebSocket> {
        self.sockets.get(&id)
    }

    /// Mutable access to a connection, for draining its event buffer.
    pub fn socket_mut(&mut self, id: WebSocketId) -> Option<&mut WebSocket> {
        self.sockets.get_mut(&id)
    }

    /// Drains inbound events for one connection.
    pub fn take_events(&mut self, id: WebSocketId) -> Option<Vec<WebSocketEvent>> {
        self.socket_mut(id).map(WebSocket::take_events)
    }

    /// Identifiers of all live connections, in ascending order.
    pub fn ids(&self) -> std::vec::IntoIter<WebSocketId> {
        let mut ids: Vec<WebSocketId> = self.sockets.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
    }

    /// Number of live connections.
    pub fn len(&self) -> usize {
        self.sockets.len()
    }

    /// True when no connections are being managed.
    pub fn is_empty(&self) -> bool {
        self.sockets.is_empty()
    }
}

impl fmt::Debug for WebSocketManager<Box<dyn WebSocketTransport>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocketManager")
            .field("sockets", &self.sockets.len())
            .finish()
    }
}

/// In-memory transport used for tests and deterministic hosts.
///
/// Outbound operations are recorded for inspection and inbound events are queued
/// with [`Self::push_event`] until the manager polls them.
#[derive(Debug, Default)]
pub struct MemoryTransport {
    connections: Vec<(WebSocketId, String)>,
    sent: Vec<(WebSocketId, WebSocketMessage)>,
    closed: Vec<(WebSocketId, u16, String)>,
    pending: VecDeque<(WebSocketId, WebSocketEvent)>,
    fail_connect: bool,
    fail_send: bool,
}

impl MemoryTransport {
    /// Creates an empty transport.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues an inbound event for a connection.
    pub fn push_event(&mut self, id: WebSocketId, event: WebSocketEvent) {
        self.pending.push_back((id, event));
    }

    /// Connections opened through this transport, in order.
    pub fn connections(&self) -> &[(WebSocketId, String)] {
        &self.connections
    }

    /// Frames sent through this transport, in order.
    pub fn sent(&self) -> &[(WebSocketId, WebSocketMessage)] {
        &self.sent
    }

    /// Close requests sent through this transport, in order.
    pub fn closed(&self) -> &[(WebSocketId, u16, String)] {
        &self.closed
    }

    /// Makes subsequent `connect` calls fail until toggled back.
    pub fn set_fail_connect(&mut self, fail: bool) {
        self.fail_connect = fail;
    }

    /// Makes subsequent `send` calls fail until toggled back.
    pub fn set_fail_send(&mut self, fail: bool) {
        self.fail_send = fail;
    }
}

impl WebSocketTransport for MemoryTransport {
    fn connect(&mut self, id: WebSocketId, url: &str) -> Result<(), TransportError> {
        if self.fail_connect {
            return Err(TransportError::new("simulated connect failure"));
        }
        self.connections.push((id, url.to_string()));
        Ok(())
    }

    fn send(&mut self, id: WebSocketId, message: WebSocketMessage) -> Result<(), TransportError> {
        if self.fail_send {
            return Err(TransportError::new("simulated send failure"));
        }
        self.sent.push((id, message));
        Ok(())
    }

    fn close(&mut self, id: WebSocketId, code: u16, reason: &str) -> Result<(), TransportError> {
        self.closed.push((id, code, reason.to_string()));
        Ok(())
    }

    fn poll(&mut self) -> Vec<(WebSocketId, WebSocketEvent)> {
        self.pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_starts_connecting_and_records_url() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let id = manager.connect("wss://example.com/s").unwrap();
        assert_eq!(id, WebSocketId(1));
        assert_eq!(manager.state(id), Some(WebSocketState::Connecting));
        assert_eq!(manager.len(), 1);
        assert_eq!(
            manager.transport().connections(),
            [(id, "wss://example.com/s".to_string())]
        );
    }

    #[test]
    fn open_event_moves_to_open_and_is_buffered() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let id = manager.connect("ws://x").unwrap();
        manager.transport_mut().push_event(id, WebSocketEvent::Open);
        assert_eq!(manager.pump(), 1);
        assert_eq!(manager.state(id), Some(WebSocketState::Open));
        assert_eq!(manager.take_events(id), Some(vec![WebSocketEvent::Open]));
        assert!(manager.socket(id).unwrap().events().is_empty());
    }

    #[test]
    fn send_requires_an_open_connection() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let id = manager.connect("ws://x").unwrap();
        let message = WebSocketMessage::Text("hi".to_string());
        assert_eq!(
            manager.send(id, message.clone()),
            Err(WebSocketError::InvalidState {
                id,
                state: WebSocketState::Connecting,
                action: "send",
            })
        );
        manager.transport_mut().push_event(id, WebSocketEvent::Open);
        manager.pump();
        manager.send(id, message.clone()).unwrap();
        assert_eq!(manager.transport().sent(), [(id, message)]);
    }

    #[test]
    fn message_events_are_delivered_without_changing_state() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let id = manager.connect("ws://x").unwrap();
        manager.transport_mut().push_event(id, WebSocketEvent::Open);
        manager.transport_mut().push_event(
            id,
            WebSocketEvent::Message(WebSocketMessage::Binary(vec![1, 2, 3])),
        );
        assert_eq!(manager.pump(), 2);
        assert_eq!(manager.state(id), Some(WebSocketState::Open));
        assert_eq!(
            manager.take_events(id),
            Some(vec![
                WebSocketEvent::Open,
                WebSocketEvent::Message(WebSocketMessage::Binary(vec![1, 2, 3])),
            ])
        );
    }

    #[test]
    fn close_state_machine_and_idempotence() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let id = manager.connect("ws://x").unwrap();
        manager.transport_mut().push_event(id, WebSocketEvent::Open);
        manager.pump();
        manager.close(id, 1000, "done").unwrap();
        assert_eq!(manager.state(id), Some(WebSocketState::Closing));
        assert_eq!(
            manager.send(id, WebSocketMessage::Text("x".into())),
            Err(WebSocketError::InvalidState {
                id,
                state: WebSocketState::Closing,
                action: "send",
            })
        );
        // A second close does not hit the transport again.
        manager.close(id, 1000, "done").unwrap();
        assert_eq!(manager.transport().closed().len(), 1);
        manager.transport_mut().push_event(
            id,
            WebSocketEvent::Close {
                code: 1000,
                reason: "done".into(),
            },
        );
        manager.pump();
        assert_eq!(manager.state(id), Some(WebSocketState::Closed));
        manager.close(id, 1000, "done").unwrap();
        assert_eq!(manager.transport().closed().len(), 1);
    }

    #[test]
    fn error_event_closes_the_connection() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let id = manager.connect("ws://x").unwrap();
        manager
            .transport_mut()
            .push_event(id, WebSocketEvent::Error("boom".to_string()));
        manager.pump();
        assert_eq!(manager.state(id), Some(WebSocketState::Closed));
        assert_eq!(
            manager.take_events(id),
            Some(vec![WebSocketEvent::Error("boom".to_string())])
        );
    }

    #[test]
    fn unknown_ids_are_reported() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let id = WebSocketId(99);
        assert_eq!(manager.state(id), None);
        assert_eq!(manager.take_events(id), None);
        assert_eq!(
            manager.send(id, WebSocketMessage::Text("x".into())),
            Err(WebSocketError::NotFound(id))
        );
        assert_eq!(
            manager.close(id, 1000, ""),
            Err(WebSocketError::NotFound(id))
        );
    }

    #[test]
    fn events_for_unknown_connections_are_ignored() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        manager
            .transport_mut()
            .push_event(WebSocketId(404), WebSocketEvent::Open);
        assert_eq!(manager.pump(), 0);
        assert!(manager.is_empty());
    }

    #[test]
    fn transport_failures_are_propagated() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        manager.transport_mut().set_fail_connect(true);
        assert_eq!(
            manager.connect("ws://x"),
            Err(TransportError::new("simulated connect failure"))
        );
        assert!(manager.is_empty());

        manager.transport_mut().set_fail_connect(false);
        let id = manager.connect("ws://x").unwrap();
        manager.transport_mut().push_event(id, WebSocketEvent::Open);
        manager.pump();
        manager.transport_mut().set_fail_send(true);
        assert_eq!(
            manager.send(id, WebSocketMessage::Text("x".into())),
            Err(WebSocketError::Transport(TransportError::new(
                "simulated send failure"
            )))
        );
    }

    #[test]
    fn events_are_routed_per_connection() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let a = manager.connect("ws://a").unwrap();
        let b = manager.connect("ws://b").unwrap();
        manager.transport_mut().push_event(a, WebSocketEvent::Open);
        manager.transport_mut().push_event(
            b,
            WebSocketEvent::Message(WebSocketMessage::Text("for b".into())),
        );
        manager.pump();
        assert_eq!(manager.state(a), Some(WebSocketState::Open));
        assert_eq!(manager.state(b), Some(WebSocketState::Connecting));
        assert_eq!(manager.take_events(a), Some(vec![WebSocketEvent::Open]));
        assert_eq!(
            manager.take_events(b),
            Some(vec![WebSocketEvent::Message(WebSocketMessage::Text(
                "for b".into()
            ))])
        );
    }

    #[test]
    fn close_all_closes_every_connection() {
        let mut manager = WebSocketManager::new(MemoryTransport::new());
        let a = manager.connect("ws://a").unwrap();
        let b = manager.connect("ws://b").unwrap();
        manager.close_all(1001, "shutdown").unwrap();
        assert_eq!(manager.state(a), Some(WebSocketState::Closing));
        assert_eq!(manager.state(b), Some(WebSocketState::Closing));
        assert_eq!(manager.transport().closed().len(), 2);
        let ids: Vec<WebSocketId> = manager.ids().collect();
        assert_eq!(ids, [a, b]);
    }

    #[test]
    fn boxed_transport_is_supported() {
        let mut transport = MemoryTransport::new();
        // The first connection is allocated id 1; queue its open event up front
        // because the boxed trait object exposes only the transport interface.
        transport.push_event(WebSocketId(1), WebSocketEvent::Open);
        let mut manager: WebSocketManager<Box<dyn WebSocketTransport>> =
            WebSocketManager::new(Box::new(transport));
        let id = manager.connect("ws://x").unwrap();
        assert_eq!(id, WebSocketId(1));
        assert_eq!(manager.pump(), 1);
        assert_eq!(manager.state(id), Some(WebSocketState::Open));
    }
}
