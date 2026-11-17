//! Task sources, queued tasks and FIFO task queues.

use std::collections::VecDeque;
use std::fmt;

use crate::microtasks::MicrotaskQueue;

/// The task source a callback belongs to.
///
/// The HTML event loop selects tasks from several task sources. This engine uses
/// an explicit priority order so that input and timer work cannot be starved by a
/// busy network or rendering pipeline:
///
/// 1. [`TaskSource::UserInteraction`]
/// 2. [`TaskSource::Timers`]
/// 3. [`TaskSource::DomManipulation`]
/// 4. [`TaskSource::Networking`]
/// 5. [`TaskSource::Rendering`]
///
/// Tasks from the same source run in FIFO order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskSource {
    /// Network responses, fetches and other I/O completions.
    Networking,
    /// DOM mutation notifications and dispatch of queued DOM events.
    DomManipulation,
    /// Input events such as clicks, key presses and pointer movement.
    UserInteraction,
    /// `setTimeout` and `setInterval` callbacks.
    Timers,
    /// Animation-frame and paint work.
    Rendering,
}

impl TaskSource {
    /// Priority rank: lower ranks run first.
    pub const fn priority(self) -> u8 {
        match self {
            Self::UserInteraction => 0,
            Self::Timers => 1,
            Self::DomManipulation => 2,
            Self::Networking => 3,
            Self::Rendering => 4,
        }
    }

    const fn index(self) -> usize {
        self.priority() as usize
    }
}

const fn source_for_index(index: usize) -> TaskSource {
    match index {
        0 => TaskSource::UserInteraction,
        1 => TaskSource::Timers,
        2 => TaskSource::DomManipulation,
        3 => TaskSource::Networking,
        _ => TaskSource::Rendering,
    }
}

/// A queued task with its source and a boxed callback.
pub struct Task {
    /// The source this task was queued from.
    pub source: TaskSource,
    /// The work to perform. The callback runs exactly once.
    pub callback: Box<dyn FnOnce() + Send>,
}

impl Task {
    /// Creates a task from a source and callback.
    pub fn new(source: TaskSource, callback: Box<dyn FnOnce() + Send>) -> Self {
        Self { source, callback }
    }

    /// Runs the task's callback.
    pub fn run(self) {
        (self.callback)();
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task")
            .field("source", &self.source)
            .field("callback", &"<callback>")
            .finish()
    }
}

/// FIFO task queues plus a FIFO microtask queue.
///
/// `TaskQueue` is deliberately independent of clocks and I/O so it can be driven
/// deterministically by tests. [`crate::Scheduler`] adds timer scheduling on top.
#[derive(Debug, Default)]
pub struct TaskQueue {
    /// One queue per priority rank, indexed by [`TaskSource::priority`].
    queues: [VecDeque<Task>; 5],
    microtasks: MicrotaskQueue,
}

impl TaskQueue {
    /// Creates an empty task queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a task to the queue for `source`.
    pub fn queue_task(&mut self, source: TaskSource, callback: Box<dyn FnOnce() + Send>) {
        self.queues[source.index()].push_back(Task::new(source, callback));
    }

    /// Appends a microtask. Microtasks run after the current task and are
    /// processed until the queue is empty (subject to the scheduler's limit).
    pub fn queue_microtask(&mut self, callback: Box<dyn FnOnce() + Send>) {
        self.microtasks.enqueue(callback);
    }

    /// Pops the next task, honouring source priority and FIFO order.
    pub fn pop_task(&mut self) -> Option<Task> {
        for queue in &mut self.queues {
            if let Some(task) = queue.pop_front() {
                return Some(task);
            }
        }
        None
    }

    /// The source of the next task without removing it.
    pub fn next_source(&self) -> Option<TaskSource> {
        for (index, queue) in self.queues.iter().enumerate() {
            if !queue.is_empty() {
                return Some(source_for_index(index));
            }
        }
        None
    }

    /// Pops the next microtask.
    pub fn pop_microtask(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
        self.microtasks.dequeue()
    }

    /// Drains at most `limit` microtasks, returning how many ran.
    pub fn drain_microtasks(&mut self, limit: usize) -> usize {
        self.microtasks.drain(limit)
    }

    /// True when either a task or a microtask is queued.
    pub fn has_pending(&self) -> bool {
        self.has_pending_tasks() || !self.microtasks.is_empty()
    }

    /// True when at least one task (not microtask) is queued.
    pub fn has_pending_tasks(&self) -> bool {
        self.queues.iter().any(|queue| !queue.is_empty())
    }

    /// Total number of queued tasks across every source.
    pub fn pending_tasks(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }

    /// Number of queued microtasks.
    pub fn pending_microtasks(&self) -> usize {
        self.microtasks.len()
    }

    /// Number of queued tasks for one source.
    pub fn pending_for(&self, source: TaskSource) -> usize {
        self.queues[source.index()].len()
    }

    /// Removes every queued task and microtask without running them.
    pub fn clear(&mut self) {
        for queue in &mut self.queues {
            queue.clear();
        }
        self.microtasks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<&'static str>>>;

    fn log() -> Log {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn recorder(log: &Log, label: &'static str) -> Box<dyn FnOnce() + Send> {
        let log = Arc::clone(log);
        Box::new(move || log.lock().unwrap().push(label))
    }

    #[test]
    fn preserves_fifo_order_within_a_source() {
        let mut queue = TaskQueue::new();
        let log = log();
        for label in ["a", "b", "c"] {
            queue.queue_task(TaskSource::Networking, recorder(&log, label));
        }
        while let Some(task) = queue.pop_task() {
            task.run();
        }
        assert_eq!(*log.lock().unwrap(), ["a", "b", "c"]);
    }

    #[test]
    fn pops_higher_priority_sources_first() {
        let mut queue = TaskQueue::new();
        let log = log();
        queue.queue_task(TaskSource::Rendering, recorder(&log, "render"));
        queue.queue_task(TaskSource::Networking, recorder(&log, "network"));
        queue.queue_task(TaskSource::Timers, recorder(&log, "timer"));
        queue.queue_task(TaskSource::UserInteraction, recorder(&log, "input"));
        while let Some(task) = queue.pop_task() {
            task.run();
        }
        assert_eq!(
            *log.lock().unwrap(),
            ["input", "timer", "network", "render"]
        );
    }

    #[test]
    fn tracks_pending_counts_per_kind() {
        let mut queue = TaskQueue::new();
        let log = log();
        assert!(!queue.has_pending());
        queue.queue_task(TaskSource::Networking, recorder(&log, "task"));
        queue.queue_microtask(recorder(&log, "micro"));
        assert!(queue.has_pending());
        assert!(queue.has_pending_tasks());
        assert_eq!(queue.pending_tasks(), 1);
        assert_eq!(queue.pending_for(TaskSource::Networking), 1);
        assert_eq!(queue.pending_microtasks(), 1);
        queue.clear();
        assert_eq!(queue.pending_tasks(), 0);
        assert_eq!(queue.pending_microtasks(), 0);
    }

    #[test]
    fn source_priority_is_stable() {
        assert!(TaskSource::UserInteraction.priority() < TaskSource::Timers.priority());
        assert!(TaskSource::Timers.priority() < TaskSource::Networking.priority());
        assert!(TaskSource::Networking.priority() < TaskSource::Rendering.priority());
    }
}
