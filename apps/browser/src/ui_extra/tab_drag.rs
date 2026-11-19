//! Drag-to-reorder state machine for the tab strip.
//!
//! A drag starts on a source tab, tracks a target slot as the pointer moves,
//! and produces a new ordering (a permutation of `0..tab_count`) on commit.
//! All state is plain integers, so this can be driven from any event system.

// A framework-agnostic model: not every accessor is used by the window shell.
#![allow(dead_code)]

/// State of a tab drag operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabDrag {
    /// No drag in progress.
    #[default]
    Idle,
    /// A tab is being dragged toward a target slot.
    Dragging {
        /// Index of the tab being dragged.
        source: usize,
        /// Index the dragged tab will occupy when committed.
        target: usize,
    },
}

impl TabDrag {
    /// Creates an idle drag state.
    pub fn new() -> Self {
        Self::Idle
    }

    /// True when no drag is in progress.
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// True when a drag is in progress.
    pub fn is_dragging(&self) -> bool {
        matches!(self, Self::Dragging { .. })
    }

    /// Starts a drag from `source`, with the target initially equal to source.
    ///
    /// Returns `true` if the transition happened, `false` if a drag was already
    /// in progress.
    pub fn start(&mut self, source: usize) -> bool {
        if self.is_idle() {
            *self = Self::Dragging {
                source,
                target: source,
            };
            true
        } else {
            false
        }
    }

    /// Updates the target slot while dragging. No-op when idle.
    ///
    /// Returns `true` if the target was updated.
    pub fn move_to(&mut self, target: usize) -> bool {
        if let Self::Dragging { target: t, .. } = self {
            *t = target;
            true
        } else {
            false
        }
    }

    /// Index of the dragged tab, if a drag is in progress.
    pub fn source(&self) -> Option<usize> {
        match self {
            Self::Dragging { source, .. } => Some(*source),
            Self::Idle => None,
        }
    }

    /// Current target slot, if a drag is in progress.
    pub fn target(&self) -> Option<usize> {
        match self {
            Self::Dragging { target, .. } => Some(*target),
            Self::Idle => None,
        }
    }

    /// Computes the reordered tab indices for an active drag.
    ///
    /// Returns `None` when idle or when either index is out of range for
    /// `tab_count`. Does not mutate the drag state.
    pub fn commit(&self, tab_count: usize) -> Option<Vec<usize>> {
        match self {
            Self::Dragging { source, target } => reorder(tab_count, *source, *target),
            Self::Idle => None,
        }
    }

    /// Aborts the drag and returns to idle.
    pub fn cancel(&mut self) {
        *self = Self::Idle;
    }
}

/// Builds a new tab order by moving the tab at `source` to `target`.
///
/// The returned vector is a permutation of `0..tab_count`, where `target` is
/// the final resting index of the moved tab. Returns `None` when either index
/// is outside `0..tab_count` (including the empty case).
pub fn reorder(tab_count: usize, source: usize, target: usize) -> Option<Vec<usize>> {
    if source >= tab_count || target >= tab_count {
        return None;
    }
    let mut order: Vec<usize> = (0..tab_count).collect();
    let moved = order.remove(source);
    order.insert(target, moved);
    Some(order)
}

/// Applies a committed drag order to a display-slot → browser-tab mapping.
///
/// `positions[slot]` is the index of the tab in `browser.tabs().ids()` shown in
/// display slot `slot`. `order` is a permutation of display slots, normally the
/// value returned by [`reorder`]. The result maps each new display slot to the
/// same browser tab it showed before, just in the new order.
///
/// Returns `None` when the lengths differ or `order` is not a permutation.
///
/// # Engine gap
///
/// `browser_core::TabSet` has no `reorder` method, so the engine's tab order is
/// deliberately left untouched. The window layer keeps this mapping and uses it
/// to translate display slots into browser tab indices when selecting or
/// closing tabs. A future `TabSet::reorder` can replace the mapping wholesale.
pub fn reorder_tabs(positions: &[usize], order: &[usize]) -> Option<Vec<usize>> {
    if positions.len() != order.len() || !is_permutation(order, positions.len()) {
        return None;
    }
    Some(order.iter().map(|&slot| positions[slot]).collect())
}

/// Removes a browser tab index from a display mapping.
///
/// `index` is an index into `browser.tabs().ids()`, not a display slot. Later
/// indices shift down so the mapping stays valid after `TabSet::close`.
pub fn remove_tab(positions: &[usize], index: usize) -> Vec<usize> {
    positions
        .iter()
        .copied()
        .filter(|&position| position != index)
        .map(|position| {
            if position > index {
                position - 1
            } else {
                position
            }
        })
        .collect()
}

/// Appends a browser tab index to a display mapping.
pub fn push_tab(positions: &[usize], index: usize) -> Vec<usize> {
    let mut next = positions.to_vec();
    next.push(index);
    next
}

fn is_permutation(order: &[usize], len: usize) -> bool {
    if order.len() != len {
        return false;
    }
    let mut seen = vec![false; len];
    for &slot in order {
        if slot >= len || seen[slot] {
            return false;
        }
        seen[slot] = true;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_moves_tabs_to_the_target_slot() {
        assert_eq!(reorder(4, 0, 2), Some(vec![1, 2, 0, 3]));
        assert_eq!(reorder(4, 3, 0), Some(vec![3, 0, 1, 2]));
        assert_eq!(reorder(4, 1, 3), Some(vec![0, 2, 3, 1]));
    }

    #[test]
    fn reorder_noop_and_invalid_inputs() {
        assert_eq!(reorder(4, 1, 1), Some(vec![0, 1, 2, 3]));
        assert_eq!(reorder(0, 0, 0), None);
        assert_eq!(reorder(3, 3, 0), None);
        assert_eq!(reorder(3, 0, 3), None);
    }

    #[test]
    fn reorder_is_always_a_permutation() {
        for source in 0..5 {
            for target in 0..5 {
                let order = reorder(5, source, target).unwrap();
                let mut sorted = order.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, vec![0, 1, 2, 3, 4]);
            }
        }
    }

    #[test]
    fn state_machine_lifecycle() {
        let mut drag = TabDrag::new();
        assert_eq!(drag, TabDrag::Idle);
        assert!(drag.is_idle());
        assert!(!drag.is_dragging());
        assert_eq!(drag.source(), None);
        assert_eq!(drag.target(), None);
        assert_eq!(drag.commit(4), None);
        assert!(!drag.move_to(2), "moving while idle is a no-op");

        assert!(drag.start(0));
        assert!(!drag.start(1), "cannot start a second drag");
        assert!(drag.is_dragging());
        assert_eq!(drag.source(), Some(0));
        assert_eq!(drag.target(), Some(0));

        assert!(drag.move_to(2));
        assert_eq!(drag.target(), Some(2));
        assert_eq!(drag.commit(4), Some(vec![1, 2, 0, 3]));
        // commit is pure; state is unchanged until cancel.
        assert_eq!(drag.target(), Some(2));

        drag.cancel();
        assert!(drag.is_idle());
        assert_eq!(drag.commit(4), None);
    }

    #[test]
    fn commit_rejects_out_of_range_indices() {
        let mut drag = TabDrag::new();
        drag.start(0);
        drag.move_to(5);
        assert_eq!(drag.commit(3), None);
        assert_eq!(drag.commit(6), Some(vec![1, 2, 3, 4, 5, 0]));
    }

    #[test]
    fn reorder_tabs_maps_display_slots_to_browser_indices() {
        let mut drag = TabDrag::new();
        drag.start(0);
        drag.move_to(2);
        let order = drag.commit(4).unwrap();
        // Before: display slots [tab0, tab1, tab2, tab3].
        assert_eq!(reorder_tabs(&[0, 1, 2, 3], &order), Some(vec![1, 2, 0, 3]));
        // A non-identity starting mapping is carried through unchanged.
        assert_eq!(reorder_tabs(&[3, 2, 1, 0], &order), Some(vec![2, 1, 3, 0]));
    }

    #[test]
    fn reorder_tabs_rejects_invalid_orders() {
        assert_eq!(reorder_tabs(&[0, 1], &[0, 0]), None);
        assert_eq!(reorder_tabs(&[0, 1], &[0, 2]), None);
        assert_eq!(reorder_tabs(&[0, 1], &[0]), None);
    }

    #[test]
    fn tab_mapping_tracks_closes_and_opens() {
        assert_eq!(remove_tab(&[1, 2, 0, 3], 2), vec![1, 0, 2]);
        assert_eq!(push_tab(&[1, 0, 2], 3), vec![1, 0, 2, 3]);
    }
}
