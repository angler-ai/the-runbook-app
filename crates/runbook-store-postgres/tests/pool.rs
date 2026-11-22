//! F32: every handle built from one config shares one bounded pool, as the
//! server builds a store per request.
mod support;
use futures_util::FutureExt;
use runbook_core::ExecutionStore;
use runbook_store_conformance::{execution::projection, scope};
use runbook_store_postgres::{PostgresExecutionStore, PostgresStore};
use std::{cell::Cell, panic::AssertUnwindSafe, time::Duration};
use support::database::TestDatabase;

const CAP: usize = 5;

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn concurrent_requests_stay_within_the_pool_and_reuse_connections() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut admin = PostgresStore::connect(db.config()).await.unwrap();
        admin.migrate().await.unwrap();
        let config = db.config().clone().with_pool_size(CAP);
        assert_eq!(config.pool_size(), CAP);
        let store = || PostgresExecutionStore::new(config.clone(), scope(1));
        store().create_execution(projection()).await.unwrap();
        let id = projection().execution_id;
        // Backends of this database other than the observer itself.
        let backends = async || {
            admin
                .client()
                .query(
                    "SELECT pid FROM pg_stat_activity \
                     WHERE datname = current_database() AND pid <> pg_backend_pid() \
                     AND backend_type = 'client backend' \
                     ORDER BY pid",
                    &[],
                )
                .await
                .unwrap()
                .iter()
                .map(|row| row.get::<_, i32>(0))
                .collect::<Vec<_>>()
        };
        // 200 concurrent requests, each with its own handle.
        let done = Cell::new(false);
        let peak = Cell::new(0);
        let loads = async {
            let results = futures_util::future::join_all((0..200).map(|_| {
                let handle = store();
                let id = id.clone();
                async move { handle.load(&id).await }
            }))
            .await;
            done.set(true);
            results
        };
        // Connections working at once (running a statement or inside a
        // transaction). A connection the pool closes to hand its slot to a
        // waiter leaves its server backend in `pg_stat_activity` (last state
        // `idle`) for a moment after the client drops it, so counting every
        // backend can briefly read CAP + 1 without the pool exceeding its cap.
        // A backend still starting up (state NULL until its first query)
        // isn't working either: counting it read CAP + 1 on Linux CI.
        let working = async || {
            admin
                .client()
                .query(
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE datname = current_database() AND pid <> pg_backend_pid() \
                     AND backend_type = 'client backend' \
                     AND state IN ('active', 'idle in transaction', \
                     'idle in transaction (aborted)')",
                    &[],
                )
                .await
                .unwrap()[0]
                .get::<_, i64>(0) as usize
        };
        let sampler = async {
            while !done.get() {
                peak.set(peak.get().max(working().await));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        let (results, ()) = tokio::join!(loads, sampler);
        assert!(
            results.iter().all(Result::is_ok),
            "{:?}",
            results.iter().find(|r| r.is_err())
        );
        assert!(peak.get() <= CAP, "peak {} working backends", peak.get());
        // Closed connections' backends exit shortly after; then at most CAP
        // remain (the idle pool).
        let mut settled = backends().await.len();
        for _ in 0..200 {
            if settled <= CAP {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            settled = backends().await.len();
        }
        assert!(settled <= CAP, "{settled} backends after the load");
        // A later request on a fresh handle reuses an idle backend.
        let first = store();
        first.load(&id).await.unwrap();
        let before = backends().await;
        let second = store();
        second.load(&id).await.unwrap();
        assert_eq!(backends().await, before);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
