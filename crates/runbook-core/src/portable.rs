//! D3 portable project archive (PRD section 14, "Export and portability").
//!
//! One tenant-scoped project becomes `manifest.json`, one NDJSON file per
//! logical table in a fixed order, and `blobs/<sha256>` content files. This
//! module only encodes, decodes and validates in memory; hosts read and
//! write the files and adapters supply the rows. Runs travel as units in the
//! existing B2.4 run-slice shape ([`ExecutionSlice`]), so the daily archive
//! and the project archive share one run format.
//!
//! Never exported: secrets and hashes of any kind (PATs and service tokens,
//! execution credential families and tokens, webhook signing secrets),
//! verified-email bindings, webhook delivery logs and the project event
//! outbox. See docs/architecture/export-archive.md.
use crate::{
    StoreError,
    authoring::hex,
    blob::BlobKey,
    catalog::{DraftRevision, RunbookRecord, SkillVersion, TenantScope, VersionRecord},
    directory::{
        DirectoryCommit, DirectoryError, DirectoryMutation, DirectorySnapshot, OrgRole,
        OrganizationRecord, ProjectRecord, TeamRecord, UserRecord,
    },
    directory_operations::VariablesRecord,
    retention::{ExecutionSlice, Tombstone},
    start::{StartRecord, StartReplay},
    webhooks::{SealedSecret, Subscription},
};
use runbook_engine::EventEnvelope;
use runbook_model::{
    ExecutionId, OrganizationId, ProjectId, ProjectRole, RunbookId, StepRunId, TeamId, UserId,
    WebhookId, WebhookNotice,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub mod memory;

pub const FORMAT: &str = "runbook-project-archive";
/// D42: version 2 adds `memory_entries.ndjson` and `memory_revisions.ndjson`
/// after the version 1 tables. Version 1 archives still decode (no memory).
pub const SCHEMA_VERSION: u32 = 2;
/// The oldest schema version [`decode`] accepts.
pub const SCHEMA_VERSION_V1: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
pub const BLOB_DIR: &str = "blobs/";

/// Every table file, in manifest order. Each is present even when empty.
pub const TABLES: &[&str] = &[
    "organization.ndjson",
    "project.ndjson",
    "users.ndjson",
    "teams.ndjson",
    "project_members.ndjson",
    "team_members.ndjson",
    "project_variables.ndjson",
    "runbooks.ndjson",
    "runbook_versions.ndjson",
    "drafts.ndjson",
    "skill_versions.ndjson",
    "admission_pools.ndjson",
    "executions.ndjson",
    "execution_events.ndjson",
    "tombstones.ndjson",
    "webhook_subscriptions.ndjson",
    "blobs.ndjson",
    "memory_entries.ndjson",
    "memory_revisions.ndjson",
];
/// The version 1 tables: every table before the D42 memory tables.
pub const TABLES_V1: usize = 17;

/// The at-rest marker for an imported subscription: no secret travels, so
/// deliveries fail closed (`secret_unavailable`) until an admin rotates.
pub const UNSET_SECRET: &str = "";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortableError {
    /// Malformed, truncated or tampered archive; `path` names the file.
    Archive {
        code: &'static str,
        path: Option<String>,
    },
    /// The target project already holds data; import never merges.
    TargetNotEmpty,
    /// The target holds a record that conflicts with the archive.
    Conflict(&'static str),
    Store(StoreError),
}
impl PortableError {
    pub(crate) fn archive(code: &'static str) -> Self {
        Self::Archive { code, path: None }
    }
    pub(crate) fn at(code: &'static str, path: &str) -> Self {
        Self::Archive {
            code,
            path: Some(path.to_owned()),
        }
    }
}
impl fmt::Display for PortableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archive {
                code,
                path: Some(path),
            } => write!(f, "{code} ({path})"),
            Self::Archive { code, path: None } => f.write_str(code),
            Self::TargetNotEmpty => f.write_str("import.target_not_empty"),
            Self::Conflict(code) => f.write_str(code),
            Self::Store(StoreError::NotFound) => f.write_str("store.not_found"),
            Self::Store(StoreError::InvalidBatch(code) | StoreError::Backend(code)) => {
                f.write_str(code)
            }
        }
    }
}
impl std::error::Error for PortableError {}
impl From<StoreError> for PortableError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<DirectoryError> for PortableError {
    fn from(error: DirectoryError) -> Self {
        match error {
            DirectoryError::Backend(code) => Self::Store(StoreError::Backend(code)),
            DirectoryError::NotFound => Self::Conflict("import.directory_not_found"),
            _ => Self::Conflict("import.directory_conflict"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMember {
    pub user_id: UserId,
    pub role: ProjectRole,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMember {
    pub team_id: TeamId,
    pub user_id: UserId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunbookRow {
    pub record: RunbookRecord,
    pub draft_revision: u64,
    pub draft_source: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionRow {
    pub runbook_id: RunbookId,
    pub version: VersionRecord,
}
/// A concurrency pool (D25): limit, active count and FIFO waiters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolRow {
    pub key: String,
    pub limit: u32,
    pub active: u32,
    pub queue: Vec<ExecutionId>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartReplayRow {
    pub replay: StartReplay,
    pub created_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimLog {
    pub step_run_id: StepRunId,
    pub lines: Vec<String>,
}
/// One run as a unit: the B2.4 run slice plus its start facts, start replay
/// rows and claim logs. In the archive the slice's events move to
/// `execution_events.ndjson`, one event per line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunUnit {
    pub slice: ExecutionSlice,
    pub start: Option<StartRecord>,
    pub start_replays: Vec<StartReplayRow>,
    pub claim_logs: Vec<ClaimLog>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct EventRow {
    execution_id: ExecutionId,
    event: EventEnvelope,
}
/// A webhook subscription without its signing secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookRow {
    pub id: WebhookId,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub runbooks: Vec<RunbookId>,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub secret_rotated_at_ms: i64,
    pub failing_since_ms: Option<i64>,
    pub disabled_reason: Option<String>,
    pub disabled_at_ms: Option<i64>,
    pub revision: u64,
    pub notice: Option<WebhookNotice>,
}
impl WebhookRow {
    pub fn from_subscription(s: &Subscription) -> Self {
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            url: s.url.clone(),
            events: s.events.clone(),
            runbooks: s.runbooks.clone(),
            tags: s.tags.clone(),
            enabled: s.enabled,
            created_at_ms: s.created_at_ms,
            updated_at_ms: s.updated_at_ms,
            secret_rotated_at_ms: s.secret_rotated_at_ms,
            failing_since_ms: s.failing_since_ms,
            disabled_reason: s.disabled_reason.clone(),
            disabled_at_ms: s.disabled_at_ms,
            revision: s.revision,
            notice: s.notice.clone(),
        }
    }
    /// The stored form after import: no secret, no previous secret.
    pub fn to_subscription(&self, scope: &TenantScope) -> Subscription {
        Subscription {
            id: self.id.clone(),
            scope: scope.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            events: self.events.clone(),
            runbooks: self.runbooks.clone(),
            tags: self.tags.clone(),
            enabled: self.enabled,
            secret: SealedSecret(UNSET_SECRET.into()),
            previous_secret: None,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            secret_rotated_at_ms: self.secret_rotated_at_ms,
            failing_since_ms: self.failing_since_ms,
            last_failure_at_ms: None,
            disabled_reason: self.disabled_reason.clone(),
            disabled_at_ms: self.disabled_at_ms,
            revision: self.revision,
            notice: self.notice.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRow {
    pub key: BlobKey,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BlobIndexRow {
    key: BlobKey,
    sha256: String,
    size: u64,
}

/// The project's directory slice: its organization and project records,
/// the users it references (project members plus organization owners and
/// admins), every team of the organization and the memberships among them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectorySlice {
    pub organization: OrganizationRecord,
    pub project: ProjectRecord,
    pub users: Vec<UserRecord>,
    pub teams: Vec<TeamRecord>,
    pub project_members: Vec<ProjectMember>,
    pub team_members: Vec<TeamMember>,
}

/// The catalog, run and blob rows an execution-store adapter owns.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct StoreRows {
    pub runbooks: Vec<RunbookRow>,
    pub versions: Vec<VersionRow>,
    pub drafts: Vec<DraftRevision>,
    pub skills: Vec<SkillVersion>,
    pub pools: Vec<PoolRow>,
    pub runs: Vec<RunUnit>,
    pub tombstones: Vec<Tombstone>,
    pub blobs: Vec<BlobRow>,
    /// D42 runbook memory: each entry with its full revision history.
    pub memory: Vec<MemoryRow>,
}

/// One memory entry as an adapter stores it: every revision, oldest first
/// (the last is the current entry), and for an agent proposal the execution
/// sequence that recorded it (D40 idempotency survives an import).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRow {
    pub revisions: Vec<crate::memory_entries::MemoryRevisionRecord>,
    pub proposal_sequence: Option<u64>,
}
impl MemoryRow {
    pub fn current(&self) -> Option<&crate::memory_entries::MemoryEntryRecord> {
        self.revisions.last().map(|r| &r.entry)
    }
}
/// `memory_entries.ndjson`: the current entry and its proposal sequence.
/// Revisions travel one per line in `memory_revisions.ndjson`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryEntryLine {
    entry: crate::memory_entries::MemoryEntryRecord,
    proposal_sequence: Option<u64>,
}

/// How an import may treat its target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Allow importing into an organization that already exists on the
    /// target. Off by default: the archive's organization must be new.
    pub into_existing_org: bool,
}

/// The logical content of one project archive.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectData {
    pub scope: TenantScope,
    pub directory: DirectorySlice,
    pub variables: Option<VariablesRecord>,
    pub rows: StoreRows,
    pub webhooks: Vec<WebhookRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    /// NDJSON line count; absent for blob files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records: Option<u64>,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub schema_version: u32,
    /// The newest engine semantics version among the archive's runs that
    /// the exporter could have written (F95); each run head carries its own.
    /// Archives written before the marker existed read as the legacy version.
    #[serde(default = "runbook_engine::legacy_engine_version")]
    pub engine_version: u32,
    pub org_id: OrganizationId,
    pub project_id: ProjectId,
    /// Host time when the archive was written; not part of the content.
    pub created_at_ms: i64,
    pub files: Vec<FileEntry>,
}

/// An archive as named byte files (`manifest.json`, `*.ndjson`, `blobs/*`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Archive {
    pub files: BTreeMap<String, Vec<u8>>,
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha256(bytes))
}

/// Team IDs that exported versions, runs and blobs mention (approvers,
/// escalation targets, compiled snapshots). Only these teams are exported.
pub fn referenced_teams(rows: &StoreRows) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut scan = |bytes: &[u8]| {
        let mut at = 0;
        while let Some(offset) = bytes[at..].windows(5).position(|w| w == b"team_") {
            let start = at + offset;
            let end = start + 5 + 26;
            if let Some(candidate) = bytes.get(start..end)
                && candidate[5..]
                    .iter()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
                && let Ok(text) = std::str::from_utf8(candidate)
            {
                found.insert(text.to_owned());
            }
            at = start + 5;
        }
    };
    for version in &rows.versions {
        scan(&serde_json::to_vec(version).unwrap_or_default());
    }
    for run in &rows.runs {
        scan(&serde_json::to_vec(run).unwrap_or_default());
    }
    for blob in &rows.blobs {
        scan(&blob.bytes);
    }
    found
}

/// Directory slice for one project from an organization snapshot: project
/// members plus organization owners and admins, the teams in `teams`
/// (see [`referenced_teams`]) and those teams' memberships among them.
pub fn directory_slice(
    snapshot: &DirectorySnapshot,
    project_id: &ProjectId,
    teams: &BTreeSet<String>,
) -> Result<DirectorySlice, PortableError> {
    let project = snapshot
        .projects
        .get(project_id)
        .cloned()
        .ok_or(PortableError::Store(StoreError::NotFound))?;
    let project_members: Vec<ProjectMember> = snapshot
        .project_members
        .iter()
        .filter(|((p, _), _)| p == project_id)
        .map(|((_, user_id), role)| ProjectMember {
            user_id: user_id.clone(),
            role: *role,
        })
        .collect();
    let referenced: BTreeSet<&UserId> = project_members
        .iter()
        .map(|m| &m.user_id)
        .chain(
            snapshot
                .users
                .values()
                .filter(|u| matches!(u.role, OrgRole::Owner | OrgRole::Admin))
                .map(|u| &u.id),
        )
        .collect();
    let users: Vec<UserRecord> = snapshot
        .users
        .values()
        .filter(|u| referenced.contains(&u.id))
        .cloned()
        .collect();
    let teams: Vec<TeamRecord> = snapshot
        .teams
        .values()
        .filter(|t| teams.contains(t.id.as_str()))
        .cloned()
        .collect();
    let kept: BTreeSet<&TeamId> = teams.iter().map(|t| &t.id).collect();
    let team_members = snapshot
        .team_members
        .iter()
        .filter(|(team, user)| kept.contains(team) && referenced.contains(user))
        .map(|(team_id, user_id)| TeamMember {
            team_id: team_id.clone(),
            user_id: user_id.clone(),
        })
        .collect();
    let mut slice = DirectorySlice {
        organization: snapshot.organization.clone(),
        project,
        users,
        teams,
        project_members,
        team_members,
    };
    normalize_directory(&mut slice);
    Ok(slice)
}

/// The directory mutations an import applies. `existing` is the target
/// organization, if any; `known_users` are the archived user IDs that
/// already have an identity anywhere on the target.
///
/// A new organization is created as archived, but no archived user may
/// already exist on the instance. An existing organization is refused
/// unless `options.into_existing_org`; then nothing that exists changes:
/// users already in the organization keep their role, identity and teams;
/// a user known elsewhere on the instance is refused; new users are
/// created as members; only teams that do not exist are created, and only
/// they receive members; project roles are only added, never changed.
pub fn directory_commit(
    slice: &DirectorySlice,
    existing: Option<&DirectorySnapshot>,
    known_users: &BTreeSet<UserId>,
    options: ImportOptions,
) -> Result<Option<DirectoryCommit>, PortableError> {
    let org = &slice.organization.id;
    let mut mutations = Vec::new();
    if existing.is_some() && !options.into_existing_org {
        return Err(PortableError::Conflict("import.org_exists"));
    }
    let project_exists = match existing {
        None => {
            mutations.push(DirectoryMutation::CreateOrganization {
                title: slice.organization.title.clone(),
            });
            false
        }
        Some(snapshot) => match snapshot.projects.get(&slice.project.id) {
            Some(record) if record != &slice.project => {
                return Err(PortableError::Conflict("import.project_mismatch"));
            }
            found => found.is_some(),
        },
    };
    for user in &slice.users {
        if existing.is_some_and(|s| s.users.contains_key(&user.id)) {
            continue;
        }
        if known_users.contains(&user.id) {
            return Err(PortableError::Conflict("import.user_exists"));
        }
        let mut user = user.clone();
        if existing.is_some() {
            user.role = OrgRole::Member;
        }
        mutations.push(DirectoryMutation::CreateUser(user));
    }
    let mut new_teams = BTreeSet::new();
    for team in &slice.teams {
        if !existing.is_some_and(|s| s.teams.contains_key(&team.id)) {
            new_teams.insert(&team.id);
            mutations.push(DirectoryMutation::CreateTeam(team.clone()));
        }
    }
    if !project_exists {
        mutations.push(DirectoryMutation::CreateProject(slice.project.clone()));
    }
    for member in &slice.project_members {
        let key = (slice.project.id.clone(), member.user_id.clone());
        if existing.is_some_and(|s| s.project_members.contains_key(&key)) {
            continue;
        }
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: slice.project.id.clone(),
            user_id: member.user_id.clone(),
            role: member.role,
        });
    }
    for member in &slice.team_members {
        if new_teams.contains(&member.team_id) {
            mutations.push(DirectoryMutation::AddTeamMember {
                team_id: member.team_id.clone(),
                user_id: member.user_id.clone(),
            });
        }
    }
    Ok((!mutations.is_empty()).then(|| DirectoryCommit {
        org_id: org.clone(),
        expected_revision: existing.map_or(0, |s| s.revision),
        mutations,
    }))
}

fn normalize_directory(slice: &mut DirectorySlice) {
    slice.users.sort_by(|a, b| a.id.cmp(&b.id));
    slice.teams.sort_by(|a, b| a.id.cmp(&b.id));
    slice
        .project_members
        .sort_by(|a, b| a.user_id.cmp(&b.user_id));
    slice
        .team_members
        .sort_by(|a, b| (&a.team_id, &a.user_id).cmp(&(&b.team_id, &b.user_id)));
}

fn replay_order(record: &crate::IdempotencyRecord) -> (String, i64, String) {
    match crate::actor::replay_record_parts(record) {
        Ok(Some((prefix, at))) => (prefix.to_owned(), at, record.key.clone()),
        _ => (record.key.clone(), i64::MIN, record.key.clone()),
    }
}

/// Canonical order inside one run unit (shared with the daily archive, D53).
pub fn normalize_run(run: &mut RunUnit) {
    run.slice.actor_replays.sort_by_key(replay_order);
    run.slice.work.sort_by(|a, b| a.lease.id.cmp(&b.lease.id));
    run.slice.blob_keys.sort();
    run.start_replays
        .sort_by(|a, b| a.replay.key.cmp(&b.replay.key));
    run.claim_logs
        .sort_by(|a, b| a.step_run_id.cmp(&b.step_run_id));
}

/// Canonical row order, so equal content always encodes to equal bytes
/// whichever adapter produced it. Order that carries meaning is kept:
/// skill recording order (latest wins), assignment order, pool queues.
pub fn normalize(data: &mut ProjectData) {
    normalize_directory(&mut data.directory);
    let rows = &mut data.rows;
    rows.runbooks
        .sort_by(|a, b| a.record.runbook_id.cmp(&b.record.runbook_id));
    rows.versions.sort_by(|a, b| {
        (
            &a.runbook_id,
            a.version.number.is_none(),
            a.version.number,
            &a.version.version_id,
        )
            .cmp(&(
                &b.runbook_id,
                b.version.number.is_none(),
                b.version.number,
                &b.version.version_id,
            ))
    });
    rows.drafts.sort_by(|a, b| a.key.cmp(&b.key));
    rows.pools.sort_by(|a, b| a.key.cmp(&b.key));
    rows.runs.sort_by(|a, b| {
        a.slice
            .run
            .projection
            .execution_id
            .cmp(&b.slice.run.projection.execution_id)
    });
    for run in &mut rows.runs {
        normalize_run(run);
    }
    rows.tombstones
        .sort_by(|a, b| a.execution_id.cmp(&b.execution_id));
    rows.blobs.sort_by(|a, b| a.key.cmp(&b.key));
    // A drop marker (D1: a run's write that lost the race for the last
    // slot) records only that the write was refused; it never travels.
    rows.memory.retain(|row| !is_drop_marker(row));
    rows.memory
        .sort_by(|a, b| a.current().map(|e| &e.id).cmp(&b.current().map(|e| &e.id)));
    data.webhooks.sort_by(|a, b| a.id.cmp(&b.id));
}

fn ndjson<T: Serialize>(
    rows: impl IntoIterator<Item = T>,
) -> Result<(Vec<u8>, u64), PortableError> {
    let mut out = Vec::new();
    let mut count = 0;
    for row in rows {
        serde_json::to_writer(&mut out, &row)
            .map_err(|_| PortableError::archive("archive.encode_failed"))?;
        out.push(b'\n');
        count += 1;
    }
    Ok((out, count))
}

/// Encode a project with the host's creation time. Deterministic: the same
/// content and time always produce the same bytes.
pub fn encode(data: &ProjectData, created_at_ms: i64) -> Result<Archive, PortableError> {
    let mut data = data.clone();
    normalize(&mut data);
    let d = &data.directory;
    let r = &data.rows;
    let mut events = Vec::new();
    let mut runs = Vec::new();
    for run in &r.runs {
        let mut unit = run.clone();
        for event in std::mem::take(&mut unit.slice.run.events) {
            events.push(EventRow {
                execution_id: run.slice.run.projection.execution_id.clone(),
                event,
            });
        }
        runs.push(unit);
    }
    let mut blob_files: BTreeMap<String, &[u8]> = BTreeMap::new();
    let mut blob_index = Vec::new();
    for blob in &r.blobs {
        let digest = sha256_hex(&blob.bytes);
        blob_index.push(BlobIndexRow {
            key: blob.key.clone(),
            sha256: digest.clone(),
            size: blob.bytes.len() as u64,
        });
        blob_files.insert(digest, &blob.bytes);
    }
    let memory_lines = r
        .memory
        .iter()
        .map(|row| {
            Ok(MemoryEntryLine {
                entry: row
                    .current()
                    .cloned()
                    .ok_or(PortableError::archive("archive.invalid_memory"))?,
                proposal_sequence: row.proposal_sequence,
            })
        })
        .collect::<Result<Vec<_>, PortableError>>()?;
    let tables = [
        ndjson([&d.organization])?,
        ndjson([&d.project])?,
        ndjson(&d.users)?,
        ndjson(&d.teams)?,
        ndjson(&d.project_members)?,
        ndjson(&d.team_members)?,
        ndjson(&data.variables)?,
        ndjson(&r.runbooks)?,
        ndjson(&r.versions)?,
        ndjson(&r.drafts)?,
        ndjson(&r.skills)?,
        ndjson(&r.pools)?,
        ndjson(&runs)?,
        ndjson(&events)?,
        ndjson(&r.tombstones)?,
        ndjson(&data.webhooks)?,
        ndjson(&blob_index)?,
        ndjson(memory_lines)?,
        ndjson(r.memory.iter().flat_map(|row| &row.revisions))?,
    ];
    let mut archive = Archive::default();
    let mut files = Vec::new();
    for (path, (bytes, records)) in TABLES.iter().zip(tables) {
        files.push(FileEntry {
            path: (*path).into(),
            records: Some(records),
            bytes: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        });
        archive.files.insert((*path).into(), bytes);
    }
    for (digest, bytes) in blob_files {
        let path = format!("{BLOB_DIR}{digest}");
        files.push(FileEntry {
            path: path.clone(),
            records: None,
            bytes: bytes.len() as u64,
            sha256: digest,
        });
        archive.files.insert(path, bytes.to_vec());
    }
    let manifest = Manifest {
        format: FORMAT.into(),
        schema_version: SCHEMA_VERSION,
        engine_version: runbook_engine::ENGINE_VERSION,
        org_id: data.scope.org_id.clone(),
        project_id: data.scope.project_id.clone(),
        created_at_ms,
        files,
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| PortableError::archive("archive.encode_failed"))?;
    bytes.push(b'\n');
    archive.files.insert(MANIFEST.into(), bytes);
    Ok(archive)
}

fn parse_lines<T: DeserializeOwned>(
    path: &str,
    bytes: &[u8],
    records: u64,
) -> Result<Vec<T>, PortableError> {
    if bytes.is_empty() {
        return if records == 0 {
            Ok(vec![])
        } else {
            Err(PortableError::at("archive.record_count_mismatch", path))
        };
    }
    if bytes.last() != Some(&b'\n') {
        return Err(PortableError::at("archive.truncated", path));
    }
    let mut rows = Vec::new();
    for line in bytes[..bytes.len() - 1].split(|b| *b == b'\n') {
        rows.push(
            serde_json::from_slice(line)
                .map_err(|_| PortableError::at("archive.invalid_record", path))?,
        );
    }
    if rows.len() as u64 != records {
        return Err(PortableError::at("archive.record_count_mismatch", path));
    }
    Ok(rows)
}

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Verify and decode an archive: the manifest's format and version, the
/// exact file set, every size and SHA-256 digest, record counts, tenant
/// scope on every scoped row, event continuity and canonical order. Any
/// failure rejects the whole archive before a target is touched.
pub fn decode(archive: &Archive) -> Result<(Manifest, ProjectData), PortableError> {
    let manifest: Manifest = serde_json::from_slice(
        archive
            .files
            .get(MANIFEST)
            .ok_or(PortableError::at("archive.missing_file", MANIFEST))?,
    )
    .map_err(|_| PortableError::at("archive.invalid_manifest", MANIFEST))?;
    if manifest.format != FORMAT {
        return Err(PortableError::at("archive.unknown_format", MANIFEST));
    }
    // D42: a version 1 archive has no memory tables; it still imports.
    let table_count = match manifest.schema_version {
        SCHEMA_VERSION_V1 => TABLES_V1,
        SCHEMA_VERSION => TABLES.len(),
        _ => {
            return Err(PortableError::at(
                "archive.unsupported_schema_version",
                MANIFEST,
            ));
        }
    };
    if manifest.engine_version > runbook_engine::ENGINE_VERSION {
        return Err(PortableError::at(
            "archive.unsupported_engine_version",
            MANIFEST,
        ));
    }
    let tables = manifest.files.get(..table_count).unwrap_or_default();
    if tables.len() != table_count
        || tables
            .iter()
            .zip(TABLES)
            .any(|(entry, path)| entry.path != *path || entry.records.is_none())
    {
        return Err(PortableError::at("archive.invalid_manifest", MANIFEST));
    }
    let blob_entries = &manifest.files[table_count..];
    let mut previous: Option<&str> = None;
    for entry in blob_entries {
        let digest = entry
            .path
            .strip_prefix(BLOB_DIR)
            .filter(|d| is_digest(d) && *d == entry.sha256)
            .ok_or(PortableError::at("archive.invalid_manifest", MANIFEST))?;
        if entry.records.is_some() || previous.is_some_and(|p| p >= digest) {
            return Err(PortableError::at("archive.invalid_manifest", MANIFEST));
        }
        previous = Some(digest);
    }
    let listed: BTreeSet<&str> = manifest
        .files
        .iter()
        .map(|e| e.path.as_str())
        .chain([MANIFEST])
        .collect();
    for path in archive.files.keys() {
        if !listed.contains(path.as_str()) {
            return Err(PortableError::at("archive.unexpected_file", path));
        }
    }
    let mut contents: BTreeMap<&str, &[u8]> = BTreeMap::new();
    for entry in &manifest.files {
        let bytes = archive
            .files
            .get(&entry.path)
            .ok_or(PortableError::at("archive.missing_file", &entry.path))?;
        if bytes.len() as u64 != entry.bytes {
            return Err(PortableError::at("archive.size_mismatch", &entry.path));
        }
        if sha256_hex(bytes) != entry.sha256 {
            return Err(PortableError::at("archive.digest_mismatch", &entry.path));
        }
        contents.insert(&entry.path, bytes);
    }
    let table = |index: usize| -> (&str, &[u8], u64) {
        let entry = &manifest.files[index];
        (
            entry.path.as_str(),
            contents[entry.path.as_str()],
            entry.records.unwrap_or_default(),
        )
    };
    macro_rules! rows {
        ($index:expr) => {{
            let (path, bytes, records) = table($index);
            parse_lines(path, bytes, records)?
        }};
    }
    let scope = TenantScope {
        org_id: manifest.org_id.clone(),
        project_id: manifest.project_id.clone(),
    };
    let organization: Vec<OrganizationRecord> = rows!(0);
    let project: Vec<ProjectRecord> = rows!(1);
    let (Ok([organization]), Ok([project])) = (
        <[_; 1]>::try_from(organization),
        <[_; 1]>::try_from(project),
    ) else {
        return Err(PortableError::archive("archive.invalid_directory"));
    };
    if organization.id != scope.org_id
        || project.id != scope.project_id
        || project.org_id != scope.org_id
    {
        return Err(PortableError::archive("archive.scope_mismatch"));
    }
    let teams: Vec<TeamRecord> = rows!(3);
    if teams.iter().any(|t| t.org_id != scope.org_id) {
        return Err(PortableError::archive("archive.scope_mismatch"));
    }
    let directory = DirectorySlice {
        organization,
        project,
        users: rows!(2),
        teams,
        project_members: rows!(4),
        team_members: rows!(5),
    };
    let variables: Vec<VariablesRecord> = rows!(6);
    if variables.len() > 1 {
        return Err(PortableError::archive("archive.invalid_variables"));
    }
    let mut runs: Vec<RunUnit> = rows!(12);
    let events: Vec<EventRow> = rows!(13);
    let mut by_run: BTreeMap<ExecutionId, Vec<EventEnvelope>> = BTreeMap::new();
    for row in events {
        by_run.entry(row.execution_id).or_default().push(row.event);
    }
    for run in &mut runs {
        let slice = &mut run.slice;
        let id = slice.run.projection.execution_id.clone();
        if !slice.run.events.is_empty() {
            return Err(PortableError::archive("archive.invalid_events"));
        }
        let events = by_run.remove(&id).unwrap_or_default();
        if events.len() as u64 != slice.run.revision
            || events
                .iter()
                .enumerate()
                .any(|(n, e)| e.seq != n as u64 + 1 || e.execution_id != id)
        {
            return Err(PortableError::archive("archive.invalid_events"));
        }
        slice.run.events = events;
    }
    if !by_run.is_empty() {
        return Err(PortableError::archive("archive.orphan_events"));
    }
    let tombstones: Vec<Tombstone> = rows!(14);
    if tombstones.iter().any(|t| t.scope != scope) {
        return Err(PortableError::archive("archive.scope_mismatch"));
    }
    let index: Vec<BlobIndexRow> = rows!(16);
    let mut used = BTreeSet::new();
    let mut blobs = Vec::new();
    for row in index {
        let path = format!("{BLOB_DIR}{}", row.sha256);
        let bytes = contents
            .get(path.as_str())
            .ok_or(PortableError::at("archive.missing_blob", &path))?;
        if bytes.len() as u64 != row.size {
            return Err(PortableError::at("archive.size_mismatch", &path));
        }
        used.insert(path);
        blobs.push(BlobRow {
            key: row.key,
            bytes: bytes.to_vec(),
        });
    }
    if used.len() != blob_entries.len() {
        return Err(PortableError::archive("archive.unreferenced_blob"));
    }
    let memory = if table_count > TABLES_V1 {
        decode_memory(rows!(TABLES_V1), rows!(TABLES_V1 + 1))?
    } else {
        vec![]
    };
    let data = ProjectData {
        scope,
        directory,
        variables: variables.into_iter().next(),
        rows: StoreRows {
            memory,
            runbooks: rows!(7),
            versions: rows!(8),
            drafts: rows!(9),
            skills: rows!(10),
            pools: rows!(11),
            runs,
            tombstones,
            blobs,
        },
        webhooks: rows!(15),
    };
    validate(&data)?;
    Ok((manifest, data))
}

/// Join `memory_entries.ndjson` with `memory_revisions.ndjson`. Revisions
/// must be in canonical order (entry ID, then revision) and every revision
/// must belong to a listed entry whose current state is its last revision.
fn decode_memory(
    entries: Vec<MemoryEntryLine>,
    revisions: Vec<crate::memory_entries::MemoryRevisionRecord>,
) -> Result<Vec<MemoryRow>, PortableError> {
    let invalid = || PortableError::archive("archive.invalid_memory");
    let mut previous: Option<(&runbook_model::MemoryId, u32)> = None;
    for revision in &revisions {
        let key = (&revision.entry.id, revision.entry.revision);
        if previous.is_some_and(|p| p >= key) {
            return Err(PortableError::archive("archive.not_canonical"));
        }
        previous = Some(key);
    }
    let mut by_id: BTreeMap<runbook_model::MemoryId, Vec<_>> = BTreeMap::new();
    for revision in revisions {
        by_id
            .entry(revision.entry.id.clone())
            .or_default()
            .push(revision);
    }
    let mut rows = Vec::with_capacity(entries.len());
    for line in entries {
        let revisions = by_id.remove(&line.entry.id).ok_or_else(invalid)?;
        if revisions.last().map(|r| &r.entry) != Some(&line.entry) {
            return Err(invalid());
        }
        rows.push(MemoryRow {
            revisions,
            proposal_sequence: line.proposal_sequence,
        });
    }
    if !by_id.is_empty() {
        return Err(PortableError::archive("archive.orphan_memory_revisions"));
    }
    // Archives written before markers were left out may still hold them:
    // import skips them.
    rows.retain(|row| !is_drop_marker(row));
    Ok(rows)
}

/// A D1 drop marker: an archived entry naming why its write was refused.
pub fn is_drop_marker(row: &MemoryRow) -> bool {
    row.current().is_some_and(|e| e.dropped.is_some())
}

/// One memory row is self-consistent: revisions of one entry in the
/// archive's scope, starting at 1 and increasing, with only the gap the
/// entry records as pruned (MEM6 review fix 2: the first and the newest
/// revisions are kept), a fixed identity (scope, source, creation), a first
/// revision that created or proposed it, and a proposal sequence exactly
/// when a run wrote it (a proposal, or a MEM6 live save).
fn check_memory(scope: &TenantScope, row: &MemoryRow) -> Result<(), PortableError> {
    use crate::memory_entries::MemoryAction;
    let invalid = || PortableError::archive("archive.invalid_memory");
    let first = &row.revisions.first().ok_or_else(invalid)?.entry;
    let last = &row.revisions.last().ok_or_else(invalid)?.entry;
    // The kept revisions are exactly what pruning leaves: revision 1, the
    // newest revision a person wrote if it falls in the pruned middle
    // (MEM6 re-check fix 1), and the newest ones up to the current one; and
    // the entry counts the rest as pruned.
    let current = last.revision;
    let through = current.saturating_sub(crate::memory_entries::MEMORY_REVISIONS_KEPT);
    let mut kept = vec![1];
    if (2..=through).contains(&last.person_revision) {
        kept.push(last.person_revision);
    }
    kept.extend(through.max(1) + 1..=current);
    let numbers: Vec<u32> = row.revisions.iter().map(|r| r.entry.revision).collect();
    if numbers != kept || current as usize - kept.len() != last.revisions_pruned as usize {
        return Err(invalid());
    }
    for (index, revision) in row.revisions.iter().enumerate() {
        let entry = &revision.entry;
        if &entry.scope != scope {
            return Err(PortableError::archive("archive.scope_mismatch"));
        }
        let first_action = matches!(
            revision.action,
            MemoryAction::Create | MemoryAction::Propose
        );
        if entry.id != first.id
            || entry.memory_scope != first.memory_scope
            || entry.source != first.source
            || entry.created_at_ms != first.created_at_ms
            || first_action != (index == 0)
        {
            return Err(invalid());
        }
    }
    let from_run = matches!(first.source, runbook_model::MemorySource::StepRun { .. });
    if from_run != row.proposal_sequence.is_some()
        || (from_run && row.revisions[0].action != crate::memory_entries::run_action(first))
    {
        return Err(invalid());
    }
    Ok(())
}

fn unique<T: Ord>(items: impl IntoIterator<Item = T>) -> Result<(), PortableError> {
    let mut seen = BTreeSet::new();
    for item in items {
        if !seen.insert(item) {
            return Err(PortableError::archive("archive.duplicate_row"));
        }
    }
    Ok(())
}

/// The content checks every import runs, whether or not the data came
/// through [`decode`]: archives are untrusted and their digests can be
/// recomputed, so tenant isolation never relies on them. Every scoped row
/// carries the archive's scope, every ID embedded in a run names that run,
/// keyed rows are unique, pools agree with their admissions, references
/// resolve and rows are in canonical order.
pub fn validate(data: &ProjectData) -> Result<(), PortableError> {
    let scope = &data.scope;
    let mismatch = || PortableError::archive("archive.scope_mismatch");
    let d = &data.directory;
    if d.organization.id != scope.org_id
        || d.project.id != scope.project_id
        || d.project.org_id != scope.org_id
        || d.teams.iter().any(|t| t.org_id != scope.org_id)
    {
        return Err(mismatch());
    }
    let invalid = || PortableError::archive("archive.invalid_directory");
    if d.organization.title.trim().is_empty()
        || !crate::directory::slug(&d.project.slug)
        || d.project.title.trim().is_empty()
        || d.teams
            .iter()
            .any(|t| !crate::directory::slug(&t.slug) || t.title.trim().is_empty())
        || d.users.iter().any(|u| u.name.trim().is_empty())
    {
        return Err(invalid());
    }
    unique(d.users.iter().map(|u| &u.id))?;
    unique(d.teams.iter().map(|t| &t.id))?;
    unique(d.teams.iter().map(|t| &t.slug))?;
    unique(d.project_members.iter().map(|m| &m.user_id))?;
    unique(d.team_members.iter().map(|m| (&m.team_id, &m.user_id)))?;
    let r = &data.rows;
    // T0: upload and archive blobs join the format with UP1/ARC1 (D42/D53);
    // until then an archive naming them is refused, never half-imported.
    if r.blobs
        .iter()
        .any(|blob| matches!(blob.key, BlobKey::Upload { .. } | BlobKey::Archive { .. }))
    {
        return Err(PortableError::archive("archive.unsupported_blob"));
    }
    // A shared blob is content-addressed: its bytes must hash to its key
    // (F19). File digests only prove the bytes match the manifest.
    if r.blobs.iter().any(|blob| match &blob.key {
        BlobKey::Shared { hash } => sha256(&blob.bytes) != *hash,
        BlobKey::Execution { .. } | BlobKey::Upload { .. } | BlobKey::Archive { .. } => false,
    }) {
        return Err(PortableError::archive("archive.blob_hash_mismatch"));
    }
    // Draft revisions start at 1 (F23); 0 would panic the draft view.
    if r.runbooks.iter().any(|x| x.draft_revision == 0) {
        return Err(PortableError::archive("archive.invalid_draft"));
    }
    unique(r.runbooks.iter().map(|x| &x.record.runbook_id))?;
    unique(r.runbooks.iter().map(|x| &x.record.slug))?;
    unique(r.versions.iter().map(|v| &v.version.version_id))?;
    unique(
        r.versions
            .iter()
            .filter_map(|v| v.version.number.map(|n| (&v.runbook_id, n))),
    )?;
    unique(r.drafts.iter().map(|x| &x.key))?;
    unique(r.skills.iter().map(|x| (&x.name, x.hash)))?;
    unique(r.pools.iter().map(|x| &x.key))?;
    unique(r.pools.iter().flat_map(|x| &x.queue))?;
    unique(r.runs.iter().map(|x| &x.slice.run.projection.execution_id))?;
    unique(
        r.runs
            .iter()
            .map(|x| &x.slice.run.projection.execution_id)
            .chain(r.tombstones.iter().map(|t| &t.execution_id)),
    )?;
    unique(
        r.runs
            .iter()
            .flat_map(|x| &x.slice.run.projection.steps)
            .map(|s| &s.step_run_id),
    )?;
    unique(
        r.runs
            .iter()
            .flat_map(|x| &x.start_replays)
            .map(|s| &s.replay.key),
    )?;
    unique(r.blobs.iter().map(|x| &x.key))?;
    unique(data.webhooks.iter().map(|x| &x.id))?;
    for row in &r.memory {
        check_memory(scope, row)?;
    }
    unique(
        r.memory
            .iter()
            .filter_map(MemoryRow::current)
            .map(|e| &e.id),
    )?;
    if r.tombstones.iter().any(|t| &t.scope != scope) {
        return Err(mismatch());
    }
    for run in &r.runs {
        check_run(scope, run)?;
    }
    check_pools(data)?;
    check_references(data)?;
    // Canonical order, checked without copying the blobs' bytes (CF12: a
    // Workers import holds its whole archive in a 128 MB isolate). The only
    // normalization of blobs is their key order, which is checked in place.
    let mut canonical = ProjectData {
        scope: data.scope.clone(),
        directory: data.directory.clone(),
        variables: data.variables.clone(),
        rows: StoreRows {
            runbooks: r.runbooks.clone(),
            versions: r.versions.clone(),
            drafts: r.drafts.clone(),
            skills: r.skills.clone(),
            pools: r.pools.clone(),
            runs: r.runs.clone(),
            tombstones: r.tombstones.clone(),
            blobs: Vec::new(),
            memory: r.memory.clone(),
        },
        webhooks: data.webhooks.clone(),
    };
    normalize(&mut canonical);
    let c = &canonical.rows;
    let same = canonical.scope == data.scope
        && canonical.directory == data.directory
        && canonical.variables == data.variables
        && canonical.webhooks == data.webhooks
        && c.runbooks == r.runbooks
        && c.versions == r.versions
        && c.drafts == r.drafts
        && c.skills == r.skills
        && c.pools == r.pools
        && c.runs == r.runs
        && c.tombstones == r.tombstones
        && c.memory == r.memory
        && r.blobs.windows(2).all(|w| w[0].key <= w[1].key);
    if !same {
        return Err(PortableError::archive("archive.not_canonical"));
    }
    Ok(())
}

/// CF12c: what a store whose rows hold at most 2,000,000 bytes (D1 and
/// Durable Object SQLite: the Cloudflare edition) checks after
/// [`validate`] and before any write. A start record whose JSON passes
/// `start::MAX_START_ROW_BYTES`, the bound every start is held to since
/// CF12b, is `archive.value_too_large`; without this the Cloudflare
/// edition failed at the run's object (a retryable `do.write`) after it
/// had begun writing.
///
/// CF12c review round 1: every other JSON body a run's object stores in
/// one row is held to the same bound: each event (a run's
/// `ExecutionStarted` event carries its whole start context, so it can
/// pass a row while the start record, or an archive without one, does
/// not), each idempotency record, notification intent and derived update,
/// and each actor and start replay.
///
/// CF12c review round 2 (N6): and the run's head (`meta.projection`, which
/// holds the run's result and outputs and every step's outputs and
/// summary in one row), each claim, and each archived log line (stored as
/// its text, one row a line).
///
/// CF13 (CF13-7): the object keeps the head without its steps, each step
/// run in its own row, and each derived update without its assignments,
/// each assignment in its own row; so the head, each step, each derived
/// update's remainder and each assignment are measured on their own, and
/// an archive of a long run the object can hold is not refused here. The
/// object's import measures every row it writes as well (CF13 review
/// round 1) and refuses one past the limit with `execution.row_too_large`.
///
/// [`validate`] does not refuse it (CF12b review round 2 did): a native
/// run started before CF12b may hold a larger record, and Postgres and the
/// memory store hold it, so an older native archive stays importable
/// natively, as an archived log line over 64 KiB does.
pub fn check_row_bounds(data: &ProjectData) -> Result<(), PortableError> {
    fn too_large(row: &impl Serialize) -> bool {
        serde_json::to_vec(row).map_or(true, |b| b.len() > crate::start::MAX_START_ROW_BYTES)
    }
    fn head_too_large(projection: &crate::ExecutionProjection) -> bool {
        let head = crate::ExecutionProjection {
            execution_id: projection.execution_id.clone(),
            runbook_id: projection.runbook_id.clone(),
            version_id: projection.version_id.clone(),
            compiled_hash: projection.compiled_hash,
            engine_version: projection.engine_version,
            sequence: projection.sequence,
            status: projection.status,
            phase: projection.phase,
            completed_at_ms: projection.completed_at_ms,
            result: projection.result.clone(),
            outputs: projection.outputs.clone(),
            steps: Vec::new(),
        };
        too_large(&head) || projection.steps.iter().any(too_large)
    }
    fn derived_too_large(update: &crate::catalog::DerivedUpdate) -> bool {
        let rest = crate::catalog::DerivedUpdate {
            summary: update.summary.clone(),
            assignments: Vec::new(),
            release_concurrency: update.release_concurrency,
            memory: update.memory.clone(),
        };
        too_large(&rest) || update.assignments.iter().any(too_large)
    }
    let refused = data.rows.runs.iter().any(|unit| {
        let run = &unit.slice.run;
        unit.start.as_ref().is_some_and(too_large)
            || head_too_large(&run.projection)
            || run.claims.values().any(too_large)
            || unit.claim_logs.iter().any(|log| {
                log.lines
                    .iter()
                    .any(|line| line.len() > crate::start::MAX_START_ROW_BYTES)
            })
            || run.events.iter().any(too_large)
            || run.idempotency.values().any(too_large)
            || run.outbox.values().any(too_large)
            || run.derived_outbox.values().any(derived_too_large)
            || unit.slice.actor_replays.iter().any(too_large)
            || unit.start_replays.iter().any(|r| too_large(&r.replay))
    });
    if refused {
        return Err(PortableError::archive("archive.value_too_large"));
    }
    Ok(())
}

/// Every ID embedded in a run names the run itself, so no row of the unit
/// can address another tenant's execution.
fn check_run(scope: &TenantScope, unit: &RunUnit) -> Result<(), PortableError> {
    let slice = &unit.slice;
    let run = &slice.run;
    let id = &run.projection.execution_id;
    let foreign = || PortableError::archive("archive.foreign_id");
    if &slice.scope != scope || slice.work.iter().any(|w| &w.lease.scope != scope) {
        return Err(PortableError::archive("archive.scope_mismatch"));
    }
    if run.events.len() as u64 != run.revision
        || run
            .events
            .iter()
            .enumerate()
            .any(|(n, e)| e.seq != n as u64 + 1 || &e.execution_id != id)
    {
        return Err(PortableError::archive("archive.invalid_events"));
    }
    let catalog = &slice.catalog;
    let own = |other: &ExecutionId| other == id;
    if run.projection.steps.iter().any(|s| !own(&s.execution_id))
        || run
            .timers
            .iter()
            .chain(&run.timer_history)
            .any(|(k, t)| !own(&t.execution_id) || k != &t.key)
        || run
            .outbox
            .iter()
            .any(|(k, o)| !own(&o.execution_id) || k != &o.event_id)
        || run
            .idempotency
            .iter()
            .any(|(k, i)| !own(&i.execution_id) || k != &i.key)
        || run
            .claims
            .iter()
            .any(|(k, c)| !own(&c.execution_id) || k != &c.step_run_id)
        || run.derived_outbox.iter().any(|(k, u)| {
            !own(&u.summary.execution_id)
                || *k != u.summary.sequence
                || u.assignments.iter().any(|a| !own(&a.execution_id))
                || u.memory.iter().any(|m| {
                    !own(&m.execution_id)
                        || &m.entry.scope != scope
                        || !matches!(&m.entry.source,
                            runbook_model::MemorySource::StepRun { execution_id, .. } if own(execution_id))
                })
        })
        || slice.actor_replays.iter().any(|a| !own(&a.execution_id))
        || catalog
            .summary
            .as_ref()
            .is_some_and(|s| !own(&s.execution_id) || s.runbook_id != run.projection.runbook_id)
        || catalog.assignments.iter().any(|a| !own(&a.execution_id))
        || catalog
            .admission
            .as_ref()
            .is_some_and(|(request, row)| !own(&request.execution_id) || !own(&row.execution_id))
        || slice.work.iter().any(|w| !own(&w.lease.id.execution_id))
        || slice
            .blob_keys
            .iter()
            .any(|k| !matches!(k, BlobKey::Execution { execution_id, .. } if own(execution_id)))
        || unit
            .start
            .as_ref()
            .is_some_and(|s| s.runbook_id != run.projection.runbook_id)
        || unit
            .start_replays
            .iter()
            .any(|s| !own(&s.replay.outcome.execution_id))
    {
        return Err(foreign());
    }
    let mut replays = BTreeSet::new();
    for record in &slice.actor_replays {
        let parts = crate::actor::replay_record_parts(record)
            .ok()
            .flatten()
            .ok_or(PortableError::archive("archive.invalid_replay"))?;
        if !replays.insert(parts) {
            return Err(PortableError::archive("archive.duplicate_row"));
        }
    }
    unique(catalog.assignments.iter().map(|a| &a.step_run_id))?;
    unique(slice.work.iter().map(|w| &w.lease.id))?;
    unique(&slice.blob_keys)?;
    unique(unit.claim_logs.iter().map(|l| &l.step_run_id))?;
    Ok(())
}

/// Each pool's queue holds exactly its queued admissions in FIFO order,
/// every admission names an exported pool, and `active` counts the pool's
/// admitted runs.
fn check_pools(data: &ProjectData) -> Result<(), PortableError> {
    use crate::catalog::AdmissionState;
    let invalid = || PortableError::archive("archive.invalid_pool");
    let pools: BTreeMap<&str, &PoolRow> = data
        .rows
        .pools
        .iter()
        .map(|p| (p.key.as_str(), p))
        .collect();
    let mut active: BTreeMap<&str, u32> = BTreeMap::new();
    let mut queued: BTreeMap<&ExecutionId, &str> = BTreeMap::new();
    for run in &data.rows.runs {
        let Some((request, row)) = &run.slice.catalog.admission else {
            continue;
        };
        if !pools.contains_key(request.key.as_str()) {
            return Err(invalid());
        }
        match row.state {
            AdmissionState::Admitted => *active.entry(&request.key).or_default() += 1,
            AdmissionState::Queued => {
                queued.insert(&run.slice.run.projection.execution_id, &request.key);
            }
            AdmissionState::Released => {}
        }
    }
    for pool in &data.rows.pools {
        if active.get(pool.key.as_str()).copied().unwrap_or_default() != pool.active
            || pool
                .queue
                .iter()
                .any(|id| queued.get(id).is_some_and(|key| *key != pool.key))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn check_references(data: &ProjectData) -> Result<(), PortableError> {
    let bad = || PortableError::archive("archive.dangling_reference");
    let users: BTreeSet<&UserId> = data.directory.users.iter().map(|u| &u.id).collect();
    let teams: BTreeSet<&TeamId> = data.directory.teams.iter().map(|t| &t.id).collect();
    if data
        .directory
        .project_members
        .iter()
        .any(|m| !users.contains(&m.user_id))
        || data
            .directory
            .team_members
            .iter()
            .any(|m| !users.contains(&m.user_id) || !teams.contains(&m.team_id))
    {
        return Err(bad());
    }
    let runbooks: BTreeSet<&RunbookId> = data
        .rows
        .runbooks
        .iter()
        .map(|r| &r.record.runbook_id)
        .collect();
    if data
        .rows
        .versions
        .iter()
        .any(|v| !runbooks.contains(&v.runbook_id))
    {
        return Err(bad());
    }
    let runs: BTreeSet<&ExecutionId> = data
        .rows
        .runs
        .iter()
        .map(|r| &r.slice.run.projection.execution_id)
        .collect();
    let queued: BTreeSet<&ExecutionId> = data.rows.pools.iter().flat_map(|p| &p.queue).collect();
    if queued.iter().any(|id| !runs.contains(id))
        || data.rows.runs.iter().any(|r| {
            let waiting = r
                .slice
                .catalog
                .admission
                .as_ref()
                .is_some_and(|(_, a)| a.state == crate::catalog::AdmissionState::Queued);
            waiting != queued.contains(&r.slice.run.projection.execution_id)
        })
    {
        return Err(bad());
    }
    if data
        .rows
        .memory
        .iter()
        .filter_map(MemoryRow::current)
        .any(|e| match &e.memory_scope {
            runbook_model::MemoryScope::Runbook { runbook_id } => !runbooks.contains(runbook_id),
            runbook_model::MemoryScope::Project => false,
        })
    {
        return Err(bad());
    }
    let blobs: BTreeSet<&BlobKey> = data.rows.blobs.iter().map(|b| &b.key).collect();
    for blob in &data.rows.blobs {
        if let BlobKey::Execution { execution_id, .. } = &blob.key
            && !runs.contains(execution_id)
        {
            return Err(bad());
        }
    }
    if data
        .rows
        .runs
        .iter()
        .flat_map(|r| &r.slice.blob_keys)
        .any(|k| !blobs.contains(k))
    {
        return Err(bad());
    }
    Ok(())
}
