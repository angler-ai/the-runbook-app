//! UPD1: what an update and a rollback meet on the Cloudflare edition.
//!
//! - `/v1/meta` reports the release and the schema heads this code
//!   migrates to.
//! - An update applies new D1 files with `wrangler d1 migrations apply`
//!   before deploying (README §0), which records them in `d1_migrations`
//!   only. The new Worker's runner must adopt them rather than run them
//!   again (an `ALTER TABLE ... ADD COLUMN` would fail as a duplicate).
//! - A rollback (`wrangler rollback`) runs the previous release's code on a
//!   database the newer release migrated. Expand/contract keeps that code
//!   working, so its runner accepts versions it does not know as long as
//!   every file it knows is applied unchanged.
use runbook_store_d1::{
    D1,
    native::{NativeD1, run},
    prepare,
    sql::Columns,
};

#[test]
fn meta_reports_the_release_and_the_schema_heads() {
    let heads = runbook_worker::cloudflare::schema_heads();
    let last = |v: Option<i64>| v.unwrap() as u64;
    assert_eq!(
        heads["d1"],
        last(
            runbook_store_d1::migrate::MIGRATIONS
                .last()
                .map(|m| m.version)
        )
    );
    assert_eq!(
        heads["execution_do"],
        last(
            runbook_store_do::migrate::MIGRATIONS
                .last()
                .map(|m| m.version)
        )
    );
    assert_eq!(
        heads["org_do"],
        last(
            runbook_store_do::org::ORG_MIGRATIONS
                .last()
                .map(|m| m.version)
        )
    );
    assert_eq!(
        heads["webhook_do"],
        last(
            runbook_store_do::webhook::WEBHOOK_MIGRATIONS
                .last()
                .map(|m| m.version)
        )
    );
    let meta = runbook_worker::cloudflare::router_config("https://app.example.test", 1)
        .discovery
        .meta();
    assert_eq!(meta.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(meta.schema, heads);
}

fn recorded(db: &NativeD1) -> Vec<i64> {
    run(db.all(prepare(
        "SELECT version FROM schema_migration ORDER BY version",
    )))
    .unwrap()
    .iter()
    .map(|row| row.integer(0).unwrap())
    .collect()
}

#[test]
fn an_update_adopts_files_wrangler_applied_before_the_deploy() {
    let migrations = runbook_store_d1::migrate::MIGRATIONS;
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    // Release N recorded everything but the last file; wrangler then
    // applied the last one (release N+1's) and recorded only its own row.
    let last = migrations.last().unwrap();
    run(db.batch(vec![
        prepare("DELETE FROM schema_migration WHERE version = ?").bind(last.version),
        prepare(
            "CREATE TABLE d1_migrations (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE, \
             applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL)",
        ),
        prepare("INSERT INTO d1_migrations (name) VALUES (?)").bind(last.name),
    ]))
    .unwrap();
    assert!(!run(runbook_store_d1::migrate::is_current(&db)).unwrap());
    // Running the file again would fail (`duplicate column name`).
    assert_eq!(
        run(runbook_store_d1::migrate::migrate(&db)).unwrap(),
        Vec::<i64>::new()
    );
    assert_eq!(recorded(&db).len(), migrations.len());
    assert!(run(runbook_store_d1::migrate::is_current(&db)).unwrap());
}

#[test]
fn a_rolled_back_worker_accepts_a_newer_release_schema() {
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    let newer = runbook_store_d1::migrate::MIGRATIONS
        .last()
        .unwrap()
        .version
        + 1;
    run(db.run(
        prepare("INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?, ?, ?)")
            .bind(newer)
            .bind("from-a-newer-release")
            .bind(1_000_i64),
    ))
    .unwrap();
    assert!(run(runbook_store_d1::migrate::is_current(&db)).unwrap());
    assert_eq!(
        run(runbook_store_d1::migrate::migrate(&db)).unwrap(),
        Vec::<i64>::new()
    );
    // A file this code knows, changed, is still refused.
    run(db.run(prepare(
        "UPDATE schema_migration SET checksum = 'changed' WHERE version = 2",
    )))
    .unwrap();
    assert!(run(runbook_store_d1::migrate::migrate(&db)).is_err());
}

#[test]
fn an_unknown_version_with_a_known_file_missing_is_refused() {
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    let last = runbook_store_d1::migrate::MIGRATIONS
        .last()
        .unwrap()
        .version;
    run(db.batch(vec![
        prepare("DELETE FROM schema_migration WHERE version = ?").bind(last),
        prepare("INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?, ?, ?)")
            .bind(last + 1)
            .bind("unknown")
            .bind(1_000_i64),
    ]))
    .unwrap();
    assert!(run(runbook_store_d1::migrate::is_current(&db)).is_err());
}
