//! ARC1/ARC2: `ArchiveIndex` on PostgreSQL (migration 0029). Every call is
//! one transaction filtered by organization and project. Stale marks are
//! written by the transactions that make a file out of date (`commit`,
//! `delete_execution` and `finalize_evidence` call [`mark_run_stale`]); each
//! mark bumps `stale_marks`, and a record that read an older count stays
//! stale, so a record never clears a concurrent mark.
//!
//! Lock order, everywhere: a run's head row, then its tombstone or summary
//! row, then the file's index row. Commits, deletions and finalize lock the
//! head `FOR UPDATE`; `record_archive` takes the listed heads `FOR SHARE OF
//! e`, retained runs' tombstones and summary rows `FOR SHARE`, then the
//! index row `FOR UPDATE`, so a racing amendment, deletion, finalize or purge
//! either waits for the record and then marks it, or commits first and the
//! record sees it. A deadlock (40P01) or serialization failure (40001) maps
//! to `postgres.retry_transaction`; `record_archive` retries it up to three
//! times, and the job retries anything else on a later pass.
//!
//! Stored versions of a day's file are deleted only here, under the index
//! row's lock: those older than the version the index names, or older than
//! `VERSION_GRACE_MS` (a leftover); never a younger, in-flight write.
use crate::{
    execution::{PostgresExecutionStore, READ_COMMITTED, SNAPSHOT_READ},
    rows::{db, decode, encode, int, uint},
};
use runbook_core::{
    StoreError,
    archives::{
        ArchiveIndex, ArchiveRecord, ArchivedRun, DAY_MS, RETENTION_REASON, VERSION_GRACE_MS,
        backoff_ms,
    },
    blob::BlobKey,
    catalog::TenantScope,
    retention::Tombstone,
};
use runbook_model::{EvidenceId, ExecutionId, RunbookId};
use tokio_postgres::{Client, Row};

const FINISHED: &str = "('succeeded', 'failed', 'cancelled')";
const NOW_US: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000000)::bigint";

/// SQL: the day's attempt row, if any, is due at `now` (not backing off).
fn due(rb: &str, day: &str, now: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM runbook.archive_attempt t \
         WHERE t.org_id = $1 AND t.project_id = $2 AND t.runbook_id = {rb} AND t.day = {day} \
         AND t.next_attempt_at_ms > {now})"
    )
}

fn record_of(row: &Row) -> Result<ArchiveRecord, StoreError> {
    let mut record: ArchiveRecord = decode(row.get::<_, &str>("body"))?;
    record.stale = row.get("stale");
    record.stale_marks = uint(row.get("stale_marks"))?;
    Ok(record)
}

fn ids(rows: Vec<Row>) -> Result<Vec<ExecutionId>, StoreError> {
    rows.iter()
        .map(|row| {
            row.get::<_, &str>(0)
                .parse()
                .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))
        })
        .collect()
}

fn key_of(record: &ArchiveRecord) -> BlobKey {
    BlobKey::Archive {
        runbook_id: record.runbook_id.clone(),
        day: record.day,
        digest: record.digest,
    }
}

async fn now_us(c: &Client) -> Result<i64, StoreError> {
    c.query_one(NOW_US, &[]).await.map(|r| r.get(0)).map_err(db)
}

async fn blob_stored(c: &Client, scope: &TenantScope, key: &BlobKey) -> Result<bool, StoreError> {
    Ok(c.query_one(
        "SELECT EXISTS (SELECT 1 FROM runbook.blob \
             WHERE org_id = $1 AND project_id = $2 AND key = $3)",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &encode(key)?,
        ],
    )
    .await
    .map_err(db)?
    .get(0))
}

/// Inside a commit, finalize or deletion of `id`: mark the file that lists
/// it stale (found through its membership, which outlives the summary row).
pub(crate) async fn mark_run_stale(c: &Client, id: &str) -> Result<(), StoreError> {
    c.execute(
        "UPDATE runbook.archive a SET stale = true, stale_marks = a.stale_marks + 1 \
         FROM runbook.archive_member m \
         WHERE m.execution_id = $1 AND a.org_id = m.org_id AND a.project_id = m.project_id \
         AND a.runbook_id = m.runbook_id AND a.day = m.day",
        &[&id],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// Inside `delete_execution`, under the head lock: retention may delete the
/// run only while a stored, not stale file lists it at `revision`.
pub(crate) async fn archived_current(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
    revision: u64,
) -> Result<bool, StoreError> {
    let Some(row) = c
        .query_opt(
            "SELECT r.body, a.stale, a.stale_marks, a.body AS file FROM runbook.archive_run r \
             JOIN runbook.archive a ON a.org_id = r.org_id AND a.project_id = r.project_id \
               AND a.runbook_id = r.runbook_id AND a.day = r.day \
             WHERE r.execution_id = $1 AND r.org_id = $2 AND r.project_id = $3 \
             FOR SHARE OF a",
            &[
                &id.as_str(),
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
            ],
        )
        .await
        .map_err(db)?
    else {
        return Ok(false);
    };
    let run: ArchivedRun = decode(row.get(0))?;
    if row.get::<_, bool>(1) || run.revision != revision {
        return Ok(false);
    }
    let file: ArchiveRecord = decode(row.get::<_, &str>("file"))?;
    blob_stored(c, scope, &key_of(&file)).await
}

/// Inside `delete_execution`: retention keeps the summary row; any other
/// reason takes the run out of its file.
pub(crate) async fn on_deletion(c: &Client, id: &str, reason: &str) -> Result<(), StoreError> {
    if reason == RETENTION_REASON {
        return Ok(());
    }
    mark_run_stale(c, id).await?;
    c.execute(
        "DELETE FROM runbook.archive_run WHERE execution_id = $1",
        &[&id],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// A retained run's uploads (moved to its runbook at retention), found by
/// exact key under the run: the upload and UP2's derived objects alike.
async fn retained_upload_keys(
    c: &Client,
    scope: &TenantScope,
    runbook_id: &str,
    id: &ExecutionId,
) -> Result<Vec<String>, StoreError> {
    Ok(c.query(
        "SELECT key FROM runbook.blob WHERE org_id = $1 AND project_id = $2 \
             AND runbook_id = $3 AND name IS NULL AND created_at_us IS NULL",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &runbook_id,
        ],
    )
    .await
    .map_err(db)?
    .iter()
    .map(|row| row.get::<_, String>(0))
    .filter(|key| {
        decode::<BlobKey>(key)
            .is_ok_and(|k| matches!(k, BlobKey::Upload { execution_id, .. } if &execution_id == id))
    })
    .collect())
}

/// Delete a retained run's uploads and their counted bytes.
async fn drop_retained_uploads(
    c: &Client,
    scope: &TenantScope,
    runbook_id: &str,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    let keys = retained_upload_keys(c, scope, runbook_id, id).await?;
    c.execute(
        "DELETE FROM runbook.blob WHERE org_id = $1 AND project_id = $2 AND key = ANY($3)",
        &[&scope.org_id.as_str(), &scope.project_id.as_str(), &keys],
    )
    .await
    .map_err(db)?;
    c.execute(
        "DELETE FROM runbook.retained_evidence \
         WHERE execution_id = $1 AND org_id = $2 AND project_id = $3",
        &[
            &id.as_str(),
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// A single-run deletion of a run retention already deleted, inside the
/// caller's transaction. `None` when `id` has no retention tombstone here.
pub(crate) async fn purge_retained(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
    reason: &str,
) -> Result<Option<Tombstone>, StoreError> {
    let Some(row) = c
        .query_opt(
            "SELECT body FROM runbook.tombstone \
             WHERE execution_id = $1 AND org_id = $2 AND project_id = $3 FOR UPDATE",
            &[
                &id.as_str(),
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
            ],
        )
        .await
        .map_err(db)?
    else {
        return Ok(None);
    };
    let mut tombstone: Tombstone = decode(row.get(0))?;
    if tombstone.reason != RETENTION_REASON {
        return Ok(None);
    }
    tombstone.reason = reason.into();
    c.execute(
        "UPDATE runbook.tombstone SET body = $2 WHERE execution_id = $1",
        &[&id.as_str(), &encode(&tombstone)?],
    )
    .await
    .map_err(db)?;
    mark_run_stale(c, id.as_str()).await?;
    c.execute(
        "DELETE FROM runbook.archive_run WHERE execution_id = $1",
        &[&id.as_str()],
    )
    .await
    .map_err(db)?;
    drop_retained_uploads(c, scope, tombstone.runbook_id.as_str(), id).await?;
    Ok(Some(tombstone))
}

async fn clear_backoff(
    c: &Client,
    scope: &TenantScope,
    runbook_id: &str,
    day: i64,
) -> Result<(), StoreError> {
    c.execute(
        "DELETE FROM runbook.archive_attempt \
         WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &runbook_id,
            &day,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// Stored versions of one day's file: (key text, digest, created at, µs).
async fn versions(
    c: &Client,
    scope: &TenantScope,
    runbook_id: &str,
    day: i64,
) -> Result<Vec<(String, [u8; 32], i64)>, StoreError> {
    Ok(c.query(
        "SELECT key, created_at_us FROM runbook.blob WHERE org_id = $1 AND project_id = $2 \
             AND runbook_id = $3 AND created_at_us IS NOT NULL",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &runbook_id,
        ],
    )
    .await
    .map_err(db)?
    .iter()
    .filter_map(|row| {
        let text: String = row.get(0);
        match decode::<BlobKey>(&text) {
            Ok(BlobKey::Archive { day: d, digest, .. }) if d == day => {
                Some((text, digest, row.get::<_, i64>(1)))
            }
            _ => None,
        }
    })
    .collect())
}

async fn delete_keys(c: &Client, scope: &TenantScope, keys: &[String]) -> Result<(), StoreError> {
    if !keys.is_empty() {
        c.execute(
            "DELETE FROM runbook.blob WHERE org_id = $1 AND project_id = $2 AND key = ANY($3)",
            &[&scope.org_id.as_str(), &scope.project_id.as_str(), &keys],
        )
        .await
        .map_err(db)?;
    }
    Ok(())
}

/// Under the index row's lock: delete versions the index does not name that
/// are older than the named one, or than the grace period.
async fn sweep(
    c: &Client,
    scope: &TenantScope,
    runbook_id: &str,
    day: i64,
    named: Option<[u8; 32]>,
) -> Result<usize, StoreError> {
    let all = versions(c, scope, runbook_id, day).await?;
    let named_at = all
        .iter()
        .find(|(_, digest, _)| Some(*digest) == named)
        .map(|(_, _, at)| *at);
    let cutoff = now_us(c).await? - VERSION_GRACE_MS * 1000;
    let doomed: Vec<String> = all
        .into_iter()
        .filter(|(_, digest, at)| {
            Some(*digest) != named && (named_at.is_some_and(|n| *at < n) || *at < cutoff)
        })
        .map(|(key, _, _)| key)
        .collect();
    delete_keys(c, scope, &doomed).await?;
    Ok(doomed.len())
}

/// Serialize everything that writes one day's file or its versions, row or
/// no row: a first write (no index row to lock yet), the leftover sweep and
/// deletion take this before the row (re-check 3, finding 2).
async fn lock_day(
    c: &Client,
    scope: &TenantScope,
    runbook_id: &str,
    day: i64,
) -> Result<(), StoreError> {
    c.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended(\
         'archive:' || $1 || '/' || $2 || '/' || $3 || '/' || ($4::bigint)::text, 0))",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &runbook_id,
            &day,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// The index row of a file, locked.
async fn lock_file(
    c: &Client,
    scope: &TenantScope,
    runbook_id: &str,
    day: i64,
) -> Result<Option<ArchiveRecord>, StoreError> {
    c.query_opt(
        "SELECT stale, stale_marks, body FROM runbook.archive \
         WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4 FOR UPDATE",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &runbook_id,
            &day,
        ],
    )
    .await
    .map_err(db)?
    .map(|row| record_of(&row))
    .transpose()
}

impl PostgresExecutionStore {
    /// ARC3 org-scoped routing read (grants nothing): the project of a run's
    /// summary row, live or not.
    pub async fn archived_run_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query_opt(
                "SELECT project_id FROM runbook.archive_run WHERE org_id = $1 AND execution_id = $2",
                &[&org_id.as_str(), &id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| {
                row.get::<_, &str>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))
            })
            .transpose()
        })
        .await
    }

    async fn record_once(
        &self,
        scope: &TenantScope,
        record: &ArchiveRecord,
        runs: &[ArchivedRun],
        expected: Option<[u8; 32]>,
    ) -> Result<(), StoreError> {
        let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
        let rb = record.runbook_id.as_str();
        self.transaction(READ_COMMITTED, async |c| {
            if self.retry_faults.get() > 0 {
                self.retry_faults.set(self.retry_faults.get() - 1);
                return Err(StoreError::Backend("postgres.retry_transaction"));
            }
            // Heads first (the order commits, deletions and finalize use),
            // then retained runs' tombstones and summary rows, then the
            // file's row.
            let mut stale = false;
            let mut keep = Vec::new();
            for run in runs {
                let id = run.execution_id.as_str();
                let live = c
                    .query_opt(
                        "SELECT e.revision FROM runbook.execution e \
                         JOIN runbook.execution_owner o USING (execution_id) \
                         WHERE e.execution_id = $1 AND o.org_id = $2 AND o.project_id = $3 \
                         FOR SHARE OF e",
                        &[&id, &org, &project],
                    )
                    .await
                    .map_err(db)?;
                if let Some(row) = live {
                    let finalized: Vec<EvidenceId> = c
                        .query(
                            "SELECT evidence_id FROM runbook.evidence \
                             WHERE execution_id = $1 AND state = 'finalized'",
                            &[&id],
                        )
                        .await
                        .map_err(db)?
                        .iter()
                        .map(|r| {
                            r.get::<_, &str>(0)
                                .parse()
                                .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))
                        })
                        .collect::<Result<_, _>>()?;
                    let (mut finalized, mut listed) = (finalized, run.evidence.clone());
                    finalized.sort();
                    listed.sort();
                    stale |= uint(row.get(0))? != run.revision || finalized != listed;
                    keep.push(run);
                    continue;
                }
                let retained = c
                    .query_opt(
                        "SELECT body FROM runbook.tombstone \
                         WHERE execution_id = $1 AND org_id = $2 AND project_id = $3 FOR SHARE",
                        &[&id, &org, &project],
                    )
                    .await
                    .map_err(db)?
                    .map(|row| decode::<Tombstone>(row.get(0)))
                    .transpose()?
                    .is_some_and(|t| t.reason == RETENTION_REASON);
                if !retained {
                    return Err(StoreError::InvalidBatch("archive.run_gone"));
                }
                // A folder built from the live run, which retention has
                // deleted since: the writer retries from the stored folder.
                if run.gathered_live {
                    return Err(StoreError::InvalidBatch("archive.conflict"));
                }
                // A purged summary row is not restored by a rewrite.
                let has_row = c
                    .query_opt(
                        "SELECT 1 FROM runbook.archive_run \
                         WHERE execution_id = $1 AND org_id = $2 AND project_id = $3 FOR SHARE",
                        &[&id, &org, &project],
                    )
                    .await
                    .map_err(db)?
                    .is_some();
                if has_row {
                    keep.push(run);
                }
            }
            lock_day(c, scope, rb, record.day).await?;
            let current = lock_file(c, scope, rb, record.day).await?;
            self.record_pause_point().await;
            if current.as_ref().map(|r| r.digest) != expected {
                return Err(StoreError::InvalidBatch("archive.conflict"));
            }
            // Under the lock, so a sweep cannot take the version between
            // this check and the swing.
            if !blob_stored(c, scope, &key_of(record)).await? {
                return Err(StoreError::InvalidBatch("archive.blob_missing"));
            }
            // A mark since the writer read the row keeps the file stale.
            let marks = current.as_ref().map_or(0, |r| r.stale_marks);
            stale |= marks != record.stale_marks;
            let stored = ArchiveRecord {
                stale,
                stale_marks: marks,
                ..record.clone()
            };
            let params: [&(dyn tokio_postgres::types::ToSql + Sync); 7] = [
                &org,
                &project,
                &rb,
                &record.day,
                &stale,
                &int(marks)?,
                &encode(&stored)?,
            ];
            let written = if current.is_none() {
                // A first write: another first write that won meanwhile
                // makes this one lose, never overwrites it.
                c.execute(
                    "INSERT INTO runbook.archive \
                     (org_id, project_id, runbook_id, day, stale, stale_marks, body) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (org_id, project_id, runbook_id, day) DO NOTHING",
                    &params,
                )
                .await
            } else {
                c.execute(
                    "UPDATE runbook.archive SET stale = $5, body = $7 \
                     WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4 \
                     AND stale_marks = $6",
                    &params,
                )
                .await
            }
            .map_err(db)?;
            if written == 0 {
                return Err(StoreError::InvalidBatch("archive.conflict"));
            }
            let listed: Vec<&str> = keep.iter().map(|r| r.execution_id.as_str()).collect();
            c.execute(
                "DELETE FROM runbook.archive_run \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4 \
                 AND NOT (execution_id = ANY($5))",
                &[&org, &project, &rb, &record.day, &listed],
            )
            .await
            .map_err(db)?;
            for run in keep {
                // A run's day never changes, but a crafted or imported row
                // elsewhere would collide: replace it only within the tenant.
                let inserted = c
                    .execute(
                        "INSERT INTO runbook.archive_run \
                         (execution_id, org_id, project_id, runbook_id, day, completed_at_ms, body) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7) \
                         ON CONFLICT (execution_id) DO UPDATE SET \
                         runbook_id = EXCLUDED.runbook_id, day = EXCLUDED.day, \
                         completed_at_ms = EXCLUDED.completed_at_ms, body = EXCLUDED.body \
                         WHERE runbook.archive_run.org_id = EXCLUDED.org_id \
                         AND runbook.archive_run.project_id = EXCLUDED.project_id",
                        &[
                            &run.execution_id.as_str(),
                            &org,
                            &project,
                            &rb,
                            &record.day,
                            &run.summary.completed_at_ms,
                            &encode(run)?,
                        ],
                    )
                    .await
                    .map_err(db)?;
                if inserted == 0 {
                    return Err(StoreError::InvalidBatch("archive.foreign_run"));
                }
            }
            // Membership: every listed run, summary row or not.
            let members: Vec<&str> = runs.iter().map(|r| r.execution_id.as_str()).collect();
            c.execute(
                "DELETE FROM runbook.archive_member \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4 \
                 AND NOT (execution_id = ANY($5))",
                &[&org, &project, &rb, &record.day, &members],
            )
            .await
            .map_err(db)?;
            for id in &members {
                let inserted = c
                    .execute(
                        "INSERT INTO runbook.archive_member \
                         (execution_id, org_id, project_id, runbook_id, day) \
                         VALUES ($1, $2, $3, $4, $5) \
                         ON CONFLICT (execution_id) DO UPDATE SET \
                         runbook_id = EXCLUDED.runbook_id, day = EXCLUDED.day \
                         WHERE runbook.archive_member.org_id = EXCLUDED.org_id \
                         AND runbook.archive_member.project_id = EXCLUDED.project_id",
                        &[id, &org, &project, &rb, &record.day],
                    )
                    .await
                    .map_err(db)?;
                if inserted == 0 {
                    return Err(StoreError::InvalidBatch("archive.foreign_run"));
                }
            }
            clear_backoff(c, scope, rb, record.day).await?;
            sweep(c, scope, rb, record.day, Some(record.digest)).await?;
            Ok(())
        })
        .await
    }
}

impl ArchiveIndex for PostgresExecutionStore {
    async fn record_archive(
        &self,
        scope: &TenantScope,
        record: ArchiveRecord,
        runs: Vec<ArchivedRun>,
        expected: Option<[u8; 32]>,
    ) -> Result<(), StoreError> {
        if &record.scope != scope
            || runs
                .iter()
                .any(|r| r.runbook_id != record.runbook_id || r.day != record.day)
        {
            return Err(StoreError::InvalidBatch("archive.foreign_run"));
        }
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self.record_once(scope, &record, &runs, expected).await {
                Err(StoreError::Backend("postgres.retry_transaction")) if attempts < 3 => {
                    self.retries_taken.set(self.retries_taken.get() + 1);
                    continue;
                }
                other => return other,
            }
        }
    }

    async fn mark_archive_stale(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<bool, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            let Some(current) = lock_file(c, scope, runbook_id.as_str(), day).await? else {
                return Ok(false);
            };
            c.execute(
                "UPDATE runbook.archive SET stale = true, stale_marks = stale_marks + 1 \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4",
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &runbook_id.as_str(),
                    &day,
                ],
            )
            .await
            .map_err(db)?;
            Ok(!current.stale)
        })
        .await
    }

    async fn get_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<Option<ArchiveRecord>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query_opt(
                "SELECT stale, stale_marks, body FROM runbook.archive \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4",
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &runbook_id.as_str(),
                    &day,
                ],
            )
            .await
            .map_err(db)?
            .map(|row| record_of(&row))
            .transpose()
        })
        .await
    }

    async fn list_archives(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        from_day: Option<i64>,
        to_day: Option<i64>,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query(
                "SELECT stale, stale_marks, body FROM runbook.archive \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 \
                 AND ($4::bigint IS NULL OR day >= $4) AND ($5::bigint IS NULL OR day <= $5) \
                 ORDER BY day DESC",
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &runbook_id.as_str(),
                    &from_day,
                    &to_day,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(record_of)
            .collect()
        })
        .await
    }

    async fn archived_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<Option<ArchivedRun>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query_opt(
                "SELECT body FROM runbook.archive_run \
                 WHERE execution_id = $1 AND org_id = $2 AND project_id = $3",
                &[
                    &execution_id.as_str(),
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
        })
        .await
    }

    async fn stale_archives(
        &self,
        scope: &TenantScope,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query(
                &format!(
                    "SELECT a.stale, a.stale_marks, a.body FROM runbook.archive a \
                     WHERE a.org_id = $1 AND a.project_id = $2 AND a.stale AND {} \
                     ORDER BY a.day, a.runbook_id COLLATE \"C\" LIMIT $4",
                    due("a.runbook_id", "a.day", "$3")
                ),
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &now_ms,
                    &int(limit as u64)?,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(record_of)
            .collect()
        })
        .await
    }

    async fn delete_archive(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        expected: [u8; 32],
    ) -> Result<bool, StoreError> {
        let (org, project, rb) = (
            scope.org_id.as_str(),
            scope.project_id.as_str(),
            runbook_id.as_str(),
        );
        self.transaction(READ_COMMITTED, async |c| {
            lock_day(c, scope, rb, day).await?;
            match lock_file(c, scope, rb, day).await? {
                None => return Ok(false),
                Some(r) if r.digest != expected => {
                    return Err(StoreError::InvalidBatch("archive.conflict"));
                }
                Some(_) => {}
            }
            // Never while a member run is live: its live copy would outlive
            // the file that is to keep it.
            let live: bool = c
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM runbook.archive_member m \
                     JOIN runbook.execution_owner o ON o.execution_id = m.execution_id \
                       AND o.org_id = m.org_id AND o.project_id = m.project_id \
                     JOIN runbook.execution e ON e.execution_id = m.execution_id \
                     WHERE m.org_id = $1 AND m.project_id = $2 AND m.runbook_id = $3 \
                     AND m.day = $4)",
                    &[&org, &project, &rb, &day],
                )
                .await
                .map_err(db)?
                .get(0);
            if live {
                return Err(StoreError::InvalidBatch("archive.member_live"));
            }
            // Summary rows stay (no cascade): they outlive their file.
            c.execute(
                "DELETE FROM runbook.archive \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4",
                &[&org, &project, &rb, &day],
            )
            .await
            .map_err(db)?;
            let all: Vec<String> = versions(c, scope, rb, day)
                .await?
                .into_iter()
                .map(|(key, _, _)| key)
                .collect();
            delete_keys(c, scope, &all).await?;
            // The member runs' retained uploads follow the file (PRD §14).
            let members = ids(c
                .query(
                    "DELETE FROM runbook.archive_member \
                     WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4 \
                     RETURNING execution_id",
                    &[&org, &project, &rb, &day],
                )
                .await
                .map_err(db)?)?;
            for id in &members {
                let retained = c
                    .query_opt(
                        "SELECT body FROM runbook.tombstone \
                         WHERE execution_id = $1 AND org_id = $2 AND project_id = $3",
                        &[&id.as_str(), &org, &project],
                    )
                    .await
                    .map_err(db)?
                    .map(|row| decode::<Tombstone>(row.get(0)))
                    .transpose()?
                    .is_some_and(|t| t.reason == RETENTION_REASON);
                if retained {
                    drop_retained_uploads(c, scope, rb, id).await?;
                }
            }
            clear_backoff(c, scope, rb, day).await?;
            Ok(true)
        })
        .await
    }

    async fn unarchived_days(
        &self,
        scope: &TenantScope,
        before_day: i64,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<(RunbookId, i64)>, StoreError> {
        let before_ms = before_day
            .checked_mul(DAY_MS)
            .ok_or(StoreError::InvalidBatch("store.day_overflow"))?;
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query(
                &format!(
                    "SELECT runbook_id, day FROM ( \
                       SELECT DISTINCT e.runbook_id, \
                         floor(e.completed_at_ms::numeric / {DAY_MS})::bigint AS day \
                       FROM runbook.execution e JOIN runbook.execution_owner o USING (execution_id) \
                       WHERE o.org_id = $1 AND o.project_id = $2 AND e.status IN {FINISHED} \
                       AND e.completed_at_ms < $3 \
                       AND NOT EXISTS (SELECT 1 FROM runbook.archive_run r \
                                       WHERE r.execution_id = e.execution_id)) d \
                     WHERE {} \
                     ORDER BY day, runbook_id COLLATE \"C\" LIMIT $5",
                    due("d.runbook_id", "d.day", "$4")
                ),
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &before_ms,
                    &now_ms,
                    &int(limit as u64)?,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| {
                Ok((
                    row.get::<_, &str>(0)
                        .parse()
                        .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
                    row.get::<_, i64>(1),
                ))
            })
            .collect()
        })
        .await
    }

    async fn archived_live_runs(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            ids(c
                .query(
                    &format!(
                        "SELECT e.execution_id FROM runbook.archive_run r \
                         JOIN runbook.archive a ON a.org_id = r.org_id \
                           AND a.project_id = r.project_id AND a.runbook_id = r.runbook_id \
                           AND a.day = r.day \
                         JOIN runbook.execution_owner o ON o.execution_id = r.execution_id \
                           AND o.org_id = r.org_id AND o.project_id = r.project_id \
                         JOIN runbook.execution e ON e.execution_id = r.execution_id \
                         WHERE r.org_id = $1 AND r.project_id = $2 AND NOT a.stale \
                         AND e.status IN {FINISHED} AND e.completed_at_ms < $3 \
                         ORDER BY e.completed_at_ms, e.execution_id COLLATE \"C\" LIMIT $4"
                    ),
                    &[
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        &completed_before_ms,
                        &int(limit as u64)?,
                    ],
                )
                .await
                .map_err(db)?)
        })
        .await
    }

    async fn expired_archives(
        &self,
        scope: &TenantScope,
        before_day: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            c.query(
                "SELECT stale, stale_marks, body FROM runbook.archive \
                 WHERE org_id = $1 AND project_id = $2 AND day < $3 \
                 ORDER BY day, runbook_id COLLATE \"C\" LIMIT $4",
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &before_day,
                    &int(limit as u64)?,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(record_of)
            .collect()
        })
        .await
    }

    async fn purge_summaries(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            let purged = c
                .execute(
                    "DELETE FROM runbook.archive_run WHERE execution_id IN ( \
                       SELECT r.execution_id FROM runbook.archive_run r \
                       WHERE r.org_id = $1 AND r.project_id = $2 AND r.completed_at_ms < $3 \
                       AND NOT EXISTS (SELECT 1 FROM runbook.execution_owner o \
                                       WHERE o.execution_id = r.execution_id) \
                       ORDER BY r.completed_at_ms, r.execution_id COLLATE \"C\" LIMIT $4)",
                    &[
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        &completed_before_ms,
                        &int(limit as u64)?,
                    ],
                )
                .await
                .map_err(db)?;
            usize::try_from(purged).map_err(|_| StoreError::Backend("postgres.negative_counter"))
        })
        .await
    }

    async fn archive_failed(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
        now_ms: i64,
    ) -> Result<u32, StoreError> {
        let (org, project, rb) = (
            scope.org_id.as_str(),
            scope.project_id.as_str(),
            runbook_id.as_str(),
        );
        self.transaction(READ_COMMITTED, async |c| {
            let attempts: i32 = c
                .query_one(
                    "INSERT INTO runbook.archive_attempt \
                     (org_id, project_id, runbook_id, day, attempts, next_attempt_at_ms) \
                     VALUES ($1, $2, $3, $4, 1, $5) \
                     ON CONFLICT (org_id, project_id, runbook_id, day) \
                     DO UPDATE SET attempts = runbook.archive_attempt.attempts + 1 \
                     RETURNING attempts",
                    &[&org, &project, &rb, &day, &now_ms],
                )
                .await
                .map_err(db)?
                .get(0);
            let attempts = u32::try_from(attempts)
                .map_err(|_| StoreError::Backend("postgres.negative_counter"))?;
            c.execute(
                "UPDATE runbook.archive_attempt SET next_attempt_at_ms = $5 \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4",
                &[
                    &org,
                    &project,
                    &rb,
                    &day,
                    &now_ms.saturating_add(backoff_ms(attempts)),
                ],
            )
            .await
            .map_err(db)?;
            Ok(attempts)
        })
        .await
    }

    async fn file_members(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.transaction(SNAPSHOT_READ, async |c| {
            ids(c
                .query(
                    "SELECT execution_id FROM runbook.archive_member \
                     WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3 AND day = $4 \
                     ORDER BY execution_id COLLATE \"C\"",
                    &[
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        &runbook_id.as_str(),
                        &day,
                    ],
                )
                .await
                .map_err(db)?)
        })
        .await
    }

    async fn sweep_leftover_versions(
        &self,
        scope: &TenantScope,
        limit: usize,
    ) -> Result<usize, StoreError> {
        // Days with a version the index does not name, older than the grace
        // period: a losing or interrupted write's bytes on a day nothing
        // revisits (re-check 3, finding 1).
        let days: Vec<(String, i64)> = self
            .transaction(SNAPSHOT_READ, async |c| {
                let cutoff = now_us(c).await? - VERSION_GRACE_MS * 1000;
                Ok(c.query(
                    "SELECT DISTINCT b.runbook_id, (b.key::jsonb #>> '{Archive,day}')::bigint AS day \
                     FROM runbook.blob b \
                     WHERE b.org_id = $1 AND b.project_id = $2 AND b.runbook_id IS NOT NULL \
                     AND b.created_at_us IS NOT NULL AND b.created_at_us < $3 \
                     AND NOT EXISTS (SELECT 1 FROM runbook.archive a \
                       WHERE a.org_id = b.org_id AND a.project_id = b.project_id \
                       AND a.runbook_id = b.runbook_id \
                       AND a.day = (b.key::jsonb #>> '{Archive,day}')::bigint \
                       AND a.body::jsonb -> 'digest' = b.key::jsonb #> '{Archive,digest}') \
                     ORDER BY 1, 2 LIMIT $4",
                    &[
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        &cutoff,
                        &i64::try_from(limit).unwrap_or(i64::MAX),
                    ],
                )
                .await
                .map_err(db)?
                .iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect())
            })
            .await?;
        let mut swept = 0;
        for (rb, day) in days {
            swept += self
                .transaction(READ_COMMITTED, async |c| {
                    lock_day(c, scope, &rb, day).await?;
                    let named = lock_file(c, scope, &rb, day).await?.map(|r| r.digest);
                    sweep(c, scope, &rb, day, named).await
                })
                .await?;
        }
        Ok(swept)
    }

    async fn sweep_archive_versions(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        day: i64,
    ) -> Result<(), StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            lock_day(c, scope, runbook_id.as_str(), day).await?;
            let named = lock_file(c, scope, runbook_id.as_str(), day)
                .await?
                .map(|r| r.digest);
            sweep(c, scope, runbook_id.as_str(), day, named).await?;
            Ok(())
        })
        .await
    }

    async fn purge_retained_run(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
        reason: &str,
    ) -> Result<Option<Tombstone>, StoreError> {
        if reason.trim().is_empty() || reason == RETENTION_REASON {
            return Err(StoreError::InvalidBatch("archive.reserved_reason"));
        }
        self.transaction(READ_COMMITTED, async |c| {
            purge_retained(c, scope, execution_id, reason).await
        })
        .await
    }
}
