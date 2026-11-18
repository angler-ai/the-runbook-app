//! ARC3: the Postgres side of the daily archive routes
//! (`runbook_api::archives`).
use crate::config::RetentionConfig;
use runbook_api::archives::ArchiveBackend;
use runbook_core::{StoreError, archives::job::RetentionPolicy, catalog::TenantScope};
use runbook_model::{ExecutionId, OrganizationId, ProjectId, RunbookId};
use runbook_store_postgres::{DatabaseConfig, PostgresExecutionStore};
use std::sync::Arc;

#[derive(Clone)]
pub struct PostgresArchiveBackend {
    pub config: DatabaseConfig,
    pub retention: RetentionConfig,
    pub objects: Option<Arc<runbook_store_objects::ObjectStorage>>,
}
impl PostgresArchiveBackend {
    /// A handle for org-scoped routing reads; its project is never used.
    fn lookup(&self, org: &OrganizationId) -> Result<PostgresExecutionStore, StoreError> {
        Ok(PostgresExecutionStore::new(
            self.config.clone(),
            TenantScope {
                org_id: org.clone(),
                project_id: "prj_00000000000000000000000000"
                    .parse()
                    .map_err(|_| StoreError::Backend("server.invalid_id"))?,
            },
        )
        .with_objects(self.objects.clone()))
    }
}
impl ArchiveBackend for PostgresArchiveBackend {
    type Store = PostgresExecutionStore;
    async fn runbook_project(
        &self,
        org: &OrganizationId,
        runbook_id: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.lookup(org)?.runbook_project(org, runbook_id).await
    }
    async fn archived_run_project(
        &self,
        org: &OrganizationId,
        execution_id: &ExecutionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.lookup(org)?
            .archived_run_project(org, execution_id)
            .await
    }
    fn store(&self, scope: &TenantScope) -> PostgresExecutionStore {
        PostgresExecutionStore::new(self.config.clone(), scope.clone())
            .with_objects(self.objects.clone())
    }
    fn policy(&self, project: &ProjectId) -> RetentionPolicy {
        self.retention.policy_for(project)
    }
}
