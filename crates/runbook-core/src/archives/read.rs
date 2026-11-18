//! Reading stored daily archives through range reads (D53, PRD §14: "Opening
//! an archived run reads just that folder with an R2 range request using the
//! ZIP index").
use super::{
    ARCHIVE_SCHEMA_VERSION, ArchiveIndex, ArchiveRecord, ArchivedRun, DAILY_FORMAT,
    build::{DayManifest, MANIFEST, ManifestRun, RunFolder},
    zip,
};
use crate::{
    StoreError,
    blob::{BlobKey, BlobStore},
    catalog::TenantScope,
    portable,
};
use runbook_model::{ExecutionId, RunbookId};
use std::collections::BTreeMap;

/// The blob holding the file an index row names.
pub fn archive_key(runbook_id: &RunbookId, day: i64, digest: [u8; 32]) -> BlobKey {
    BlobKey::Archive {
        runbook_id: runbook_id.clone(),
        day,
        digest,
    }
}

/// The blob of the current file of `record`.
pub fn record_key(record: &ArchiveRecord) -> BlobKey {
    archive_key(&record.runbook_id, record.day, record.digest)
}

/// Parse and check a day manifest (format and schema version).
pub fn parse_manifest(bytes: &[u8]) -> Result<DayManifest, StoreError> {
    let manifest: DayManifest =
        serde_json::from_slice(bytes).map_err(|_| StoreError::Backend("archive.corrupt"))?;
    if manifest.format != DAILY_FORMAT || manifest.schema_version != ARCHIVE_SCHEMA_VERSION {
        return Err(StoreError::Backend("archive.unsupported_schema_version"));
    }
    Ok(manifest)
}

/// The stored file's manifest, read through the ZIP index.
pub async fn manifest<B: BlobStore>(
    blobs: &B,
    record: &ArchiveRecord,
) -> Result<DayManifest, StoreError> {
    let key = record_key(record);
    let mut entries = zip::read_entries(blobs, &key, |name| name == MANIFEST).await?;
    parse_manifest(
        &entries
            .remove(MANIFEST)
            .ok_or(StoreError::Backend("archive.corrupt"))?,
    )
}

/// One run's folder from the stored file: only that folder's bytes are read.
/// Every file must match the manifest line's size and digest.
pub async fn folder<B: BlobStore>(
    blobs: &B,
    record: &ArchiveRecord,
    run: &ManifestRun,
) -> Result<RunFolder, StoreError> {
    let key = record_key(record);
    let prefix = run.folder.as_str();
    let entries = zip::read_entries(blobs, &key, |name| name.starts_with(prefix)).await?;
    folder_from(run, entries)
}

pub(crate) fn folder_from(
    run: &ManifestRun,
    entries: BTreeMap<String, Vec<u8>>,
) -> Result<RunFolder, StoreError> {
    let corrupt = || StoreError::Backend("archive.corrupt");
    let mut files = BTreeMap::new();
    for (name, bytes) in entries {
        let path = name.strip_prefix(&run.folder).ok_or_else(corrupt)?;
        files.insert(path.to_owned(), bytes);
    }
    let listed = super::build::manifest_files(&files);
    if listed != run.files {
        return Err(corrupt());
    }
    Ok(RunFolder {
        manifest: run.clone(),
        files,
    })
}

/// An archived run, read-only: its summary row and, while its file exists,
/// the file's row and the run's folder.
pub struct ArchivedRead {
    pub run: ArchivedRun,
    pub file: Option<(ArchiveRecord, RunFolder)>,
}

/// Open one archived run by ID: the index locates its file and folder, then
/// the manifest and the folder are read by range. `None` when the run has no
/// summary row in this tenant. A rewrite that swings the index between the
/// two reads is followed once.
pub async fn archived_run<S: ArchiveIndex + BlobStore>(
    store: &S,
    scope: &TenantScope,
    execution_id: &ExecutionId,
) -> Result<Option<ArchivedRead>, StoreError> {
    let Some(run) = store.archived_run(scope, execution_id).await? else {
        return Ok(None);
    };
    let mut attempts = 0;
    loop {
        attempts += 1;
        let Some(record) = store.get_archive(scope, &run.runbook_id, run.day).await? else {
            return Ok(Some(ArchivedRead { run, file: None }));
        };
        let opened = async {
            let manifest = manifest(store, &record).await?;
            let line = manifest
                .runs
                .iter()
                .find(|r| &r.execution_id == execution_id)
                .ok_or(StoreError::Backend("archive.corrupt"))?;
            folder(store, &record, line).await
        }
        .await;
        match opened {
            Ok(folder) => {
                return Ok(Some(ArchivedRead {
                    run,
                    file: Some((record, folder)),
                }));
            }
            Err(StoreError::NotFound) if attempts < 2 => continue,
            Err(e) => return Err(e),
        }
    }
}

/// The Markdown report inside a folder.
pub fn report_markdown(folder: &RunFolder) -> Result<String, StoreError> {
    folder
        .files
        .get(super::build::REPORT)
        .and_then(|b| String::from_utf8(b.clone()).ok())
        .ok_or(StoreError::Backend("archive.corrupt"))
}

/// Hex digest helper for callers listing files.
pub fn digest_hex(bytes: &[u8]) -> String {
    portable::sha256_hex(bytes)
}
