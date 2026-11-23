mod support;
use futures_util::FutureExt;
use runbook_store_postgres::PostgresStore;
use serde::{Deserialize, Serialize};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use support::database::TestDatabase;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestColumn {
    pub name: String,
    pub pg_type: String,
    pub d1_type: String,
    pub nullable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestTable {
    pub name: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_table: Option<String>,
    pub columns: Vec<ManifestColumn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub tables: Vec<ManifestTable>,
}

fn map_pg_type_to_d1(data_type: &str, udt_name: &str) -> String {
    match (data_type, udt_name) {
        ("jsonb", _) | (_, "jsonb") => "TEXT".to_string(),
        ("bytea", _) | (_, "bytea") => "BLOB".to_string(),
        ("timestamp with time zone", _) | (_, "timestamptz") => "INTEGER".to_string(),
        ("bigint", _) | (_, "int8") => "INTEGER".to_string(),
        ("integer", _) | (_, "int4") => "INTEGER".to_string(),
        ("smallint", _) | (_, "int2") => "INTEGER".to_string(),
        ("boolean", _) | (_, "bool") => "INTEGER".to_string(),
        ("text", _) | (_, "text") => "TEXT".to_string(),
        ("ARRAY", _) | (_, "_text") => "TEXT".to_string(),
        ("USER-DEFINED", "tsvector") | (_, "tsvector") => "FTS5".to_string(),
        (dt, udt) => panic!("unhandled postgres type: {dt} (udt: {udt})"),
    }
}

fn target_for_table(name: &str) -> (String, Option<String>) {
    match name {
        // ExecutionDO tables
        "execution" => ("do".to_string(), Some("meta".to_string())),
        "execution_event" => ("do".to_string(), Some("event".to_string())),
        "execution_timer" => ("do".to_string(), Some("timer".to_string())),
        "execution_timer_history" => ("do".to_string(), Some("timer_history".to_string())),
        "execution_outbox" => ("do".to_string(), Some("outbox".to_string())),
        "execution_derived_outbox" => ("do".to_string(), Some("derived_outbox".to_string())),
        "execution_idempotency" => ("do".to_string(), Some("idempotency".to_string())),
        "execution_claim" => ("do".to_string(), Some("claim".to_string())),
        "claim_log" => ("do".to_string(), Some("claim_log".to_string())),
        "execution_start" => ("do".to_string(), Some("start_record".to_string())),
        "work_lease" => ("do".to_string(), Some("work_lease".to_string())),
        "credential_family" => ("do".to_string(), Some("credential_family".to_string())),
        "execution_credential" => ("do".to_string(), Some("execution_credential".to_string())),
        "actor_replay" => ("do".to_string(), Some("actor_replay".to_string())),

        // R2 blobs
        "blob" | "blob_chunk" => ("r2".to_string(), None),
        // CF6: one OrgDO per organization (its tables keep these names)
        "usage_monthly" | "quota_reservation" => ("orgdo".to_string(), Some(name.to_string())),
        // HOST1: shared rate-limit windows; the Cloudflare edition keeps
        // them in its LimitsDO.
        "rate_window" => ("limitsdo".to_string(), None),
        // CF11: one WebhookDO per subscription holds its delivery log
        "webhook_delivery" | "webhook_attempt" => ("webhookdo".to_string(), Some(name.to_string())),

        // R0/R0b: the runner's core tables live in D1 under the same names
        // (signing_keys, runner_*, project_runner_settings), so they take the
        // default D1 mapping below.

        // D1 database
        d1_name => ("d1".to_string(), Some(d1_name.to_string())),
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn export_postgres_manifest() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut store = PostgresStore::connect(db.config()).await.unwrap();
        let applied = store.migrate().await.unwrap();
        assert_eq!(
            applied,
            runbook_store_postgres::SCHEMA_VERSION,
            "expected every migration to apply"
        );

        let rows = store
            .client()
            .query(
                "SELECT
                    t.table_name,
                    c.column_name,
                    c.data_type,
                    c.udt_name,
                    c.is_nullable
                 FROM information_schema.tables t
                 JOIN information_schema.columns c
                     ON t.table_schema = c.table_schema AND t.table_name = c.table_name
                 WHERE t.table_schema = 'runbook' AND t.table_type = 'BASE TABLE'
                 ORDER BY t.table_name, c.ordinal_position",
                &[],
            )
            .await
            .unwrap();

        let mut tables_map: std::collections::BTreeMap<String, Vec<ManifestColumn>> =
            std::collections::BTreeMap::new();

        for row in rows {
            let table_name: String = row.get(0);
            let column_name: String = row.get(1);
            let data_type: String = row.get(2);
            let udt_name: String = row.get(3);
            let is_nullable: String = row.get(4);

            let d1_type = map_pg_type_to_d1(&data_type, &udt_name);
            let pg_type = if data_type == "USER-DEFINED" {
                udt_name.clone()
            } else {
                data_type.clone()
            };

            tables_map
                .entry(table_name)
                .or_default()
                .push(ManifestColumn {
                    name: column_name,
                    pg_type,
                    d1_type,
                    nullable: is_nullable == "YES",
                });
        }

        let mut manifest_tables = Vec::new();
        for (table_name, columns) in tables_map {
            let (target, target_table) = target_for_table(&table_name);
            manifest_tables.push(ManifestTable {
                name: table_name,
                target,
                target_table,
                columns,
            });
        }

        let manifest = Manifest {
            tables: manifest_tables,
        };

        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();

        let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../cloudflare/migrations/postgres-manifest.json");

        std::fs::write(&manifest_path, &manifest_json)
            .unwrap_or_else(|e| panic!("failed to write manifest to {:?}: {e}", manifest_path));

        println!("Manifest written to {:?}", manifest_path);
        assert!(manifest_path.exists());
    })
    .catch_unwind()
    .await;

    db.finish(result).await;
}
