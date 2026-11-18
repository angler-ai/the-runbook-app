//! B4.4 on PostgreSQL: the drain activates a started run, two drain workers
//! racing on the same due deadline apply one transition, and a restarted
//! worker finds nothing left to apply.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use futures_util::FutureExt;
use runbook_core::{
    ExecutionStore,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord},
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, prepare_start_commit},
};
use runbook_model::{Data, ExecutionId, ExecutionStatus, RunbookId};
use runbook_server::{
    config::ServerConfig,
    drain::{DrainSettings, Drainer},
};
use runbook_store_postgres::{PostgresDirectory, PostgresExecutionStore, PostgresStore};
use std::{panic::AssertUnwindSafe, time::Duration};

fn scope() -> TenantScope {
    TenantScope {
        org_id: "org_00000000000000000000000001".parse().unwrap(),
        project_id: "prj_00000000000000000000000001".parse().unwrap(),
    }
}
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: timed, title: Timed}\n\
    spec:\n  execution: {timeout: 1s}\n  steps: [{id: only, title: Only}]\n";

async fn seed(db: &database::TestDatabase) -> ServerConfig {
    let mut admin = PostgresStore::connect(db.config()).await.unwrap();
    admin.migrate().await.unwrap();
    let directory = PostgresDirectory::new(db.config().clone());
    directory
        .commit(DirectoryCommit {
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
        })
        .await
        .unwrap();
    let store = PostgresExecutionStore::new(db.config().clone(), scope());
    let rb: RunbookId = "rb_00000000000000000000000001".parse().unwrap();
    store
        .create_runbook(
            RunbookRecord {
                runbook_id: rb.clone(),
                slug: "timed".into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            SPEC.into(),
        )
        .await
        .unwrap();
    let prepared = prepare(
        "timed",
        SPEC,
        &Default::default(),
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    publish_draft(&store, &store, &rb, 1, prepared)
        .await
        .unwrap();
    let command = StartCommand {
        execution_id: exe(),
        runbook_id: rb,
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
    let commit = prepare_start_commit(&store, &store, &command)
        .await
        .unwrap();
    store.commit_start(commit).await.unwrap();
    ServerConfig::new(
        db.config().clone(),
        "https://instance.example",
        "127.0.0.1:0",
    )
    .unwrap()
}

fn terminal(status: ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn timer_restart_applies_one_transition() {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let config = seed(&db).await;
        let settings = DrainSettings {
            batch: 16,
            lease_ms: 30_000,
            idle: Duration::from_millis(10),
        };
        // Activation: the run starts and stores its execution deadline.
        let mut first = Drainer::new(config.clone(), settings);
        let activated = first.drain_once().await.unwrap();
        assert_eq!(activated.activated, 1, "{activated:?}");
        let reader = PostgresExecutionStore::new(db.config().clone(), scope());
        let started = reader.load(&exe()).await.unwrap();
        assert!(started.timers.contains_key("execution-deadline"));
        assert!(!terminal(started.projection.status));

        // Wait past the one-second deadline on the database clock.
        tokio::time::sleep(Duration::from_millis(1_300)).await;

        // Two independent workers (separate pools) race on the same due work.
        let mut second = Drainer::new(config.clone(), settings);
        let (a, b) = tokio::join!(first.drain_once(), second.drain_once());
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a.fired + b.fired, 1, "exactly one transition: {a:?} {b:?}");
        assert_eq!(a.failed + b.failed, 0, "{a:?} {b:?}");
        let fired = reader.load(&exe()).await.unwrap();
        assert!(
            terminal(fired.projection.status),
            "{:?}",
            fired.projection.status
        );
        assert!(!fired.timers.contains_key("execution-deadline"));

        // A restarted worker finds no timer work and changes nothing.
        let mut restarted = Drainer::new(config, settings);
        let again = restarted.drain_once().await.unwrap();
        assert_eq!(again.fired + again.activated, 0, "{again:?}");
        let after = reader.load(&exe()).await.unwrap();
        assert_eq!(after.revision, fired.revision);
        assert_eq!(after.events, fired.events);
        assert_eq!(after.outbox, fired.outbox);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sto1_drain_sweeper_runs() {
    use runbook_store_objects::object_store::ObjectStoreExt;
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut config = seed(&db).await;
        config.blob_store.sweep_age = Duration::ZERO;
        config.blob_store.sweep_interval = Duration::from_secs(3600);

        let mem = std::sync::Arc::new(runbook_store_objects::object_store::memory::InMemory::new());
        let storage = std::sync::Arc::new(runbook_store_objects::ObjectStorage::new(
            mem.clone(),
            "runbook/",
        ));

        // Create an orphan object in storage
        let key = runbook_core::blob::BlobKey::Execution {
            execution_id: exe(),
            name: "crash_dump.bin".into(),
        };
        let sha = [0x55u8; 32];
        let path = storage.path(&scope(), &key, &sha);
        mem.put(&path, vec![1, 2, 3].into()).await.unwrap();

        let settings = DrainSettings::default();
        let mut drainer =
            Drainer::new(config.clone(), settings).with_objects(Some(storage.clone()));

        // sweep_once should find the orphan and delete it
        let stats = drainer.sweep_once().await.unwrap();
        assert_eq!(stats.blobs_swept, 1);

        // Verify object is gone
        assert!(mem.get(&path).await.is_err());

        // Calling sweep_if_due right after should return None because interval hasn't elapsed
        let due = drainer.sweep_if_due().await.unwrap();
        assert!(due.is_none());
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
