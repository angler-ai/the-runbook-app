//! CF5: the D1 half of `RunStore` (B2.4 export and deletion, ARC2
//! retention). A run lives in its ExecutionDO, D1 and R2; its deletion
//! commits here, in one batch: the tombstone, the owner row (its summary,
//! inbox, admission, replay, start-key, step-index and evidence rows
//! cascade), the step-run tombstones, the webhook rows, the token routes and
//! the archive bookkeeping. The batch also records an [`object_purge`] row:
//! the host empties the run's object and its R2 prefix after the commit and
//! then deletes the row, and the minute cron finishes any row a crash left.
//!
//! [`object_purge`]: ../../../cloudflare/migrations/d1/1006_object_purge.sql
use crate::{
    catalog::{D1Catalog, count, decode, err},
    sql::{Columns, D1, D1Error, Statement, guard, json, prepare, uint},
    uploads::mark_run_stale,
};
use runbook_core::{
    StoreError,
    archives::RETENTION_REASON,
    catalog::{Admission, AdmitRequest, RunCatalogSlice, TenantScope},
    retention::Tombstone,
};
use runbook_model::{ExecutionId, ExecutionStatus, RunbookId};

/// What a deletion needs checked under its batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletionChecks {
    /// ARC2: retention deletes only while the run's file is current and
    /// lists this revision of the run.
    pub archived_revision: Option<u64>,
}

/// A run whose object and R2 prefix still need emptying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingPurge {
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub keep_uploads: bool,
}

const FINISHED: &str = "('succeeded', 'failed', 'cancelled')";

impl<D: D1> D1Catalog<D> {
    /// This tenant's tombstone of `id`, if the run was deleted.
    pub async fn tombstone(&self, id: &ExecutionId) -> Result<Option<Tombstone>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM tombstone WHERE execution_id = ? AND org_id = ? \
                     AND project_id = ?",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }

    /// Finished live runs of `runbook` that completed on UTC day `day`
    /// (`[day*86400000, (day+1)*86400000)`), oldest first.
    pub async fn finished_on(
        &self,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        let start = day
            .checked_mul(86_400_000)
            .ok_or(StoreError::InvalidBatch("store.day_overflow"))?;
        let end = start
            .checked_add(86_400_000)
            .ok_or(StoreError::InvalidBatch("store.day_overflow"))?;
        self.db
            .all(
                prepare(format!(
                    "SELECT s.execution_id FROM run_summary s JOIN execution_owner o \
                     ON o.execution_id = s.execution_id \
                     WHERE o.org_id = ? AND o.project_id = ? AND s.runbook_id = ? \
                     AND s.status IN {FINISHED} AND s.completed_at_ms >= ? \
                     AND s.completed_at_ms < ? ORDER BY s.completed_at_ms, s.execution_id"
                ))
                .bind(self.org())
                .bind(self.project())
                .bind(runbook.as_str())
                .bind(start)
                .bind(end),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| row.parse(0).map_err(err))
            .collect()
    }

    /// The run's catalog rows for an export: its summary, inbox rows and
    /// admission (with the request it was admitted under).
    pub async fn export_catalog(&self, id: &ExecutionId) -> Result<RunCatalogSlice, StoreError> {
        let summary = self.get_run(id).await?;
        let assignments = self.list_assignments_of(id).await?;
        let admission = self
            .db
            .all(
                prepare(
                    "SELECT a.request, a.state, a.admitted_at_ms FROM admission a \
                     JOIN execution_owner o ON o.execution_id = a.execution_id \
                     WHERE a.execution_id = ? AND o.org_id = ? AND o.project_id = ?",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| -> Result<(AdmitRequest, Admission), StoreError> {
                Ok((
                    decode(row, 0)?,
                    Admission {
                        execution_id: id.clone(),
                        state: crate::admission::state_of(row.text(1).map_err(err)?)?,
                        admitted_at_ms: row.optional_integer(2).map_err(err)?,
                    },
                ))
            })
            .transpose()?;
        Ok(RunCatalogSlice {
            summary,
            assignments,
            admission,
        })
    }

    /// Commit `id`'s deletion (see the module docs). `runbook_id` is the
    /// run's, from its object. A retry after a commit is answered by the
    /// caller from the tombstone.
    pub async fn delete_run(
        &self,
        id: &ExecutionId,
        runbook_id: &RunbookId,
        reason: &str,
        checks: &DeletionChecks,
    ) -> Result<Tombstone, StoreError> {
        let retention = reason == RETENTION_REASON;
        let tombstone = Tombstone {
            scope: self.scope.clone(),
            execution_id: id.clone(),
            runbook_id: runbook_id.clone(),
            deleted_at_ms: self.db.now_ms(),
            reason: reason.into(),
        };
        let (org, project, run) = (self.org(), self.project(), id.as_str());
        let mut statements: Vec<Statement> = vec![
            guard(
                "not_found",
                "EXISTS (SELECT 1 FROM execution_owner WHERE execution_id = ? AND org_id = ? \
                 AND project_id = ?)",
            )
            .bind(run)
            .bind(org)
            .bind(project),
            guard(
                "catalog.admission_not_released",
                "NOT EXISTS (SELECT 1 FROM admission WHERE execution_id = ? \
                 AND state <> 'released')",
            )
            .bind(run),
        ];
        if let Some(revision) = checks.archived_revision {
            statements.push(
                guard(
                    "archive.not_current",
                    "EXISTS (SELECT 1 FROM archive_run r JOIN archive a \
                     ON a.org_id = r.org_id AND a.project_id = r.project_id \
                     AND a.runbook_id = r.runbook_id AND a.day = r.day \
                     WHERE r.execution_id = ? AND r.org_id = ? AND r.project_id = ? \
                     AND a.stale = 0 AND json_extract(r.body, '$.revision') = ?)",
                )
                .bind(run)
                .bind(org)
                .bind(project)
                .bind(uint(revision).map_err(err)?),
            );
        }
        statements.push(
            prepare(
                "INSERT INTO step_run_tombstone (step_run_id, execution_id, org_id, project_id) \
                 SELECT step_run_id, execution_id, org_id, project_id FROM step_run_index \
                 WHERE execution_id = ? ON CONFLICT DO NOTHING",
            )
            .bind(run),
        );
        if retention {
            // PRD §14: retention deletes only the live copy; the run's
            // uploads follow its archive file and keep counting.
            statements.push(
                prepare(format!(
                    "INSERT INTO retained_evidence \
                     (evidence_id, org_id, project_id, execution_id, counted_bytes) \
                     SELECT evidence_id, org_id, project_id, execution_id, {} \
                     FROM evidence WHERE execution_id = ? AND state <> 'rejected' \
                     ON CONFLICT DO NOTHING",
                    crate::uploads::COUNTED_BYTES
                ))
                .bind(run),
            );
        } else {
            // Any other deletion takes the run out of its file.
            statements.push(mark_run_stale(id));
            statements.push(prepare("DELETE FROM archive_run WHERE execution_id = ?").bind(run));
        }
        statements.extend([
            // CF11: the run's deliveries are in its subscriptions'
            // WebhookDOs and its token routes on the control database;
            // the purge after this batch forgets both
            // (`credentials::forget_execution_routes`).
            prepare(
                "DELETE FROM webhook_event WHERE org_id = ? AND project_id = ? \
                 AND execution_id = ?",
            )
            .bind(org)
            .bind(project)
            .bind(run),
            prepare("DELETE FROM admission_notice WHERE execution_id = ?").bind(run),
            prepare("DELETE FROM email_delivery WHERE execution_id = ?").bind(run),
            prepare(
                "DELETE FROM execution_owner WHERE execution_id = ? AND org_id = ? \
                 AND project_id = ?",
            )
            .bind(run)
            .bind(org)
            .bind(project),
            guard(
                "recorded",
                "NOT EXISTS (SELECT 1 FROM tombstone WHERE execution_id = ?)",
            )
            .bind(run),
            prepare(
                "INSERT INTO tombstone (execution_id, org_id, project_id, body) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(run)
            .bind(org)
            .bind(project)
            .bind(json(&tombstone).map_err(err)?),
            prepare(
                "INSERT INTO object_purge (execution_id, org_id, project_id, keep_uploads, \
                 created_at_ms) VALUES (?, ?, ?, ?, ?) ON CONFLICT (execution_id) DO NOTHING",
            )
            .bind(run)
            .bind(org)
            .bind(project)
            .bind(i64::from(retention))
            .bind(tombstone.deleted_at_ms),
        ]);
        match self.db.batch(statements).await {
            Ok(_) => Ok(tombstone),
            Err(D1Error::Guard(code)) if code == "recorded" => {
                self.tombstone(id).await?.ok_or(StoreError::NotFound)
            }
            Err(error) => Err(err(error)),
        }
    }

    /// The inbox rows of one run (for its export).
    async fn list_assignments_of(
        &self,
        id: &ExecutionId,
    ) -> Result<Vec<runbook_core::catalog::Assignment>, StoreError> {
        Ok(self
            .list_assignments()
            .await?
            .into_iter()
            .filter(|a| &a.execution_id == id)
            .collect())
    }
}

/// Runs whose deletion committed but whose object or R2 prefix may still
/// hold data, created before `before_ms`, oldest first (the cron's sweep).
pub async fn pending_purges<D: D1>(
    db: &D,
    before_ms: i64,
    limit: usize,
) -> Result<Vec<PendingPurge>, StoreError> {
    db.all(
        prepare(
            "SELECT org_id, project_id, execution_id, keep_uploads FROM object_purge \
             WHERE created_at_ms <= ? ORDER BY created_at_ms, execution_id LIMIT ?",
        )
        .bind(before_ms)
        .bind(count(limit)?),
    )
    .await
    .map_err(err)?
    .iter()
    .map(|row| {
        Ok(PendingPurge {
            scope: TenantScope {
                org_id: row.parse(0).map_err(err)?,
                project_id: row.parse(1).map_err(err)?,
            },
            execution_id: row.parse(2).map_err(err)?,
            keep_uploads: row.integer(3).map_err(err)? != 0,
        })
    })
    .collect()
}

/// Forget a finished purge.
pub async fn purge_done<D: D1>(db: &D, id: &ExecutionId) -> Result<(), StoreError> {
    db.run(prepare("DELETE FROM object_purge WHERE execution_id = ?").bind(id.as_str()))
        .await
        .map(|_| ())
        .map_err(err)
}

/// Whether a status is final (a finished run may be exported or deleted).
pub fn finished(status: &ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

/// Gate fix (low): the backoff after a failed cron attempt, as SQL over the
/// row's `attempts` (before this one): a minute, doubling, up to an hour.
pub(crate) const CRON_BACKOFF_SQL: &str = "min(3600000, 60000 * (1 << min(attempts, 6)))";

/// Gate fix (low): a run purge the cron could not finish waits its turn
/// again (`CRON_BACKOFF_SQL` from `now_ms`), so the next ones are reached.
pub async fn defer_purge<D: D1>(db: &D, id: &ExecutionId, now_ms: i64) -> Result<(), StoreError> {
    db.run(
        prepare(format!(
            "UPDATE object_purge SET created_at_ms = ? + {CRON_BACKOFF_SQL}, \
             attempts = attempts + 1 WHERE execution_id = ?"
        ))
        .bind(now_ms)
        .bind(id.as_str()),
    )
    .await
    .map(|_| ())
    .map_err(err)
}
