//! D3 admin commands end to end through the `runbook-server` binary: export
//! a project from one owned database, import it into another, re-export
//! byte-identically, and refuse bad archives and non-empty targets with
//! clear messages and exit code 1. No secret reaches stdout, stderr or disk.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use futures_util::FutureExt;
use runbook_core::{
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord},
    publish::{prepare, publish_draft},
    webhooks::{SealedSecret, Subscription, WebhookStore},
};
use runbook_store_postgres::{PostgresDirectory, PostgresExecutionStore, PostgresWebhooks};
use std::{
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const SECRET: &str = "v1.SERVER-SEALED-SECRET";
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: deploy, title: Deploy}\n\
    spec: {steps: [{id: only, title: Only}]}\n";

fn scope() -> TenantScope {
    TenantScope {
        org_id: "org_00000000000000000000000001".parse().unwrap(),
        project_id: "prj_00000000000000000000000001".parse().unwrap(),
    }
}
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "runbook-d3-cli-{}-{name}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}
/// The binary with this database URL; `RUNBOOK_ORIGIN` is required config.
fn server(database_url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_runbook-server"))
        .args(args)
        .env_clear()
        .env("RUNBOOK_ORIGIN", "http://localhost:8080")
        .env("DATABASE_URL", database_url)
        .output()
        .unwrap()
}
fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
/// The test database's URL: the configured server URL with its name.
fn url_for(name: &str) -> String {
    let base = std::env::var("DATABASE_URL").unwrap();
    let (prefix, _) = base.rsplit_once('/').unwrap();
    format!("{prefix}/{name}")
}
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in walk(dir) {
        let relative = entry
            .strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        out.push((relative, std::fs::read(&entry).unwrap()));
    }
    out.sort();
    out
}
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
/// The manifest's creation time is the host clock; everything else must match.
fn without_created_at(mut files: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    for (path, bytes) in &mut files {
        if path == "manifest.json" {
            let mut manifest: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            manifest["created_at_ms"] = 0.into();
            *bytes = serde_json::to_vec(&manifest).unwrap();
        }
    }
    files
}

#[test]
fn invalid_arguments_and_archives_exit_one_before_any_connection() {
    let unreachable = "postgres://nobody@127.0.0.1:1/none";
    let usage = server(
        unreachable,
        &["export", "--org", "org_00000000000000000000000001"],
    );
    assert_eq!(usage.status.code(), Some(1));
    assert!(text(&usage).contains("export.project_required"));
    let empty = scratch("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let missing = server(unreachable, &["import", "--in", empty.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing).contains("archive.missing_file (manifest.json)"));
    std::fs::remove_dir_all(&empty).unwrap();
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn export_import_round_trip_through_the_binary() {
    let mut source = database::TestDatabase::create_from_env().await.unwrap();
    let mut target = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(scenario(&source, &target))
        .catch_unwind()
        .await;
    target.finish(Ok(())).await;
    source.finish(result).await;
}

async fn scenario(source: &database::TestDatabase, target: &database::TestDatabase) {
    let mut admin = runbook_store_postgres::PostgresStore::connect(source.config())
        .await
        .unwrap();
    admin.migrate().await.unwrap();
    PostgresDirectory::new(source.config().clone())
        .commit(DirectoryCommit {
            org_id: scope().org_id,
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: "Org".into(),
                },
                DirectoryMutation::CreateProject(ProjectRecord {
                    id: scope().project_id,
                    org_id: scope().org_id,
                    slug: "api".into(),
                    title: "API".into(),
                }),
            ],
        })
        .await
        .unwrap();
    let store = PostgresExecutionStore::new(source.config().clone(), scope());
    let runbook = "rb_00000000000000000000000001".parse().unwrap();
    store
        .create_runbook(
            RunbookRecord {
                runbook_id: runbook,
                slug: "deploy".into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            SPEC.into(),
        )
        .await
        .unwrap();
    let prepared = prepare(
        "deploy",
        SPEC,
        &Default::default(),
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    publish_draft(
        &store,
        &store,
        &"rb_00000000000000000000000001".parse().unwrap(),
        1,
        prepared,
    )
    .await
    .unwrap();
    let webhooks = PostgresWebhooks::new(source.config().clone());
    let now = webhooks.webhook_time_ms().await.unwrap();
    webhooks
        .create_subscription(
            &scope(),
            Subscription {
                id: "whk_00000000000000000000000001".parse().unwrap(),
                scope: scope(),
                name: "Pager".into(),
                url: "https://hooks.example.test/x".into(),
                events: vec!["*".into()],
                runbooks: vec![],
                tags: vec![],
                enabled: true,
                secret: SealedSecret(SECRET.into()),
                previous_secret: None,
                created_at_ms: now,
                updated_at_ms: now,
                secret_rotated_at_ms: now,
                failing_since_ms: None,
                last_failure_at_ms: None,
                disabled_reason: None,
                disabled_at_ms: None,
                revision: 0,
                notice: None,
            },
            20,
            None,
        )
        .await
        .unwrap();

    let org = scope().org_id.to_string();
    let (from, to) = (url_for(source.name()), url_for(target.name()));
    let first = scratch("first");
    let unknown = server(
        &from,
        &[
            "export",
            "--org",
            &org,
            "--project",
            "nope",
            "--out",
            first.to_str().unwrap(),
        ],
    );
    assert_eq!(unknown.status.code(), Some(1));
    assert!(text(&unknown).contains("export.project_not_found"));

    // Export by slug; the summary names IDs and counts only.
    let exported = server(
        &from,
        &[
            "export",
            "--org",
            &org,
            "--project",
            "api",
            "--out",
            first.to_str().unwrap(),
        ],
    );
    assert!(exported.status.success(), "{}", text(&exported));
    assert!(text(&exported).contains("Exported project prj_00000000000000000000000001"));
    let all: Vec<u8> = files(&first).into_iter().flat_map(|(_, b)| b).collect();
    assert!(!all.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));

    let other_org = "org_00000000000000000000000009";
    let wrong = server(
        &to,
        &[
            "import",
            "--in",
            first.to_str().unwrap(),
            "--org",
            other_org,
        ],
    );
    assert_eq!(wrong.status.code(), Some(1));
    assert!(text(&wrong).contains("import.org_mismatch"));

    let tampered = scratch("tampered");
    std::fs::create_dir_all(&tampered).unwrap();
    for (path, bytes) in files(&first) {
        let dest = tampered.join(&path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let mut bytes = bytes;
        if path == "runbooks.ndjson" {
            bytes[10] ^= 1;
        }
        std::fs::write(dest, bytes).unwrap();
    }
    let rejected = server(&to, &["import", "--in", tampered.to_str().unwrap()]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(text(&rejected).contains("archive.digest_mismatch (runbooks.ndjson)"));

    let imported = server(
        &to,
        &["import", "--in", first.to_str().unwrap(), "--org", &org],
    );
    assert!(imported.status.success(), "{}", text(&imported));
    assert!(text(&imported).contains("Imported project prj_00000000000000000000000001"));
    // The organization now exists: refused unless the operator opts in,
    // and then refused again because the project is not empty.
    let again = server(&to, &["import", "--in", first.to_str().unwrap()]);
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again).contains("import.org_exists"));
    let opted = server(
        &to,
        &[
            "import",
            "--in",
            first.to_str().unwrap(),
            "--into-existing-org",
        ],
    );
    assert_eq!(opted.status.code(), Some(1));
    assert!(text(&opted).contains("import.target_not_empty"));

    let second = scratch("second");
    let reexported = server(
        &to,
        &[
            "export",
            "--org",
            &org,
            "--project",
            "prj_00000000000000000000000001",
            "--out",
            second.to_str().unwrap(),
        ],
    );
    assert!(reexported.status.success(), "{}", text(&reexported));
    assert_eq!(
        without_created_at(files(&second)),
        without_created_at(files(&first))
    );
    for output in [&exported, &imported, &reexported, &again, &rejected] {
        assert!(!text(output).contains(SECRET));
        assert!(!text(output).contains("postgres://"));
    }
    for dir in [first, second, tampered] {
        std::fs::remove_dir_all(dir).unwrap();
    }
}
