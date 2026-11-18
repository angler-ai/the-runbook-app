//! STO1: Orphan blob sweeper.
use crate::{layout::parse_object_path_raw, storage::ObjectStorage};
use futures::StreamExt;
use object_store::{ObjectStoreExt, path::Path};
use std::time::{Duration, SystemTime};
use tokio_postgres::GenericClient;

/// Advisory lock key for the blob sweeper: 0x72625f7377656570 ("rb_sweep").
pub const SWEEPER_LOCK_KEY: i64 = 0x72625f7377656570;

/// Try to acquire the advisory lock for the sweeper.
pub async fn try_acquire_sweeper_lock(
    client: &impl GenericClient,
) -> Result<bool, tokio_postgres::Error> {
    let row = client
        .query_one("SELECT pg_try_advisory_lock($1)", &[&SWEEPER_LOCK_KEY])
        .await?;
    Ok(row.get(0))
}

pub async fn release_sweeper_lock(
    client: &impl GenericClient,
) -> Result<(), tokio_postgres::Error> {
    client
        .execute("SELECT pg_advisory_unlock($1)", &[&SWEEPER_LOCK_KEY])
        .await?;
    Ok(())
}

/// Sweep orphan objects older than `min_age` from the object store.
/// Returns the number of orphaned objects deleted.
pub async fn sweep_orphans(
    client: &impl GenericClient,
    storage: &ObjectStorage,
    min_age: Duration,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    sweep_orphans_with_hook(client, storage, min_age, |_| async {}).await
}

/// Sweep orphan objects older than `min_age` from the object store, with a hook
/// called before the pre-delete checks (for race condition tests).
pub async fn sweep_orphans_with_hook<F, Fut>(
    client: &impl GenericClient,
    storage: &ObjectStorage,
    min_age: Duration,
    mut before_delete_hook: F,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>>
where
    F: FnMut(&Path) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let now = SystemTime::now();
    let prefix = crate::layout::normalize_prefix(&storage.prefix);
    let list_prefix = if prefix.is_empty() {
        None
    } else {
        Some(Path::from(prefix.trim_end_matches('/')))
    };

    let mut stream = storage.store.list(list_prefix.as_ref());
    let mut swept = 0;

    while let Some(item) = stream.next().await {
        let meta = item?;
        // If newer than cutoff, skip (might be in-flight write)
        if now
            .duration_since(meta.last_modified.into())
            .is_ok_and(|elapsed| elapsed < min_age)
        {
            continue;
        }

        // Parse key
        if let Some((scope, raw_key, sha256)) =
            parse_object_path_raw(&storage.prefix, &meta.location)
        {
            let row = client
                .query_opt(
                    "SELECT 1 FROM runbook.blob \
                     WHERE org_id = $1 AND project_id = $2 AND key = $3 \
                       AND location = 'object' AND sha256 = $4",
                    &[
                        &scope.org_id.as_str(),
                        &scope.project_id.as_str(),
                        &raw_key,
                        &&sha256[..],
                    ],
                )
                .await?;

            if row.is_none() {
                before_delete_hook(&meta.location).await;

                // Sweeper race guard: between SELECT 1 returning nothing and delete,
                // an identical re-put can rewrite the same object and commit its row.
                // Re-check head: if last_modified is now younger than min_age, skip it.
                let head_meta = match storage.store.head(&meta.location).await {
                    Ok(m) => m,
                    Err(object_store::Error::NotFound { .. }) => continue,
                    Err(e) => return Err(Box::new(e)),
                };
                let current_now = SystemTime::now();
                if current_now
                    .duration_since(head_meta.last_modified.into())
                    .is_ok_and(|elapsed| elapsed < min_age)
                {
                    continue;
                }

                // Re-check the row once more right before the delete.
                let row2 = client
                    .query_opt(
                        "SELECT 1 FROM runbook.blob \
                         WHERE org_id = $1 AND project_id = $2 AND key = $3 \
                           AND location = 'object' AND sha256 = $4",
                        &[
                            &scope.org_id.as_str(),
                            &scope.project_id.as_str(),
                            &raw_key,
                            &&sha256[..],
                        ],
                    )
                    .await?;

                if row2.is_none() {
                    // Orphan! Delete from object store.
                    if storage.store.delete(&meta.location).await.is_ok() {
                        swept += 1;
                    }
                }
            }
        }
    }

    Ok(swept)
}
