//! CF8: the D3 portable project archive on Cloudflare, over the composed
//! store: D1 (`runbook_store_d1::portable`), one ExecutionDO per run (the
//! object protocol's `Install`) and R2. The archive format, its validation
//! and canonical encoding are `runbook_core::portable`'s, so an archive
//! exported here imports into Postgres or memory and the other way round.
//!
//! **Export** reads the project's D1 rows, then each run's object (the
//! stored run, actor replays, work leases, start record and claim logs, open
//! runs included) and every object under the project's R2 prefixes except
//! step-log chunks, which travel as claim logs.
//!
//! **Import** has no transaction spanning D1, the objects and R2, so it is
//! ordered around one commit point, the D1 batch:
//! 1. the archive is validated, and the batch planned and its guards checked
//!    (target empty, unused execution IDs, directory) before anything is
//!    written;
//! 2. each run is installed into its (empty) object, and each blob put under
//!    the project's prefix; neither is reachable without D1's owner rows;
//! 3. the D1 batch commits the project.
//!
//! If step 2 or 3 fails, the objects this import installed are purged and
//! the objects it created deleted, so nothing it wrote stays reachable.
//!
//! CF11: when the organization's shard is not the control database, step 3
//! is two batches, the shard's (the project's rows) and then the control
//! database's (directory and variables); if the control batch fails, the
//! shard's rows are deleted again (its guards proved the project held none
//! there) before the objects and blobs are undone.
//!
//! CF12: the import is one request, so it must fit one invocation's Workers
//! Paid limits. Its D1 queries are a constant ([`IMPORT_D1_QUERIES`]: every
//! write is a batch, which D1 counts as one query), and its subrequests grow
//! with the archive ([`import_cost`]): an archive whose worst case does not
//! fit [`ImportBudget::PAID`] is refused (`import.too_large`) before
//! anything is written.
use crate::compose::{CfStore, Objects};
use crate::object::ObjectHandle;
use runbook_core::{
    StoreError,
    blob::BlobKey,
    catalog::AdmissionState,
    directory::{DirectoryError, DirectoryStore},
    portable::{
        self, Archive, BlobRow, ImportOptions, PortableError, ProjectData, RunUnit,
        referenced_teams,
    },
    retention::ExecutionSlice,
};
use runbook_model::ExecutionId;
use runbook_store_d1::{D1, D1Directory};
use runbook_store_do::portable::RunInstall;
use runbook_store_r2::ObjectBucket;

fn store(error: StoreError) -> PortableError {
    PortableError::Store(error)
}

/// CF12 review round 1: the most D1 queries an export makes, whatever the
/// project holds: the run count (for its cost), the variables (on the
/// control database), one batch of the project's rows (runs and memory
/// entries included, project-wide) and the directory snapshot's batch.
pub const EXPORT_D1_QUERIES: u64 = 4;

/// The refusal of an export whose worst case does not fit its budget.
pub const EXPORT_TOO_LARGE: &str = "export.too_large";

/// CF12 review round 1: the most an export of a project spends, known
/// before any run is read: [`EXPORT_D1_QUERIES`], the R2 list calls that
/// found its `keys` objects, one object request for each of its `runs`,
/// and at most one R2 read for each listed object (a blob, a step-log
/// chunk or a runbook source, each read once).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportCost {
    pub subrequests: u64,
    pub d1_queries: u64,
}
pub fn export_cost(runs: usize, keys: usize, list_calls: u64) -> ExportCost {
    ExportCost {
        subrequests: EXPORT_D1_QUERIES + list_calls + runs as u64 + keys as u64,
        d1_queries: EXPORT_D1_QUERIES,
    }
}

/// Export `store`'s project under [`ImportBudget::PAID`].
pub async fn export<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
) -> Result<ProjectData, PortableError> {
    export_within(store, ImportBudget::PAID)
        .await
        .map(|(data, _)| data)
}

/// The project's R2 objects as the export reads them: each once, sources
/// and blobs alike (a runbook source and a snapshot may be the same bytes).
struct Reads<'a, B: ObjectBucket, O: runbook_store_r2::BlobOwners> {
    blobs: &'a runbook_store_r2::R2BlobStore<B, O>,
    read: std::cell::RefCell<std::collections::BTreeMap<BlobKey, Vec<u8>>>,
}
impl<B: ObjectBucket, O: runbook_store_r2::BlobOwners> Reads<'_, B, O> {
    async fn get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        if let Some(bytes) = self.read.borrow().get(key) {
            return Ok(bytes.clone());
        }
        let bytes = self.blobs.export_get(key).await?;
        self.read.borrow_mut().insert(key.clone(), bytes.clone());
        Ok(bytes)
    }
    /// The bytes, read now or before, and forgotten (each is taken once).
    async fn take(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        if let Some(bytes) = self.read.borrow_mut().remove(key) {
            return Ok(bytes);
        }
        self.blobs.export_get(key).await
    }
}
impl<B: ObjectBucket, O: runbook_store_r2::BlobOwners> runbook_store_d1::sources::SourceBlobs
    for Reads<'_, B, O>
{
    fn put(
        &self,
        _digest: [u8; 32],
        _bytes: Vec<u8>,
    ) -> runbook_store_d1::sources::SourceFuture<'_, ()> {
        Box::pin(async { Err(StoreError::Backend("export.read_only")) })
    }
    fn get(
        &self,
        digest: [u8; 32],
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            match Reads::get(self, &BlobKey::Shared { hash: digest }).await {
                Ok(bytes) => Ok(Some(bytes)),
                Err(StoreError::Backend("r2.blob_vanished")) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
}

/// [`export`], refused (`export.too_large`, before any run is read) when
/// its worst case ([`export_cost`]) does not fit `budget`. Returns the
/// project and that worst case.
pub async fn export_within<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    budget: ImportBudget,
) -> Result<(ProjectData, ExportCost), PortableError> {
    let scope = store.scope().clone();
    let (keys, list_calls) = store.blobs().project_listing().await.map_err(store_err)?;
    let runs = runbook_store_d1::portable::run_count(store.d1(), &scope)
        .await
        .map_err(store_err)?;
    let cost = export_cost(runs, keys.len(), list_calls);
    if cost.subrequests > budget.subrequests || cost.d1_queries > budget.d1_queries {
        return Err(PortableError::Archive {
            code: EXPORT_TOO_LARGE,
            path: None,
        });
    }
    let reads = Reads {
        blobs: store.blobs(),
        read: Default::default(),
    };
    let exported =
        runbook_store_d1::portable::export(&store.control(), store.d1(), &scope, Some(&reads))
            .await
            .map_err(store_err)?;
    let mut rows = exported.rows;
    let mut chunks = std::collections::BTreeSet::new();
    for run in exported.runs {
        let id = run.id;
        let object = store.objects().object(&id, &scope).map_err(store_err)?;
        let read = match object.export_run(&id).await {
            Ok(read) => read,
            // An owner row whose start never reached its object (the
            // reconciler abandons it): no run to export, as natively.
            Err(StoreError::NotFound) => continue,
            Err(other) => return Err(store_err(other)),
        };
        // The chunks the object moved to R2, each read once.
        let mut chunk_lines = std::collections::BTreeMap::new();
        for step in &read.logs {
            for chunk in &step.chunks {
                let key = BlobKey::Execution {
                    execution_id: id.clone(),
                    name: runbook_store_do::logs::chunk_name(&step.step_run_id, chunk.chunk),
                };
                let bytes = reads.take(&key).await.map_err(store_err)?;
                let lines: Vec<String> = serde_json::from_slice(&bytes)
                    .map_err(|_| store_err(StoreError::Backend("logs.chunk_invalid")))?;
                chunk_lines.insert((step.step_run_id.clone(), chunk.chunk), lines);
                chunks.insert(key);
            }
        }
        let claim_logs = read
            .claim_logs(|step, chunk| {
                chunk_lines
                    .remove(&(step.clone(), chunk.chunk))
                    .ok_or(StoreError::Backend("logs.chunk_invalid"))
            })
            .map_err(store_err)?;
        let blob_keys = keys
            .iter()
            .filter(|key| {
                matches!(key, BlobKey::Execution { execution_id, .. } if *execution_id == id)
                    && !chunks.contains(*key)
            })
            .cloned()
            .collect();
        rows.runs.push(RunUnit {
            slice: ExecutionSlice {
                scope: scope.clone(),
                run: read.run,
                actor_replays: read.actor_replays,
                catalog: run.catalog,
                work: read.work,
                blob_keys,
            },
            start: read.start,
            start_replays: run.start_replays,
            claim_logs,
        });
    }
    // CF11: runbook sources the catalog keeps in R2 travel as the rows'
    // text, not as blobs (as natively), unless a snapshot or skill file is
    // the same bytes.
    let sources = source_only_digests(&rows);
    for key in &keys {
        if chunks.contains(key) {
            continue;
        }
        if let BlobKey::Shared { hash } = key
            && sources.contains(hash)
        {
            continue;
        }
        let bytes = reads.take(key).await.map_err(store_err)?;
        rows.blobs.push(BlobRow {
            key: key.clone(),
            bytes,
        });
    }
    let snapshot = D1Directory::new(store.control())
        .snapshot(&scope.org_id)
        .await
        .map_err(|e| match e {
            DirectoryError::NotFound => PortableError::Store(StoreError::NotFound),
            other => other.into(),
        })?;
    let directory =
        portable::directory_slice(&snapshot, &scope.project_id, &referenced_teams(&rows))?;
    let mut data = ProjectData {
        scope,
        directory,
        variables: exported.variables,
        rows,
        webhooks: exported.webhooks,
    };
    portable::normalize(&mut data);
    Ok((data, cost))
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    if text.len() != 64 {
        return None;
    }
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The digests of the project's runbook sources that no snapshot or skill
/// file shares.
fn source_only_digests(
    rows: &runbook_core::portable::StoreRows,
) -> std::collections::BTreeSet<[u8; 32]> {
    use runbook_store_d1::sources::digest;
    let mut needed: std::collections::BTreeSet<[u8; 32]> = rows
        .versions
        .iter()
        .map(|v| v.version.snapshot_digest)
        .collect();
    for skill in &rows.skills {
        for entry in &skill.manifest.entries {
            if let Some(bytes) = unhex32(&entry.sha256) {
                needed.insert(bytes);
            }
        }
    }
    rows.runbooks
        .iter()
        .map(|r| digest(&r.draft_source))
        .chain(rows.versions.iter().map(|v| digest(&v.version.source)))
        .chain(rows.drafts.iter().map(|d| digest(&d.source)))
        .filter(|d| !needed.contains(d))
        .collect()
}

fn store_err(error: StoreError) -> PortableError {
    store(error)
}

/// What an import wrote before its commit point, to undo on failure.
#[derive(Default)]
struct Written {
    objects: Vec<ExecutionId>,
    blobs: Vec<BlobKey>,
    /// Whether the commit landed is unknown: undo nothing (CF12 review
    /// round 1).
    unknown: bool,
}

/// A D1 batch error as the import reports it.
fn batch_error(error: StoreError) -> PortableError {
    match error {
        StoreError::InvalidBatch("import.target_not_empty") => PortableError::TargetNotEmpty,
        StoreError::InvalidBatch(code @ ("import.project_conflict" | "import.id_conflict")) => {
            PortableError::Conflict(code)
        }
        StoreError::InvalidBatch("memory.duplicate")
        | StoreError::Backend("d1.unique_violation") => {
            PortableError::Conflict("import.id_conflict")
        }
        StoreError::InvalidBatch("conflict") => {
            PortableError::Conflict("import.directory_conflict")
        }
        other => PortableError::Store(other),
    }
}

/// CF12: the most D1 queries (calls: a batch is one) an import makes,
/// whatever the archive holds:
/// - planning (`import_statements`): the directory snapshot, the users it
///   already knows, the directory plan (3);
/// - the guards, then the commit: one batch each, or two when the
///   organization has its own shard (4);
/// - CF12 review round 1: after a commit batch fails without saying why
///   (its reply lost), its guards again, to read back whether it landed
///   (2 at most: one per commit batch);
/// - after a failure, the shard's rows taken back (when its batch had
///   committed) and the installed runs' rows (2); after the commit, the
///   release rows, which a failure never reaches.
pub const IMPORT_D1_QUERIES: u64 = 11;

/// CF12 review round 1: the most statements one D1 batch of an import may
/// hold. D1 holds a whole batch to its 30-second query limit; at this
/// bound a statement could take 3 ms (local SQLite takes microseconds for
/// these inserts and indexed checks) and the batch would still fit.
pub const MAX_BATCH_STATEMENTS: u64 = 10_000;

/// CF12: what one import's request may spend. Workers Paid allows 10,000
/// subrequests per invocation by default and 1,000 D1 queries;
/// [`ImportBudget::ROUTE_RESERVE`] of each is left for the operator route's
/// own calls (the schema check on a cold isolate, one per D1 binding).
///
/// The figures are compiled in: a Worker cannot read its own
/// `limits.subrequests` setting, so raising that setting in
/// `wrangler.jsonc` does not raise [`ImportBudget::PAID`]; that needs a
/// code change (CF12 review round 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportBudget {
    pub subrequests: u64,
    pub d1_queries: u64,
    /// The most statements in one D1 batch ([`MAX_BATCH_STATEMENTS`]).
    pub batch_statements: u64,
}
impl ImportBudget {
    pub const ROUTE_RESERVE: u64 = 100;
    /// The Workers Paid defaults, less the route's reserve.
    pub const PAID: Self = Self {
        subrequests: 10_000 - Self::ROUTE_RESERVE,
        d1_queries: 1_000 - Self::ROUTE_RESERVE,
        batch_statements: MAX_BATCH_STATEMENTS,
    };
}

/// CF12: the most an import of `data` (planned as `plan`) spends. Every
/// subrequest is a D1 query, an R2 operation or an object request. The
/// worst case is a failed commit:
/// - each run: its install, then its release or (undone) its purge;
/// - each blob and each runbook source (`plan.sources`): its conditional
///   put, then the read that compares an existing object's bytes, or
///   (undone) its delete;
/// - [`IMPORT_D1_QUERIES`].
///
/// `statements` is the largest D1 batch it may send (CF12 review round 1):
/// the commit as one batch (guards, control rows and shard rows), or the
/// undo of its runs' rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportCost {
    pub subrequests: u64,
    pub d1_queries: u64,
    pub statements: u64,
    /// The commit's bound values and their bytes (texts and BLOBs), which
    /// the D1 binding copies into JavaScript (the memory test counts them).
    pub bound_values: u64,
    pub bound_bytes: u64,
}
pub fn import_cost(
    data: &ProjectData,
    plan: &runbook_store_d1::portable::ImportPlan,
) -> ImportCost {
    let items = data.rows.runs.len() + data.rows.blobs.len() + plan.sources.len();
    let ids: Vec<ExecutionId> = data
        .rows
        .runs
        .iter()
        .map(|r| r.slice.run.projection.execution_id.clone())
        .collect();
    let batch = || {
        plan.control_guards
            .iter()
            .chain(&plan.shard_guards)
            .chain(&plan.control)
            .chain(&plan.shard)
    };
    let commit = batch().count();
    let bound_values = batch().map(|s| s.params.len() as u64).sum();
    let bound_bytes = batch()
        .flat_map(|s| &s.params)
        .map(|value| match value {
            runbook_store_d1::Value::Text(text) => text.len() as u64,
            runbook_store_d1::Value::Blob(bytes) => bytes.len() as u64,
            runbook_store_d1::Value::Integer(_) | runbook_store_d1::Value::Null => 8,
        })
        .sum();
    let undo = runbook_store_d1::portable::forget_run_rows(&data.scope, &ids)
        .len()
        .max(runbook_store_d1::portable::SCOPED_TABLES.len());
    ImportCost {
        subrequests: 2 * items as u64 + IMPORT_D1_QUERIES,
        d1_queries: IMPORT_D1_QUERIES,
        statements: commit.max(undo) as u64,
        bound_values,
        bound_bytes,
    }
}

/// The most runs, blobs and runbook sources together that one import under
/// [`ImportBudget::PAID`] can hold (two subrequests each at worst). The
/// statement bound may refuse fewer: each run, step, assignment and
/// tombstone is a statement of the commit batch (the test fixture's runs
/// take five each, so about 2,000 of them fit).
pub const MAX_IMPORT_ITEMS: u64 = (ImportBudget::PAID.subrequests - IMPORT_D1_QUERIES) / 2;

/// The refusal of an archive whose import does not fit its budget.
pub const TOO_LARGE: &str = "import.too_large";

/// Import `data` into `store`'s project (which must be the archive's).
/// `now_ms` anchors the activation of an unstarted, unqueued run with no
/// admission time.
pub async fn import<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    data: &ProjectData,
    options: ImportOptions,
    now_ms: i64,
) -> Result<(), PortableError> {
    import_within(store, data, options, now_ms, ImportBudget::PAID)
        .await
        .map(|_| ())
}

/// [`import`], refused (`import.too_large`, nothing written) when its worst
/// case ([`import_cost`]) does not fit `budget`. Returns that worst case.
pub async fn import_within<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    data: &ProjectData,
    options: ImportOptions,
    now_ms: i64,
    budget: ImportBudget,
) -> Result<ImportCost, PortableError> {
    import_watched(store, data, options, now_ms, budget, &|| {}).await
}

/// [`import_within`], calling `progress` as it goes (CF12b review round 1).
pub async fn import_watched<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    data: &ProjectData,
    options: ImportOptions,
    now_ms: i64,
    budget: ImportBudget,
    progress: &dyn Fn(),
) -> Result<ImportCost, PortableError> {
    if store.scope() != &data.scope {
        return Err(PortableError::Conflict("import.scope_mismatch"));
    }
    // Untrusted content: validated before anything is read or written,
    // with the bounds of D1 and Durable Object rows (CF12c: native imports
    // a start record past one row; this edition cannot hold it).
    portable::validate(data)?;
    portable::check_row_bounds(data)?;
    progress();
    let plan = runbook_store_d1::portable::import_statements(&store.control(), data, options, true)
        .await?;
    progress();
    let cost = import_cost(data, &plan);
    if cost.subrequests > budget.subrequests
        || cost.d1_queries > budget.d1_queries
        || cost.statements > budget.batch_statements
    {
        return Err(PortableError::Archive {
            code: TOO_LARGE,
            path: None,
        });
    }
    // Check the guards alone first (they write nothing when they hold), so
    // a refused import touches nothing.
    let checked = if store.colocated() {
        store.d1().batch(plan.guards()).await
    } else {
        match store.control().batch(plan.control_guards.clone()).await {
            Ok(_) => {
                progress();
                store.d1().batch(plan.shard_guards.clone()).await
            }
            Err(error) => Err(error),
        }
    };
    progress();
    checked.map_err(|e| batch_error(runbook_store_d1::sql_store_error(e)))?;
    let mut written = Written::default();
    let result = write(store, data, plan, now_ms, &mut written, progress).await;
    if result.is_err() && !written.unknown {
        undo(store, &written, progress).await;
    }
    result.map(|()| cost)
}

async fn write<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    data: &ProjectData,
    plan: runbook_store_d1::portable::ImportPlan,
    now_ms: i64,
    written: &mut Written,
    progress: &dyn Fn(),
) -> Result<(), PortableError> {
    let scope = &data.scope;
    let import_id = format!("{}@{}", scope.project_id.as_str(), store.d1().now_ms());
    for unit in &data.rows.runs {
        let id = unit.slice.run.projection.execution_id.clone();
        let object = store.objects().object(&id, scope).map_err(store_err)?;
        // Gate fix D2: held until the D1 commit point below, stamped with
        // this import (gate round 2). Its D1 guards passed, so a stale hold
        // (a failed import's orphan) may be replaced; a fresh one may be
        // another import's, still running.
        let mut install = run_install(unit, now_ms);
        install.held = true;
        install.import_id = import_id.clone();
        install.replace_held = true;
        let installed = object.install(&id, install).await;
        progress();
        match installed {
            Ok(()) => written.objects.push(id),
            Err(StoreError::InvalidBatch("store.execution_exists")) => {
                return Err(PortableError::Conflict("import.id_conflict"));
            }
            Err(StoreError::InvalidBatch("import.in_progress")) => {
                return Err(PortableError::Conflict("import.in_progress"));
            }
            Err(other) => return Err(store_err(other)),
        }
    }
    for blob in &data.rows.blobs {
        let put = store.blobs().import_put(&blob.key, &blob.bytes).await;
        progress();
        if put.map_err(|e| match e {
            StoreError::InvalidBatch("import.id_conflict") => {
                PortableError::Conflict("import.id_conflict")
            }
            other => store_err(other),
        })? {
            written.blobs.push(blob.key.clone());
        }
    }
    // CF11: the runbook sources the rows name by digest.
    for (hash, bytes) in &plan.sources {
        let key = BlobKey::Shared { hash: *hash };
        let put = store.blobs().import_put(&key, bytes).await;
        progress();
        if put.map_err(store_err)? {
            written.blobs.push(key);
        }
    }
    match commit(store, plan, progress).await {
        Ok(()) => {}
        Err(CommitFailure::Undo(error)) => return Err(error),
        Err(CommitFailure::Unknown(error)) => {
            written.unknown = true;
            return Err(error);
        }
    }
    // Past the commit point: release each run (its object then settles and
    // publishes). One that fails keeps its `run_release` row for the cron;
    // the others' rows go in one batch (CF12: one D1 query, not one a run).
    let mut released = Vec::with_capacity(written.objects.len());
    for id in &written.objects {
        let ok = release_object(store, id).await.is_ok();
        progress();
        if ok {
            released.push(id.clone());
        }
    }
    if !released.is_empty() {
        let _ = store
            .control()
            .batch(runbook_store_d1::portable::releases_done(&released))
            .await;
        progress();
    }
    written.objects.clear();
    Ok(())
}

/// The run as an import installs it (not held).
pub fn run_install(unit: &RunUnit, now_ms: i64) -> RunInstall {
    let admission = unit.slice.catalog.admission.as_ref();
    let queued = admission.is_some_and(|(_, a)| a.state == AdmissionState::Queued);
    let admitted_at = admission.and_then(|(_, a)| a.admitted_at_ms);
    RunInstall {
        run: unit.slice.run.clone(),
        actor_replays: unit.slice.actor_replays.clone(),
        work: unit.slice.work.clone(),
        start: unit.start.clone(),
        claim_logs: unit.claim_logs.clone(),
        activation_at_ms: (!queued).then(|| admitted_at.unwrap_or(now_ms)),
        held: false,
        import_id: String::new(),
        replace_held: false,
    }
}

/// Gate fix D2: release a run an import installed held, then forget its
/// `run_release` row (the import right after its commit, or the minute
/// cron for an import that stopped first).
pub async fn release_run<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    release_object(store, id).await?;
    // Gate round 2: the release rows are the control database's.
    runbook_store_d1::portable::release_done(&store.control(), id).await
}

/// Release the run's object (one object request).
async fn release_object<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    match store.objects().object(id, store.scope())?.release(id).await {
        // An object purged since (the run deleted) has nothing to release.
        Ok(_) | Err(StoreError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Gate round 2: a held run's expiry (its object's alarm, an hour after the
/// hold). A run D1 committed (it has an owner row) was imported but never
/// released: release it. One D1 never committed is a failed import's
/// orphan: purge it, so it neither lingers nor blocks a retry.
pub async fn expire_hold<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    id: &ExecutionId,
) -> Result<(), StoreError> {
    // Gate round 2: a pending release (the control batch committed) is
    // released; an owner row without one is a half-committed import, left
    // held for an operator rather than released or purged.
    if runbook_store_d1::portable::release_pending(&store.control(), id).await? {
        return release_run(store, id).await;
    }
    if runbook_store_d1::catalog::execution_owner(store.d1(), id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let object = store.objects().object(id, store.scope())?;
    match object.release(id).await {
        // Held: it is the orphan this expiry is for (releasing it is not
        // a commit: the purge follows at once).
        Ok(true) => object.purge(id).await.map(|_| ()),
        Ok(false) | Err(StoreError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether a commit batch that failed with `error` may have landed: D1
/// answers a guard, constraint or decoding failure only after rolling the
/// batch back; anything else (a lost reply, a timeout) says nothing.
fn ambiguous(error: &runbook_store_d1::D1Error) -> bool {
    matches!(error, runbook_store_d1::D1Error::Other(_))
}

/// CF12 review round 1: whether a commit batch whose reply was lost
/// landed, read back by running its guards again: they held before it, and
/// its rows make the project non-empty (CF12b: or, for a control batch of
/// directory rows only, the directory now records the project it created).
/// `None` when that read fails too.
async fn landed<D: D1>(db: &D, guards: Vec<runbook_store_d1::Statement>) -> Option<bool> {
    match db.batch(guards).await {
        Ok(_) => Some(false),
        Err(runbook_store_d1::D1Error::Guard(code)) if code == "import.target_not_empty" => {
            Some(true)
        }
        Err(_) => None,
    }
}

/// How a commit ended short of success.
enum CommitFailure {
    /// Nothing of it stands: undo what the import wrote.
    Undo(PortableError),
    /// Whether it landed is unknown: leave the held runs to their expiry
    /// (`expire_hold` releases a run D1 committed and purges an orphan).
    Unknown(PortableError),
}

/// The D1 commit point: one batch, or the shard's then the control's (CF11).
/// A batch whose reply is lost is read back before anything is undone
/// (CF12 review round 1).
async fn commit<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    plan: runbook_store_d1::portable::ImportPlan,
    progress: &dyn Fn(),
) -> Result<(), CommitFailure> {
    let failed = |e| batch_error(runbook_store_d1::sql_store_error(e));
    let unknown = || {
        CommitFailure::Unknown(PortableError::Store(StoreError::Backend(
            "import.commit_unknown",
        )))
    };
    if store.colocated() {
        let guards = plan.readback();
        let committed = store.d1().batch(plan.one_batch()).await;
        progress();
        return match committed {
            Ok(_) => Ok(()),
            Err(error) if ambiguous(&error) => match landed(store.d1(), guards).await {
                Some(true) => Ok(()),
                Some(false) => Err(CommitFailure::Undo(failed(error))),
                None => Err(unknown()),
            },
            Err(error) => Err(CommitFailure::Undo(failed(error))),
        };
    }
    // CF12b: with the directory's record of the project, so a batch that
    // wrote only directory rows reads back as landed too.
    let control_guards = plan.control_readback();
    let shard_guards = plan.shard_guards.clone();
    let mut shard = plan.shard_guards;
    shard.extend(plan.shard);
    let committed = store.d1().batch(shard).await;
    progress();
    match committed {
        Ok(_) => {}
        Err(error) if ambiguous(&error) => match landed(store.d1(), shard_guards).await {
            Some(true) => {}
            Some(false) => return Err(CommitFailure::Undo(failed(error))),
            None => return Err(unknown()),
        },
        Err(error) => return Err(CommitFailure::Undo(failed(error))),
    }
    let mut control = plan.control_guards;
    control.extend(plan.control);
    let committed = store.control().batch(control).await;
    progress();
    let error = match committed {
        Ok(_) => return Ok(()),
        Err(error) if ambiguous(&error) => match landed(&store.control(), control_guards).await {
            Some(true) => return Ok(()),
            Some(false) => error,
            None => return Err(unknown()),
        },
        Err(error) => error,
    };
    let _ = store
        .d1()
        .batch(runbook_store_d1::portable::forget_shard_rows(store.scope()))
        .await;
    Err(CommitFailure::Undo(failed(error)))
}

/// Best effort: the import already failed; what cannot be undone now is
/// unreachable (no owner rows) and the next import of the project refuses
/// to overwrite it. Gate fix D2: the runs it installed were held, so none
/// drained into D1; any per-run row of theirs is deleted all the same.
async fn undo<D: D1, O: Objects, B: ObjectBucket>(
    store: &CfStore<D, O, B>,
    written: &Written,
    progress: &dyn Fn(),
) {
    for id in &written.objects {
        if let Ok(object) = store.objects().object(id, store.scope()) {
            let _ = object.purge(id).await;
            progress();
        }
    }
    if !written.objects.is_empty() {
        let _ = store
            .d1()
            .batch(runbook_store_d1::portable::forget_run_rows(
                store.scope(),
                &written.objects,
            ))
            .await;
    }
    for key in &written.blobs {
        let _ = store.blobs().import_delete(key).await;
        progress();
    }
}

/// Encode `data` as the archive's files in one ZIP (the operator route's
/// transport), with `created_at_ms` in the manifest.
pub fn to_zip(data: &ProjectData, created_at_ms: i64) -> Result<Vec<u8>, PortableError> {
    let archive = portable::encode(data, created_at_ms)?;
    let entries: Vec<(String, Vec<u8>)> = archive.files.into_iter().collect();
    runbook_core::archives::zip::write(&entries, 0).map_err(store)
}

fn unreadable() -> PortableError {
    PortableError::Archive {
        code: "archive.unreadable",
        path: None,
    }
}

/// Read a project archive from a ZIP of its files, then verify and decode
/// it (`portable::decode`).
pub fn from_zip(bytes: &[u8]) -> Result<ProjectData, PortableError> {
    let archive = read_zip(bytes, runbook_core::archives::zip::MAX_IMPORT_BYTES)?;
    let (_, data) = portable::decode(&archive)?;
    Ok(data)
}

/// [`from_zip`] with at most `budget` bytes of files once inflated (CF12:
/// the operator route's memory bound; more is `import.too_large`). It takes
/// the request's bytes and drops them before decoding, so the bytes, the
/// files and the decoded project are never all in memory at once.
pub fn from_zip_within(bytes: Vec<u8>, budget: u64) -> Result<ProjectData, PortableError> {
    let archive = read_zip(&bytes, budget)?;
    drop(bytes);
    let (_, data) = portable::decode(&archive)?;
    Ok(data)
}

/// The archive's files from its ZIP, at most `budget` bytes once inflated.
fn read_zip(bytes: &[u8], budget: u64) -> Result<Archive, PortableError> {
    let too_large = |e: StoreError| match e {
        StoreError::Backend("archive.too_large") => PortableError::Archive {
            code: TOO_LARGE,
            path: None,
        },
        _ => unreadable(),
    };
    let zip =
        runbook_core::archives::zip::Zip::parse_with_budget(bytes, budget).map_err(too_large)?;
    let mut archive = Archive::default();
    for entry in &zip.entries {
        let content = zip
            .read(&entry.name)
            .map_err(too_large)?
            .ok_or(unreadable())?;
        if archive.files.insert(entry.name.clone(), content).is_some() {
            return Err(PortableError::Archive {
                code: "archive.duplicate_file",
                path: None,
            });
        }
    }
    Ok(archive)
}
