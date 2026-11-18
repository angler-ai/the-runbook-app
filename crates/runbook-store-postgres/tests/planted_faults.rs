//! Store hardening: transaction atomicity under a real database failure.
//! Instead of the adapter's own `fail_after` checkpoints, each case plants a
//! constraint the database itself enforces (`CHECK (false) NOT VALID`) on
//! one table a multi-table write touches, runs the write, and asserts that
//! no table in the schema changed. The plant list is checked against what a
//! clean run of the same write changes, so no plant is vacuous.
mod support;
use runbook_core::{
    CommitResult, ExecutionCommit, ExecutionStore, StepProjection,
    catalog::{AdmissionState, AdmitRequest, CatalogStore},
    publish::prepare,
    retention::RunStore,
    start::{StartCommand, StartStore, prepare_start_commit},
};
use runbook_model::{Data, ExecutionStatus, StepStatus};
use runbook_store_conformance::{
    StoreFactory,
    catalog::{batch, id},
    execution::{self, projection},
    parity,
    portable::{GATE, exe, p, rb, user},
    scope,
};
use std::collections::{BTreeMap, BTreeSet};
use support::factory::{PostgresFactory, with_factory};

/// Every application table in the schema, from the catalog (not a list).
async fn tables(f: &PostgresFactory) -> Vec<String> {
    f.admin
        .client()
        .query(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'runbook' AND table_type = 'BASE TABLE' \
             AND table_name <> 'schema_migration' ORDER BY 1",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.get(0))
        .collect()
}
/// A content digest of every table, read through the admin connection.
async fn snapshot(f: &PostgresFactory) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for table in tables(f).await {
        let digest: Option<String> = f
            .admin
            .client()
            .query_one(
                &format!(
                    "SELECT md5(string_agg(t::text, '|' ORDER BY t::text)) FROM runbook.{table} t"
                ),
                &[],
            )
            .await
            .unwrap()
            .get(0);
        out.insert(table, digest.unwrap_or_default());
    }
    out
}
fn changed(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    after
        .iter()
        .filter(|(t, d)| before.get(*t) != Some(d))
        .map(|(t, _)| t.clone())
        .collect()
}
async fn plant(f: &PostgresFactory, table: &str) {
    f.admin
        .client()
        .batch_execute(&format!(
            "ALTER TABLE runbook.{table} ADD CONSTRAINT planted_fault CHECK (false) NOT VALID"
        ))
        .await
        .unwrap();
}
async fn unplant(f: &PostgresFactory, table: &str) {
    f.admin
        .client()
        .batch_execute(&format!(
            "ALTER TABLE runbook.{table} DROP CONSTRAINT planted_fault"
        ))
        .await
        .unwrap();
}
fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// A terminal execution commit that writes every execution table at once:
/// events, head, timer, outbox, idempotency, claim, step index, derived
/// outbox, run summary, and an admission release that promotes a waiter.
fn wide_commit() -> ExecutionCommit {
    let mut b = execution::batch();
    let terminal = batch(1, 0, 1, ExecutionStatus::Succeeded);
    b.projection = terminal.projection;
    b.projection.steps = vec![StepProjection {
        execution_id: b.execution_id.clone(),
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        path: "only".into(),
        status: StepStatus::Succeeded,
        verdict: None,
        outputs: Data::new(),
        summary: None,
    }];
    b.derived_outbox = terminal.derived_outbox;
    b
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn execution_commit_is_atomic_under_a_planted_constraint() {
    with_factory(async |f| {
        let store = f.create(projection()).await?;
        for n in [1u8, 2] {
            store
                .admit(AdmitRequest {
                    execution_id: id(n),
                    key: "api".into(),
                    limit: 1,
                    idempotency_key: format!("admit-{n}"),
                })
                .await?;
        }
        let planted = set(&[
            "admission",
            "execution",
            "execution_claim",
            "execution_derived_outbox",
            "execution_event",
            "execution_idempotency",
            "execution_outbox",
            "execution_timer",
            "run_summary",
            "step_run_index",
        ]);
        let before = snapshot(f).await;
        for table in &planted {
            plant(f, table).await;
            let result = f.fresh_store(scope(1)).await.commit(wide_commit()).await;
            unplant(f, table).await;
            assert!(result.is_err(), "{table}: {result:?}");
            assert_eq!(changed(&before, &snapshot(f).await), set(&[]), "{table}");
        }
        // The same commit, unplanted, changes exactly the planted tables.
        assert_eq!(
            store.commit(wide_commit()).await?,
            CommitResult::Committed(1)
        );
        let changed = changed(&before, &snapshot(f).await);
        assert!(planted.is_subset(&changed), "{changed:?}");
        assert_eq!(
            store.admission(&id(2)).await?.unwrap().state,
            AdmissionState::Admitted
        );
        Ok(())
    })
    .await;
}

fn start_command(n: u8) -> StartCommand {
    StartCommand {
        execution_id: exe(n),
        runbook_id: rb(1),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: format!("start-{n}"),
        principal: format!("user:{}", user(1)),
        request_hash: [n; 32],
    }
}

/// A queued start writes the owner, head, start record, queued admission,
/// queued outbox intent, summary and replay row in one transaction.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn queued_start_is_atomic_under_a_planted_constraint() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        parity::publish(&store, 1, "gate", GATE, 1).await;
        let first = prepare_start_commit(&store, &store, &start_command(1))
            .await
            .unwrap();
        store.commit_start(first).await?;
        let commit = prepare_start_commit(&store, &store, &start_command(2))
            .await
            .unwrap();
        let planted = set(&[
            "admission",
            "execution",
            "execution_outbox",
            "execution_owner",
            "execution_start",
            "run_summary",
            "start_replay",
        ]);
        let before = snapshot(f).await;
        for table in &planted {
            plant(f, table).await;
            let result = f.fresh_store(p()).await.commit_start(commit.clone()).await;
            unplant(f, table).await;
            assert!(result.is_err(), "{table}: {result:?}");
            assert_eq!(changed(&before, &snapshot(f).await), set(&[]), "{table}");
            assert_eq!(store.find_replay("start-2").await?, None, "{table}");
        }
        let outcome = store.commit_start(commit).await?;
        assert_eq!(
            outcome.admission.map(|a| a.state),
            Some(AdmissionState::Queued)
        );
        let changed = changed(&before, &snapshot(f).await);
        assert!(planted.is_subset(&changed), "{changed:?}");
        Ok(())
    })
    .await;
}

/// Deletion removes rows from many tables and writes the tombstone last: a
/// failure on that last statement must bring every deleted row back.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn deletion_is_atomic_under_a_planted_constraint_on_the_tombstone() {
    with_factory(async |f| {
        let store = f.create(projection()).await?;
        store
            .admit(AdmitRequest {
                execution_id: id(1),
                key: "api".into(),
                limit: 1,
                idempotency_key: "admit-1".into(),
            })
            .await?;
        store.commit(wide_commit()).await?;
        let before = snapshot(f).await;
        plant(f, "tombstone").await;
        let result = f
            .fresh_store(scope(1))
            .await
            .delete_execution(&id(1), "retention")
            .await;
        unplant(f, "tombstone").await;
        assert!(result.is_err(), "{result:?}");
        assert_eq!(changed(&before, &snapshot(f).await), set(&[]));
        assert_eq!(store.tombstone(&id(1)).await?, None);
        assert_eq!(store.load(&id(1)).await?.revision, 1);
        // Unplanted, the same deletion empties every table it touched.
        store.delete_execution(&id(1), "retention").await?;
        let changed = changed(&before, &snapshot(f).await);
        assert!(
            set(&[
                "admission",
                "execution",
                "execution_event",
                "execution_owner",
                "run_summary",
                "step_run_index",
                "tombstone",
            ])
            .is_subset(&changed),
            "{changed:?}"
        );
        Ok(())
    })
    .await;
}

/// Publishing numbers a version, records it, moves the draft and writes a
/// project event together; a failure on any of them publishes nothing.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn publish_is_atomic_under_a_planted_constraint() {
    use runbook_core::blob::{BlobKey, BlobStore};
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        parity::publish(&store, 1, "gate", GATE, 1).await;
        let next = GATE.replace("title: After", "title: After v2");
        store.save_draft(&rb(1), 1, next.clone()).await?;
        let context = runbook_spec::CompileContext {
            teams: [("oncall".into(), runbook_store_conformance::portable::team())].into(),
            ..Default::default()
        };
        let prepared = prepare(
            "gate",
            &next,
            &context,
            "rbv_00000000000000000000000002".parse().unwrap(),
        )
        .unwrap();
        // The snapshot blob is a separate, idempotent write; store it first
        // so the case isolates the numbering transaction.
        store
            .put(
                BlobKey::Shared {
                    hash: prepared.snapshot_digest,
                },
                &prepared.snapshot,
            )
            .await?;
        let before = snapshot(f).await;
        let planted = set(&["project_event_outbox", "runbook_version"]);
        for table in &planted {
            plant(f, table).await;
            let result = f
                .fresh_store(p())
                .await
                .publish_revision(&rb(1), 2, prepared.clone())
                .await;
            unplant(f, table).await;
            assert!(result.is_err(), "{table}: {result:?}");
            assert_eq!(changed(&before, &snapshot(f).await), set(&[]), "{table}");
        }
        let published = store.publish_revision(&rb(1), 2, prepared).await?;
        assert_eq!(published.runbook_version.get(), 2);
        let changed = changed(&before, &snapshot(f).await);
        assert!(planted.is_subset(&changed), "{changed:?}");
        Ok(())
    })
    .await;
}
