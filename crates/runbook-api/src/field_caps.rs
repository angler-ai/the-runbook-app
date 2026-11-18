//! CF12b: caps on request values that a store keeps whole in one row.
//!
//! The JSON body cap (`http::JSON_BODY_BYTES`, 2 MiB + 64 KiB) admits a
//! single value larger than the 2,000,000 bytes D1 and Durable Object
//! SQLite hold in one string or row, so on Workers such a request failed
//! with a retryable 503 that could never succeed. These caps hold each such
//! value under the row limit, on both editions alike (they live here, in
//! the shared API), and refuse more with `422 request.value_too_large`
//! naming the value's path. Values with a product limit of their own keep
//! it: runbook sources (1 MiB), step outputs (64 KiB), memory entries
//! (4 KiB), project variables (1 MiB a project), webhook fields.
//!
//! - [`TEXT_BYTES`]: one free-text value (a log line, a summary, a comment
//!   or reason, a skill's reference). 64 KiB, the engine's cap on one
//!   string of run data and the size of a log chunk.
//! - [`NAME_BYTES`]: a name or identity (an organization, project or team
//!   title, a user's name, an invited email, an agent's session, name and
//!   model). Each is copied into many rows (every event an agent signs).
//! - [`DATA_BYTES`]: the run data one request carries as JSON (a start's
//!   inputs, fields, title and tags; a signal's payload; a patch; an
//!   override's record). 256 KiB: a run's start record keeps it beside the
//!   project's variables (up to 1 MiB) and the tags twice, and stays under
//!   2,000,000 bytes.
//! - [`REPORT_LOG_BYTES`]: the log lines one report carries (1 MiB as
//!   JSON), which its event keeps whole; the step's 5 MiB is reached
//!   through the logs route, one row an entry. A line is not capped here:
//!   core splits a line longer than one entry after redaction
//!   (`runbook_core::claims::split_log_lines`), and holds a report's logs
//!   to `MAX_REPORT_LOG_JSON_BYTES` once redacted (CF12b review round 1).
//! - [`EVIDENCE_ENTRIES`] of at most [`EVIDENCE_ENTRY_BYTES`] each on one
//!   report (8 KiB: a presigned link with a session token passes 2 KiB),
//!   and all of them at most [`EVIDENCE_TOTAL_BYTES`] as JSON.
//!
//! The aggregate caps measure the JSON a row keeps (escaped); a single
//! value's cap measures its bytes, so its JSON is at most six times that.
//! - [`SKILL_FILES`]: a skill bundle's files (PRD: 200), whose manifest is
//!   one row.

use runbook_model::Diagnostic;

pub const TEXT_BYTES: usize = 65_536;
pub const NAME_BYTES: usize = 1_024;
pub const DATA_BYTES: usize = 262_144;
pub const REPORT_LOG_BYTES: usize = 1_048_576;
pub const EVIDENCE_ENTRIES: usize = 100;
pub const EVIDENCE_ENTRY_BYTES: usize = 8_192;
pub const EVIDENCE_TOTAL_BYTES: usize = 204_800;
pub const SKILL_FILES: usize = 200;
/// The caps keep the worst cases under the 2,000,000 bytes D1 and Durable
/// Object SQLite hold in a row, with 64 KiB to spare for the rest of it,
/// checked when this compiles:
/// - a start record: the data, its tags again, and a full set of
///   variables (1 MiB);
/// - a report's event: its logs once redacted (which core bounds), its
///   summary (escaped at worst), its outputs (64 KiB) and its evidence.
///
/// The start record also keeps the version's input defaults and spec tags,
/// which these caps cannot see; core measures the merged record before it
/// writes (`runbook_core::start::MAX_START_ROW_BYTES`).
const _: () = {
    const ROW: usize = 2_000_000 - 65_536;
    assert!(DATA_BYTES * 2 + 1_048_576 < ROW);
    assert!(REPORT_LOG_BYTES <= runbook_core::claims::MAX_REPORT_LOG_JSON_BYTES);
    assert!(
        runbook_core::claims::MAX_REPORT_LOG_JSON_BYTES
            + 6 * TEXT_BYTES
            + runbook_core::claims::MAX_STEP_OUTPUT_BYTES
            + EVIDENCE_TOTAL_BYTES
            < ROW
    );
    assert!(EVIDENCE_TOTAL_BYTES >= EVIDENCE_ENTRY_BYTES);
    assert!(TEXT_BYTES == runbook_core::claims::MAX_LOG_ENTRY_BYTES);
};
/// The code every refusal here answers with (422).
pub const CODE: &str = "request.value_too_large";

/// A value past its cap: where it is in the request, and the cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TooLarge {
    pub path: String,
    pub limit: usize,
}
impl TooLarge {
    pub fn diagnostic(&self) -> Diagnostic {
        Diagnostic {
            path: self.path.clone(),
            code: CODE.into(),
            message: format!("at most {} bytes", self.limit),
            line: None,
            column: None,
        }
    }
}
/// The directory handlers' error (status, code).
impl From<TooLarge> for (u16, &'static str) {
    fn from(_: TooLarge) -> Self {
        (422, CODE)
    }
}
impl From<TooLarge> for crate::executions::ExecutionApiError {
    fn from(e: TooLarge) -> Self {
        Self {
            status: 422,
            code: CODE,
            diagnostics: vec![e.diagnostic()],
        }
    }
}

/// `value` (at `path`) is at most `limit` bytes.
pub fn text(path: &str, value: &str, limit: usize) -> Result<(), TooLarge> {
    if value.len() > limit {
        return Err(TooLarge {
            path: path.into(),
            limit,
        });
    }
    Ok(())
}

/// [`text`] for an optional value.
pub fn optional(path: &str, value: Option<&str>, limit: usize) -> Result<(), TooLarge> {
    value.map_or(Ok(()), |v| text(path, v, limit))
}

/// `value` (at `path`) is at most `limit` bytes as JSON.
pub fn json<T: serde::Serialize + ?Sized>(
    path: &str,
    value: &T,
    limit: usize,
) -> Result<(), TooLarge> {
    let bytes = serde_json::to_vec(value).map_or(usize::MAX, |b| b.len());
    if bytes > limit {
        return Err(TooLarge {
            path: path.into(),
            limit,
        });
    }
    Ok(())
}

/// An agent's identity on a request: its session and declaration.
pub fn actor(
    session_id: Option<&str>,
    actor: Option<&runbook_model::ActorDeclaration>,
) -> Result<(), TooLarge> {
    optional("/session_id", session_id, NAME_BYTES)?;
    if let Some(actor) = actor {
        optional("/actor/agent_name", actor.agent_name.as_deref(), NAME_BYTES)?;
        optional("/actor/model", actor.model.as_deref(), NAME_BYTES)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_at_its_cap_passes_and_one_byte_more_does_not() {
        assert_eq!(text("/s", &"a".repeat(TEXT_BYTES), TEXT_BYTES), Ok(()));
        let e = text("/s", &"a".repeat(TEXT_BYTES + 1), TEXT_BYTES).unwrap_err();
        assert_eq!(e.path, "/s");
        assert_eq!(e.diagnostic().code, CODE);
    }

    /// Measured as JSON, as a row keeps it: 1 KiB of control characters
    /// is 6 KiB escaped.
    #[test]
    fn aggregates_are_measured_as_json() {
        let escaped = vec!["\u{1}".repeat(1_024)];
        assert!(json("/logs", &escaped, 6_000).is_err());
        assert!(json("/logs", &escaped, 6_200).is_ok());
    }
}
