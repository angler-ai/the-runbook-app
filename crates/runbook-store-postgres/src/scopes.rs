//! B4.4: which tenants have runs, so a drain worker can claim each tenant's
//! due work through its own tenant-bound store. Work queues stay scoped; this
//! read only enumerates scopes and never returns run data.
use crate::{
    DatabaseConfig,
    rows::{connection, db},
};
use runbook_core::{StoreError, catalog::TenantScope};

/// Every (organization, project) that owns at least one execution, has a
/// pending webhook delivery (a test delivery needs no execution), a pending
/// project event (`runbook.published`) or a webhook subscription (its
/// delivery log is purged after 90 days).
pub async fn tenant_scopes(config: &DatabaseConfig) -> Result<Vec<TenantScope>, StoreError> {
    let store = crate::pool::Pool::new(config.clone())
        .get()
        .await
        .map_err(connection)?;
    let rows = store
        .client()
        .query(
            "SELECT org_id, project_id FROM runbook.execution_owner \
             UNION SELECT org_id, project_id FROM runbook.webhook_delivery \
             WHERE state = 'pending' \
             UNION SELECT org_id, project_id FROM runbook.project_event_outbox \
             UNION SELECT org_id, project_id FROM runbook.webhook_subscription \
             ORDER BY org_id, project_id",
            &[],
        )
        .await
        .map_err(db)?;
    rows.iter()
        .map(|row| {
            let org: &str = row.get(0);
            let project: &str = row.get(1);
            Ok(TenantScope {
                org_id: org
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
                project_id: project
                    .parse()
                    .map_err(|_| StoreError::Backend("postgres.invalid_stored_id"))?,
            })
        })
        .collect()
}

/// UP2: the tenant that owns one execution, by its primary key, for the
/// user-content listener: a signed URL's tenant must own its run. None when
/// no such run exists. `Pool::new` is a handle to the process-wide pool of
/// this config (F32), so no connection is opened per call.
pub async fn execution_owner(
    config: &DatabaseConfig,
    execution_id: &runbook_model::ExecutionId,
) -> Result<Option<TenantScope>, StoreError> {
    let store = crate::pool::Pool::new(config.clone())
        .get()
        .await
        .map_err(connection)?;
    crate::catalog::owner(store.client(), execution_id).await
}

/// LANG9b: every non-archived runbook's latest published version, for the
/// schedule pass. One query; the triggers come from the snapshots, cached.
pub async fn published_versions(
    config: &DatabaseConfig,
) -> Result<Vec<runbook_core::triggers::PublishedVersion>, StoreError> {
    let store = crate::pool::Pool::new(config.clone())
        .get()
        .await
        .map_err(connection)?;
    let rows = store
        .client()
        .query(
            "SELECT v.org_id, v.project_id, v.runbook_id, v.version_id, v.number, \
                    v.snapshot_digest \
             FROM runbook.runbook_version v \
             JOIN runbook.runbook r ON r.runbook_id = v.runbook_id \
             WHERE NOT r.archived AND v.number = ( \
                 SELECT max(w.number) FROM runbook.runbook_version w \
                 WHERE w.runbook_id = v.runbook_id) \
             ORDER BY v.org_id, v.project_id, v.runbook_id",
            &[],
        )
        .await
        .map_err(db)?;
    let invalid = || StoreError::Backend("postgres.invalid_stored_id");
    rows.iter()
        .map(|row| {
            let number: i32 = row.get(4);
            let digest: Vec<u8> = row.get(5);
            Ok(runbook_core::triggers::PublishedVersion {
                scope: TenantScope {
                    org_id: row.get::<_, &str>(0).parse().map_err(|_| invalid())?,
                    project_id: row.get::<_, &str>(1).parse().map_err(|_| invalid())?,
                },
                runbook_id: row.get::<_, &str>(2).parse().map_err(|_| invalid())?,
                version_id: row.get::<_, &str>(3).parse().map_err(|_| invalid())?,
                number: u32::try_from(number)
                    .ok()
                    .and_then(std::num::NonZeroU32::new)
                    .ok_or_else(invalid)?,
                snapshot_digest: digest
                    .try_into()
                    .map_err(|_| StoreError::Backend("postgres.invalid_hash"))?,
            })
        })
        .collect()
}
