#[path = "../../runbook-api/tests/support/cases.rs"]
mod cases;
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn directory_routes_enforce_scope_and_roles() {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut s = runbook_store_postgres::PostgresStore::connect(db.config())
            .await
            .unwrap();
        s.migrate().await.unwrap();
        cases::directory_routes_enforce_scope_and_roles(
            runbook_store_postgres::PostgresDirectory::new(db.config().clone()),
            runbook_store_postgres::PostgresCredentials::new(db.config().clone()),
        )
        .await;
        use runbook_core::directory::DirectoryStore;
        let fresh = runbook_store_postgres::PostgresDirectory::new(db.config().clone());
        let org = format!("org_{:026}", 1).parse().unwrap();
        let snapshot = fresh.snapshot(&org).await.unwrap();
        let alice = format!("usr_{:026}", 10).parse().unwrap();
        assert!(!snapshot.users[&alice].active);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn admins_cannot_deactivate_owners() {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut s = runbook_store_postgres::PostgresStore::connect(db.config())
            .await
            .unwrap();
        s.migrate().await.unwrap();
        cases::admins_cannot_deactivate_owners(
            runbook_store_postgres::PostgresDirectory::new(db.config().clone()),
            runbook_store_postgres::PostgresCredentials::new(db.config().clone()),
        )
        .await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
