//! Pure staged memory operations for the in-memory adapter (MEM1). The
//! adapter applies them to a cloned database inside its transaction and
//! publishes the clone only when every step succeeded.
use super::{
    MemoryAction, MemoryEntryRecord, MemoryPatch, MemoryProposal, MemoryQuery, MemoryRecordPage,
    MemoryRevisionRecord, actor_label, after_cursor, cursor_for, cursor_from, is_expired, matches,
    newest_first,
};
use crate::{StoreError, catalog::TenantScope};
use runbook_model::{ExecutionId, MemoryId, MemoryScope, MemorySource, MemoryStatus, RunbookId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One stored entry: its full revision history (the last is current) and,
/// for an agent proposal, the execution sequence that recorded it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMemory {
    pub revisions: Vec<MemoryRevisionRecord>,
    pub proposal_sequence: Option<u64>,
}
impl StoredMemory {
    pub fn current(&self) -> &MemoryEntryRecord {
        &self
            .revisions
            .last()
            .expect("an entry has a revision")
            .entry
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryData {
    entries: BTreeMap<MemoryId, StoredMemory>,
}

/// Entries of `status` in the quota scope of `entry` (a runbook, or the
/// project for project entries). Active entries and proposals have
/// separate quotas; archived history counts toward neither.
fn counted(
    entries: &BTreeMap<MemoryId, StoredMemory>,
    entry: &MemoryEntryRecord,
    status: MemoryStatus,
) -> usize {
    entries
        .values()
        .map(StoredMemory::current)
        .filter(|e| {
            e.scope == entry.scope && e.memory_scope == entry.memory_scope && e.status == status
        })
        .count()
}

/// The execution a proposal came from.
fn source_execution(entry: &MemoryEntryRecord) -> Option<&ExecutionId> {
    match &entry.source {
        MemorySource::StepRun { execution_id, .. } => Some(execution_id),
        _ => None,
    }
}

impl MemoryData {
    fn visible(&self, scope: &TenantScope, id: &MemoryId) -> Option<&StoredMemory> {
        self.entries
            .get(id)
            .filter(|stored| &stored.current().scope == scope)
    }
    fn insert(
        &mut self,
        mut entry: MemoryEntryRecord,
        action: MemoryAction,
        actor: &str,
        limit: usize,
        proposal_sequence: Option<u64>,
    ) -> Result<MemoryEntryRecord, StoreError> {
        if self.entries.contains_key(&entry.id) {
            return Err(StoreError::InvalidBatch("memory.duplicate"));
        }
        if counted(&self.entries, &entry, entry.status) >= limit {
            return Err(StoreError::InvalidBatch("memory.limit_reached"));
        }
        entry.revision = 1;
        entry.last_writer = Some(actor.to_owned());
        self.entries.insert(
            entry.id.clone(),
            StoredMemory {
                revisions: vec![MemoryRevisionRecord {
                    action,
                    actor: actor.to_owned(),
                    at_ms: entry.updated_at_ms,
                    entry: entry.clone(),
                }],
                proposal_sequence,
            },
        );
        Ok(entry)
    }
    pub(crate) fn create(
        &mut self,
        scope: &TenantScope,
        mut entry: MemoryEntryRecord,
        actor: &str,
        limit: usize,
        now: i64,
    ) -> Result<MemoryEntryRecord, StoreError> {
        let action = match entry.status {
            MemoryStatus::Active => MemoryAction::Create,
            MemoryStatus::Proposed if source_execution(&entry).is_none() => MemoryAction::Propose,
            _ => return Err(StoreError::InvalidBatch("memory.invalid_entry")),
        };
        if &entry.scope != scope {
            return Err(StoreError::InvalidBatch("memory.invalid_entry"));
        }
        entry.created_at_ms = now;
        entry.updated_at_ms = now;
        self.insert(entry, action, actor, limit, None)
    }
    /// D40: true when applied now; false for a replay or an older sequence
    /// of the same execution.
    pub(crate) fn apply_proposal(
        &mut self,
        scope: &TenantScope,
        proposal: MemoryProposal,
        limit: usize,
    ) -> Result<bool, StoreError> {
        let entry = &proposal.entry;
        if &entry.scope != scope
            || entry.status == MemoryStatus::Archived
            || source_execution(entry) != Some(&proposal.execution_id)
        {
            return Err(StoreError::InvalidBatch("memory.invalid_entry"));
        }
        let applied = self.entries.values().any(|stored| {
            source_execution(stored.current()) == Some(&proposal.execution_id)
                && stored
                    .proposal_sequence
                    .is_some_and(|s| s >= proposal.sequence)
        });
        if applied {
            return Ok(false);
        }
        let actor = match &entry.source {
            MemorySource::StepRun { actor, .. } => actor_label(actor),
            _ => return Err(StoreError::InvalidBatch("memory.invalid_entry")),
        };
        let action = super::run_action(&proposal.entry);
        self.insert(
            proposal.entry,
            action,
            &actor,
            limit,
            Some(proposal.sequence),
        )?;
        Ok(true)
    }
    pub(crate) fn get(&self, scope: &TenantScope, id: &MemoryId) -> Option<MemoryEntryRecord> {
        self.visible(scope, id).map(|s| s.current().clone())
    }
    pub(crate) fn revisions(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Vec<MemoryRevisionRecord> {
        self.visible(scope, id)
            .map(|s| s.revisions.clone())
            .unwrap_or_default()
    }
    pub(crate) fn patch(
        &mut self,
        scope: &TenantScope,
        id: &MemoryId,
        expected_revision: u32,
        patch: MemoryPatch,
        actor: &str,
        now: i64,
    ) -> Result<(MemoryEntryRecord, MemoryAction), StoreError> {
        let stored = self.visible(scope, id).ok_or(StoreError::NotFound)?;
        let current = stored.current();
        if current.revision != expected_revision {
            return Err(StoreError::InvalidBatch("memory.conflict"));
        }
        let (next, action) = super::next_revision(current, patch, actor, now)?;
        if next.status == MemoryStatus::Active
            && current.status != MemoryStatus::Active
            && counted(&self.entries, current, MemoryStatus::Active)
                >= super::MEMORY_ENTRIES_PER_RUNBOOK
        {
            return Err(StoreError::InvalidBatch("memory.limit_reached"));
        }
        let stored = self.entries.get_mut(id).expect("visible entry exists");
        stored.revisions.push(MemoryRevisionRecord {
            action,
            actor: actor.to_owned(),
            at_ms: now,
            entry: next.clone(),
        });
        if let Some(through) = super::pruned_through(&next) {
            stored.revisions.retain(|r| {
                let n = r.entry.revision;
                n == 1 || n > through || n == next.person_revision
            });
        }
        Ok((next, action))
    }
    pub(crate) fn revision(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        revision: u32,
    ) -> Option<MemoryRevisionRecord> {
        self.visible(scope, id)?
            .revisions
            .iter()
            .find(|r| r.entry.revision == revision)
            .cloned()
    }
    pub(crate) fn revision_page(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        after: u32,
        limit: usize,
    ) -> Vec<MemoryRevisionRecord> {
        self.visible(scope, id)
            .map(|s| {
                s.revisions
                    .iter()
                    .filter(|r| r.entry.revision > after)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(crate) fn search(
        &self,
        scope: &TenantScope,
        query: &MemoryQuery,
        cursor: Option<String>,
        limit: usize,
        now: i64,
    ) -> Result<MemoryRecordPage, StoreError> {
        let position = cursor
            .as_deref()
            .map(|c| cursor_from(scope, query, c))
            .transpose()?;
        let mut items: Vec<MemoryEntryRecord> = self
            .entries
            .values()
            .map(StoredMemory::current)
            .filter(|e| &e.scope == scope && matches(e, query, now))
            .filter(|e| position.as_ref().is_none_or(|p| after_cursor(e, p)))
            .cloned()
            .collect();
        items.sort_by(newest_first);
        let more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = more
            .then(|| items.last().map(|last| cursor_for(scope, query, last)))
            .flatten();
        Ok(MemoryRecordPage { items, next_cursor })
    }
    pub(crate) fn for_step(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        step_id: &str,
        limit: usize,
        now: i64,
    ) -> Vec<MemoryEntryRecord> {
        let within = [
            MemoryScope::Runbook {
                runbook_id: runbook_id.clone(),
            },
            MemoryScope::Project,
        ];
        let mut items: Vec<_> = self
            .entries
            .values()
            .map(StoredMemory::current)
            .filter(|e| {
                &e.scope == scope
                    && within.contains(&e.memory_scope)
                    && e.status == MemoryStatus::Active
                    && e.step_id.as_deref() == Some(step_id)
                    && !is_expired(e, now)
            })
            .cloned()
            .collect();
        items.sort_by(super::step_order);
        items.truncate(limit);
        items
    }
}

impl MemoryData {
    /// Every entry of a tenant with its history (portable export).
    pub(crate) fn export(&self, scope: &TenantScope) -> Vec<crate::portable::MemoryRow> {
        self.entries
            .values()
            .filter(|s| &s.current().scope == scope)
            .map(|s| crate::portable::MemoryRow {
                revisions: s.revisions.clone(),
                proposal_sequence: s.proposal_sequence,
            })
            .collect()
    }
    /// Whether a tenant holds any entry (import requires an empty target).
    pub(crate) fn holds(&self, scope: &TenantScope) -> bool {
        self.entries.values().any(|s| &s.current().scope == scope)
    }
    /// Install validated archive rows; an ID taken anywhere is a conflict.
    pub(crate) fn import(&mut self, rows: &[crate::portable::MemoryRow]) -> Result<(), StoreError> {
        for row in rows {
            let id = &row
                .current()
                .ok_or(StoreError::InvalidBatch("archive.invalid_memory"))?
                .id;
            if self.entries.contains_key(id) {
                return Err(StoreError::InvalidBatch("import.id_conflict"));
            }
            self.entries.insert(
                id.clone(),
                StoredMemory {
                    revisions: row.revisions.clone(),
                    proposal_sequence: row.proposal_sequence,
                },
            );
        }
        Ok(())
    }
}
