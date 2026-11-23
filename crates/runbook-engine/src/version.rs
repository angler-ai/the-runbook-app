//! Engine semantics versions (review F95).
//!
//! A run records the engine version it started under on its
//! `ExecutionStarted` event, so its stored log says which reducer rules
//! produced it. When a release changes what an already-recorded event
//! means, it bumps [`ENGINE_VERSION`] and keeps the older behaviour for
//! runs that recorded an older version. Logs from before the marker
//! existed have no field and read as [`LEGACY_ENGINE_VERSION`].
//!
//! - 1: logs recorded before the marker existed.
//! - 2: a task timeout follows `retry` and `on_failure` like any failure,
//!   and each new attempt or handoff gets a fresh task deadline (F4); a
//!   wait whose timeout equals its target time succeeds (F17).
//! - 3: a run whose only open work waits on people or timers reports
//!   `waiting` (R50). The reducer state is unchanged; only the reported
//!   status differs.
//! - 4: a wait never parks a run more than 7 days (PRD §17): an `until`
//!   target further away than that when the wait starts fails the wait
//!   step and the run with `limit.wait_exceeded`.
//! - 5: a run's outputs together are held to `MAX_OUTPUTS_BYTES` (CF13
//!   review round 1): a run that would succeed with more fails with
//!   `execution.outputs_too_large`, and a failed or cancelled run closes
//!   with its outputs past the budget null.
//! - 6: a run's event log has a hard ceiling (CF13b). A host that finds an
//!   open run's log past it records `ExecutionFaulted` with
//!   [`crate::RUN_SIZE_EXCEEDED`]: the run's work is cancelled, its cleanup
//!   is skipped and it closes failed (a cancelled run stays cancelled).
//!   Earlier versions refuse that fault, so runs under versions 1 to 5
//!   replay unchanged and are not held to the ceiling.
//! - 7: independent approval decisions exclude every recorded contributor.
//! - 8: independent review uses shared foreach iterations, amended results and service-token attribution.

/// The engine semantics this build writes for new runs.
pub const ENGINE_VERSION: u32 = 8;
/// The version of logs recorded before the marker existed.
pub const LEGACY_ENGINE_VERSION: u32 = 1;

/// Serde default for stored records written before the marker existed.
pub fn legacy_engine_version() -> u32 {
    LEGACY_ENGINE_VERSION
}

/// Version 2 rules: F4 (task timeouts are ordinary failures with fresh
/// per-attempt deadlines) and F17 (a wait's target wins a tie with its
/// timeout).
pub(crate) const TIMEOUT_IS_FAILURE: u32 = 2;
/// Version 3 rule: the reported status distinguishes `waiting` (R50).
pub(crate) const WAITING_STATUS: u32 = 3;
/// Version 4 rule: the 7-day cap on a single `until` wait (PRD §17).
pub(crate) const WAIT_CAP: u32 = 4;
/// Version 5 rule: the budget on a run's outputs together (CF13).
pub(crate) const OUTPUTS_BUDGET: u32 = 5;
/// Version 6 rule: the hard ceiling on a run's event log (CF13b).
pub(crate) const RUN_CEILING: u32 = 6;

/// Version 7 records independent approval decisions after filtering all contributors.
pub(crate) const INDEPENDENT_APPROVALS: u32 = 7;

pub(crate) const SCOPED_INDEPENDENT_APPROVALS: u32 = 8;

pub(crate) fn check(version: u32) -> Result<(), crate::EngineError> {
    if (LEGACY_ENGINE_VERSION..=ENGINE_VERSION).contains(&version) {
        Ok(())
    } else {
        Err(crate::EngineError::new(
            "execution.unsupported_engine_version",
        ))
    }
}
