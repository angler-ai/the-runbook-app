use crate::{
    PostgresExecutionStore,
    rows::{connection, db},
};
use runbook_core::{StoreError, step_lookup::StepRunLookup};
use runbook_model::{ExecutionId, OrganizationId, ProjectId, StepRunId};
impl StepRunLookup for PostgresExecutionStore {
    async fn step_run_execution(
        &self,
        org_id: &OrganizationId,
        step_run_id: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT execution_id, project_id FROM runbook.step_run_index WHERE org_id=$1 AND step_run_id=$2",
                &[&org_id.as_str(), &step_run_id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| {
                Ok((
                    row.get::<_, &str>(0)
                        .parse()
                        .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
                    row.get::<_, &str>(1)
                        .parse()
                        .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
                ))
            })
            .transpose()
    }
}

/// New IDs are immutable and globally unique; ownership never moves on conflict.
pub(crate) async fn insert(
    c: &tokio_postgres::Client,
    scope: &runbook_core::catalog::TenantScope,
    execution: &ExecutionId,
    step: &StepRunId,
) -> Result<(), StoreError> {
    c.execute("INSERT INTO runbook.step_run_index (step_run_id,execution_id,org_id,project_id) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING", &[&step.as_str(),&execution.as_str(),&scope.org_id.as_str(),&scope.project_id.as_str()]).await.map_err(db)?;
    let row=c.query_one("SELECT execution_id, org_id, project_id FROM runbook.step_run_index WHERE step_run_id=$1", &[&step.as_str()]).await.map_err(db)?;
    if row.get::<_, &str>(0) != execution.as_str()
        || row.get::<_, &str>(1) != scope.org_id.as_str()
        || row.get::<_, &str>(2) != scope.project_id.as_str()
    {
        return Err(StoreError::InvalidBatch("store.step_run_id_conflict"));
    }
    Ok(())
}
