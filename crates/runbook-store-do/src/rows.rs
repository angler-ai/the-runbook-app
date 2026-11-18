//! CF13: a run's rows stay within what Durable Object SQLite keeps in one
//! row ([`MAX_ROW_BYTES`]), however long or wide the run grows.
//!
//! - The head (`meta.projection`) keeps the projection's run-level fields
//!   and no `steps`; each step run is its own `step_run` row (`body`, at
//!   `position`). A commit rewrites only the step rows that changed.
//! - A derived row (`derived_outbox.entry`) keeps no `assignments`; each
//!   open assignment is its own `derived_assignment` row.
//! - Every row these paths write is measured first, and one past the limit
//!   is refused with `422 execution.row_too_large` ([`ROW_TOO_LARGE`]): the
//!   request cannot succeed as it is, so no client retries it, as they
//!   would retry the `503 storage.unavailable` a failed write answers.
//!
//! A head or derived row written before CF13 holds `steps` or
//! `assignments` and is read as it is; a head is split by its run's next
//! commit. An older release refuses the new shapes (it requires the key)
//! instead of reading a run with no steps or no assignments.
use crate::{
    DoSql, DoStore,
    commit::status_text,
    sql::{Columns, Value, decode, encode, int, uint},
};
use runbook_core::{
    ExecutionProjection, MAX_ROW_BYTES, ROW_TOO_LARGE, StepProjection, StoreError,
    catalog::DerivedUpdate,
};
use runbook_model::ExecutionId;
use std::collections::BTreeSet;

/// What SQLite's record header and a value's own header may add to the
/// bytes [`row_bytes`] counts: the check refuses a little early rather
/// than let the object's write fail.
const ROW_SLACK: usize = 4_096;

/// The bytes a row of `values` holds, as [`check_row`] counts them.
pub(crate) fn row_bytes(values: &[Value]) -> usize {
    values
        .iter()
        .map(|value| match value {
            Value::Null => 1,
            Value::Integer(_) => 9,
            Value::Text(text) => text.len() + 9,
            Value::Blob(bytes) => bytes.len() + 9,
        })
        .sum()
}

/// Refuse a row the object could not write ([`ROW_TOO_LARGE`]).
pub(crate) fn check_row(values: &[Value]) -> Result<(), StoreError> {
    if row_bytes(values).saturating_add(ROW_SLACK) > MAX_ROW_BYTES {
        return Err(StoreError::InvalidBatch(ROW_TOO_LARGE));
    }
    Ok(())
}

/// The worst step row (CF13's budget), checked when this compiles: a
/// reported summary (64 KiB, kept once, in `body`, escaped there to at most
/// six times its bytes; CF13 review round 1 stopped writing the unread
/// `summary` column), a step's outputs (64 KiB as JSON) and 64 KiB for its
/// IDs, path, verdict and status, under the row with half to spare. A
/// `call` step's outputs are its child's run outputs, held to the engine's
/// `MAX_OUTPUTS_BYTES` (below).
const _: () = {
    let summary = runbook_core::claims::MAX_LOG_ENTRY_BYTES;
    let worst = 6 * summary + runbook_core::claims::MAX_STEP_OUTPUT_BYTES + 65_536;
    assert!(worst < MAX_ROW_BYTES / 2);
};

/// The rows that carry a run's outputs (CF13 review round 1), checked when
/// this compiles: the engine holds the outputs together to
/// `MAX_OUTPUTS_BYTES` in the stored form every one of these rows uses.
/// The head keeps them and `result` (a copy of one), at most twice that;
/// `ExecutionCompleted`, a parent's `CallCompleted` and its call step's row
/// keep them once. 64 KiB more for each row's other fields stays under it.
const _: () = {
    let outputs = runbook_engine::MAX_OUTPUTS_BYTES;
    assert!(2 * outputs + 65_536 + ROW_SLACK < MAX_ROW_BYTES);
};

/// The head's JSON: `projection` without its steps.
pub(crate) fn encode_head(projection: &ExecutionProjection) -> Result<String, StoreError> {
    let head = ExecutionProjection {
        execution_id: projection.execution_id.clone(),
        runbook_id: projection.runbook_id.clone(),
        version_id: projection.version_id.clone(),
        compiled_hash: projection.compiled_hash,
        engine_version: projection.engine_version,
        sequence: projection.sequence,
        status: projection.status,
        phase: projection.phase,
        completed_at_ms: projection.completed_at_ms,
        result: projection.result.clone(),
        outputs: projection.outputs.clone(),
        steps: Vec::new(),
    };
    without(&head, "steps")
}

/// `value` as JSON without its `key`.
fn without<T: serde::Serialize>(value: &T, key: &str) -> Result<String, StoreError> {
    let encoding = || StoreError::Backend("do.encode");
    let mut json = serde_json::to_value(value).map_err(|_| encoding())?;
    json.as_object_mut().ok_or_else(encoding)?.remove(key);
    serde_json::to_string(&json).map_err(|_| encoding())
}

/// Decode `text`; `None` for `key` when the JSON has none (a CF13 row,
/// whose `key` lives in rows of its own), with an empty list put in its
/// place.
fn with_list<T: serde::de::DeserializeOwned>(
    text: &str,
    key: &str,
) -> Result<(T, bool), StoreError> {
    let decoding = || StoreError::Backend("do.decode");
    let mut json: serde_json::Value = serde_json::from_str(text).map_err(|_| decoding())?;
    let object = json.as_object_mut().ok_or_else(decoding)?;
    let split = !object.contains_key(key);
    if split {
        object.insert(key.to_owned(), serde_json::Value::Array(vec![]));
    }
    Ok((serde_json::from_value(json).map_err(|_| decoding())?, split))
}

/// How the stored head is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeadShape {
    /// Written before CF13: the steps are in the head.
    Whole,
    /// CF13: each step run is a `step_run` row.
    Split,
}

impl<B: DoSql> DoStore<B> {
    /// `exec` a statement that writes one row of `params`, refused first
    /// when the row would pass the object's limit.
    pub(crate) fn put(&self, sql: &str, params: &[Value]) -> Result<(), StoreError> {
        check_row(params)?;
        self.sql.exec(sql, params)
    }

    /// The stored projection, whole, and how its head is laid out.
    pub(crate) fn read_projection(&self) -> Result<(ExecutionProjection, HeadShape), StoreError> {
        let meta = self
            .sql
            .query("SELECT projection FROM meta WHERE id = 1", &[])?;
        let text = meta.first().ok_or(StoreError::NotFound)?.text(0)?;
        let (mut projection, split): (ExecutionProjection, bool) = with_list(text, "steps")?;
        if !split {
            return Ok((projection, HeadShape::Whole));
        }
        projection.steps = self
            .sql
            .query("SELECT body FROM step_run ORDER BY position", &[])?
            .iter()
            .map(|row| decode(row.text(0)?))
            .collect::<Result<_, _>>()?;
        Ok((projection, HeadShape::Split))
    }

    /// Write the head at `revision` and the step rows of `projection`.
    /// `previous` is the stored steps when the stored head is split: only
    /// the step rows that changed are written. `None` rewrites every step
    /// row (a new or imported run, or a head written before CF13).
    pub(crate) fn write_projection(
        &self,
        id: &ExecutionId,
        revision: u64,
        projection: &ExecutionProjection,
        previous: Option<&[StepProjection]>,
    ) -> Result<(), StoreError> {
        self.put(
            "INSERT INTO meta (id, org_id, project_id, execution_id, revision, runbook_id, \
             version_id, status, completed_at_ms, projection) \
             VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET revision = excluded.revision, \
             status = excluded.status, completed_at_ms = excluded.completed_at_ms, \
             projection = excluded.projection",
            &self.owned(
                id,
                vec![
                    uint(revision)?,
                    projection.runbook_id.as_str().into(),
                    projection.version_id.as_str().into(),
                    status_text(&projection.status)?.into(),
                    projection
                        .completed_at_ms
                        .map(int)
                        .transpose()?
                        .unwrap_or(Value::Null),
                    encode_head(projection)?.into(),
                ],
            ),
        )?;
        let Some(previous) = previous else {
            self.sql.exec("DELETE FROM step_run", &[])?;
            for (position, step) in projection.steps.iter().enumerate() {
                self.write_step(id, position, step)?;
            }
            return Ok(());
        };
        for (position, step) in projection.steps.iter().enumerate() {
            if previous.get(position) != Some(step) {
                self.write_step(id, position, step)?;
            }
        }
        let current: BTreeSet<&str> = projection
            .steps
            .iter()
            .map(|step| step.step_run_id.as_str())
            .collect();
        for step in previous {
            if !current.contains(step.step_run_id.as_str()) {
                self.sql.exec(
                    "DELETE FROM step_run WHERE step_run_id = ?",
                    &[step.step_run_id.as_str().into()],
                )?;
            }
        }
        Ok(())
    }

    fn write_step(
        &self,
        id: &ExecutionId,
        position: usize,
        step: &StepProjection,
    ) -> Result<(), StoreError> {
        self.put(
            "INSERT INTO step_run (org_id, project_id, execution_id, step_run_id, path, status, \
             verdict, summary, position, body) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (step_run_id) DO UPDATE SET path = excluded.path, \
             status = excluded.status, verdict = excluded.verdict, summary = excluded.summary, \
             position = excluded.position, body = excluded.body",
            &self.owned(
                id,
                vec![
                    step.step_run_id.as_str().into(),
                    step.path.as_str().into(),
                    status_text(&step.status)?.into(),
                    match &step.verdict {
                        Some(verdict) => status_text(verdict)?.into(),
                        None => Value::Null,
                    },
                    // CF13 review round 1: nothing reads the `summary`
                    // index column (the summary is in `body`), so it is no
                    // longer written: a step row keeps its summary once.
                    Value::Null,
                    uint(position as u64)?,
                    encode(step)?,
                ],
            ),
        )
    }

    /// Record the derived row `update` at `sequence`, its assignments one
    /// row each.
    pub(crate) fn write_derived(
        &self,
        id: &ExecutionId,
        sequence: u64,
        at_ms: i64,
        update: &DerivedUpdate,
    ) -> Result<(), StoreError> {
        let entry = DerivedUpdate {
            summary: update.summary.clone(),
            assignments: Vec::new(),
            release_concurrency: update.release_concurrency,
            memory: update.memory.clone(),
        };
        self.put(
            "INSERT INTO derived_outbox (org_id, project_id, execution_id, sequence, at_ms, \
             entry) VALUES (?, ?, ?, ?, ?, ?)",
            &self.owned(
                id,
                vec![
                    uint(sequence)?,
                    int(at_ms)?,
                    without(&entry, "assignments")?.into(),
                ],
            ),
        )?;
        for (ordinal, assignment) in update.assignments.iter().enumerate() {
            self.put(
                "INSERT INTO derived_assignment (org_id, project_id, execution_id, sequence, \
                 ordinal, body) VALUES (?, ?, ?, ?, ?, ?)",
                &self.owned(
                    id,
                    vec![uint(sequence)?, uint(ordinal as u64)?, encode(assignment)?],
                ),
            )?;
        }
        Ok(())
    }

    /// The derived row `entry` recorded at `sequence`, with its assignments.
    pub(crate) fn read_derived(
        &self,
        sequence: u64,
        entry: &str,
    ) -> Result<DerivedUpdate, StoreError> {
        let (mut update, split): (DerivedUpdate, bool) = with_list(entry, "assignments")?;
        if split {
            update.assignments = self
                .sql
                .query(
                    "SELECT body FROM derived_assignment WHERE sequence = ? ORDER BY ordinal",
                    &[uint(sequence)?],
                )?
                .iter()
                .map(|row| decode(row.text(0)?))
                .collect::<Result<_, _>>()?;
        }
        Ok(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_at_the_limit_less_its_slack_passes_and_one_byte_more_does_not() {
        let fits = MAX_ROW_BYTES - ROW_SLACK - 9;
        assert_eq!(check_row(&[Value::Text("x".repeat(fits))]), Ok(()));
        assert_eq!(
            check_row(&[Value::Text("x".repeat(fits + 1))]),
            Err(StoreError::InvalidBatch(ROW_TOO_LARGE))
        );
        // Every value of the row counts.
        let half = Value::Blob(vec![0; MAX_ROW_BYTES / 2]);
        assert!(check_row(&[half.clone(), half]).is_err());
    }
}
