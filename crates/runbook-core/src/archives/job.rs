//! The daily archive and retention job (ARC2; PRD §14, D53–D54). Hosts call
//! [`run_once`] per tenant from their drain loop with the store clock's time;
//! everything it does is idempotent per (runbook, day), so any number of
//! workers, restarts and passes converge on the same files.
//!
//! One pass, in order:
//! 1. **Archive** every (runbook, day) before today that holds a finished
//!    live run no file lists yet ("shortly after midnight UTC").
//! 2. **Rewrite** stale files (an amendment, evidence finalized late, a
//!    single-run deletion). A rewrite keeps the folders of runs retention
//!    already deleted (copied from the old file) and leaves out runs deleted
//!    for any other reason.
//! 3. **Delete live detail** past `live_days`, only for runs listed in a file
//!    that is not stale, through the B2.4 deletion with
//!    [`RETENTION_REASON`]; the store rechecks under the run's lock that the
//!    file is current and holds the run's revision, and refuses otherwise.
//!    The tombstone, the summary row and the evidence objects remain.
//! 4. **Expire files** past `archive_days` (whole files, PRD §14), once none
//!    of their runs is still live; the runs' evidence objects go with them.
//! 5. **Purge summary rows** past `summary_days`. Summary rows outlive their
//!    files (the PRD's Free plan keeps files 30 days and summaries a year).
//!
//! A day whose write fails backs off (one minute doubling to a day) so later
//! days still progress; `JobStats::first_failure` carries a code to log.
//!
//! With no retention variables set nothing is deleted (D54 default pending
//! confirmation): steps 3–5 are skipped and files are kept indefinitely.
use super::{
    ArchiveIndex, ArchiveRecord, ArchivedRun, ArchivedRunSummary, DAY_MS, RETENTION_REASON,
    build::{LiveRun, ManifestEvidence, RunFolder, build_day, live_folder},
    day_of, import, read,
};
use crate::{
    StoreError,
    blob::{BlobKey, BlobStore},
    catalog::{CatalogStore, TenantScope},
    claims::ClaimStore,
    directory::DirectoryStore,
    portable::{
        self, BlobRow, ClaimLog, PoolRow, ProjectData, RunUnit, RunbookRow, StoreRows, VersionRow,
    },
    retention::{RunStore, Tombstone},
    start::StartStore,
    uploads::{EvidenceState, UploadStore},
};
use runbook_model::{ExecutionId, RunbookId, VersionId};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
};

/// Everything the job reads and writes, on one tenant-bound store handle.
pub trait ArchiveStore:
    ArchiveIndex + RunStore + StartStore + CatalogStore + BlobStore + ClaimStore + UploadStore
{
}
impl<T> ArchiveStore for T where
    T: ArchiveIndex + RunStore + StartStore + CatalogStore + BlobStore + ClaimStore + UploadStore
{
}

/// The host renders each run's `report.md` the way its report route does
/// (names, D34 custom report), from live data.
pub trait ReportSource {
    fn report_markdown(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> impl Future<Output = Result<String, StoreError>>;
}

/// D54 retention in days; `None` keeps forever. Self-hosted defaults come
/// from `RUNBOOK_RETENTION_*_DAYS`, with per-project overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub live_days: Option<u32>,
    pub archive_days: Option<u32>,
    pub summary_days: Option<u32>,
}
impl RetentionPolicy {
    /// Files may not expire before the live copies they back up, or a pass
    /// would delete a file and archive the same runs again. So expiring
    /// files needs a live window no longer than theirs; zero days is refused.
    ///
    /// Summary rows must outlive their files: `summary_days` at least
    /// `archive_days`, and none at all while files are kept forever. A
    /// summary purged before its file would leave a run the summary no
    /// longer points at inside a served file.
    pub fn check(&self) -> Result<(), &'static str> {
        if [self.live_days, self.archive_days, self.summary_days].contains(&Some(0)) {
            return Err("config.retention_zero_days");
        }
        if let Some(archive) = self.archive_days
            && self.live_days.is_none_or(|live| live > archive)
        {
            return Err("config.retention_archive_before_live");
        }
        match (self.archive_days, self.summary_days) {
            (Some(archive), Some(summary)) if summary < archive => {
                return Err("config.retention_summary_before_archive");
            }
            (None, Some(_)) => return Err("config.retention_summary_before_archive"),
            _ => {}
        }
        Ok(())
    }
    /// When a file of `day` expires, for the index and the list route.
    pub fn archive_expiry_ms(&self, day: i64) -> Option<i64> {
        self.archive_days
            .map(|days| (day + 1 + i64::from(days)) * DAY_MS)
    }
}

/// What one pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobStats {
    /// Files written or rewritten (including unchanged rewrites of stale files).
    pub written: usize,
    /// Files removed because none of their runs remain.
    pub removed: usize,
    pub live_deleted: usize,
    /// Retention deletions the store refused because the run's file was not
    /// current any more (a racing amendment); the next pass rewrites first.
    pub live_skipped: usize,
    /// LANG4: finished children kept because their parent still awaits
    /// their result (the `call-parent` item delivers it shortly; a parent
    /// that never takes it can be cancelled).
    pub results_pending: usize,
    /// LANG4: closed parents kept until they have cancelled a child still
    /// running (the `call-cancel` item runs shortly).
    pub cancels_pending: usize,
    pub archives_expired: usize,
    pub summaries_purged: usize,
    /// Leftover file versions swept after the grace period.
    pub versions_swept: usize,
    /// Steps that failed; they are retried on a later pass (days back off).
    pub failed: usize,
    /// The first failure's code, for a warning that names no tenant data.
    pub first_failure: Option<&'static str>,
}
impl JobStats {
    pub fn busy(&self) -> bool {
        self.written
            + self.removed
            + self.live_deleted
            + self.archives_expired
            + self.summaries_purged
            + self.versions_swept
            > 0
    }
    fn fail(&mut self, error: &StoreError) {
        self.failed += 1;
        self.first_failure.get_or_insert(match error {
            StoreError::NotFound => "store.not_found",
            StoreError::InvalidBatch(code) | StoreError::Backend(code) => code,
        });
    }
}

/// What writing one day did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DayOutcome {
    Written(ArchiveRecord),
    /// The index already named a file with these exact bytes, not stale.
    Unchanged,
    /// No run remains for the day; the file and its index row are gone.
    Removed,
}

fn admission_key(unit: &RunUnit) -> Option<(String, u32)> {
    unit.slice
        .catalog
        .admission
        .as_ref()
        .map(|(request, _)| (request.key.clone(), request.limit))
}

/// Read every claim-log line of one run (D33 paging).
pub async fn claim_logs<S: ClaimStore>(
    store: &S,
    unit_steps: &[runbook_model::StepRunId],
    id: &ExecutionId,
) -> Result<Vec<ClaimLog>, StoreError> {
    const PAGE: usize = 1000;
    let mut logs = Vec::new();
    for step_run_id in unit_steps {
        let mut lines = Vec::new();
        let mut after = 0;
        loop {
            let page = store.read_logs(id, step_run_id, after, PAGE).await?;
            let full = page.len() == PAGE;
            if let Some((seq, _)) = page.last() {
                after = *seq;
            }
            lines.extend(page.into_iter().map(|(_, line)| line));
            if !full {
                break;
            }
        }
        if !lines.is_empty() {
            logs.push(ClaimLog {
                step_run_id: step_run_id.clone(),
                lines,
            });
        }
    }
    Ok(logs)
}

/// Gather one finished live run: its unit, summary, rendered report and
/// finalized evidence. `export_execution` refuses open runs.
pub async fn gather_live<S: ArchiveStore, R: ReportSource>(
    store: &S,
    reports: &R,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<LiveRun, StoreError> {
    let slice = store.export_execution(id).await?;
    let start = store.start_record(id).await?;
    let steps: Vec<_> = slice
        .run
        .projection
        .steps
        .iter()
        .map(|s| s.step_run_id.clone())
        .collect();
    let claim_logs = claim_logs(store, &steps, id).await?;
    let evidence = store
        .list_evidence(scope, id)
        .await?
        .into_iter()
        .filter(|r| r.state == EvidenceState::Finalized)
        .filter_map(|r| {
            let verified = r.verified?;
            Some(ManifestEvidence {
                evd_id: r.id,
                step_run_id: r.step_run_id,
                name: r.name,
                content_type: verified.content_type,
                size: verified.size,
            })
        })
        .collect();
    let projection = &slice.run.projection;
    let catalog = slice.catalog.summary.as_ref();
    let summary = ArchivedRunSummary {
        version_id: projection.version_id.clone(),
        version: start.as_ref().and_then(|s| s.version),
        status: projection.status,
        title: catalog.map(|s| s.title.clone()).unwrap_or_default(),
        created_by: catalog.map(|s| s.created_by.clone()).unwrap_or_default(),
        created_at_ms: catalog.map_or(0, |s| s.created_at_ms),
        completed_at_ms: projection
            .completed_at_ms
            .ok_or(StoreError::InvalidBatch("store.execution_open"))?,
    };
    let report_markdown = reports.report_markdown(scope, id).await?;
    Ok(LiveRun {
        unit: RunUnit {
            slice,
            start,
            start_replays: vec![],
            claim_logs,
        },
        summary,
        report_markdown,
        evidence,
    })
}

/// The portable project context for one day's runs (D3 format, no runs):
/// the directory slice, the runbook with its draft, the versions the runs
/// used with their snapshots, and their concurrency pools (all released).
pub async fn context<S: ArchiveStore, D: DirectoryStore>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    runbook_id: &RunbookId,
    units: &[RunUnit],
) -> Result<ProjectData, StoreError> {
    let record = store
        .read_runbook(runbook_id)
        .await?
        .ok_or(StoreError::NotFound)?;
    let draft = store
        .get_draft(runbook_id)
        .await?
        .ok_or(StoreError::NotFound)?;
    let versions: BTreeSet<&VersionId> = units
        .iter()
        .map(|u| &u.slice.run.projection.version_id)
        .collect();
    let mut rows = StoreRows {
        runbooks: vec![RunbookRow {
            record,
            draft_revision: draft.revision.get(),
            draft_source: draft.source,
        }],
        ..Default::default()
    };
    let mut snapshots = BTreeSet::new();
    for version_id in versions {
        let version = store
            .version_record(runbook_id, version_id)
            .await?
            .ok_or(StoreError::NotFound)?;
        snapshots.insert(version.snapshot_digest);
        rows.versions.push(VersionRow {
            runbook_id: runbook_id.clone(),
            version,
        });
    }
    for hash in snapshots {
        rows.blobs.push(BlobRow {
            key: BlobKey::Shared { hash },
            bytes: store.get(&BlobKey::Shared { hash }).await?,
        });
    }
    let mut pools: BTreeMap<String, u32> = BTreeMap::new();
    for (key, limit) in units.iter().filter_map(admission_key) {
        let entry = pools.entry(key).or_default();
        *entry = (*entry).max(limit);
    }
    rows.pools = pools
        .into_iter()
        .map(|(key, limit)| PoolRow {
            key,
            limit,
            active: 0,
            queue: vec![],
        })
        .collect();
    rows.runs = units.to_vec();
    let teams = portable::referenced_teams(&rows);
    rows.runs.clear();
    let snapshot = directory
        .snapshot(&scope.org_id)
        .await
        .map_err(|_| StoreError::Backend("directory.unavailable"))?;
    let directory = portable::directory_slice(&snapshot, &scope.project_id, &teams)
        .map_err(|_| StoreError::NotFound)?;
    let mut data = ProjectData {
        scope: scope.clone(),
        directory,
        variables: None,
        rows,
        webhooks: vec![],
    };
    portable::normalize(&mut data);
    Ok(data)
}

/// Write (or rewrite) one runbook's file for one day from the live runs
/// that finished that day plus, from the current file, the folders of runs
/// retention has deleted. Same content, same bytes: when the index already
/// names a current file with this digest, nothing is written.
///
/// The swap is atomic: the new bytes go under their own content-addressed
/// key and `record_archive` swings the index to it only if the index still
/// names the file this build read and the bytes are stored. Old versions are
/// deleted only by the store, under the index row's lock, and never a
/// version younger than the one named (another writer's in-flight file). A
/// build that loses (a run deleted meanwhile: `archive.run_gone`; another
/// writer first: `archive.conflict`) starts over from the index; its bytes
/// are a leftover the store sweeps once older than the named version or the
/// grace period.
#[allow(clippy::too_many_arguments)]
pub async fn write_day<S: ArchiveStore, D: DirectoryStore, R: ReportSource>(
    store: &S,
    directory: &D,
    reports: &R,
    scope: &TenantScope,
    runbook_id: &RunbookId,
    day: i64,
    now_ms: i64,
    policy: &RetentionPolicy,
) -> Result<DayOutcome, StoreError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match write_day_once(
            store, directory, reports, scope, runbook_id, day, now_ms, policy,
        )
        .await
        {
            Err(StoreError::InvalidBatch("archive.run_gone" | "archive.conflict"))
                if attempts < 4 =>
            {
                continue;
            }
            other => {
                // Leftovers of earlier interrupted or losing writes; the
                // store keeps any younger in-flight version.
                if other.is_ok() {
                    store.sweep_archive_versions(scope, runbook_id, day).await?;
                }
                return other;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_day_once<S: ArchiveStore, D: DirectoryStore, R: ReportSource>(
    store: &S,
    directory: &D,
    reports: &R,
    scope: &TenantScope,
    runbook_id: &RunbookId,
    day: i64,
    now_ms: i64,
    policy: &RetentionPolicy,
) -> Result<DayOutcome, StoreError> {
    let old = store.get_archive(scope, runbook_id, day).await?;
    let live: Vec<ExecutionId> = store.finished_on(runbook_id, day).await?;
    let mut folders: Vec<RunFolder> = Vec::new();
    let mut units = Vec::new();
    for id in &live {
        let run = gather_live(store, reports, scope, id).await?;
        units.push(run.unit.clone());
        folders.push(live_folder(run)?);
    }
    // A row naming a file that is not stored (an interrupted earlier
    // version of this code, or a lost object): rebuild from the live runs
    // when none of the listed runs exists only in that file; otherwise keep
    // the day stale for an operator and fail with a code.
    let readable = match &old {
        Some(old) => match store.head(&read::record_key(old)).await {
            Ok(_) => true,
            Err(StoreError::NotFound) => {
                for id in store.file_members(scope, runbook_id, day).await? {
                    if store
                        .tombstone(&id)
                        .await?
                        .is_some_and(|t| t.reason == RETENTION_REASON)
                    {
                        store.mark_archive_stale(scope, runbook_id, day).await?;
                        return Err(StoreError::Backend("archive.blob_missing"));
                    }
                }
                false
            }
            Err(e) => return Err(e),
        },
        None => false,
    };
    if let Some(old) = old.as_ref().filter(|_| readable) {
        let manifest = read::manifest(store, old).await?;
        for line in &manifest.runs {
            if live.contains(&line.execution_id) {
                continue;
            }
            // Only retention's own deletion keeps a run in its file; any
            // other deletion (a privacy request) takes it out.
            let kept = matches!(
                store.tombstone(&line.execution_id).await?,
                Some(Tombstone { ref reason, .. }) if reason == RETENTION_REASON
            );
            if !kept {
                continue;
            }
            let folder = read::folder(store, old, line).await?;
            units.push(
                import::unit_of(&folder).map_err(|_| StoreError::Backend("archive.corrupt"))?,
            );
            folders.push(folder);
        }
    }
    if folders.is_empty() {
        if let Some(old) = &old
            && !store
                .delete_archive(scope, runbook_id, day, old.digest)
                .await?
        {
            return Err(StoreError::InvalidBatch("archive.conflict"));
        }
        return Ok(DayOutcome::Removed);
    }
    let data = context(store, directory, scope, runbook_id, &units).await?;
    let (bytes, manifest) = build_day(scope, runbook_id, day, &data, folders)?;
    let digest = portable::sha256(&bytes);
    if readable && old.as_ref().is_some_and(|o| o.digest == digest && !o.stale) {
        return Ok(DayOutcome::Unchanged);
    }
    store
        .put(read::archive_key(runbook_id, day, digest), &bytes)
        .await?;
    let record = ArchiveRecord {
        scope: scope.clone(),
        runbook_id: runbook_id.clone(),
        day,
        run_count: manifest.run_count,
        size_bytes: bytes.len() as u64,
        digest,
        schema_version: manifest.schema_version,
        written_at_ms: now_ms,
        stale: false,
        expires_at_ms: policy.archive_expiry_ms(day),
        stale_marks: old.as_ref().map_or(0, |o| o.stale_marks),
    };
    let runs = manifest
        .runs
        .into_iter()
        .map(|line| ArchivedRun {
            gathered_live: live.contains(&line.execution_id),
            execution_id: line.execution_id,
            runbook_id: runbook_id.clone(),
            day,
            folder: line.folder,
            revision: line.revision,
            summary: line.summary,
            evidence: line.evidence.into_iter().map(|e| e.evd_id).collect(),
        })
        .collect();
    let previous = old.as_ref().map(|o| o.digest);
    store
        .record_archive(scope, record.clone(), runs, previous)
        .await?;
    Ok(DayOutcome::Written(record))
}

/// One pass of the job for one tenant at `now_ms` (the store clock). Each
/// step is bounded by `batch`; a failed day backs off, a failed deletion is
/// retried on a later pass.
pub async fn run_once<S: ArchiveStore, D: DirectoryStore, R: ReportSource>(
    store: &S,
    directory: &D,
    reports: &R,
    scope: &TenantScope,
    now_ms: i64,
    policy: &RetentionPolicy,
    batch: usize,
) -> Result<JobStats, StoreError> {
    let today = day_of(now_ms);
    let mut stats = JobStats::default();
    let mut days: Vec<(RunbookId, i64)> =
        store.unarchived_days(scope, today, now_ms, batch).await?;
    days.extend(
        store
            .stale_archives(scope, now_ms, batch)
            .await?
            .into_iter()
            .map(|r| (r.runbook_id, r.day)),
    );
    for (runbook_id, day) in days {
        match write_day(
            store,
            directory,
            reports,
            scope,
            &runbook_id,
            day,
            now_ms,
            policy,
        )
        .await
        {
            Ok(DayOutcome::Written(_) | DayOutcome::Unchanged) => stats.written += 1,
            Ok(DayOutcome::Removed) => stats.removed += 1,
            Err(error) => {
                stats.fail(&error);
                store
                    .archive_failed(scope, &runbook_id, day, now_ms)
                    .await?;
            }
        }
    }
    if let Some(days) = policy.live_days {
        let cutoff = now_ms - i64::from(days) * DAY_MS;
        // LANG4: a child's result reaches its parent before the child is
        // deleted. Kept children are paged past (the window widens by them,
        // up to `MAX_PENDING_WINDOW` batches) so they never hold up the
        // runs behind them.
        let mut seen = std::collections::BTreeSet::new();
        let mut window = batch;
        loop {
            let ids = store.archived_live_runs(scope, cutoff, window).await?;
            let mut kept = 0;
            for id in &ids {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if result_undelivered(store, id).await? {
                    stats.results_pending += 1;
                    kept += 1;
                    continue;
                }
                if crate::calls::child_cancel_pending(store, id).await? {
                    stats.cancels_pending += 1;
                    kept += 1;
                    continue;
                }
                match store.delete_execution(id, RETENTION_REASON).await {
                    Ok(_) => stats.live_deleted += 1,
                    Err(StoreError::InvalidBatch("archive.not_current")) => stats.live_skipped += 1,
                    Err(error) => stats.fail(&error),
                }
            }
            if kept == 0 || ids.len() < window || window >= batch * MAX_PENDING_WINDOW {
                break;
            }
            window = (window + kept).min(batch * MAX_PENDING_WINDOW);
        }
    }
    if let Some(days) = policy.archive_days {
        for record in store
            .expired_archives(scope, today - i64::from(days), batch)
            .await?
        {
            match expire_file(store, scope, &record).await {
                Ok(true) => stats.archives_expired += 1,
                Ok(false) => {}
                Err(error) => stats.fail(&error),
            }
        }
    }
    if let Some(days) = policy.summary_days {
        stats.summaries_purged += store
            .purge_summaries(scope, now_ms - i64::from(days) * DAY_MS, batch)
            .await?;
    }
    // Leftover versions of any day (a losing write's bytes), once past the
    // grace period, whether or not the day is written again.
    match store.sweep_leftover_versions(scope, batch).await {
        Ok(swept) => stats.versions_swept += swept,
        Err(error) => stats.fail(&error),
    }
    Ok(stats)
}

/// Delete an expired file unless one of its runs is still live (it would
/// only be archived again). An unreadable manifest refuses: the live guard
/// cannot be checked. The store deletes the file, every version of it, and
/// the member runs' retained uploads (by key under each run, PRD §14: they
/// follow the file) in one transaction.
async fn expire_file<S: ArchiveStore>(
    store: &S,
    scope: &TenantScope,
    record: &ArchiveRecord,
) -> Result<bool, StoreError> {
    let manifest = read::manifest(store, record).await?;
    for line in &manifest.runs {
        if store.tombstone(&line.execution_id).await?.is_none() {
            return Ok(false);
        }
    }
    // The store rechecks under the row's lock that the row still names this
    // file and no member run is live.
    match store
        .delete_archive(scope, &record.runbook_id, record.day, record.digest)
        .await
    {
        Err(StoreError::InvalidBatch("archive.member_live" | "archive.conflict")) => Ok(false),
        other => other,
    }
}

/// LANG4: the code a deletion of a child is refused with while its parent
/// still awaits its result (the `call-parent` item delivers it shortly; to
/// delete it at once, cancel the parent first).
pub const RESULT_UNDELIVERED: &str = "execution.call_result_pending";

/// LANG4: the code a deletion of a closed parent is refused with while it
/// still has to cancel a child (its `call-cancel` item runs shortly).
pub const CHILD_CANCEL_PENDING: &str = "execution.call_cancel_pending";

/// What to tell an operator about a LANG4 deletion refusal.
pub fn refusal_message(code: &str) -> Option<&'static str> {
    match code {
        RESULT_UNDELIVERED => Some(
            "This run is a child whose parent still awaits its result, which is \
             delivered within moments. To delete it now, cancel the parent first.",
        ),
        CHILD_CANCEL_PENDING => Some(
            "This run still has to cancel a child it started, which happens within \
             moments. Try again shortly, or cancel the child run first.",
        ),
        _ => None,
    }
}

/// How many batches one retention pass may read to get past children kept
/// for their parents.
const MAX_PENDING_WINDOW: usize = 4;

/// Whether `id` is a child whose parent's call step still awaits its
/// outcome: deleting it now would leave the parent waiting forever.
async fn result_undelivered<S: ArchiveStore>(
    store: &S,
    id: &ExecutionId,
) -> Result<bool, StoreError> {
    crate::calls::result_pending(store, id).await
}

/// Delete one finished run for a reason other than retention (a privacy
/// request or legal requirement, PRD §14). A live run goes through the B2.4
/// deletion; a run retention already reduced to its summary row goes
/// through `purge_retained_run`. Either way the store drops its summary row,
/// marks its file stale and deletes its evidence in the deletion's own
/// transaction, then the file is rewritten without it at once. If that
/// rewrite fails the file is (re)marked stale, so downloads refuse it and the
/// next pass rewrites it.
#[allow(clippy::too_many_arguments)]
pub async fn delete_run<S: ArchiveStore, D: DirectoryStore, R: ReportSource>(
    store: &S,
    directory: &D,
    reports: &R,
    scope: &TenantScope,
    id: &ExecutionId,
    reason: &str,
    now_ms: i64,
    policy: &RetentionPolicy,
) -> Result<Tombstone, StoreError> {
    if reason == RETENTION_REASON || reason.trim().is_empty() {
        return Err(StoreError::InvalidBatch("archive.reserved_reason"));
    }
    if result_undelivered(store, id).await? {
        return Err(StoreError::InvalidBatch(RESULT_UNDELIVERED));
    }
    if crate::calls::child_cancel_pending(store, id).await? {
        return Err(StoreError::InvalidBatch(CHILD_CANCEL_PENDING));
    }
    let located = store.archived_run(scope, id).await?;
    let tombstone = match store.purge_retained_run(scope, id, reason).await? {
        Some(tombstone) => tombstone,
        None => store.delete_execution(id, reason).await?,
    };
    if let Some(run) = located
        && write_day(
            store,
            directory,
            reports,
            scope,
            &run.runbook_id,
            run.day,
            now_ms,
            policy,
        )
        .await
        .is_err()
    {
        // The deletion marked the file stale; make sure it still is, so the
        // job rewrites it (and downloads refuse it) until a rewrite succeeds.
        store
            .mark_archive_stale(scope, &run.runbook_id, run.day)
            .await?;
    }
    Ok(tombstone)
}
