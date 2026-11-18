//! Start admission on D1 (B5a, D24, D25, D56): pools, queues and start
//! idempotency. Each admission or release is one conditional batch:
//! - the plan reads the pool row, the run's admission and owner rows and
//!   the waiters it would promote, in one consistent read;
//! - the batch guards exactly what the decision used: the pool's active
//!   count and limit (or its absence), each promoted waiter still queued at
//!   the position read, the run's admission state, its owner;
//! - a racing admit or release fails the guard and the plan runs again
//!   (`ADMIT_ATTEMPTS`), so two starts can never both take the last slot.
//!
//! Waiters promoted in D1 must also be told in their ExecutionDO before
//! they activate: the same batch writes an `admission_notice` row for each
//! (1004), which the host delivers and deletes.
//!
//! LANG3's `on_conflict` (`queue`, `reject`) is resolved here with
//! `runbook_core::concurrency::resolve` against the same guarded read.
use crate::{
    catalog::{D1Catalog, count, decode, err},
    sql::{Columns, D1, D1Error, Statement, guard, json, prepare},
};
use runbook_core::{
    StoreError,
    catalog::{Admission, AdmissionState, AdmitRequest, TenantScope},
    concurrency::{OnConflict, PoolHolder, PoolView},
};
use runbook_model::ExecutionId;

/// Plans per admission or release before the pool is reported contended.
const ADMIT_ATTEMPTS: usize = 8;

pub(crate) fn state_of(text: &str) -> Result<AdmissionState, StoreError> {
    match text {
        "admitted" => Ok(AdmissionState::Admitted),
        "queued" => Ok(AdmissionState::Queued),
        "released" => Ok(AdmissionState::Released),
        _ => Err(StoreError::Backend("d1.invalid_admission_state")),
    }
}
pub(crate) fn state_label(state: AdmissionState) -> &'static str {
    match state {
        AdmissionState::Admitted => "admitted",
        AdmissionState::Queued => "queued",
        AdmissionState::Released => "released",
    }
}

/// One promoted waiter: who, and at which queue position it was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Promotion {
    pub execution_id: ExecutionId,
    pub admitted_at_ms: i64,
}

/// A pool as one consistent read saw it.
struct PoolRead {
    /// `(limit, active, revision)`, or none for a new key.
    row: Option<(i64, i64, i64)>,
    /// Queued waiters, oldest first, with their queue positions.
    waiters: Vec<(ExecutionId, i64)>,
}

fn pool_reads(
    scope: &TenantScope,
    key: &str,
    waiters: usize,
) -> Result<Vec<Statement>, StoreError> {
    Ok(vec![
        prepare(
            "SELECT concurrency_limit, active, revision FROM admission_pool WHERE org_id = ? \
             AND project_id = ? AND key = ?",
        )
        .bind(scope.org_id.as_str())
        .bind(scope.project_id.as_str())
        .bind(key),
        prepare(
            "SELECT execution_id, queue_order FROM admission WHERE org_id = ? AND project_id = ? \
             AND key = ? AND queue_order IS NOT NULL ORDER BY queue_order LIMIT ?",
        )
        .bind(scope.org_id.as_str())
        .bind(scope.project_id.as_str())
        .bind(key)
        .bind(count(waiters)?),
    ])
}
fn decode_pool(rows: &[crate::sql::Outcome]) -> Result<PoolRead, StoreError> {
    Ok(PoolRead {
        row: rows[0]
            .rows
            .first()
            .map(|r| {
                Ok::<_, StoreError>((
                    r.integer(0).map_err(err)?,
                    r.integer(1).map_err(err)?,
                    r.integer(2).map_err(err)?,
                ))
            })
            .transpose()?,
        waiters: rows[1]
            .rows
            .iter()
            .map(|r| Ok((r.parse(0).map_err(err)?, r.integer(1).map_err(err)?)))
            .collect::<Result<_, StoreError>>()?,
    })
}

/// Guards that the pool is as read, plus each waiter about to be promoted.
fn pool_guards(scope: &TenantScope, key: &str, pool: &PoolRead, promoted: usize) -> Vec<Statement> {
    let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
    let mut out = vec![match pool.row {
        // The revision moves with every admission and release batch, so
        // a writer that rewrote the same counts still fails this guard.
        Some((limit, active, revision)) => guard(
            "catalog.admission_stale",
            "EXISTS (SELECT 1 FROM admission_pool WHERE org_id = ? AND project_id = ? AND key = ? \
             AND concurrency_limit = ? AND active = ? AND revision = ?)",
        )
        .bind(org)
        .bind(project)
        .bind(key)
        .bind(limit)
        .bind(active)
        .bind(revision),
        None => guard(
            "catalog.admission_stale",
            "NOT EXISTS (SELECT 1 FROM admission_pool WHERE org_id = ? AND project_id = ? \
             AND key = ?)",
        )
        .bind(org)
        .bind(project)
        .bind(key),
    }];
    for (id, order) in pool.waiters.iter().take(promoted) {
        out.push(
            guard(
                "catalog.admission_stale",
                "EXISTS (SELECT 1 FROM admission WHERE execution_id = ? AND state = 'queued' \
                 AND queue_order = ?)",
            )
            .bind(id.as_str())
            .bind(*order),
        );
    }
    out
}

/// Promote `waiters` (already guarded) at `now`, with their notices.
fn promote(scope: &TenantScope, waiters: &[(ExecutionId, i64)], now: i64) -> Vec<Statement> {
    let mut out = vec![];
    for (id, _) in waiters {
        out.push(
            prepare(
                "UPDATE admission SET state = 'admitted', queue_order = NULL, admitted_at_ms = ? \
                 WHERE execution_id = ?",
            )
            .bind(now)
            .bind(id.as_str()),
        );
        out.push(
            prepare(
                "INSERT INTO admission_notice (execution_id, org_id, project_id, admitted_at_ms) \
                 VALUES (?, ?, ?, ?) ON CONFLICT (execution_id) DO UPDATE \
                 SET admitted_at_ms = excluded.admitted_at_ms",
            )
            .bind(id.as_str())
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str())
            .bind(now),
        );
    }
    out
}

/// Record the owner if new; a different tenant fails the batch.
pub(crate) fn claim_owner(scope: &TenantScope, id: &ExecutionId) -> Vec<Statement> {
    vec![
        prepare(
            "INSERT INTO execution_owner (execution_id, org_id, project_id) VALUES (?, ?, ?) \
             ON CONFLICT (execution_id) DO NOTHING",
        )
        .bind(id.as_str())
        .bind(scope.org_id.as_str())
        .bind(scope.project_id.as_str()),
        guard(
            "catalog.execution_scope_conflict",
            "EXISTS (SELECT 1 FROM execution_owner WHERE execution_id = ? AND org_id = ? \
             AND project_id = ?)",
        )
        .bind(id.as_str())
        .bind(scope.org_id.as_str())
        .bind(scope.project_id.as_str()),
    ]
}

/// A start's admission plan and the runs it evicts (LANG3).
pub(crate) struct StartPlan {
    pub(crate) plan: AdmitPlan,
    pub(crate) evicted: Vec<PoolHolder>,
}

/// What one admission plan decided and the statements that apply it.
pub(crate) struct AdmitPlan {
    pub(crate) admission: Admission,
    pub(crate) statements: Vec<Statement>,
    pub(crate) promoted: Vec<Promotion>,
}

/// Plan a pooled admission (D25): the request's limit replaces the pool's;
/// raising it promotes waiters, oldest first, before the request is
/// considered; with `first`, a request that has to wait goes ahead of
/// every waiter (LANG3 `cancel_previous`).
fn plan_admit(
    scope: &TenantScope,
    request: &AdmitRequest,
    pool: &PoolRead,
    now: i64,
    first: bool,
) -> Result<AdmitPlan, StoreError> {
    let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
    let limit = i64::from(request.limit);
    let mut active = pool.row.map_or(0, |(_, active, _)| active);
    let room = usize::try_from((limit - active).max(0)).unwrap_or(0);
    let promoted = room.min(pool.waiters.len());
    active += i64::try_from(promoted).unwrap_or(0);
    let state = if active < limit {
        active += 1;
        AdmissionState::Admitted
    } else {
        AdmissionState::Queued
    };
    let mut statements = pool_guards(scope, &request.key, pool, promoted);
    statements.push(
        guard(
            "catalog.admission_conflict",
            "NOT EXISTS (SELECT 1 FROM admission WHERE execution_id = ?)",
        )
        .bind(request.execution_id.as_str()),
    );
    statements.extend(claim_owner(scope, &request.execution_id));
    if pool.row.is_none() {
        statements.push(
            prepare(
                "INSERT INTO admission_pool (org_id, project_id, key, concurrency_limit, active) \
                 VALUES (?, ?, ?, ?, 0)",
            )
            .bind(org)
            .bind(project)
            .bind(request.key.as_str())
            .bind(limit),
        );
    }
    statements.extend(promote(scope, &pool.waiters[..promoted], now));
    statements.push(
        prepare(
            "UPDATE admission_pool SET concurrency_limit = ?, active = ?, revision = revision + 1 \
             WHERE org_id = ? \
             AND project_id = ? AND key = ?",
        )
        .bind(limit)
        .bind(active)
        .bind(org)
        .bind(project)
        .bind(request.key.as_str()),
    );
    let order = match (state, first) {
        (AdmissionState::Queued, true) => {
            "(SELECT coalesce(min(queue_order), 1) - 1 FROM admission WHERE org_id = ? \
             AND project_id = ? AND key = ? AND queue_order IS NOT NULL)"
        }
        (AdmissionState::Queued, false) => {
            "(SELECT coalesce(max(queue_order), 0) + 1 FROM admission WHERE org_id = ? \
             AND project_id = ? AND key = ? AND queue_order IS NOT NULL)"
        }
        _ => "(SELECT NULL WHERE ? IS NULL AND ? IS NULL AND ? IS NULL)",
    };
    let admitted_at = (state == AdmissionState::Admitted).then_some(now);
    statements.push(
        prepare(format!(
            "INSERT INTO admission (execution_id, org_id, project_id, key, state, queue_order, \
             request, admitted_at_ms) VALUES (?, ?, ?, ?, ?, {order}, ?, ?)"
        ))
        .bind(request.execution_id.as_str())
        .bind(org)
        .bind(project)
        .bind(request.key.as_str())
        .bind(state_label(state))
        .bind(org)
        .bind(project)
        .bind(request.key.as_str())
        .bind(json(request).map_err(err)?)
        .bind(admitted_at),
    );
    Ok(AdmitPlan {
        admission: Admission {
            execution_id: request.execution_id.clone(),
            state,
            admitted_at_ms: admitted_at,
        },
        statements,
        promoted: pool.waiters[..promoted]
            .iter()
            .map(|(id, _)| Promotion {
                execution_id: id.clone(),
                admitted_at_ms: now,
            })
            .collect(),
    })
}

/// Whether a guard failure means "plan again".
fn stale(error: &D1Error) -> bool {
    matches!(error, D1Error::Guard(code) if code == "catalog.admission_stale")
}

impl<D: D1> D1Catalog<D> {
    /// Whether any tenant's tombstone names `id` (a deleted ID is never
    /// reused).
    pub async fn tombstoned(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        Ok(!self
            .db
            .all(prepare("SELECT 1 FROM tombstone WHERE execution_id = ?").bind(id.as_str()))
            .await
            .map_err(err)?
            .is_empty())
    }
    async fn foreign_owner(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        Ok(crate::catalog::execution_owner(&self.db, id)
            .await?
            .is_some_and(|o| o != self.scope))
    }

    /// The read for one admission plan: the pool and enough waiters to
    /// fill the request's limit.
    async fn read_pool(&self, key: &str, limit: u32) -> Result<PoolRead, StoreError> {
        let rows = self
            .db
            .batch(pool_reads(
                &self.scope,
                key,
                usize::try_from(limit).unwrap_or(usize::MAX),
            )?)
            .await
            .map_err(err)?;
        decode_pool(&rows)
    }

    /// `CatalogStore::admit`: idempotent per start key (D20), then pooled.
    pub async fn admit(&self, request: AdmitRequest) -> Result<Admission, StoreError> {
        if self.tombstoned(&request.execution_id).await? {
            return Err(StoreError::NotFound);
        }
        if self.foreign_owner(&request.execution_id).await? {
            return Err(StoreError::InvalidBatch("catalog.execution_scope_conflict"));
        }
        if request.key.is_empty() || request.idempotency_key.is_empty() || request.limit == 0 {
            return Err(StoreError::InvalidBatch("catalog.invalid_admission"));
        }
        for _ in 0..ADMIT_ATTEMPTS {
            if let Some(prior) = self.start_key(&request.idempotency_key).await? {
                if prior != request {
                    return Err(StoreError::InvalidBatch("catalog.idempotency_conflict"));
                }
                return self
                    .admission(&request.execution_id)
                    .await?
                    .ok_or(StoreError::NotFound);
            }
            let pool = self.read_pool(&request.key, request.limit).await?;
            let mut plan = plan_admit(&self.scope, &request, &pool, self.db.now_ms(), false)?;
            plan.statements.push(
                prepare(
                    "INSERT INTO start_idempotency (org_id, project_id, idempotency_key, \
                     execution_id, request) VALUES (?, ?, ?, ?, ?)",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(request.idempotency_key.as_str())
                .bind(request.execution_id.as_str())
                .bind(json(&request).map_err(err)?),
            );
            match self.db.batch(plan.statements).await {
                Ok(_) => return Ok(plan.admission),
                Err(error) if stale(&error) => {}
                // A racing retry of the same start key won: answer from it.
                Err(D1Error::Unique) => {}
                Err(error) => return Err(err(error)),
            }
        }
        Err(StoreError::Backend("d1.admission_contended"))
    }

    async fn start_key(&self, key: &str) -> Result<Option<AdmitRequest>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT request FROM start_idempotency WHERE org_id = ? AND project_id = ? \
                     AND idempotency_key = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(key),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }

    /// Plan a pooled admission for a start (no start-key bookkeeping; the
    /// start replay is the start's), resolving LANG3's `on_conflict` with
    /// `runbook_core::concurrency::resolve` against the pool the batch
    /// guards (its revision):
    /// - `reject` refuses a start that would wait
    ///   (`concurrency::CONFLICT_CODE`, nothing written);
    /// - `cancel_previous` on a conflict (CF5) preempts the pool the core
    ///   prepared against (`start.preemption_stale` otherwise): every evicted
    ///   waiter is released, the start is admitted if a slot is free or
    ///   queued first in line, and each evicted run gets a
    ///   `supersede_notice` row (1008) its object applies after the batch.
    ///   An evicted holder keeps its slot until its cancellation closes it.
    ///
    /// The caller adds its statements, runs the batch and plans again on
    /// `catalog.admission_stale`.
    pub(crate) async fn plan_start_admission(
        &self,
        request: &AdmitRequest,
        on_conflict: Option<&OnConflict>,
        superseded: &runbook_engine::Supersession,
    ) -> Result<StartPlan, StoreError> {
        use runbook_core::concurrency::{Resolution, evictions, resolve};
        if request.key.is_empty() || request.limit == 0 {
            return Err(StoreError::InvalidBatch("catalog.invalid_admission"));
        }
        let mut pool = self.read_pool(&request.key, request.limit).await?;
        let view = self.admission_pool(&request.key).await?;
        let now = self.db.now_ms();
        match resolve(on_conflict, request.limit, &view)? {
            Resolution::Admit => {
                let plan = plan_admit(&self.scope, request, &pool, now, false)?;
                // A pool that changed between the two reads: plan again.
                if on_conflict.is_some() && plan.admission.state == AdmissionState::Queued {
                    return Err(StoreError::InvalidBatch("catalog.admission_stale"));
                }
                Ok(StartPlan {
                    plan,
                    evicted: vec![],
                })
            }
            Resolution::Preempt { waiters } => {
                let evicted = evictions(request.limit, &view);
                // Every waiter is evicted: none is promoted ahead of the start.
                pool.waiters.clear();
                let mut statements = vec![];
                for waiter in &waiters {
                    statements.push(
                        guard(
                            "catalog.admission_stale",
                            "EXISTS (SELECT 1 FROM admission WHERE execution_id = ? \
                             AND state = 'queued')",
                        )
                        .bind(waiter.as_str()),
                    );
                    statements.push(
                        prepare(
                            "UPDATE admission SET state = 'released', queue_order = NULL \
                             WHERE execution_id = ?",
                        )
                        .bind(waiter.as_str()),
                    );
                }
                let mut plan = plan_admit(&self.scope, request, &pool, now, true)?;
                statements.append(&mut plan.statements);
                for run in &evicted {
                    statements.push(
                        prepare(
                            "INSERT INTO supersede_notice (execution_id, org_id, project_id, \
                             state, superseded_by, started_by, created_at_ms) \
                             VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (execution_id) DO NOTHING",
                        )
                        .bind(run.execution_id.as_str())
                        .bind(self.org())
                        .bind(self.project())
                        .bind(state_label(run.state))
                        .bind(superseded.execution_id.as_str())
                        .bind(superseded.started_by.as_str())
                        .bind(now),
                    );
                }
                plan.statements = statements;
                Ok(StartPlan { plan, evicted })
            }
        }
    }

    pub async fn admission(&self, id: &ExecutionId) -> Result<Option<Admission>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT a.state, a.admitted_at_ms FROM admission a JOIN execution_owner o \
                     ON o.execution_id = a.execution_id \
                     WHERE a.execution_id = ? AND o.org_id = ? AND o.project_id = ?",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok(Admission {
                    execution_id: id.clone(),
                    state: state_of(row.text(0).map_err(err)?)?,
                    admitted_at_ms: row.optional_integer(1).map_err(err)?,
                })
            })
            .transpose()
    }

    /// LANG3: holders in admission order, then waiters in queue order.
    pub async fn admission_pool(&self, key: &str) -> Result<PoolView, StoreError> {
        pool_view(&self.db, &self.scope, key).await
    }

    /// Release one admission and promote waiters while the pool is under
    /// its current limit (a lowered limit admits no one new until the
    /// active count drops below it, D25). Idempotent: a released or absent
    /// admission changes nothing. Returns the promoted runs.
    pub async fn release(&self, id: &ExecutionId) -> Result<Vec<Promotion>, StoreError> {
        for _ in 0..ADMIT_ATTEMPTS {
            let rows = self
                .db
                .all(
                    prepare(
                        "SELECT key, state FROM admission WHERE execution_id = ? AND org_id = ? \
                         AND project_id = ?",
                    )
                    .bind(id.as_str())
                    .bind(self.org())
                    .bind(self.project()),
                )
                .await
                .map_err(err)?;
            let Some(row) = rows.first() else {
                return Ok(vec![]);
            };
            let key = row.string(0).map_err(err)?;
            let previous = state_of(row.text(1).map_err(err)?)?;
            if previous == AdmissionState::Released {
                return Ok(vec![]);
            }
            let pool = {
                let rows = self
                    .db
                    .batch(pool_reads(&self.scope, &key, 1 << 16)?)
                    .await
                    .map_err(err)?;
                decode_pool(&rows)?
            };
            let (limit, mut active, _) = pool.row.ok_or(StoreError::Backend("d1.missing_pool"))?;
            let now = self.db.now_ms();
            let mut statements = vec![
                guard(
                    "catalog.admission_stale",
                    "EXISTS (SELECT 1 FROM admission WHERE execution_id = ? AND state = ?)",
                )
                .bind(id.as_str())
                .bind(state_label(previous)),
                prepare(
                    "UPDATE admission SET state = 'released', queue_order = NULL \
                     WHERE execution_id = ?",
                )
                .bind(id.as_str()),
            ];
            if previous == AdmissionState::Admitted {
                active -= 1;
                // The released run itself is not a waiter.
                let waiters: Vec<(ExecutionId, i64)> = pool
                    .waiters
                    .iter()
                    .filter(|(w, _)| w != id)
                    .cloned()
                    .collect();
                let room = usize::try_from((limit - active).max(0)).unwrap_or(0);
                let promoted = room.min(waiters.len());
                active += i64::try_from(promoted).unwrap_or(0);
                let read = PoolRead {
                    row: pool.row,
                    waiters,
                };
                statements.extend(pool_guards(&self.scope, &key, &read, promoted));
                statements.extend(promote(&self.scope, &read.waiters[..promoted], now));
                statements.push(
                    prepare(
                        "UPDATE admission_pool SET active = ?, revision = revision + 1 \
                         WHERE org_id = ? AND project_id = ? AND key = ?",
                    )
                    .bind(active)
                    .bind(self.org())
                    .bind(self.project())
                    .bind(key.as_str()),
                );
                match self.db.batch(statements).await {
                    Ok(_) => {
                        return Ok(read.waiters[..promoted]
                            .iter()
                            .map(|(w, _)| Promotion {
                                execution_id: w.clone(),
                                admitted_at_ms: now,
                            })
                            .collect());
                    }
                    Err(error) if stale(&error) => continue,
                    Err(error) => return Err(err(error)),
                }
            }
            // A queued run leaves the queue: other plans read it.
            statements.extend(pool_guards(&self.scope, &key, &pool, 0));
            statements.push(
                prepare(
                    "UPDATE admission_pool SET revision = revision + 1 WHERE org_id = ? \
                     AND project_id = ? AND key = ?",
                )
                .bind(self.org())
                .bind(self.project())
                .bind(key.as_str()),
            );
            match self.db.batch(statements).await {
                Ok(_) => return Ok(vec![]),
                Err(error) if stale(&error) => continue,
                Err(error) => return Err(err(error)),
            }
        }
        Err(StoreError::Backend("d1.admission_contended"))
    }

    /// B6 cancellation of a waiter: queued to released, never promoted.
    /// The row stays (activation treats a run with no row as due, F37).
    pub async fn release_queued(&self, id: &ExecutionId) -> Result<bool, StoreError> {
        // The pool's revision moves first (while the row is still queued),
        // so a plan that read this waiter fails its guard.
        let outcomes = self
            .db
            .batch(vec![
                prepare(
                    "UPDATE admission_pool SET revision = revision + 1 WHERE (org_id, project_id, \
                     key) IN (SELECT org_id, project_id, key FROM admission \
                     WHERE execution_id = ? AND org_id = ? AND project_id = ? AND state = 'queued')",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
                prepare(
                    "UPDATE admission SET state = 'released', queue_order = NULL \
                     WHERE execution_id = ? AND org_id = ? AND project_id = ? AND state = 'queued'",
                )
                .bind(id.as_str())
                .bind(self.org())
                .bind(self.project()),
            ])
            .await
            .map_err(err)?;
        Ok(outcomes.get(1).is_some_and(|o| o.changes == 1))
    }

    /// Admission notices not yet delivered to their objects, oldest first.
    pub async fn pending_notices(&self, limit: usize) -> Result<Vec<Promotion>, StoreError> {
        pending_notices(&self.db, Some(&self.scope), limit).await
    }
}

/// Undelivered admission notices, oldest first (every tenant when `scope`
/// is none: the minute cron).
pub async fn pending_notices<D: D1>(
    db: &D,
    scope: Option<&TenantScope>,
    limit: usize,
) -> Result<Vec<Promotion>, StoreError> {
    let statement = match scope {
        Some(scope) => prepare(
            "SELECT execution_id, admitted_at_ms FROM admission_notice WHERE org_id = ? \
             AND project_id = ? ORDER BY admitted_at_ms, execution_id LIMIT ?",
        )
        .bind(scope.org_id.as_str())
        .bind(scope.project_id.as_str())
        .bind(count(limit)?),
        None => prepare(
            "SELECT execution_id, admitted_at_ms FROM admission_notice \
             ORDER BY admitted_at_ms, execution_id LIMIT ?",
        )
        .bind(count(limit)?),
    };
    db.all(statement)
        .await
        .map_err(err)?
        .iter()
        .map(|row| {
            Ok(Promotion {
                execution_id: row.parse(0).map_err(err)?,
                admitted_at_ms: row.integer(1).map_err(err)?,
            })
        })
        .collect()
}

/// One page of undelivered notices strictly after `after` (admitted time,
/// then execution ID), so a caller can page past ones it could not deliver.
pub async fn pending_notices_after<D: D1>(
    db: &D,
    scope: Option<&TenantScope>,
    after: Option<&(i64, ExecutionId)>,
    limit: usize,
) -> Result<Vec<Promotion>, StoreError> {
    let (at, id) = match after {
        Some((at, id)) => (Some(*at), Some(id.as_str().to_owned())),
        None => (None, None),
    };
    let mut statement = prepare(
        "SELECT execution_id, admitted_at_ms FROM admission_notice \
         WHERE (? IS NULL OR org_id = ?) AND (? IS NULL OR project_id = ?) \
         AND (? IS NULL OR admitted_at_ms > ? OR (admitted_at_ms = ? AND execution_id > ?)) \
         ORDER BY admitted_at_ms, execution_id LIMIT ?",
    );
    let org = scope.map(|s| s.org_id.as_str().to_owned());
    let project = scope.map(|s| s.project_id.as_str().to_owned());
    statement = statement
        .bind(org.clone())
        .bind(org)
        .bind(project.clone())
        .bind(project)
        .bind(at)
        .bind(at)
        .bind(at)
        .bind(id)
        .bind(count(limit)?);
    db.all(statement)
        .await
        .map_err(err)?
        .iter()
        .map(|row| {
            Ok(Promotion {
                execution_id: row.parse(0).map_err(err)?,
                admitted_at_ms: row.integer(1).map_err(err)?,
            })
        })
        .collect()
}

/// The object acknowledged its admission: forget the notice (only the one
/// it acknowledged; a later promotion's notice stays).
pub async fn ack_notice<D: D1>(db: &D, notice: &Promotion) -> Result<(), StoreError> {
    db.run(
        prepare("DELETE FROM admission_notice WHERE execution_id = ? AND admitted_at_ms = ?")
            .bind(notice.execution_id.as_str())
            .bind(notice.admitted_at_ms),
    )
    .await
    .map(|_| ())
    .map_err(err)
}

/// Gate fix D1: whether a notice whose object holds no run can be dropped.
/// It can when the run will never initialize: it has no owner row (an
/// abandoned or never-reserved start) or it was deleted (a tombstone).
/// Otherwise the start is half-committed and its retry, or the reconciler,
/// will initialize the object, so the notice stays for it.
pub async fn notice_orphaned<D: D1>(db: &D, id: &ExecutionId) -> Result<bool, StoreError> {
    let rows = db
        .all(
            prepare(
                "SELECT EXISTS (SELECT 1 FROM execution_owner WHERE execution_id = ?) \
                 AND NOT EXISTS (SELECT 1 FROM tombstone WHERE execution_id = ?)",
            )
            .bind(id.as_str())
            .bind(id.as_str()),
        )
        .await
        .map_err(err)?;
    let live = rows
        .first()
        .map(|row| row.integer(0))
        .transpose()
        .map_err(err)?;
    Ok(live != Some(1))
}

/// The key's holders (admission order) then waiters (queue order).
pub(crate) async fn pool_view<D: D1>(
    db: &D,
    scope: &TenantScope,
    key: &str,
) -> Result<PoolView, StoreError> {
    let holders = db
        .all(
            prepare(
                "SELECT execution_id, state FROM admission WHERE org_id = ? AND project_id = ? \
                 AND key = ? AND state IN ('admitted', 'queued') ORDER BY state = 'queued', \
                 CASE WHEN state = 'admitted' THEN admitted_at_ms END, queue_order, execution_id",
            )
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str())
            .bind(key),
        )
        .await
        .map_err(err)?
        .iter()
        .map(|row| {
            Ok(PoolHolder {
                execution_id: row.parse(0).map_err(err)?,
                state: state_of(row.text(1).map_err(err)?)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(PoolView { holders })
}
