//! The microtask queue and its bounded drain helper.
//!
//! Microtasks run after the currently executing task, one checkpoint at a time.
//! Because a microtask may queue another microtask, every drain is bounded: the
//! host regains control after [`MAX_MICROTASK_BATCH`] callbacks and the rest stay
//! queued for the next checkpoint. This is the engine's starvation guard.

use std::collections::VecDeque;
use std::fmt;

/// Default maximum number of microtasks processed in a single checkpoint.
pub const MAX_MICROTASK_BATCH: usize = 4096;

/// A FIFO queue of boxed microtask callbacks.
#[derive(Default)]
pub struct MicrotaskQueue {
    queue: VecDeque<Box<dyn FnOnce() + Send>>,
}

impl MicrotaskQueue {
    /// Creates an empty microtask queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a microtask.
    pub fn enqueue(&mut self, callback: Box<dyn FnOnce() + Send>) {
        self.queue.push_back(callback);
    }

    /// Pops the oldest microtask.
    pub fn dequeue(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
        self.queue.pop_front()
    }

    /// True when no microtask is queued.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Number of queued microtasks.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Removes all queued microtasks without running them.
    pub fn clear(&mut self) {
        self.queue.clear();
    }

    /// Drains at most `limit` microtasks, returning how many ran.
    pub fn drain(&mut self, limit: usize) -> usize {
        drain_bounded(self, limit)
    }
}

impl fmt::Debug for MicrotaskQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MicrotaskQueue")
            .field("len", &self.queue.len())
            .finish()
    }
}

/// Runs at most `limit` microtasks from `queue` and returns the number run.
pub fn drain_bounded(queue: &mut MicrotaskQueue, limit: usize) -> usize {
    let mut ran = 0;
    while ran < limit {
        let Some(callback) = queue.dequeue() else {
            break;
        };
        callback();
        ran += 1;
    }
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn drains_fifo_up_to_the_limit() {
        let mut queue = MicrotaskQueue::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        for label in [1_u8, 2, 3, 4] {
            let log = Arc::clone(&log);
            queue.enqueue(Box::new(move || log.lock().unwrap().push(label)));
        }
        assert_eq!(queue.len(), 4);
        assert_eq!(queue.drain(2), 2);
        assert_eq!(*log.lock().unwrap(), [1, 2]);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.drain(10), 2);
        assert!(queue.is_empty());
    }

    #[test]
    fn clear_drops_pending_work() {
        let mut queue = MicrotaskQueue::new();
        queue.enqueue(Box::new(|| {}));
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.drain(1), 0);
    }
}
