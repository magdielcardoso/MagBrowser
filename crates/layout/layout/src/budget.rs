//! Global per-layout work budget.
//!
//! A hostile page (a very wide or very deep DOM, or a single enormous text
//! node) must not make layout take unbounded time or overflow the stack. This
//! module keeps a thread-local budget that is reset at the start of every
//! [`crate::layout`] call and consumed while the layout tree is built and laid
//! out. When a limit is hit the remaining subtrees are skipped gracefully and
//! [`crate::LayoutTree::truncated`] is set so callers can tell the result is
//! partial rather than complete.
//!
//! The counters are deliberately global to a single layout pass (rather than
//! threaded through every function signature) so that the checks can live in
//! the tree builder and in the grid/table/flex passes without changing the
//! public layout API.

use std::cell::Cell;

/// Maximum number of boxes a single layout may create.
pub const MAX_BOXES: usize = 50_000;

/// Maximum number of inline items (words, spaces, breaks) a single layout may
/// produce while line-breaking text.
pub const MAX_INLINE_ITEMS: usize = 200_000;

/// Maximum nesting depth of the layout tree. Acts as a recursion guard for
/// deeply nested block/table/grid structures (and the float layout that
/// descends through them).
pub const MAX_DEPTH: usize = 256;

/// Maximum number of guarded layout iterations across all grid/table/flex
/// passes of a single layout. Catches pathological reflow loops.
pub const MAX_ITERATIONS: usize = 500_000;

#[derive(Debug, Clone, Copy)]
struct Budget {
    boxes: usize,
    inline_items: usize,
    depth: usize,
    iterations: usize,
    truncated: bool,
    max_boxes: usize,
    max_inline_items: usize,
    max_depth: usize,
    max_iterations: usize,
}

impl Budget {
    fn fresh() -> Self {
        let (max_boxes, max_inline_items, max_depth, max_iterations) = current_limits();
        Self {
            boxes: 0,
            inline_items: 0,
            depth: 0,
            iterations: 0,
            truncated: false,
            max_boxes,
            max_inline_items,
            max_depth,
            max_iterations,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::fresh()
    }
}

fn current_limits() -> (usize, usize, usize, usize) {
    #[cfg(test)]
    if let Some(limits) = TEST_LIMITS.get() {
        return limits;
    }
    (MAX_BOXES, MAX_INLINE_ITEMS, MAX_DEPTH, MAX_ITERATIONS)
}

thread_local! {
    static BUDGET: Cell<Option<Budget>> = const { Cell::new(None) };
    #[cfg(test)]
    static TEST_LIMITS: Cell<Option<(usize, usize, usize, usize)>> = const { Cell::new(None) };
}

/// Overrides the limits for subsequent passes on this thread (tests only).
#[cfg(test)]
pub fn set_test_limits(boxes: usize, inline: usize, depth: usize, iterations: usize) {
    TEST_LIMITS.set(Some((boxes, inline, depth, iterations)));
}

/// Restores the production limits on this thread (tests only).
#[cfg(test)]
pub fn clear_test_limits() {
    TEST_LIMITS.set(None);
}

/// Runs `f` with mutable access to the active budget, creating a fresh one if
/// none is active (e.g. a direct call into a layout helper in a test).
fn with<R>(f: impl FnOnce(&mut Budget) -> R) -> R {
    BUDGET.with(|cell| {
        let mut budget = cell.get().unwrap_or_else(Budget::fresh);
        let result = f(&mut budget);
        cell.set(Some(budget));
        result
    })
}

/// Starts a fresh budget for the current thread. Called once at the top of
/// [`crate::layout`].
pub fn begin() {
    BUDGET.with(|cell| cell.set(Some(Budget::fresh())));
}

/// Ensures a budget is active. Returns `true` when a new one was started; the
/// caller is then responsible for calling [`finish`] to harvest the flag.
///
/// This lets `TreeBuilder::build` work standalone while avoiding a premature
/// reset when it is called from [`crate::layout`] (which already began one).
pub fn ensure_active() -> bool {
    BUDGET.with(|cell| {
        if cell.get().is_some() {
            false
        } else {
            cell.set(Some(Budget::fresh()));
            true
        }
    })
}

/// Ends the current budget and returns whether any limit was hit.
pub fn finish() -> bool {
    BUDGET.with(|cell| cell.take().unwrap_or_else(Budget::fresh).truncated)
}

/// Records that layout was truncated without otherwise touching the budget.
pub fn mark_truncated() {
    with(|budget| budget.truncated = true);
}

/// Accounts for one box. Returns `false` (and marks truncation) once the box
/// limit is reached; callers should then skip the subtree.
pub fn try_create_box() -> bool {
    with(|budget| {
        if budget.boxes >= budget.max_boxes {
            budget.truncated = true;
            false
        } else {
            budget.boxes += 1;
            true
        }
    })
}

/// Accounts for `count` inline items. Returns `false` (and marks truncation)
/// when they would exceed the budget; callers should skip that text.
pub fn consume_inline_items(count: usize) -> bool {
    with(|budget| {
        if budget.inline_items.saturating_add(count) > budget.max_inline_items {
            budget.truncated = true;
            false
        } else {
            budget.inline_items += count;
            true
        }
    })
}

/// Accounts for one layout iteration (grid/table/flex pass). Returns `false`
/// (and marks truncation) once the iteration cap is reached.
pub fn try_step() -> bool {
    with(|budget| {
        if budget.iterations >= budget.max_iterations {
            budget.truncated = true;
            false
        } else {
            budget.iterations += 1;
            true
        }
    })
}

/// RAII guard that caps layout-tree recursion depth.
///
/// Constructed at the entry of each `build_node` call; when the depth limit is
/// reached [`Self::entered`] is `false` and the caller skips the subtree. The
/// `Drop` impl keeps the accounting balanced even when a subtree is skipped.
pub struct DepthGuard {
    entered: bool,
}

impl DepthGuard {
    /// Attempts to descend one level, returning a guard that records whether
    /// the descent was allowed.
    #[must_use]
    pub fn enter() -> Self {
        let entered = with(|budget| {
            if budget.depth >= budget.max_depth {
                budget.truncated = true;
                false
            } else {
                budget.depth += 1;
                true
            }
        });
        Self { entered }
    }

    /// True when the guard successfully descended a level.
    pub fn entered(&self) -> bool {
        self.entered
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        if self.entered {
            with(|budget| budget.depth -= 1);
        }
    }
}

/// Estimates how many inline items a text node will produce during line
/// breaking. Each word becomes a text item and each whitespace run a space or
/// forced-break item. The result is clamped by the byte length so a single
/// pathological text node cannot be underestimated.
pub fn estimate_inline_items(text: &str) -> usize {
    let words = text.split_whitespace().count();
    let items = words.saturating_mul(2).saturating_add(1);
    // Also account for raw length so a single enormous unbroken word cannot
    // slip through with a tiny item estimate and still cost unbounded time to
    // measure while line breaking.
    items.max(text.len() / 64).min(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_budget_stops_at_the_limit() {
        begin();
        for _ in 0..MAX_BOXES {
            assert!(try_create_box());
        }
        assert!(!try_create_box());
        assert!(!try_create_box());
        assert!(finish());
        // A fresh pass starts with a clean slate.
        begin();
        assert!(try_create_box());
        assert!(!finish());
    }

    #[test]
    fn inline_budget_counted_in_batches() {
        begin();
        assert!(consume_inline_items(MAX_INLINE_ITEMS));
        assert!(!consume_inline_items(1));
        assert!(finish());
    }

    #[test]
    fn depth_guard_is_balanced_on_drop() {
        begin();
        let guards: Vec<_> = (0..MAX_DEPTH).map(|_| DepthGuard::enter()).collect();
        assert!(guards.iter().all(DepthGuard::entered));
        assert!(!DepthGuard::enter().entered());
        drop(guards);
        assert!(DepthGuard::enter().entered());
        let _ = finish();
    }

    #[test]
    fn estimates_inline_items_without_panicking() {
        assert_eq!(estimate_inline_items(""), 0);
        assert_eq!(estimate_inline_items("hello"), 3);
        assert!(estimate_inline_items("one two three") >= 6);
    }
}
