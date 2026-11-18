//! CF12b: one portable import at a time per isolate (CF12 review round 1),
//! with a hold that goes stale.
//!
//! The operator route takes the gate before reading the archive and gives
//! it back when the request ends (its guard's `Drop`). If Cloudflare
//! cancels the invocation (the client went away), its future may be
//! abandoned rather than dropped, so the guard never runs and every later
//! import on that isolate would answer `409 import.busy` until the isolate
//! is evicted. So a hold with no progress for [`STALE_AFTER_MS`] no longer
//! refuses: the next import takes the gate over. A guard released after
//! its hold was taken over leaves the newer hold alone.
//!
//! CF12b review round 1: the hold is refreshed as the import progresses
//! ([`Gate::touch`]): as the archive's body arrives, after it is decoded
//! and planned, and on each of its subrequests. Stale means "no progress
//! for 15 minutes", not "started 15 minutes ago", so a long import (a slow
//! client sending its body, or up to 9,900 subrequests one after another)
//! is never taken over while it works.

use std::cell::Cell;

/// How long a hold refuses other imports without progress.
///
/// Between two touches an import does at most one of: one chunk of its
/// body, one R2 or object request, one D1 query (at most 30 seconds), or
/// the CPU work of decoding, validating and planning (at most 5 minutes of
/// CPU, the most `limits.cpu_ms` allows; the default is 30 seconds). 15
/// minutes is well past any of those, so an import that is abandoned
/// mid-way is the only hold that goes stale. Should a single step still
/// run past it, the cost is two imports sharing the isolate's memory, not
/// a wrong import (each is still checked by its own guards).
pub const STALE_AFTER_MS: i64 = 15 * 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hold {
    generation: u64,
    /// When the hold last made progress (taken, or touched).
    progress_ms: i64,
}

/// One isolate's import gate.
#[derive(Debug, Default)]
pub struct Gate {
    held: Cell<Option<Hold>>,
    next: Cell<u64>,
}

impl Gate {
    pub const fn new() -> Self {
        Self {
            held: Cell::new(None),
            next: Cell::new(0),
        }
    }

    /// Take the gate at `now_ms`: the hold's generation, or `None` while
    /// another import holds it and has made progress within the bound. A
    /// clock that went backwards counts as no time passed.
    pub fn begin(&self, now_ms: i64) -> Option<u64> {
        if let Some(hold) = self.held.get()
            && now_ms.saturating_sub(hold.progress_ms) < STALE_AFTER_MS
        {
            return None;
        }
        let generation = self.next.get();
        self.next.set(generation.wrapping_add(1));
        self.held.set(Some(Hold {
            generation,
            progress_ms: now_ms,
        }));
        Some(generation)
    }

    /// Record progress by the import holding `generation` at `now_ms`. A
    /// hold taken over since is left alone, and progress never moves a
    /// hold's time back.
    pub fn touch(&self, generation: u64, now_ms: i64) {
        if let Some(hold) = self.held.get()
            && hold.generation == generation
            && now_ms > hold.progress_ms
        {
            self.held.set(Some(Hold {
                generation,
                progress_ms: now_ms,
            }));
        }
    }

    /// Give back the hold `generation` took; a hold taken over since is
    /// left alone.
    pub fn end(&self, generation: u64) {
        if self.held.get().is_some_and(|h| h.generation == generation) {
            self.held.set(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000;

    #[test]
    fn a_second_import_is_refused_while_one_runs() {
        let gate = Gate::new();
        let first = gate.begin(T0).unwrap();
        assert_eq!(gate.begin(T0 + 1_000), None);
        gate.end(first);
        assert!(gate.begin(T0 + 2_000).is_some(), "let in once it ends");
    }

    /// The finding: a request abandoned without its guard running held the
    /// gate for the isolate's life. Now the hold goes stale.
    #[test]
    fn a_hold_never_given_back_goes_stale() {
        let gate = Gate::new();
        let abandoned = gate.begin(T0).unwrap();
        assert_eq!(gate.begin(T0 + STALE_AFTER_MS - 1), None, "not yet");
        let next = gate.begin(T0 + STALE_AFTER_MS).unwrap();
        assert_ne!(next, abandoned);
        // The newer hold refuses others in its turn.
        assert_eq!(gate.begin(T0 + STALE_AFTER_MS + 1), None);
        // The abandoned request's guard, should it run after all, does not
        // release the newer hold.
        gate.end(abandoned);
        assert_eq!(gate.begin(T0 + STALE_AFTER_MS + 2), None);
        gate.end(next);
        assert!(gate.begin(T0 + STALE_AFTER_MS + 3).is_some());
    }

    #[test]
    fn a_clock_that_goes_back_does_not_free_the_gate() {
        let gate = Gate::new();
        gate.begin(T0).unwrap();
        assert_eq!(gate.begin(T0 - STALE_AFTER_MS * 2), None);
        assert_eq!(gate.begin(i64::MIN), None);
    }

    /// CF12b review round 1 (finding 3): an import that keeps making
    /// progress (each stage and subrequest touches its hold) is never taken
    /// over, however long it runs; one that stops for the bound is. A touch
    /// by a hold that was taken over does nothing.
    #[test]
    fn a_hold_that_makes_progress_does_not_go_stale() {
        let gate = Gate::new();
        let held = gate.begin(T0).unwrap();
        let mut now = T0;
        for _ in 0..10 {
            now += STALE_AFTER_MS - 1;
            gate.touch(held, now);
            assert_eq!(gate.begin(now + 1), None, "still making progress");
        }
        // No progress for the bound: stale.
        let t1 = now + STALE_AFTER_MS;
        let next = gate.begin(t1).unwrap();
        // The abandoned import's late touch does not refresh the new hold.
        gate.touch(held, t1 + 10);
        assert!(gate.begin(t1 + STALE_AFTER_MS).is_some(), "not refreshed");
        // Progress never moves a hold back.
        let t2 = t1 + STALE_AFTER_MS;
        let third = gate.begin(t2 + STALE_AFTER_MS).unwrap();
        assert_ne!(third, next);
        gate.touch(third, t2 + STALE_AFTER_MS + 100);
        gate.touch(third, 0);
        assert_eq!(gate.begin(t2 + 2 * STALE_AFTER_MS + 99), None);
    }

    /// The bound is longer than the longest step between two touches the
    /// limits allow: the CPU of decoding and planning, or one D1 query.
    #[test]
    fn the_bound_outlasts_a_step_of_an_import() {
        let cpu_ms = 5 * 60_000;
        let d1_ms = 30_000;
        assert!(STALE_AFTER_MS > cpu_ms + d1_ms, "{}", cpu_ms + d1_ms);
    }
}
