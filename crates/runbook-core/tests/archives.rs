//! ARC1/ARC2 on the memory store: deterministic daily files, range reads of
//! one run's folder, the retention job under a moved store clock, stale
//! rewrites, single-run deletion, tenancy, and import through the D3
//! portable importer.
use runbook_core::{
    ExecutionStore, StoreError,
    archives::{
        ArchiveIndex, ArchiveRecord, DAY_MS, RETENTION_REASON,
        build::{self, DayManifest},
        day_of, import,
        job::{self, DayOutcome, ReportSource, RetentionPolicy},
        read, zip,
    },
    blob::{BlobHead, BlobKey, BlobPrefix, BlobStore},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, memory::MemoryDirectory,
    },
    drain::process_lease,
    memory::MemoryExecutionStore,
    portable::{self, ImportOptions},
    publish::{prepare, publish_draft},
    retention::RunStore,
    start::{StartCommand, StartStore, prepare_start_commit},
    webhooks::memory::MemoryWebhooks,
    work_queue::WorkQueue,
};
use runbook_model::{ActorRef, Data, ExecutionId, ExecutionStatus, RunbookId};
use std::{
    cell::Cell,
    future::Future,
    ops::Range,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory store must be synchronous"),
    }
}
fn scope() -> TenantScope {
    TenantScope {
        org_id: "org_00000000000000000000000001".parse().unwrap(),
        project_id: "prj_00000000000000000000000001".parse().unwrap(),
    }
}
fn other() -> TenantScope {
    TenantScope {
        org_id: "org_00000000000000000000000002".parse().unwrap(),
        project_id: "prj_00000000000000000000000002".parse().unwrap(),
    }
}
fn rb() -> RunbookId {
    "rb_00000000000000000000000001".parse().unwrap()
}
fn exe(n: u8) -> ExecutionId {
    format!("exe_{n:026}").parse().unwrap()
}
const START_MS: i64 = 1_000;
/// Every run's deadline fires here: UTC day 0.
const DONE_MS: i64 = START_MS + 1_500;
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: timed, title: Timed}\n\
    spec:\n  execution: {timeout: 1s}\n  steps: [{id: only, title: Only}]\n";

struct Reports;
impl ReportSource for Reports {
    async fn report_markdown(
        &self,
        _: &TenantScope,
        id: &ExecutionId,
    ) -> Result<String, StoreError> {
        Ok(format!("# Run {id}\n"))
    }
}
/// A host whose report rendering is down: nothing can be archived.
struct NoReports;
impl ReportSource for NoReports {
    async fn report_markdown(
        &self,
        _: &TenantScope,
        _: &ExecutionId,
    ) -> Result<String, StoreError> {
        Err(StoreError::Backend("report.unavailable"))
    }
}

struct World {
    store: MemoryExecutionStore,
    directory: MemoryDirectory,
}

fn world() -> World {
    let store = MemoryExecutionStore::empty(scope(), START_MS);
    let directory = MemoryDirectory::default();
    ready(directory.commit(DirectoryCommit {
        org_id: scope().org_id,
        expected_revision: 0,
        mutations: vec![
            DirectoryMutation::CreateOrganization {
                title: "Org".into(),
            },
            DirectoryMutation::CreateProject(ProjectRecord {
                id: scope().project_id,
                org_id: scope().org_id,
                slug: "api".into(),
                title: "API".into(),
            }),
        ],
    }))
    .unwrap();
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb(),
            slug: "timed".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        SPEC.into(),
    ))
    .unwrap();
    let prepared = prepare(
        "timed",
        SPEC,
        &Default::default(),
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    ready(publish_draft(&store, &store, &rb(), 1, prepared)).unwrap();
    World { store, directory }
}

impl World {
    /// Start run `n` and let its one-second deadline finish it at DONE_MS.
    fn finish(&self, n: u8) -> ExecutionId {
        self.finish_at(n, 0)
    }
    /// The same, `days` later (it finishes on UTC day `days`).
    fn finish_at(&self, n: u8, days: i64) -> ExecutionId {
        let (start, done) = (START_MS + days * DAY_MS, DONE_MS + days * DAY_MS);
        let store = &self.store;
        store.set_time(start);
        let command = StartCommand {
            execution_id: exe(n),
            runbook_id: rb(),
            version: None,
            inputs: Data::new(),
            fields: Data::new(),
            project_vars: Data::new(),
            title: None,
            tags: vec![],
            url: String::new(),
            created_by: "alice".into(),
            limits: Default::default(),
            idempotency_key: format!("k{n}"),
            principal: "user:alice".into(),
            request_hash: [n; 32],
        };
        let commit = ready(prepare_start_commit(store, store, &command)).unwrap();
        ready(store.commit_start(commit)).unwrap();
        for at in [start, done] {
            store.set_time(at);
            for lease in ready(store.claim_due(10, 60_000)).unwrap() {
                ready(process_lease(store, &self.directory, scope(), &lease)).unwrap();
            }
        }
        let run = ready(store.load(&exe(n))).unwrap();
        assert_eq!(run.projection.completed_at_ms, Some(done));
        exe(n)
    }
    /// `finish`, without running any call work item (LANG4 tests).
    fn finish_with(&self, n: u8, _keep: &dyn Fn(&ExecutionId) -> bool) -> ExecutionId {
        let store = &self.store;
        store.set_time(START_MS);
        let command = StartCommand {
            execution_id: exe(n),
            runbook_id: rb(),
            version: None,
            inputs: Data::new(),
            fields: Data::new(),
            project_vars: Data::new(),
            title: None,
            tags: vec![],
            url: String::new(),
            created_by: "alice".into(),
            limits: Default::default(),
            idempotency_key: format!("k{n}"),
            principal: "user:alice".into(),
            request_hash: [n; 32],
        };
        let commit = ready(prepare_start_commit(store, store, &command)).unwrap();
        ready(store.commit_start(commit)).unwrap();
        self.settle_without_calls(START_MS);
        self.settle_without_calls(DONE_MS);
        exe(n)
    }
    fn pass(&self, now_ms: i64, policy: &RetentionPolicy) -> job::JobStats {
        ready(job::run_once(
            &self.store,
            &self.directory,
            &Reports,
            &scope(),
            now_ms,
            policy,
            16,
        ))
        .unwrap()
    }
    fn record(&self, day: i64) -> ArchiveRecord {
        ready(self.store.get_archive(&scope(), &rb(), day))
            .unwrap()
            .expect("the day has a file")
    }
    /// The bytes the index names; they always hash to its digest.
    fn file(&self, day: i64) -> Vec<u8> {
        let record = self.record(day);
        let bytes = ready(self.store.get(&read::record_key(&record))).unwrap();
        assert_eq!(portable::sha256(&bytes), record.digest);
        bytes
    }
    fn manifest(&self, day: i64) -> DayManifest {
        ready(read::manifest(&self.store, &self.record(day))).unwrap()
    }
    /// Every stored version of the day's file.
    fn versions(&self, day: i64) -> Vec<BlobKey> {
        ready(self.store.list_prefix(&BlobPrefix::Archive {
            runbook_id: rb(),
            day,
        }))
        .unwrap()
    }
    fn live(&self, id: &ExecutionId) -> bool {
        ready(self.store.load(id)).is_ok()
    }
}

fn retention(days: u32) -> RetentionPolicy {
    RetentionPolicy {
        live_days: Some(days),
        ..Default::default()
    }
}

#[test]
fn the_same_runs_give_identical_bytes() {
    let a = world();
    let b = world();
    for w in [&a, &b] {
        w.finish(1);
        w.finish(2);
        assert_eq!(w.pass(DAY_MS + 1, &RetentionPolicy::default()).written, 1);
    }
    assert_eq!(a.file(0), b.file(0), "deterministic per D53");
    // Rebuilding from the same rows gives the same bytes again, and a pass
    // over an unchanged day writes nothing.
    let rebuilt = ready(job::write_day(
        &a.store,
        &a.directory,
        &Reports,
        &scope(),
        &rb(),
        0,
        DAY_MS + 5,
        &RetentionPolicy::default(),
    ))
    .unwrap();
    assert_eq!(rebuilt, DayOutcome::Unchanged);
    assert!(!a.pass(DAY_MS + 10, &RetentionPolicy::default()).busy());
    let manifest = a.manifest(0);
    assert_eq!(manifest.date, "1970-01-01");
    assert_eq!(manifest.run_count, 2);
    assert_eq!(manifest.versions.len(), 1);
    let names: Vec<String> = zip::Zip::parse(&a.file(0))
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names[0], "manifest.json");
    for want in [
        format!("{}/report.md", exe(1)),
        format!("{}/execution.json", exe(1)),
        format!("{}/events.ndjson", exe(1)),
        format!("{}/report.md", exe(2)),
        "project/manifest.json".into(),
        "project/runbook_versions.ndjson".into(),
    ] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
}

/// Golden: a fixed input always hashes to this digest, whatever the build.
#[test]
fn golden_digest_is_stable() {
    let entries = vec![
        (
            "manifest.json".to_owned(),
            b"{\"format\":\"runbook-daily-archive\"}\n".to_vec(),
        ),
        (
            "exe_1/report.md".to_owned(),
            b"# Run\n\nAll good.\n".repeat(20),
        ),
        ("exe_1/events.ndjson".to_owned(), b"{\"seq\":1}\n".to_vec()),
    ];
    let bytes = zip::write(&entries, 20_404).unwrap();
    assert_eq!(
        portable::sha256_hex(&bytes),
        GOLDEN,
        "the ZIP writer's bytes changed: deflate, timestamps or headers"
    );
}
const GOLDEN: &str = "b4d45bbdec30447b2837fc2454b0bd4b2957bd97ce57725d50645bf2da23ba4f";

/// Counts reads so a test can prove a folder opens by range.
struct Counting<'a> {
    inner: &'a MemoryExecutionStore,
    whole: Cell<usize>,
    ranged: Cell<u64>,
}
impl BlobStore for Counting<'_> {
    async fn put(&self, key: BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        self.inner.put(key, bytes).await
    }
    async fn get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        self.whole.set(self.whole.get() + 1);
        self.inner.get(key).await
    }
    async fn get_range(&self, key: &BlobKey, range: Range<u64>) -> Result<Vec<u8>, StoreError> {
        self.ranged
            .set(self.ranged.get() + (range.end - range.start));
        self.inner.get_range(key, range).await
    }
    async fn head(&self, key: &BlobKey) -> Result<BlobHead, StoreError> {
        self.inner.head(key).await
    }
    async fn delete(&self, key: &BlobKey) -> Result<(), StoreError> {
        self.inner.delete(key).await
    }
    async fn list_prefix(&self, prefix: &BlobPrefix) -> Result<Vec<BlobKey>, StoreError> {
        self.inner.list_prefix(prefix).await
    }
}

#[test]
fn one_run_opens_through_range_reads_of_its_folder() {
    let w = world();
    for n in 1..=3 {
        w.finish(n);
    }
    w.pass(DAY_MS, &RetentionPolicy::default());
    let size = w.file(0).len() as u64;
    let counting = Counting {
        inner: &w.store,
        whole: Cell::new(0),
        ranged: Cell::new(0),
    };
    let record = w.record(0);
    let manifest = ready(read::manifest(&counting, &record)).unwrap();
    let line = manifest
        .runs
        .iter()
        .find(|r| r.execution_id == exe(2))
        .unwrap();
    let before = counting.ranged.get();
    let folder = ready(read::folder(&counting, &record, line)).unwrap();
    assert_eq!(counting.whole.get(), 0, "never the whole file");
    let folder_bytes = counting.ranged.get() - before;
    assert!(folder_bytes < size, "{folder_bytes} of {size}");
    assert_eq!(
        read::report_markdown(&folder).unwrap(),
        format!("# Run {}\n", exe(2))
    );
    let unit = import::unit_of(&folder).unwrap();
    assert_eq!(
        unit.slice.run.events,
        ready(w.store.load(&exe(2))).unwrap().events
    );
    // The archived-run read goes through the summary row.
    let opened = ready(read::archived_run(&w.store, &scope(), &exe(2)))
        .unwrap()
        .unwrap();
    assert_eq!(opened.run.summary.status, ExecutionStatus::Failed);
    assert_eq!(opened.run.summary.completed_at_ms, DONE_MS);
    assert_eq!(opened.file.unwrap().1, folder);
}

#[test]
fn retention_deletes_live_detail_only_after_the_archive_exists() {
    let w = world();
    let id = w.finish(1);
    // Day 1 archives the run and keeps it live (89 days to go).
    let early = world();
    let early_id = early.finish(1);
    let stats = early.pass(DONE_MS + DAY_MS, &retention(90));
    assert_eq!((stats.written, stats.live_deleted), (1, 0));
    assert!(early.live(&early_id));
    assert_eq!(
        early
            .pass(DONE_MS + 90 * DAY_MS - 1, &retention(90))
            .live_deleted,
        0,
        "still inside the window"
    );
    assert!(early.live(&early_id));
    let later = DONE_MS + 91 * DAY_MS;
    // The archive cannot be written (report rendering is down): the pass
    // past the 90-day window deletes nothing, and the day backs off.
    let stats = ready(job::run_once(
        &w.store,
        &w.directory,
        &NoReports,
        &scope(),
        later,
        &retention(90),
        16,
    ))
    .unwrap();
    assert_eq!((stats.failed, stats.live_deleted), (1, 0));
    assert_eq!(stats.first_failure, Some("report.unavailable"));
    assert!(w.live(&id));
    assert_eq!(
        ready(w.store.get_archive(&scope(), &rb(), 0)).unwrap(),
        None
    );
    assert!(!w.pass(later + 1, &retention(90)).busy(), "backing off");
    // Once it can be written, the same pass archives first, then deletes.
    let stats = w.pass(later + 120_000, &retention(90));
    assert_eq!((stats.written, stats.live_deleted), (1, 1));
    assert!(!w.live(&id));
    let tombstone = ready(w.store.tombstone(&id)).unwrap().unwrap();
    assert_eq!(tombstone.reason, RETENTION_REASON);
    let summary = ready(w.store.archived_run(&scope(), &id)).unwrap().unwrap();
    assert_eq!(summary.day, 0);
    let before = w.file(0);
    assert!(
        ready(read::archived_run(&w.store, &scope(), &id))
            .unwrap()
            .is_some()
    );
    // And the deleted run is not archived again; the file is unchanged.
    assert!(!w.pass(later + DAY_MS, &retention(90)).busy());
    assert_eq!(
        w.file(0),
        before,
        "deleting live detail never touches the file"
    );
}

#[test]
fn with_retention_unset_nothing_is_deleted() {
    let w = world();
    let id = w.finish(1);
    let stats = w.pass(DONE_MS + 1_000 * DAY_MS, &RetentionPolicy::default());
    assert_eq!(stats.written, 1, "archives are still written daily");
    assert_eq!(
        stats.live_deleted + stats.archives_expired + stats.summaries_purged,
        0
    );
    assert!(w.live(&id));
    assert!(
        ready(w.store.get_archive(&scope(), &rb(), 0))
            .unwrap()
            .is_some()
    );
}

#[test]
fn an_amendment_marks_the_day_stale_and_the_next_pass_rewrites_it() {
    let w = world();
    let id = w.finish(1);
    w.pass(DAY_MS, &retention(90));
    let first = ready(w.store.get_archive(&scope(), &rb(), 0))
        .unwrap()
        .unwrap();
    // A commit to the finished run, as an amendment makes.
    let mut run = ready(w.store.load(&id)).unwrap();
    let batch = runbook_core::ExecutionCommit {
        execution_id: id.clone(),
        expected_sequence: run.revision,
        events: vec![runbook_engine::EventEnvelope {
            execution_id: id.clone(),
            seq: run.revision + 1,
            at_ms: DONE_MS + DAY_MS,
            actor: ActorRef::System,
            event: runbook_engine::Event::ManualPauseChanged { paused: false },
        }],
        projection: {
            run.projection.sequence += 1;
            run.projection
        },
        timers: vec![],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![],
    };
    ready(w.store.commit(batch)).unwrap();
    let stale = ready(w.store.get_archive(&scope(), &rb(), 0))
        .unwrap()
        .unwrap();
    assert!(stale.stale);
    // A stale file is never the copy retention relies on.
    assert_eq!(
        ready(w.store.archived_live_runs(&scope(), i64::MAX, 10)).unwrap(),
        Vec::<ExecutionId>::new()
    );
    let stats = w.pass(DAY_MS * 2, &retention(90));
    assert_eq!(stats.written, 1);
    let rewritten = ready(w.store.get_archive(&scope(), &rb(), 0))
        .unwrap()
        .unwrap();
    assert!(!rewritten.stale);
    assert_ne!(rewritten.digest, first.digest);
    let line = w.manifest(0).runs.remove(0);
    let folder = ready(read::folder(&w.store, &w.record(0), &line)).unwrap();
    assert_eq!(line.revision, ready(w.store.load(&id)).unwrap().revision);
    assert_eq!(
        import::unit_of(&folder).unwrap().slice.run.events.len() as u64,
        line.revision
    );
}

#[test]
fn a_rewrite_keeps_runs_retention_deleted_and_drops_a_single_run_deletion() {
    let w = world();
    let (one, two, three) = (w.finish(1), w.finish(2), w.finish(3));
    w.pass(DAY_MS, &RetentionPolicy::default());
    // Retention takes run 1's live copy; run 2 is deleted on request.
    ready(w.store.delete_execution(&one, RETENTION_REASON)).unwrap();
    let tombstone = ready(job::delete_run(
        &w.store,
        &w.directory,
        &Reports,
        &scope(),
        &two,
        "privacy request",
        DAY_MS * 2,
        &RetentionPolicy::default(),
    ))
    .unwrap();
    assert_eq!(tombstone.reason, "privacy request");
    // Rewritten at once: run 1's archived folder survives from the old file.
    let manifest = w.manifest(0);
    let ids: Vec<_> = manifest
        .runs
        .iter()
        .map(|r| r.execution_id.clone())
        .collect();
    assert_eq!(ids, [one.clone(), three.clone()]);
    assert!(
        !ready(w.store.get_archive(&scope(), &rb(), 0))
            .unwrap()
            .unwrap()
            .stale
    );
    let bytes = w.file(0);
    assert!(
        !bytes
            .windows(two.as_str().len())
            .any(|window| window == two.as_str().as_bytes()),
        "the deleted run leaves no trace in the file"
    );
    assert_eq!(ready(w.store.archived_run(&scope(), &two)).unwrap(), None);
    assert!(
        ready(read::archived_run(&w.store, &scope(), &one))
            .unwrap()
            .is_some()
    );
    // Retention's reason is reserved for the job.
    assert_eq!(
        ready(job::delete_run(
            &w.store,
            &w.directory,
            &Reports,
            &scope(),
            &three,
            RETENTION_REASON,
            DAY_MS * 2,
            &RetentionPolicy::default(),
        )),
        Err(StoreError::InvalidBatch("archive.reserved_reason"))
    );
    // Deleting the last live run on request keeps run 1 in the file.
    ready(job::delete_run(
        &w.store,
        &w.directory,
        &Reports,
        &scope(),
        &three,
        "legal requirement",
        DAY_MS * 2,
        &RetentionPolicy::default(),
    ))
    .unwrap();
    let ids: Vec<_> = w
        .manifest(0)
        .runs
        .into_iter()
        .map(|r| r.execution_id)
        .collect();
    assert_eq!(ids, [one]);
}

#[test]
fn archive_files_expire_after_their_window_and_never_before_live() {
    assert_eq!(
        RetentionPolicy {
            live_days: Some(90),
            archive_days: Some(30),
            summary_days: None,
        }
        .check(),
        Err("config.retention_archive_before_live")
    );
    assert_eq!(
        RetentionPolicy {
            archive_days: Some(30),
            ..Default::default()
        }
        .check(),
        Err("config.retention_archive_before_live")
    );
    assert_eq!(retention(0).check(), Err("config.retention_zero_days"));
    let policy = RetentionPolicy {
        live_days: Some(2),
        archive_days: Some(3),
        summary_days: Some(5),
    };
    policy.check().unwrap();
    let w = world();
    let id = w.finish(1);
    w.pass(DAY_MS, &policy);
    assert_eq!(
        ready(w.store.get_archive(&scope(), &rb(), 0))
            .unwrap()
            .unwrap()
            .expires_at_ms,
        Some(4 * DAY_MS)
    );
    w.pass(3 * DAY_MS, &policy);
    assert!(!w.live(&id));
    assert_eq!(w.pass(4 * DAY_MS, &policy).archives_expired, 1);
    assert_eq!(
        ready(w.store.get_archive(&scope(), &rb(), 0)).unwrap(),
        None
    );
    assert!(w.versions(0).is_empty(), "no version of the file is left");
    assert_eq!(
        ready(w.store.tombstone(&id)).unwrap().unwrap().reason,
        RETENTION_REASON
    );
}

#[test]
fn another_tenant_never_sees_an_archive() {
    let w = world();
    let id = w.finish(1);
    w.pass(DAY_MS, &RetentionPolicy::default());
    let theirs = w.store.scoped(other());
    let key = read::record_key(&w.record(0));
    assert_eq!(ready(theirs.get(&key)), Err(StoreError::NotFound));
    assert_eq!(ready(theirs.head(&key)), Err(StoreError::NotFound));
    assert_eq!(
        ready(theirs.get_range(&key, 0..4)),
        Err(StoreError::NotFound)
    );
    assert!(ready(theirs.put(key.clone(), b"PK")).is_err());
    assert!(
        ready(theirs.list_archives(&other(), &rb(), None, None))
            .unwrap()
            .is_empty()
    );
    assert_eq!(ready(theirs.archived_run(&other(), &id)).unwrap(), None);
    assert!(
        ready(read::archived_run(&theirs, &other(), &id))
            .unwrap()
            .is_none()
    );
    assert!(ready(read::manifest(&theirs, &w.record(0))).is_err());
    // Their own job finds nothing of ours to archive or delete.
    let stats = ready(job::run_once(
        &theirs,
        &w.directory,
        &Reports,
        &other(),
        DONE_MS + 100 * DAY_MS,
        &retention(1),
        16,
    ))
    .unwrap();
    assert!(!stats.busy());
    assert!(w.live(&id));
}

#[test]
fn a_daily_archive_imports_through_the_portable_importer() {
    let w = world();
    for n in 1..=2 {
        w.finish(n);
    }
    w.pass(DAY_MS, &RetentionPolicy::default());
    let bytes = w.file(0);
    let (manifest, data) = import::to_project_data(&bytes).unwrap();
    assert_eq!(manifest.run_count, 2);
    assert_eq!(data.rows.runs.len(), 2);
    let target = MemoryExecutionStore::empty(scope(), DONE_MS);
    let directory = MemoryDirectory::default();
    let webhooks = MemoryWebhooks::default();
    portable::memory::import(
        &target,
        &directory,
        &webhooks,
        &data,
        ImportOptions::default(),
    )
    .unwrap();
    for n in 1..=2 {
        let original = ready(w.store.load(&exe(n))).unwrap();
        assert_eq!(ready(target.load(&exe(n))).unwrap(), original);
        assert_eq!(
            ready(target.start_record(&exe(n))).unwrap(),
            ready(w.store.start_record(&exe(n))).unwrap()
        );
    }
    assert!(ready(target.read_runbook(&rb())).unwrap().is_some());
    assert_eq!(day_of(DONE_MS), 0);
    // Untrusted bytes: a tampered entry or an extra file is refused.
    let mut zip = zip::Zip::parse(&bytes).unwrap().entries;
    zip.sort_by_key(|e| e.offset);
    let mut tampered = bytes.clone();
    let report = zip.iter().find(|e| e.name.ends_with("report.md")).unwrap();
    let at = report.offset as usize + 30 + report.name.len() + 1;
    tampered[at] ^= 0x55;
    assert!(import::to_project_data(&tampered).is_err());
    let mut entries: Vec<(String, Vec<u8>)> = zip
        .iter()
        .map(|e| {
            (
                e.name.clone(),
                zip::Zip::parse(&bytes)
                    .unwrap()
                    .read(&e.name)
                    .unwrap()
                    .unwrap(),
            )
        })
        .collect();
    entries.push(("extra.txt".into(), b"x".to_vec()));
    assert!(import::to_project_data(&zip::write(&entries, 0).unwrap()).is_err());
    // A second import of the same runs conflicts (IDs are preserved).
    assert!(
        portable::memory::import(
            &target,
            &directory,
            &webhooks,
            &data,
            ImportOptions::default()
        )
        .is_err()
    );
    let _ = build::PROJECT_DIR;
}

/// Commit one more event to a finished run, as an amendment does.
fn amend(w: &World, id: &ExecutionId, at_ms: i64) {
    let mut run = ready(w.store.load(id)).unwrap();
    let batch = runbook_core::ExecutionCommit {
        execution_id: id.clone(),
        expected_sequence: run.revision,
        events: vec![runbook_engine::EventEnvelope {
            execution_id: id.clone(),
            seq: run.revision + 1,
            at_ms,
            actor: ActorRef::System,
            event: runbook_engine::Event::ManualPauseChanged { paused: false },
        }],
        projection: {
            run.projection.sequence += 1;
            run.projection
        },
        timers: vec![],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![],
        credentials: vec![],
        derived_outbox: vec![],
    };
    ready(w.store.commit(batch)).unwrap();
}

/// Every entry of a stored file, inflated and joined.
fn inflated(bytes: &[u8]) -> String {
    let zip = zip::Zip::parse(bytes).unwrap();
    zip.entries
        .iter()
        .map(|e| String::from_utf8_lossy(&zip.read(&e.name).unwrap().unwrap()).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Finding 2 (review 2026-11-12): a run deleted on request while its day is
/// being rewritten never stays in any stored version of the file.
#[test]
fn a_privacy_deletion_during_a_rewrite_never_stays_in_a_stored_file() {
    let w = world();
    let (one, two, three) = (w.finish(1), w.finish(2), w.finish(3));
    w.pass(DAY_MS, &RetentionPolicy::default());
    amend(&w, &three, DAY_MS + 5);
    assert!(w.record(0).stale);
    // The rewrite gathers run 2 while it is live; the privacy deletion lands
    // just before the new bytes are put.
    let store = w.store.clone();
    let doomed = two.clone();
    w.store.before_next_archive_put(move || {
        ready(store.delete_execution(&doomed, "privacy request")).unwrap();
    });
    let stats = w.pass(2 * DAY_MS, &RetentionPolicy::default());
    assert_eq!((stats.written, stats.failed), (1, 0));
    let ids: Vec<_> = w
        .manifest(0)
        .runs
        .into_iter()
        .map(|r| r.execution_id)
        .collect();
    assert_eq!(ids, [one, three]);
    assert!(!w.record(0).stale);
    let versions = w.versions(0);
    assert_eq!(versions.len(), 1, "no other version of the file remains");
    for key in versions {
        let text = inflated(&ready(w.store.get(&key)).unwrap());
        assert!(!text.contains(two.as_str()), "the deleted run survived");
    }
}

/// Finding 5: two writers of one day interleave put and record; the index
/// always names bytes that are stored and current.
#[test]
fn two_writers_of_one_day_leave_the_index_describing_the_stored_bytes() {
    let w = world();
    let (one, two) = (w.finish(1), w.finish(2));
    w.pass(DAY_MS, &RetentionPolicy::default());
    amend(&w, &two, DAY_MS + 5);
    // Writer A builds; before its put, writer B sees a further amendment and
    // completes a whole rewrite.
    let store = w.store.clone();
    let directory = w.directory.clone();
    let first = one.clone();
    w.store.before_next_archive_put(move || {
        let world = World {
            store: store.clone(),
            directory: directory.clone(),
        };
        amend(&world, &first, DAY_MS + 6);
        let b = ready(job::write_day(
            &store,
            &directory,
            &Reports,
            &scope(),
            &rb(),
            0,
            DAY_MS + 7,
            &RetentionPolicy::default(),
        ))
        .unwrap();
        assert!(matches!(b, job::DayOutcome::Written(_)), "{b:?}");
    });
    let a = ready(job::write_day(
        &w.store,
        &w.directory,
        &Reports,
        &scope(),
        &rb(),
        0,
        DAY_MS + 8,
        &RetentionPolicy::default(),
    ))
    .unwrap();
    // A lost the swap, deleted its bytes, and found B's file current.
    assert_eq!(a, job::DayOutcome::Unchanged);
    let record = w.record(0);
    assert!(!record.stale);
    let _ = w.file(0); // asserts the stored bytes hash to the index digest
    // A's losing bytes are younger than the named file, which is exactly
    // what another writer's in-flight file looks like: the store keeps them
    // until the grace period has passed, then the job's pass removes them
    // (no manual sweep: nothing else revisits a quiet day).
    assert_eq!(w.versions(0).len(), 2);
    let later = DAY_MS + 8 + runbook_core::archives::VERSION_GRACE_MS + 1;
    w.store.set_time(later);
    assert_eq!(w.pass(later, &RetentionPolicy::default()).versions_swept, 1);
    assert_eq!(w.versions(0), [read::record_key(&record)]);
    for line in w.manifest(0).runs {
        assert_eq!(
            line.revision,
            ready(w.store.load(&line.execution_id)).unwrap().revision,
            "the file holds every amendment"
        );
    }
}

fn finalized_upload(w: &World, id: &ExecutionId, n: u8) -> BlobKey {
    use runbook_core::uploads::{
        EvidenceRecord, EvidenceState, UploadOutcome, UploadQuota, UploadStore, VerifiedUpload,
    };
    let run = ready(w.store.load(id)).unwrap();
    let evd: runbook_model::EvidenceId = format!("evd_{n:026}").parse().unwrap();
    let record = EvidenceRecord {
        id: evd.clone(),
        scope: scope(),
        execution_id: id.clone(),
        step_run_id: run.projection.steps[0].step_run_id.clone(),
        name: "shot.png".into(),
        declared_type: "image/png".into(),
        declared_size: 4,
        state: EvidenceState::Pending,
        verified: None,
        created_by: "human:alice".into(),
        created_at_ms: DONE_MS,
        url_expires_at_ms: DONE_MS + 900_000,
        finalized_at_ms: None,
    };
    ready(
        w.store
            .reserve_evidence(&scope(), record, &UploadQuota::default()),
    )
    .unwrap();
    let key = BlobKey::Upload {
        execution_id: id.clone(),
        evd_id: evd.clone(),
    };
    ready(w.store.put(key.clone(), b"\x89PNG")).unwrap();
    ready(w.store.finalize_evidence(
        &scope(),
        &evd,
        UploadOutcome::Verified(VerifiedUpload {
            size: 4,
            content_type: "image/png".into(),
            width: None,
            height: None,
            thumbnail: false,
            thumbnail_bytes: 0,
        }),
        &UploadQuota::default(),
    ))
    .unwrap();
    key
}

/// PRD §14 (decided 2026-11-12): uploaded evidence follows the archive, not
/// the live detail; summary rows outlive their file (finding 4).
#[test]
fn evidence_follows_the_archive_and_summaries_outlive_the_file() {
    let policy = RetentionPolicy {
        live_days: Some(2),
        archive_days: Some(3),
        summary_days: Some(10),
    };
    let w = world();
    let id = w.finish(1);
    let upload = finalized_upload(&w, &id, 1);
    w.pass(DAY_MS, &policy);
    assert_eq!(w.manifest(0).runs[0].evidence.len(), 1);
    // Live detail goes; the object stays, and a bundle still carries it.
    assert_eq!(w.pass(3 * DAY_MS, &policy).live_deleted, 1);
    assert!(!w.live(&id));
    assert_eq!(ready(w.store.get(&upload)).unwrap(), b"\x89PNG");
    let bundle = ready(runbook_core::archives::bundle::bundle(
        &w.store,
        &scope(),
        &rb(),
        0,
        0,
        true,
    ))
    .unwrap();
    let zip = zip::Zip::parse(&bundle).unwrap();
    let path = format!("evidence/{id}/evd_{:026}", 1);
    assert_eq!(zip.read(&path).unwrap().unwrap(), b"\x89PNG");
    // The file expires: its evidence goes with it; the summary row stays.
    assert_eq!(w.pass(4 * DAY_MS, &policy).archives_expired, 1);
    assert_eq!(ready(w.store.get(&upload)), Err(StoreError::NotFound));
    assert!(w.versions(0).is_empty());
    let opened = ready(read::archived_run(&w.store, &scope(), &id))
        .unwrap()
        .expect("the summary row outlives the file");
    assert!(opened.file.is_none());
    assert_eq!(opened.run.summary.completed_at_ms, DONE_MS);
    assert_eq!(w.pass(11 * DAY_MS, &policy).summaries_purged, 1);
    assert_eq!(ready(w.store.archived_run(&scope(), &id)).unwrap(), None);
}

/// UP2 + ARC: a processed image's original is gone and its re-encoded bytes
/// and thumbnail live under derived IDs. A bundle carries the processed
/// bytes, and the file's expiry deletes all three objects.
#[test]
fn processed_images_are_bundled_and_expire_with_the_file() {
    let policy = RetentionPolicy {
        live_days: Some(2),
        archive_days: Some(3),
        summary_days: Some(10),
    };
    let w = world();
    let id = w.finish(1);
    let original = finalized_upload(&w, &id, 1);
    let BlobKey::Upload { evd_id, .. } = &original else {
        unreachable!()
    };
    let derived = |evd: runbook_model::EvidenceId| BlobKey::Upload {
        execution_id: id.clone(),
        evd_id: evd,
    };
    let processed = derived(runbook_core::uploads::processed_id(evd_id));
    let thumbnail = derived(runbook_core::uploads::thumbnail_id(evd_id));
    // What UP2's finalize leaves behind.
    ready(w.store.put(processed.clone(), b"CLEAN")).unwrap();
    ready(w.store.put(thumbnail.clone(), b"THUMB")).unwrap();
    ready(w.store.delete(&original)).unwrap();
    w.pass(DAY_MS, &policy);
    assert_eq!(w.pass(3 * DAY_MS, &policy).live_deleted, 1);
    let bundle = ready(runbook_core::archives::bundle::bundle(
        &w.store,
        &scope(),
        &rb(),
        0,
        0,
        true,
    ))
    .unwrap();
    let zip = zip::Zip::parse(&bundle).unwrap();
    let path = format!("evidence/{id}/evd_{:026}", 1);
    assert_eq!(zip.read(&path).unwrap().unwrap(), b"CLEAN");
    assert_eq!(w.pass(4 * DAY_MS, &policy).archives_expired, 1);
    for key in [&processed, &thumbnail] {
        assert_eq!(ready(w.store.get(key)), Err(StoreError::NotFound));
    }
}

/// Finding 3: a privacy deletion of a run retention already deleted takes
/// it out of its file, with its evidence and summary row.
#[test]
fn a_privacy_deletion_after_retention_takes_the_run_out_of_its_file() {
    let w = world();
    let (one, two) = (w.finish(1), w.finish(2));
    let upload = finalized_upload(&w, &one, 1);
    w.pass(DAY_MS, &retention(1));
    assert_eq!(w.pass(3 * DAY_MS, &retention(1)).live_deleted, 2);
    assert_eq!(ready(w.store.get(&upload)).unwrap(), b"\x89PNG");
    let tombstone = ready(job::delete_run(
        &w.store,
        &w.directory,
        &Reports,
        &scope(),
        &one,
        "privacy request",
        3 * DAY_MS,
        &retention(1),
    ))
    .unwrap();
    assert_eq!(tombstone.reason, "privacy request");
    let ids: Vec<_> = w
        .manifest(0)
        .runs
        .into_iter()
        .map(|r| r.execution_id)
        .collect();
    assert_eq!(ids, [two], "rewritten without it at once");
    assert!(!inflated(&w.file(0)).contains(one.as_str()));
    assert_eq!(w.versions(0).len(), 1);
    assert_eq!(ready(w.store.get(&upload)), Err(StoreError::NotFound));
    assert_eq!(ready(w.store.archived_run(&scope(), &one)).unwrap(), None);
}

/// A day the job cannot write.
struct FailFor(ExecutionId);
impl ReportSource for FailFor {
    async fn report_markdown(
        &self,
        _: &TenantScope,
        id: &ExecutionId,
    ) -> Result<String, StoreError> {
        if id == &self.0 {
            Err(StoreError::Backend("report.unavailable"))
        } else {
            Ok(format!("# Run {id}\n"))
        }
    }
}

/// Finding 7: a permanently failing day backs off instead of blocking the
/// days after it.
#[test]
fn a_failing_day_does_not_block_later_days() {
    let w = world();
    let (bad, good) = (w.finish_at(1, 0), w.finish_at(2, 1));
    let reports = FailFor(bad.clone());
    let pass = |now: i64| {
        ready(job::run_once(
            &w.store,
            &w.directory,
            &reports,
            &scope(),
            now,
            &RetentionPolicy::default(),
            1,
        ))
        .unwrap()
    };
    let now = 2 * DAY_MS + 5;
    let first = pass(now);
    assert_eq!((first.written, first.failed), (0, 1));
    let second = pass(now + 1);
    assert_eq!((second.written, second.failed), (1, 0), "day 1 progresses");
    assert!(
        ready(w.store.archived_run(&scope(), &good))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        ready(w.store.get_archive(&scope(), &rb(), 0)).unwrap(),
        None
    );
}

/// Finding 4: a file whose manifest cannot be read is never expired (its
/// live-run guard cannot be checked).
#[test]
fn an_unreadable_file_is_never_expired() {
    let policy = RetentionPolicy {
        live_days: Some(1),
        archive_days: Some(2),
        summary_days: None,
    };
    let w = world();
    let id = w.finish(1);
    w.pass(DAY_MS, &policy);
    assert_eq!(w.pass(2 * DAY_MS + DONE_MS, &policy).live_deleted, 1);
    // Point the index at bytes that are not a ZIP.
    let mut record = w.record(0);
    let garbage = b"not a zip".to_vec();
    let previous = record.digest;
    record.digest = portable::sha256(&garbage);
    ready(w.store.put(read::record_key(&record), &garbage)).unwrap();
    let mut run = ready(w.store.archived_run(&scope(), &id)).unwrap().unwrap();
    run.gathered_live = false; // carried from the stored folder
    ready(
        w.store
            .record_archive(&scope(), record, vec![run], Some(previous)),
    )
    .unwrap();
    let stats = w.pass(5 * DAY_MS, &policy);
    assert_eq!((stats.archives_expired, stats.failed), (0, 1));
    assert!(
        ready(w.store.get_archive(&scope(), &rb(), 0))
            .unwrap()
            .is_some()
    );
}

/// Re-check finding 2: summary rows must outlive their files, or a privacy
/// deletion after the summary purge could leave the run in a served file.
#[test]
fn policies_keep_summaries_at_least_as_long_as_files() {
    let policy = |live, archive, summary| RetentionPolicy {
        live_days: live,
        archive_days: archive,
        summary_days: summary,
    };
    let refused = Err("config.retention_summary_before_archive");
    assert_eq!(policy(Some(30), Some(365), Some(30)).check(), refused);
    assert_eq!(
        policy(Some(30), None, Some(365)).check(),
        refused,
        "files kept forever"
    );
    assert_eq!(policy(Some(30), Some(30), Some(365)).check(), Ok(()));
    assert_eq!(policy(Some(30), Some(365), Some(365)).check(), Ok(()));
    assert_eq!(policy(Some(30), None, None).check(), Ok(()));
}

/// Re-check finding 5: when `delete_run`'s rewrite fails, the day is left
/// stale (downloads refuse it, the job rewrites it), never silently current.
#[test]
fn a_failed_rewrite_after_a_single_run_deletion_leaves_the_day_stale() {
    let w = world();
    let (one, two) = (w.finish(1), w.finish(2));
    w.pass(DAY_MS, &RetentionPolicy::default());
    ready(job::delete_run(
        &w.store,
        &w.directory,
        &FailFor(two.clone()),
        &scope(),
        &one,
        "privacy request",
        2 * DAY_MS,
        &RetentionPolicy::default(),
    ))
    .unwrap();
    assert!(w.record(0).stale, "the rewrite failed: the day stays stale");
    w.pass(2 * DAY_MS + 1, &RetentionPolicy::default());
    let ids: Vec<_> = w
        .manifest(0)
        .runs
        .into_iter()
        .map(|r| r.execution_id)
        .collect();
    assert_eq!(ids, [two]);
    assert!(!w.record(0).stale);
}

/// An `ArchiveIndex + BlobStore` that counts whole-object reads of uploads.
struct CountingIndex<'a> {
    inner: &'a MemoryExecutionStore,
    upload_gets: Cell<usize>,
}
impl BlobStore for CountingIndex<'_> {
    async fn put(&self, key: BlobKey, bytes: &[u8]) -> Result<(), StoreError> {
        self.inner.put(key, bytes).await
    }
    async fn get(&self, key: &BlobKey) -> Result<Vec<u8>, StoreError> {
        if matches!(key, BlobKey::Upload { .. }) {
            self.upload_gets.set(self.upload_gets.get() + 1);
        }
        self.inner.get(key).await
    }
    async fn get_range(&self, key: &BlobKey, range: Range<u64>) -> Result<Vec<u8>, StoreError> {
        self.inner.get_range(key, range).await
    }
    async fn head(&self, key: &BlobKey) -> Result<BlobHead, StoreError> {
        self.inner.head(key).await
    }
    async fn delete(&self, key: &BlobKey) -> Result<(), StoreError> {
        self.inner.delete(key).await
    }
    async fn list_prefix(&self, prefix: &BlobPrefix) -> Result<Vec<BlobKey>, StoreError> {
        self.inner.list_prefix(prefix).await
    }
}
impl ArchiveIndex for CountingIndex<'_> {
    async fn record_archive(
        &self,
        scope: &TenantScope,
        record: ArchiveRecord,
        runs: Vec<runbook_core::archives::ArchivedRun>,
        expected: Option<[u8; 32]>,
    ) -> Result<(), StoreError> {
        self.inner
            .record_archive(scope, record, runs, expected)
            .await
    }
    async fn mark_archive_stale(
        &self,
        s: &TenantScope,
        r: &RunbookId,
        d: i64,
    ) -> Result<bool, StoreError> {
        self.inner.mark_archive_stale(s, r, d).await
    }
    async fn get_archive(
        &self,
        s: &TenantScope,
        r: &RunbookId,
        d: i64,
    ) -> Result<Option<ArchiveRecord>, StoreError> {
        self.inner.get_archive(s, r, d).await
    }
    async fn list_archives(
        &self,
        s: &TenantScope,
        r: &RunbookId,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.inner.list_archives(s, r, from, to).await
    }
    async fn archived_run(
        &self,
        s: &TenantScope,
        id: &ExecutionId,
    ) -> Result<Option<runbook_core::archives::ArchivedRun>, StoreError> {
        self.inner.archived_run(s, id).await
    }
    async fn stale_archives(
        &self,
        s: &TenantScope,
        now: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.inner.stale_archives(s, now, limit).await
    }
    async fn delete_archive(
        &self,
        s: &TenantScope,
        r: &RunbookId,
        d: i64,
        expected: [u8; 32],
    ) -> Result<bool, StoreError> {
        self.inner.delete_archive(s, r, d, expected).await
    }
    async fn file_members(
        &self,
        s: &TenantScope,
        r: &RunbookId,
        d: i64,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.inner.file_members(s, r, d).await
    }
    async fn sweep_leftover_versions(
        &self,
        s: &TenantScope,
        limit: usize,
    ) -> Result<usize, StoreError> {
        self.inner.sweep_leftover_versions(s, limit).await
    }
    async fn unarchived_days(
        &self,
        s: &TenantScope,
        before: i64,
        now: i64,
        limit: usize,
    ) -> Result<Vec<(RunbookId, i64)>, StoreError> {
        self.inner.unarchived_days(s, before, now, limit).await
    }
    async fn archived_live_runs(
        &self,
        s: &TenantScope,
        before: i64,
        limit: usize,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.inner.archived_live_runs(s, before, limit).await
    }
    async fn expired_archives(
        &self,
        s: &TenantScope,
        before: i64,
        limit: usize,
    ) -> Result<Vec<ArchiveRecord>, StoreError> {
        self.inner.expired_archives(s, before, limit).await
    }
    async fn purge_summaries(
        &self,
        s: &TenantScope,
        before: i64,
        limit: usize,
    ) -> Result<usize, StoreError> {
        self.inner.purge_summaries(s, before, limit).await
    }
    async fn archive_failed(
        &self,
        s: &TenantScope,
        r: &RunbookId,
        d: i64,
        now: i64,
    ) -> Result<u32, StoreError> {
        self.inner.archive_failed(s, r, d, now).await
    }
    async fn sweep_archive_versions(
        &self,
        s: &TenantScope,
        r: &RunbookId,
        d: i64,
    ) -> Result<(), StoreError> {
        self.inner.sweep_archive_versions(s, r, d).await
    }
    async fn purge_retained_run(
        &self,
        s: &TenantScope,
        id: &ExecutionId,
        reason: &str,
    ) -> Result<Option<runbook_core::retention::Tombstone>, StoreError> {
        self.inner.purge_retained_run(s, id, reason).await
    }
}

/// Re-check finding 8: the bundle's size cap is checked with `head` before
/// any evidence object is read.
#[test]
fn the_bundle_cap_is_checked_before_an_object_is_read() {
    let w = world();
    let id = w.finish(1);
    finalized_upload(&w, &id, 1);
    w.pass(DAY_MS, &RetentionPolicy::default());
    let counting = CountingIndex {
        inner: &w.store,
        upload_gets: Cell::new(0),
    };
    let file = w.record(0).size_bytes;
    // Room for the day's file but not for the 4-byte object.
    let refused = ready(runbook_core::archives::bundle::bundle_capped(
        &counting,
        &scope(),
        &rb(),
        0,
        0,
        true,
        file + 3,
    ));
    assert_eq!(
        refused,
        Err(StoreError::InvalidBatch("archive.bundle_too_large"))
    );
    assert_eq!(counting.upload_gets.get(), 0, "never read past the cap");
    let fits = ready(runbook_core::archives::bundle::bundle_capped(
        &counting,
        &scope(),
        &rb(),
        0,
        0,
        true,
        file + 4,
    ));
    assert!(fits.is_ok());
    assert_eq!(counting.upload_gets.get(), 1);
}

/// Re-check 3, finding 1: a privacy deletion's own rewrite lands between the
/// drain's build and its put. The drain's bytes (still holding the deleted
/// run) lose, it rebuilds identical bytes and returns `Unchanged`, and
/// nothing revisits the quiet day. A later pass must still remove them once
/// past the grace period.
#[test]
fn a_losing_writes_leftover_is_swept_on_a_quiet_day() {
    let w = world();
    let (one, two) = (w.finish(1), w.finish(2));
    w.pass(DAY_MS, &RetentionPolicy::default());
    amend(&w, &two, DAY_MS + 5);
    let store = w.store.clone();
    let directory = w.directory.clone();
    let doomed = one.clone();
    // Store time of the writes below.
    w.store.set_time(DAY_MS + 6);
    w.store.before_next_archive_put(move || {
        ready(job::delete_run(
            &store,
            &directory,
            &Reports,
            &scope(),
            &doomed,
            "privacy request",
            DAY_MS + 6,
            &RetentionPolicy::default(),
        ))
        .unwrap();
    });
    let outcome = ready(job::write_day(
        &w.store,
        &w.directory,
        &Reports,
        &scope(),
        &rb(),
        0,
        DAY_MS + 7,
        &RetentionPolicy::default(),
    ))
    .unwrap();
    assert_eq!(outcome, job::DayOutcome::Unchanged);
    let holding = |w: &World| {
        w.versions(0)
            .into_iter()
            .filter(|key| inflated(&ready(w.store.get(key)).unwrap()).contains(one.as_str()))
            .count()
    };
    assert_eq!(
        holding(&w),
        1,
        "the losing bytes still hold the deleted run"
    );
    // A quiet day: passes before the grace period keep them (they could be
    // an in-flight write); the first pass after it removes them.
    let early = DAY_MS + 8;
    w.store.set_time(early);
    w.pass(early, &RetentionPolicy::default());
    assert_eq!(holding(&w), 1);
    let later = DAY_MS + 8 + runbook_core::archives::VERSION_GRACE_MS + 1;
    w.store.set_time(later);
    let stats = w.pass(later, &RetentionPolicy::default());
    assert_eq!(stats.versions_swept, 1);
    assert_eq!(holding(&w), 0, "the deleted run is in no stored object");
    assert_eq!(w.versions(0).len(), 1);
}

// ---- LANG4: a child's result reaches its parent before retention ----

const CALLER: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: caller, title: Caller}\n\
    spec:\n  steps: [{id: c, call: {runbook: timed}}]\n";

impl World {
    /// Process due work at `at_ms`, leaving call work items alone (their
    /// lease is taken and held, as by a worker that has not got to it).
    fn settle_without_calls(&self, at_ms: i64) {
        self.store.set_time(at_ms);
        for _ in 0..10 {
            let leases = ready(self.store.claim_due(64, 60_000)).unwrap();
            let mut worked = false;
            for lease in leases {
                // The child's start runs; its result and cancel items wait.
                let call = matches!(&lease.id.kind,
                    runbook_core::work_queue::WorkKind::Timer { key, .. }
                        if runbook_core::calls::is_call_key(key)
                            && !key.starts_with(runbook_core::calls::START_KEY));
                if call
                    || matches!(
                        lease.id.kind,
                        runbook_core::work_queue::WorkKind::Notification { .. }
                    )
                {
                    continue;
                }
                worked = true;
                ready(process_lease(&self.store, &self.directory, scope(), &lease)).unwrap();
            }
            if !worked {
                return;
            }
        }
    }
}

/// Publish `caller` (it calls `timed`) and start it as run 50 at time 0.
fn start_caller(w: &World) {
    let caller: RunbookId = "rb_00000000000000000000000002".parse().unwrap();
    ready(w.store.create_runbook(
        RunbookRecord {
            runbook_id: caller.clone(),
            slug: "caller".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        CALLER.into(),
    ))
    .unwrap();
    let runbooks = vec![
        ready(w.store.read_runbook(&rb())).unwrap().unwrap(),
        ready(w.store.read_runbook(&caller)).unwrap().unwrap(),
    ];
    let context = runbook_spec::CompileContext {
        runbooks: ready(runbook_core::calls::call_targets(
            &w.store, &runbooks, CALLER,
        ))
        .unwrap(),
        ..Default::default()
    };
    let prepared = prepare(
        "caller",
        CALLER,
        &context,
        "rbv_00000000000000000000000002".parse().unwrap(),
    )
    .unwrap();
    ready(publish_draft(&w.store, &w.store, &caller, 1, prepared)).unwrap();
    // The caller starts at 0; its child (a `timed` run) fails at its 1 s
    // deadline, before every other run; its result item is not run.
    w.store.set_time(0);
    let command = StartCommand {
        execution_id: exe(50),
        runbook_id: caller,
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "caller".into(),
        principal: "user:alice".into(),
        request_hash: [50; 32],
    };
    let commit = ready(prepare_start_commit(&w.store, &w.store, &command)).unwrap();
    ready(w.store.commit_start(commit)).unwrap();
}

/// Re-check 4: a finished child whose parent still awaits its result is
/// kept by retention, and paged past, so it never holds up the runs behind
/// it, even in a batch of one.
#[test]
fn retention_pages_past_a_child_whose_result_is_pending() {
    let w = world();
    start_caller(&w);
    w.settle_without_calls(0);
    let state = ready(runbook_core::calls::stored_state(&w.store, &exe(50)))
        .unwrap()
        .unwrap();
    let step = state.steps.iter().find(|s| s.step_id == "c").unwrap();
    let child = step.call.as_ref().unwrap().child_execution_id.clone();
    assert!(step.call.as_ref().unwrap().started);
    w.settle_without_calls(1_500);
    assert_eq!(
        ready(w.store.load(&child)).unwrap().projection.status,
        ExecutionStatus::Failed
    );
    assert!(ready(runbook_core::calls::result_pending(&w.store, &child)).unwrap());
    // Two ordinary runs finish after it.
    let others = [w.finish_with(1, &|_| true), w.finish_with(2, &|_| true)];
    let policy = retention(1);
    let mut stats = job::JobStats::default();
    for pass in 0..3 {
        let one = ready(job::run_once(
            &w.store,
            &w.directory,
            &Reports,
            &scope(),
            3 * DAY_MS + pass,
            &policy,
            1,
        ))
        .unwrap();
        stats.live_deleted += one.live_deleted;
        stats.results_pending += one.results_pending;
    }
    assert!(w.live(&child), "the child waits for its result's delivery");
    assert!(stats.results_pending >= 1);
    for id in &others {
        assert!(!w.live(id), "{id} was held up behind the child");
    }
    // A deletion request names the escape.
    assert_eq!(
        ready(job::delete_run(
            &w.store,
            &w.directory,
            &Reports,
            &scope(),
            &child,
            "privacy request",
            3 * DAY_MS,
            &policy,
        )),
        Err(StoreError::InvalidBatch(job::RESULT_UNDELIVERED))
    );
}

/// Re-check 5: a cancelled parent whose child is still running (its
/// `call-cancel` item has not run) is kept by retention and refused by a
/// deletion request; once the child is cancelled, it goes.
#[test]
fn a_parent_is_kept_until_its_child_is_cancelled() {
    let w = world();
    start_caller(&w);
    w.settle_without_calls(0);
    let state = ready(runbook_core::calls::stored_state(&w.store, &exe(50)))
        .unwrap()
        .unwrap();
    let child = state.steps[0]
        .call
        .as_ref()
        .unwrap()
        .child_execution_id
        .clone();
    w.store.set_time(500);
    let work = ready(runbook_core::actor::begin_actor(
        &w.store,
        &w.directory,
        scope(),
        &exe(50),
    ))
    .unwrap();
    let principal = runbook_core::actor::ActorPrincipal {
        principal: "user:alice".into(),
        actor: ActorRef::System,
        credential_ref: "test".into(),
    };
    ready(runbook_core::control::cancel(
        &w.store, work, &principal, None,
    ))
    .unwrap();
    w.settle_without_calls(500);
    assert_eq!(
        ready(w.store.load(&exe(50))).unwrap().projection.status,
        ExecutionStatus::Cancelled
    );
    assert_eq!(
        ready(w.store.load(&child)).unwrap().projection.status,
        ExecutionStatus::Running
    );
    let policy = retention(1);
    let mut kept = 0;
    for pass in 0..2 {
        kept += ready(job::run_once(
            &w.store,
            &w.directory,
            &Reports,
            &scope(),
            3 * DAY_MS + pass,
            &policy,
            16,
        ))
        .unwrap()
        .cancels_pending;
    }
    assert!(kept >= 1);
    assert!(w.live(&exe(50)));
    assert_eq!(
        ready(job::delete_run(
            &w.store,
            &w.directory,
            &Reports,
            &scope(),
            &exe(50),
            "privacy request",
            3 * DAY_MS,
            &policy
        )),
        Err(StoreError::InvalidBatch(job::CHILD_CANCEL_PENDING))
    );
    assert!(job::refusal_message(job::CHILD_CANCEL_PENDING).is_some());
    assert!(
        job::refusal_message(job::RESULT_UNDELIVERED)
            .unwrap()
            .contains("cancel the parent")
    );
    // The cancel item runs (its earlier lease lapsed): the child is
    // cancelled and the parent may go.
    w.store.set_time(500 + 60_001);
    for lease in ready(w.store.claim_due(64, 60_000)).unwrap() {
        ready(process_lease(&w.store, &w.directory, scope(), &lease)).unwrap();
    }
    assert_eq!(
        ready(w.store.load(&child)).unwrap().projection.status,
        ExecutionStatus::Cancelled
    );
    assert!(
        !ready(runbook_core::calls::child_cancel_pending(
            &w.store,
            &exe(50)
        ))
        .unwrap()
    );
}
