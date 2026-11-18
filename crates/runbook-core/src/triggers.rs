//! LANG9b: the triggers of each runbook's latest published version. A
//! store lists those versions in one query ([`PublishedVersion`]); the
//! triggers come from the version's snapshot, cached by its digest, so a
//! scheduler pass that finds nothing new reads no snapshots.
use crate::{StoreError, blob::BlobKey, blob::BlobStore, catalog::TenantScope};
use runbook_model::{RunbookId, VersionId};
use runbook_spec::ir::Triggers;
use std::{cell::RefCell, collections::BTreeMap, num::NonZeroU32, rc::Rc};

/// One runbook's latest published version (archived runbooks left out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedVersion {
    pub scope: TenantScope,
    pub runbook_id: RunbookId,
    pub version_id: VersionId,
    pub number: NonZeroU32,
    pub snapshot_digest: [u8; 32],
}

/// Versions never change, so their triggers are cached by snapshot digest.
const CACHE_LIMIT: usize = 1024;
thread_local! {
    static CACHE: RefCell<BTreeMap<[u8; 32], Rc<Triggers>>> = const { RefCell::new(BTreeMap::new()) };
}

/// The triggers of the version whose snapshot is `digest`.
pub async fn triggers_of(
    blobs: &impl BlobStore,
    digest: [u8; 32],
) -> Result<Rc<Triggers>, StoreError> {
    if let Some(found) = CACHE.with(|cache| cache.borrow().get(&digest).cloned()) {
        return Ok(found);
    }
    let bytes = blobs.get(&BlobKey::Shared { hash: digest }).await?;
    let compiled = runbook_spec::restore(&bytes)
        .map_err(|_| StoreError::Backend("trigger.snapshot_invalid"))?;
    let triggers = Rc::new(compiled.ir().triggers.clone());
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(digest, triggers.clone());
    });
    Ok(triggers)
}

/// The latest published version of `runbook_id` and its triggers, if it has
/// one.
pub async fn published_triggers(
    catalog: &(impl crate::catalog::CatalogStore + BlobStore),
    scope: &TenantScope,
    runbook_id: &RunbookId,
) -> Result<Option<(PublishedVersion, Rc<Triggers>)>, StoreError> {
    let resolved = match catalog.resolve_version(runbook_id, None).await {
        Ok(resolved) => resolved,
        Err(StoreError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(record) = catalog
        .version_record(runbook_id, &resolved.runbook_version_id)
        .await?
    else {
        return Ok(None);
    };
    let Some(number) = record.number else {
        return Ok(None);
    };
    let triggers = triggers_of(catalog, record.snapshot_digest).await?;
    Ok(Some((
        PublishedVersion {
            scope: scope.clone(),
            runbook_id: runbook_id.clone(),
            version_id: record.version_id,
            number,
            snapshot_digest: record.snapshot_digest,
        },
        triggers,
    )))
}

/// The idempotency key of a trigger's start: one run per (runbook,
/// trigger, occasion), whoever or whatever delivers it.
pub fn start_key(kind: &str, runbook_id: &RunbookId, trigger: &str, occasion: &str) -> String {
    use sha2::{Digest, Sha256};
    let framed = serde_json::to_vec(&serde_json::json!([
        "trigger.start",
        kind,
        runbook_id.as_str(),
        trigger,
        occasion
    ]))
    .unwrap_or_default();
    crate::authoring::hex(&Sha256::digest(framed))
}
