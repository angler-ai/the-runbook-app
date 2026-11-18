use super::*;
use crate::actor::replay_record_parts;

pub(super) fn lookup(
    db: &Database,
    id: &ExecutionId,
    prefix: &str,
    now: i64,
) -> Option<IdempotencyRecord> {
    db.actor_replays
        .range(
            (id.clone(), prefix.to_owned(), i64::MIN)..=(id.clone(), prefix.to_owned(), i64::MAX),
        )
        .rev()
        .find(|((_, _, at), _)| *at > now.saturating_sub(86_400_000))
        .map(|(_, r)| r.clone())
}
pub(super) fn write(db: &mut Database, row: IdempotencyRecord, now: i64) -> Result<(), StoreError> {
    let (prefix, at) =
        replay_record_parts(&row)?.ok_or(StoreError::InvalidBatch("replay.invalid_record"))?;
    if at != now {
        return Err(StoreError::InvalidBatch("store.transaction_time_mismatch"));
    }
    db.actor_replays.retain(|(id, _, time), _| {
        *id != row.execution_id || *time > now.saturating_sub(86_400_000)
    });
    let key = (row.execution_id.clone(), prefix.to_owned(), at);
    if db.actor_replays.insert(key, row).is_some() {
        return Err(StoreError::InvalidBatch("store.idempotency_conflict"));
    }
    Ok(())
}
