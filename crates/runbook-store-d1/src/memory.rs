//! MEM2's `MemoryStore` on D1 (migration 0026, FTS5). Every call filters by
//! organization and project. The per-scope quota and the ID check are
//! guards in the insert's batch (D1 batches are serialized, so two writers
//! cannot both take the last slot). A patch is fenced by the revision it
//! read. The D40 derived-row applier ([`apply_proposal_statements`]) runs in
//! the derived-row applier's batch (`applier.rs`).
//!
//! Full-text search: `search_tsv` holds the entry's words (`text_words` of
//! title and body), which decide a match exactly; the FTS5 index
//! (`memory_entry_fts`) narrows the candidates first.
use crate::sql::{Columns, D1, D1Error, Row, Statement, guard, json, placeholders, prepare, uint};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    memory_entries::{
        MemoryAction, MemoryEntryRecord, MemoryPatch, MemoryProposal, MemoryQuery,
        MemoryRecordPage, MemoryRevisionRecord, MemoryStore, actor_label, cursor_for, cursor_from,
        next_revision, notice_for, pruned_through, text_words,
    },
};
use runbook_model::{MemoryId, MemoryScope, MemorySource, MemoryStatus, RunbookId};

#[derive(Clone)]
pub struct D1Memory<D: D1> {
    db: D,
}
impl<D: D1> D1Memory<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }
}

fn err(error: D1Error) -> StoreError {
    crate::store_error(error)
}
fn decode<T: serde::de::DeserializeOwned>(row: &Row, index: usize) -> Result<T, StoreError> {
    row.json(index).map_err(err)
}
fn label<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        _ => Err(StoreError::Backend("d1.encode")),
    }
}
pub(crate) fn scope_key(scope: &MemoryScope) -> String {
    match scope {
        MemoryScope::Runbook { runbook_id } => runbook_id.to_string(),
        MemoryScope::Project => "project".into(),
    }
}
fn words(entry: &MemoryEntryRecord) -> String {
    text_words(&format!("{} {}", entry.title, entry.body.as_ref())).join(" ")
}
fn source_execution(entry: &MemoryEntryRecord) -> Option<&str> {
    match &entry.source {
        MemorySource::StepRun { execution_id, .. } => Some(execution_id.as_str()),
        _ => None,
    }
}

fn revision_statement(record: &MemoryRevisionRecord) -> Result<Statement, StoreError> {
    let entry = &record.entry;
    Ok(prepare(
        "INSERT INTO memory_revision (memory_id, revision, org_id, project_id, action, at_ms, body) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(entry.id.as_str())
    .bind(i64::from(entry.revision))
    .bind(entry.scope.org_id.as_str())
    .bind(entry.scope.project_id.as_str())
    .bind(label(&record.action)?)
    .bind(record.at_ms)
    .bind(json(record).map_err(err)?))
}

/// CF8: an archived entry as it stands, with every revision (portable
/// import; the archive was validated, so no quota applies).
pub(crate) fn import_statements(
    row: &runbook_core::portable::MemoryRow,
) -> Result<Vec<Statement>, StoreError> {
    let last = row
        .revisions
        .last()
        .ok_or(StoreError::InvalidBatch("archive.invalid_memory"))?;
    let entry = &last.entry;
    let mut out = vec![
        guard(
            "memory.duplicate",
            "NOT EXISTS (SELECT 1 FROM memory_entry WHERE memory_id = ?)",
        )
        .bind(entry.id.as_str()),
        entry_statement(entry, row.proposal_sequence)?,
    ];
    for revision in &row.revisions {
        out.push(revision_statement(revision)?);
    }
    Ok(out)
}

fn entry_statement(
    entry: &MemoryEntryRecord,
    proposal_sequence: Option<u64>,
) -> Result<Statement, StoreError> {
    Ok(prepare(
        "INSERT INTO memory_entry (memory_id, org_id, project_id, scope_key, status, step_id, \
         tags, revision, expires_at_ms, created_at_ms, updated_at_ms, source_execution_id, \
         proposal_sequence, body, search_tsv) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(entry.id.as_str())
    .bind(entry.scope.org_id.as_str())
    .bind(entry.scope.project_id.as_str())
    .bind(scope_key(&entry.memory_scope))
    .bind(label(&entry.status)?)
    .bind(entry.step_id.clone())
    .bind(json(&entry.tags).map_err(err)?)
    .bind(i64::from(entry.revision))
    .bind(entry.expires_at_ms)
    .bind(entry.created_at_ms)
    .bind(entry.updated_at_ms)
    .bind(source_execution(entry).map(str::to_owned))
    .bind(proposal_sequence.map(uint).transpose().map_err(err)?)
    .bind(json(entry).map_err(err)?)
    .bind(words(entry)))
}

/// Fails the batch with `memory.limit_reached` when the quota scope of
/// `entry` holds `limit` entries of `status` (active entries and proposals
/// have separate quotas).
fn quota_guard(
    entry: &MemoryEntryRecord,
    status: &MemoryStatus,
    limit: impl Into<crate::Value>,
) -> Result<Statement, StoreError> {
    Ok(guard(
        "memory.limit_reached",
        "(SELECT count(*) FROM memory_entry WHERE org_id = ? AND project_id = ? \
         AND scope_key = ? AND status = ?) < ?",
    )
    .bind(entry.scope.org_id.as_str())
    .bind(entry.scope.project_id.as_str())
    .bind(scope_key(&entry.memory_scope))
    .bind(label(status)?)
    .bind(limit))
}

/// A new entry at revision 1 with its first revision, guarded by the ID
/// (reported first, as the memory adapter does) and the quota.
pub(crate) fn insert_statements(
    entry: &MemoryEntryRecord,
    action: MemoryAction,
    actor: &str,
    limit: usize,
    proposal_sequence: Option<u64>,
) -> Result<Vec<Statement>, StoreError> {
    let limit = uint(u64::try_from(limit).unwrap_or(u64::MAX).min(1 << 40)).map_err(err)?;
    Ok(vec![
        guard(
            "memory.duplicate",
            "NOT EXISTS (SELECT 1 FROM memory_entry WHERE memory_id = ?)",
        )
        .bind(entry.id.as_str()),
        quota_guard(entry, &entry.status, limit)?,
        entry_statement(entry, proposal_sequence)?,
        revision_statement(&MemoryRevisionRecord {
            action,
            actor: actor.to_owned(),
            at_ms: entry.updated_at_ms,
            entry: entry.clone(),
        })?,
    ])
}

/// D40: the statements that apply one run's memory write (a proposal or a
/// MEM6 live save), guarded so a replay or an older sequence of the same
/// execution writes nothing (`memory.applied`).
pub(crate) fn apply_proposal_statements(
    scope: &TenantScope,
    proposal: &MemoryProposal,
    limit: usize,
) -> Result<Vec<Statement>, StoreError> {
    let entry = &proposal.entry;
    if &entry.scope != scope
        || entry.status == MemoryStatus::Archived
        || source_execution(entry) != Some(proposal.execution_id.as_str())
    {
        return Err(StoreError::InvalidBatch("memory.invalid_entry"));
    }
    let MemorySource::StepRun { actor, .. } = &entry.source else {
        return Err(StoreError::InvalidBatch("memory.invalid_entry"));
    };
    let actor = actor_label(actor);
    let mut entry = entry.clone();
    entry.revision = 1;
    entry.last_writer = Some(actor.clone());
    let mut statements = vec![
        guard(
            "memory.applied",
            "NOT EXISTS (SELECT 1 FROM memory_entry WHERE source_execution_id = ? \
             AND proposal_sequence >= ?)",
        )
        .bind(proposal.execution_id.as_str())
        .bind(uint(proposal.sequence).map_err(err)?),
    ];
    statements.extend(insert_statements(
        &entry,
        runbook_core::memory_entries::run_action(&entry),
        &actor,
        limit,
        Some(proposal.sequence),
    )?);
    Ok(statements)
}

impl<D: D1> D1Memory<D> {
    async fn current(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Option<MemoryEntryRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM memory_entry WHERE memory_id = ? AND org_id = ? \
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
}

impl<D: D1> MemoryStore for D1Memory<D> {
    async fn memory_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.db.now_ms())
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
        let now = self.db.now_ms();
        entry.created_at_ms = now;
        entry.updated_at_ms = now;
        entry.revision = 1;
        entry.last_writer = Some(actor.to_owned());
        let mut statements = insert_statements(&entry, action, actor, limit, None)?;
        // MEM6 review fix 3: an agent's save notifies, in this batch.
        if let Some(event) = notice_for(&entry, action, actor) {
            statements.push(crate::catalog::project_event(scope, &event)?);
        }
        match self.db.batch(statements).await {
            Ok(_) => Ok(entry),
            Err(D1Error::Unique) => Err(StoreError::InvalidBatch("memory.duplicate")),
            Err(error) => Err(err(error)),
        }
    }
    async fn apply_memory_proposal(
        &self,
        scope: &TenantScope,
        proposal: MemoryProposal,
        limit: usize,
    ) -> Result<bool, StoreError> {
        let statements = apply_proposal_statements(scope, &proposal, limit)?;
        match self.db.batch(statements).await {
            Ok(_) => Ok(true),
            Err(D1Error::Guard(code)) if code == "memory.applied" => Ok(false),
            Err(D1Error::Unique) => Err(StoreError::InvalidBatch("memory.duplicate")),
            Err(error) => Err(err(error)),
        }
    }
    async fn get_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Option<MemoryEntryRecord>, StoreError> {
        self.current(scope, id).await
    }
    async fn memory_revisions(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM memory_revision WHERE memory_id = ? AND org_id = ? \
                     AND project_id = ? ORDER BY revision",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }
    async fn memory_revision(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        revision: u32,
    ) -> Result<Option<MemoryRevisionRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM memory_revision WHERE memory_id = ? AND org_id = ? \
                     AND project_id = ? AND revision = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(i64::from(revision)),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| decode(row, 0))
            .transpose()
    }
    async fn memory_revision_page(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        after: u32,
        limit: usize,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM memory_revision WHERE memory_id = ? AND org_id = ? \
                     AND project_id = ? AND revision > ? ORDER BY revision LIMIT ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(i64::from(after))
                .bind(uint(u64::try_from(limit).unwrap_or(u64::MAX).min(1 << 40)).map_err(err)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }
    async fn patch_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        expected_revision: u32,
        patch: MemoryPatch,
        actor: &str,
    ) -> Result<MemoryEntryRecord, StoreError> {
        let stored = self.current(scope, id).await?.ok_or(StoreError::NotFound)?;
        if stored.revision != expected_revision {
            return Err(StoreError::InvalidBatch("memory.conflict"));
        }
        let (next, action) = next_revision(&stored, patch, actor, self.db.now_ms())?;
        let status = next.status;
        let mut statements = vec![];
        // Accepting or restoring takes an active slot (checked in the
        // batch, as creates are).
        if status == MemoryStatus::Active && stored.status != MemoryStatus::Active {
            statements.push(quota_guard(
                &stored,
                &MemoryStatus::Active,
                uint(runbook_core::memory_entries::MEMORY_ENTRIES_PER_RUNBOOK as u64)
                    .map_err(err)?,
            )?);
        }
        statements.extend([
            guard(
                "memory.conflict",
                "EXISTS (SELECT 1 FROM memory_entry WHERE memory_id = ? AND org_id = ? \
                 AND project_id = ? AND revision = ?)",
            )
            .bind(id.as_str())
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str())
            .bind(i64::from(stored.revision)),
            prepare(
                "UPDATE memory_entry SET status = ?, step_id = ?, tags = ?, revision = ?, \
                 expires_at_ms = ?, updated_at_ms = ?, body = ?, search_tsv = ? \
                 WHERE memory_id = ? AND org_id = ? AND project_id = ?",
            )
            .bind(label(&next.status)?)
            .bind(next.step_id.clone())
            .bind(json(&next.tags).map_err(err)?)
            .bind(i64::from(next.revision))
            .bind(next.expires_at_ms)
            .bind(next.updated_at_ms)
            .bind(json(&next).map_err(err)?)
            .bind(words(&next))
            .bind(id.as_str())
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str()),
            revision_statement(&MemoryRevisionRecord {
                action,
                actor: actor.to_owned(),
                at_ms: next.updated_at_ms,
                entry: next.clone(),
            })?,
        ]);
        if let Some(through) = pruned_through(&next) {
            statements.push(
                prepare(
                    "DELETE FROM memory_revision WHERE memory_id = ? AND org_id = ? \
                     AND project_id = ? AND revision > 1 AND revision <= ? \
                     AND revision != ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(i64::from(through))
                .bind(i64::from(next.person_revision)),
            );
        }
        if let Some(event) = notice_for(&next, action, actor) {
            statements.push(crate::catalog::project_event(scope, &event)?);
        }
        self.db.batch(statements).await.map_err(err)?;
        Ok(next)
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
        let now = self.db.now_ms();
        let mut sql = String::from(
            "SELECT body FROM memory_entry WHERE org_id = ? AND project_id = ? \
             AND (? OR expires_at_ms IS NULL OR expires_at_ms > ?)",
        );
        let mut statement_params: Vec<crate::Value> = vec![
            scope.org_id.as_str().into(),
            scope.project_id.as_str().into(),
            query.include_expired.into(),
            now.into(),
        ];
        if !query.within.is_empty() {
            sql.push_str(&format!(
                " AND scope_key IN ({})",
                placeholders(query.within.len())
            ));
            statement_params.extend(query.within.iter().map(|s| scope_key(s).into()));
        }
        if let Some(status) = &query.status {
            sql.push_str(" AND status = ?");
            statement_params.push(label(status)?.into());
        }
        if let Some(step) = &query.step_id {
            sql.push_str(" AND step_id = ?");
            statement_params.push(step.as_str().into());
        }
        for tag in &query.tags {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM json_each(memory_entry.tags) WHERE value = ?)",
            );
            statement_params.push(tag.as_str().into());
        }
        let text: Vec<String> = query.text.as_deref().map(text_words).unwrap_or_default();
        if !text.is_empty() {
            // FTS5 narrows (tokens of the stored JSON); the stored words
            // decide exactly, as `text_words` does.
            sql.push_str(
                " AND rowid IN (SELECT rowid FROM memory_entry_fts WHERE memory_entry_fts MATCH ?)",
            );
            statement_params.push(
                text.iter()
                    .map(|w| format!("\"{w}\""))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into(),
            );
            for word in &text {
                sql.push_str(" AND instr(' ' || search_tsv || ' ', ' ' || ? || ' ') > 0");
                statement_params.push(word.as_str().into());
            }
        }
        if let Some((at, id)) = &position {
            sql.push_str(" AND (updated_at_ms < ? OR (updated_at_ms = ? AND memory_id < ?))");
            statement_params.push((*at).into());
            statement_params.push((*at).into());
            statement_params.push(id.as_str().into());
        }
        sql.push_str(" ORDER BY updated_at_ms DESC, memory_id DESC LIMIT ?");
        statement_params
            .push(uint(u64::try_from(limit).unwrap_or(u64::MAX).min(1 << 40) + 1).map_err(err)?);
        let mut statement = prepare(sql);
        statement.params = statement_params;
        let rows = self.db.all(statement).await.map_err(err)?;
        let mut items: Vec<MemoryEntryRecord> = rows
            .iter()
            .map(|row| decode(row, 0))
            .collect::<Result<_, _>>()?;
        let more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = more
            .then(|| items.last().map(|last| cursor_for(scope, query, last)))
            .flatten();
        Ok(MemoryRecordPage { items, next_cursor })
    }
    async fn memory_for_step(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        step_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntryRecord>, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT body FROM memory_entry WHERE org_id = ? AND project_id = ? \
                     AND scope_key IN (?, 'project') AND status = 'active' AND step_id = ? \
                     AND (expires_at_ms IS NULL OR expires_at_ms > ?) \
                     ORDER BY coalesce(json_extract(body, '$.last_writer') LIKE 'human:%', 0) DESC, \
                     updated_at_ms DESC, memory_id DESC LIMIT ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(runbook_id.as_str())
                .bind(step_id)
                .bind(self.db.now_ms())
                .bind(uint(u64::try_from(limit).unwrap_or(u64::MAX).min(1 << 40)).map_err(err)?),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| decode(row, 0))
            .collect()
    }
}

impl<D: D1> D1Memory<D> {
    /// Entries of `status` in one quota scope (a runbook, or the project):
    /// pending proposals or, for MEM6 live saves, active entries. The
    /// ExecutionDO checks it before committing a run's memory write, so a
    /// full quota fails the commit as on Postgres; the applier's conditional
    /// insert is the backstop for a race between two runs (it leaves a drop
    /// marker).
    pub async fn entries_counted(
        &self,
        scope: &TenantScope,
        memory_scope: &MemoryScope,
        status: MemoryStatus,
    ) -> Result<u64, StoreError> {
        self.db
            .all(
                prepare(
                    "SELECT count(*) FROM memory_entry WHERE org_id = ? AND project_id = ? \
                     AND scope_key = ? AND status = ?",
                )
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(scope_key(memory_scope))
                .bind(label(&status)?),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| row.unsigned(0).map_err(err))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    /// Org-scoped routing read (never grants access): the project of a
    /// memory entry in this organization.
    pub async fn memory_project(
        &self,
        org_id: &runbook_model::OrganizationId,
        id: &MemoryId,
    ) -> Result<Option<runbook_model::ProjectId>, StoreError> {
        self.db
            .all(
                prepare("SELECT project_id FROM memory_entry WHERE org_id = ? AND memory_id = ?")
                    .bind(org_id.as_str())
                    .bind(id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| row.parse(0).map_err(err))
            .transpose()
    }
}
