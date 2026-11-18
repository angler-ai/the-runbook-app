//! ARC3: download bundles. One day's file is served as stored; a `from`/`to`
//! range, or any download with evidence, is a ZIP of:
//!
//! ```text
//! bundle.json                      BundleManifest
//! <YYYY-MM-DD>.zip                 each day's file, as stored
//! evidence/<exe>/<evd_id>          evidence objects still held (D53 keeps
//!                                  them outside the day files)
//! ```
//!
//! Evidence follows the archive (PRD §14): retention's live deletion keeps
//! the objects, file expiry and single-run deletion remove them; an object
//! no longer held is listed with `file: null`. A stale day is refused
//! (`archive.rewrite_pending`) until the job rewrites it.
use super::{
    ArchiveIndex, ArchiveRecord, date_of,
    read::{manifest, record_key},
    zip,
};
use crate::{
    StoreError,
    blob::{BlobKey, BlobStore},
    catalog::TenantScope,
    portable,
};
use runbook_model::{EvidenceId, ExecutionId, RunbookId};
use serde::{Deserialize, Serialize};

pub const BUNDLE_FORMAT: &str = "runbook-archive-bundle";
pub const BUNDLE_MANIFEST: &str = "bundle.json";
/// Most days one range download may cover.
pub const MAX_BUNDLE_DAYS: i64 = 92;
/// Most bytes one bundle may hold (files plus evidence).
pub const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub format: String,
    pub runbook_id: RunbookId,
    pub from: String,
    pub to: String,
    pub days: Vec<BundleDay>,
    pub include_evidence: bool,
    pub evidence: Vec<BundleEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleDay {
    pub date: String,
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub run_count: u32,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleEvidence {
    pub execution_id: ExecutionId,
    pub evd_id: EvidenceId,
    pub name: String,
    pub content_type: String,
    pub size: u64,
    /// Where the object is in the bundle; null when it is no longer held.
    pub file: Option<String>,
}

fn too_large() -> StoreError {
    StoreError::InvalidBatch("archive.bundle_too_large")
}

/// Build the bundle for `from_day..=to_day`. `NotFound` when no file is in
/// range; `InvalidBatch("archive.range_too_large")` past [`MAX_BUNDLE_DAYS`];
/// `InvalidBatch("archive.bundle_too_large")` past [`MAX_BUNDLE_BYTES`]
/// (checked with `head` before any object is read);
/// `InvalidBatch("archive.rewrite_pending")` when a day in range is stale,
/// since a stale file may still hold a run deleted on request.
pub async fn bundle<S: ArchiveIndex + BlobStore>(
    store: &S,
    scope: &TenantScope,
    runbook_id: &RunbookId,
    from_day: i64,
    to_day: i64,
    include_evidence: bool,
) -> Result<Vec<u8>, StoreError> {
    bundle_capped(
        store,
        scope,
        runbook_id,
        from_day,
        to_day,
        include_evidence,
        MAX_BUNDLE_BYTES,
    )
    .await
}

/// [`bundle`] with another byte cap (tests prove the cap is applied before an
/// object is read).
pub async fn bundle_capped<S: ArchiveIndex + BlobStore>(
    store: &S,
    scope: &TenantScope,
    runbook_id: &RunbookId,
    from_day: i64,
    to_day: i64,
    include_evidence: bool,
    cap: u64,
) -> Result<Vec<u8>, StoreError> {
    if to_day < from_day || to_day - from_day >= MAX_BUNDLE_DAYS {
        return Err(StoreError::InvalidBatch("archive.range_too_large"));
    }
    let mut records: Vec<ArchiveRecord> = store
        .list_archives(scope, runbook_id, Some(from_day), Some(to_day))
        .await?;
    if records.is_empty() {
        return Err(StoreError::NotFound);
    }
    records.sort_by_key(|r| r.day);
    if records.iter().any(|r| r.stale) {
        return Err(StoreError::InvalidBatch("archive.rewrite_pending"));
    }
    let mut total = 0u64;
    let mut entries = Vec::new();
    let mut days = Vec::new();
    let mut evidence = Vec::new();
    let mut objects = Vec::new();
    for record in &records {
        total = total.saturating_add(record.size_bytes);
        if total > cap {
            return Err(too_large());
        }
        let bytes = store.get(&record_key(record)).await?;
        let file = format!("{}.zip", date_of(record.day));
        days.push(BundleDay {
            date: date_of(record.day),
            file: file.clone(),
            bytes: bytes.len() as u64,
            sha256: portable::sha256_hex(&bytes),
            run_count: record.run_count,
            stale: record.stale,
        });
        if include_evidence {
            for run in manifest(store, record).await?.runs {
                for item in run.evidence {
                    // UP2: a processed image lives under its derived ID and
                    // the original is deleted; anything else is the upload.
                    let mut held = None;
                    for evd_id in [
                        crate::uploads::processed_id(&item.evd_id),
                        item.evd_id.clone(),
                    ] {
                        let key = BlobKey::Upload {
                            execution_id: run.execution_id.clone(),
                            evd_id,
                        };
                        // Size first: an object over the cap is never read.
                        match store.head(&key).await {
                            Ok(head) => {
                                total = total.saturating_add(head.size);
                                if total > cap {
                                    return Err(too_large());
                                }
                                held = match store.get(&key).await {
                                    Ok(bytes) => Some(bytes),
                                    Err(StoreError::NotFound) => None,
                                    Err(e) => return Err(e),
                                };
                                break;
                            }
                            Err(StoreError::NotFound) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    let path = held
                        .as_ref()
                        .map(|_| format!("evidence/{}/{}", run.execution_id, item.evd_id));
                    if let (Some(path), Some(bytes)) = (&path, held) {
                        objects.push((path.clone(), bytes));
                    }
                    evidence.push(BundleEvidence {
                        execution_id: run.execution_id.clone(),
                        evd_id: item.evd_id,
                        name: item.name,
                        content_type: item.content_type,
                        size: item.size,
                        file: path,
                    });
                }
            }
        }
        entries.push((file, bytes));
    }
    let head = BundleManifest {
        format: BUNDLE_FORMAT.into(),
        runbook_id: runbook_id.clone(),
        from: date_of(from_day),
        to: date_of(to_day),
        days,
        include_evidence,
        evidence,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&head)
        .map_err(|_| StoreError::Backend("archive.encode_failed"))?;
    manifest_bytes.push(b'\n');
    entries.insert(0, (BUNDLE_MANIFEST.to_owned(), manifest_bytes));
    entries.extend(objects);
    zip::write(&entries, to_day)
}
