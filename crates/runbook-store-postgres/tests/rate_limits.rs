//! HOST1: rate-limit windows live in Postgres, so every server instance on
//! one database counts against the same limit.
mod support;
use futures_util::FutureExt;
use runbook_store_postgres::{
    DatabaseConfig, PostgresStore, RateLimitError, purge_rate_windows, rate_limit_hit,
};
use std::{
    panic::AssertUnwindSafe,
    time::{Duration, Instant},
};
use support::database::TestDatabase;

/// Generous for a live database under the test suite's load.
const ACQUIRE: Duration = Duration::from_secs(5);
/// Short, for the database that never answers.
const SHORT: Duration = Duration::from_millis(500);

/// A database that accepts connections and never answers.
fn silent_database() -> DatabaseConfig {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    DatabaseConfig::from_values(Some(format!("postgres://u@127.0.0.1:{port}/db")), None).unwrap()
}

/// HOST1 review: a database that hangs (or a pool kept busy) is told apart
/// from one that refuses connections, within the short acquire timeout,
/// so the server can fail closed on the first and fall back to local
/// windows only on the second.
#[tokio::test]
async fn a_busy_database_is_told_apart_from_an_unreachable_one_quickly() {
    let started = Instant::now();
    let busy = rate_limit_hit(&silent_database(), "k", 60_000, 5, SHORT).await;
    assert_eq!(busy, Err(RateLimitError::Busy));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let absent =
        DatabaseConfig::from_values(Some(format!("postgres://u@127.0.0.1:{port}/db")), None)
            .unwrap();
    assert_eq!(
        rate_limit_hit(&absent, "k", 60_000, 5, SHORT).await,
        Err(RateLimitError::Unreachable)
    );
}

/// A query that fails (here: no table yet) is a failure, not "unreachable".
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_failing_query_is_a_failure() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        assert_eq!(
            rate_limit_hit(db.config(), "k", 60_000, 5, ACQUIRE).await,
            Err(RateLimitError::Failed)
        );
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// The purge may delete a window between two hits; a hit never errors on
/// it (the read is one upsert under the row lock).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn hits_racing_purges_never_fail() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut admin = PostgresStore::connect(db.config()).await.unwrap();
        admin.migrate().await.unwrap();
        let config = db.config().clone().with_pool_size(4);
        let hits = futures_util::future::join_all((0..60).map(|_| {
            let config = config.clone();
            async move { rate_limit_hit(&config, "racy", 1, 1_000, ACQUIRE).await }
        }));
        let purges = futures_util::future::join_all((0..30).map(|_| {
            let config = config.clone();
            async move { purge_rate_windows(&config, 10).await }
        }));
        let (hits, purges) = tokio::join!(hits, purges);
        assert!(hits.iter().all(|h| h == &Ok(Ok(()))), "{hits:?}");
        assert!(purges.iter().all(Result::is_ok), "{purges:?}");
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn instances_share_one_window_and_expired_windows_are_purged() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut admin = PostgresStore::connect(db.config()).await.unwrap();
        admin.migrate().await.unwrap();
        // Two configs with their own pools stand for two replicas.
        let a = db.config().clone().with_pool_size(2);
        let b = db.config().clone().with_pool_size(2);
        let hour = 3_600_000;
        assert_eq!(
            rate_limit_hit(&a, "code:192.0.2.1", hour, 3, ACQUIRE)
                .await
                .unwrap(),
            Ok(())
        );
        assert_eq!(
            rate_limit_hit(&b, "code:192.0.2.1", hour, 3, ACQUIRE)
                .await
                .unwrap(),
            Ok(())
        );
        assert_eq!(
            rate_limit_hit(&a, "code:192.0.2.1", hour, 3, ACQUIRE)
                .await
                .unwrap(),
            Ok(())
        );
        let refused = rate_limit_hit(&b, "code:192.0.2.1", hour, 3, ACQUIRE)
            .await
            .unwrap();
        assert!(
            matches!(refused, Err(s) if (3_590..=3_600).contains(&s)),
            "{refused:?}"
        );
        // Another key has its own window.
        assert_eq!(
            rate_limit_hit(&b, "code:192.0.2.2", hour, 3, ACQUIRE)
                .await
                .unwrap(),
            Ok(())
        );
        // Racing first hits on a fresh key never exceed the limit.
        let raced = futures_util::future::join_all((0..8).map(|i| {
            let config = if i % 2 == 0 { a.clone() } else { b.clone() };
            async move {
                rate_limit_hit(&config, "miss:usr_race", 60_000, 5, ACQUIRE)
                    .await
                    .unwrap()
            }
        }))
        .await;
        assert_eq!(raced.iter().filter(|r| r.is_ok()).count(), 5, "{raced:?}");
        // Keys are stored hashed: no address or user ID in the table.
        let stored: Vec<String> = admin
            .client()
            .query("SELECT key_hash FROM runbook.rate_window", &[])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(stored.len(), 3);
        assert!(
            stored
                .iter()
                .all(|k| k.len() == 64 && !k.contains("192.0.2"))
        );
        // A window that has passed is purged; a live one stays.
        assert_eq!(
            rate_limit_hit(&a, "short", 1, 10, ACQUIRE).await.unwrap(),
            Ok(())
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(purge_rate_windows(&a, 100).await.unwrap(), 1);
        assert_eq!(purge_rate_windows(&a, 100).await.unwrap(), 0);
        let left: i64 = admin
            .client()
            .query_one("SELECT count(*) FROM runbook.rate_window", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 3);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
