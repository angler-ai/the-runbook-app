//! CF5 (CF4-8): step log chunks. `append_logs` writes lines inline
//! (`claim_log`); the host then moves full chunks to R2 in two steps around
//! its R2 write ([`DoStore::log_flush`], then [`DoStore::log_flushed`]), and
//! reads stitch them back ([`DoStore::log_chunks`]). A chunk's boundaries
//! are a pure function of the inline rows (the oldest unflushed lines, up to
//! [`LOG_CHUNK_BYTES`]), so a flush retried after a crash writes the same
//! bytes to the same immutable key.
use crate::{
    DoSql, DoStore,
    sql::{Columns, int, uint},
};
use runbook_core::StoreError;
use runbook_model::{ExecutionId, StepRunId};

/// A chunk is flushed once this many bytes of lines wait inline.
pub const LOG_CHUNK_BYTES: u64 = 64 * 1024;

/// One chunk to move, or one already moved.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogChunk {
    pub chunk: u64,
    pub first_seq: u64,
    pub last_seq: u64,
    /// The chunk's size toward the step's log limit: each line's bytes, at
    /// least one per line (`runbook_core::claims::log_entry_bytes`). Chunks
    /// recorded before that rule counted blank lines as zero.
    pub bytes: u64,
}

/// A chunk to move and its lines, in sequence order.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingChunk {
    pub chunk: LogChunk,
    pub lines: Vec<String>,
}

/// The R2 blob name of a chunk (under the run's `BlobKey::Execution`).
pub fn chunk_name(step_run_id: &StepRunId, chunk: u64) -> String {
    format!("logs/{step_run_id}/{chunk:06}")
}

impl<B: DoSql> DoStore<B> {
    /// The next full chunk of `step_run_id`'s inline lines, if at least
    /// [`LOG_CHUNK_BYTES`] wait: the oldest lines up to that size (at least
    /// one line).
    pub fn log_flush(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
    ) -> Result<Option<PendingChunk>, StoreError> {
        self.owned_head(id)?;
        let waiting = self.sql.query(
            "SELECT COALESCE(SUM(max(length(CAST(line AS BLOB)), 1)), 0) FROM claim_log \
             WHERE step_run_id = ?",
            &[step_run_id.as_str().into()],
        )?;
        let waiting = waiting
            .first()
            .ok_or(StoreError::Backend("do.decode"))?
            .unsigned(0)?;
        if waiting < LOG_CHUNK_BYTES {
            return Ok(None);
        }
        let chunk = self.sql.query(
            "SELECT COALESCE(max(chunk) + 1, 0) FROM log_chunk WHERE step_run_id = ?",
            &[step_run_id.as_str().into()],
        )?;
        let chunk = chunk
            .first()
            .ok_or(StoreError::Backend("do.decode"))?
            .unsigned(0)?;
        let rows = self.sql.query(
            "SELECT seq, line FROM claim_log WHERE step_run_id = ? ORDER BY seq",
            &[step_run_id.as_str().into()],
        )?;
        let mut lines = vec![];
        let (mut first, mut last, mut bytes) = (0, 0, 0u64);
        for row in &rows {
            let line = row.text(1)?;
            let size = runbook_core::claims::log_entry_bytes(line);
            if !lines.is_empty() && bytes + size > LOG_CHUNK_BYTES {
                break;
            }
            let seq = row.unsigned(0)?;
            if lines.is_empty() {
                first = seq;
            }
            last = seq;
            bytes += size;
            lines.push(line.to_owned());
        }
        Ok((!lines.is_empty()).then_some(PendingChunk {
            chunk: LogChunk {
                chunk,
                first_seq: first,
                last_seq: last,
                bytes,
            },
            lines,
        }))
    }

    /// The chunk is in R2: record it and drop its inline lines, in one
    /// transaction. A chunk already recorded (a repeated flush) is a no-op;
    /// a chunk whose lines are no longer inline is refused
    /// (`log.flush_stale`) and nothing changes.
    pub fn log_flushed(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        chunk: &LogChunk,
    ) -> Result<bool, StoreError> {
        let (id, step_run_id, chunk) = (id.clone(), step_run_id.clone(), chunk.clone());
        self.write(move |store| {
            store.owned_head(&id)?;
            let recorded = store.sql.query(
                "SELECT first_seq, last_seq FROM log_chunk WHERE step_run_id = ? AND chunk = ?",
                &[step_run_id.as_str().into(), uint(chunk.chunk)?],
            )?;
            if let Some(row) = recorded.first() {
                return if (row.unsigned(0)?, row.unsigned(1)?) == (chunk.first_seq, chunk.last_seq)
                {
                    Ok(false)
                } else {
                    Err(StoreError::InvalidBatch("log.flush_stale"))
                };
            }
            let inline = store.sql.query(
                "SELECT count(*), COALESCE(SUM(max(length(CAST(line AS BLOB)), 1)), 0) FROM claim_log \
                 WHERE step_run_id = ? AND seq >= ? AND seq <= ?",
                &[
                    step_run_id.as_str().into(),
                    uint(chunk.first_seq)?,
                    uint(chunk.last_seq)?,
                ],
            )?;
            let inline = inline.first().ok_or(StoreError::Backend("do.decode"))?;
            if inline.unsigned(0)? != chunk.last_seq - chunk.first_seq + 1
                || inline.unsigned(1)? != chunk.bytes
            {
                return Err(StoreError::InvalidBatch("log.flush_stale"));
            }
            store.sql.exec(
                "INSERT INTO log_chunk (org_id, project_id, execution_id, step_run_id, chunk, \
                 first_seq, last_seq, bytes) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                &store.owned(
                    &id,
                    vec![
                        step_run_id.as_str().into(),
                        uint(chunk.chunk)?,
                        uint(chunk.first_seq)?,
                        uint(chunk.last_seq)?,
                        uint(chunk.bytes)?,
                    ],
                ),
            )?;
            store.sql.exec(
                "DELETE FROM claim_log WHERE step_run_id = ? AND seq >= ? AND seq <= ?",
                &[
                    step_run_id.as_str().into(),
                    uint(chunk.first_seq)?,
                    uint(chunk.last_seq)?,
                ],
            )?;
            Ok(true)
        })
    }

    /// `step_run_id`'s chunks in R2 that hold lines after `after`, in order.
    pub fn log_chunks(
        &self,
        id: &ExecutionId,
        step_run_id: &StepRunId,
        after: u64,
    ) -> Result<Vec<LogChunk>, StoreError> {
        self.owned_head(id)?;
        self.sql
            .query(
                "SELECT chunk, first_seq, last_seq, bytes FROM log_chunk \
                 WHERE step_run_id = ? AND last_seq > ? ORDER BY chunk",
                &[
                    step_run_id.as_str().into(),
                    int(i64::try_from(after).unwrap_or(i64::MAX))?,
                ],
            )?
            .iter()
            .map(|row| {
                Ok(LogChunk {
                    chunk: row.unsigned(0)?,
                    first_seq: row.unsigned(1)?,
                    last_seq: row.unsigned(2)?,
                    bytes: row.unsigned(3)?,
                })
            })
            .collect()
    }
}
