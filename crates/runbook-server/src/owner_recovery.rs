//! A local command; no public route invokes this recovery operation.
use crate::{Error, config::ServerConfig};
use runbook_model::{OrganizationId, UserId};
use runbook_store_postgres::{
    OwnerAssignment, OwnerRecoveryFault, PostgresDirectory, PostgresStore,
};
pub async fn run(
    config: &ServerConfig,
    org: &OrganizationId,
    user: &UserId,
) -> Result<OwnerAssignment, Error> {
    let mut store = PostgresStore::connect(&config.database)
        .await
        .map_err(|e| Error(e.0))?;
    store.migrate().await.map_err(|e| Error(e.0))?;
    drop(store);
    PostgresDirectory::new(config.database.clone())
        .assign_owner(org, user, OwnerRecoveryFault::None)
        .await
        .map_err(|e| match e {
            runbook_core::directory::DirectoryError::NotFound => {
                Error("owner_recovery.org_not_found")
            }
            runbook_core::directory::DirectoryError::Invalid("owner exists") => {
                Error("owner_recovery.owner_exists")
            }
            runbook_core::directory::DirectoryError::Invalid(
                "owner recovery target not active member",
            ) => Error("owner_recovery.target_not_active_member"),
            _ => Error("owner_recovery.failed"),
        })
}
