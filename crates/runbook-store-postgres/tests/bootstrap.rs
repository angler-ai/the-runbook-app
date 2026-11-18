mod support;
use futures_util::FutureExt;
use runbook_store_postgres::{DatabaseConfig, PostgresStore};
use std::panic::AssertUnwindSafe;
use support::database::TestDatabase;

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn test_database_ownership_is_explicit() {
    assert_eq!(
        DatabaseConfig::from_values(None, None).unwrap_err().0,
        "postgres.missing_database_url"
    );
    let mut first = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut second = TestDatabase::create_from_env().await.unwrap();
        let inner = AssertUnwindSafe(async {
            assert_ne!(first.name(), second.name());
            assert_eq!(
                first.cleanup_named(second.name()).await.unwrap_err().0,
                "test_database.not_owned"
            );
            assert!(second.exists().await.unwrap());
            first.cleanup().await.unwrap();
            assert!(!first.exists().await.unwrap());
        })
        .catch_unwind()
        .await;
        second.finish(inner).await;
    })
    .catch_unwind()
    .await;
    first.finish(result).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn migrations_are_repeatable_and_auth_untouched() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut store = PostgresStore::connect(db.config()).await.unwrap();
        let applied = store.migrate().await.unwrap();
        assert!(applied >= 2);
        assert_eq!(store.migrate().await.unwrap(), applied);
        let auth: bool = store
            .client()
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname='auth')",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert!(!auth);
        let count: i64 = store
            .client()
            .query_one("SELECT count(*) FROM runbook.schema_migration", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, i64::from(applied));
        store
            .client()
            .execute(
                "UPDATE runbook.schema_migration SET checksum='corrupt' WHERE version=1",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            store.migrate().await.unwrap_err().0,
            "postgres.migration_history_mismatch"
        );
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// UPD1: a database migrated by a newer release (more history rows than
/// this code has files, the known ones intact) is refused with its own
/// code, never silently served, unless the operator allows it
/// (`RUNBOOK_ALLOW_NEWER_SCHEMA`, the rollback path: expand/contract keeps
/// the previous release working on the next one's schema). A changed known
/// file is still a mismatch either way.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_newer_schema_is_refused_unless_allowed() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut store = PostgresStore::connect(db.config()).await.unwrap();
        let code = store.migrate().await.unwrap();
        assert_eq!(code, runbook_store_postgres::SCHEMA_VERSION);
        let next = i32::try_from(code).unwrap() + 1;
        store
            .client()
            .execute(
                "INSERT INTO runbook.schema_migration(version, checksum) VALUES ($1, 'from-a-newer-release')",
                &[&next],
            )
            .await
            .unwrap();
        assert_eq!(
            store.migrate().await.unwrap_err().0,
            "postgres.schema_newer_than_code"
        );
        let doctor = PostgresStore::connect(db.config()).await.unwrap();
        assert_eq!(
            doctor.doctor().await.unwrap_err().0,
            "postgres.schema_newer_than_code"
        );

        let allowed = db.config().clone().allow_newer_schema(true);
        let mut store = PostgresStore::connect(&allowed).await.unwrap();
        assert_eq!(store.migrate().await.unwrap(), code + 1);
        let report = PostgresStore::connect(&allowed)
            .await
            .unwrap()
            .doctor()
            .await
            .unwrap();
        assert_eq!(report.migration_version, code + 1);
        assert_eq!(report.code_version, code);
        assert!(report.to_string().contains(&format!("schema: ahead (database {}, code {code}", code + 1)));

        store
            .client()
            .execute(
                "UPDATE runbook.schema_migration SET checksum='corrupt' WHERE version=2",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            store.migrate().await.unwrap_err().0,
            "postgres.migration_history_mismatch"
        );
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// F36: every foreign key has an index that leads with its columns (in any
/// order) or with its specific key column (not `org_id` alone), full or
/// partial on that column being set, so
/// cascades from `execution_owner` and `webhook_event` and lookups such as
/// `credential_family(execution_id)` on a terminal commit never scan; the
/// webhook purge reads events by age through an index.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn foreign_keys_and_purge_paths_are_indexed() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut store = PostgresStore::connect(db.config()).await.unwrap();
        store.migrate().await.unwrap();
        // Foreign keys whose columns no index leads with, in key order.
        let unindexed: Vec<String> = store
            .client()
            .query(
                "SELECT c.conrelid::regclass::text || '(' || \
                 (SELECT string_agg(a.attname, ', ' ORDER BY k.n) \
                  FROM unnest(c.conkey) WITH ORDINALITY k(attnum, n) \
                  JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum) || ')' \
                 FROM pg_constraint c \
                 WHERE c.contype = 'f' AND c.connamespace = 'runbook'::regnamespace \
                 AND NOT EXISTS ( \
                   SELECT 1 FROM pg_index i \
                   JOIN pg_attribute lead ON lead.attrelid = i.indrelid \
                     AND lead.attnum = i.indkey[0] \
                   WHERE i.indrelid = c.conrelid \
                   AND (i.indpred IS NULL OR EXISTS ( \
                     SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.conrelid \
                     AND a.attnum = ANY (c.conkey) \
                     AND pg_get_expr(i.indpred, i.indrelid) = '(' || a.attname || ' IS NOT NULL)')) \
                   AND ((SELECT array_agg(x ORDER BY x) \
                         FROM unnest((i.indkey::int2[])[0:cardinality(c.conkey) - 1]) x) \
                        = (SELECT array_agg(x ORDER BY x) FROM unnest(c.conkey) x) \
                     OR (lead.attnum = ANY (c.conkey) AND lead.attname <> 'org_id'))) \
                 ORDER BY 1",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        assert!(
            unindexed.is_empty(),
            "unindexed foreign keys: {unindexed:?}"
        );
        let purge: bool = store
            .client()
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'runbook' \
                 AND tablename = 'webhook_event' \
                 AND indexdef LIKE '%(org_id, project_id, created_at_ms%')",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert!(purge, "webhook_event has no age index for the purge");
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
