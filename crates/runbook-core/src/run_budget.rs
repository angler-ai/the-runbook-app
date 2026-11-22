//! CF13 review round 1, item 2: a per-run budget on the stored event log.
//!
//! Every request on a run loads all of it (its events, and the projection
//! and engine state rebuilt from them), and nothing bounded how large a run
//! could grow: on Workers the old 2 MB head capped it by accident, and
//! CF13's split rows lifted that. The log is now held to a budget, the
//! same on both editions:
//!
//! - **What counts.** The bytes of the run's event log as every store keeps
//!   it (each envelope's JSON, [`log_bytes`]). Reports carry what grows a
//!   run (summaries, outputs, logs, evidence); the projection's step rows
//!   repeat a report's summary and outputs, so a run's stored data is at
//!   most about twice its log (lines appended through the logs route are
//!   paged from their own rows and never loaded with the run).
//! - **What is refused.** A request whose own event carries more than
//!   [`SMALL_WRITE_BYTES`] and would take the log past the budget is
//!   refused with `422` [`RUN_TOO_LARGE`] and writes nothing. Engine and
//!   system commits (timers, settling, a child's outcome) are never
//!   refused, and neither is a small write: a claim, a release, a cancel, a
//!   decision or a short report still passes, so a person can always finish
//!   or cancel a run at its budget. Past the budget a run grows only by
//!   such small writes and the engine's own bounded events.
//! - **The default** is [`DEFAULT_MAX_RUN_BYTES`] (8 MiB), configurable
//!   with `RUNBOOK_LIMIT_RUN_BYTES` (the server's environment, the api
//!   Worker's vars). Measured (`runbook-worker/tests/run_memory.rs`, the
//!   CF13 review round 1 decisions), one report request holds about 7.3
//!   times the log at its peak, so a run at 8 MiB peaks near 61 MB, half
//!   of a Worker isolate's 128 MB; 32 MiB would need about 240 MB.
//! - **The hard ceiling (CF13b).** The budget refuses only requests' large
//!   events; the engine's own (a call's result, a `set` step's new values)
//!   and small writes still grow a run past it. At [`run_ceiling`] (one and
//!   a half times the budget, 12 MiB by default) an open run under engine
//!   version 6 is faulted with `limit.run_size_exceeded` in the settle that
//!   passes it (its activation included) and closes failed in the same
//!   commit, without its cleanup (no `catch`, group or top-level `finally`),
//!   so no later request has to load more than the ceiling plus one settle
//!   round and the close.
use runbook_engine::{Event, EventEnvelope};

/// The default most a run's event log may hold before data-carrying writes
/// are refused.
pub const DEFAULT_MAX_RUN_BYTES: u64 = 8 * 1024 * 1024;
/// The problem code of a write refused by the budget (HTTP 422, not
/// retried: the same write is refused again).
pub const RUN_TOO_LARGE: &str = "execution.too_large";
/// A request's event of at most this many bytes is never refused by the
/// budget (it cannot grow the run meaningfully).
pub const SMALL_WRITE_BYTES: u64 = 4 * 1024;
/// What an envelope adds to its event: the run and actor IDs, sequence and
/// time (an upper bound, so a request is measured as it will be stored).
const ENVELOPE_BYTES: u64 = 512;

/// A `std::io::Write` that only counts.
struct Count(u64);
impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn json_bytes<T: serde::Serialize>(value: &T) -> u64 {
    let mut count = Count(0);
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        Err(_) => u64::MAX,
    }
}

/// The bytes of a stored event log: each envelope's JSON, as every store
/// keeps it. Counted without allocating the text.
pub fn log_bytes(events: &[EventEnvelope]) -> u64 {
    events
        .iter()
        .fold(0u64, |sum, event| sum.saturating_add(json_bytes(event)))
}

/// The bytes `event` will take in the log once it is in its envelope.
pub fn event_bytes(event: &Event) -> u64 {
    json_bytes(event).saturating_add(ENVELOPE_BYTES)
}

/// Whether a request's `event` may join a log of `logged` bytes under
/// `budget`: a small one always may; a larger one may if the log stays
/// within the budget.
pub fn admits(budget: u64, logged: u64, event: u64) -> bool {
    event <= SMALL_WRITE_BYTES || logged.saturating_add(event) <= budget
}

/// The hard ceiling on a run's event log, derived from its `budget`: one
/// and a half times it (CF13b). Not twice: a request holds about 7 to 8
/// times the log at its peak (`runbook-worker/tests/run_memory.rs`: 95.1 MB
/// at a 13.1 MB log, 106.8 MB at 15.1 MB, the ceiling and one closing
/// settle past it), so 16 MiB and its close would need about 134 MB, past
/// a Worker isolate's 128 MB.
pub fn run_ceiling(budget: u64) -> u64 {
    budget.saturating_add(budget / 2)
}

/// Read a budget from a variable's value: empty is the default, otherwise
/// a positive number of bytes.
pub fn parse_run_budget(value: Option<&str>) -> Result<u64, &'static str> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(DEFAULT_MAX_RUN_BYTES),
        Some(v) => v
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or("config.invalid_run_budget"),
    }
}

/// The most step runs one run may materialize (`RUNBOOK_LIMIT_STEP_RUNS`):
/// unset, empty or `off` is no cap (the default); otherwise a positive
/// integer. A run that would open one more fails with
/// `limit.step_runs_exceeded`, keeping what it did; publishing a runbook
/// whose worst case is over the cap is allowed, and the editor warns.
pub fn parse_step_cap(value: Option<&str>) -> Result<Option<u64>, &'static str> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None | Some("off") => Ok(None),
        Some(v) => v
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or("config.invalid_step_cap"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_write_always_passes_and_a_large_one_only_within_the_budget() {
        assert!(admits(100, 1_000_000, SMALL_WRITE_BYTES));
        assert!(!admits(100_000, 90_000, SMALL_WRITE_BYTES + 10_001));
        assert!(admits(100_000, 90_000 - 4_097, SMALL_WRITE_BYTES + 1));
        assert!(admits(100_000, 100_000 - 5_000, 5_000));
        assert!(!admits(100_000, 100_000 - 5_000, 5_001));
    }

    #[test]
    fn a_step_cap_is_off_unless_a_positive_number() {
        assert_eq!(parse_step_cap(None), Ok(None));
        assert_eq!(parse_step_cap(Some(" ")), Ok(None));
        assert_eq!(parse_step_cap(Some("off")), Ok(None));
        assert_eq!(parse_step_cap(Some(" 500 ")), Ok(Some(500)));
        for bad in ["0", "-1", "1e3", "lots"] {
            assert_eq!(parse_step_cap(Some(bad)), Err("config.invalid_step_cap"));
        }
    }

    #[test]
    fn the_ceiling_is_one_and_a_half_budgets() {
        assert_eq!(run_ceiling(DEFAULT_MAX_RUN_BYTES), 12 * 1024 * 1024);
        assert_eq!(run_ceiling(400_000), 600_000);
        assert_eq!(run_ceiling(u64::MAX), u64::MAX);
    }

    #[test]
    fn the_budget_reads_from_a_variable() {
        assert_eq!(parse_run_budget(None), Ok(DEFAULT_MAX_RUN_BYTES));
        assert_eq!(parse_run_budget(Some(" ")), Ok(DEFAULT_MAX_RUN_BYTES));
        assert_eq!(parse_run_budget(Some("1048576")), Ok(1_048_576));
        assert!(parse_run_budget(Some("0")).is_err());
        assert!(parse_run_budget(Some("off")).is_err());
        assert!(parse_run_budget(Some("-1")).is_err());
    }

    #[test]
    fn the_log_is_counted_as_stored() {
        let event = EventEnvelope {
            execution_id: "exe_00000000000000000000000001".parse().unwrap(),
            seq: 1,
            at_ms: 0,
            actor: runbook_model::ActorRef::System,
            event: Event::StepClaimed {
                step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
                agent_version_id: None,
                claim_id: None,
            },
        };
        let text = serde_json::to_vec(&event).unwrap().len() as u64;
        assert_eq!(log_bytes(&[event.clone(), event.clone()]), 2 * text);
        assert!(event_bytes(&event.event) >= text);
    }
}
