//! MEM2: `MemoryStore` on PostgreSQL (migration 0026). Each call is one
//! READ COMMITTED transaction filtered by organization and project. The
//! per-scope quota is checked under a transaction-scoped advisory lock on
//! that scope, so concurrent writers cannot both take the last slot. The
//! D40 derived-row applier (`apply_proposal`) runs inside the execution
//! commit's transaction.
use crate::{
    execution::{PostgresExecutionStore, READ_COMMITTED},
    rows::{db, decode, encode, int, is_unique_violation, label, uint},
};
use runbook_core::{
    StoreError,
    catalog::{DerivedUpdate, TenantScope},
    memory_entries::{
        MEMORY_ENTRIES_PER_RUNBOOK, MemoryAction, MemoryEntryRecord, MemoryPatch, MemoryProposal,
        MemoryQuery, MemoryRecordPage, MemoryRevisionRecord, MemoryStore, actor_label, cursor_for,
        cursor_from, next_revision, notice_for, pruned_through, run_action, run_quota, text_words,
    },
};
use runbook_model::{MemoryId, MemoryScope, MemorySource, MemoryStatus, RunbookId};
use tokio_postgres::{Client, types::ToSql};

const NOW_MS: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

async fn now_ms(c: &Client) -> Result<i64, StoreError> {
    c.query_one(NOW_MS, &[]).await.map(|r| r.get(0)).map_err(db)
}

pub(crate) fn scope_key(scope: &MemoryScope) -> String {
    match scope {
        MemoryScope::Runbook { runbook_id } => runbook_id.to_string(),
        MemoryScope::Project => "project".into(),
    }
}

/// The words full-text search indexes and matches (`text_words`).
fn words(entry: &MemoryEntryRecord) -> String {
    text_words(&format!("{} {}", entry.title, entry.body.as_ref())).join(" ")
}

fn source_execution(entry: &MemoryEntryRecord) -> Option<&str> {
    match &entry.source {
        MemorySource::StepRun { execution_id, .. } => Some(execution_id.as_str()),
        _ => None,
    }
}

/// Serialize writers of one quota scope until the transaction ends.
async fn lock_quota(c: &Client, entry: &MemoryEntryRecord) -> Result<(), StoreError> {
    let key = format!(
        "memory:{}:{}:{}",
        entry.scope.org_id,
        entry.scope.project_id,
        scope_key(&entry.memory_scope)
    );
    c.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&key],
    )
    .await
    .map_err(db)?;
    Ok(())
}

async fn write_revision(c: &Client, record: &MemoryRevisionRecord) -> Result<(), StoreError> {
    let entry = &record.entry;
    c.execute(
        "INSERT INTO runbook.memory_revision \
         (memory_id, revision, org_id, project_id, action, at_ms, body) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        &[
            &entry.id.as_str(),
            &i64::from(entry.revision),
            &entry.scope.org_id.as_str(),
            &entry.scope.project_id.as_str(),
            &label(&record.action)?,
            &record.at_ms,
            &encode(record)?,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

/// Entries of `status` in the quota scope of `entry` (active entries and
/// proposals have separate quotas). The caller holds the quota lock.
async fn taken(
    c: &Client,
    entry: &MemoryEntryRecord,
    status: MemoryStatus,
) -> Result<u64, StoreError> {
    let taken = c
        .query_one(
            "SELECT count(*) FROM runbook.memory_entry WHERE org_id = $1 AND project_id = $2 \
             AND scope_key = $3 AND status = $4",
            &[
                &entry.scope.org_id.as_str(),
                &entry.scope.project_id.as_str(),
                &scope_key(&entry.memory_scope),
                &label(&status)?,
            ],
        )
        .await
        .map_err(db)?
        .get::<_, i64>(0);
    uint(taken)
}

/// Insert a new entry at revision 1 with its first revision, under the
/// quota lock. A taken ID is `memory.duplicate`.
pub(crate) async fn insert(
    c: &Client,
    entry: &MemoryEntryRecord,
    action: MemoryAction,
    actor: &str,
    limit: usize,
    proposal_sequence: Option<u64>,
) -> Result<(), StoreError> {
    lock_quota(c, entry).await?;
    if taken(c, entry, entry.status).await? >= limit as u64 {
        // An existing ID is reported first, as the memory adapter does.
        if exists(c, &entry.id).await? {
            return Err(StoreError::InvalidBatch("memory.duplicate"));
        }
        return Err(StoreError::InvalidBatch("memory.limit_reached"));
    }
    write_entry(c, entry, proposal_sequence, true).await?;
    write_revision(
        c,
        &MemoryRevisionRecord {
            action,
            actor: actor.to_owned(),
            at_ms: entry.updated_at_ms,
            entry: entry.clone(),
        },
    )
    .await
}

async fn exists(c: &Client, id: &MemoryId) -> Result<bool, StoreError> {
    Ok(c.query_opt(
        "SELECT 1 FROM runbook.memory_entry WHERE memory_id = $1",
        &[&id.as_str()],
    )
    .await
    .map_err(db)?
    .is_some())
}

async fn write_entry(
    c: &Client,
    entry: &MemoryEntryRecord,
    proposal_sequence: Option<u64>,
    new: bool,
) -> Result<(), StoreError> {
    let sequence = proposal_sequence.map(int).transpose()?;
    let params: [&(dyn ToSql + Sync); 15] = [
        &entry.id.as_str(),
        &entry.scope.org_id.as_str(),
        &entry.scope.project_id.as_str(),
        &scope_key(&entry.memory_scope),
        &label(&entry.status)?,
        &entry.step_id,
        &entry.tags,
        &i64::from(entry.revision),
        &entry.expires_at_ms,
        &entry.created_at_ms,
        &entry.updated_at_ms,
        &source_execution(entry),
        &sequence,
        &encode(entry)?,
        &words(entry),
    ];
    let sql = if new {
        "INSERT INTO runbook.memory_entry (memory_id, org_id, project_id, scope_key, status, \
         step_id, tags, revision, expires_at_ms, created_at_ms, updated_at_ms, \
         source_execution_id, proposal_sequence, body, search_tsv) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
         to_tsvector('simple', $15))"
    } else {
        "UPDATE runbook.memory_entry SET status = $5, step_id = $6, tags = $7, revision = $8, \
         expires_at_ms = $9, updated_at_ms = $11, body = $14, \
         search_tsv = to_tsvector('simple', $15) \
         WHERE memory_id = $1 AND org_id = $2 AND project_id = $3 AND scope_key = $4 \
         AND created_at_ms = $10 AND source_execution_id IS NOT DISTINCT FROM $12 \
         AND proposal_sequence IS NOT DISTINCT FROM $13"
    };
    match c.execute(sql, &params).await {
        Ok(1) => Ok(()),
        Ok(_) => Err(StoreError::Backend("postgres.memory_write_lost")),
        Err(e) if is_unique_violation(&e) => Err(StoreError::InvalidBatch("memory.duplicate")),
        Err(e) => Err(db(e)),
    }
}

/// D40 derived-row applier (a proposal, or a MEM6 live save): true when
/// applied now; false for a replay or an older sequence of the same
/// execution. Runs in the caller's transaction.
pub(crate) async fn apply_proposal(
    c: &Client,
    scope: &TenantScope,
    proposal: &MemoryProposal,
    limit: usize,
) -> Result<bool, StoreError> {
    let entry = &proposal.entry;
    if &entry.scope != scope
        || entry.status == MemoryStatus::Archived
        || source_execution(entry) != Some(proposal.execution_id.as_str())
    {
        return Err(StoreError::InvalidBatch("memory.invalid_entry"));
    }
    let applied = c
        .query_opt(
            "SELECT 1 FROM runbook.memory_entry WHERE source_execution_id = $1 \
             AND proposal_sequence >= $2 LIMIT 1",
            &[&proposal.execution_id.as_str(), &int(proposal.sequence)?],
        )
        .await
        .map_err(db)?
        .is_some();
    if applied {
        return Ok(false);
    }
    let MemorySource::StepRun { actor, .. } = &entry.source else {
        return Err(StoreError::InvalidBatch("memory.invalid_entry"));
    };
    let actor = actor_label(actor);
    let mut entry = entry.clone();
    entry.revision = 1;
    entry.last_writer = Some(actor.clone());
    insert(
        c,
        &entry,
        run_action(&entry),
        &actor,
        limit,
        Some(proposal.sequence),
    )
    .await?;
    Ok(true)
}

/// Apply every memory write of one derived row (the execution commit's
/// transaction, or a redelivery), each under the quota of its status.
pub(crate) async fn apply_derived(
    c: &Client,
    scope: &TenantScope,
    update: &DerivedUpdate,
) -> Result<(), StoreError> {
    for proposal in &update.memory {
        apply_proposal(c, scope, proposal, run_quota(proposal.entry.status)).await?;
    }
    Ok(())
}

async fn current(
    c: &Client,
    scope: &TenantScope,
    id: &MemoryId,
    lock: bool,
) -> Result<Option<(MemoryEntryRecord, Option<u64>)>, StoreError> {
    let sql = if lock {
        "SELECT body, proposal_sequence FROM runbook.memory_entry \
         WHERE memory_id = $1 AND org_id = $2 AND project_id = $3 FOR UPDATE"
    } else {
        "SELECT body, proposal_sequence FROM runbook.memory_entry \
         WHERE memory_id = $1 AND org_id = $2 AND project_id = $3"
    };
    let row = c
        .query_opt(
            sql,
            &[
                &id.as_str(),
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
            ],
        )
        .await
        .map_err(db)?;
    row.map(|row| {
        let sequence: Option<i64> = row.get(1);
        Ok((
            decode::<MemoryEntryRecord>(row.get(0))?,
            sequence.map(uint).transpose()?,
        ))
    })
    .transpose()
}

pub(crate) async fn revisions(
    c: &Client,
    scope: &TenantScope,
    id: &MemoryId,
) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
    c.query(
        "SELECT body FROM runbook.memory_revision \
         WHERE memory_id = $1 AND org_id = $2 AND project_id = $3 ORDER BY revision",
        &[
            &id.as_str(),
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
        ],
    )
    .await
    .map_err(db)?
    .iter()
    .map(|row| decode(row.get(0)))
    .collect()
}

/// Every entry of a tenant with its history and proposal sequence, by ID
/// (portable export).
pub(crate) async fn export(
    c: &Client,
    scope: &TenantScope,
) -> Result<Vec<runbook_core::portable::MemoryRow>, StoreError> {
    let rows = c
        .query(
            "SELECT memory_id, proposal_sequence FROM runbook.memory_entry \
             WHERE org_id = $1 AND project_id = $2 ORDER BY memory_id COLLATE \"C\"",
            &[&scope.org_id.as_str(), &scope.project_id.as_str()],
        )
        .await
        .map_err(db)?;
    let mut out = vec![];
    for row in rows {
        let id: MemoryId = row
            .get::<_, &str>(0)
            .parse()
            .map_err(|_| StoreError::Backend("postgres.decode_failed"))?;
        let sequence: Option<i64> = row.get(1);
        out.push(runbook_core::portable::MemoryRow {
            revisions: revisions(c, scope, &id).await?,
            proposal_sequence: sequence.map(uint).transpose()?,
        });
    }
    Ok(out)
}

/// Install validated archive rows (portable import; plain inserts).
pub(crate) async fn import(
    c: &Client,
    rows: &[runbook_core::portable::MemoryRow],
) -> Result<(), StoreError> {
    for row in rows {
        let Some(last) = row.revisions.last() else {
            return Err(StoreError::InvalidBatch("archive.invalid_memory"));
        };
        write_entry(c, &last.entry, row.proposal_sequence, true).await?;
        for revision in &row.revisions {
            write_revision(c, revision).await?;
        }
    }
    Ok(())
}

impl MemoryStore for PostgresExecutionStore {
    async fn memory_time_ms(&self) -> Result<i64, StoreError> {
        self.backend_time_ms().await
    }
    async fn create_memory(
        &self,
        scope: &TenantScope,
        mut entry: MemoryEntryRecord,
        actor: &str,
        limit: usize,
    ) -> Result<MemoryEntryRecord, StoreError> {
        let action = match entry.status {
            MemoryStatus::Active => MemoryAction::Create,
            MemoryStatus::Proposed if source_execution(&entry).is_none() => MemoryAction::Propose,
            _ => return Err(StoreError::InvalidBatch("memory.invalid_entry")),
        };
        if &entry.scope != scope {
            return Err(StoreError::InvalidBatch("memory.invalid_entry"));
        }
        self.transaction(READ_COMMITTED, async |c| {
            let now = now_ms(c).await?;
            entry.created_at_ms = now;
            entry.updated_at_ms = now;
            entry.revision = 1;
            entry.last_writer = Some(actor.to_owned());
            insert(c, &entry, action, actor, limit, None).await?;
            // MEM6 review fix 3: an agent's save notifies, in this transaction.
            if let Some(event) = notice_for(&entry, action, actor) {
                crate::project_outbox::insert(c, scope, &event).await?;
            }
            Ok(entry)
        })
        .await
    }
    async fn apply_memory_proposal(
        &self,
        scope: &TenantScope,
        proposal: MemoryProposal,
        limit: usize,
    ) -> Result<bool, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            apply_proposal(c, scope, &proposal, limit).await
        })
        .await
    }
    async fn get_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Option<MemoryEntryRecord>, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            Ok(current(c, scope, id, false).await?.map(|(e, _)| e))
        })
        .await
    }
    async fn memory_revisions(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        self.transaction(READ_COMMITTED, async |c| revisions(c, scope, id).await)
            .await
    }
    async fn memory_revision(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        revision: u32,
    ) -> Result<Option<MemoryRevisionRecord>, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            c.query_opt(
                "SELECT body FROM runbook.memory_revision WHERE memory_id = $1 \
                 AND org_id = $2 AND project_id = $3 AND revision = $4",
                &[
                    &id.as_str(),
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &i64::from(revision),
                ],
            )
            .await
            .map_err(db)?
            .map(|row| decode(row.get(0)))
            .transpose()
        })
        .await
    }
    async fn memory_revision_page(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        after: u32,
        limit: usize,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        let limit = int(limit as u64)?;
        self.transaction(READ_COMMITTED, async |c| {
            c.query(
                "SELECT body FROM runbook.memory_revision WHERE memory_id = $1 \
                 AND org_id = $2 AND project_id = $3 AND revision > $4 \
                 ORDER BY revision LIMIT $5",
                &[
                    &id.as_str(),
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &i64::from(after),
                    &limit,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| decode(row.get(0)))
            .collect()
        })
        .await
    }
    async fn patch_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        expected_revision: u32,
        patch: MemoryPatch,
        actor: &str,
    ) -> Result<MemoryEntryRecord, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            let (stored, sequence) = current(c, scope, id, true)
                .await?
                .ok_or(StoreError::NotFound)?;
            if stored.revision != expected_revision {
                return Err(StoreError::InvalidBatch("memory.conflict"));
            }
            let now = now_ms(c).await?;
            let (next, action) = next_revision(&stored, patch, actor, now)?;
            // Accepting or restoring takes an active slot, under the same
            // lock as creates.
            if next.status == MemoryStatus::Active && stored.status != MemoryStatus::Active {
                lock_quota(c, &stored).await?;
                if taken(c, &stored, MemoryStatus::Active).await?
                    >= MEMORY_ENTRIES_PER_RUNBOOK as u64
                {
                    return Err(StoreError::InvalidBatch("memory.limit_reached"));
                }
            }
            write_entry(c, &next, sequence, false).await?;
            write_revision(
                c,
                &MemoryRevisionRecord {
                    action,
                    actor: actor.to_owned(),
                    at_ms: next.updated_at_ms,
                    entry: next.clone(),
                },
            )
            .await?;
            if let Some(through) = pruned_through(&next) {
                c.execute(
                    "DELETE FROM runbook.memory_revision WHERE memory_id = $1 AND org_id = $2 \
                     AND project_id = $3 AND revision > 1 AND revision <= $4 \
                     AND revision <> $5",
                    &[
                        &id.as_str(),
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        &i64::from(through),
                        &i64::from(next.person_revision),
                    ],
                )
                .await
                .map_err(db)?;
            }
            if let Some(event) = notice_for(&next, action, actor) {
                crate::project_outbox::insert(c, scope, &event).await?;
            }
            Ok(next)
        })
        .await
    }
    async fn search_memory(
        &self,
        scope: &TenantScope,
        query: &MemoryQuery,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<MemoryRecordPage, StoreError> {
        let position = cursor
            .as_deref()
            .map(|c| cursor_from(scope, query, c))
            .transpose()?;
        self.transaction(READ_COMMITTED, async |c| {
            let now = now_ms(c).await?;
            let org = scope.org_id.as_str();
            let project = scope.project_id.as_str();
            let within: Vec<String> = query.within.iter().map(scope_key).collect();
            let status = query.status.as_ref().map(label).transpose()?;
            let text = query
                .text
                .as_deref()
                .map(|t| text_words(t).join(" "))
                .filter(|t| !t.is_empty());
            let (at, after) = match &position {
                Some((at, id)) => (Some(*at), Some(id.as_str())),
                None => (None, None),
            };
            let fetch = int(limit as u64 + 1)?;
            let rows = c
                .query(
                    "SELECT body FROM runbook.memory_entry \
                     WHERE org_id = $1 AND project_id = $2 \
                     AND (cardinality($3::text[]) = 0 OR scope_key = ANY($3)) \
                     AND ($4::text IS NULL OR status = $4) \
                     AND ($5::text IS NULL OR step_id = $5) \
                     AND tags @> $6::text[] \
                     AND ($7 OR expires_at_ms IS NULL OR expires_at_ms > $8) \
                     AND ($9::text IS NULL OR search_tsv @@ plainto_tsquery('simple', $9)) \
                     AND ($10::bigint IS NULL OR updated_at_ms < $10 OR \
                          (updated_at_ms = $10 AND memory_id COLLATE \"C\" < $11::text COLLATE \"C\")) \
                     ORDER BY updated_at_ms DESC, memory_id COLLATE \"C\" DESC LIMIT $12",
                    &[
                        &org,
                        &project,
                        &within,
                        &status,
                        &query.step_id,
                        &query.tags,
                        &query.include_expired,
                        &now,
                        &text,
                        &at,
                        &after,
                        &fetch,
                    ],
                )
                .await
                .map_err(db)?;
            let mut items: Vec<MemoryEntryRecord> = rows
                .iter()
                .map(|row| decode(row.get(0)))
                .collect::<Result<_, _>>()?;
            let more = items.len() > limit;
            items.truncate(limit);
            let next_cursor = more
                .then(|| items.last().map(|last| cursor_for(scope, query, last)))
                .flatten();
            Ok(MemoryRecordPage { items, next_cursor })
        })
        .await
    }
    async fn memory_for_step(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        step_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntryRecord>, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            let now = now_ms(c).await?;
            c.query(
                "SELECT body FROM runbook.memory_entry \
                 WHERE org_id = $1 AND project_id = $2 AND scope_key IN ($3, 'project') \
                 AND status = 'active' AND step_id = $4 \
                 AND (expires_at_ms IS NULL OR expires_at_ms > $5) \
                 ORDER BY (body::jsonb->>'last_writer' LIKE 'human:%') IS TRUE DESC, \
                 updated_at_ms DESC, memory_id COLLATE \"C\" DESC LIMIT $6",
                &[
                    &scope.org_id.as_str(),
                    &scope.project_id.as_str(),
                    &runbook_id.as_str(),
                    &step_id,
                    &now,
                    &int(limit as u64)?,
                ],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|row| decode(row.get(0)))
            .collect()
        })
        .await
    }
}

impl PostgresExecutionStore {
    /// Org-scoped routing read (never grants access): the project that owns
    /// a memory entry in this organization, if any.
    pub async fn memory_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &MemoryId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        self.routing_project(
            "SELECT project_id FROM runbook.memory_entry WHERE org_id = $1 AND memory_id = $2",
            org_id,
            id.as_str(),
        )
        .await
    }
    /// Org-scoped routing read: the project that owns a runbook.
    pub async fn runbook_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &RunbookId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        self.routing_project(
            "SELECT project_id FROM runbook.runbook WHERE org_id = $1 AND runbook_id = $2",
            org_id,
            id.as_str(),
        )
        .await
    }
    async fn routing_project(
        &self,
        sql: &str,
        org_id: &runbook_model::OrganizationId,
        id: &str,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            c.query_opt(sql, &[&org_id.as_str(), &id])
                .await
                .map_err(db)?
                .map(|row| {
                    row.get::<_, &str>(0)
                        .parse()
                        .map_err(|_| StoreError::Backend("postgres.decode_failed"))
                })
                .transpose()
        })
        .await
    }
}
