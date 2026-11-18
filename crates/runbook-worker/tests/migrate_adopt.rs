//! Deploy finding: `cloudflare/README.md` tells operators to apply the D1
//! migrations with `wrangler d1 migrations apply` before traffic. Wrangler
//! records them in its own `d1_migrations` table, so the Worker's runner
//! (which keeps `schema_migration`) found its history empty, re-ran every
//! file and failed on the first `ALTER TABLE` (`duplicate column name`):
//! every API request answered 500. The runner now adopts wrangler's
//! history the first time it runs.
use runbook_store_d1::{
    D1,
    native::{NativeD1, run},
    prepare,
    sql::Columns,
};

/// A database as `wrangler d1 migrations apply` leaves it: every file
/// applied, `d1_migrations` naming them, `schema_migration` empty.
fn applied_by_wrangler(names: &[&str]) -> NativeD1 {
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    run(db.batch(vec![
        prepare("DELETE FROM schema_migration"),
        prepare(
            "CREATE TABLE d1_migrations (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE, \
             applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL)",
        ),
    ]))
    .unwrap();
    for name in names {
        run(db.run(prepare("INSERT INTO d1_migrations (name) VALUES (?)").bind(*name))).unwrap();
    }
    db
}

fn recorded(db: &NativeD1) -> u64 {
    run(db.all(prepare("SELECT count(*) FROM schema_migration"))).unwrap()[0]
        .unsigned(0)
        .unwrap()
}

#[test]
fn the_runner_adopts_what_wrangler_applied() {
    let names: Vec<&str> = runbook_store_d1::migrate::MIGRATIONS
        .iter()
        .map(|m| m.name)
        .collect();
    let db = applied_by_wrangler(&names);
    assert_eq!(
        run(runbook_store_d1::migrate::migrate(&db)).unwrap(),
        Vec::<i64>::new()
    );
    assert_eq!(recorded(&db), names.len() as u64);
    // And again: nothing left to do.
    assert_eq!(
        run(runbook_store_d1::migrate::migrate(&db)).unwrap(),
        Vec::<i64>::new()
    );
}
