//! CF8: a run's object written whole from a D3 portable archive (the
//! Cloudflare import). [`DoStore::install`] is the inverse of what the
//! export reads from the object (`load`, `export_parts`, `start_record` and
//! the claim logs): every row of the stored run, in one storage transaction,
//! into an empty object, with the alarm re-armed from what it left behind.
//! Every row is measured as it is written (CF13 review round 1): one past
//! what an object row holds is `422 execution.row_too_large`, not a failed
//! storage write.
use crate::{
    DoSql, DoStore,
    commit::event_type,
    sql::{Columns, Value, encode, int, uint},
};
use runbook_core::{
    IdempotencyRecord, StoreError, StoredExecution, actor::replay_record_parts, portable::ClaimLog,
    retention::WorkState, start::StartRecord,
};
use runbook_model::ExecutionId;
use serde::{Deserialize, Serialize};

/// Everything one run's object holds, as an archive carries it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunInstall {
    pub run: StoredExecution,
    pub actor_replays: Vec<IdempotencyRecord>,
    pub work: Vec<WorkState>,
    pub start: Option<StartRecord>,
    pub claim_logs: Vec<ClaimLog>,
    /// When a run that has not started (revision 0) may be activated: its
    /// admission time, or the import time when it was not queued; `None`
    /// while it waits in its pool's queue.
    pub activation_at_ms: Option<i64>,
    /// Gate fix D2: install the run held ([`DoStore::release`] ends it).
    #[serde(default)]
    pub held: bool,
    /// Gate round 2: the import that holds it.
    #[serde(default)]
    pub import_id: String,
    /// Gate round 2: the import's D1 guards passed (no owner row, tombstone
    /// or webhook event names this run), so a stale held object here is an
    /// orphan of a failed import and may be replaced.
    #[serde(default)]
    pub replace_held: bool,
}

/// Gate round 2: a held run wakes this long after it was held; if its
/// import never committed (no owner row in D1), it is purged.
pub const HOLD_EXPIRY_MS: i64 = 3_600_000;
/// Gate round 2: a hold younger than this may belong to an import still
/// running, so no other import replaces it.
pub const HOLD_STALE_MS: i64 = 600_000;

fn invalid(code: &'static str) -> StoreError {
    StoreError::InvalidBatch(code)
}

impl<B: DoSql> DoStore<B> {
    /// Write `unit` into this empty object (`store.execution_exists`
    /// otherwise). Rows go in as stored: no event is replayed and nothing
    /// is validated against the engine, which the archive's decode did.
    pub fn install(&self, unit: RunInstall) -> Result<(), StoreError> {
        self.write(move |store| store.install_in(&unit))
    }

    fn install_in(&self, unit: &RunInstall) -> Result<(), StoreError> {
        let run = &unit.run;
        let projection = &run.projection;
        let id = projection.execution_id.clone();
        self.named(&id)?;
        if self.head()?.is_some() {
            // Gate round 2: a stale hold is a failed import's orphan; an
            // import whose D1 guards passed replaces it.
            match self.hold()? {
                Some((_, at)) if self.sql.now_ms() - at < HOLD_STALE_MS => {
                    return Err(invalid("import.in_progress"));
                }
                Some(_) if unit.replace_held => {
                    for table in crate::TABLES {
                        self.sql.exec(&format!("DELETE FROM {table}"), &[])?;
                    }
                }
                _ => return Err(invalid("store.execution_exists")),
            }
        }
        self.write_projection(&id, run.revision, projection, None)?;
        for event in &run.events {
            self.put(
                "INSERT INTO event (org_id, project_id, execution_id, seq, at_ms, type, envelope) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                &self.owned(
                    &id,
                    vec![
                        uint(event.seq)?,
                        int(event.at_ms)?,
                        event_type(event).into(),
                        encode(event)?,
                    ],
                ),
            )?;
        }
        for (table, timers) in [
            ("timer", &run.timers),
            ("timer_history", &run.timer_history),
        ] {
            for timer in timers.values() {
                crate::queue::alarm_time(timer.due_at_ms, "store.timer_out_of_range")?;
                self.put(
                    &format!(
                        "INSERT INTO {table} (org_id, project_id, execution_id, key, generation, \
                         due_at_ms) VALUES (?, ?, ?, ?, ?, ?)"
                    ),
                    &self.owned(
                        &id,
                        vec![
                            timer.key.as_str().into(),
                            uint(timer.generation)?,
                            int(timer.due_at_ms)?,
                        ],
                    ),
                )?;
            }
        }
        for intent in run.outbox.values() {
            self.insert_intent(&id, intent)?;
        }
        for record in run.idempotency.values() {
            self.put(
                "INSERT INTO idempotency (org_id, project_id, execution_id, key, request_hash, \
                 body) VALUES (?, ?, ?, ?, ?, ?)",
                &self.owned(
                    &id,
                    vec![
                        record.key.as_str().into(),
                        record.request_hash.to_vec().into(),
                        encode(record)?,
                    ],
                ),
            )?;
        }
        for claim in run.claims.values() {
            if let Some(deadline) = claim.lease_deadline_at_ms {
                crate::queue::alarm_time(deadline, "store.time_out_of_range")?;
            }
            self.write_claim(&id, claim)?;
        }
        let now = self.sql.now_ms();
        for update in run.derived_outbox.values() {
            self.write_derived(&id, update.summary.sequence, now, update)?;
        }
        for record in &unit.actor_replays {
            let (prefix, at) =
                replay_record_parts(record)?.ok_or(invalid("replay.invalid_record"))?;
            self.put(
                "INSERT INTO actor_replay (org_id, project_id, execution_id, prefix, at_ms, body) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                &self.owned(&id, vec![prefix.into(), int(at)?, encode(record)?]),
            )?;
        }
        for work in &unit.work {
            if work.lease.id.execution_id != id {
                return Err(invalid("import.invalid_work"));
            }
            crate::queue::alarm_time(work.lease.deadline_at_ms, "store.time_out_of_range")?;
            let (kind, key, work_generation) = crate::queue::columns(&work.lease.id.kind)?;
            self.put(
                "INSERT INTO work_lease (org_id, project_id, execution_id, kind, work_key, \
                 work_generation, generation, deadline_at_ms, acknowledged) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                &self.owned(
                    &id,
                    vec![
                        kind.into(),
                        key.into(),
                        work_generation,
                        uint(work.lease.generation)?,
                        int(work.lease.deadline_at_ms)?,
                        int(i64::from(work.acknowledged))?,
                    ],
                ),
            )?;
        }
        if let Some(start) = &unit.start {
            let activation = match unit.activation_at_ms {
                Some(at) if run.revision == 0 => {
                    crate::queue::alarm_time(at, "store.time_out_of_range")?;
                    int(at)?
                }
                _ => Value::Null,
            };
            self.put(
                "INSERT INTO start_record (id, org_id, project_id, execution_id, body, \
                 activation_at_ms) VALUES (1, ?, ?, ?, ?, ?)",
                &self.owned(&id, vec![encode(start)?, activation]),
            )?;
        }
        for log in &unit.claim_logs {
            for (n, line) in log.lines.iter().enumerate() {
                self.put(
                    "INSERT INTO claim_log (org_id, project_id, execution_id, step_run_id, seq, \
                     line) VALUES (?, ?, ?, ?, ?, ?)",
                    &self.owned(
                        &id,
                        vec![
                            log.step_run_id.as_str().into(),
                            uint(n as u64 + 1)?,
                            line.as_str().into(),
                        ],
                    ),
                )?;
            }
        }
        if unit.held {
            self.sql.exec(
                "INSERT INTO import_hold (id, held_at_ms, import_id) VALUES (1, ?, ?)",
                &[int(now)?, unit.import_id.as_str().into()],
            )?;
        }
        self.bump_writes()
    }

    /// Gate fix D2: whether this object's run is held by an import that has
    /// not reached its D1 commit point (or not been released since).
    pub fn held(&self) -> Result<bool, StoreError> {
        Ok(self.hold()?.is_some())
    }

    /// Gate round 2: the import holding this run, and since when.
    pub fn hold(&self) -> Result<Option<(String, i64)>, StoreError> {
        self.sql
            .query(
                "SELECT import_id, held_at_ms FROM import_hold WHERE id = 1",
                &[],
            )?
            .first()
            .map(|row| Ok((row.text(0)?.to_owned(), row.integer(1)?)))
            .transpose()
    }

    /// Release a held run: its alarm is armed again and its outboxes are
    /// handed out. True when it was held. Idempotent; an empty object is
    /// `NotFound`.
    pub fn release(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        let id = id.clone();
        self.write(move |store| {
            store.owned_head(&id)?;
            let held = store.held()?;
            if held {
                store
                    .sql
                    .exec("DELETE FROM import_hold WHERE id = 1", &[])?;
            }
            Ok(held)
        })
    }
}

/// One step's log as its object holds it: the chunks moved to R2 (each
/// read there by `logs::chunk_name`), then the lines still inline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepLog {
    pub step_run_id: runbook_model::StepRunId,
    pub chunks: Vec<crate::logs::LogChunk>,
    /// `(seq, line)`, in order.
    pub inline: Vec<(u64, String)>,
}

/// CF12 review round 1: everything an archive needs from one run's object,
/// in one object request (the export's per-run cost): the run, its actor
/// replays, work leases and start record, and each step's log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunExport {
    pub run: StoredExecution,
    pub actor_replays: Vec<IdempotencyRecord>,
    pub work: Vec<WorkState>,
    pub start: Option<StartRecord>,
    pub logs: Vec<StepLog>,
}

impl RunExport {
    /// The claim logs, given each chunk's lines (`chunk_lines(step, chunk)`,
    /// read from R2), as the composed store's `read_logs` pages them: the
    /// chunks' lines in order, then the inline lines after the last of
    /// them. Steps without lines are left out.
    pub fn claim_logs(
        &self,
        mut chunk_lines: impl FnMut(
            &runbook_model::StepRunId,
            &crate::logs::LogChunk,
        ) -> Result<Vec<String>, StoreError>,
    ) -> Result<Vec<ClaimLog>, StoreError> {
        let mut out = vec![];
        for step in &self.logs {
            let mut lines = vec![];
            let mut cursor = 0;
            for chunk in &step.chunks {
                for (seq, line) in (chunk.first_seq..).zip(chunk_lines(&step.step_run_id, chunk)?) {
                    if seq > cursor {
                        lines.push(line);
                        cursor = seq;
                    }
                }
            }
            lines.extend(
                step.inline
                    .iter()
                    .filter(|(seq, _)| *seq > cursor)
                    .map(|(_, line)| line.clone()),
            );
            if !lines.is_empty() {
                out.push(ClaimLog {
                    step_run_id: step.step_run_id.clone(),
                    lines,
                });
            }
        }
        Ok(out)
    }
}

impl<B: DoSql> DoStore<B> {
    /// [`RunExport`] of `id`, read locally.
    pub fn export_run(&self, id: &ExecutionId) -> Result<RunExport, StoreError> {
        let run = self.load_in(id)?;
        let (actor_replays, work) = self.export_parts(id)?;
        let start = self.start_record(id)?;
        let mut logs = vec![];
        for step in &run.projection.steps {
            let chunks = self.log_chunks(id, &step.step_run_id, 0)?;
            let mut inline = vec![];
            loop {
                let after = inline.last().map_or(0, |(seq, _): &(u64, String)| *seq);
                let page = self.inline_logs(id, &step.step_run_id, after)?;
                if page.is_empty() {
                    break;
                }
                inline.extend(page);
            }
            logs.push(StepLog {
                step_run_id: step.step_run_id.clone(),
                chunks,
                inline,
            });
        }
        Ok(RunExport {
            run,
            actor_replays,
            work,
            start,
            logs,
        })
    }

    /// Up to 100,000 of the step's inline log lines after `after`.
    fn inline_logs(
        &self,
        id: &ExecutionId,
        step_run_id: &runbook_model::StepRunId,
        after: u64,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        self.owned_head(id)?;
        self.sql
            .query(
                "SELECT seq, line FROM claim_log WHERE step_run_id = ? AND seq > ? \
                 ORDER BY seq LIMIT 100000",
                &[step_run_id.as_str().into(), uint(after)?],
            )?
            .iter()
            .map(|row| Ok((row.unsigned(0)?, row.text(1)?.to_owned())))
            .collect()
    }
}
