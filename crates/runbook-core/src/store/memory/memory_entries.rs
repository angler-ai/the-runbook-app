//! MEM1: `MemoryStore` on the in-memory adapter. Each call is one staged
//! transaction on the shared database, stamped with the store clock.
use super::MemoryExecutionStore;
use crate::{
    StoreError,
    catalog::{DerivedUpdate, TenantScope},
    memory_entries::{
        MemoryEntryRecord, MemoryPatch, MemoryProposal, MemoryQuery, MemoryRecordPage,
        MemoryRevisionRecord, MemoryStore, data::MemoryData, notice_for, run_quota,
    },
};
use runbook_model::{MemoryId, RunbookId};

/// D40: apply a derived row's memory writes (proposals and MEM6 live saves)
/// inside the execution transaction, each idempotent by (execution,
/// sequence) and under the quota of its status.
pub(super) fn apply_derived_memory(
    memory: &mut MemoryData,
    scope: &TenantScope,
    update: &DerivedUpdate,
) -> Result<(), StoreError> {
    for proposal in &update.memory {
        let limit = run_quota(proposal.entry.status);
        memory.apply_proposal(scope, proposal.clone(), limit)?;
    }
    Ok(())
}

impl MemoryStore for MemoryExecutionStore {
    async fn memory_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.clock.get())
    }
    async fn create_memory(
        &self,
        scope: &TenantScope,
        entry: MemoryEntryRecord,
        actor: &str,
        limit: usize,
    ) -> Result<MemoryEntryRecord, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let created = db.memory.create(scope, entry, actor, limit, now)?;
            let action = match created.status {
                runbook_model::MemoryStatus::Active => crate::memory_entries::MemoryAction::Create,
                _ => crate::memory_entries::MemoryAction::Propose,
            };
            if let Some(event) = notice_for(&created, action, actor) {
                db.project_outbox
                    .insert((scope.clone(), event.event_id.clone()), event);
            }
            Ok(created)
        })
    }
    async fn apply_memory_proposal(
        &self,
        scope: &TenantScope,
        proposal: MemoryProposal,
        limit: usize,
    ) -> Result<bool, StoreError> {
        self.transaction(|db| db.memory.apply_proposal(scope, proposal, limit))
    }
    async fn get_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Option<MemoryEntryRecord>, StoreError> {
        Ok(self.database.borrow().memory.get(scope, id))
    }
    async fn memory_revisions(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        Ok(self.database.borrow().memory.revisions(scope, id))
    }
    async fn patch_memory(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        expected_revision: u32,
        patch: MemoryPatch,
        actor: &str,
    ) -> Result<MemoryEntryRecord, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let (next, action) =
                db.memory
                    .patch(scope, id, expected_revision, patch, actor, now)?;
            if let Some(event) = notice_for(&next, action, actor) {
                db.project_outbox
                    .insert((scope.clone(), event.event_id.clone()), event);
            }
            Ok(next)
        })
    }
    async fn memory_revision(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        revision: u32,
    ) -> Result<Option<MemoryRevisionRecord>, StoreError> {
        Ok(self.database.borrow().memory.revision(scope, id, revision))
    }
    async fn memory_revision_page(
        &self,
        scope: &TenantScope,
        id: &MemoryId,
        after: u32,
        limit: usize,
    ) -> Result<Vec<MemoryRevisionRecord>, StoreError> {
        Ok(self
            .database
            .borrow()
            .memory
            .revision_page(scope, id, after, limit))
    }
    async fn search_memory(
        &self,
        scope: &TenantScope,
        query: &MemoryQuery,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<MemoryRecordPage, StoreError> {
        self.database
            .borrow()
            .memory
            .search(scope, query, cursor, limit, self.clock.get())
    }
    async fn memory_for_step(
        &self,
        scope: &TenantScope,
        runbook_id: &RunbookId,
        step_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntryRecord>, StoreError> {
        Ok(self.database.borrow().memory.for_step(
            scope,
            runbook_id,
            step_id,
            limit,
            self.clock.get(),
        ))
    }
}
