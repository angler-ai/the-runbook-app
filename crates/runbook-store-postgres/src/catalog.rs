//! Tenant-bound catalog. Derived rows change only inside the execution commit or
//! a redelivery of a recorded update. Lock order: execution head, then admission
//! pool, then admission rows, so concurrent releases and admits cannot deadlock.
use crate::{
    PostgresExecutionStore,
    rows::{connection, db, decode, encode, int, is_unique_violation, label, uint},
};
use runbook_core::{
    CommitResult, StoreError,
    catalog::{
        Admission, AdmissionState, AdmitRequest, Assignment, CatalogStore, DerivedUpdate,
        DraftRevision, RunSummary, TenantScope,
    },
};
use runbook_model::ExecutionId;
use std::collections::BTreeSet;
use tokio_postgres::Client;

pub(crate) async fn owner(c: &Client, id: &ExecutionId) -> Result<Option<TenantScope>, StoreError> {
    c.query_opt(
        "SELECT org_id, project_id FROM runbook.execution_owner WHERE execution_id = $1",
        &[&id.as_str()],
    )
    .await
    .map_err(db)?
    .map(|row| {
        Ok(TenantScope {
            org_id: parse(row.get(0))?,
            project_id: parse(row.get(1))?,
        })
    })
    .transpose()
}

fn parse<T: std::str::FromStr>(text: &str) -> Result<T, StoreError> {
    text.parse()
        .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))
}

/// Record or confirm ownership. A different tenant is a scope conflict.
pub(crate) async fn claim_owner(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    c.execute(
        "INSERT INTO runbook.execution_owner (execution_id, org_id, project_id) \
         VALUES ($1, $2, $3) ON CONFLICT (execution_id) DO NOTHING",
        &[
            &id.as_str(),
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
        ],
    )
    .await
    .map_err(db)?;
    if owner(c, id).await?.as_ref() != Some(scope) {
        return Err(StoreError::InvalidBatch("catalog.execution_scope_conflict"));
    }
    Ok(())
}

pub(crate) fn state_of(text: &str) -> Result<AdmissionState, StoreError> {
    match text {
        "admitted" => Ok(AdmissionState::Admitted),
        "queued" => Ok(AdmissionState::Queued),
        "released" => Ok(AdmissionState::Released),
        _ => Err(StoreError::Backend("postgres.invalid_admission_state")),
    }
}
pub(crate) fn state_label(state: AdmissionState) -> &'static str {
    match state {
        AdmissionState::Admitted => "admitted",
        AdmissionState::Queued => "queued",
        AdmissionState::Released => "released",
    }
}

/// Apply one derived update. Old or duplicate sequences are ignored.
pub(crate) async fn apply_derived(
    c: &Client,
    scope: &TenantScope,
    mut update: DerivedUpdate,
) -> Result<(), StoreError> {
    let id = update.summary.execution_id.clone();
    let key = id.as_str();
    if owner(c, &id).await?.is_some_and(|o| &o != scope) {
        return Err(StoreError::InvalidBatch("catalog.execution_scope_conflict"));
    }
    // D40: proposals first, each idempotent by (execution, sequence) on its
    // own, so a later summary never hides an earlier row's proposals.
    crate::memory_entries::apply_derived(c, scope, &update).await?;
    let prior = c
        .query_opt(
            "SELECT sequence, created_at_ms FROM runbook.run_summary WHERE execution_id = $1 FOR UPDATE",
            &[&key],
        )
        .await
        .map_err(db)?;
    if let Some(row) = &prior {
        if uint(row.get(0))? >= update.summary.sequence {
            return Ok(());
        }
        // Only the start stamps the creation time; later summaries keep it.
        update.summary.created_at_ms = row.get(1);
    }
    let mut steps = BTreeSet::new();
    if update
        .assignments
        .iter()
        .any(|row| row.execution_id != id || !steps.insert(&row.step_run_id))
    {
        return Err(StoreError::InvalidBatch("catalog.invalid_assignments"));
    }
    claim_owner(c, scope, &id).await?;
    c.execute(
        "DELETE FROM runbook.assignment WHERE execution_id = $1",
        &[&key],
    )
    .await
    .map_err(db)?;
    for (ordinal, row) in update.assignments.iter().enumerate() {
        insert_assignment(c, key, ordinal, row).await?;
    }
    if update.release_concurrency {
        release(c, scope, key).await?;
    }
    write_summary(c, &update.summary, true).await
}

/// One assignment row: the exact body plus the scalar inbox columns the
/// `/v1/me/tasks` keyset query filters and orders on (R27, migration 0025).
pub(crate) async fn insert_assignment(
    c: &Client,
    execution_id: &str,
    ordinal: usize,
    row: &Assignment,
) -> Result<(), StoreError> {
    let ordinal = i32::try_from(ordinal)
        .map_err(|_| StoreError::InvalidBatch("catalog.too_many_assignments"))?;
    let recipients: Vec<String> = row.recipients.iter().map(recipient_key).collect();
    let voted_by: Vec<&str> = row.voted_by.iter().map(|u| u.as_str()).collect();
    c.execute(
        "INSERT INTO runbook.assignment \
         (execution_id, ordinal, step_run_id, body, kind, updated_at_ms, recipients, voted_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[
            &execution_id,
            &ordinal,
            &row.step_run_id.as_str(),
            &encode(row)?,
            &assignment_kind_label(row.kind),
            &row.updated_at_ms,
            &recipients,
            &voted_by,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}
pub(crate) fn recipient_key(recipient: &runbook_model::RecipientRef) -> String {
    match recipient {
        runbook_model::RecipientRef::User(user) => format!("user:{user}"),
        runbook_model::RecipientRef::Team(team) => format!("team:{team}"),
    }
}
pub(crate) fn assignment_kind_label(kind: runbook_core::catalog::AssignmentKind) -> &'static str {
    use runbook_core::catalog::AssignmentKind;
    match kind {
        AssignmentKind::Work => "work",
        AssignmentKind::Approval => "approval",
        AssignmentKind::Handoff => "handoff",
    }
}

/// The run summary row with its search columns (B3.3). `replace` upserts
/// (derived updates); D3 imports pass false so an existing row is a
/// unique violation, never overwritten.
pub(crate) async fn write_summary(
    c: &Client,
    s: &runbook_core::catalog::RunSummary,
    replace: bool,
) -> Result<(), StoreError> {
    let key = s.execution_id.as_str();
    let fields_json = serde_json::to_string(&s.fields)
        .map_err(|_| StoreError::Backend("postgres.encode_failed"))?;
    let insert = "INSERT INTO runbook.run_summary \
         (execution_id, runbook_id, sequence, status, completed_at_ms, body, \
          title, tags, fields, created_by, created_at_ms, search_tsv) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, ($9::text)::jsonb, $10, $11, \
                 to_tsvector('simple', $7 || ' ' || array_to_string($8::text[], ' ')))";
    let upsert = " ON CONFLICT (execution_id) DO UPDATE SET \
         runbook_id = EXCLUDED.runbook_id, sequence = EXCLUDED.sequence, \
         status = EXCLUDED.status, completed_at_ms = EXCLUDED.completed_at_ms, \
         body = EXCLUDED.body, title = EXCLUDED.title, tags = EXCLUDED.tags, \
         fields = EXCLUDED.fields, created_by = EXCLUDED.created_by, \
         created_at_ms = EXCLUDED.created_at_ms, search_tsv = EXCLUDED.search_tsv";
    c.execute(
        &if replace {
            format!("{insert}{upsert}")
        } else {
            insert.to_owned()
        },
        &[
            &key,
            &s.runbook_id.as_str(),
            &int(s.sequence)?,
            &label(&s.status)?,
            &s.completed_at_ms,
            &encode(s)?,
            &s.title,
            &s.tags,
            &fields_json,
            &s.created_by,
            &s.created_at_ms,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// Release one admission, then promote queued runs oldest first while the
/// pool is under its current limit. A lowered limit admits no one new until
/// the active count drops below it (D25).
async fn release(c: &Client, scope: &TenantScope, id: &str) -> Result<(), StoreError> {
    let Some(row) = c
        .query_opt(
            "SELECT key FROM runbook.admission WHERE execution_id = $1",
            &[&id],
        )
        .await
        .map_err(db)?
    else {
        return Ok(());
    };
    let pool_key: String = row.get(0);
    let pool = [
        &scope.org_id.as_str() as &(dyn tokio_postgres::types::ToSql + Sync),
        &scope.project_id.as_str(),
        &pool_key,
    ];
    let row = c
        .query_one(
            "SELECT active, concurrency_limit FROM runbook.admission_pool \
             WHERE org_id = $1 AND project_id = $2 AND key = $3 FOR UPDATE",
            &pool,
        )
        .await
        .map_err(db)?;
    let (mut active, limit): (i32, i32) = (row.get(0), row.get(1));
    let previous = state_of(
        c.query_one(
            "SELECT state FROM runbook.admission WHERE execution_id = $1 FOR UPDATE",
            &[&id],
        )
        .await
        .map_err(db)?
        .get(0),
    )?;
    if previous == AdmissionState::Released {
        return Ok(());
    }
    c.execute(
        "UPDATE runbook.admission SET state = 'released', queue_order = NULL \
         WHERE execution_id = $1",
        &[&id],
    )
    .await
    .map_err(db)?;
    if previous == AdmissionState::Queued {
        return Ok(());
    }
    active -= 1;
    while active < limit {
        let next = c
            .query_opt(
                "SELECT execution_id FROM runbook.admission \
                 WHERE org_id = $1 AND project_id = $2 AND key = $3 AND queue_order IS NOT NULL \
                 ORDER BY queue_order LIMIT 1 FOR UPDATE",
                &pool,
            )
            .await
            .map_err(db)?;
        let Some(row) = next else { break };
        let next: String = row.get(0);
        c.execute(
            "UPDATE runbook.admission SET state = 'admitted', queue_order = NULL, \
             admitted_at_ms = floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint \
             WHERE execution_id = $1",
            &[&next],
        )
        .await
        .map_err(db)?;
        active += 1;
    }
    c.execute(
        "UPDATE runbook.admission_pool SET active = $4 \
         WHERE org_id = $1 AND project_id = $2 AND key = $3",
        &[pool[0], pool[1], pool[2], &active],
    )
    .await
    .map_err(db)?;
    Ok(())
}

impl PostgresExecutionStore {
    fn tenant(&self) -> [&str; 2] {
        [self.scope.org_id.as_str(), self.scope.project_id.as_str()]
    }
}

impl CatalogStore for PostgresExecutionStore {
    async fn read_draft(&self, key: &str) -> Result<Option<DraftRevision>, StoreError> {
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT revision, source FROM runbook.draft \
                 WHERE org_id = $1 AND project_id = $2 AND key = $3",
                &[&org, &project, &key],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok(DraftRevision {
                    key: key.into(),
                    revision: uint(row.get(0))?,
                    source: row.get(1),
                })
            })
            .transpose()
    }

    /// Compare-and-set in one statement; the row's revision is the precondition.
    async fn replace_draft(
        &self,
        key: &str,
        expected: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        if key.is_empty() {
            return Err(StoreError::InvalidBatch("catalog.empty_draft_key"));
        }
        let next = expected
            .checked_add(1)
            .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?;
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        let changed = if expected == 0 {
            conn.client()
                .execute(
                    "INSERT INTO runbook.draft (org_id, project_id, key, revision, source) \
                     VALUES ($1, $2, $3, 1, $4) ON CONFLICT DO NOTHING",
                    &[&org, &project, &key, &source],
                )
                .await
        } else {
            conn.client()
                .execute(
                    "UPDATE runbook.draft SET revision = $5, source = $4 \
                     WHERE org_id = $1 AND project_id = $2 AND key = $3 AND revision = $6",
                    &[&org, &project, &key, &source, &int(next)?, &int(expected)?],
                )
                .await
        }
        .map_err(db)?;
        Ok(if changed == 1 {
            CommitResult::Committed(next)
        } else {
            CommitResult::Conflict
        })
    }

    async fn list_runs(&self) -> Result<Vec<RunSummary>, StoreError> {
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query(
                "SELECT r.body FROM runbook.run_summary r \
                 JOIN runbook.execution_owner o USING (execution_id) \
                 WHERE o.org_id = $1 AND o.project_id = $2 ORDER BY r.execution_id",
                &[&org, &project],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| decode(row.get(0)))
            .collect()
    }

    async fn get_run(&self, id: &ExecutionId) -> Result<Option<RunSummary>, StoreError> {
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT r.body FROM runbook.run_summary r \
                 JOIN runbook.execution_owner o USING (execution_id) \
                 WHERE r.execution_id = $1 AND o.org_id = $2 AND o.project_id = $3",
                &[&id.as_str(), &org, &project],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
    }

    async fn list_assignments(&self) -> Result<Vec<Assignment>, StoreError> {
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query(
                "SELECT a.body FROM runbook.assignment a \
                 JOIN runbook.execution_owner o USING (execution_id) \
                 WHERE o.org_id = $1 AND o.project_id = $2 \
                 ORDER BY a.execution_id, a.ordinal",
                &[&org, &project],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| decode(row.get(0)))
            .collect()
    }

    async fn admit(&self, request: AdmitRequest) -> Result<Admission, StoreError> {
        self.transaction(crate::execution::READ_COMMITTED, async |c| {
            admit(c, &self.scope, request).await
        })
        .await
    }

    async fn create_runbook(
        &self,
        record: runbook_core::catalog::RunbookRecord,
        source: String,
    ) -> Result<runbook_model::DraftView, StoreError> {
        self.create_runbook_row(record, source).await
    }
    async fn read_runbook(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Option<runbook_core::catalog::RunbookRecord>, StoreError> {
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT slug, origin, archived FROM runbook.runbook \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3",
                &[&org, &project, &id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok(runbook_core::catalog::RunbookRecord {
                    runbook_id: id.clone(),
                    slug: row.get(0),
                    origin: crate::publish::origin_of(row.get(1))?,
                    archived: row.get(2),
                })
            })
            .transpose()
    }
    async fn get_draft(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Option<runbook_model::DraftView>, StoreError> {
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT draft_source, draft_revision FROM runbook.runbook \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3",
                &[&org, &project, &id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| {
                crate::publish::draft_view(
                    id,
                    crate::publish::decode_draft_source(row.get(0))?,
                    row.get(1),
                )
            })
            .transpose()
    }
    async fn save_draft(
        &self,
        id: &runbook_model::RunbookId,
        expected_revision: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        self.transaction(crate::execution::READ_COMMITTED, async |c| {
            self.save_draft_in(c, id, expected_revision, source).await
        })
        .await
    }
    async fn publish_revision(
        &self,
        id: &runbook_model::RunbookId,
        expected_revision: u64,
        prepared: runbook_core::publish::PreparedPublication,
    ) -> Result<runbook_model::PublishedVersionView, StoreError> {
        self.publish_row(id, expected_revision, prepared).await
    }
    async fn create_preview(
        &self,
        id: &runbook_model::RunbookId,
        prepared: runbook_core::publish::PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<runbook_model::VersionView, StoreError> {
        self.preview_row(id, prepared, branch, commit_sha).await
    }
    async fn publish_git(
        &self,
        id: &runbook_model::RunbookId,
        prepared: runbook_core::publish::PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<runbook_model::PublishedVersionView, StoreError> {
        self.publish_git_row(id, prepared, branch, commit_sha).await
    }
    async fn archive_git(&self, id: &runbook_model::RunbookId) -> Result<(), StoreError> {
        self.archive_git_row(id).await
    }
    async fn detach_git(&self, id: &runbook_model::RunbookId) -> Result<(), StoreError> {
        self.detach_git_row(id).await
    }
    async fn resolve_version(
        &self,
        id: &runbook_model::RunbookId,
        selector: Option<runbook_model::VersionSelector>,
    ) -> Result<runbook_model::VersionView, StoreError> {
        self.resolve_row(id, selector).await
    }
    async fn version_record(
        &self,
        runbook_id: &runbook_model::RunbookId,
        version_id: &runbook_model::VersionId,
    ) -> Result<Option<runbook_core::catalog::VersionRecord>, StoreError> {
        self.version_row(runbook_id, version_id).await
    }
    async fn list_versions(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Vec<runbook_model::PublishedVersionView>, StoreError> {
        self.transaction(crate::execution::SNAPSHOT_READ, async |c| {
            crate::publish::runbook_exists(c, &self.scope, id).await?;
            let mut views = Vec::new();
            for row in c
                .query(
                    &format!(
                        "SELECT {} FROM runbook.runbook_version \
                         WHERE runbook_id = $1 AND number IS NOT NULL ORDER BY number",
                        crate::publish::VERSION_COLUMNS
                    ),
                    &[&id.as_str()],
                )
                .await
                .map_err(db)?
            {
                let record = crate::publish::version_record(&row)?;
                views.push(runbook_model::PublishedVersionView {
                    warnings: Vec::new(),
                    runbook_id: id.clone(),
                    runbook_version_id: record.version_id,
                    runbook_version: record
                        .number
                        .ok_or(StoreError::Backend("postgres.invalid_version_number"))?,
                });
            }
            Ok(views)
        })
        .await
    }
    async fn list_previews(
        &self,
        id: &runbook_model::RunbookId,
    ) -> Result<Vec<runbook_model::VersionView>, StoreError> {
        self.transaction(crate::execution::SNAPSHOT_READ, async |c| {
            crate::publish::runbook_exists(c, &self.scope, id).await?;
            let mut views = Vec::new();
            for row in c
                .query(
                    &format!(
                        "SELECT {} FROM runbook.runbook_version \
                         WHERE runbook_id = $1 AND number IS NULL ORDER BY version_id",
                        crate::publish::VERSION_COLUMNS
                    ),
                    &[&id.as_str()],
                )
                .await
                .map_err(db)?
            {
                views.push(crate::publish::version_record(&row)?.view(id));
            }
            Ok(views)
        })
        .await
    }

    async fn record_skill_version(
        &self,
        version: runbook_core::catalog::SkillVersion,
    ) -> Result<runbook_core::catalog::SkillVersion, StoreError> {
        self.record_skill_row(version).await
    }
    async fn skill_version(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<runbook_core::catalog::SkillVersion>, StoreError> {
        self.skill_row(hash).await
    }
    async fn latest_skills(&self) -> Result<Vec<runbook_core::catalog::SkillVersion>, StoreError> {
        self.latest_skill_rows().await
    }
    async fn versions_pinning_skill(
        &self,
        hash: &[u8; 32],
    ) -> Result<
        Vec<(
            runbook_model::RunbookId,
            runbook_core::catalog::VersionRecord,
        )>,
        StoreError,
    > {
        self.pinning_rows(hash).await
    }

    async fn execution_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT project_id FROM runbook.execution_owner \
                 WHERE org_id = $1 AND execution_id = $2",
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
    }
    async fn tombstone_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &ExecutionId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT project_id FROM runbook.tombstone \
                 WHERE org_id = $1 AND execution_id = $2",
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
    }
    async fn search_runs(
        &self,
        filters: runbook_core::search::RunFilters,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<runbook_core::search::RunPage, StoreError> {
        self.search_rows(filters, cursor, limit).await
    }
    async fn runbook_run_stats(
        &self,
        runbook_ids: &[runbook_model::RunbookId],
        active_cap: usize,
    ) -> Result<
        std::collections::BTreeMap<runbook_model::RunbookId, runbook_core::search::RunbookRunStats>,
        StoreError,
    > {
        self.runbook_run_stats_rows(runbook_ids, active_cap).await
    }

    async fn admission_pool(
        &self,
        key: &str,
    ) -> Result<runbook_core::concurrency::PoolView, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        admission_pool(conn.client(), &self.scope, key, false).await
    }
    async fn admission(&self, id: &ExecutionId) -> Result<Option<Admission>, StoreError> {
        let [org, project] = self.tenant();
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT a.state, a.admitted_at_ms FROM runbook.admission a \
                 JOIN runbook.execution_owner o USING (execution_id) \
                 WHERE a.execution_id = $1 AND o.org_id = $2 AND o.project_id = $3",
                &[&id.as_str(), &org, &project],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok(Admission {
                    execution_id: id.clone(),
                    state: state_of(row.get(0))?,
                    admitted_at_ms: row.get(1),
                })
            })
            .transpose()
    }
}

/// LANG3: the key's holders in admission order (oldest first, ties by
/// execution ID), then its waiters in queue order. With `lock` the
/// admission rows are taken FOR UPDATE; the caller locks the pool row
/// first ([`lock_pool`]), in the B2.2a order.
pub(crate) async fn admission_pool(
    c: &Client,
    scope: &TenantScope,
    key: &str,
    lock: bool,
) -> Result<runbook_core::concurrency::PoolView, StoreError> {
    let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let rows = c
        .query(
            &format!(
                "SELECT execution_id, state FROM runbook.admission \
                 WHERE org_id = $1 AND project_id = $2 AND key = $3 \
                 AND state IN ('admitted', 'queued') \
                 ORDER BY state = 'queued', \
                 CASE WHEN state = 'admitted' THEN admitted_at_ms END, \
                 queue_order, execution_id COLLATE \"C\"{suffix}"
            ),
            &[&org, &project, &key],
        )
        .await
        .map_err(db)?;
    let holders = rows
        .iter()
        .map(|row| {
            Ok(runbook_core::concurrency::PoolHolder {
                execution_id: row
                    .get::<_, String>(0)
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_execution_id"))?,
                state: state_of(row.get(1))?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(runbook_core::concurrency::PoolView { holders })
}

/// LANG3: create the key's pool row if it is new (as `admit_pooled` does)
/// and lock it. This row lock is what serializes concurrent starts on one
/// key while each decides its conflict policy: without it two starts read
/// the same pool and the loser of `admit_pooled` queues despite `reject`
/// (pinned by the Postgres `reject` races on a new and on an idle key).
pub(crate) async fn lock_pool(
    c: &Client,
    scope: &TenantScope,
    request: &AdmitRequest,
) -> Result<(), StoreError> {
    ensure_pool(c, scope, request).await?;
    c.execute(
        "SELECT 1 FROM runbook.admission_pool \
         WHERE org_id = $1 AND project_id = $2 AND key = $3 FOR UPDATE",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &request.key,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// Create the key's pool row if it is new.
async fn ensure_pool(
    c: &Client,
    scope: &TenantScope,
    request: &AdmitRequest,
) -> Result<(), StoreError> {
    let limit = i32::try_from(request.limit)
        .map_err(|_| StoreError::InvalidBatch("catalog.invalid_admission"))?;
    c.execute(
        "INSERT INTO runbook.admission_pool (org_id, project_id, key, concurrency_limit, active) \
         VALUES ($1, $2, $3, $4, 0) ON CONFLICT DO NOTHING",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &request.key,
            &limit,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// Release one admission inside the caller's transaction (LANG3 eviction).
pub(crate) async fn release_admission(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    release(c, scope, id.as_str()).await
}

async fn now_ms(c: &Client) -> Result<i64, StoreError> {
    c.query_one(
        "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
        &[],
    )
    .await
    .map(|row| row.get(0))
    .map_err(db)
}

async fn admit(
    c: &Client,
    scope: &TenantScope,
    request: AdmitRequest,
) -> Result<Admission, StoreError> {
    let id = request.execution_id.as_str();
    let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
    if crate::retention::tombstoned(c, &request.execution_id).await? {
        return Err(StoreError::NotFound);
    }
    if owner(c, &request.execution_id)
        .await?
        .is_some_and(|o| &o != scope)
    {
        return Err(StoreError::InvalidBatch("catalog.execution_scope_conflict"));
    }
    if request.key.is_empty() || request.idempotency_key.is_empty() || request.limit == 0 {
        return Err(StoreError::InvalidBatch("catalog.invalid_admission"));
    }
    // Serialize retries of one start request so a racing retry sees the first answer.
    c.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended($1 || '/' || $2 || '/' || $3, 0))",
        &[&org, &project, &request.idempotency_key],
    )
    .await
    .map_err(db)?;
    if let Some(row) = c
        .query_opt(
            "SELECT request FROM runbook.start_idempotency \
             WHERE org_id = $1 AND project_id = $2 AND idempotency_key = $3",
            &[&org, &project, &request.idempotency_key],
        )
        .await
        .map_err(db)?
    {
        let prior: AdmitRequest = decode(row.get(0))?;
        if prior != request {
            return Err(StoreError::InvalidBatch("catalog.idempotency_conflict"));
        }
        let current = c
            .query_one(
                "SELECT state, admitted_at_ms FROM runbook.admission WHERE execution_id = $1",
                &[&id],
            )
            .await
            .map_err(db)?;
        return Ok(Admission {
            execution_id: request.execution_id,
            state: state_of(current.get(0))?,
            admitted_at_ms: current.get(1),
        });
    }
    let row = admit_pooled(c, scope, request.clone(), now_ms(c).await?).await?;
    c.execute(
        "INSERT INTO runbook.start_idempotency \
         (org_id, project_id, idempotency_key, execution_id, request) \
         VALUES ($1, $2, $3, $4, $5)",
        &[
            &org,
            &project,
            &request.idempotency_key,
            &id,
            &encode(&request)?,
        ],
    )
    .await
    .map_err(db)?;
    Ok(row)
}

/// Pool admission without start-idempotency bookkeeping. D25: the request
/// limit replaces the pool limit; raising it promotes waiters (FIFO,
/// admitted_at stamped) before the new request is considered.
pub(crate) async fn admit_pooled(
    c: &Client,
    scope: &TenantScope,
    request: AdmitRequest,
    now: i64,
) -> Result<Admission, StoreError> {
    admit_pooled_at(c, scope, request, now, false).await
}

/// Pooled admission; with `first`, a start that has to wait is queued
/// ahead of every other waiter (LANG3 `cancel_previous`).
pub(crate) async fn admit_pooled_at(
    c: &Client,
    scope: &TenantScope,
    request: AdmitRequest,
    now: i64,
    first: bool,
) -> Result<Admission, StoreError> {
    let id = request.execution_id.as_str();
    let (org, project) = (scope.org_id.as_str(), scope.project_id.as_str());
    if owner(c, &request.execution_id)
        .await?
        .is_some_and(|o| &o != scope)
    {
        return Err(StoreError::InvalidBatch("catalog.execution_scope_conflict"));
    }
    if request.key.is_empty() || request.limit == 0 {
        return Err(StoreError::InvalidBatch("catalog.invalid_admission"));
    }
    let limit = i32::try_from(request.limit)
        .map_err(|_| StoreError::InvalidBatch("catalog.invalid_admission"))?;
    let taken: bool = c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM runbook.admission WHERE execution_id = $1)",
            &[&id],
        )
        .await
        .map_err(db)?
        .get(0);
    if taken {
        return Err(StoreError::InvalidBatch("catalog.admission_conflict"));
    }
    c.execute(
        "INSERT INTO runbook.admission_pool (org_id, project_id, key, concurrency_limit, active) \
         VALUES ($1, $2, $3, $4, 0) ON CONFLICT DO NOTHING",
        &[&org, &project, &request.key, &limit],
    )
    .await
    .map_err(db)?;
    let pool = c
        .query_one(
            "SELECT active FROM runbook.admission_pool \
             WHERE org_id = $1 AND project_id = $2 AND key = $3 FOR UPDATE",
            &[&org, &project, &request.key],
        )
        .await
        .map_err(db)?;
    let mut active: i32 = pool.get(0);
    let pool_params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] =
        [&org, &project, &request.key];
    while active < limit {
        let next = c
            .query_opt(
                "SELECT execution_id FROM runbook.admission \
                 WHERE org_id = $1 AND project_id = $2 AND key = $3 AND queue_order IS NOT NULL \
                 ORDER BY queue_order LIMIT 1 FOR UPDATE",
                &pool_params,
            )
            .await
            .map_err(db)?;
        let Some(row) = next else { break };
        let next: String = row.get(0);
        c.execute(
            "UPDATE runbook.admission SET state = 'admitted', queue_order = NULL, \
             admitted_at_ms = $2 WHERE execution_id = $1",
            &[&next, &now],
        )
        .await
        .map_err(db)?;
        active += 1;
    }
    let state = if active < limit {
        active += 1;
        AdmissionState::Admitted
    } else {
        AdmissionState::Queued
    };
    c.execute(
        "UPDATE runbook.admission_pool SET concurrency_limit = $4, active = $5 \
         WHERE org_id = $1 AND project_id = $2 AND key = $3",
        &[&org, &project, &request.key, &limit, &active],
    )
    .await
    .map_err(db)?;
    claim_owner(c, scope, &request.execution_id).await?;
    let inserted = c
        .execute(
            "INSERT INTO runbook.admission \
             (execution_id, org_id, project_id, key, state, queue_order, request, admitted_at_ms) \
             VALUES ($1, $2, $3, $4, $5, \
             CASE WHEN $5 = 'queued' THEN COALESCE(\
             CASE WHEN $8 THEN (SELECT MIN(queue_order) - 1 FROM runbook.admission \
             WHERE org_id = $2 AND project_id = $3 AND key = $4 AND queue_order IS NOT NULL) END, \
             nextval('runbook.admission_queue_order')) END, $6, $7)",
            &[
                &id,
                &org,
                &project,
                &request.key,
                &state_label(state),
                &encode(&request)?,
                &(state == AdmissionState::Admitted).then_some(now),
                &first,
            ],
        )
        .await;
    match inserted {
        Ok(_) => Ok(Admission {
            execution_id: request.execution_id,
            state,
            admitted_at_ms: (state == AdmissionState::Admitted).then_some(now),
        }),
        Err(error) if is_unique_violation(&error) => {
            Err(StoreError::InvalidBatch("catalog.admission_conflict"))
        }
        Err(error) => Err(db(error)),
    }
}

impl PostgresExecutionStore {
    pub(crate) async fn save_draft_in(
        &self,
        c: &Client,
        id: &runbook_model::RunbookId,
        expected_revision: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        let runbook = crate::publish::lock_runbook(c, &self.scope, id).await?;
        if runbook.origin == runbook_core::catalog::RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.git_managed"));
        }
        if runbook.draft_revision != expected_revision {
            return Ok(CommitResult::Conflict);
        }
        let next = expected_revision
            .checked_add(1)
            .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?;
        c.execute(
            "UPDATE runbook.runbook SET draft_source = $4, draft_revision = $5 \
                 WHERE org_id = $1 AND project_id = $2 AND runbook_id = $3",
            &[
                &self.scope.org_id.as_str(),
                &self.scope.project_id.as_str(),
                &id.as_str(),
                &source.as_bytes(),
                &int(next)?,
            ],
        )
        .await
        .map_err(db)?;
        Ok(CommitResult::Committed(next))
    }
}
