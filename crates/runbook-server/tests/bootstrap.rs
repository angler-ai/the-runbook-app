//! Reuse the receipt-based harness; never operate on a database we did not create.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use database::TestDatabase;
use futures_util::FutureExt;
use runbook_core::{credentials::*, directory::*, identity::*};
use runbook_model::ProjectRole;
use runbook_server::{
    bootstrap::{self, BootstrapOptions},
    config::ServerConfig,
};
use runbook_store_postgres::{
    BootstrapFault, PostgresCredentials, PostgresDirectory, PostgresStore,
};
use std::panic::AssertUnwindSafe;

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn bootstrap_is_local_atomic_and_once() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let config = ServerConfig::new(
            db.config().clone(),
            "https://runbooks.example.test",
            "127.0.0.1:8080",
        )
        .unwrap();
        let options = BootstrapOptions::default();
        // A late failure rolls back migrations too on a completely fresh database.
        assert!(
            bootstrap::run_with_fault(&config, &options, Some(BootstrapFault::BeforeCommit))
                .await
                .is_err()
        );
        let mut observer = PostgresStore::connect(db.config()).await.unwrap();
        let absent: bool = observer
            .client()
            .query_one("SELECT to_regnamespace('runbook') IS NULL", &[])
            .await
            .unwrap()
            .get(0);
        assert!(absent);
        observer.migrate().await.unwrap();
        for fault in [
            BootstrapFault::AfterDirectory,
            BootstrapFault::AfterPersonalToken,
            BootstrapFault::BeforeCommit,
        ] {
            assert!(
                bootstrap::run_with_fault(&config, &options, Some(fault))
                    .await
                    .is_err()
            );
            for table in [
                "organization",
                "directory_revision",
                "user_identity",
                "org_user",
                "project",
                "project_member",
                "credential",
                "instance_bootstrap",
            ] {
                let count: i64 = observer
                    .client()
                    .query_one(&format!("SELECT count(*) FROM runbook.{table}"), &[])
                    .await
                    .unwrap()
                    .get(0);
                assert_eq!(count, 0, "partial state in {table}");
            }
        }
        // Two independent connections race; exactly one may return secrets.
        let (a, b) = tokio::join!(
            bootstrap::run(&config, &options),
            bootstrap::run(&config, &options)
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        let output = match (a, b) {
            (Ok(v), Err(_)) | (Err(_), Ok(v)) => v,
            _ => panic!("bootstrap did not have one winner"),
        };
        let debug = format!("{output:?}");
        assert!(!debug.contains("trb_"));
        let mut printed = Vec::new();
        output.write_once(&mut printed).unwrap();
        let printed = String::from_utf8(printed).unwrap();
        assert_eq!(printed.matches("trb_pat_").count(), 1);
        assert_eq!(printed.matches("trb_svc_").count(), 1);
        let data: serde_json::Value = serde_json::from_str(&printed).unwrap();
        let org_id = data["org_id"].as_str().unwrap().parse().unwrap();
        let project_id: runbook_model::ProjectId =
            data["project_id"].as_str().unwrap().parse().unwrap();
        let user_id: runbook_model::UserId = data["user_id"].as_str().unwrap().parse().unwrap();
        let snap = PostgresDirectory::new(db.config().clone())
            .snapshot(&org_id)
            .await
            .unwrap();
        assert_eq!(snap.users.len(), 1);
        assert_eq!(snap.users[&user_id].role, OrgRole::Owner);
        assert!(snap.users[&user_id].active);
        assert_eq!(
            snap.project_members[&(project_id.clone(), user_id)],
            ProjectRole::Admin
        );
        assert_eq!(snap.revision, 1);
        let credentials = PostgresCredentials::new(db.config().clone());
        for (field, kind) in [
            ("pat", TokenKind::Personal),
            ("service", TokenKind::Service),
        ] {
            let secret = data[field].as_str().unwrap();
            assert_eq!(parse_secret(secret).unwrap().0, kind);
            let r = credentials
                .find_by_hash(&secret_hash(secret))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(r.kind, kind);
            assert!(authorize(&r, &snap, 0, &project_id, Scope::ExecutionsAct).is_ok());
            assert!(!format!("{r:?}").contains(secret));
            let raw: String = observer
                .client()
                .query_one(
                    "SELECT payload FROM runbook.credential WHERE id=$1",
                    &[&r.id.as_str()],
                )
                .await
                .unwrap()
                .get(0);
            assert!(!raw.contains(secret));
        }
        assert!(bootstrap::run(&config, &options).await.is_err());
        let count: i64 = observer
            .client()
            .query_one("SELECT count(*) FROM runbook.credential", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(count, 2);
        assert!(project_access(
            &snap,
            &project_id,
            &snap.users.keys().next().unwrap().clone(),
            ProjectAction::Manage
        ));
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// UPD1: the one-shot migrate command's core applies the schema without
/// serving, and the store's schema check refuses an unmigrated database
/// before it and passes after it.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn migrate_command_applies_schema_and_check_agrees() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let config = ServerConfig::new(
            db.config().clone(),
            "https://runbooks.example.test",
            "127.0.0.1:8080",
        )
        .unwrap();
        let store = PostgresStore::connect(db.config()).await.unwrap();
        assert_eq!(
            store.check_schema().await.unwrap_err().0,
            "postgres.schema_out_of_date"
        );
        drop(store);

        let applied = bootstrap::migrate(&config.database).await.unwrap();
        assert_eq!(applied, runbook_store_postgres::SCHEMA_VERSION);
        let store = PostgresStore::connect(db.config()).await.unwrap();
        assert_eq!(store.check_schema().await.unwrap(), applied);
        // Migration is idempotent: the Job can be retried safely.
        assert_eq!(bootstrap::migrate(&config.database).await.unwrap(), applied);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn doctor_reports_without_secrets() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let config = ServerConfig::new(db.config().clone(), "https://runbooks.example.test", "127.0.0.1:8080").unwrap();
        assert!(bootstrap::doctor(&config).await.is_err()); // does not migrate
        let output = bootstrap::run(&config, &BootstrapOptions::default()).await.unwrap();
        let mut secrets = Vec::new(); output.write_once(&mut secrets).unwrap();
        let report = bootstrap::doctor(&config).await.unwrap();
        let text = report.to_string();
        assert!(text.contains("database: ok"));
        assert!(text.contains("schema: current"));
        assert!(text.contains("blobs: ok"));
        assert!(!text.contains("trb_") && !text.contains("postgres://") && !text.contains("https://"));
        assert!(!format!("{config:?}").contains("https://"));
        let observer = PostgresStore::connect(db.config()).await.unwrap();
        for table in ["blob","blob_chunk"] {
            let count:i64 = observer.client().query_one(&format!("SELECT count(*) FROM runbook.{table}"), &[]).await.unwrap().get(0);
            assert_eq!(count,0);
        }
        observer.client().batch_execute("ALTER TABLE runbook.blob_chunk RENAME TO missing_chunk").await.unwrap();
        assert!(bootstrap::doctor(&config).await.is_err());
        observer.client().batch_execute("ALTER TABLE runbook.missing_chunk RENAME TO blob_chunk; UPDATE runbook.schema_migration SET checksum='bad' WHERE version=1").await.unwrap();
        assert!(bootstrap::doctor(&config).await.is_err());
    }).catch_unwind().await;
    db.finish(result).await;
}

/// AUTH5: bootstrap by email creates the org, the owner with that verified
/// email and the default project in one commit, and no credential unless
/// asked; with `with_token` it also prints the two tokens.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn bootstrap_by_email_binds_a_verified_owner_and_issues_tokens_only_when_asked() {
    for with_token in [false, true] {
        let mut db = TestDatabase::create_from_env().await.unwrap();
        let result = AssertUnwindSafe(async {
            let config = ServerConfig::new(
                db.config().clone(),
                "https://runbooks.example.test",
                "127.0.0.1:8080",
            )
            .unwrap();
            let options = BootstrapOptions {
                owner_email: Some(" Ada@Example.test ".into()),
                user_name: "Ada Lovelace".into(),
                with_token,
                ..BootstrapOptions::default()
            };
            let output = bootstrap::run(&config, &options).await.unwrap();
            assert!(!format!("{output:?}").contains("example.test"));
            let mut printed = Vec::new();
            output.write_once(&mut printed).unwrap();
            let printed = String::from_utf8(printed).unwrap();
            let data: serde_json::Value = serde_json::from_str(&printed).unwrap();
            assert_eq!(data["owner_email"], "ada@example.test");
            assert_eq!(printed.contains("trb_"), with_token, "{printed}");
            assert_eq!(data.get("pat").is_some(), with_token);
            assert_eq!(data.get("service").is_some(), with_token);
            let org_id: runbook_model::OrganizationId =
                data["org_id"].as_str().unwrap().parse().unwrap();
            let user_id: runbook_model::UserId = data["user_id"].as_str().unwrap().parse().unwrap();
            let project_id: runbook_model::ProjectId =
                data["project_id"].as_str().unwrap().parse().unwrap();
            let directory = PostgresDirectory::new(db.config().clone());
            let snap = directory.snapshot(&org_id).await.unwrap();
            assert_eq!(snap.revision, 1, "one directory commit");
            assert_eq!(snap.users[&user_id].role, OrgRole::Owner);
            assert_eq!(
                snap.project_members[&(project_id.clone(), user_id.clone())],
                ProjectRole::Admin
            );
            assert_eq!(snap.projects[&project_id].slug, "default");
            // Signing in by email resolves to this owner (D45).
            let found = directory
                .user_by_verified_email("ADA@example.test")
                .await
                .unwrap()
                .unwrap();
            assert_eq!((found.id, found.name.as_str()), (user_id, "Ada Lovelace"));
            let observer = PostgresStore::connect(db.config()).await.unwrap();
            let count: i64 = observer
                .client()
                .query_one("SELECT count(*) FROM runbook.credential", &[])
                .await
                .unwrap()
                .get(0);
            assert_eq!(count, if with_token { 2 } else { 0 });
            // Still once per instance, by email or not.
            assert_eq!(
                bootstrap::run(&config, &options).await.unwrap_err().0,
                "bootstrap.already_initialized"
            );
            assert!(
                bootstrap::run(&config, &BootstrapOptions::default())
                    .await
                    .is_err()
            );
        })
        .catch_unwind()
        .await;
        db.finish(result).await;
    }
}

/// AUTH5: a malformed owner email, or one outside RUNBOOK_AUTH_ALLOWED_DOMAINS
/// (the owner could never sign in), is refused before anything is written.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn bootstrap_by_email_refuses_an_address_that_cannot_sign_in() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut config = ServerConfig::new(
            db.config().clone(),
            "https://runbooks.example.test",
            "127.0.0.1:8080",
        )
        .unwrap();
        let mut auth = runbook_server::config::AuthConfig::new(vec!["unused".into()]);
        auth.allowed_domains = vec!["example.test".into()];
        config.auth = Some(auth);
        for (email, code) in [
            ("not-an-email", "bootstrap.invalid_owner_email"),
            ("a@b@example.test", "bootstrap.invalid_owner_email"),
            ("ada@elsewhere.test", "bootstrap.owner_domain_not_allowed"),
        ] {
            let options = BootstrapOptions {
                owner_email: Some(email.into()),
                ..BootstrapOptions::default()
            };
            assert_eq!(bootstrap::run(&config, &options).await.unwrap_err().0, code);
        }
        let observer = PostgresStore::connect(db.config()).await.unwrap();
        let absent: bool = observer
            .client()
            .query_one("SELECT to_regnamespace('runbook') IS NULL", &[])
            .await
            .unwrap()
            .get(0);
        assert!(absent, "nothing written, not even migrations");
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
