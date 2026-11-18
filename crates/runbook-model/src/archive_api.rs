//! Daily archive wire types (PRD §14 "Retention and daily archives";
//! D53–D54). Frozen at T0 as planned operations; ARC3 implements them.
use crate::*;

/// One runbook's ZIP for one UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArchiveView {
    /// UTC day, `YYYY-MM-DD`.
    pub date: String,
    pub run_count: u32,
    pub size_bytes: u64,
    /// Null: kept indefinitely (the self-hosted default).
    pub expires_at_ms: Option<i64>,
    /// An amendment or deletion is waiting for the next daily rewrite.
    pub stale: bool,
    /// ARC3: lowercase hex SHA-256 of the file's bytes (deterministic, D53).
    pub sha256: String,
    /// ARC3: when the current bytes were written (store clock).
    pub written_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArchivePage {
    /// Newest day first.
    pub items: Vec<ArchiveView>,
}

/// One file of an archived run's folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArchivedFileView {
    /// Relative to the run's folder, e.g. `report.md`, `logs/sr_….ndjson`.
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// Evidence a run's folder lists; the objects are kept separately (D53).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArchivedEvidenceView {
    pub evd_id: EvidenceId,
    pub step_run_id: StepRunId,
    pub name: String,
    pub content_type: String,
    pub size: u64,
}

/// ARC3: an archived run, read-only, from its summary row and its folder in
/// the day's file (read by range through the ZIP index).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArchivedRunView {
    pub execution_id: ExecutionId,
    pub runbook_id: RunbookId,
    pub runbook_version_id: VersionId,
    /// Null for a preview version.
    pub runbook_version: Option<NonZeroU32>,
    pub status: ExecutionStatus,
    pub title: String,
    pub created_by: String,
    pub created_at_ms: i64,
    pub completed_at_ms: i64,
    /// The day's file (`YYYY-MM-DD`) and the run's folder inside it.
    pub date: String,
    pub folder: String,
    /// The run's live detail still exists (false once retention deleted it).
    pub live: bool,
    /// The file is waiting for a rewrite (an amendment since it was written).
    pub stale: bool,
    pub files: Vec<ArchivedFileView>,
    pub evidence: Vec<ArchivedEvidenceView>,
    /// The execution document as Markdown, as archived.
    pub report_markdown: String,
}

/// ARC3: the archive routes (implemented).
pub const ARCHIVE_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}/archives",
        request: None,
        response: Some("ArchivePage"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}/archives/{date}",
        request: None,
        response: None,
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/executions/{exe}/archive",
        request: None,
        response: Some("ArchivedRunView"),
        status: 200,
    },
];
