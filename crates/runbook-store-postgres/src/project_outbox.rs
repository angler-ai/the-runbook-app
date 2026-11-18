//! C1 project-level webhook outbox on PostgreSQL: events with no execution
//! (`runbook.published`) are inserted in the transaction of the fact they
//! report and removed once the drain has fanned them out.
use crate::{
    PostgresExecutionStore,
    execution::READ_COMMITTED,
    rows::{db, decode, encode, int},
};
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    webhooks::{ProjectEvent, ProjectOutbox},
};
use tokio_postgres::Client;

pub(crate) async fn insert(
    c: &Client,
    scope: &TenantScope,
    event: &ProjectEvent,
) -> Result<(), StoreError> {
    c.execute(
        "INSERT INTO runbook.project_event_outbox \
         (org_id, project_id, event_id, created_at_ms, body) VALUES ($1, $2, $3, $4, $5)",
        &[
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &event.event_id,
            &event.at_ms,
            &encode(event)?,
        ],
    )
    .await
    .map_err(db)?;
    Ok(())
}

impl ProjectOutbox for PostgresExecutionStore {
    async fn pending_project_events(&self, limit: usize) -> Result<Vec<ProjectEvent>, StoreError> {
        let limit = int(limit as u64)?;
        self.transaction(READ_COMMITTED, async |c| {
            c.query(
                "SELECT body FROM runbook.project_event_outbox \
                 WHERE org_id = $1 AND project_id = $2 \
                 ORDER BY created_at_ms, event_id LIMIT $3",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
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
    async fn ack_project_event(&self, event_id: &str) -> Result<(), StoreError> {
        self.transaction(READ_COMMITTED, async |c| {
            c.execute(
                "DELETE FROM runbook.project_event_outbox \
                 WHERE org_id = $1 AND project_id = $2 AND event_id = $3",
                &[
                    &self.scope.org_id.as_str(),
                    &self.scope.project_id.as_str(),
                    &event_id,
                ],
            )
            .await
            .map_err(db)?;
            Ok(())
        })
        .await
    }
}
