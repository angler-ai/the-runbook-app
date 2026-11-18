//! Indexed replay access under an execution head lock; never loaded in snapshots.
use crate::rows::{db, decode, encode};
use runbook_core::{IdempotencyRecord, StoreError, actor::replay_record_parts};
use runbook_model::ExecutionId;
use tokio_postgres::Client;

pub(crate) async fn lookup(
    c: &Client,
    id: &ExecutionId,
    prefix: &str,
    now: i64,
) -> Result<Option<IdempotencyRecord>, StoreError> {
    c.query_opt("SELECT body FROM runbook.actor_replay WHERE execution_id=$1 AND prefix=$2 AND at_ms>$3 ORDER BY at_ms DESC LIMIT 1",
        &[&id.as_str(), &prefix, &now.saturating_sub(86_400_000)]).await.map_err(db)?
        .map(|row|decode(row.get(0))).transpose()
}
pub(crate) async fn write(
    c: &Client,
    record: &IdempotencyRecord,
    now: i64,
) -> Result<(), StoreError> {
    let (prefix, at) =
        replay_record_parts(record)?.ok_or(StoreError::InvalidBatch("replay.invalid_record"))?;
    if at != now {
        return Err(StoreError::InvalidBatch("store.transaction_time_mismatch"));
    }
    let id = record.execution_id.as_str();
    c.execute(
        "DELETE FROM runbook.actor_replay WHERE execution_id=$1 AND at_ms <= $2",
        &[&id, &now.saturating_sub(86_400_000)],
    )
    .await
    .map_err(db)?;
    c.execute(
        "INSERT INTO runbook.actor_replay (execution_id,prefix,at_ms,body) VALUES ($1,$2,$3,$4)",
        &[&id, &prefix, &at, &encode(record)?],
    )
    .await
    .map_err(db)?;
    Ok(())
}
pub(crate) async fn export(
    c: &Client,
    id: &ExecutionId,
) -> Result<Vec<IdempotencyRecord>, StoreError> {
    c.query(
        "SELECT body FROM runbook.actor_replay WHERE execution_id=$1 ORDER BY prefix,at_ms",
        &[&id.as_str()],
    )
    .await
    .map_err(db)?
    .into_iter()
    .map(|row| decode(row.get(0)))
    .collect()
}
