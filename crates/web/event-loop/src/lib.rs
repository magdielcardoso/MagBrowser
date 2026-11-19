//! Task and microtask queues with a cooperative scheduler.
//!
//! This crate implements the scheduling subset of the HTML event loop used by the
//! engine:
//!
//! * [`TaskSource`] classifies work so user input and timers take priority over
//!   network and rendering work.
//! * [`TaskQueue`] holds FIFO task queues (one per source) and a FIFO microtask
//!   queue.
//! * [`Scheduler`] adds a `std::time::Instant`-based timer heap, drains
//!   microtasks at every checkpoint and reports how much work it ran.
//! * [`ServiceWorkerRegistry`] models worker registration and fetch interception,
//!   delegating script execution to a host-supplied callback.
//! * [`WebSocketManager`] drives WebSocket connections through a state machine
//!   with a pluggable [`WebSocketTransport`] and an in-memory test transport.
//!
//! Time is always supplied by the caller ([`Scheduler::run_due_timers`] and
//! [`Scheduler::run_until_idle`]), so hosts can use a real clock while tests use a
//! synthetic one without sleeping.
//!
//! ```
//! use browser_event_loop::{Scheduler, TaskSource};
//! use std::sync::{Arc, Mutex};
//! use std::time::Instant;
//!
//! let mut scheduler = Scheduler::new();
//! let order = Arc::new(Mutex::new(Vec::new()));
//! let sink = Arc::clone(&order);
//! scheduler.queue_task(
//!     TaskSource::UserInteraction,
//!     Box::new(move || sink.lock().unwrap().push("click")),
//! );
//! scheduler.queue_microtask({
//!     let order = Arc::clone(&order);
//!     Box::new(move || order.lock().unwrap().push("microtask"))
//! });
//! let report = scheduler.run_until_idle(Instant::now());
//! assert_eq!(report.tasks, 1);
//! assert_eq!(*order.lock().unwrap(), ["microtask", "click"]);
//! ```

#![forbid(unsafe_code)]

mod microtasks;
mod scheduler;
mod service_worker;
mod tasks;
mod websocket;

pub use microtasks::{MAX_MICROTASK_BATCH, MicrotaskQueue, drain_bounded};
pub use scheduler::{DEFAULT_MICROTASK_LIMIT, RunReport, Scheduler, TimerId};
pub use service_worker::{
    Registration, RegistrationId, Response, ServiceWorkerEvent, ServiceWorkerRegistry,
    ServiceWorkerResponse, WorkerError, WorkerEventKind, WorkerEventResult, WorkerScriptHandler,
    WorkerState,
};
pub use tasks::{Task, TaskQueue, TaskSource};
pub use websocket::{
    MemoryTransport, TransportError, WebSocket, WebSocketError, WebSocketEvent, WebSocketId,
    WebSocketManager, WebSocketMessage, WebSocketState, WebSocketTransport,
};
