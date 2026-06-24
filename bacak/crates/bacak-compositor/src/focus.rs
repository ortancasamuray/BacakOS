//! Focus history and alt+tab cycle state — pure logic, no Smithay.
//!
//! Two responsibilities live here:
//!
//! 1. **MRU history**: a `WindowId` deque ordered most-recent-first.
//!    Every successful focus call from outside the cycle pushes the
//!    target to the front, and dropping a window forgets it. Bounded
//!    at [`MRU_CAP`] so a long session can't bloat the deque past
//!    practical use.
//!
//! 2. **Cycle**: when the user holds Alt and taps Tab, the WM steps
//!    through a frozen snapshot of visible windows without committing
//!    each intermediate focus to history. Releasing Alt commits the
//!    current selection; pressing Escape restores whatever was focused
//!    before the cycle began.
//!
//! The module deliberately doesn't touch [`crate::wm::WindowManager`]
//! or Smithay — callers (`BacakState`) feed it `WindowId`s, get back
//! `WindowId`s, and do the actual focus / surface plumbing themselves.
//! That makes the cycle math unit-testable without standing up a
//! wayland-server.

use std::collections::VecDeque;

use crate::wm::{OutputId, WindowId};

/// Upper bound on the MRU deque. Hundreds of windows in a single
/// session is already aggressive; capping prevents pathological clients
/// from making the history grow forever.
pub const MRU_CAP: usize = 256;

/// Per-cycle bookkeeping. Frozen at the moment the user first pressed
/// Alt+Tab so a window opening or closing mid-cycle doesn't reshuffle
/// the candidate order under their fingers.
#[derive(Debug, Clone)]
struct Cycle {
    /// The cycle's candidate windows, in the order they're stepped
    /// through. Entry 0 is the originally-focused window — the cycle
    /// starts at entry 1 (or `len-1` for reverse) so Alt+Tab "moves
    /// to next", not "moves to self".
    candidates: Vec<WindowId>,
    /// Index into `candidates` currently being previewed.
    index: usize,
    /// Window that was focused at the moment the cycle started. The
    /// cancel path restores it.
    started_from: Option<WindowId>,
    /// Output the switcher overlay is pinned to for this cycle's whole
    /// life. Captured once at `start_cycle` so the overlay doesn't
    /// hop between monitors if the pointer drifts while Alt is held.
    output: OutputId,
}

/// Combined MRU + cycle state owned by `BacakState`.
#[derive(Debug, Default)]
pub struct FocusHistory {
    mru: VecDeque<WindowId>,
    cycle: Option<Cycle>,
}

impl FocusHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cycling(&self) -> bool {
        self.cycle.is_some()
    }

    /// Push `id` to the MRU front. **No-op while a cycle is active** —
    /// cycle steps preview windows without committing to history, so a
    /// quick `Alt+Tab Tab Tab` doesn't permanently scramble the order.
    pub fn promote(&mut self, id: WindowId) {
        if self.cycle.is_some() {
            return;
        }
        self.mru.retain(|x| *x != id);
        self.mru.push_front(id);
        while self.mru.len() > MRU_CAP {
            self.mru.pop_back();
        }
    }

    /// Drop a closed window from history. If a cycle is in progress
    /// and `id` was one of its candidates, strip it from the snapshot
    /// too: otherwise the user would Tab past a tile that no longer
    /// corresponds to a real window. The cycle's `index` is adjusted
    /// to stay on the same logical entry, and the cycle is torn down
    /// entirely if too few candidates remain to be useful.
    pub fn forget(&mut self, id: WindowId) {
        self.mru.retain(|x| *x != id);

        let kill = if let Some(cycle) = self.cycle.as_mut() {
            if let Some(removed_at) = cycle.candidates.iter().position(|x| *x == id) {
                cycle.candidates.remove(removed_at);
                if cycle.candidates.len() < 2 {
                    true
                } else {
                    if cycle.index > removed_at {
                        // Entry before the cursor disappeared — shift to
                        // keep the visible "current" the same.
                        cycle.index -= 1;
                    } else if cycle.index == removed_at
                        && cycle.index >= cycle.candidates.len()
                    {
                        // The currently-previewed entry just died *and*
                        // it was the tail. Wrap back to the start; any
                        // other in-bounds index means we naturally
                        // land on what used to be the next entry.
                        cycle.index = 0;
                    }
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        if kill {
            self.cycle = None;
        }
    }

    /// Build the cycle order for `current_visible`: MRU entries first
    /// (filtered to what's actually on-screen right now), then any
    /// visible window that has never been focused, appended in the
    /// caller's iteration order. The caller is expected to skip
    /// minimized windows before invoking this.
    pub fn cycle_order(&self, current_visible: &[WindowId]) -> Vec<WindowId> {
        use std::collections::HashSet;
        let visible: HashSet<WindowId> = current_visible.iter().copied().collect();
        let mut out: Vec<WindowId> = self
            .mru
            .iter()
            .copied()
            .filter(|id| visible.contains(id))
            .collect();
        let in_mru: HashSet<WindowId> = out.iter().copied().collect();
        for id in current_visible {
            if !in_mru.contains(id) {
                out.push(*id);
            }
        }
        out
    }

    /// Start a cycle. Returns the first window to focus, or `None` if
    /// there's nothing meaningful to cycle to (fewer than two
    /// candidates).
    pub fn start_cycle(
        &mut self,
        candidates: Vec<WindowId>,
        reverse: bool,
        output: OutputId,
    ) -> Option<WindowId> {
        if candidates.len() < 2 {
            return None;
        }
        let started_from = candidates.first().copied();
        let index = if reverse { candidates.len() - 1 } else { 1 };
        let target = candidates[index];
        self.cycle = Some(Cycle { candidates, index, started_from, output });
        Some(target)
    }

    /// Output the active cycle's overlay is pinned to, or `None` when
    /// no cycle is in progress. The renderer compares this against the
    /// output it's drawing so the switcher shows on exactly one screen.
    pub fn cycle_output(&self) -> Option<OutputId> {
        self.cycle.as_ref().map(|c| c.output)
    }

    /// Step the cycle by one. Wraps at the ends — Tab on the last
    /// candidate goes back to the first, matching every other WM's
    /// alt-tab. Returns `None` when no cycle is active.
    pub fn advance_cycle(&mut self, reverse: bool) -> Option<WindowId> {
        let cycle = self.cycle.as_mut()?;
        let len = cycle.candidates.len();
        if len < 2 {
            return None;
        }
        cycle.index = if reverse {
            (cycle.index + len - 1) % len
        } else {
            (cycle.index + 1) % len
        };
        Some(cycle.candidates[cycle.index])
    }

    /// The window currently previewed by the cycle, or `None` outside
    /// a cycle.
    pub fn current_cycle_window(&self) -> Option<WindowId> {
        let cycle = self.cycle.as_ref()?;
        cycle.candidates.get(cycle.index).copied()
    }

    /// The frozen candidate list for the active cycle, or `None` when
    /// no cycle is in progress. Renderers use this so the task-switcher
    /// overlay shows the exact tiles the cycle is iterating — even if
    /// a window closes mid-cycle and falls out of the visible set.
    pub fn cycle_candidates(&self) -> Option<&[WindowId]> {
        self.cycle.as_ref().map(|c| c.candidates.as_slice())
    }

    /// The most-recently-used window satisfying `pred`, by MRU order.
    /// Minimised windows stay in the MRU (only `forget` on close drops
    /// them), so passing an "is minimised on this workspace" predicate
    /// gives a true *recency* pick for un-minimise — the window the
    /// user was last on beats whatever happens to sit highest in z.
    pub fn most_recent_matching<P: Fn(WindowId) -> bool>(
        &self,
        pred: P,
    ) -> Option<WindowId> {
        self.mru.iter().copied().find(|&id| pred(id))
    }

    /// End the cycle and promote its current selection to the MRU
    /// front. Idempotent if no cycle is active.
    pub fn commit_cycle(&mut self) {
        let Some(cycle) = self.cycle.take() else { return };
        let Some(&id) = cycle.candidates.get(cycle.index) else { return };
        // Direct MRU edit — `promote` would no-op because we *just*
        // had a cycle active and the check fires too early.
        self.mru.retain(|x| *x != id);
        self.mru.push_front(id);
        while self.mru.len() > MRU_CAP {
            self.mru.pop_back();
        }
    }

    /// End the cycle and return the window that was focused before it
    /// began — the caller restores focus to that id. MRU isn't
    /// touched. Idempotent if no cycle is active.
    pub fn cancel_cycle(&mut self) -> Option<WindowId> {
        let cycle = self.cycle.take()?;
        cycle.started_from
    }

    /// MRU snapshot, most-recent first. Test-only access.
    #[cfg(test)]
    fn mru_snapshot(&self) -> Vec<WindowId> {
        self.mru.iter().copied().collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promote_pushes_to_front_and_dedupes() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3);
        h.promote(1); // moved, not duplicated
        assert_eq!(h.mru_snapshot(), vec![1, 3, 2]);
    }

    #[test]
    fn forget_removes_from_history() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.forget(1);
        assert_eq!(h.mru_snapshot(), vec![2]);
    }

    #[test]
    fn cycle_order_prefers_mru_then_appends_unseen() {
        let mut h = FocusHistory::new();
        h.promote(3);
        h.promote(1);
        // Currently visible: 1, 2, 3, 4. 4 has never been focused.
        let order = h.cycle_order(&[1, 2, 3, 4]);
        // 1 first (most recent), 3 next (older), then 2 and 4 in
        // caller's iteration order.
        assert_eq!(order, vec![1, 3, 2, 4]);
    }

    #[test]
    fn cycle_order_drops_invisible_mru_entries() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3);
        // 2 isn't visible anymore — it should be filtered out, not
        // appended to the end.
        let order = h.cycle_order(&[1, 3, 4]);
        assert_eq!(order, vec![3, 1, 4]);
    }

    #[test]
    fn start_cycle_needs_two_candidates() {
        let mut h = FocusHistory::new();
        assert!(h.start_cycle(vec![], false, 0).is_none());
        assert!(h.start_cycle(vec![1], false, 0).is_none());
        assert!(!h.is_cycling());
    }

    #[test]
    fn start_cycle_targets_second_entry_forward() {
        let mut h = FocusHistory::new();
        let first = h.start_cycle(vec![10, 20, 30], false, 0).unwrap();
        assert_eq!(first, 20);
        assert!(h.is_cycling());
        assert_eq!(h.current_cycle_window(), Some(20));
    }

    #[test]
    fn start_cycle_reverse_targets_last_entry() {
        let mut h = FocusHistory::new();
        let first = h.start_cycle(vec![10, 20, 30], true, 0).unwrap();
        assert_eq!(first, 30);
    }

    #[test]
    fn advance_wraps_in_both_directions() {
        let mut h = FocusHistory::new();
        h.start_cycle(vec![10, 20, 30], false, 0).unwrap(); // at index 1 → 20
        assert_eq!(h.advance_cycle(false), Some(30));
        assert_eq!(h.advance_cycle(false), Some(10)); // wrapped
        assert_eq!(h.advance_cycle(false), Some(20));
        assert_eq!(h.advance_cycle(true), Some(10)); // reverse wrap
    }

    #[test]
    fn promote_is_no_op_during_cycle() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.start_cycle(vec![2, 1], false, 0).unwrap();
        // While cycling, promote() should not mutate.
        h.promote(1);
        // mru still has [2, 1].
        assert_eq!(h.mru_snapshot(), vec![2, 1]);
    }

    #[test]
    fn commit_makes_current_selection_mru_top() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3);
        // mru = [3, 2, 1]. Cycle: candidates [3, 2, 1], starts at 2.
        h.start_cycle(vec![3, 2, 1], false, 0).unwrap();
        h.advance_cycle(false); // → 1
        h.commit_cycle();
        assert!(!h.is_cycling());
        assert_eq!(h.mru_snapshot(), vec![1, 3, 2]);
    }

    #[test]
    fn cancel_returns_started_from_and_clears_cycle() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.start_cycle(vec![2, 1], false, 0).unwrap(); // started_from = 2
        h.advance_cycle(false); // currently at 2 again (wrapped)
        let prev = h.cancel_cycle();
        assert_eq!(prev, Some(2));
        assert!(!h.is_cycling());
        // mru unchanged by the cycle.
        assert_eq!(h.mru_snapshot(), vec![2, 1]);
    }

    #[test]
    fn most_recent_matching_respects_mru_order() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3); // mru = [3, 2, 1]
        // First match scanning most-recent-first.
        assert_eq!(h.most_recent_matching(|id| id == 1 || id == 2), Some(2));
        assert_eq!(h.most_recent_matching(|id| id == 1), Some(1));
        assert_eq!(h.most_recent_matching(|_| false), None);
        // A re-focus moves it to the front and changes the pick.
        h.promote(1); // mru = [1, 3, 2]
        assert_eq!(h.most_recent_matching(|id| id == 1 || id == 2), Some(1));
    }

    #[test]
    fn forget_strips_dead_entry_before_cursor() {
        // mru = [3, 2, 1], start cycle at index 1 = 2. Kill 3 (which
        // sits before the cursor) and the index should shift left so
        // the *same* logical entry stays "current".
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3);
        h.start_cycle(vec![3, 2, 1], false, 0).unwrap();
        assert_eq!(h.current_cycle_window(), Some(2));
        h.forget(3);
        assert!(h.is_cycling());
        assert_eq!(h.current_cycle_window(), Some(2));
        assert_eq!(h.cycle_candidates(), Some(&[2, 1][..]));
    }

    #[test]
    fn forget_strips_dead_entry_after_cursor() {
        // Killing an entry past the cursor must not move the cursor.
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3);
        h.start_cycle(vec![3, 2, 1], false, 0).unwrap();
        assert_eq!(h.current_cycle_window(), Some(2));
        h.forget(1);
        assert_eq!(h.current_cycle_window(), Some(2));
        assert_eq!(h.cycle_candidates(), Some(&[3, 2][..]));
    }

    #[test]
    fn forget_kills_cycle_when_only_one_remains() {
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.start_cycle(vec![2, 1], false, 0).unwrap();
        h.forget(1);
        assert!(!h.is_cycling(), "cycle should die with <2 candidates left");
        assert_eq!(h.cycle_candidates(), None);
    }

    #[test]
    fn forget_current_at_tail_wraps_to_start() {
        // Candidates [3, 2, 1], index 2 (= 1 is current). Killing 1
        // leaves [3, 2] and index 2 is out of bounds → wrap to 0.
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3);
        let mut cycle_starter = vec![3, 2, 1];
        let first = h.start_cycle(cycle_starter.clone(), false, 0).unwrap();
        assert_eq!(first, 2);
        // Advance once to land on 1 (the tail).
        h.advance_cycle(false);
        assert_eq!(h.current_cycle_window(), Some(1));
        h.forget(1);
        assert!(h.is_cycling());
        assert_eq!(h.current_cycle_window(), Some(3));
        // Silence the unused-write lint.
        cycle_starter.clear();
    }

    #[test]
    fn forget_current_in_middle_keeps_index_pointing_at_next() {
        // Candidates [3, 2, 1], cursor at index 1 (= 2). Killing 2
        // shifts what was at index 2 (= 1) down into index 1. We end
        // up with the same numeric index pointing at "the next entry"
        // — i.e. the cycle naturally steps forward on death.
        let mut h = FocusHistory::new();
        h.promote(1);
        h.promote(2);
        h.promote(3);
        h.start_cycle(vec![3, 2, 1], false, 0).unwrap();
        assert_eq!(h.current_cycle_window(), Some(2));
        h.forget(2);
        assert!(h.is_cycling());
        assert_eq!(h.current_cycle_window(), Some(1));
        assert_eq!(h.cycle_candidates(), Some(&[3, 1][..]));
    }
}
