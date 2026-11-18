//! CF8: the D1 half of the D3 portable project archive on Cloudflare
//! (`runbook-worker`'s `portable.rs` adds each run's object and R2).
//!
//! - [`export`] reads one project's D1 rows (as the Postgres adapter's
//!   `read_project` does): variables, catalog, pools with their queues, the
//!   runs' catalog slices and start replays, tombstones, memory and webhook
//!   subscriptions (without secrets).
//! - [`import_statements`] plans the D1 write as guarded batches, one on
//!   the control database (the directory and the project's variables) and
//!   one on the organization's shard (everything else, CF11); with one
//!   database they are one batch. The target project must hold nothing
//!   (`import.target_not_empty`), no execution ID may exist anywhere
//!   (`import.id_conflict`), and the project ID may not belong to another
//!   organization (`import.project_conflict`).
//!   The directory part is the directory adapter's own plan of
//!   `portable::directory_commit`. Other primary keys that exist already
//!   fail the batch as unique violations, which the caller maps to
//!   `import.id_conflict`.
use crate::{
    applier::{assignment_statement, summary_statement},
    catalog::{VERSION_COLUMNS, decode, err, version_at},
    directory::{D1Directory, Planned},
    sql::{Columns, D1, Statement, guard, json, prepare, uint},
};
use runbook_core::{
    StoreError,
    catalog::{
        AdmissionState, DraftRevision, RunCatalogSlice, RunbookOrigin, RunbookRecord, SkillVersion,
        TenantScope,
    },
    directory::{DirectoryError, DirectoryStore},
    directory_operations::VariablesRecord,
    portable::{
        ImportOptions, MemoryRow, PoolRow, PortableError, ProjectData, RunbookRow, StartReplayRow,
        StoreRows, VersionRow, WebhookRow, directory_commit,
    },
    retention::Tombstone,
    start::StartReplay,
    webhooks::Subscription,
};
use runbook_model::{ExecutionId, UserId};
use std::collections::BTreeSet;

/// One run's D1 rows.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRows {
    pub id: ExecutionId,
    pub catalog: RunCatalogSlice,
    pub start_replays: Vec<StartReplayRow>,
}

/// A project's D1 rows. `rows` holds everything but the runs, which are
/// listed in `runs` (their objects hold the rest).
#[derive(Clone, Debug, PartialEq)]
pub struct Exported {
    pub variables: Option<VariablesRecord>,
    pub rows: StoreRows,
    pub runs: Vec<RunRows>,
    pub webhooks: Vec<WebhookRow>,
}

/// IDs per guard or undo statement (CF12 review round 1): each is bound
/// once, under D1's 100 bound parameters a statement.
const ID_CHUNK: usize = 90;

/// The control database's tables holding project-scoped rows (CF11).
pub const CONTROL_SCOPED_TABLES: &[&str] =
    &["execution_token_route", "project_variables", "run_release"];

/// Every other D1 table holding project-scoped rows: the organization's
/// shard's. Any row in either list refuses an import.
pub const SCOPED_TABLES: &[&str] = &[
    "admission",
    "admission_notice",
    "admission_pool",
    "archive",
    "archive_attempt",
    "archive_member",
    "archive_run",
    "archive_version",
    "draft",
    "evidence",
    "execution_owner",
    "memory_entry",
    "memory_revision",
    "object_purge",
    "project_event_outbox",
    "retained_evidence",
    "runbook",
    "runbook_version",
    "skill_version",
    "start_idempotency",
    "start_replay",
    "step_run_index",
    "step_run_tombstone",
    "supersede_notice",
    "tombstone",
    "webhook_event",
    "webhook_replay",
    "webhook_subscription",
];

fn origin_of(text: &str) -> Result<RunbookOrigin, StoreError> {
    match text {
        "ui" => Ok(RunbookOrigin::Ui),
        "git" => Ok(RunbookOrigin::Git),
        _ => Err(StoreError::Backend("d1.invalid_runbook_origin")),
    }
}
fn origin_label(origin: RunbookOrigin) -> &'static str {
    match origin {
        RunbookOrigin::Ui => "ui",
        RunbookOrigin::Git => "git",
    }
}
fn tenant(statement: Statement, scope: &TenantScope) -> Statement {
    statement
        .bind(scope.org_id.as_str())
        .bind(scope.project_id.as_str())
}

/// Read one project's D1 rows: its variables from the control database,
/// the rest from its organization's shard `db` (CF11).
///
/// Sources kept in the project's blobs (CF11, `sources`) are read back from
/// `sources`, so the archive holds the text as natively.
pub async fn export<D: D1>(
    control: &D,
    db: &D,
    scope: &TenantScope,
    sources: Option<&dyn crate::sources::SourceBlobs>,
) -> Result<Exported, StoreError> {
    use crate::sources::stitch_source;
    let read = |sql: &'static str| tenant(prepare(sql), scope);
    let variables = control
        .all(read(
            "SELECT body, revision FROM project_variables WHERE org_id = ? AND project_id = ?",
        ))
        .await
        .map_err(err)?
        .first()
        .map(|row| {
            Ok::<_, StoreError>(VariablesRecord {
                variables: decode(row, 0)?,
                revision: row.unsigned(1).map_err(err)?,
            })
        })
        .transpose()?;
    let rows = db
        .batch(vec![
            read(
                "SELECT runbook_id, slug, origin, archived, draft_source, draft_revision, \
                 draft_digest FROM runbook WHERE org_id = ? AND project_id = ? \
                 ORDER BY runbook_id",
            ),
            tenant(
                prepare(format!(
                    "SELECT {VERSION_COLUMNS}, runbook_id, source_digest FROM runbook_version \
                     WHERE org_id = ? AND project_id = ?"
                )),
                scope,
            ),
            read(
                "SELECT key, revision, source, source_digest FROM draft \
                 WHERE org_id = ? AND project_id = ?",
            ),
            read(
                "SELECT name, description, hash, manifest, provenance FROM skill_version \
                 WHERE org_id = ? AND project_id = ? ORDER BY seq",
            ),
            read(
                "SELECT key, concurrency_limit, active FROM admission_pool \
                 WHERE org_id = ? AND project_id = ? ORDER BY key",
            ),
            read(
                "SELECT key, execution_id FROM admission WHERE org_id = ? AND project_id = ? \
                 AND queue_order IS NOT NULL ORDER BY key, queue_order",
            ),
            read(
                "SELECT execution_id FROM execution_owner WHERE org_id = ? AND project_id = ? \
                 ORDER BY execution_id",
            ),
            read("SELECT body FROM tombstone WHERE org_id = ? AND project_id = ?"),
            read(
                "SELECT memory_id, proposal_sequence FROM memory_entry \
                 WHERE org_id = ? AND project_id = ? ORDER BY memory_id",
            ),
            read("SELECT body FROM webhook_subscription WHERE org_id = ? AND project_id = ?"),
            // CF12 review round 1: the runs' and the memory entries' rows
            // project-wide, so the export's D1 queries do not grow with
            // them.
            read(
                "SELECT execution_id, idempotency_key, principal, request_hash, outcome, \
                 created_at_ms FROM start_replay WHERE org_id = ? AND project_id = ? \
                 ORDER BY execution_id, idempotency_key, principal",
            ),
            read(
                "SELECT r.execution_id, r.body FROM run_summary r JOIN execution_owner o \
                 ON o.execution_id = r.execution_id WHERE o.org_id = ? AND o.project_id = ?",
            ),
            read(
                "SELECT a.execution_id, a.body FROM assignment a JOIN execution_owner o \
                 ON o.execution_id = a.execution_id WHERE o.org_id = ? AND o.project_id = ? \
                 ORDER BY a.execution_id, a.ordinal",
            ),
            read(
                "SELECT a.execution_id, a.request, a.state, a.admitted_at_ms FROM admission a \
                 JOIN execution_owner o ON o.execution_id = a.execution_id \
                 WHERE o.org_id = ? AND o.project_id = ?",
            ),
            read(
                "SELECT memory_id, body FROM memory_revision WHERE org_id = ? AND project_id = ? \
                 ORDER BY memory_id, revision",
            ),
        ])
        .await
        .map_err(err)?;
    let mut out = StoreRows::default();
    for row in &rows[0].rows {
        out.runbooks.push(RunbookRow {
            record: RunbookRecord {
                runbook_id: row.parse(0).map_err(err)?,
                slug: row.string(1).map_err(err)?,
                origin: origin_of(row.text(2).map_err(err)?)?,
                archived: row.integer(3).map_err(err)? != 0,
            },
            draft_source: stitch_source(
                sources,
                String::from_utf8(row.blob(4).map_err(err)?.to_vec())
                    .map_err(|_| StoreError::Backend("catalog.invalid_draft_utf8"))?,
                row.optional_blob(6).map_err(err)?,
            )
            .await?,
            draft_revision: row.unsigned(5).map_err(err)?,
        });
    }
    for row in &rows[1].rows {
        let mut version = version_at(row, 0)?;
        version.source = stitch_source(
            sources,
            std::mem::take(&mut version.source),
            row.optional_blob(10).map_err(err)?,
        )
        .await?;
        out.versions.push(VersionRow {
            runbook_id: row.parse(9).map_err(err)?,
            version,
        });
    }
    for row in &rows[2].rows {
        out.drafts.push(DraftRevision {
            key: row.string(0).map_err(err)?,
            revision: row.unsigned(1).map_err(err)?,
            source: stitch_source(
                sources,
                row.string(2).map_err(err)?,
                row.optional_blob(3).map_err(err)?,
            )
            .await?,
        });
    }
    for row in &rows[3].rows {
        out.skills.push(SkillVersion {
            name: row.string(0).map_err(err)?,
            description: row.string(1).map_err(err)?,
            hash: row.digest(2).map_err(err)?,
            manifest: decode(row, 3)?,
            provenance: decode(row, 4)?,
        });
    }
    for row in &rows[4].rows {
        let key = row.string(0).map_err(err)?;
        let queue = rows[5]
            .rows
            .iter()
            .filter(|q| q.text(0).is_ok_and(|k| k == key))
            .map(|q| q.parse(1).map_err(err))
            .collect::<Result<_, _>>()?;
        let small = |i| {
            u32::try_from(row.unsigned(i).map_err(err)?)
                .map_err(|_| StoreError::Backend("d1.invalid_pool"))
        };
        out.pools.push(PoolRow {
            key,
            limit: small(1)?,
            active: small(2)?,
            queue,
        });
    }
    // The runs' rows by run.
    use std::collections::BTreeMap;
    let mut replays: BTreeMap<String, Vec<StartReplayRow>> = BTreeMap::new();
    for row in &rows[10].rows {
        replays
            .entry(row.string(0).map_err(err)?)
            .or_default()
            .push(StartReplayRow {
                replay: StartReplay {
                    key: row.string(1).map_err(err)?,
                    principal: row.string(2).map_err(err)?,
                    request_hash: row.digest(3).map_err(err)?,
                    outcome: decode(row, 4)?,
                },
                created_at_ms: row.integer(5).map_err(err)?,
            });
    }
    let mut summaries: BTreeMap<String, runbook_core::catalog::RunSummary> = BTreeMap::new();
    for row in &rows[11].rows {
        summaries.insert(row.string(0).map_err(err)?, decode(row, 1)?);
    }
    let mut assignments: BTreeMap<String, Vec<runbook_core::catalog::Assignment>> = BTreeMap::new();
    for row in &rows[12].rows {
        assignments
            .entry(row.string(0).map_err(err)?)
            .or_default()
            .push(decode(row, 1)?);
    }
    let mut admissions = BTreeMap::new();
    for row in &rows[13].rows {
        let id: ExecutionId = row.parse(0).map_err(err)?;
        admissions.insert(
            id.to_string(),
            (
                decode::<runbook_core::catalog::AdmitRequest>(row, 1)?,
                runbook_core::catalog::Admission {
                    execution_id: id,
                    state: crate::admission::state_of(row.text(2).map_err(err)?)?,
                    admitted_at_ms: row.optional_integer(3).map_err(err)?,
                },
            ),
        );
    }
    let mut runs = vec![];
    for row in &rows[6].rows {
        let id: ExecutionId = row.parse(0).map_err(err)?;
        let key = id.to_string();
        runs.push(RunRows {
            catalog: RunCatalogSlice {
                summary: summaries.remove(&key),
                assignments: assignments.remove(&key).unwrap_or_default(),
                admission: admissions.remove(&key),
            },
            start_replays: replays.remove(&key).unwrap_or_default(),
            id,
        });
    }
    for row in &rows[7].rows {
        out.tombstones.push(decode::<Tombstone>(row, 0)?);
    }
    let mut revisions: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for row in &rows[14].rows {
        revisions
            .entry(row.string(0).map_err(err)?)
            .or_default()
            .push(decode(row, 1)?);
    }
    for row in &rows[8].rows {
        let id = row.string(0).map_err(err)?;
        out.memory.push(MemoryRow {
            revisions: revisions.remove(&id).unwrap_or_default(),
            proposal_sequence: row
                .optional_integer(1)
                .map_err(err)?
                .map(|n| u64::try_from(n).map_err(|_| StoreError::Backend("d1.decode")))
                .transpose()?,
        });
    }
    let webhooks = rows[9]
        .rows
        .iter()
        .map(|row| {
            Ok(WebhookRow::from_subscription(&decode::<Subscription>(
                row, 0,
            )?))
        })
        .collect::<Result<_, StoreError>>()?;
    Ok(Exported {
        variables,
        rows: out,
        runs,
        webhooks,
    })
}

/// CF12 review round 1: how many runs `scope`'s project owns (the export's
/// object requests, counted before any run is read).
pub async fn run_count<D: D1>(db: &D, scope: &TenantScope) -> Result<usize, StoreError> {
    let rows = db
        .all(tenant(
            prepare("SELECT count(*) FROM execution_owner WHERE org_id = ? AND project_id = ?"),
            scope,
        ))
        .await
        .map_err(err)?;
    let n = rows
        .first()
        .ok_or(StoreError::Backend("d1.decode"))?
        .unsigned(0)
        .map_err(err)?;
    usize::try_from(n).map_err(|_| StoreError::Backend("d1.decode"))
}

fn store(error: StoreError) -> PortableError {
    PortableError::Store(error)
}
fn directory(error: DirectoryError) -> PortableError {
    PortableError::from(error)
}

/// The D1 half of an import, by database (CF11). Each list of guards
/// writes nothing when its conditions hold, so it can be checked alone
/// before anything is written.
#[derive(Clone, Debug, Default)]
pub struct ImportPlan {
    /// On the control database: the project is not another organization's,
    /// and its variables and token routes are empty.
    pub control_guards: Vec<Statement>,
    /// On the control database: the directory plan and the variables.
    pub control: Vec<Statement>,
    /// On the organization's shard: the project is empty there and no
    /// execution ID exists.
    pub shard_guards: Vec<Statement>,
    /// On the organization's shard: the project's rows.
    pub shard: Vec<Statement>,
    /// CF11: the runbook sources the rows name by digest, to store in the
    /// project's blobs before the rows commit (content-addressed, once each).
    pub sources: std::collections::BTreeMap<[u8; 32], Vec<u8>>,
    /// CF12b: when the directory plan creates the project, a guard that
    /// fails (`import.target_not_empty`) once the directory records it. Only
    /// this import's control batch can add that row (the project was in no
    /// organization when planned, and `import.project_conflict` keeps it
    /// so), so after a lost reply it tells a batch that landed from one that
    /// did not even when the batch held no project-scoped row: an import
    /// with no runs and no variables.
    pub project_recorded: Option<Statement>,
}
impl ImportPlan {
    /// Every guard, control first.
    pub fn guards(&self) -> Vec<Statement> {
        self.control_guards
            .iter()
            .chain(&self.shard_guards)
            .cloned()
            .collect()
    }
    /// CF12b: what reads back whether the control batch landed after its
    /// reply was lost: its guards, and [`Self::project_recorded`].
    pub fn control_readback(&self) -> Vec<Statement> {
        self.control_guards
            .iter()
            .chain(&self.project_recorded)
            .cloned()
            .collect()
    }
    /// CF12b: the same for the one batch of a colocated import.
    pub fn readback(&self) -> Vec<Statement> {
        self.guards()
            .into_iter()
            .chain(self.project_recorded.clone())
            .collect()
    }
    /// The whole import as one batch, guards first: the control database
    /// and the shard are one database.
    pub fn one_batch(self) -> Vec<Statement> {
        let mut out = self.control_guards;
        out.extend(self.shard_guards);
        out.extend(self.control);
        out.extend(self.shard);
        out
    }
}

/// Every row of `scope`'s project on its shard (CF11: an import whose
/// control batch failed after its shard batch committed takes them back;
/// the shard guards proved the project held nothing there before).
pub fn forget_shard_rows(scope: &TenantScope) -> Vec<Statement> {
    SCOPED_TABLES
        .iter()
        .map(|t| {
            tenant(
                prepare(format!(
                    "DELETE FROM {t} WHERE org_id = ? AND project_id = ?"
                )),
                scope,
            )
        })
        .collect()
}

/// Plan the D1 half of importing `data` (already validated): the directory
/// is read from `control`. With `external_sources`, runbook sources go to
/// the project's blobs (`ImportPlan::sources`) and the rows keep their
/// digests (CF11).
pub async fn import_statements<D: D1>(
    control: &D,
    data: &ProjectData,
    options: ImportOptions,
    external_sources: bool,
) -> Result<ImportPlan, PortableError> {
    let db = control;
    let scope = &data.scope;
    let org = scope.org_id.as_str();
    let project = scope.project_id.as_str();
    // The condition a guard needs is what must hold: no table has a row.
    let empty = |tables: &[&str]| {
        tables
            .iter()
            .map(|t| {
                format!("NOT EXISTS (SELECT 1 FROM {t} WHERE org_id = ?2 AND project_id = ?3)")
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let mut plan = ImportPlan {
        control_guards: vec![
            guard(
                "import.project_conflict",
                "NOT EXISTS (SELECT 1 FROM project WHERE project_id = ?3 AND org_id <> ?2)",
            )
            .bind(org)
            .bind(project),
            guard("import.target_not_empty", &empty(CONTROL_SCOPED_TABLES))
                .bind(org)
                .bind(project),
        ],
        shard_guards: vec![
            guard("import.target_not_empty", &empty(SCOPED_TABLES))
                .bind(org)
                .bind(project),
        ],
        ..ImportPlan::default()
    };
    let ids: Vec<&str> = data
        .rows
        .runs
        .iter()
        .map(|r| r.slice.run.projection.execution_id.as_str())
        .chain(data.rows.tombstones.iter().map(|t| t.execution_id.as_str()))
        .collect();
    // CF12 review round 1: one guard per 90 IDs (each bound once, under
    // D1's 100 bound parameters), not one per ID, so the guards stay a
    // small part of the commit batch.
    for chunk in ids.chunks(ID_CHUNK) {
        let list = (2..chunk.len() + 2)
            .map(|n| format!("?{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let statement = guard(
            "import.id_conflict",
            &format!(
                "NOT EXISTS (SELECT 1 FROM execution_owner WHERE execution_id IN ({list})) \
                 AND NOT EXISTS (SELECT 1 FROM tombstone WHERE execution_id IN ({list})) \
                 AND NOT EXISTS (SELECT 1 FROM webhook_event WHERE execution_id IN ({list}))"
            ),
        );
        plan.shard_guards.push(
            chunk
                .iter()
                .fold(statement, |statement, id| statement.bind(*id)),
        );
    }

    // The directory: the adapter's own plan, guarded by the revision.
    let directories = D1Directory::new(db.clone());
    let existing = match directories.snapshot(&scope.org_id).await {
        Ok(snapshot) => Some(snapshot),
        Err(DirectoryError::NotFound) => None,
        Err(other) => return Err(directory(other)),
    };
    // CF12: one batch (one D1 query) however many users the archive names.
    let reads: Vec<Statement> = data
        .directory
        .users
        .chunks(90)
        .map(|chunk| {
            let statement = prepare(format!(
                "SELECT user_id FROM user_identity WHERE user_id IN ({})",
                crate::sql::placeholders(chunk.len())
            ));
            chunk.iter().fold(statement, |statement, user| {
                statement.bind(user.id.as_str())
            })
        })
        .collect();
    let mut found = BTreeSet::new();
    if !reads.is_empty() {
        for outcome in db.batch(reads).await.map_err(|e| store(err(e)))? {
            for row in &outcome.rows {
                found.insert(row.string(0).map_err(|e| store(err(e)))?);
            }
        }
    }
    let known: BTreeSet<UserId> = data
        .directory
        .users
        .iter()
        .filter(|user| found.contains(user.id.as_str()))
        .map(|user| user.id.clone())
        .collect();
    if let Some(commit) = directory_commit(&data.directory, existing.as_ref(), &known, options)? {
        if commit.mutations.iter().any(|m| {
            matches!(
                m,
                runbook_core::directory::DirectoryMutation::CreateProject(_)
            )
        }) {
            plan.project_recorded = Some(
                guard(
                    "import.target_not_empty",
                    "NOT EXISTS (SELECT 1 FROM project WHERE org_id = ? AND project_id = ?)",
                )
                .bind(org)
                .bind(project),
            );
        }
        match directories.plan(&commit).await.map_err(directory)? {
            Planned::Write { statements, .. } => plan.control.extend(statements),
            Planned::Conflict => return Err(PortableError::Conflict("import.directory_conflict")),
        }
    }

    if let Some(variables) = &data.variables {
        plan.control.push(
            tenant(
                prepare(
                    "INSERT INTO project_variables (org_id, project_id, body, revision) \
                     VALUES (?, ?, ?, ?)",
                ),
                scope,
            )
            .bind(json(&variables.variables).map_err(|e| store(err(e)))?)
            .bind(uint(variables.revision).map_err(|e| store(err(e)))?),
        );
    }
    let mut out = Vec::new();
    let mut source = |text: &str| -> Result<crate::sources::StoredSource, PortableError> {
        if !external_sources {
            return Ok(crate::sources::StoredSource::inline(text));
        }
        let digest = crate::sources::digest(text);
        plan.sources
            .entry(digest)
            .or_insert_with(|| text.as_bytes().to_vec());
        Ok(crate::sources::StoredSource {
            inline: String::new(),
            digest: Some(digest),
            size: Some(
                i64::try_from(text.len())
                    .map_err(|_| store(StoreError::Backend("d1.source_size")))?,
            ),
        })
    };
    let rows = &data.rows;
    for row in &rows.runbooks {
        let draft = source(&row.draft_source)?;
        // UI-1b: imported with its listing, so the first list reads no blob.
        let listing = crate::listing::StoredListing::of(&row.record.slug, &row.draft_source)
            .map_err(store)?;
        let statement = tenant(
            prepare(format!(
                "INSERT INTO runbook (org_id, project_id, runbook_id, slug, origin, archived, \
                 draft_source, draft_revision, draft_digest, draft_size, {}) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                crate::listing::LISTING_COLUMNS
            )),
            scope,
        )
        .bind(row.record.runbook_id.as_str())
        .bind(row.record.slug.as_str())
        .bind(origin_label(row.record.origin))
        .bind(i64::from(row.record.archived))
        .bind(draft.inline.as_bytes().to_vec())
        .bind(uint(row.draft_revision).map_err(|e| store(err(e)))?)
        .bind(draft.digest_value())
        .bind(draft.size);
        out.push(listing.bind(statement, row.draft_revision).map_err(store)?);
    }
    for row in &rows.versions {
        let v = &row.version;
        let text = source(&v.source)?;
        out.push(
            tenant(
                prepare(format!(
                    "INSERT INTO runbook_version (org_id, project_id, runbook_id, {VERSION_COLUMNS}, \
                     source_digest, source_size) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                )),
                scope,
            )
            .bind(row.runbook_id.as_str())
            .bind(v.version_id.as_str())
            .bind(v.number.map(std::num::NonZeroU32::get))
            .bind(v.branch.clone())
            .bind(v.commit_sha.clone())
            .bind(v.semantic_hash.to_vec())
            .bind(v.snapshot_digest.to_vec())
            .bind(uint(v.snapshot_size).map_err(|e| store(err(e)))?)
            .bind(text.inline.as_str())
            .bind(json(&v.pins).map_err(|e| store(err(e)))?)
            .bind(text.digest_value())
            .bind(text.size),
        );
    }
    for draft in &rows.drafts {
        let text = source(&draft.source)?;
        out.push(
            tenant(
                prepare(
                    "INSERT INTO draft (org_id, project_id, key, revision, source, source_digest, \
                     source_size) VALUES (?, ?, ?, ?, ?, ?, ?)",
                ),
                scope,
            )
            .bind(draft.key.as_str())
            .bind(uint(draft.revision).map_err(|e| store(err(e)))?)
            .bind(text.inline.as_str())
            .bind(text.digest_value())
            .bind(text.size),
        );
    }
    // Recording order carries meaning (the latest version of a name wins).
    for (seq, skill) in rows.skills.iter().enumerate() {
        out.push(
            tenant(
                prepare(
                    "INSERT INTO skill_version (org_id, project_id, name, description, hash, \
                     manifest, provenance, seq) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                ),
                scope,
            )
            .bind(skill.name.as_str())
            .bind(skill.description.as_str())
            .bind(skill.hash.to_vec())
            .bind(json(&skill.manifest).map_err(|e| store(err(e)))?)
            .bind(json(&skill.provenance).map_err(|e| store(err(e)))?)
            .bind(uint(seq as u64 + 1).map_err(|e| store(err(e)))?),
        );
    }
    for pool in &rows.pools {
        out.push(
            tenant(
                prepare(
                    "INSERT INTO admission_pool (org_id, project_id, key, concurrency_limit, \
                     active) VALUES (?, ?, ?, ?, ?)",
                ),
                scope,
            )
            .bind(pool.key.as_str())
            .bind(i64::from(pool.limit))
            .bind(i64::from(pool.active)),
        );
    }
    let mut queued = std::collections::BTreeMap::new();
    for unit in &rows.runs {
        let p = &unit.slice.run.projection;
        let id = p.execution_id.as_str();
        out.push(
            prepare(
                "INSERT INTO execution_owner (execution_id, org_id, project_id) VALUES (?, ?, ?)",
            )
            .bind(id)
            .bind(org)
            .bind(project),
        );
        // Gate fix D2: the run's object is installed held; this row says it
        // is to be released. Gate round 2: it commits with the control
        // batch, the import's last, so it exists only for a fully committed
        // import (a half-committed one is never released).
        plan.control.push(
            prepare(
                "INSERT INTO run_release (execution_id, org_id, project_id, created_at_ms) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(org)
            .bind(project)
            .bind(db.now_ms()),
        );
        for step in &p.steps {
            out.push(
                prepare(
                    "INSERT INTO step_run_index (step_run_id, execution_id, org_id, project_id) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(step.step_run_id.as_str())
                .bind(id)
                .bind(org)
                .bind(project),
            );
        }
        let catalog = &unit.slice.catalog;
        if let Some(summary) = &catalog.summary {
            out.push(summary_statement(summary).map_err(store)?);
        }
        for (ordinal, row) in catalog.assignments.iter().enumerate() {
            out.push(assignment_statement(id, ordinal, row).map_err(store)?);
        }
        if let Some((request, admission)) = &catalog.admission {
            if admission.state == AdmissionState::Queued {
                queued.insert(p.execution_id.clone(), (request, admission));
            } else {
                out.push(admission_statement(scope, request, admission, None)?);
            }
        }
        for row in &unit.start_replays {
            let replay = &row.replay;
            out.push(
                tenant(
                    prepare(
                        "INSERT INTO start_replay (org_id, project_id, idempotency_key, \
                         principal, request_hash, execution_id, outcome, created_at_ms) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    ),
                    scope,
                )
                .bind(replay.key.as_str())
                .bind(replay.principal.as_str())
                .bind(replay.request_hash.to_vec())
                .bind(replay.outcome.execution_id.as_str())
                .bind(json(&replay.outcome).map_err(|e| store(err(e)))?)
                .bind(row.created_at_ms),
            );
        }
    }
    // Queued admissions take their archived FIFO positions.
    for pool in &rows.pools {
        for (position, id) in pool.queue.iter().enumerate() {
            let (request, admission) = queued.remove(id).ok_or(PortableError::Archive {
                code: "archive.invalid_admission",
                path: None,
            })?;
            out.push(admission_statement(
                scope,
                request,
                admission,
                Some(position as u64 + 1),
            )?);
        }
    }
    if !queued.is_empty() {
        return Err(PortableError::Archive {
            code: "archive.invalid_admission",
            path: None,
        });
    }
    for tombstone in &rows.tombstones {
        out.push(
            prepare(
                "INSERT INTO tombstone (execution_id, org_id, project_id, body) VALUES (?, ?, ?, ?)",
            )
            .bind(tombstone.execution_id.as_str())
            .bind(tombstone.scope.org_id.as_str())
            .bind(tombstone.scope.project_id.as_str())
            .bind(json(tombstone).map_err(|e| store(err(e)))?),
        );
    }
    for row in &rows.memory {
        out.extend(crate::memory::import_statements(row).map_err(store)?);
    }
    for row in &data.webhooks {
        out.push(
            tenant(
                prepare(
                    "INSERT INTO webhook_subscription (subscription_id, org_id, project_id, \
                     created_at_ms, body) VALUES (?, ?, ?, ?, ?)",
                )
                .bind(row.id.as_str()),
                scope,
            )
            .bind(row.created_at_ms)
            .bind(json(&row.to_subscription(scope)).map_err(|e| store(err(e)))?),
        );
    }
    plan.shard = out;
    Ok(plan)
}

fn admission_statement(
    scope: &TenantScope,
    request: &runbook_core::catalog::AdmitRequest,
    admission: &runbook_core::catalog::Admission,
    queue_order: Option<u64>,
) -> Result<Statement, PortableError> {
    Ok(prepare(
        "INSERT INTO admission (execution_id, org_id, project_id, key, state, queue_order, \
         request, admitted_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(admission.execution_id.as_str())
    .bind(scope.org_id.as_str())
    .bind(scope.project_id.as_str())
    .bind(request.key.as_str())
    .bind(crate::admission::state_label(admission.state))
    .bind(
        queue_order
            .map(uint)
            .transpose()
            .map_err(|e| store(err(e)))?,
    )
    .bind(json(request).map_err(|e| store(err(e)))?)
    .bind(admission.admitted_at_ms))
}

/// Gate fix D2: runs whose import committed before `before_ms` and whose
/// objects are still to be released, oldest first.
pub async fn pending_releases<D: D1>(
    db: &D,
    before_ms: i64,
    limit: usize,
) -> Result<Vec<(TenantScope, ExecutionId)>, StoreError> {
    db.all(
        prepare(
            "SELECT org_id, project_id, execution_id FROM run_release WHERE created_at_ms < ? \
             ORDER BY created_at_ms, execution_id LIMIT ?",
        )
        .bind(before_ms)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX)),
    )
    .await
    .map_err(err)?
    .iter()
    .map(|row| {
        Ok((
            TenantScope {
                org_id: row.parse(0).map_err(err)?,
                project_id: row.parse(1).map_err(err)?,
            },
            row.parse(2).map_err(err)?,
        ))
    })
    .collect()
}

/// Gate fix D2: the run's object was released.
pub async fn release_done<D: D1>(db: &D, id: &ExecutionId) -> Result<(), StoreError> {
    db.run(prepare("DELETE FROM run_release WHERE execution_id = ?").bind(id.as_str()))
        .await
        .map(|_| ())
        .map_err(err)
}

/// CF12: [`release_done`] for many runs as statements for one batch (one
/// D1 query however many runs an import released), each under D1's 100
/// bound parameters.
pub fn releases_done(ids: &[ExecutionId]) -> Vec<Statement> {
    ids.chunks(90)
        .map(|chunk| {
            let statement = prepare(format!(
                "DELETE FROM run_release WHERE execution_id IN ({})",
                crate::sql::placeholders(chunk.len())
            ));
            chunk
                .iter()
                .fold(statement, |statement, id| statement.bind(id.as_str()))
        })
        .collect()
}

/// Gate fix D2: a failed import's per-run rows, for the runs whose objects
/// it installed (only those: an install succeeds only into an empty object,
/// so no other writer owns these IDs). Nothing drains a held run, so these
/// are normally already absent; the undo makes sure.
pub fn forget_run_rows(scope: &TenantScope, ids: &[ExecutionId]) -> Vec<Statement> {
    // Gate round 2: in `scope` only. The tables without a tenant column
    // follow the run's owner row, which goes last. CF12 review round 1:
    // 90 runs a statement, so the undo grows by 8 statements per 90 runs.
    let mut out = Vec::new();
    for chunk in ids.chunks(ID_CHUNK) {
        let list = crate::sql::placeholders(chunk.len());
        let bind = |statement: Statement| {
            tenant(
                chunk
                    .iter()
                    .fold(statement, |statement, id| statement.bind(id.as_str())),
                scope,
            )
        };
        for table in ["assignment", "run_summary"] {
            out.push(bind(prepare(format!(
                "DELETE FROM {table} WHERE execution_id IN (SELECT execution_id \
                 FROM execution_owner WHERE execution_id IN ({list}) AND org_id = ? \
                 AND project_id = ?)"
            ))));
        }
        for table in [
            "step_run_index",
            "admission_notice",
            "admission",
            "start_replay",
            "webhook_event",
            "execution_owner",
        ] {
            out.push(bind(prepare(format!(
                "DELETE FROM {table} WHERE execution_id IN ({list}) AND org_id = ? \
                 AND project_id = ?"
            ))));
        }
    }
    out
}

/// Gate round 2: whether `id`'s release is pending (its import committed in
/// full and its object was not released yet).
pub async fn release_pending<D: D1>(control: &D, id: &ExecutionId) -> Result<bool, StoreError> {
    Ok(!control
        .all(prepare("SELECT 1 FROM run_release WHERE execution_id = ?").bind(id.as_str()))
        .await
        .map_err(err)?
        .is_empty())
}

/// Gate round 2: a release the cron could not finish waits its turn again
/// (a minute, doubling, up to an hour), as a purge does.
pub async fn defer_release<D: D1>(
    control: &D,
    id: &ExecutionId,
    now_ms: i64,
) -> Result<(), StoreError> {
    control
        .run(
            prepare(format!(
                "UPDATE run_release SET created_at_ms = ? + {}, attempts = attempts + 1 \
                 WHERE execution_id = ?",
                crate::retention::CRON_BACKOFF_SQL
            ))
            .bind(now_ms)
            .bind(id.as_str()),
        )
        .await
        .map(|_| ())
        .map_err(err)
}
