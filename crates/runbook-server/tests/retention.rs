//! ARC2 on PostgreSQL through the real router and drain (PRD §14, D53–D54):
//! daily archives under a moved store clock (the debug-only test offset),
//! retention deleting live detail only after the archive exists, nothing
//! deleted while retention is unset, an amendment's stale mark and rewrite,
//! a redacted value never in the archive, rerun after expiry, and another
//! organization never seeing an archive.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::FutureExt;
use runbook_core::{
    archives::{ArchiveIndex, DAY_MS, RETENTION_REASON, import, read, zip},
    blob::BlobStore,
    catalog::TenantScope,
    retention::RunStore,
};
use runbook_model::{ExecutionId, RunbookId};
use runbook_server::{
    config::{RetentionConfig, ServerConfig},
    drain::{DrainSettings, DrainStats, Drainer},
    http::{Worker, router},
};
use runbook_store_postgres::PostgresExecutionStore;
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, time::Duration};
use tower::ServiceExt;

const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
metadata: {id: deploy, title: Deploy}\n\
spec:\n  steps:\n    - id: ship\n      title: Ship it\n      actor: agent\n";
const SECRET: &str = "hunter2-correct-horse";

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub text: String,
    pub bytes: Vec<u8>,
    pub headers: axum::http::HeaderMap,
}
pub async fn call(
    app: &Router,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> Reply {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    let body = if body.is_null() {
        Body::empty()
    } else {
        request = request.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 64 * 1_048_576)
        .await
        .unwrap()
        .to_vec();
    Reply {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
        bytes,
        headers,
    }
}

pub struct Fixture {
    _worker: Worker,
    pub app: Router,
    pub config: ServerConfig,
    pub owner: String,
    pub scope: TenantScope,
    pub runbook: RunbookId,
}
impl Fixture {
    async fn new(db: &database::TestDatabase, retention: RetentionConfig) -> Self {
        let mut config = ServerConfig::new(
            db.config().clone(),
            "https://runbooks.example",
            "127.0.0.1:0",
        )
        .unwrap();
        config.retention = retention;
        let issued = runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let worker = Worker::start(config.clone()).unwrap();
        let app = router(worker.client());
        let owner: String = issued["pat"].as_str().unwrap().into();
        let project = issued["project_id"].as_str().unwrap().to_owned();
        let created = call(
            &app,
            Some(&owner),
            "POST",
            &format!("/v1/projects/{project}/runbooks"),
            json!({"source": SPEC}),
            Some("create"),
        )
        .await;
        assert_eq!(created.status, 201, "{}", created.text);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = call(
            &app,
            Some(&owner),
            "GET",
            &format!("/v1/runbooks/{rb}/draft"),
            Value::Null,
            None,
        )
        .await;
        let etag = draft.headers["etag"].to_str().unwrap().to_owned();
        let published = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/runbooks/{rb}/versions"))
                    .header("authorization", format!("Bearer {owner}"))
                    .header("idempotency-key", "publish")
                    .header("if-match", etag)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(published.status(), 201);
        Self {
            _worker: worker,
            app,
            owner,
            scope: TenantScope {
                org_id: issued["org_id"].as_str().unwrap().parse().unwrap(),
                project_id: project.parse().unwrap(),
            },
            runbook: rb.parse().unwrap(),
            config,
        }
    }
    pub async fn owner(&self, method: &str, path: &str, body: Value, key: Option<&str>) -> Reply {
        call(&self.app, Some(&self.owner), method, path, body, key).await
    }
    fn drainer(&self, offset_days: i64) -> Drainer {
        let mut config = self.config.clone();
        config.retention.test_clock_offset_ms = offset_days * DAY_MS;
        Drainer::new(
            config,
            DrainSettings {
                batch: 64,
                lease_ms: 30_000,
                idle: Duration::from_millis(10),
            },
        )
    }
    async fn drain(&self) {
        let mut drainer = self.drainer(0);
        for _ in 0..20 {
            if !drainer.drain_once().await.unwrap().busy() {
                break;
            }
        }
    }
    /// The archive job, with the store clock moved `days` ahead.
    pub async fn archive(&self, days: i64) -> DrainStats {
        self.drainer(days).archive_once().await.unwrap()
    }
    pub fn store(&self) -> PostgresExecutionStore {
        PostgresExecutionStore::new(self.config.database.clone(), self.scope.clone())
    }
    /// One agent run to completion; its logs carry a secret the server must
    /// redact before storing (D34).
    pub async fn finished_run(&self, n: u8) -> ExecutionId {
        let project = &self.scope.project_id;
        let started = self
            .owner(
                "POST",
                &format!("/v1/projects/{project}/executions"),
                json!({"runbook": "deploy"}),
                Some(&format!("start-{n}")),
            )
            .await;
        assert_eq!(started.status, 201, "{}", started.text);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        self.drain().await;
        let minted = self
            .owner(
                "POST",
                &format!("/v1/executions/{exe}/tokens"),
                json!({"session_id": format!("agent-{n}")}),
                Some(&format!("mint-{n}")),
            )
            .await;
        assert_eq!(minted.status, 201, "{}", minted.text);
        let agent = minted.body["access_token"].as_str().unwrap().to_owned();
        let brief = call(
            &self.app,
            Some(&agent),
            "GET",
            &format!("/v1/executions/{exe}/next"),
            Value::Null,
            None,
        )
        .await;
        let sr = brief.body["ready_work"][0]["step_run_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let claimed = call(
            &self.app,
            Some(&agent),
            "POST",
            &format!("/v1/step-runs/{sr}/claim"),
            json!({"session_id": format!("agent-{n}"), "actor": {"kind": "agent", "agent_name": "claude", "model": "opus"}}),
            Some(&format!("claim-{n}")),
        )
        .await;
        assert_eq!(claimed.status, 200, "{}", claimed.text);
        let report = call(
            &self.app,
            Some(&agent),
            "POST",
            &format!("/v1/step-runs/{sr}/report"),
            json!({"step_run_id": sr, "claim": claimed.body, "status": "succeeded", "verdict": "pass",
                   "summary": "shipped", "evidence": [],
                   "logs": ["deploying build 42", format!("password={SECRET}")]}),
            Some(&format!("report-{n}")),
        )
        .await;
        assert_eq!(report.status, 200, "{}", report.text);
        self.drain().await;
        let read = self
            .owner("GET", &format!("/v1/executions/{exe}"), Value::Null, None)
            .await;
        assert_eq!(read.body["status"], "succeeded", "{}", read.text);
        exe.parse().unwrap()
    }
    /// The bytes the index names (they hash to its digest).
    pub async fn file(&self, day: i64) -> Vec<u8> {
        let record = self
            .store()
            .get_archive(&self.scope, &self.runbook, day)
            .await
            .unwrap()
            .expect("the day has a file");
        let bytes = self.store().get(&read::record_key(&record)).await.unwrap();
        assert_eq!(runbook_core::portable::sha256(&bytes), record.digest);
        bytes
    }
    pub async fn today(&self) -> i64 {
        runbook_core::archives::day_of(self.store().backend_time_ms().await.unwrap())
    }
}

pub async fn with_fixture(retention: RetentionConfig, f: impl AsyncFnOnce(&Fixture)) {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let fixture = Fixture::new(&db, retention).await;
        f(&fixture).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

fn retention(days: u32) -> RetentionConfig {
    let mut config = RetentionConfig::default();
    config.defaults.live_days = Some(days);
    config
}

/// Every entry of a ZIP, inflated, joined: what "never in the archive"
/// is checked against.
fn inflated(bytes: &[u8]) -> String {
    let zip = zip::Zip::parse(bytes).unwrap();
    zip.entries
        .iter()
        .map(|e| String::from_utf8_lossy(&zip.read(&e.name).unwrap().unwrap()).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn retention_deletes_live_detail_only_after_the_archive_exists() {
    with_fixture(retention(90), async |f| {
        let exe = f.finished_run(1).await;
        let today = f.today().await;
        // Today's runs are not archived yet, and nothing is past retention.
        let stats = f.archive(0).await.archive;
        assert_eq!((stats.written, stats.live_deleted), (0, 0));
        // After 00:00 UTC: the day's file is written, the run stays live.
        let stats = f.archive(1).await.archive;
        assert_eq!((stats.written, stats.live_deleted, stats.failed), (1, 0, 0));
        let record = f
            .store()
            .get_archive(&f.scope, &f.runbook, today)
            .await
            .unwrap()
            .unwrap();
        assert!(!record.stale);
        let file = f.file(today).await;
        assert_eq!(record.size_bytes, file.len() as u64);
        let live = f
            .owner("GET", &format!("/v1/executions/{exe}"), Value::Null, None)
            .await;
        assert_eq!(live.status, 200);
        // A redacted value never survives in the archive (D34 + PRD §14).
        let text = inflated(&file);
        assert!(!text.contains(SECRET), "the secret reached the archive");
        assert!(text.contains("[REDACTED:password]"), "{text}");
        assert!(text.contains("deploying build 42"));
        // No credential reaches an archive: the agent minted an execution
        // pair and claimed with it, yet no secret of any kind is in the file
        // (actor replays hold claim refs and principal labels only).
        for prefix in ["trb_exe_", "trb_exr_", "trb_pat_", "trb_svc_"] {
            assert!(!text.contains(prefix), "{prefix} secret in the archive");
        }
        // The report is the same Markdown export the report route serves.
        let route = f
            .owner(
                "GET",
                &format!("/v1/executions/{exe}/report?format=markdown"),
                Value::Null,
                None,
            )
            .await;
        let folder = read::archived_run(&f.store(), &f.scope, &exe)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read::report_markdown(&folder.file.unwrap().1).unwrap(),
            route.text
        );
        // Repeating the pass changes nothing (idempotent per runbook and day).
        assert!(!f.archive(1).await.archive.busy());
        assert_eq!(f.file(today).await, file);
        // Ninety-one days later the live copy goes; file and summary stay.
        let stats = f.archive(91).await.archive;
        assert_eq!((stats.live_deleted, stats.failed), (1, 0));
        let tombstone = f.store().tombstone(&exe).await.unwrap().unwrap();
        assert_eq!(tombstone.reason, RETENTION_REASON);
        assert_eq!(f.file(today).await, file, "the file is the copy");
        let gone = f
            .owner(
                "GET",
                &format!("/v1/executions/{exe}/report"),
                Value::Null,
                None,
            )
            .await;
        assert_eq!(
            (gone.status, gone.body["code"].as_str()),
            (410, Some("execution.history_expired"))
        );
        // PRD §14: a run reduced to its summary row cannot be rerun.
        let rerun = f
            .owner(
                "POST",
                &format!("/v1/executions/{exe}/rerun"),
                json!({}),
                Some("rerun-expired"),
            )
            .await;
        assert_eq!(
            (rerun.status, rerun.body["code"].as_str()),
            (422, Some("execution.inputs_expired")),
            "{}",
            rerun.text
        );
        // The archive still opens by range and imports elsewhere.
        assert!(
            read::archived_run(&f.store(), &f.scope, &exe)
                .await
                .unwrap()
                .is_some()
        );
        let (_, data) = import::to_project_data(&file).unwrap();
        assert_eq!(data.rows.runs[0].slice.run.projection.execution_id, exe);
        // A run deleted for another reason keeps the 410.
        let second = f.finished_run(2).await;
        f.store()
            .delete_execution(&second, "privacy request")
            .await
            .unwrap();
        let rerun = f
            .owner(
                "POST",
                &format!("/v1/executions/{second}/rerun"),
                json!({}),
                Some("rerun-deleted"),
            )
            .await;
        assert_eq!(rerun.status, 410, "{}", rerun.text);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn with_retention_unset_nothing_is_deleted() {
    with_fixture(RetentionConfig::default(), async |f| {
        let exe = f.finished_run(1).await;
        let stats = f.archive(1_000).await.archive;
        assert_eq!(stats.written, 1, "archives are still written daily");
        assert_eq!(
            stats.live_deleted + stats.archives_expired + stats.summaries_purged,
            0
        );
        assert_eq!(f.store().tombstone(&exe).await.unwrap(), None);
        let live = f
            .owner("GET", &format!("/v1/executions/{exe}"), Value::Null, None)
            .await;
        assert_eq!(live.status, 200);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn an_amendment_marks_the_day_stale_and_the_next_pass_rewrites_it() {
    with_fixture(retention(90), async |f| {
        let exe = f.finished_run(1).await;
        let today = f.today().await;
        f.archive(1).await;
        let before = f.store().get_archive(&f.scope, &f.runbook, today).await.unwrap().unwrap();
        let doc = f.owner("GET", &format!("/v1/executions/{exe}/report"), Value::Null, None).await;
        let sr = doc.body["sections"][0]["step_run_id"].as_str().unwrap().to_owned();
        let amended = f
            .owner(
                "POST",
                &format!("/v1/step-runs/{sr}/overrides"),
                json!({"to": {"status": "succeeded", "verdict": "fail", "outputs": {}, "summary": "rolled back after all"},
                       "reason": "postmortem"}),
                Some("amend"),
            )
            .await;
        assert_eq!(amended.status, 200, "{}", amended.text);
        let stale = f.store().get_archive(&f.scope, &f.runbook, today).await.unwrap().unwrap();
        assert!(stale.stale, "the amendment's own transaction marks the file");
        // Retention never deletes a run whose file owes a rewrite: the pass
        // rewrites first, then deletes against the current file.
        let stats = f.archive(1).await.archive;
        assert_eq!((stats.written, stats.failed), (1, 0));
        let after = f.store().get_archive(&f.scope, &f.runbook, today).await.unwrap().unwrap();
        assert!(!after.stale);
        assert_ne!(after.digest, before.digest);
        let text = inflated(&f.file(today).await);
        assert!(text.contains("rolled back after all") && text.contains("postmortem"));
    })
    .await;
}

/// A read-only service token in another organization (or another project
/// of this one), with executions:read on its own project.
async fn outsider(f: &Fixture, foreign_org: bool) -> String {
    use runbook_core::{
        credentials::*,
        directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord},
    };
    use runbook_model::{OrganizationId, ProjectId, ProjectRole};
    let n: u8 = if foreign_org { 76 } else { 75 };
    let org: OrganizationId = if foreign_org {
        format!("org_{n:026}").parse().unwrap()
    } else {
        f.scope.org_id.clone()
    };
    let project: ProjectId = format!("prj_{n:026}").parse().unwrap();
    let directory = runbook_store_postgres::PostgresDirectory::new(f.config.database.clone());
    let mut mutations = vec![];
    let revision = if foreign_org {
        mutations.push(DirectoryMutation::CreateOrganization {
            title: "Foreign".into(),
        });
        0
    } else {
        directory.snapshot(&org).await.unwrap().revision
    };
    mutations.push(DirectoryMutation::CreateProject(ProjectRecord {
        id: project.clone(),
        org_id: org.clone(),
        slug: "elsewhere".into(),
        title: "Elsewhere".into(),
    }));
    directory
        .commit(DirectoryCommit {
            org_id: org.clone(),
            expected_revision: revision,
            mutations,
        })
        .await
        .unwrap();
    let secret = encode_secret(TokenKind::Service, [n; 32]);
    runbook_store_postgres::PostgresCredentials::new(f.config.database.clone())
        .insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Service,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::Service {
                project_id: project,
                role: ProjectRole::Viewer,
            },
            org_id: org,
            scopes: [Scope::ExecutionsRead].into(),
            projects: None,
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    secret
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn archive_routes_list_download_bundle_open_and_import() {
    with_fixture(retention(90), async |f| {
        let (one, two) = (f.finished_run(1).await, f.finished_run(2).await);
        let today = f.today().await;
        let date = runbook_core::archives::date_of(today);
        let rb = &f.runbook;
        let list = f.owner("GET", &format!("/v1/runbooks/{rb}/archives"), Value::Null, None).await;
        assert_eq!((list.status, list.body["items"].clone()), (200, json!([])), "{}", list.text);
        f.archive(1).await;
        let file = f.file(today).await;
        let list = f.owner("GET", &format!("/v1/runbooks/{rb}/archives"), Value::Null, None).await;
        assert_eq!(list.status, 200, "{}", list.text);
        let item = &list.body["items"][0];
        assert_eq!(item["date"], date.as_str());
        assert_eq!(item["run_count"], 2);
        assert_eq!(item["size_bytes"], file.len());
        assert_eq!(item["stale"], false);
        assert_eq!(item["expires_at_ms"], Value::Null, "files are kept indefinitely by default");
        assert_eq!(item["sha256"], runbook_core::portable::sha256_hex(&file));
        let bounded = f
            .owner("GET", &format!("/v1/runbooks/{rb}/archives?from={date}&to={date}"), Value::Null, None)
            .await;
        assert_eq!(bounded.body["items"].as_array().unwrap().len(), 1);
        let later = runbook_core::archives::date_of(today + 1);
        let none = f.owner("GET", &format!("/v1/runbooks/{rb}/archives?from={later}"), Value::Null, None).await;
        assert_eq!(none.body["items"], json!([]));
        for bad in [
            "?from=2026-02-30",
            "?from=2026-01-02&to=2026-01-01",
            "?include_evidence=true",
            "?format=zip",
            "?format=xml",
            "?limit=3",
        ] {
            let r = f.owner("GET", &format!("/v1/runbooks/{rb}/archives{bad}"), Value::Null, None).await;
            assert_eq!((r.status, r.body["code"].as_str()), (400, Some("request.invalid_query")), "{bad}");
        }
        // One day, exactly as stored.
        let day = f.owner("GET", &format!("/v1/runbooks/{rb}/archives/{date}"), Value::Null, None).await;
        assert_eq!(day.status, 200);
        assert_eq!(day.bytes, file);
        assert_eq!(day.headers["content-type"], "application/zip");
        assert!(day.headers["content-disposition"].to_str().unwrap().contains(&format!("{rb}-{date}.zip")));
        for missing in [later.as_str(), "2026-13-01", "today"] {
            let r = f.owner("GET", &format!("/v1/runbooks/{rb}/archives/{missing}"), Value::Null, None).await;
            assert_eq!(r.status, 404, "{missing}");
        }
        // The from/to bundle, with evidence (none here) listed.
        let bundle = f
            .owner(
                "GET",
                &format!("/v1/runbooks/{rb}/archives?format=zip&from={date}&to={later}&include_evidence=true"),
                Value::Null,
                None,
            )
            .await;
        assert_eq!(bundle.status, 200, "{}", bundle.text);
        let zip = zip::Zip::parse(&bundle.bytes).unwrap();
        assert_eq!(zip.read(&format!("{date}.zip")).unwrap().unwrap(), file);
        let head: Value = serde_json::from_slice(&zip.read("bundle.json").unwrap().unwrap()).unwrap();
        assert_eq!(head["days"].as_array().unwrap().len(), 1);
        assert_eq!(head["include_evidence"], true);
        let far = runbook_core::archives::date_of(today + 100);
        let r = f
            .owner("GET", &format!("/v1/runbooks/{rb}/archives?format=zip&from={date}&to={far}"), Value::Null, None)
            .await;
        assert_eq!((r.status, r.body["code"].as_str()), (422, Some("archive.range_too_large")));
        // An archived run opens read-only, by range, live or not.
        let opened = f.owner("GET", &format!("/v1/executions/{one}/archive"), Value::Null, None).await;
        assert_eq!(opened.status, 200, "{}", opened.text);
        assert_eq!(opened.body["live"], true);
        assert_eq!(opened.body["date"], date.as_str());
        assert_eq!(opened.body["status"], "succeeded");
        let report = f
            .owner("GET", &format!("/v1/executions/{one}/report?format=markdown"), Value::Null, None)
            .await;
        assert_eq!(opened.body["report_markdown"], report.text.as_str());
        assert!(opened.body["files"].as_array().unwrap().iter().any(|f| f["path"] == "report.md"));
        let markdown = f
            .owner("GET", &format!("/v1/executions/{one}/archive?format=markdown"), Value::Null, None)
            .await;
        assert_eq!(markdown.text, report.text);
        f.archive(91).await;
        let after = f.owner("GET", &format!("/v1/executions/{two}/archive"), Value::Null, None).await;
        assert_eq!((after.status, after.body["live"].clone()), (200, json!(false)), "{}", after.text);
        // Tenancy: another organization, and another project of this one,
        // see none of it (404, never 403).
        for foreign in [true, false] {
            let token = outsider(f, foreign).await;
            for path in [
                format!("/v1/runbooks/{rb}/archives"),
                format!("/v1/runbooks/{rb}/archives/{date}"),
                format!("/v1/runbooks/{rb}/archives?format=zip&from={date}&to={date}"),
                format!("/v1/executions/{one}/archive"),
            ] {
                let r = call(&f.app, Some(&token), "GET", &path, Value::Null, None).await;
                assert_eq!((r.status, r.body["code"].as_str()), (404, Some("resource.not_found")), "{path}");
            }
        }
        // The downloaded day imports through the D3 importer into an empty
        // instance (`runbook-server import --in <day.zip>`).
        let mut target = database::TestDatabase::create_from_env().await.unwrap();
        let path = std::env::temp_dir().join(format!("runbook-arc3-{}-{date}.zip", std::process::id()));
        std::fs::write(&path, &day.bytes).unwrap();
        let imported = AssertUnwindSafe(async {
            let message = runbook_server::archive::import(
                target.config(),
                &path,
                None,
                runbook_core::portable::ImportOptions::default(),
            )
            .await
            .unwrap();
            assert!(message.contains("2 runs"), "{message}");
            let store = PostgresExecutionStore::new(target.config().clone(), f.scope.clone());
            use runbook_core::ExecutionStore;
            let run = store.load(&one).await.unwrap();
            assert_eq!(run.projection.execution_id, one);
        })
        .catch_unwind()
        .await;
        let _ = std::fs::remove_file(&path);
        target.finish(imported).await;
    })
    .await;
}

/// Finding 9: a stale day may still hold a run deleted on request, so it is
/// not served until the job rewrites it.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_stale_day_is_not_served_until_rewritten() {
    with_fixture(retention(90), async |f| {
        let exe = f.finished_run(1).await;
        let today = f.today().await;
        let date = runbook_core::archives::date_of(today);
        let rb = &f.runbook;
        f.archive(1).await;
        let doc = f.owner("GET", &format!("/v1/executions/{exe}/report"), Value::Null, None).await;
        let sr = doc.body["sections"][0]["step_run_id"].as_str().unwrap().to_owned();
        let amended = f
            .owner(
                "POST",
                &format!("/v1/step-runs/{sr}/overrides"),
                json!({"to": {"status": "succeeded", "verdict": "fail", "outputs": {}, "summary": "redo"},
                       "reason": "review"}),
                Some("amend"),
            )
            .await;
        assert_eq!(amended.status, 200, "{}", amended.text);
        for path in [
            format!("/v1/runbooks/{rb}/archives/{date}"),
            format!("/v1/runbooks/{rb}/archives/{date}?include_evidence=true"),
            format!("/v1/runbooks/{rb}/archives?format=zip&from={date}&to={date}"),
        ] {
            let r = f.owner("GET", &path, Value::Null, None).await;
            assert_eq!((r.status, r.body["code"].as_str()), (409, Some("archive.rewrite_pending")), "{path}");
            assert_eq!(r.headers["retry-after"], "900", "{path}");
        }
        let list = f.owner("GET", &format!("/v1/runbooks/{rb}/archives"), Value::Null, None).await;
        assert_eq!(list.body["items"][0]["stale"], true);
        f.archive(1).await;
        let day = f.owner("GET", &format!("/v1/runbooks/{rb}/archives/{date}"), Value::Null, None).await;
        assert_eq!(day.status, 200);
        assert_eq!(day.bytes, f.file(today).await);
    })
    .await;
}

/// The job's writer for one day of this fixture's runbook, on Postgres, at
/// `now_ms` on the store clock.
async fn write_day(
    f: &Fixture,
    store: &PostgresExecutionStore,
    day: i64,
    now_ms: i64,
) -> Result<runbook_core::archives::job::DayOutcome, runbook_core::StoreError> {
    let reports = runbook_server::drain::ArchiveReports(
        runbook_server::executions::PostgresExecutionBackend {
            config: f.config.database.clone(),
            objects: None,
        },
    );
    runbook_core::archives::job::write_day(
        store,
        &runbook_store_postgres::PostgresDirectory::new(f.config.database.clone()),
        &reports,
        &f.scope,
        &f.runbook,
        day,
        now_ms,
        &f.config.retention.policy_for(&f.scope.project_id),
    )
    .await
}

/// Every stored version of the day's file, inflated and joined.
async fn every_version(f: &Fixture, day: i64) -> Vec<String> {
    let store = f.store();
    let mut out = Vec::new();
    for key in store
        .list_prefix(&runbook_core::blob::BlobPrefix::Archive {
            runbook_id: f.runbook.clone(),
            day,
        })
        .await
        .unwrap()
    {
        out.push(inflated(&store.get(&key).await.unwrap()));
    }
    out
}

async fn amend(f: &Fixture, exe: &ExecutionId, key: &str) {
    let doc = f
        .owner(
            "GET",
            &format!("/v1/executions/{exe}/report"),
            Value::Null,
            None,
        )
        .await;
    let sr = doc.body["sections"][0]["step_run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let amended = f
        .owner(
            "POST",
            &format!("/v1/step-runs/{sr}/overrides"),
            json!({"to": {"status": "succeeded", "verdict": "fail", "outputs": {}, "summary": key},
                   "reason": key}),
            Some(key),
        )
        .await;
    assert_eq!(amended.status, 200, "{}", amended.text);
}

/// The swap on Postgres (findings 2 and 5 of both reviews): a run deleted on
/// request while its day is rewritten never stays in a stored version, and
/// two writers interleaving put and record leave the index naming stored,
/// current bytes.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_archive_swap_holds_under_races_on_postgres() {
    with_fixture(retention(90), async |f| {
        let (one, two) = (f.finished_run(1).await, f.finished_run(2).await);
        let today = f.today().await;
        let now = (today + 1) * DAY_MS;
        f.archive(1).await;
        // Privacy deletion inside a rewrite.
        amend(f, &two, "late fix").await;
        let store = f.store();
        let deleter = f.store();
        let doomed = one.clone();
        store.before_next_archive_put(move || {
            Box::pin(async move {
                deleter
                    .delete_execution(&doomed, "privacy request")
                    .await
                    .unwrap();
            })
        });
        write_day(f, &store, today, now).await.unwrap();
        for text in every_version(f, today).await {
            assert!(
                !text.contains(one.as_str()),
                "the deleted run survived in a stored file"
            );
        }
        let _ = f.file(today).await; // the index names stored bytes
        // Two writers: B completes a whole rewrite (after one more
        // amendment) between A's build and A's put.
        amend(f, &two, "second fix").await;
        let (a, b) = (f.store(), f.store());
        let config = f.config.clone();
        let scope = f.scope.clone();
        let runbook = f.runbook.clone();
        a.before_next_archive_put(move || {
            Box::pin(async move {
                let reports = runbook_server::drain::ArchiveReports(
                    runbook_server::executions::PostgresExecutionBackend {
                        config: config.database.clone(),
                        objects: None,
                    },
                );
                let outcome = runbook_core::archives::job::write_day(
                    &b,
                    &runbook_store_postgres::PostgresDirectory::new(config.database.clone()),
                    &reports,
                    &scope,
                    &runbook,
                    today,
                    now + 1,
                    &config.retention.policy_for(&scope.project_id),
                )
                .await
                .unwrap();
                assert!(matches!(
                    outcome,
                    runbook_core::archives::job::DayOutcome::Written(_)
                        | runbook_core::archives::job::DayOutcome::Unchanged
                ));
            })
        });
        let outcome = write_day(f, &a, today, now + 2).await.unwrap();
        assert!(matches!(
            outcome,
            runbook_core::archives::job::DayOutcome::Unchanged
                | runbook_core::archives::job::DayOutcome::Written(_)
        ));
        let record = f
            .store()
            .get_archive(&f.scope, &f.runbook, today)
            .await
            .unwrap()
            .unwrap();
        assert!(!record.stale);
        let bytes = f.file(today).await;
        assert!(
            inflated(&bytes).contains("second fix"),
            "the file holds every amendment"
        );
        // Retention only relies on a stored file.
        assert!(f.archive(91).await.archive.live_deleted >= 1);
        let _ = f.file(today).await;
    })
    .await;
}

/// Re-check finding 5 on Postgres: a privacy deletion of a retained run
/// that lands inside a rewrite takes it out; the rewrite never restores it.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_purge_during_a_rewrite_takes_the_run_out_on_postgres() {
    with_fixture(retention(90), async |f| {
        let (one, two) = (f.finished_run(1).await, f.finished_run(2).await);
        let today = f.today().await;
        f.archive(1).await;
        assert_eq!(f.archive(91).await.archive.live_deleted, 2);
        let store = f.store();
        assert!(
            store
                .mark_archive_stale(&f.scope, &f.runbook, today)
                .await
                .unwrap()
        );
        let purger = f.store();
        let scope = f.scope.clone();
        let doomed = one.clone();
        store.before_next_archive_put(move || {
            Box::pin(async move {
                purger
                    .purge_retained_run(&scope, &doomed, "privacy request")
                    .await
                    .unwrap()
                    .expect("retained");
            })
        });
        write_day(f, &store, today, (today + 92) * DAY_MS)
            .await
            .unwrap();
        let file = f.file(today).await;
        let text = inflated(&file);
        assert!(!text.contains(one.as_str()));
        assert!(text.contains(two.as_str()));
        for text in every_version(f, today).await {
            assert!(!text.contains(one.as_str()));
        }
        assert!(
            !f.store()
                .get_archive(&f.scope, &f.runbook, today)
                .await
                .unwrap()
                .unwrap()
                .stale
        );
    })
    .await;
}
