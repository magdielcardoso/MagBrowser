//! Cooperative scheduler: task queue, microtask checkpoints and timers.
//!
//! The scheduler is time-injectable: timer callbacks are only run when the host
//! calls [`Scheduler::run_due_timers`] or [`Scheduler::run_until_idle`] with a
//! synthetic `now`. No wall-clock sleeping happens inside this crate, which keeps
//! the engine deterministic and the tests fast.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fmt;
use std::time::{Duration, Instant};

use crate::microtasks::MAX_MICROTASK_BATCH;
use crate::tasks::{TaskQueue, TaskSource};

/// Default upper bound on microtasks run in one checkpoint.
///
/// A microtask that queues another microtask can otherwise starve the host
/// indefinitely. When the limit is reached, remaining microtasks stay queued and
/// the scheduler returns control to the host.
pub const DEFAULT_MICROTASK_LIMIT: usize = MAX_MICROTASK_BATCH;

/// Smallest period used when rescheduling an interval, so a zero-period interval
/// cannot spin forever against a fixed synthetic `now`.
const MIN_INTERVAL: Duration = Duration::from_nanos(1);

/// Handle returned by [`Scheduler::set_timeout`] and [`Scheduler::set_interval`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimerId(pub u64);

/// Counts of work performed by [`Scheduler::run_until_idle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunReport {
    /// Tasks popped and run from the task queue.
    pub tasks: usize,
    /// Microtasks run, including those queued by tasks and timers.
    pub microtasks: usize,
    /// Timer callbacks fired.
    pub timers: usize,
}

enum TimerCallback {
    Timeout(Option<Box<dyn FnOnce() + Send>>),
    Interval(Box<dyn FnMut() + Send>),
}

struct Timer {
    callback: TimerCallback,
    period: Option<Duration>,
}

#[derive(Debug)]
struct TimerEntry {
    deadline: Instant,
    id: TimerId,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.id == other.id
    }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed so `BinaryHeap` (a max-heap) yields the earliest deadline.
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.id.0.cmp(&self.id.0))
    }
}

/// The engine event loop: queued tasks, microtasks and a timer heap.
pub struct Scheduler {
    queue: TaskQueue,
    timers: HashMap<TimerId, Timer>,
    heap: BinaryHeap<TimerEntry>,
    next_id: u64,
    microtask_limit: usize,
}

impl Scheduler {
    /// Creates an empty scheduler with [`DEFAULT_MICROTASK_LIMIT`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty scheduler with a custom per-checkpoint microtask limit.
    ///
    /// A limit of zero is treated as one, because a checkpoint always attempts at
    /// least one microtask.
    pub fn with_microtask_limit(limit: usize) -> Self {
        Self {
            microtask_limit: limit.max(1),
            ..Self::default()
        }
    }

    /// Sets the per-checkpoint microtask limit.
    pub fn set_microtask_limit(&mut self, limit: usize) {
        self.microtask_limit = limit.max(1);
    }

    /// The per-checkpoint microtask limit.
    pub fn microtask_limit(&self) -> usize {
        self.microtask_limit
    }

    /// Queues a task from `source`.
    pub fn queue_task(&mut self, source: TaskSource, callback: Box<dyn FnOnce() + Send>) {
        self.queue.queue_task(source, callback);
    }

    /// Queues a microtask.
    pub fn queue_microtask(&mut self, callback: Box<dyn FnOnce() + Send>) {
        self.queue.queue_microtask(callback);
    }

    /// Schedules a one-shot callback after `delay` from the current instant.
    pub fn set_timeout(&mut self, delay: Duration, callback: Box<dyn FnOnce() + Send>) -> TimerId {
        self.schedule_timeout_at(Instant::now() + delay, callback)
    }

    /// Schedules a one-shot callback at an explicit deadline.
    ///
    /// Useful for deterministic hosts and tests that drive time themselves.
    pub fn schedule_timeout_at(
        &mut self,
        deadline: Instant,
        callback: Box<dyn FnOnce() + Send>,
    ) -> TimerId {
        let id = self.allocate_timer_id();
        self.timers.insert(
            id,
            Timer {
                callback: TimerCallback::Timeout(Some(callback)),
                period: None,
            },
        );
        self.heap.push(TimerEntry { deadline, id });
        id
    }

    /// Schedules a repeating callback.
    ///
    /// The first callback is due after `period`. After each callback the timer is
    /// rescheduled at `now + period` (where `now` is the instant passed to the
    /// scheduler), so a long pause does not cause a burst of catch-up callbacks.
    pub fn set_interval(&mut self, period: Duration, callback: Box<dyn FnMut() + Send>) -> TimerId {
        let id = self.allocate_timer_id();
        let period = if period.is_zero() {
            MIN_INTERVAL
        } else {
            period
        };
        self.timers.insert(
            id,
            Timer {
                callback: TimerCallback::Interval(callback),
                period: Some(period),
            },
        );
        self.heap.push(TimerEntry {
            deadline: Instant::now() + period,
            id,
        });
        id
    }

    /// Schedules a repeating callback with its first deadline relative to an
    /// explicit instant.
    pub fn schedule_interval_at(
        &mut self,
        first_deadline: Instant,
        period: Duration,
        callback: Box<dyn FnMut() + Send>,
    ) -> TimerId {
        let id = self.allocate_timer_id();
        let period = if period.is_zero() {
            MIN_INTERVAL
        } else {
            period
        };
        self.timers.insert(
            id,
            Timer {
                callback: TimerCallback::Interval(callback),
                period: Some(period),
            },
        );
        self.heap.push(TimerEntry {
            deadline: first_deadline,
            id,
        });
        id
    }

    /// Cancels a timeout or interval.
    ///
    /// Returns true when a live timer was removed. Cancellation is lazy with
    /// respect to the heap: the stale entry is discarded when it surfaces.
    pub fn clear_timer(&mut self, id: TimerId) -> bool {
        self.timers.remove(&id).is_some()
    }

    /// The earliest live timer deadline, or `None` when no timers remain.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.heap
            .iter()
            .filter(|entry| self.timers.contains_key(&entry.id))
            .map(|entry| entry.deadline)
            .min()
    }

    /// Runs every timer whose deadline is at or before `now`.
    ///
    /// Microtasks are drained after each timer callback. Returns the number of
    /// timer callbacks fired.
    pub fn run_due_timers(&mut self, now: Instant) -> usize {
        self.run_timers_internal(now).0
    }

    /// Runs queued work until the task queue has no ready tasks and no timer is
    /// due at `now`.
    ///
    /// A microtask checkpoint runs before the first task, after every task and
    /// after every timer callback. If a checkpoint hits the microtask limit, the
    /// remaining microtasks stay queued and control returns to the host to avoid
    /// starvation.
    pub fn run_until_idle(&mut self, now: Instant) -> RunReport {
        let mut report = RunReport::default();
        loop {
            if self.queue.pending_microtasks() > 0 {
                report.microtasks += self.drain_microtasks(self.microtask_limit);
                if self.queue.pending_microtasks() > 0 {
                    break;
                }
                continue;
            }

            let timer_due = self.next_deadline().is_some_and(|deadline| deadline <= now);
            let next_source = self.queue.next_source();

            // A due timer is a task from the Timers source, so it runs before
            // lower-priority network/DOM/rendering work but after input.
            if timer_due
                && next_source
                    .is_none_or(|source| source.priority() > TaskSource::Timers.priority())
            {
                let (timers, microtasks) = self.run_timers_internal(now);
                report.timers += timers;
                report.microtasks += microtasks;
                if self.queue.pending_microtasks() > 0 {
                    break;
                }
                if timers == 0 {
                    break;
                }
                continue;
            }

            if let Some(task) = self.queue.pop_task() {
                task.run();
                report.tasks += 1;
                report.microtasks += self.drain_microtasks(self.microtask_limit);
                if self.queue.pending_microtasks() > 0 {
                    break;
                }
                continue;
            }

            break;
        }
        report
    }

    /// Runs at most one queued task and the microtasks around it.
    ///
    /// A checkpoint drains existing microtasks before the task, then drains the
    /// microtasks produced by the task. Returns false when no task was queued.
    /// Due timers are not run by this method; use [`Self::run_due_timers`] for
    /// those.
    pub fn run_one_task(&mut self) -> bool {
        self.drain_microtasks(self.microtask_limit);
        let Some(task) = self.queue.pop_task() else {
            return false;
        };
        task.run();
        self.drain_microtasks(self.microtask_limit);
        true
    }

    /// Drains microtasks until the queue is empty or `limit` microtasks ran.
    ///
    /// Returns the number of microtasks run.
    pub fn drain_microtasks(&mut self, limit: usize) -> usize {
        self.queue.drain_microtasks(limit)
    }

    /// True when a task or microtask is queued. Timers are not considered.
    pub fn has_pending(&self) -> bool {
        self.queue.has_pending()
    }

    /// Number of queued tasks across all sources.
    pub fn pending_tasks(&self) -> usize {
        self.queue.pending_tasks()
    }

    /// Number of queued microtasks.
    pub fn pending_microtasks(&self) -> usize {
        self.queue.pending_microtasks()
    }

    /// Number of live timers.
    pub fn pending_timers(&self) -> usize {
        self.timers.len()
    }

    fn allocate_timer_id(&mut self) -> TimerId {
        self.next_id += 1;
        TimerId(self.next_id)
    }

    fn run_timers_internal(&mut self, now: Instant) -> (usize, usize) {
        let mut timers = 0;
        let mut microtasks = 0;
        while let Some(deadline) = self.next_deadline() {
            if deadline > now {
                break;
            }
            let Some(entry) = self.heap.pop() else {
                break;
            };
            let Some(mut timer) = self.timers.remove(&entry.id) else {
                continue;
            };
            let reschedule = match &mut timer.callback {
                TimerCallback::Timeout(slot) => {
                    if let Some(callback) = slot.take() {
                        callback();
                    }
                    false
                }
                TimerCallback::Interval(callback) => {
                    callback();
                    true
                }
            };
            timers += 1;
            microtasks += self.drain_microtasks(self.microtask_limit);
            if reschedule {
                let period = timer.period.unwrap_or(MIN_INTERVAL);
                let next = now + period;
                self.timers.insert(entry.id, timer);
                self.heap.push(TimerEntry {
                    deadline: next,
                    id: entry.id,
                });
            }
        }
        (timers, microtasks)
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            queue: TaskQueue::new(),
            timers: HashMap::new(),
            heap: BinaryHeap::new(),
            next_id: 0,
            microtask_limit: DEFAULT_MICROTASK_LIMIT,
        }
    }
}

impl fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scheduler")
            .field("pending_tasks", &self.pending_tasks())
            .field("pending_microtasks", &self.pending_microtasks())
            .field("pending_timers", &self.pending_timers())
            .field("microtask_limit", &self.microtask_limit)
            .finish()
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
    fn interaction_runs_before_network_and_rendering() {
        let mut scheduler = Scheduler::new();
        let log = log();
        scheduler.queue_task(TaskSource::Rendering, recorder(&log, "render"));
        scheduler.queue_task(TaskSource::Networking, recorder(&log, "network"));
        scheduler.queue_task(TaskSource::UserInteraction, recorder(&log, "input"));
        let report = scheduler.run_until_idle(Instant::now());
        assert_eq!(*log.lock().unwrap(), ["input", "network", "render"]);
        assert_eq!(report.tasks, 3);
    }

    #[test]
    fn timers_run_before_networking_tasks() {
        let mut scheduler = Scheduler::new();
        let log = log();
        scheduler.queue_task(TaskSource::Networking, recorder(&log, "network"));
        scheduler.set_timeout(Duration::ZERO, recorder(&log, "timer"));
        let report = scheduler.run_until_idle(Instant::now());
        assert_eq!(*log.lock().unwrap(), ["timer", "network"]);
        assert_eq!(report.timers, 1);
        assert_eq!(report.tasks, 1);
    }

    #[test]
    fn dom_manipulation_sits_between_timers_and_networking() {
        let mut scheduler = Scheduler::new();
        let log = log();
        scheduler.queue_task(TaskSource::Networking, recorder(&log, "network"));
        scheduler.queue_task(TaskSource::DomManipulation, recorder(&log, "dom"));
        scheduler.queue_task(TaskSource::Timers, recorder(&log, "timer"));
        scheduler.run_until_idle(Instant::now());
        assert_eq!(*log.lock().unwrap(), ["timer", "dom", "network"]);
    }

    #[test]
    fn microtasks_run_after_each_task_before_next_task() {
        let mut scheduler = Scheduler::new();
        let log = log();
        let micro_log = Arc::clone(&log);
        scheduler.queue_task(TaskSource::Networking, recorder(&log, "network"));
        scheduler.queue_microtask(Box::new(move || {
            micro_log.lock().unwrap().push("micro");
        }));
        scheduler.queue_task(TaskSource::Rendering, recorder(&log, "render"));
        let report = scheduler.run_until_idle(Instant::now());
        assert_eq!(*log.lock().unwrap(), ["micro", "network", "render"]);
        assert_eq!(report.microtasks, 1);
    }

    #[test]
    fn chained_microtasks_are_drained_to_exhaustion() {
        let mut scheduler = Scheduler::new();
        let log = log();
        for label in ["a", "b", "c"] {
            let next = Arc::clone(&log);
            scheduler.queue_microtask(Box::new(move || {
                next.lock().unwrap().push(label);
            }));
        }
        let report = scheduler.run_until_idle(Instant::now());
        assert_eq!(*log.lock().unwrap(), ["a", "b", "c"]);
        assert_eq!(report.microtasks, 3);
    }

    #[test]
    fn microtask_mutations_are_visible_to_the_next_task() {
        let mut scheduler = Scheduler::new();
        let log = log();
        let reader = Arc::clone(&log);
        scheduler.queue_microtask(Box::new({
            let log = Arc::clone(&log);
            move || {
                log.lock().unwrap().push("micro");
            }
        }));
        scheduler.queue_task(
            TaskSource::Networking,
            Box::new(move || reader.lock().unwrap().push("task")),
        );
        scheduler.run_until_idle(Instant::now());
        assert_eq!(*log.lock().unwrap(), ["micro", "task"]);
    }

    #[test]
    fn microtask_starvation_is_bounded() {
        let mut scheduler = Scheduler::with_microtask_limit(3);
        let log = log();
        // Pre-filling more microtasks than the limit models a callback that keeps
        // queueing new work: each checkpoint must stop and leave the rest queued.
        for _ in 0..8 {
            scheduler.queue_microtask({
                let log = Arc::clone(&log);
                Box::new(move || log.lock().unwrap().push("m"))
            });
        }
        let report = scheduler.run_until_idle(Instant::now());
        assert_eq!(report.microtasks, 3);
        assert_eq!(scheduler.pending_microtasks(), 5);
        assert_eq!(log.lock().unwrap().len(), 3);
        let second = scheduler.run_until_idle(Instant::now());
        assert_eq!(second.microtasks, 3);
        assert_eq!(scheduler.pending_microtasks(), 2);
    }

    #[test]
    fn timeout_waits_for_its_deadline() {
        let mut scheduler = Scheduler::new();
        let log = log();
        let start = Instant::now();
        scheduler.schedule_timeout_at(start + Duration::from_millis(50), recorder(&log, "late"));
        assert_eq!(scheduler.run_due_timers(start), 0);
        assert!(log.lock().unwrap().is_empty());
        assert_eq!(
            scheduler.run_due_timers(start + Duration::from_millis(50)),
            1
        );
        assert_eq!(*log.lock().unwrap(), ["late"]);
    }

    #[test]
    fn next_deadline_reports_the_earliest_timer() {
        let mut scheduler = Scheduler::new();
        let start = Instant::now();
        assert_eq!(scheduler.next_deadline(), None);
        scheduler.schedule_timeout_at(start + Duration::from_secs(3), Box::new(|| {}));
        scheduler.schedule_timeout_at(start + Duration::from_secs(1), Box::new(|| {}));
        scheduler.schedule_timeout_at(start + Duration::from_secs(2), Box::new(|| {}));
        assert_eq!(
            scheduler.next_deadline(),
            Some(start + Duration::from_secs(1))
        );
    }

    #[test]
    fn interval_reschedules_from_now() {
        let mut scheduler = Scheduler::new();
        let log = log();
        let start = Instant::now();
        let inner = Arc::clone(&log);
        let id = scheduler.schedule_interval_at(
            start,
            Duration::from_millis(10),
            Box::new(move || inner.lock().unwrap().push("tick")),
        );
        assert_eq!(scheduler.run_due_timers(start), 1);
        assert_eq!(
            scheduler.next_deadline(),
            Some(start + Duration::from_millis(10))
        );
        assert_eq!(
            scheduler.run_due_timers(start + Duration::from_millis(9)),
            0
        );
        assert_eq!(
            scheduler.run_due_timers(start + Duration::from_millis(10)),
            1
        );
        assert_eq!(log.lock().unwrap().len(), 2);
        assert_eq!(
            scheduler.next_deadline(),
            Some(start + Duration::from_millis(20))
        );
        scheduler.clear_timer(id);
        assert_eq!(scheduler.pending_timers(), 0);
    }

    #[test]
    fn clear_timeout_prevents_the_callback() {
        let mut scheduler = Scheduler::new();
        let log = log();
        let id = scheduler.set_timeout(Duration::ZERO, recorder(&log, "nope"));
        assert!(scheduler.clear_timer(id));
        assert_eq!(scheduler.run_due_timers(Instant::now()), 0);
        assert!(log.lock().unwrap().is_empty());
        assert!(!scheduler.clear_timer(id));
    }

    #[test]
    fn clear_interval_stops_rescheduling() {
        let mut scheduler = Scheduler::new();
        let start = Instant::now();
        let ticks = Arc::new(Mutex::new(0_usize));
        let inner = Arc::clone(&ticks);
        let id = scheduler.schedule_interval_at(
            start,
            Duration::from_secs(1),
            Box::new(move || *inner.lock().unwrap() += 1),
        );
        assert_eq!(scheduler.run_due_timers(start), 1);
        assert!(scheduler.clear_timer(id));
        assert_eq!(scheduler.run_due_timers(start + Duration::from_secs(5)), 0);
        assert_eq!(*ticks.lock().unwrap(), 1);
    }

    #[test]
    fn timers_fire_in_deadline_then_id_order() {
        let mut scheduler = Scheduler::new();
        let log = log();
        let start = Instant::now();
        scheduler.schedule_timeout_at(start, recorder(&log, "first"));
        scheduler.schedule_timeout_at(start, recorder(&log, "second"));
        assert_eq!(scheduler.run_due_timers(start), 2);
        assert_eq!(*log.lock().unwrap(), ["first", "second"]);
    }

    #[test]
    fn run_until_idle_reports_all_work() {
        let mut scheduler = Scheduler::new();
        let start = Instant::now();
        let log = log();
        scheduler.queue_task(TaskSource::DomManipulation, recorder(&log, "task"));
        scheduler.queue_microtask(recorder(&log, "micro"));
        scheduler.schedule_timeout_at(start, recorder(&log, "timer"));
        let report = scheduler.run_until_idle(start);
        assert_eq!(
            report,
            RunReport {
                tasks: 1,
                microtasks: 1,
                timers: 1,
            }
        );
        assert!(!scheduler.has_pending());
        assert_eq!(scheduler.pending_timers(), 0);
    }

    #[test]
    fn run_one_task_processes_one_task_and_its_microtasks() {
        let mut scheduler = Scheduler::new();
        let log = log();
        assert!(!scheduler.run_one_task());
        scheduler.queue_task(TaskSource::Networking, recorder(&log, "task"));
        scheduler.queue_microtask(recorder(&log, "micro"));
        assert!(scheduler.run_one_task());
        assert_eq!(*log.lock().unwrap(), ["micro", "task"]);
        assert!(!scheduler.has_pending());
    }

    #[test]
    fn zero_period_interval_does_not_spin_forever() {
        let mut scheduler = Scheduler::new();
        let start = Instant::now();
        let ticks = Arc::new(Mutex::new(0_usize));
        let inner = Arc::clone(&ticks);
        scheduler.schedule_interval_at(
            start,
            Duration::ZERO,
            Box::new(move || *inner.lock().unwrap() += 1),
        );
        assert_eq!(scheduler.run_due_timers(start), 1);
        assert_eq!(scheduler.run_due_timers(start), 0);
        assert_eq!(*ticks.lock().unwrap(), 1);
    }

    #[test]
    fn microtasks_are_drained_after_timer_callbacks() {
        let mut scheduler = Scheduler::new();
        let start = Instant::now();
        let log = log();
        let micro_log = Arc::clone(&log);
        scheduler.schedule_timeout_at(
            start,
            Box::new(move || {
                micro_log.lock().unwrap().push("timer");
            }),
        );
        scheduler.queue_microtask(Box::new({
            let log = Arc::clone(&log);
            move || log.lock().unwrap().push("micro")
        }));
        let report = scheduler.run_until_idle(start);
        assert_eq!(*log.lock().unwrap(), ["micro", "timer"]);
        assert_eq!(report.microtasks, 1);
        assert_eq!(report.timers, 1);
    }

    #[test]
    fn with_microtask_limit_is_at_least_one() {
        let scheduler = Scheduler::with_microtask_limit(0);
        assert_eq!(scheduler.microtask_limit(), 1);
    }
}
