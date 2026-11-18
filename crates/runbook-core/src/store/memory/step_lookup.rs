use super::MemoryExecutionStore;
use crate::{StoreError, step_lookup::StepRunLookup};
use runbook_model::{ExecutionId, OrganizationId, ProjectId, StepRunId};
impl StepRunLookup for MemoryExecutionStore {
    async fn step_run_execution(
        &self,
        org_id: &OrganizationId,
        step_run_id: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, StoreError> {
        let db = self.database.borrow();
        Ok(db.owners.iter().find_map(|(id, owner)| {
            if &owner.org_id != org_id {
                return None;
            }
            db.runs
                .get(id)
                .filter(|run| {
                    run.projection
                        .steps
                        .iter()
                        .any(|s| &s.step_run_id == step_run_id)
                })
                .map(|_| (id.clone(), owner.project_id.clone()))
        }))
    }
}
