//! Bounded, thread-safe message channel.
//!
//! [`Channel`] is a multi-producer, multi-consumer queue built from a mutex and
//! two condition variables (no `unsafe`, no external crates). Capacity is a
//! hard upper bound: [`Channel::send`] blocks while the queue is full, and
//! [`Channel::try_send`] fails fast with [`ChannelError::Full`].
//!
//! Closing a channel wakes every waiter. Messages already queued can still be
//! received; once the queue is drained, receive operations report
//! [`ChannelError::Disconnected`].

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::messages::Message;

/// Errors returned by channel operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    /// The bounded queue is full and the operation was non-blocking.
    Full,
    /// The queue is empty and the operation was non-blocking.
    Empty,
    /// The channel is closed and the queue is drained.
    Disconnected,
    /// A timed receive expired before a message arrived.
    Timeout,
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Full => "channel is full",
            Self::Empty => "channel is empty",
            Self::Disconnected => "channel is closed",
            Self::Timeout => "timed out waiting for a message",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ChannelError {}

#[derive(Debug)]
struct State {
    messages: VecDeque<Message>,
    capacity: usize,
    closed: bool,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    not_empty: Condvar,
    not_full: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(condvar: &Condvar, guard: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        condvar.wait(guard).unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_timeout<'a>(
        condvar: &Condvar,
        guard: MutexGuard<'a, State>,
        timeout: Duration,
    ) -> (MutexGuard<'a, State>, bool) {
        let (guard, result) = condvar
            .wait_timeout(guard, timeout)
            .unwrap_or_else(PoisonError::into_inner);
        (guard, result.timed_out())
    }
}

/// A cloneable handle to a bounded message queue.
///
/// Cloning a `Channel` does not create a second queue: all clones share the same
/// buffer, capacity and closed flag. The channel is [`Send`] and [`Sync`].
#[derive(Debug, Clone)]
pub struct Channel {
    shared: Arc<Shared>,
}

impl Channel {
    /// Creates a channel that holds at most `capacity` messages.
    ///
    /// A capacity of zero is treated as one, so the channel can always make
    /// progress.
    pub fn bounded(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    messages: VecDeque::with_capacity(capacity.min(1024)),
                    capacity,
                    closed: false,
                }),
                not_empty: Condvar::new(),
                not_full: Condvar::new(),
            }),
        }
    }

    /// Creates a channel with no practical bound.
    pub fn unbounded() -> Self {
        Self::bounded(usize::MAX)
    }

    /// Maximum number of queued messages.
    pub fn capacity(&self) -> usize {
        self.shared.lock().capacity
    }

    /// Number of messages currently queued.
    pub fn len(&self) -> usize {
        self.shared.lock().messages.len()
    }

    /// Whether the queue is currently empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether [`Self::close`] has been called.
    pub fn is_closed(&self) -> bool {
        self.shared.lock().closed
    }

    /// Sends a message, blocking while the queue is full.
    ///
    /// Returns [`ChannelError::Disconnected`] if the channel is closed (possibly
    /// while waiting for space).
    pub fn send(&self, message: Message) -> Result<(), ChannelError> {
        let mut state = self.shared.lock();
        while state.messages.len() >= state.capacity {
            if state.closed {
                return Err(ChannelError::Disconnected);
            }
            state = Shared::wait(&self.shared.not_full, state);
        }
        if state.closed {
            return Err(ChannelError::Disconnected);
        }
        state.messages.push_back(message);
        drop(state);
        self.shared.not_empty.notify_one();
        Ok(())
    }

    /// Sends a message without blocking.
    pub fn try_send(&self, message: Message) -> Result<(), ChannelError> {
        let mut state = self.shared.lock();
        if state.closed {
            return Err(ChannelError::Disconnected);
        }
        if state.messages.len() >= state.capacity {
            return Err(ChannelError::Full);
        }
        state.messages.push_back(message);
        drop(state);
        self.shared.not_empty.notify_one();
        Ok(())
    }

    /// Receives a message, blocking until one is available or the channel is
    /// closed and drained.
    pub fn recv(&self) -> Result<Message, ChannelError> {
        let mut state = self.shared.lock();
        loop {
            if let Some(message) = state.messages.pop_front() {
                drop(state);
                self.shared.not_full.notify_one();
                return Ok(message);
            }
            if state.closed {
                return Err(ChannelError::Disconnected);
            }
            state = Shared::wait(&self.shared.not_empty, state);
        }
    }

    /// Receives a message if one is already queued.
    pub fn try_recv(&self) -> Result<Message, ChannelError> {
        let mut state = self.shared.lock();
        if let Some(message) = state.messages.pop_front() {
            drop(state);
            self.shared.not_full.notify_one();
            return Ok(message);
        }
        if state.closed {
            Err(ChannelError::Disconnected)
        } else {
            Err(ChannelError::Empty)
        }
    }

    /// Receives a message, waiting at most `timeout`.
    ///
    /// Queued messages are always preferred over the timeout, and a closed
    /// channel still yields any queued messages before reporting
    /// [`ChannelError::Disconnected`].
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Message, ChannelError> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = self.shared.lock();
        loop {
            if let Some(message) = state.messages.pop_front() {
                drop(state);
                self.shared.not_full.notify_one();
                return Ok(message);
            }
            if state.closed {
                return Err(ChannelError::Disconnected);
            }
            let remaining = match deadline {
                Some(deadline) => deadline.saturating_duration_since(Instant::now()),
                None => timeout,
            };
            if remaining.is_zero() {
                return Err(ChannelError::Timeout);
            }
            let (guard, timed_out) = Shared::wait_timeout(&self.shared.not_empty, state, remaining);
            state = guard;
            if timed_out && state.messages.is_empty() && !state.closed {
                return Err(ChannelError::Timeout);
            }
        }
    }

    /// Closes the channel, waking all blocked senders and receivers.
    ///
    /// The call is idempotent. Already-queued messages remain available; new
    /// sends fail with [`ChannelError::Disconnected`].
    pub fn close(&self) {
        let mut state = self.shared.lock();
        state.closed = true;
        drop(state);
        self.shared.not_empty.notify_all();
        self.shared.not_full.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{Message, MessageKind};
    use std::thread;
    use std::time::Instant;

    fn message(id: u64) -> Message {
        Message::new(id, MessageKind::Input, "test")
    }

    #[test]
    fn preserves_fifo_order() {
        let channel = Channel::bounded(4);
        for id in 0..4 {
            channel.send(message(id)).unwrap();
        }
        assert_eq!(channel.len(), 4);
        for id in 0..4 {
            assert_eq!(channel.recv().unwrap().id.0, id);
        }
        assert!(channel.is_empty());
    }

    #[test]
    fn try_recv_reports_empty_then_disconnected() {
        let channel = Channel::bounded(1);
        assert_eq!(channel.try_recv(), Err(ChannelError::Empty));
        channel.send(message(1)).unwrap();
        assert_eq!(channel.try_recv().unwrap().id.0, 1);
        channel.close();
        assert_eq!(channel.try_recv(), Err(ChannelError::Disconnected));
    }

    #[test]
    fn bounded_capacity_rejects_non_blocking_sends() {
        let channel = Channel::bounded(1);
        channel.try_send(message(1)).unwrap();
        assert_eq!(channel.try_send(message(2)), Err(ChannelError::Full));
        assert_eq!(channel.len(), 1);
        // Receiving frees capacity again.
        channel.recv().unwrap();
        channel.try_send(message(2)).unwrap();
        assert_eq!(channel.len(), 1);
    }

    #[test]
    fn zero_capacity_behaves_as_one() {
        let channel = Channel::bounded(0);
        assert_eq!(channel.capacity(), 1);
        channel.try_send(message(1)).unwrap();
        assert_eq!(channel.try_send(message(2)), Err(ChannelError::Full));
    }

    #[test]
    fn send_blocks_until_space_is_available() {
        let channel = Channel::bounded(1);
        channel.send(message(1)).unwrap();
        let sender = channel.clone();
        let started = Instant::now();
        let handle = thread::spawn(move || {
            sender.send(message(2)).unwrap();
            started.elapsed()
        });
        // The sender must not complete while the queue is full.
        thread::sleep(Duration::from_millis(40));
        assert_eq!(channel.len(), 1);
        assert_eq!(channel.recv().unwrap().id.0, 1);
        let waited = handle.join().unwrap();
        assert!(waited >= Duration::from_millis(20));
        assert_eq!(channel.recv().unwrap().id.0, 2);
    }

    #[test]
    fn recv_timeout_returns_timeout_when_idle() {
        let channel = Channel::bounded(2);
        let started = Instant::now();
        assert_eq!(
            channel.recv_timeout(Duration::from_millis(20)),
            Err(ChannelError::Timeout)
        );
        assert!(started.elapsed() >= Duration::from_millis(15));
    }

    #[test]
    fn recv_timeout_prefers_queued_messages() {
        let channel = Channel::bounded(2);
        channel.send(message(5)).unwrap();
        let received = channel.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(received.id.0, 5);
    }

    #[test]
    fn recv_timeout_wakes_on_send() {
        let channel = Channel::bounded(2);
        let sender = channel.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            sender.send(message(9)).unwrap();
        });
        let received = channel.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(received.id.0, 9);
        handle.join().unwrap();
    }

    #[test]
    fn close_wakes_blocked_receiver() {
        let channel = Channel::bounded(1);
        let receiver = channel.clone();
        let handle = thread::spawn(move || receiver.recv());
        thread::sleep(Duration::from_millis(20));
        channel.close();
        assert_eq!(handle.join().unwrap(), Err(ChannelError::Disconnected));
        assert!(channel.is_closed());
    }

    #[test]
    fn close_wakes_blocked_sender() {
        let channel = Channel::bounded(1);
        channel.send(message(1)).unwrap();
        let sender = channel.clone();
        let handle = thread::spawn(move || sender.send(message(2)));
        thread::sleep(Duration::from_millis(20));
        channel.close();
        assert_eq!(handle.join().unwrap(), Err(ChannelError::Disconnected));
    }

    #[test]
    fn closed_channel_drains_queued_messages() {
        let channel = Channel::bounded(4);
        channel.send(message(1)).unwrap();
        channel.send(message(2)).unwrap();
        channel.close();
        assert_eq!(channel.recv().unwrap().id.0, 1);
        assert_eq!(channel.try_recv().unwrap().id.0, 2);
        assert_eq!(
            channel.recv_timeout(Duration::from_millis(10)),
            Err(ChannelError::Disconnected)
        );
        assert_eq!(channel.send(message(3)), Err(ChannelError::Disconnected));
    }

    #[test]
    fn multiple_producers_and_consumers() {
        let channel = Channel::bounded(8);
        let mut producers = Vec::new();
        for producer in 0..4u64 {
            let channel = channel.clone();
            producers.push(thread::spawn(move || {
                for index in 0..10u64 {
                    channel.send(message(producer * 100 + index)).unwrap();
                }
            }));
        }
        let consumer = channel.clone();
        let handle = thread::spawn(move || {
            let mut ids = Vec::new();
            while ids.len() < 40 {
                ids.push(consumer.recv().unwrap().id.0);
            }
            ids
        });
        for producer in producers {
            producer.join().unwrap();
        }
        let mut ids = handle.join().unwrap();
        ids.sort_unstable();
        let expected: Vec<u64> = (0..4)
            .flat_map(|p| (0..10).map(move |i| p * 100 + i))
            .collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn channel_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Channel>();
        assert_send_sync::<Message>();
    }
}
