//! The daily archive builder (D53). Pure: the job gathers the rows and the
//! rendered reports, this module turns them into deterministic ZIP bytes.
//!
//! Layout, in this entry order:
//!
//! ```text
//! manifest.json                    DayManifest (pretty JSON)
//! <exe>/events.ndjson              one EventEnvelope per line
//! <exe>/execution.json             the D3 run unit, events and logs moved out
//! <exe>/logs/<step_run>.ndjson     one JSON string per log line (lossless)
//! <exe>/report.md                  the execution document as Markdown
//! project/…                        the D3 portable project context: the
//!                                  directory slice, this runbook, the versions
//!                                  the runs used and their snapshots, pools;
//!                                  no runs (they are the folders above)
//! ```
//!
//! Folders are sorted by execution ID and each folder's files by path, so a
//! run's entries are contiguous and one range read covers them. Evidence is
//! listed per run in the manifest; its objects stay in the blob store under
//! the run's prefix (`BlobKey::Upload`) and downloads may add them.
use super::{ARCHIVE_SCHEMA_VERSION, ArchivedRunSummary, DAILY_FORMAT, DAY_MS, date_of, zip};
use crate::{
    StoreError,
    catalog::TenantScope,
    portable::{self, ClaimLog, ProjectData, RunUnit},
};
use runbook_engine::EventEnvelope;
use runbook_model::{
    EvidenceId, ExecutionId, OrganizationId, ProjectId, RunbookId, StepRunId, VersionId,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, num::NonZeroU32};

pub const MANIFEST: &str = "manifest.json";
/// Prefix of the portable project context inside the ZIP.
pub const PROJECT_DIR: &str = "project/";
pub const REPORT: &str = "report.md";
pub const EXECUTION: &str = "execution.json";
pub const EVENTS: &str = "events.ndjson";
pub const LOGS_DIR: &str = "logs/";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayManifest {
    pub format: String,
    pub schema_version: u32,
    pub org_id: OrganizationId,
    pub project_id: ProjectId,
    pub runbook_id: RunbookId,
    /// UTC day number and its `YYYY-MM-DD`.
    pub day: i64,
    pub date: String,
    /// Every version the runs used, oldest number first.
    pub versions: Vec<ManifestVersion>,
    pub run_count: u32,
    pub runs: Vec<ManifestRun>,
    /// Where the portable project context lives (`project/`).
    pub project: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestVersion {
    pub version_id: VersionId,
    pub version: Option<NonZeroU32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRun {
    pub execution_id: ExecutionId,
    /// `<exe>/`
    pub folder: String,
    /// The run's event sequence when archived.
    pub revision: u64,
    pub summary: ArchivedRunSummary,
    /// The folder's files (paths relative to the folder), sorted.
    pub files: Vec<ManifestFile>,
    /// Finalized evidence, kept as separate objects (D53).
    pub evidence: Vec<ManifestEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEvidence {
    pub evd_id: EvidenceId,
    pub step_run_id: StepRunId,
    pub name: String,
    pub content_type: String,
    pub size: u64,
}

/// One run's folder: its manifest line and its files by relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunFolder {
    pub manifest: ManifestRun,
    pub files: BTreeMap<String, Vec<u8>>,
}

/// A live run, gathered by the job.
pub struct LiveRun {
    /// The run unit with its events (start replays are left out: they are a
    /// 24-hour idempotency window, not history).
    pub unit: RunUnit,
    pub summary: ArchivedRunSummary,
    pub report_markdown: String,
    pub evidence: Vec<ManifestEvidence>,
}

pub fn folder_of(id: &ExecutionId) -> String {
    format!("{id}/")
}

fn encode_failed() -> StoreError {
    StoreError::Backend("archive.encode_failed")
}

fn ndjson<T: Serialize>(rows: &[T]) -> Result<Vec<u8>, StoreError> {
    let mut out = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut out, row).map_err(|_| encode_failed())?;
        out.push(b'\n');
    }
    Ok(out)
}

/// The folder for a live run. The run unit is normalized first, so equal
/// runs always give equal bytes.
pub fn live_folder(run: LiveRun) -> Result<RunFolder, StoreError> {
    let LiveRun {
        mut unit,
        summary,
        report_markdown,
        mut evidence,
    } = run;
    portable::normalize_run(&mut unit);
    let id = unit.slice.run.projection.execution_id.clone();
    let events: Vec<EventEnvelope> = std::mem::take(&mut unit.slice.run.events);
    let logs: Vec<ClaimLog> = std::mem::take(&mut unit.claim_logs);
    unit.start_replays.clear();
    let mut files = BTreeMap::new();
    files.insert(EVENTS.to_owned(), ndjson(&events)?);
    let mut execution = serde_json::to_vec(&unit).map_err(|_| encode_failed())?;
    execution.push(b'\n');
    files.insert(EXECUTION.to_owned(), execution);
    for log in &logs {
        files.insert(
            format!("{LOGS_DIR}{}.ndjson", log.step_run_id),
            ndjson(&log.lines)?,
        );
    }
    files.insert(REPORT.to_owned(), report_markdown.into_bytes());
    evidence.sort_by(|a, b| a.evd_id.cmp(&b.evd_id));
    Ok(RunFolder {
        manifest: ManifestRun {
            folder: folder_of(&id),
            revision: unit.slice.run.revision,
            execution_id: id,
            summary,
            files: manifest_files(&files),
            evidence,
        },
        files,
    })
}

pub fn manifest_files(files: &BTreeMap<String, Vec<u8>>) -> Vec<ManifestFile> {
    files
        .iter()
        .map(|(path, bytes)| ManifestFile {
            path: path.clone(),
            bytes: bytes.len() as u64,
            sha256: portable::sha256_hex(bytes),
        })
        .collect()
}

/// The day's ZIP bytes and manifest. `context` is the portable project
/// context; its runs (if any) are dropped, since the folders carry them.
pub fn build_day(
    scope: &TenantScope,
    runbook_id: &RunbookId,
    day: i64,
    context: &ProjectData,
    mut folders: Vec<RunFolder>,
) -> Result<(Vec<u8>, DayManifest), StoreError> {
    folders.sort_by(|a, b| a.manifest.execution_id.cmp(&b.manifest.execution_id));
    if folders
        .windows(2)
        .any(|w| w[0].manifest.execution_id == w[1].manifest.execution_id)
    {
        return Err(StoreError::InvalidBatch("archive.duplicate_run"));
    }
    let mut versions: Vec<ManifestVersion> = folders
        .iter()
        .map(|f| ManifestVersion {
            version_id: f.manifest.summary.version_id.clone(),
            version: f.manifest.summary.version,
        })
        .collect();
    versions.sort_by(|a, b| {
        (a.version.is_none(), a.version, &a.version_id).cmp(&(
            b.version.is_none(),
            b.version,
            &b.version_id,
        ))
    });
    versions.dedup();
    let manifest = DayManifest {
        format: DAILY_FORMAT.into(),
        schema_version: ARCHIVE_SCHEMA_VERSION,
        org_id: scope.org_id.clone(),
        project_id: scope.project_id.clone(),
        runbook_id: runbook_id.clone(),
        day,
        date: date_of(day),
        versions,
        run_count: u32::try_from(folders.len()).map_err(|_| encode_failed())?,
        runs: folders.iter().map(|f| f.manifest.clone()).collect(),
        project: PROJECT_DIR.into(),
    };
    let mut context = context.clone();
    context.rows.runs.clear();
    context.rows.tombstones.clear();
    // The context's own manifest carries the end of the day, not a host
    // clock, so rewrites of unchanged content are byte-identical.
    let project = portable::encode(&context, (day + 1) * DAY_MS)
        .map_err(|_| StoreError::Backend("archive.encode_failed"))?;
    let mut entries = Vec::new();
    let mut head = serde_json::to_vec_pretty(&manifest).map_err(|_| encode_failed())?;
    head.push(b'\n');
    entries.push((MANIFEST.to_owned(), head));
    for folder in folders {
        for (path, bytes) in folder.files {
            entries.push((format!("{}{path}", folder.manifest.folder), bytes));
        }
    }
    for (path, bytes) in project.files {
        entries.push((format!("{PROJECT_DIR}{path}"), bytes));
    }
    Ok((zip::write(&entries, day)?, manifest))
}
