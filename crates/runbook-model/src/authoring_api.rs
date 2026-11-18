use crate::*;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftWriteView {
    #[serde(flatten)]
    pub draft: DraftView,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunbookCatalogView {
    pub runbook_id: RunbookId,
    pub slug: String,
    pub origin: String,
    pub archived: bool,
    /// Draft metadata title (slug when the draft does not parse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Highest published version number; null when only previews exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<u32>,
    /// UI-1: draft `metadata.description`; absent when the draft has none
    /// (or does not parse). At most 300 characters in the list
    /// (`description_truncated`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// UI-1 review: `description` is the first 300 characters of a longer
    /// one. Absent (false) when it is whole.
    #[serde(default, skip_serializing_if = "is_false")]
    pub description_truncated: bool,
    /// UI-1: draft `metadata.tags`, trimmed, without empty or repeated
    /// ones (first occurrence kept); empty when the draft has none (or does
    /// not parse).
    #[serde(default)]
    pub tags: Vec<String>,
    /// UI-1: a version is published and the current draft source differs
    /// from the latest published version's source. False when nothing is
    /// published.
    #[serde(default)]
    pub draft_ahead: bool,
    /// UI-1: the most recently created run of this runbook; absent when it
    /// has none, or when the credential lacks `executions:read` on the
    /// project (then `active_runs` is absent too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunbookLastRun>,
    /// UI-1: this runbook's runs that are queued, running, waiting or
    /// paused, counting at most 100 (a value of 100 means 100 or more).
    /// Present (0 or more) only when the credential also holds
    /// `executions:read` on the project; absent means the run fields are
    /// withheld, not that there are no runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_runs: Option<u32>,
}
fn is_false(value: &bool) -> bool {
    !*value
}
/// UI-1: a runbook's newest run, as the runbook list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunbookLastRun {
    pub id: ExecutionId,
    pub status: ExecutionStatus,
    pub created_at_ms: i64,
    /// Null while the run is open.
    pub completed_at_ms: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VersionSourceView {
    #[serde(flatten)]
    pub version: VersionView,
    pub source: String,
    pub semantic_hash: String,
    pub snapshot_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VersionDiffView {
    pub unified_diff: String,
    pub added_steps: Vec<String>,
    pub removed_steps: Vec<String>,
    pub changed_steps: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillUploadRequest {
    pub files: Vec<SkillUploadFile>,
    #[serde(default)]
    pub reference: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillUploadFile {
    pub path: String,
    pub content: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillFileView {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// Base64 file bytes, present only on `GET /v1/projects/{p}/skills/{hash}?files=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillUsageView {
    pub runbook_id: RunbookId,
    pub version_id: VersionId,
    pub step_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillVersionView {
    pub name: String,
    pub description: String,
    pub hash: String,
    pub files: Vec<SkillFileView>,
    pub reference: Option<String>,
    pub usages: Vec<SkillUsageView>,
    /// Whether this is the most recently recorded version of `name` in the
    /// project: the version validation and publishing pin when a runbook
    /// names the skill (the server's compile context). Re-uploading an older
    /// version does not make it latest.
    pub latest: bool,
}
/// One file of a Git sync: its path in the repository (reported back, never
/// stored) and its runbook source, whose `metadata.id` names the runbook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncFile {
    pub path: String,
    pub source: String,
}

/// `POST /v1/projects/{p}/sync` (2026-11-17, PRD §12 open-source path):
/// a repository folder's runbooks, published as a batch. Every file is
/// compiled first; any error refuses the whole batch with nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncRequest {
    pub files: Vec<SyncFile>,
    /// Recorded on each new version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Validate and report what would change, changing nothing (for pull
    /// request checks).
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Archive this project's Git-managed runbooks that have no file here.
    #[serde(default, skip_serializing_if = "is_false")]
    pub prune: bool,
}

/// What a sync did (or, with `dry_run`, would do) to one runbook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncRunbookView {
    /// The file's path; absent for an archived runbook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub slug: String,
    /// Absent for a runbook a dry run would create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_id: Option<RunbookId>,
    /// `create`, `adopt` (a UI-authored runbook becomes Git-managed),
    /// `update`, `unchanged` or `archive`.
    pub action: String,
    /// The published version (absent on a dry run and for `archive`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook_version_id: Option<VersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncView {
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub runbooks: Vec<SyncRunbookView>,
}

/// A refused sync: each file with errors, and its diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncFileErrors {
    pub path: String,
    pub errors: Vec<Diagnostic>,
}

/// The most files one sync takes.
pub const SYNC_MAX_FILES: usize = 500;

pub const AUTHORING_ROUTES: &[PublicRoute] = &[
    // Git sync (2026-11-17): a repository folder published as a batch.
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/sync",
        request: Some("SyncRequest"),
        response: Some("SyncView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/runbooks",
        request: Some("CreateRunbookRequest"),
        response: Some("DraftWriteView"),
        status: 201,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/runbooks",
        request: None,
        response: Some("DirectoryPage_RunbookCatalogView"),
        status: 200,
    },
    // UI-1c: one runbook by its slug in a project (same view as by ID).
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/runbooks/{slug}",
        request: None,
        response: Some("RunbookCatalogView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}",
        request: None,
        response: Some("RunbookCatalogView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}/draft",
        request: None,
        response: Some("DraftView"),
        status: 200,
    },
    PublicRoute {
        method: "PATCH",
        path: "/v1/runbooks/{rb}/draft",
        request: Some("SaveDraftRequest"),
        response: Some("DraftWriteView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/runbooks/{rb}/versions",
        request: None,
        response: Some("PublishedVersionView"),
        status: 201,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}/versions",
        request: None,
        response: Some("DirectoryPage_VersionView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}/versions/{version}",
        request: None,
        response: Some("VersionSourceView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}/versions/{version}/form",
        request: None,
        response: Some("VersionFormView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/runbooks/{rb}/versions/{a}/diff/{b}",
        request: None,
        response: Some("VersionDiffView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/validate",
        request: Some("ValidateRequest"),
        response: Some("ValidateResponse"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/skills",
        request: None,
        response: Some("DirectoryPage_SkillVersionView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/projects/{p}/skills",
        request: Some("SkillUploadRequest"),
        response: Some("SkillVersionView"),
        status: 201,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/skills/{hash}",
        request: None,
        response: Some("SkillVersionView"),
        status: 200,
    },
];

/// Start-form JSON Schemas for one immutable version (B1), and its step
/// outline (CLI3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VersionFormView {
    pub inputs: serde_json::Value,
    pub fields: serde_json::Value,
    /// The version's steps in order, titles only (CLI3; empty from servers
    /// that predate it).
    #[serde(default)]
    pub steps: Vec<StepOutline>,
    /// The runbook-level `finally` steps, in the same form.
    #[serde(default)]
    pub finally: Vec<StepOutline>,
}

/// One step of a version's outline: its ID, its title as written (a
/// template when it uses `${{ }}`), its kind, and the steps nested in it in
/// source order (a group's body, `catch` and `finally`; every branch of a
/// `parallel`; a `foreach` or `loop` body; each `switch` case, then its
/// default). Never instructions or other step content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepOutline {
    pub id: String,
    pub title: Option<String>,
    /// `task`, `group`, `parallel`, `foreach`, `loop`, `switch`, `wait`,
    /// `approval`, `set`, `end` or `call`.
    pub kind: String,
    #[serde(default)]
    pub steps: Vec<StepOutline>,
}
