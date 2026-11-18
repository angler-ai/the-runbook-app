//! CF4 Cron passes over core's in-memory stores: the archive and retention
//! job for every tenant the host lists, and webhook redelivery.
use runbook_core::{
    ExecutionStore, StoreError,
    archives::{ArchiveIndex, job::ReportSource, job::RetentionPolicy},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, memory::MemoryDirectory,
    },
    drain::process_lease,
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, prepare_start_commit},
    work_queue::WorkQueue,
};
use runbook_model::{Data, ExecutionId, RunbookId};
use runbook_store_conformance::scope;
use runbook_worker::cron::{ArchiveHost, archive_pass};
use runbook_worker_conformance::native::run;

const DAY_MS: i64 = 86_400_000;
const START_MS: i64 = 1_000;
/// The run's one-second deadline finishes it here: UTC day 0.
const DONE_MS: i64 = START_MS + 1_500;
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: timed, title: Timed}\n\
    spec:\n  execution: {timeout: 1s}\n  steps: [{id: only, title: Only}]\n";

fn rb() -> RunbookId {
    "rb_00000000000000000000000001".parse().unwrap()
}

/// A published runbook and one run its deadline finished on day 0 (the
/// same world as core's archive tests).
fn finished_run(directory: &MemoryDirectory) -> MemoryExecutionStore {
    let store = MemoryExecutionStore::empty(scope(1), START_MS);
    run(store.create_runbook(
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
    run(publish_draft(&store, &store, &rb(), 1, prepared)).unwrap();
    let id: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
    let command = StartCommand {
        execution_id: id.clone(),
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
        idempotency_key: "k1".into(),
        principal: "user:alice".into(),
        request_hash: [1; 32],
    };
    let commit = run(prepare_start_commit(&store, &store, &command)).unwrap();
    run(store.commit_start(commit)).unwrap();
    for at in [START_MS, DONE_MS] {
        store.set_time(at);
        for lease in run(store.claim_due(10, 60_000)).unwrap() {
            run(process_lease(&store, directory, scope(1), &lease)).unwrap();
        }
    }
    assert_eq!(
        run(store.load(&id)).unwrap().projection.completed_at_ms,
        Some(DONE_MS)
    );
    store
}

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

struct Host {
    store: MemoryExecutionStore,
    directory: MemoryDirectory,
    tenants: Vec<TenantScope>,
    now: i64,
}
impl ArchiveHost for Host {
    type Store = MemoryExecutionStore;
    type Directory = MemoryDirectory;
    type Reports = Reports;
    async fn tenants(&self) -> Result<Vec<TenantScope>, StoreError> {
        Ok(self.tenants.clone())
    }
    fn store(&self, scope: &TenantScope) -> MemoryExecutionStore {
        self.store.scoped(scope.clone())
    }
    fn directory(&self) -> &MemoryDirectory {
        &self.directory
    }
    fn reports(&self) -> &Reports {
        &Reports
    }
    fn policy(&self, _: &TenantScope) -> RetentionPolicy {
        RetentionPolicy::default()
    }
    fn now_ms(&self) -> i64 {
        self.now
    }
}

/// The tenant's organization and project, which the job names in files.
fn directory() -> MemoryDirectory {
    let directory = MemoryDirectory::default();
    run(directory.commit(DirectoryCommit {
        org_id: scope(1).org_id,
        expected_revision: 0,
        mutations: vec![
            DirectoryMutation::CreateOrganization {
                title: "Org".into(),
            },
            DirectoryMutation::CreateProject(ProjectRecord {
                id: scope(1).project_id,
                org_id: scope(1).org_id,
                slug: "api".into(),
                title: "API".into(),
            }),
        ],
    }))
    .unwrap();
    directory
}

#[test]
fn the_archive_pass_writes_each_tenants_finished_days() {
    let directory = directory();
    let store = finished_run(&directory);
    let now = 3 * DAY_MS;
    store.set_time(now);
    assert!(
        !run(store.unarchived_days(&scope(1), 3, now, 16))
            .unwrap()
            .is_empty(),
        "day 0 waits for its file"
    );
    let host = Host {
        store: store.clone(),
        directory,
        tenants: vec![scope(1), scope(2)],
        now,
    };
    let pass = run(archive_pass(&host)).unwrap();
    assert_eq!(pass.tenants, 2);
    assert!(pass.failed.is_empty(), "{:?}", pass.failed);
    assert_eq!(pass.job.written, 1, "{:?}", pass.job);
    assert!(
        run(store.unarchived_days(&scope(1), 3, now, 16))
            .unwrap()
            .is_empty(),
        "the day is archived"
    );
    // A second pass finds nothing left to do.
    let again = run(archive_pass(&host)).unwrap();
    assert_eq!(again.job.written, 0);
}
