//! CF5: the D1 half of `ArchiveIndex` (ARC1/ARC2) over `0029` and the
//! D1-only `archive_version` (1007). The files are R2's; everything a
//! decision needs is here, so each decision is one guarded batch, and the
//! R2 objects it makes obsolete are deleted by the caller after the batch
//! commits (a crash leaves version rows the sweep finds again).
//!
//! Stale marks, which Postgres takes inside the transaction that makes a
//! file out of date, are taken here:
//! - by the derived-row applier, for a commit to a listed run whose new
//!   sequence passed the listed revision (the commit's row reaches D1
//!   through the D56 outbox, so the mark is never lost, only bounded-late);
//! - by `record_archive` itself, for a listed live run whose summary in D1
//!   already passed the listed revision, or whose finalized evidence differs
//!   from the listed evidence;
//! - by finalizing evidence and by a single-run deletion (`uploads.rs`,
//!   `retention.rs`).
//!
//! D1 binds at most 100 parameters per statement, so a file's runs are
//! checked and written in chunks of rows (`VALUES` lists).
use crate::{
    catalog::{count, decode, err},
    sql::{Columns, D1, D1Error, Row, Statement, Value, guard, json, prepare, uint},
    uploads::mark_run_stale,
};
use runbook_core::{
    StoreError,
    archives::{ArchiveRecord, ArchivedRun, RETENTION_REASON, VERSION_GRACE_MS, backoff_ms},
    catalog::TenantScope,
    retention::Tombstone,
};
use runbook_model::{ExecutionId, RunbookId};

/// Rows per chunked statement: under D1's 100 bound parameters.
fn chunk_rows(params_per_row: usize, fixed: usize) -> usize {
    ((90 - fixed) / params_per_row).max(1)
}

fn values_list(rows: usize, width: usize) -> String {
    let row = format!("({})", vec!["?"; width].join(", "));
    vec![row; rows].join(", ")
}

/// What `delete_archive` left for the caller to delete in R2.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeletedFile {
    /// Every stored version of the day's file.
    pub versions: Vec<[u8; 32]>,
    /// Member runs retention had deleted: their uploads follow the file.
    pub retained_runs: Vec<ExecutionId>,
}

/// The archive index of every tenant; each call takes its tenant.
#[derive(Clone)]
pub struct D1Archives<D: D1> {
    db: D,
}

fn record_of(
    row: &Row,
    body: usize,
    stale: usize,
    marks: usize,
) -> Result<ArchiveRecord, StoreError> {
    let mut record: ArchiveRecord = decode(row, body)?;
    record.stale = row.flag(stale).map_err(err)?;
    record.stale_marks = row.unsigned(marks).map_err(err)?;
    Ok(record)
}

impl<D: D1> D1Archives<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }

    // ---- versions (the R2 objects of each day's file) ----

    /// Note a version about to be put (before its R2 write).
    pub async fn begin_version(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
        digest: &[u8; 32],
    ) -> Result<(), StoreError> {
        self.db
            .run(
                prepare(
                    "INSERT INTO archive_version (org_id, project_id, runbook_id, day, digest, \
                     seq, created_at_ms) SELECT ?, ?, ?, ?, ?, \
                     COALESCE((SELECT max(seq) FROM archive_version), 0) + 1, ? \
                     WHERE NOT EXISTS (SELECT 1 FROM archive_version WHERE org_id = ? \
                     AND project_id = ? AND runbook_id = ? AND day = ? AND digest = ?)",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day)
                .bind(digest.to_vec())
                .bind(self.db.now_ms())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day)
                .bind(digest.to_vec()),
            )
            .await
            .map(|_| ())
            .map_err(err)
    }

    /// The version's object is in R2.
    pub async fn version_stored(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
        digest: &[u8; 32],
    ) -> Result<(), StoreError> {
        self.db
            .run(
                prepare(
                    "UPDATE archive_version SET stored = 1 WHERE org_id = ? AND project_id = ? \
                     AND runbook_id = ? AND day = ? AND digest = ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day)
                .bind(digest.to_vec()),
            )
            .await
            .map(|_| ())
            .map_err(err)
    }

    /// Forget a version whose object is gone.
    pub async fn forget_version(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
        digest: &[u8; 32],
    ) -> Result<(), StoreError> {
        self.db
            .run(
                prepare(
                    "DELETE FROM archive_version WHERE org_id = ? AND project_id = ? \
                     AND runbook_id = ? AND day = ? AND digest = ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day)
                .bind(digest.to_vec()),
            )
            .await
            .map(|_| ())
            .map_err(err)
    }

    /// The day's versions the index does not name that are older than the
    /// named one (by write order) or than the grace period: what a sweep
    /// deletes. Never a younger in-flight write.
    pub async fn doomed_versions(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<Vec<[u8; 32]>, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT v.digest FROM archive_version v WHERE v.org_id = ?1 \
                     AND v.project_id = ?2 AND v.runbook_id = ?3 AND v.day = ?4 AND v.named = 0 \
                     AND (v.created_at_ms < ?5 OR v.seq < COALESCE((SELECT n.seq \
                       FROM archive_version n WHERE n.org_id = ?1 AND n.project_id = ?2 \
                       AND n.runbook_id = ?3 AND n.day = ?4 AND n.named = 1), 0)) \
                     ORDER BY v.seq",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day)
                .bind(self.db.now_ms() - VERSION_GRACE_MS),
            )
            .await
            .map_err(err)?;
        rows.iter().map(|row| row.digest(0).map_err(err)).collect()
    }

    /// Every version row of a day's file.
    pub async fn versions(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<Vec<[u8; 32]>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT digest FROM archive_version WHERE org_id = ? AND project_id = ? \
                     AND runbook_id = ? AND day = ? ORDER BY seq",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| row.digest(0).map_err(err))
            .collect()
    }

    /// Up to `limit` days with a version the index does not name, older
    /// than the grace period.
    pub async fn leftover_days(
        &self,
        scope: &TenantScope,
        limit: usize,
    ) -> Result<Vec<(RunbookId, i64)>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT DISTINCT runbook_id, day FROM archive_version \
                     WHERE org_id = ? AND project_id = ? AND named = 0 AND created_at_ms < ? \
                     ORDER BY day, runbook_id LIMIT ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(self.db.now_ms() - VERSION_GRACE_MS)
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| Ok((row.parse(0).map_err(err)?, row.integer(1).map_err(err)?)))
            .collect()
    }

    // ---- the index ----

    pub async fn get_archive(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<Option<ArchiveRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body, stale, stale_marks FROM archive WHERE org_id = ? \
                     AND project_id = ? AND runbook_id = ? AND day = ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| record_of(row, 0, 1, 2))
            .transpose()
    }

    pub async fn list_archives(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        from_day: Option<i64>,
        to_day: Option<i64>,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body, stale, stale_marks FROM archive WHERE org_id = ? \
                     AND project_id = ? AND runbook_id = ? AND day >= ? AND day <= ? \
                     ORDER BY day DESC",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                // Open ends as D1's exact-integer range (2^53 - 1).
                .bind(from_day.unwrap_or(-crate::sql::MAX_EXACT_INTEGER))
                .bind(to_day.unwrap_or(crate::sql::MAX_EXACT_INTEGER)),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| record_of(row, 0, 1, 2))
            .collect()
    }

    pub async fn archived_run(
        &self,
        scope: &TenantScope,
        id: &ExecutionId,
    ) -> Result<Option<ArchivedRun>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM archive_run WHERE execution_id = ? AND org_id = ? \
                     AND project_id = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }

    /// The digest of the file retention may delete `id` behind: a current
    /// (not stale) file that lists `id` at `revision`.
    pub async fn current_file_of(
        &self,
        scope: &TenantScope,
        id: &ExecutionId,
        revision: u64,
    ) -> Result<Option<ArchiveRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT a.body, a.stale, a.stale_marks FROM archive_run r JOIN archive a \
                     ON a.org_id = r.org_id AND a.project_id = r.project_id \
                     AND a.runbook_id = r.runbook_id AND a.day = r.day \
                     WHERE r.execution_id = ? AND r.org_id = ? AND r.project_id = ? \
                     AND a.stale = 0 AND json_extract(r.body, '$.revision') = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(uint(revision).map_err(err)?),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| record_of(row, 0, 1, 2))
            .transpose()
    }

    /// See `ArchiveIndex::record_archive`. One batch: the expected digest
    /// and a stored version, every listed run live or retained, the
    /// membership and summary rows, then the row itself with its stale
    /// flag computed from what the batch sees.
    pub async fn record_archive(
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
        let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
        let rb = record.runbook_id.as_str();
        let day = record.day;
        let file = |s: Statement| s.bind(org).bind(project).bind(rb).bind(day);
        let mut statements = vec![];
        statements.push(match expected {
            None => file(guard(
                "archive.conflict",
                "NOT EXISTS (SELECT 1 FROM archive WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ?)",
            )),
            Some(digest) => file(guard(
                "archive.conflict",
                "EXISTS (SELECT 1 FROM archive WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ? AND json_extract(body, '$.digest') = json(?))",
            ))
            .bind(json(&digest).map_err(err)?),
        });
        statements.push(
            file(guard(
                "archive.blob_missing",
                "EXISTS (SELECT 1 FROM archive_version WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ? AND digest = ? AND stored = 1)",
            ))
            .bind(record.digest.to_vec()),
        );
        // Every listed run is live, or deleted by retention. (A guard binds
        // its code first, as `?1`; numbered parameters start at `?2`.)
        let ids: Vec<&ExecutionId> = runs.iter().map(|r| &r.execution_id).collect();
        let per = chunk_rows(1, 3);
        for chunk in ids.chunks(per) {
            statements.push(
                guard(
                    "archive.run_gone",
                    &format!(
                        "NOT EXISTS (SELECT 1 FROM (VALUES {}) v WHERE NOT ( \
                         EXISTS (SELECT 1 FROM execution_owner o WHERE o.execution_id = v.column1 \
                           AND o.org_id = ?2 AND o.project_id = ?3) \
                         OR EXISTS (SELECT 1 FROM tombstone t WHERE t.execution_id = v.column1 \
                           AND t.org_id = ?2 AND t.project_id = ?3 \
                           AND json_extract(t.body, '$.reason') = ?4)))",
                        (0..chunk.len())
                            .map(|i| format!("(?{})", i + 5))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .bind(org)
                .bind(project)
                .bind(RETENTION_REASON)
                .binds(chunk.iter().map(|id| id.as_str())),
            );
        }
        // A folder built from the live run is refused once retention has
        // deleted the run: the writer starts over and copies the stored one.
        let live_built: Vec<&ExecutionId> = runs
            .iter()
            .filter(|r| r.gathered_live)
            .map(|r| &r.execution_id)
            .collect();
        for chunk in live_built.chunks(per) {
            statements.push(
                guard(
                    "archive.conflict",
                    &format!(
                        "NOT EXISTS (SELECT 1 FROM (VALUES {}) v WHERE NOT EXISTS ( \
                         SELECT 1 FROM execution_owner o WHERE o.execution_id = v.column1 \
                         AND o.org_id = ?2 AND o.project_id = ?3))",
                        (0..chunk.len())
                            .map(|i| format!("(?{})", i + 4))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .bind(org)
                .bind(project)
                .binds(chunk.iter().map(|id| id.as_str())),
            );
        }
        // Membership: every listed run, until the file is deleted.
        statements.push(file(prepare(
            "DELETE FROM archive_member WHERE org_id = ? AND project_id = ? AND runbook_id = ? \
             AND day = ?",
        )));
        for chunk in ids.chunks(chunk_rows(5, 0)) {
            let mut statement = prepare(format!(
                "INSERT INTO archive_member (execution_id, org_id, project_id, runbook_id, day) \
                 VALUES {} ON CONFLICT (execution_id) DO UPDATE SET org_id = excluded.org_id, \
                 project_id = excluded.project_id, runbook_id = excluded.runbook_id, \
                 day = excluded.day",
                values_list(chunk.len(), 5)
            ));
            for id in chunk {
                statement = statement
                    .bind(id.as_str())
                    .bind(org)
                    .bind(project)
                    .bind(rb)
                    .bind(day);
            }
            statements.push(statement);
        }
        // Summary rows: a live run's is written; a retained run keeps its
        // row only if it still has one (a purged summary is not restored).
        for chunk in runs.chunks(chunk_rows(3, 4)) {
            let mut params: Vec<Value> = vec![org.into(), project.into(), rb.into(), day.into()];
            for run in chunk {
                params.push(run.execution_id.as_str().into());
                params.push(run.summary.completed_at_ms.into());
                params.push(json(run).map_err(err)?);
            }
            let mut statement = prepare(format!(
                "INSERT OR REPLACE INTO archive_run (execution_id, org_id, project_id, runbook_id, \
                 day, completed_at_ms, body) \
                 SELECT v.column1, ?1, ?2, ?3, ?4, v.column2, v.column3 FROM (VALUES {}) v \
                 WHERE EXISTS (SELECT 1 FROM execution_owner o WHERE o.execution_id = v.column1 \
                   AND o.org_id = ?1 AND o.project_id = ?2) \
                 OR EXISTS (SELECT 1 FROM archive_run r WHERE r.execution_id = v.column1 \
                   AND r.org_id = ?1 AND r.project_id = ?2)",
                (0..chunk.len())
                    .map(|i| format!("(?{}, ?{}, ?{})", 5 + 3 * i, 6 + 3 * i, 7 + 3 * i))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            statement.params = params;
            statements.push(statement);
        }
        statements.push(file(prepare(
            "DELETE FROM archive_run WHERE org_id = ?1 AND project_id = ?2 AND runbook_id = ?3 \
             AND day = ?4 AND execution_id NOT IN (SELECT execution_id FROM archive_member \
               WHERE org_id = ?1 AND project_id = ?2 AND runbook_id = ?3 AND day = ?4)",
        )));
        // The row, stale if a mark landed since the writer read it, if a
        // listed live run's summary passed its listed revision, or if a
        // listed live run's finalized evidence differs from its listed one.
        let mut row = prepare(
            "INSERT INTO archive (org_id, project_id, runbook_id, day, stale, stale_marks, body) \
             SELECT ?1, ?2, ?3, ?4, \
               CASE WHEN COALESCE((SELECT stale_marks FROM archive WHERE org_id = ?1 \
                   AND project_id = ?2 AND runbook_id = ?3 AND day = ?4), 0) <> ?5 \
                 OR EXISTS (SELECT 1 FROM archive_run r JOIN run_summary s \
                   ON s.execution_id = r.execution_id JOIN execution_owner o \
                   ON o.execution_id = r.execution_id AND o.org_id = r.org_id \
                   AND o.project_id = r.project_id \
                   WHERE r.org_id = ?1 AND r.project_id = ?2 AND r.runbook_id = ?3 AND r.day = ?4 \
                   AND s.sequence > json_extract(r.body, '$.revision')) \
                 OR EXISTS (SELECT 1 FROM archive_run r JOIN evidence e \
                   ON e.execution_id = r.execution_id AND e.state = 'finalized' \
                   JOIN execution_owner o ON o.execution_id = r.execution_id \
                   WHERE r.org_id = ?1 AND r.project_id = ?2 AND r.runbook_id = ?3 AND r.day = ?4 \
                   AND NOT EXISTS (SELECT 1 FROM json_each(r.body, '$.evidence') j \
                     WHERE j.value = e.evidence_id)) \
                 OR EXISTS (SELECT 1 FROM archive_run r JOIN execution_owner o \
                   ON o.execution_id = r.execution_id, json_each(r.body, '$.evidence') j \
                   WHERE r.org_id = ?1 AND r.project_id = ?2 AND r.runbook_id = ?3 AND r.day = ?4 \
                   AND NOT EXISTS (SELECT 1 FROM evidence e WHERE e.evidence_id = j.value \
                     AND e.state = 'finalized')) \
               THEN 1 ELSE 0 END, \
               COALESCE((SELECT stale_marks FROM archive WHERE org_id = ?1 AND project_id = ?2 \
                 AND runbook_id = ?3 AND day = ?4), 0), ?6 \
             WHERE 1 \
             ON CONFLICT (org_id, project_id, runbook_id, day) DO UPDATE SET \
               stale = excluded.stale, stale_marks = excluded.stale_marks, body = excluded.body",
        );
        row.params = vec![
            org.into(),
            project.into(),
            rb.into(),
            day.into(),
            uint(record.stale_marks).map_err(err)?,
            json(&record).map_err(err)?,
        ];
        statements.push(row);
        statements.push(file(prepare(
            "DELETE FROM archive_attempt WHERE org_id = ? AND project_id = ? AND runbook_id = ? \
             AND day = ?",
        )));
        statements.push(file(prepare(
            "UPDATE archive_version SET named = 0 WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ?",
        )));
        statements.push(
            file(prepare(
                "UPDATE archive_version SET named = 1 WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ? AND digest = ?",
            ))
            .bind(record.digest.to_vec()),
        );
        self.db.batch(statements).await.map(|_| ()).map_err(err)
    }

    /// True when the file existed and was not already stale.
    pub async fn mark_archive_stale(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<bool, StoreError> {
        let file = |s: Statement| {
            s.bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day)
        };
        let outcomes = self
            .db
            .batch(vec![
                file(prepare(
                    "SELECT stale FROM archive WHERE org_id = ? AND project_id = ? \
                     AND runbook_id = ? AND day = ?",
                )),
                file(prepare(
                    "UPDATE archive SET stale = 1, stale_marks = stale_marks + 1 \
                     WHERE org_id = ? AND project_id = ? AND runbook_id = ? AND day = ?",
                )),
            ])
            .await
            .map_err(err)?;
        Ok(match outcomes.first().and_then(|o| o.rows.first()) {
            Some(row) => !row.flag(0).map_err(err)?,
            None => false,
        })
    }

    /// Up to `limit` stale files not backing off at `now_ms`, oldest first.
    pub async fn stale_archives(
        &self,
        scope: &TenantScope,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT a.body, a.stale, a.stale_marks FROM archive a \
                     WHERE a.org_id = ? AND a.project_id = ? AND a.stale = 1 \
                     AND NOT EXISTS (SELECT 1 FROM archive_attempt t WHERE t.org_id = a.org_id \
                       AND t.project_id = a.project_id AND t.runbook_id = a.runbook_id \
                       AND t.day = a.day AND t.next_attempt_at_ms > ?) \
                     ORDER BY a.day, a.runbook_id LIMIT ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(now_ms)
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| record_of(row, 0, 1, 2))
            .collect()
    }

    /// Delete the day's file (see `ArchiveIndex::delete_archive`): one
    /// batch for the rows; the caller deletes the returned versions and
    /// the retained runs' uploads in R2. `None` when there is no file.
    pub async fn delete_archive(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
        expected: [u8; 32],
    ) -> Result<Option<DeletedFile>, StoreError> {
        if self.get_archive(scope, runbook, day).await?.is_none() {
            return Ok(None);
        }
        let (org, project, rb) = (
            scope.org_id.as_str(),
            scope.project_id.as_str(),
            runbook.as_str(),
        );
        let file = |s: Statement| s.bind(org).bind(project).bind(rb).bind(day);
        let retained_runs: Vec<ExecutionId> = self
            .db
            .all(
                file(prepare(
                    "SELECT m.execution_id FROM archive_member m JOIN tombstone t \
                 ON t.execution_id = m.execution_id AND t.org_id = m.org_id \
                 AND t.project_id = m.project_id \
                 WHERE m.org_id = ? AND m.project_id = ? AND m.runbook_id = ? AND m.day = ? \
                 AND json_extract(t.body, '$.reason') = ?",
                ))
                .bind(RETENTION_REASON),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| row.parse(0).map_err(err))
            .collect::<Result<_, _>>()?;
        let versions = self.versions(scope, runbook, day).await?;
        let statements = vec![
            file(guard(
                "archive.conflict",
                "EXISTS (SELECT 1 FROM archive WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ? AND json_extract(body, '$.digest') = json(?))",
            ))
            .bind(json(&expected).map_err(err)?),
            file(guard(
                "archive.member_live",
                "NOT EXISTS (SELECT 1 FROM archive_member m JOIN execution_owner o \
                 ON o.execution_id = m.execution_id AND o.org_id = m.org_id \
                 AND o.project_id = m.project_id \
                 WHERE m.org_id = ? AND m.project_id = ? AND m.runbook_id = ? AND m.day = ?)",
            )),
            file(prepare(
                "DELETE FROM retained_evidence WHERE execution_id IN (SELECT m.execution_id \
                 FROM archive_member m JOIN tombstone t ON t.execution_id = m.execution_id \
                 WHERE m.org_id = ?1 AND m.project_id = ?2 AND m.runbook_id = ?3 AND m.day = ?4 \
                 AND json_extract(t.body, '$.reason') = ?5)",
            ))
            .bind(RETENTION_REASON),
            file(prepare(
                "DELETE FROM archive_member WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ?",
            )),
            file(prepare(
                "DELETE FROM archive_attempt WHERE org_id = ? AND project_id = ? \
                 AND runbook_id = ? AND day = ?",
            )),
            file(prepare(
                "DELETE FROM archive WHERE org_id = ? AND project_id = ? AND runbook_id = ? \
                 AND day = ?",
            )),
        ];
        self.db.batch(statements).await.map_err(err)?;
        Ok(Some(DeletedFile {
            versions,
            retained_runs,
        }))
    }

    /// Finished live runs no file lists yet, by (runbook, day) before
    /// `before_day`, not backing off; oldest day first.
    pub async fn unarchived_days(
        &self,
        scope: &TenantScope,
        before_day: i64,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<(RunbookId, i64)>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT DISTINCT s.runbook_id, s.completed_at_ms / 86400000 AS day \
                     FROM run_summary s JOIN execution_owner o ON o.execution_id = s.execution_id \
                     WHERE o.org_id = ?1 AND o.project_id = ?2 \
                     AND s.status IN ('succeeded', 'failed', 'cancelled') \
                     AND s.completed_at_ms IS NOT NULL AND s.completed_at_ms >= 0 \
                     AND s.completed_at_ms / 86400000 < ?3 \
                     AND NOT EXISTS (SELECT 1 FROM archive_run r WHERE r.execution_id = s.execution_id) \
                     AND NOT EXISTS (SELECT 1 FROM archive_attempt t WHERE t.org_id = ?1 \
                       AND t.project_id = ?2 AND t.runbook_id = s.runbook_id \
                       AND t.day = s.completed_at_ms / 86400000 AND t.next_attempt_at_ms > ?4) \
                     ORDER BY day, s.runbook_id LIMIT ?5",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(before_day)
                .bind(now_ms)
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| Ok((row.parse(0).map_err(err)?, row.integer(1).map_err(err)?)))
            .collect()
    }

    /// Finished live runs a current file lists, completed before
    /// `completed_before_ms`, oldest first.
    pub async fn archived_live_runs(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT r.execution_id FROM archive_run r \
                     JOIN archive a ON a.org_id = r.org_id AND a.project_id = r.project_id \
                       AND a.runbook_id = r.runbook_id AND a.day = r.day AND a.stale = 0 \
                     JOIN execution_owner o ON o.execution_id = r.execution_id \
                       AND o.org_id = r.org_id AND o.project_id = r.project_id \
                     JOIN run_summary s ON s.execution_id = r.execution_id \
                     WHERE r.org_id = ? AND r.project_id = ? \
                     AND s.status IN ('succeeded', 'failed', 'cancelled') \
                     AND s.completed_at_ms < ? \
                     ORDER BY s.completed_at_ms, r.execution_id LIMIT ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(completed_before_ms)
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| row.parse(0).map_err(err))
            .collect()
    }

    pub async fn expired_archives(
        &self,
        scope: &TenantScope,
        before_day: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body, stale, stale_marks FROM archive WHERE org_id = ? \
                     AND project_id = ? AND day < ? ORDER BY day, runbook_id LIMIT ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(before_day)
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| record_of(row, 0, 1, 2))
            .collect()
    }

    pub async fn purge_summaries(
        &self,
        scope: &TenantScope,
        completed_before_ms: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        Ok(self
            .db
            .all(
                prepare(
                    "DELETE FROM archive_run WHERE execution_id IN ( \
                       SELECT r.execution_id FROM archive_run r WHERE r.org_id = ?1 \
                       AND r.project_id = ?2 AND r.completed_at_ms < ?3 \
                       AND NOT EXISTS (SELECT 1 FROM execution_owner o \
                         WHERE o.execution_id = r.execution_id) \
                       ORDER BY r.completed_at_ms, r.execution_id LIMIT ?4) \
                     RETURNING execution_id",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(completed_before_ms)
                .bind(count(limit)?),
            )
            .await
            .map_err(err)?
            .len())
    }

    pub async fn archive_failed(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
        now_ms: i64,
    ) -> Result<u32, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "INSERT INTO archive_attempt (org_id, project_id, runbook_id, day, attempts, \
                     next_attempt_at_ms) VALUES (?, ?, ?, ?, 1, ?) \
                     ON CONFLICT (org_id, project_id, runbook_id, day) DO UPDATE SET \
                     attempts = attempts + 1 RETURNING attempts",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day)
                .bind(now_ms),
            )
            .await
            .map_err(err)?;
        let attempts = u32::try_from(
            rows.first()
                .ok_or(StoreError::Backend("d1.decode"))?
                .unsigned(0)
                .map_err(err)?,
        )
        .unwrap_or(u32::MAX);
        self.db
            .run(
                prepare(
                    "UPDATE archive_attempt SET next_attempt_at_ms = ? WHERE org_id = ? \
                     AND project_id = ? AND runbook_id = ? AND day = ?",
                )
                .bind(now_ms.saturating_add(backoff_ms(attempts)))
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day),
            )
            .await
            .map_err(err)?;
        Ok(attempts)
    }

    pub async fn file_members(
        &self,
        scope: &TenantScope,
        runbook: &RunbookId,
        day: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT execution_id FROM archive_member WHERE org_id = ? AND project_id = ? \
                     AND runbook_id = ? AND day = ? ORDER BY execution_id",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook.as_str())
                .bind(day),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| row.parse(0).map_err(err))
            .collect()
    }

    /// A single-run deletion of a run retention already deleted: the
    /// tombstone takes `reason`, the summary row goes, the file is marked
    /// stale and the retained evidence stops counting, in one batch. The
    /// caller deletes the run's uploads in R2. `None` when `id` has no
    /// retention tombstone in this tenant.
    pub async fn purge_retained_run(
        &self,
        scope: &TenantScope,
        id: &ExecutionId,
        reason: &str,
    ) -> Result<Option<Tombstone>, StoreError> {
        if reason.trim().is_empty() || reason == RETENTION_REASON {
            return Err(StoreError::InvalidBatch("archive.reserved_reason"));
        }
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT body FROM tombstone WHERE execution_id = ? AND org_id = ? \
                     AND project_id = ? AND json_extract(body, '$.reason') = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(RETENTION_REASON),
            )
            .await
            .map_err(err)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let mut tombstone: Tombstone = decode(row, 0)?;
        tombstone.reason = reason.into();
        let statements = vec![
            guard(
                "archive.conflict",
                "EXISTS (SELECT 1 FROM tombstone WHERE execution_id = ? \
                 AND json_extract(body, '$.reason') = ?)",
            )
            .bind(id.as_str())
            .bind(RETENTION_REASON),
            prepare("UPDATE tombstone SET body = ? WHERE execution_id = ?")
                .bind(json(&tombstone).map_err(err)?)
                .bind(id.as_str()),
            mark_run_stale(id),
            prepare("DELETE FROM archive_run WHERE execution_id = ?").bind(id.as_str()),
            prepare("DELETE FROM retained_evidence WHERE execution_id = ?").bind(id.as_str()),
        ];
        match self.db.batch(statements).await {
            Ok(_) => Ok(Some(tombstone)),
            // Another deletion changed the reason meanwhile.
            Err(D1Error::Guard(code)) if code == "archive.conflict" => Ok(None),
            Err(error) => Err(err(error)),
        }
    }
}
