//! The D1 half of a start (B5a, CF2-8): the replay check, the owner claim,
//! pooled admission (D24/D25, LANG3 `queue`/`reject`) and the saved replay,
//! in one guarded batch. This is the start's commit point: the host then
//! initializes the run's ExecutionDO with the outcome (its head, start
//! record, `execution.queued` intent and sequence-0 derived row), which is
//! idempotent, so a retry of the same key finishes a start whose object
//! write was lost.
use crate::{
    admission::{Promotion, claim_owner},
    catalog::{D1Catalog, decode, err},
    sql::{Columns, D1, D1Error, guard, json, prepare},
};
use runbook_core::{
    StoreError,
    start::{REPLAY_RETENTION_MS, StartCommit, StartOutcome, StartReplay},
};

/// What the D1 half decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Reserved {
    pub outcome: StartOutcome,
    /// A saved outcome answered this start (same key, same request).
    pub replayed: bool,
    /// Waiters a raised limit promoted (their objects are told).
    pub promoted: Vec<Promotion>,
    /// LANG3 `cancel_previous`: the runs this start evicted, each with a
    /// `supersede_notice` row its object applies after the batch.
    pub superseded: Vec<runbook_core::concurrency::PoolHolder>,
}

/// Plans per start before the pool is reported contended.
const START_ATTEMPTS: usize = 8;

impl<D: D1> D1Catalog<D> {
    /// The saved start for `key`, if younger than the retention window
    /// (older rows are purged and read as absent, D20).
    pub async fn find_start_replay(&self, key: &str) -> Result<Option<StartReplay>, StoreError> {
        let now = self.db.now_ms();
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT principal, request_hash, outcome, created_at_ms FROM start_replay \
                     WHERE org_id = ? AND project_id = ? AND idempotency_key = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(key),
            )
            .await
            .map_err(err)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        if now - row.integer(3).map_err(err)? >= REPLAY_RETENTION_MS {
            self.db
                .run(
                    prepare(
                        "DELETE FROM start_replay WHERE org_id = ? AND project_id = ? \
                         AND idempotency_key = ? AND created_at_ms <= ?",
                    )
                    .bind(self.org())
                    .bind(self.project())
                    .bind(key)
                    .bind(now - REPLAY_RETENTION_MS),
                )
                .await
                .map_err(err)?;
            return Ok(None);
        }
        Ok(Some(StartReplay {
            key: key.to_owned(),
            principal: row.string(0).map_err(err)?,
            request_hash: row.digest(1).map_err(err)?,
            outcome: decode(row, 2)?,
        }))
    }

    /// Commit the D1 half of `commit`: replay, owner, admission, replay row.
    pub async fn reserve_start(&self, commit: &StartCommit) -> Result<Reserved, StoreError> {
        let id = &commit.projection.execution_id;
        for _ in 0..START_ATTEMPTS {
            if let Some(replay) = &commit.replay
                && let Some(saved) = self.find_start_replay(&replay.key).await?
            {
                if saved.principal != replay.principal || saved.request_hash != replay.request_hash
                {
                    return Err(StoreError::InvalidBatch("request.idempotency_key_reused"));
                }
                return Ok(Reserved {
                    outcome: saved.outcome,
                    replayed: true,
                    promoted: vec![],
                    superseded: vec![],
                });
            }
            let now = self.db.now_ms();
            let mut statements = vec![
                guard(
                    "not_found",
                    "NOT EXISTS (SELECT 1 FROM tombstone WHERE execution_id = ?)",
                )
                .bind(id.as_str()),
            ];
            if let Some(replay) = &commit.replay {
                statements.push(
                    guard(
                        "replayed",
                        "NOT EXISTS (SELECT 1 FROM start_replay WHERE org_id = ? \
                         AND project_id = ? AND idempotency_key = ?)",
                    )
                    .bind(self.org())
                    .bind(self.project())
                    .bind(replay.key.as_str()),
                );
            }
            // Another tenant's run with this ID reads as absent.
            statements.push(
                guard(
                    "not_found",
                    "NOT EXISTS (SELECT 1 FROM execution_owner WHERE execution_id = ? \
                     AND (org_id <> ? OR project_id <> ?))",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            );
            statements.push(
                guard(
                    "store.execution_exists",
                    "NOT EXISTS (SELECT 1 FROM execution_owner WHERE execution_id = ?)",
                )
                .bind(id.as_str()),
            );
            statements.extend(claim_owner(&self.scope, id));
            let superseded_by = runbook_engine::Supersession {
                execution_id: id.clone(),
                started_by: commit.record.context.created_by.clone(),
            };
            let (admission, promoted, superseded) = match &commit.admission {
                Some(request) => {
                    let start = match self
                        .plan_start_admission(request, commit.on_conflict.as_ref(), &superseded_by)
                        .await
                    {
                        Ok(start) => start,
                        Err(StoreError::InvalidBatch("catalog.admission_stale")) => continue,
                        Err(error) => return Err(error),
                    };
                    statements.extend(start.plan.statements);
                    (
                        Some(start.plan.admission),
                        start.plan.promoted,
                        start.evicted,
                    )
                }
                None => (None, vec![], vec![]),
            };
            let outcome = StartOutcome {
                execution_id: id.clone(),
                runbook_version_id: commit.record.version_id.clone(),
                runbook_version: commit.record.version,
                admission,
            };
            if let Some(replay) = &commit.replay {
                statements.push(
                    prepare(
                        "INSERT INTO start_replay (org_id, project_id, idempotency_key, principal, \
                         request_hash, execution_id, outcome, created_at_ms) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(self.org())
                    .bind(self.project())
                    .bind(replay.key.as_str())
                    .bind(replay.principal.as_str())
                    .bind(replay.request_hash.to_vec())
                    .bind(id.as_str())
                    .bind(json(&outcome).map_err(err)?)
                    .bind(now),
                );
            }
            match self.db.batch(statements).await {
                Ok(_) => {
                    return Ok(Reserved {
                        outcome,
                        replayed: false,
                        promoted,
                        superseded,
                    });
                }
                Err(D1Error::Guard(code))
                    if code == "replayed" || code == "catalog.admission_stale" => {}
                Err(D1Error::Guard(code)) if code == "not_found" => {
                    return Err(StoreError::NotFound);
                }
                Err(error) => return Err(err(error)),
            }
        }
        Err(StoreError::Backend("d1.admission_contended"))
    }
}

/// How long a reserved start may stay without its run's first derived row
/// before the reconciler looks at it (the object normally initializes and
/// drains within the start request).
pub const START_GRACE_MS: i64 = 5 * 60_000;

/// Starts reserved before `before_ms` whose run never reached D1 (no
/// summary row) and that are not tombstoned, oldest first.
pub async fn unsettled_starts<D: D1>(
    db: &D,
    before_ms: i64,
    limit: usize,
) -> Result<
    Vec<(
        runbook_core::catalog::TenantScope,
        runbook_model::ExecutionId,
    )>,
    StoreError,
> {
    Ok(unsettled_starts_after(db, before_ms, None, limit)
        .await?
        .into_iter()
        .map(|(scope, id, _)| (scope, id))
        .collect())
}

/// Gate fix (low): [`unsettled_starts`] after a cursor (the last one's
/// start time and run ID), so a cron pass pages past starts it already
/// tried instead of rereading the oldest page. Each comes with its start
/// time (the next cursor).
pub async fn unsettled_starts_after<D: D1>(
    db: &D,
    before_ms: i64,
    after: Option<(i64, &runbook_model::ExecutionId)>,
    limit: usize,
) -> Result<
    Vec<(
        runbook_core::catalog::TenantScope,
        runbook_model::ExecutionId,
        i64,
    )>,
    StoreError,
> {
    // Times are positive; -1 (a JS-safe number, as D1 needs) precedes all.
    let (at, id) = after.map_or((-1, ""), |(at, id)| (at, id.as_str()));
    db.all(
        prepare(
            "SELECT r.org_id, r.project_id, r.execution_id, r.created_at_ms FROM start_replay r \
             WHERE r.created_at_ms <= ? AND (r.created_at_ms, r.execution_id) > (?, ?) \
             AND NOT EXISTS (SELECT 1 FROM run_summary s WHERE s.execution_id = r.execution_id) \
             AND NOT EXISTS (SELECT 1 FROM tombstone t WHERE t.execution_id = r.execution_id) \
             ORDER BY r.created_at_ms, r.execution_id LIMIT ?",
        )
        .bind(before_ms)
        .bind(at)
        .bind(id)
        .bind(crate::catalog::count(limit)?),
    )
    .await
    .map_err(err)?
    .iter()
    .map(|row| {
        Ok((
            runbook_core::catalog::TenantScope {
                org_id: row.parse(0).map_err(err)?,
                project_id: row.parse(1).map_err(err)?,
            },
            row.parse(2).map_err(err)?,
            row.integer(3).map_err(err)?,
        ))
    })
    .collect()
}

impl<D: D1> D1Catalog<D> {
    /// Review fix 2: a start whose D1 half committed but whose object never
    /// initialized (and no retry finished it). Release its admission (its
    /// waiters move on), free its replay key, and tombstone the run with
    /// reason `start.abandoned`, so reads answer 410 and the ID is never
    /// started later. Guarded: a run that reached D1 meanwhile is left alone.
    pub async fn abandon_start(&self, id: &runbook_model::ExecutionId) -> Result<bool, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT v.runbook_id FROM start_replay r JOIN runbook_version v \
                     ON v.version_id = json_extract(r.outcome, '$.runbook_version_id') \
                     WHERE r.execution_id = ? AND r.org_id = ? AND r.project_id = ?",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?;
        let runbook_id: runbook_model::RunbookId = match rows.first() {
            Some(row) => row.parse(0).map_err(err)?,
            None => "rb_00000000000000000000000000"
                .parse()
                .map_err(|_| StoreError::Backend("d1.decode"))?,
        };
        self.release(id).await?;
        let tombstone = runbook_core::retention::Tombstone {
            scope: self.scope.clone(),
            execution_id: id.clone(),
            runbook_id,
            deleted_at_ms: self.db.now_ms(),
            reason: "start.abandoned".into(),
        };
        let statements = vec![
            guard(
                "recorded",
                "NOT EXISTS (SELECT 1 FROM run_summary WHERE execution_id = ?)",
            )
            .bind(id.as_str()),
            // The owner row goes as retention's does (its start key, replay
            // and admission rows cascade with it), so reads find the
            // tombstone and answer 410.
            prepare("DELETE FROM execution_owner WHERE execution_id = ? AND org_id = ? AND project_id = ?")
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            prepare(
                "INSERT INTO tombstone (execution_id, org_id, project_id, body) VALUES (?, ?, ?, ?) \
                 ON CONFLICT (execution_id) DO NOTHING",
            )
            .bind(id.as_str())
            .bind(self.org())
            .bind(self.project())
            .bind(json(&tombstone).map_err(err)?),
        ];
        match self.db.batch(statements).await {
            Ok(_) => Ok(true),
            Err(D1Error::Guard(code)) if code == "recorded" => Ok(false),
            Err(error) => Err(err(error)),
        }
    }
}

/// One evicted run whose object has not applied its cancellation yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersedeNotice {
    pub scope: runbook_core::catalog::TenantScope,
    pub run: runbook_core::concurrency::PoolHolder,
    pub superseded: runbook_engine::Supersession,
}

/// Supersede notices recorded before `before_ms`, oldest first (the
/// minute cron delivers what the starting host did not).
pub async fn pending_supersedes<D: D1>(
    db: &D,
    before_ms: i64,
    limit: usize,
) -> Result<Vec<SupersedeNotice>, StoreError> {
    db.all(
        prepare(
            "SELECT org_id, project_id, execution_id, state, superseded_by, started_by \
             FROM supersede_notice WHERE created_at_ms <= ? \
             ORDER BY created_at_ms, execution_id LIMIT ?",
        )
        .bind(before_ms)
        .bind(crate::catalog::count(limit)?),
    )
    .await
    .map_err(err)?
    .iter()
    .map(|row| {
        Ok(SupersedeNotice {
            scope: runbook_core::catalog::TenantScope {
                org_id: row.parse(0).map_err(err)?,
                project_id: row.parse(1).map_err(err)?,
            },
            run: runbook_core::concurrency::PoolHolder {
                execution_id: row.parse(2).map_err(err)?,
                state: crate::admission::state_of(row.text(3).map_err(err)?)?,
            },
            superseded: runbook_engine::Supersession {
                execution_id: row.parse(4).map_err(err)?,
                started_by: row.string(5).map_err(err)?,
            },
        })
    })
    .collect()
}

/// Forget a delivered supersede notice.
pub async fn ack_supersede<D: D1>(
    db: &D,
    id: &runbook_model::ExecutionId,
) -> Result<(), StoreError> {
    db.run(prepare("DELETE FROM supersede_notice WHERE execution_id = ?").bind(id.as_str()))
        .await
        .map(|_| ())
        .map_err(err)
}

impl<D: D1> D1Catalog<D> {
    /// LANG3: a start whose evictions admitted it at once answers (and
    /// replays) admitted, as a native start does when the holder closes in
    /// the start transaction.
    pub async fn update_start_replay(
        &self,
        key: &str,
        outcome: &StartOutcome,
    ) -> Result<(), StoreError> {
        self.db
            .run(
                prepare(
                    "UPDATE start_replay SET outcome = ? WHERE org_id = ? AND project_id = ? \
                     AND idempotency_key = ?",
                )
                .bind(json(outcome).map_err(err)?)
                .bind(self.org())
                .bind(self.project())
                .bind(key),
            )
            .await
            .map(|_| ())
            .map_err(err)
    }
}

/// Gate fix (low): a supersede notice the cron could not deliver waits its
/// turn again (a minute, doubling, up to an hour), so the next ones are
/// reached.
pub async fn defer_supersede<D: D1>(
    db: &D,
    id: &runbook_model::ExecutionId,
    now_ms: i64,
) -> Result<(), StoreError> {
    db.run(
        prepare(format!(
            "UPDATE supersede_notice SET created_at_ms = ? + {}, attempts = attempts + 1 \
             WHERE execution_id = ?",
            crate::retention::CRON_BACKOFF_SQL
        ))
        .bind(now_ms)
        .bind(id.as_str()),
    )
    .await
    .map(|_| ())
    .map_err(err)
}
