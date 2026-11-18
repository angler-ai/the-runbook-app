//! CF4's `BlobOwners` seam on D1 (CF3): who owns what a blob key names.
//! An execution's (and its uploads') tenant is the run's owner row, an
//! archive's is its runbook's, and a run retention deleted keeps its
//! uploads readable while its daily file lists it (`archive_run`).
use runbook_core::{StoreError, catalog::TenantScope};
use runbook_model::{ExecutionId, RunbookId};
use runbook_store_d1::{
    D1,
    sql::{Columns, prepare},
};
use runbook_store_r2::BlobOwners;

#[derive(Clone)]
pub struct D1Owners<D: D1> {
    db: D,
}
impl<D: D1> D1Owners<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }
}

fn err(_: runbook_store_d1::D1Error) -> StoreError {
    StoreError::Backend("d1.blob_owner")
}

impl<D: D1> BlobOwners for D1Owners<D> {
    async fn execution_owner(&self, id: &ExecutionId) -> Result<Option<TenantScope>, StoreError> {
        runbook_store_d1::catalog::execution_owner(&self.db, id).await
    }
    async fn runbook_owner(&self, id: &RunbookId) -> Result<Option<TenantScope>, StoreError> {
        self.db
            .all(
                prepare("SELECT org_id, project_id FROM runbook WHERE runbook_id = ?")
                    .bind(id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok(TenantScope {
                    org_id: row.parse(0).map_err(err)?,
                    project_id: row.parse(1).map_err(err)?,
                })
            })
            .transpose()
    }
    async fn retained(&self, scope: &TenantScope, id: &ExecutionId) -> Result<bool, StoreError> {
        Ok(!self
            .db
            .all(
                prepare(
                    "SELECT 1 FROM tombstone t JOIN archive_run r ON r.execution_id = t.execution_id \
                     WHERE t.execution_id = ? AND t.org_id = ? AND t.project_id = ?",
                )
                .bind(id.as_str())
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?
            .is_empty())
    }
}
