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
        // The 200 requests run on the same runtime. Hand slots back with
        // their healthy connections, including while other calls wait.
        assert!(
            config.pool_stats().opened <= CAP as u64,
            "{:?}",
            config.pool_stats()
        );
        // At most CAP pooled backends remain after the load.
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

#[tokio::test]
#[ignore = "requires a local TCP DATABASE_URL; creates and drops only a test-owned database"]
async fn readiness_replaces_a_connection_that_stopped_responding() {
    use runbook_store_postgres::{DatabaseConfig, ping};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::watch;

    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let url = std::env::var("DATABASE_URL").unwrap();
        let target: tokio_postgres::Config = url.parse().unwrap();
        let [tokio_postgres::config::Host::Tcp(host)] = target.get_hosts() else {
            panic!("the reconnect fixture needs a single TCP endpoint");
        };
        let port = target.get_ports().first().copied().unwrap_or(5432);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (blackhole, mut first) = watch::channel(());
        let host = host.clone();
        // The proxy accepts a replacement connection, but the first socket
        // stays open and sends no more responses after the simulated outage.
        let proxy = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut upstream = TcpStream::connect((host.as_str(), port)).await.unwrap();
            let stalled = async move {
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
                    _ = first.changed() => std::future::pending::<()>().await,
                }
            };
            let (mut client, _) = tokio::select! {
                socket = listener.accept() => socket.unwrap(),
                _ = stalled => return,
            };
            let mut upstream = TcpStream::connect((host.as_str(), port)).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });
        let (_, endpoint) = url.split_once("://").unwrap();
        let (authority, _) = endpoint.split_once('/').unwrap();
        let credentials = authority.rsplit_once('@').map_or("", |(user, _)| user);
        let url = format!(
            "postgres://{credentials}@{address}/{}?sslmode=disable",
            db.name()
        );
        let config = DatabaseConfig::from_values(Some(url), None)
            .unwrap()
            .with_pool_size(1);
        ping(&config).await.unwrap();
        blackhole.send(()).unwrap();
        let recovered = tokio::time::timeout(Duration::from_secs(3), ping(&config)).await;
        proxy.abort();
        assert_eq!(recovered.unwrap(), Ok(()));
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only a test-owned database"]
async fn cancelling_an_execution_request_discards_its_open_transaction() {
    use runbook_core::session::SessionStore;

    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result =
        AssertUnwindSafe(async {
            let mut admin = PostgresStore::connect(db.config()).await.unwrap();
            admin.migrate().await.unwrap();
            let store =
                PostgresExecutionStore::new(db.config().clone().with_pool_size(1), scope(1));
            let id = projection().execution_id;
            store.create_execution(projection()).await.unwrap();
            let idle_transactions =
                async || {
                    admin.client().query_one(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
                 AND state LIKE 'idle in transaction%'",
                &[],
            ).await.unwrap().get::<_, i64>(0)
                };
            let request = async {
                let _session = store.begin(&id).await.unwrap();
                std::future::pending::<()>().await
            };
            let mut request = Box::pin(request);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        _ = &mut request => unreachable!(),
                        n = idle_transactions() => if n == 1 { break },
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            drop(request);
            tokio::time::timeout(Duration::from_secs(2), async {
                while idle_transactions().await != 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            store.load(&id).await.unwrap();
        })
        .catch_unwind()
        .await;
    db.finish(result).await;
}
