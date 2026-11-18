//! HOST1 review: the native rate limiter fails closed. A busy or hung
//! database refuses (503) within a short acquire timeout instead of
//! counting nothing; only an unreachable database falls back to the
//! instance's own windows; and a key already refused is refused from memory
//! without database work.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
#[allow(dead_code)]
mod database;
use futures_util::FutureExt;
use runbook_api::backends::{Backends, Limited};
use runbook_server::{backends::PostgresBackends, config::ServerConfig};
use runbook_store_postgres::{DatabaseConfig, PostgresStore};
use std::{
    panic::AssertUnwindSafe,
    sync::Arc,
    time::{Duration, Instant},
};

fn backends(database: DatabaseConfig) -> PostgresBackends {
    let config =
        ServerConfig::new(database, "https://runbooks.example.test", "127.0.0.1:0").unwrap();
    PostgresBackends::new(&config, Arc::new(tokio::sync::Semaphore::new(1)), None)
}

#[tokio::test]
async fn a_hung_database_refuses_with_503_quickly() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let b = backends(
        DatabaseConfig::from_values(Some(format!("postgres://u@127.0.0.1:{port}/db")), None)
            .unwrap(),
    );
    let started = Instant::now();
    assert_eq!(
        b.rate_limit("miss:usr_x", 60_000, 5).await,
        Err(Limited::Unavailable)
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    let refused = Limited::Unavailable.response("req_1");
    assert_eq!(refused.status, 503, "fails closed, never allowed");
    assert_eq!(refused.body["code"], "storage.unavailable");
}

#[tokio::test]
async fn an_unreachable_database_falls_back_to_this_instances_windows() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let b = backends(
        DatabaseConfig::from_values(Some(format!("postgres://u@127.0.0.1:{port}/db")), None)
            .unwrap(),
    );
    for n in 0..3 {
        assert_eq!(
            b.rate_limit("code:192.0.2.1", 60_000, 3).await,
            Ok(()),
            "{n}"
        );
    }
    assert!(matches!(
        b.rate_limit("code:192.0.2.1", 60_000, 3).await,
        Err(Limited::Retry(_))
    ));
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_refused_key_is_refused_from_memory() {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let admin = {
            let mut admin = PostgresStore::connect(db.config()).await.unwrap();
            admin.migrate().await.unwrap();
            admin
        };
        let b = backends(db.config().clone());
        for _ in 0..2 {
            assert_eq!(b.rate_limit("miss:usr_a", 60_000, 2).await, Ok(()));
        }
        assert!(matches!(
            b.rate_limit("miss:usr_a", 60_000, 2).await,
            Err(Limited::Retry(s)) if (1..=60).contains(&s)
        ));
        // With the table gone, a database read would now fail (503): the
        // refused key is answered from memory, another key is not.
        admin
            .client()
            .batch_execute("DROP TABLE runbook.rate_window")
            .await
            .unwrap();
        assert!(matches!(
            b.rate_limit("miss:usr_a", 60_000, 2).await,
            Err(Limited::Retry(_))
        ));
        assert_eq!(
            b.rate_limit("miss:usr_b", 60_000, 2).await,
            Err(Limited::Unavailable)
        );
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
