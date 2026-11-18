//! B2.3: NOTIFY hints after commit only, carrying IDs and sequence; the
//! listener rides the direct connection setting.
mod support;
use runbook_core::{ExecutionStore, WriteStage};
use runbook_model::ExecutionStatus;
use runbook_store_conformance::{StoreFactory, catalog, execution, scope};
use runbook_store_postgres::LiveEvents;
use std::time::Duration;
use support::factory::with_factory;

const WAIT: Duration = Duration::from_secs(5);

/// A rolled-back commit delivers nothing; a committed one delivers exactly
/// its execution ID and sequence, and the authoritative load agrees.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn notify_contains_ids_only_after_commit() {
    with_factory(async |f| {
        let mut live = LiveEvents::listen(&f.config).await.unwrap();
        let store = f.create(execution::projection()).await?;
        let id = execution::projection().execution_id;
        store
            .commit(catalog::batch(1, 0, 1, ExecutionStatus::Running))
            .await?;
        let event = live.next_timeout(WAIT).await.unwrap().unwrap();
        assert_eq!(event.execution_id, id);
        assert_eq!(event.sequence, 1);
        assert_eq!(store.load(&id).await?.revision, 1);

        f.fail_after(&store, WriteStage::DerivedRows).await?;
        assert!(
            store
                .commit(catalog::batch(1, 1, 2, ExecutionStatus::Running))
                .await
                .is_err()
        );
        store
            .commit(catalog::batch(1, 1, 2, ExecutionStatus::Running))
            .await?;
        // The first hint after the retry is the committed sequence 2, and
        // nothing else follows: the rollback emitted no phantom hint.
        let event = live.next_timeout(WAIT).await.unwrap().unwrap();
        assert_eq!((&event.execution_id, event.sequence), (&id, 2));
        assert_eq!(
            live.next_timeout(Duration::from_millis(300)).await.unwrap(),
            None
        );
        assert_eq!(store.load(&id).await?.revision, 2);
        Ok(())
    })
    .await;
}

/// The listener uses DATABASE_URL_DIRECT when set and DATABASE_URL
/// otherwise; connection errors never carry either value.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn listener_uses_direct_connection_setting() {
    with_factory(async |f| {
        f.create(execution::projection()).await?;
        // Primary names a database that does not exist; only the direct
        // setting can reach the test database.
        let direct_wins = f
            .config
            .with_database("rb_missing_db_xyz")
            .with_direct_database(&f.database);
        let mut live = LiveEvents::listen(&direct_wins).await.unwrap();
        let store = f.fresh_store(scope(1)).await;
        store
            .commit(catalog::batch(1, 0, 1, ExecutionStatus::Running))
            .await?;
        let event = live.next_timeout(WAIT).await.unwrap().unwrap();
        assert_eq!(event.sequence, 1);
        drop(live);

        // Without a direct setting, the primary connection is the fallback.
        let mut live = LiveEvents::listen(&f.config).await.unwrap();
        store
            .commit(catalog::batch(1, 1, 2, ExecutionStatus::Running))
            .await?;
        assert_eq!(live.next_timeout(WAIT).await.unwrap().unwrap().sequence, 2);
        drop(live);

        // Unreachable either way: the error names no database or URL.
        let unreachable = f.config.with_database("rb_missing_db_xyz");
        let error = LiveEvents::listen(&unreachable).await.err().unwrap();
        assert_eq!(error.to_string(), "postgres.connection_failed");
        assert!(!error.to_string().contains("rb_missing"));
        Ok(())
    })
    .await;
}
