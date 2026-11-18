#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::FutureExt;
use runbook_core::{credentials::*, directory::*};
use runbook_model::*;
use runbook_server::{
    config::ServerConfig,
    http::{Worker, router},
};
use runbook_store_postgres::{PostgresCredentials, PostgresDirectory};
use serde_json::{Value, json};
use std::panic::AssertUnwindSafe;
use tower::ServiceExt;
struct Fixture {
    config: ServerConfig,
    worker: Worker,
    app: Router,
    token: String,
    org: OrganizationId,
    user: UserId,
    project: ProjectId,
}
struct Reply {
    status: u16,
    body: Value,
    etag: Option<String>,
}
async fn call(
    app: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
    etag: Option<&str>,
) -> Reply {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"));
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    if let Some(etag) = etag {
        request = request.header("if-match", etag);
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
    assert!(response.headers().contains_key("x-request-id"));
    let status = response.status().as_u16();
    if status >= 400 {
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
    }
    let etag = response
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().into());
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    Reply {
        status,
        // 204 and other empty answers read as null.
        body: if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
        etag,
    }
}
fn spec(slug: &str, title: &str, extra_required: bool) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {slug}, title: {title}}}\nspec:\n  execution: {{timeout: 2h, concurrency: {{key: '${{{{ inputs.service }}}}', limit: 1}}}}\n  inputs:\n    service: {{type: string, default: api}}\n{}  steps: [{{id: work, title: {title}}}]\n",
        if extra_required {
            "    extra: {type: string, required: true}\n"
        } else {
            ""
        }
    )
}
impl Fixture {
    async fn new(db: &database::TestDatabase) -> Self {
        Self::with_run_budget(db, None).await
    }
    /// CF13 review round 1: a fixture whose stores hold runs to `budget`.
    async fn with_run_budget(db: &database::TestDatabase, budget: Option<u64>) -> Self {
        let database = match budget {
            Some(bytes) => db.config().clone().with_run_budget(bytes),
            None => db.config().clone(),
        };
        Self::with_database(database).await
    }
    /// A fixture whose runs may open at most `cap` step runs
    /// (`RUNBOOK_LIMIT_STEP_RUNS`).
    async fn with_step_cap(db: &database::TestDatabase, cap: u64) -> Self {
        Self::with_database(db.config().clone().with_step_cap(Some(cap))).await
    }
    async fn with_database(database: runbook_store_postgres::DatabaseConfig) -> Self {
        let config =
            ServerConfig::new(database, "https://runbooks.example", "127.0.0.1:0").unwrap();
        let issued = runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let worker = Worker::start(config.clone()).unwrap();
        let app = router(worker.client());
        Self {
            config,
            worker,
            app,
            token: issued["pat"].as_str().unwrap().into(),
            org: issued["org_id"].as_str().unwrap().parse().unwrap(),
            user: issued["user_id"].as_str().unwrap().parse().unwrap(),
            project: issued["project_id"].as_str().unwrap().parse().unwrap(),
        }
    }
    async fn create(&self, slug: &str) -> String {
        let r = call(
            &self.app,
            &self.token,
            "POST",
            &format!("/v1/projects/{}/runbooks", self.project),
            json!({"source":spec(slug,"One",false)}),
            Some(slug),
            None,
        )
        .await;
        assert_eq!(r.status, 201, "{}", r.body);
        r.body["runbook_id"].as_str().unwrap().into()
    }
    async fn publish(&self, rb: &str, source: String, key: &str) -> Value {
        let path = format!("/v1/runbooks/{rb}/draft");
        let r = call(
            &self.app,
            &self.token,
            "GET",
            &path,
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(r.status, 200);
        let r = call(
            &self.app,
            &self.token,
            "PATCH",
            &path,
            json!({"source":source}),
            None,
            r.etag.as_deref(),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
        let r = call(
            &self.app,
            &self.token,
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            json!({}),
            Some(key),
            r.etag.as_deref(),
        )
        .await;
        assert_eq!(r.status, 201, "{}", r.body);
        r.body
    }
    async fn start(&self, body: Value, key: Option<&str>) -> Reply {
        call(
            &self.app,
            &self.token,
            "POST",
            &format!("/v1/projects/{}/executions", self.project),
            body,
            key,
            None,
        )
        .await
    }
    async fn get(&self, id: &str) -> Reply {
        call(
            &self.app,
            &self.token,
            "GET",
            &format!("/v1/executions/{id}"),
            Value::Null,
            None,
            None,
        )
        .await
    }
}
/// R25: every fixture owns a database plus server and store pools, and the
/// local Postgres allows 100 connections. Bound concurrent fixtures so a
/// growing suite never exhausts them (which surfaced as a spurious 503).
/// Each `#[tokio::test]` runs its own runtime on its own thread, so blocking
/// here before any async work is safe.
const MAX_FIXTURES: usize = 6;
static FIXTURES: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static FIXTURE_FREED: std::sync::Condvar = std::sync::Condvar::new();
struct FixtureSlot;
impl FixtureSlot {
    fn acquire() -> Self {
        let mut active = FIXTURES.lock().unwrap_or_else(|e| e.into_inner());
        while *active >= MAX_FIXTURES {
            active = FIXTURE_FREED
                .wait(active)
                .unwrap_or_else(|e| e.into_inner());
        }
        *active += 1;
        FixtureSlot
    }
}
impl Drop for FixtureSlot {
    fn drop(&mut self) {
        *FIXTURES.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        FIXTURE_FREED.notify_one();
    }
}
async fn with_fixture(f: impl AsyncFnOnce(&mut Fixture, &database::TestDatabase)) {
    let _slot = FixtureSlot::acquire();
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut fixture = Fixture::new(&db).await;
        f(&mut fixture, &db).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn start_pins_version_before_queue() {
    with_fixture(async |f, db| {
        let rb = f.create("deploy").await;
        let v1 = f.publish(&rb, spec("deploy", "One", false), "v1").await;
        let v2 = f.publish(&rb, spec("deploy", "Two", false), "v2").await;
        let a = f.start(json!({"runbook":"deploy"}), Some("a")).await;
        assert_eq!(a.status, 201, "{}", a.body);
        assert_eq!(a.body["runbook_version_id"], v2["runbook_version_id"]);
        assert_eq!(a.body["admission"]["state"], "admitted");
        let b = f.start(json!({"runbook":rb,"version":1}), Some("b")).await;
        assert_eq!(b.status, 201, "{}", b.body);
        assert_eq!(b.body["runbook_version_id"], v1["runbook_version_id"]);
        assert_eq!(b.body["admission"]["state"], "queued");
        let c = f.start(json!({"runbook":"deploy"}), Some("c")).await;
        assert_eq!(c.status, 201);
        assert_eq!(c.body["inputs"]["service"], "api");
        assert_eq!(c.body["steps"], json!([]));
        assert!(c.body["execution_deadline_at_ms"].is_null());
        f.publish(&rb, spec("deploy", "Three", false), "v3").await;
        let read = f.get(c.body["id"].as_str().unwrap()).await;
        assert_eq!(read.status, 200);
        assert_eq!(read.body["runbook_version_id"], v2["runbook_version_id"]);
        assert_eq!(read.body["inputs"]["service"], "api");
        use runbook_core::{
            authoring::{AuthoringFactory, AuthoringStore},
            catalog::TenantScope,
            publish::{prepare, publish_preview},
        };
        let factory = runbook_store_postgres::PostgresAuthoring::new(db.config().clone());
        let authoring = factory.scoped(TenantScope {
            org_id: f.org.clone(),
            project_id: f.project.clone(),
        });
        let preview = publish_preview(
            authoring.catalog(),
            authoring.catalog(),
            &rb.parse().unwrap(),
            prepare(
                "deploy",
                &spec("deploy", "Preview", false),
                &Default::default(),
                format!("rbv_{:026}", 88).parse().unwrap(),
            )
            .unwrap(),
            Some("feature".into()),
            None,
        )
        .await
        .unwrap();
        let p = f
            .start(
                json!({"runbook":"deploy","version":preview.runbook_version_id}),
                Some("preview"),
            )
            .await;
        assert_eq!(p.status, 201, "{}", p.body);
        assert_eq!(
            p.body["runbook_version_id"],
            preview.runbook_version_id.as_str()
        );
        assert!(p.body.get("runbook_version").unwrap().is_null());
        for version in [json!(99), json!(format!("rbv_{:026}", 99))] {
            let r = f
                .start(
                    json!({"runbook":"deploy","version":version}),
                    Some(&format!("bad-{version}")),
                )
                .await;
            assert_eq!(r.status, 404, "{}", r.body);
            assert_eq!(r.body["code"], "runbook.version_not_found");
        }
        let other = f.create("other").await;
        let foreign = f
            .publish(&other, spec("other", "Foreign", false), "foreign")
            .await;
        let r = f
            .start(
                json!({"runbook":"deploy","version":foreign["runbook_version_id"]}),
                Some("foreign-start"),
            )
            .await;
        assert_eq!(r.status, 404);
        assert_eq!(r.body["code"], "runbook.version_not_found");
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn start_replay_is_atomic_and_authorized() {
    with_fixture(async |f, db| {
        let rb = f.create("deploy").await;
        f.publish(&rb, spec("deploy", "One", false), "v1").await;
        let payload = json!({"runbook":"deploy"});
        let missing = f.start(payload.clone(), None).await;
        assert_eq!(missing.status, 428, "{}", missing.body);
        assert_eq!(missing.body["code"], "request.idempotency_key_required");
        let (a, b) = tokio::join!(
            f.start(payload.clone(), Some("same")),
            f.start(payload.clone(), Some("same"))
        );
        assert_eq!(a.status, 201, "{}", a.body);
        assert_eq!(b.status, 201);
        assert_eq!(a.body, b.body);
        let store = runbook_store_postgres::PostgresStore::connect(db.config())
            .await
            .unwrap();
        assert_eq!(
            store
                .client()
                .query_one("SELECT count(*) FROM runbook.execution_start", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        assert_eq!(
            store
                .client()
                .query_one("SELECT count(*) FROM runbook.admission", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        f.publish(&rb, spec("deploy", "New required input", true), "v2")
            .await;
        let replay = f.start(payload.clone(), Some("same")).await;
        assert_eq!(replay.status, 201);
        assert_eq!(replay.body, a.body);
        let conflict = f
            .start(
                json!({"runbook":"deploy","inputs":{"service":"other"}}),
                Some("same"),
            )
            .await;
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.body["code"], "request.idempotency_key_reused");
        store
            .client()
            .execute(
                "UPDATE runbook.start_replay SET created_at_ms=0 WHERE execution_id=$1",
                &[&a.body["id"].as_str().unwrap()],
            )
            .await
            .unwrap();
        let fresh = f
            .start(
                json!({"runbook":"deploy","inputs":{"extra":"ok"}}),
                Some("same"),
            )
            .await;
        assert_eq!(fresh.status, 201, "{}", fresh.body);
        assert_ne!(fresh.body["id"], a.body["id"]);
        let d = PostgresDirectory::new(db.config().clone());
        let s = d.snapshot(&f.org).await.unwrap();
        // D29 needs another active owner before deactivating the bootstrap owner.
        d.commit(DirectoryCommit {
            org_id: f.org.clone(),
            expected_revision: s.revision,
            mutations: vec![
                DirectoryMutation::CreateUser(UserRecord {
                    id: format!("usr_{:026}", 99).parse().unwrap(),
                    name: "Second owner".into(),
                    active: true,
                    role: OrgRole::Owner,
                }),
                DirectoryMutation::SetUserActive {
                    user_id: f.user.clone(),
                    active: false,
                },
            ],
        })
        .await
        .unwrap();
        let denied = f.start(payload, Some("same")).await;
        assert_eq!(denied.status, 403);
        assert_eq!(denied.body["code"], "auth.forbidden");
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn execution_routes_enforce_scope_and_roles() {
    with_fixture(async |f, db| {
        let rb = f.create("deploy").await;
        f.publish(&rb, spec("deploy", "One", false), "v1").await;
        let d = PostgresDirectory::new(db.config().clone());
        let c = PostgresCredentials::new(db.config().clone());
        let viewer: UserId = format!("usr_{:026}", 44).parse().unwrap();
        let s = d.snapshot(&f.org).await.unwrap();
        d.commit(DirectoryCommit {
            org_id: f.org.clone(),
            expected_revision: s.revision,
            mutations: vec![
                DirectoryMutation::CreateUser(UserRecord {
                    id: viewer.clone(),
                    name: "Viewer".into(),
                    active: true,
                    role: OrgRole::Member,
                }),
                DirectoryMutation::GrantProjectRole {
                    project_id: f.project.clone(),
                    user_id: viewer.clone(),
                    role: ProjectRole::Viewer,
                },
            ],
        })
        .await
        .unwrap();
        for n in [44u8, 45, 46] {
            let secret = encode_secret(
                if n == 46 {
                    TokenKind::Service
                } else {
                    TokenKind::Personal
                },
                [n; 32],
            );
            c.insert(CredentialRecord {
                id: format!("tok_{n:026}").parse().unwrap(),
                kind: if n == 46 {
                    TokenKind::Service
                } else {
                    TokenKind::Personal
                },
                hash: secret_hash(&secret),
                principal: if n == 46 {
                    CredentialPrincipal::Service {
                        project_id: f.project.clone(),
                        role: ProjectRole::Operator,
                    }
                } else {
                    CredentialPrincipal::User {
                        user_id: if n == 44 {
                            viewer.clone()
                        } else {
                            f.user.clone()
                        },
                        actor: runbook_core::credentials::ActorKind::Human,
                    }
                },
                org_id: f.org.clone(),
                scopes: if n == 45 {
                    [Scope::ExecutionsRead].into()
                } else {
                    [Scope::ExecutionsStart, Scope::ExecutionsRead].into()
                },
                projects: None,
                expires_at: Some(i64::MAX),
                revoked_at: None,
                last_used_at: None,
            })
            .await
            .unwrap();
            let r = call(
                &f.app,
                &secret,
                "POST",
                &format!("/v1/projects/{}/executions", f.project),
                json!({"runbook":"deploy"}),
                Some("role-start"),
                None,
            )
            .await;
            if n == 46 {
                assert_eq!(r.status, 201, "{}", r.body);
            } else {
                assert_eq!(r.status, 403, "{}", r.body);
                assert_eq!(
                    r.body["code"],
                    if n == 44 {
                        "auth.forbidden"
                    } else {
                        "auth.scope_missing"
                    }
                );
            }
        }
        let run = f
            .start(json!({"runbook":"deploy"}), Some("read-test"))
            .await;
        assert_eq!(run.status, 201);
        let read = call(
            &f.app,
            &encode_secret(TokenKind::Personal, [44; 32]),
            "GET",
            &format!("/v1/executions/{}", run.body["id"].as_str().unwrap()),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(read.status, 200);
        // An org-scoped key lookup cannot disclose a foreign org's run.
        let foreign: OrganizationId = format!("org_{:026}", 77).parse().unwrap();
        let foreign_project: ProjectId = format!("prj_{:026}", 77).parse().unwrap();
        d.commit(DirectoryCommit {
            org_id: foreign.clone(),
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: "Foreign".into(),
                },
                DirectoryMutation::CreateProject(ProjectRecord {
                    id: foreign_project.clone(),
                    org_id: foreign.clone(),
                    slug: "foreign".into(),
                    title: "Foreign".into(),
                }),
            ],
        })
        .await
        .unwrap();
        let secret = encode_secret(TokenKind::Service, [77; 32]);
        c.insert(CredentialRecord {
            id: format!("tok_{:026}", 77).parse().unwrap(),
            kind: TokenKind::Service,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::Service {
                project_id: foreign_project,
                role: ProjectRole::Admin,
            },
            org_id: foreign,
            scopes: [Scope::ExecutionsRead].into(),
            projects: None,
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
        for id in [
            run.body["id"].as_str().unwrap().to_string(),
            format!("exe_{:026}", 999),
        ] {
            let r = call(
                &f.app,
                &secret,
                "GET",
                &format!("/v1/executions/{id}"),
                Value::Null,
                None,
                None,
            )
            .await;
            assert_eq!(r.status, 404);
            assert_eq!(r.body["code"], "resource.not_found");
        }
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn queue_limit_fifo_and_restart() {
    with_fixture(async |f,db| {
        let rb=f.create("deploy").await;f.publish(&rb,spec("deploy","One",false),"v1").await;
        let mut runs=vec![];
        for key in ["first","second","third"] {
            let r=f.start(json!({"runbook":"deploy"}),Some(key)).await;assert_eq!(r.status,201,"{}",r.body);runs.push(r.body);
        }
        assert_eq!(runs[0]["admission"]["state"],"admitted");
        assert_eq!(runs[1]["admission"]["state"],"queued");assert_eq!(runs[2]["admission"]["state"],"queued");
        let other=f.start(json!({"runbook":"deploy","inputs":{"service":"web"}}),Some("web")).await;assert_eq!(other.status,201);assert_eq!(other.body["admission"]["state"],"admitted");
        let old=std::mem::replace(&mut f.worker,Worker::start(f.config.clone()).unwrap());
        f.app=router(f.worker.client());drop(old);
        for run in &runs {let read=f.get(run["id"].as_str().unwrap()).await;assert_eq!(read.status,200);assert_eq!(read.body["admission"],run["admission"]);assert_eq!(read.body["inputs"],run["inputs"]);}
        let store=runbook_store_postgres::PostgresStore::connect(db.config()).await.unwrap();
        let rows=store.client().query("SELECT execution_id FROM runbook.admission WHERE state='queued' ORDER BY queue_order",&[]).await.unwrap();
        assert_eq!(rows.iter().map(|r|r.get::<_,String>(0)).collect::<Vec<_>>(),vec![runs[1]["id"].as_str().unwrap(),runs[2]["id"].as_str().unwrap()]);
        use runbook_core::{ExecutionStore, catalog::{TenantScope,CatalogStore}, start::activate, work_queue::{WorkQueue,WorkKind}};
        let scoped=runbook_store_postgres::PostgresExecutionStore::new(db.config().clone(),TenantScope{org_id:f.org.clone(),project_id:f.project.clone()});
        // Queued runs also offer their `execution.queued` notification (C1).
        for lease in scoped.claim_due(10,60_000).await.unwrap().into_iter().filter(|lease|!matches!(lease.id.kind,WorkKind::Notification{..})) {
            assert_eq!(lease.id.kind,WorkKind::Activation);activate(&scoped,&PostgresDirectory::new(db.config().clone()),TenantScope{org_id:f.org.clone(),project_id:f.project.clone()},&lease).await.unwrap();
        }
        let first=f.get(runs[0]["id"].as_str().unwrap()).await;assert_eq!(first.status,200,"{}",first.body);assert_eq!(first.body["status"],"running");
        let at=first.body["admission"]["admitted_at_ms"].as_i64().unwrap();
        assert_eq!(first.body["execution_deadline_at_ms"],at+7_200_000);
        let replay=f.start(json!({"runbook":"deploy"}),Some("first")).await;assert_eq!(replay.status,201);assert_eq!(replay.body,runs[0]);
        let id:ExecutionId=runs[0]["id"].as_str().unwrap().parse().unwrap();
        let current=scoped.load(&id).await.unwrap();
        let mut projection=current.projection.clone();projection.sequence+=1;projection.status=ExecutionStatus::Succeeded;projection.completed_at_ms=Some(at+1);
        let mut summary=scoped.get_run(&id).await.unwrap().unwrap();summary.sequence=projection.sequence;summary.status=ExecutionStatus::Succeeded;summary.completed_at_ms=Some(at+1);
        // Storage fixture releases the slot; B6 owns driving real work to completion.
        scoped.commit(runbook_core::ExecutionCommit{execution_id:id.clone(),expected_sequence:current.revision,events:vec![runbook_engine::EventEnvelope{execution_id:id,seq:projection.sequence,at_ms:at+1,actor:ActorRef::System,event:runbook_engine::Event::ManualPauseChanged{paused:false}}],projection,timers:vec![],outbox:vec![],idempotency:vec![],claims:vec![],credentials:vec![],derived_outbox:vec![runbook_core::catalog::DerivedUpdate{summary,assignments:vec![],release_concurrency:true,memory:vec![]}]}).await.unwrap();
        let second=f.get(runs[1]["id"].as_str().unwrap()).await;assert_eq!(second.status,200);assert_eq!(second.body["admission"]["state"],"admitted");
        let third=f.get(runs[2]["id"].as_str().unwrap()).await;assert_eq!(third.status,200);assert_eq!(third.body["admission"]["state"],"queued");
        let leases:Vec<_>=scoped.claim_due(10,60_000).await.unwrap().into_iter().filter(|lease|lease.id.kind==WorkKind::Activation).collect();assert_eq!(leases.len(),1);assert_eq!(leases[0].id.execution_id.as_str(),runs[1]["id"].as_str().unwrap());
        activate(&scoped,&PostgresDirectory::new(db.config().clone()),TenantScope{org_id:f.org.clone(),project_id:f.project.clone()},&leases[0]).await.unwrap();
        let second=f.get(runs[1]["id"].as_str().unwrap()).await;assert_eq!(second.status,200,"{}",second.body);assert_eq!(second.body["status"],"running");
        assert_eq!(second.body["execution_deadline_at_ms"].as_i64().unwrap(),second.body["admission"]["admitted_at_ms"].as_i64().unwrap()+7_200_000);
    }).await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn unpooled_start_is_admitted_and_activatable_over_http() {
    with_fixture(async |f,db| {
        use runbook_core::{catalog::TenantScope, start::activate, work_queue::{WorkQueue,WorkKind}};
        let rb=f.create("plain").await;
        let source="apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: plain, title: Plain}\nspec:\n  execution: {timeout: 2h}\n  steps: [{id: work, title: Work}]\n";
        f.publish(&rb,source.into(),"plain").await;
        let run=f.start(json!({"runbook":"plain"}),Some("plain-start")).await;assert_eq!(run.status,201,"{}",run.body);assert_eq!(run.body["admission"]["state"],"admitted");assert!(run.body["admission"]["admitted_at_ms"].is_i64());
        let store=runbook_store_postgres::PostgresExecutionStore::new(db.config().clone(),TenantScope{org_id:f.org.clone(),project_id:f.project.clone()});
        let leases=store.claim_due(10,60_000).await.unwrap();assert_eq!(leases.len(),1);assert_eq!(leases[0].id.kind,WorkKind::Activation);
        activate(&store,&PostgresDirectory::new(db.config().clone()),TenantScope{org_id:f.org.clone(),project_id:f.project.clone()},&leases[0]).await.unwrap();
        let read=f.get(run.body["id"].as_str().unwrap()).await;assert_eq!(read.status,200,"{}",read.body);assert_eq!(read.body["status"],"running");assert_eq!(read.body["execution_deadline_at_ms"].as_i64().unwrap(),run.body["admission"]["admitted_at_ms"].as_i64().unwrap()+7_200_000);
        let replay=f.start(json!({"runbook":"plain"}),Some("plain-start")).await;assert_eq!(replay.status,201);assert_eq!(replay.body,run.body);
    }).await;
}
/// U10: invalid start data answers like an invalid rerun (PRD §6, §9):
/// `422 execution.inputs_invalid` with one `{path, code, message}` field
/// error per problem under `inputs.`/`fields.`, nothing created, and the
/// idempotency key left unspent so the corrected retry can reuse it.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn start_rejects_invalid_data_with_field_errors() {
    with_fixture(async |f, _| {
        let rb = f.create("picky").await;
        let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: picky, title: Picky}\nspec:\n  inputs:\n    service: {type: enum, options: [api, web], required: true}\n    count: {type: integer, default: 1}\n  fields:\n    note: {type: string, required: true}\n  steps: [{id: work, title: Work}]\n";
        f.publish(&rb, source.into(), "v1").await;
        let bad = f
            .start(
                json!({"runbook":"picky","inputs":{"service":"db","count":"two","extra":1},"fields":{}}),
                Some("one-action"),
            )
            .await;
        assert_eq!(bad.status, 422, "{}", bad.body);
        assert_eq!(bad.body["code"], "execution.inputs_invalid");
        let errors = bad.body["errors"].as_array().expect("field errors");
        let code_at = |path: &str| {
            errors
                .iter()
                .find(|e| e["path"] == path)
                .unwrap_or_else(|| panic!("no error at {path}: {}", bad.body))["code"]
                .clone()
        };
        assert_eq!(code_at("inputs.service"), "data.enum");
        assert_eq!(code_at("inputs.count"), "data.type_error");
        assert_eq!(code_at("inputs.extra"), "data.unknown_field");
        assert_eq!(code_at("fields.note"), "data.required");
        assert_eq!(errors.len(), 4, "{}", bad.body);
        for error in errors {
            let keys: Vec<&str> = error
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys, ["code", "message", "path"], "{error}");
            assert!(!error["message"].as_str().unwrap().is_empty());
        }
        let listed = call(
            &f.app,
            &f.token,
            "GET",
            &format!("/v1/projects/{}/executions", f.project),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(listed.status, 200, "{}", listed.body);
        assert_eq!(listed.body["items"], json!([]), "nothing was created");
        // The rejected attempt did not spend the key: the corrected retry of
        // the same user action starts the run.
        let good = f
            .start(
                json!({"runbook":"picky","inputs":{"service":"web"},"fields":{"note":"go"}}),
                Some("one-action"),
            )
            .await;
        assert_eq!(good.status, 201, "{}", good.body);
        assert_eq!(good.body["inputs"], json!({"service":"web","count":1}));
        assert_eq!(good.body["fields"], json!({"note":"go"}));
    })
    .await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn list_filters_reuse_scoped_indexes() {
    with_fixture(async |f,db| {
        let rb=f.create("searchable").await;
        let source=spec("searchable","Searchable",false).replace("  inputs:","  fields: {severity: {type: string, indexed: true}, score: {type: number, indexed: true}, unused: {type: string, indexed: true}, flag: {type: boolean, indexed: true}, private: {type: string}}\n  inputs:");
        f.publish(&rb,source,"v1").await;
        let mut ids=vec![];
        for (n,tags) in [(1,json!(["prod","blue"])),(2,json!(["prod"])),(3,json!(["prod","blue"]))] {
            let r=f.start(json!({"runbook":"searchable","tags":tags,"fields":{"severity":"major","score":n,"flag":true,"private":"not indexed"}}),Some(&format!("s{n}"))).await;
            assert_eq!(r.status,201,"{}",r.body);ids.push(r.body["id"].as_str().unwrap().to_owned());
        }
        let path=format!("/v1/projects/{}/executions",f.project);
        let (app,token)=(&f.app,&f.token);
        let get=|pairs:Vec<(&str,String)>|{let uri=format!("{path}?{}",serde_urlencoded::to_string(pairs).unwrap());async move {call(app,token,"GET",&uri,Value::Null,None,None).await}};
        let r=get(vec![("limit","1".into())]).await;assert_eq!(r.status,200,"{}",r.body);assert_eq!(r.body["items"][0]["id"],ids[2]);
        let cursor=r.body["next_cursor"].as_str().unwrap().to_owned();
        let next=get(vec![("limit","1".into()),("cursor",cursor.clone())]).await;assert_eq!(next.status,200);assert_eq!(next.body["items"][0]["id"],ids[1]);
        let mismatch=get(vec![("cursor",cursor.clone()),("tags","blue".into())]).await;assert_eq!(mismatch.status,400);assert_eq!(mismatch.body["code"],"request.invalid_cursor");
        let from=r.body["items"][0]["created_at_ms"].as_i64().unwrap();
        let filters=vec![("runbook","searchable".into()),("status","queued".into()),("tags","prod,blue".into()),("field.severity","\"major\"".into()),("field.score.min","2".into()),("field.score.max","4".into()),("from","0".into()),("to",(from+1).to_string()),("actor",format!("human:{}",f.user))];
        let found=get(filters.clone()).await;assert_eq!(found.status,200,"{}",found.body);assert_eq!(found.body["items"].as_array().unwrap().len(),1);assert_eq!(found.body["items"][0]["id"],ids[2]);assert!(found.body["items"][0]["fields"].get("private").is_none());
        let bad=get(vec![("runbook","searchable".into()),("field.private","\"x\"".into())]).await;assert_eq!(bad.status,400);assert_eq!(bad.body["code"],"query.field_not_indexed");
        let empty=get(vec![("runbook","searchable".into()),("field.unused","\"x\"".into())]).await;assert_eq!(empty.status,200,"{}",empty.body);assert_eq!(empty.body["items"],json!([]));
        let d=PostgresDirectory::new(db.config().clone());
        let other:ProjectId=format!("prj_{:026}",84).parse().unwrap();
        let snapshot=d.snapshot(&f.org).await.unwrap();
        d.commit(DirectoryCommit{org_id:f.org.clone(),expected_revision:snapshot.revision,mutations:vec![DirectoryMutation::CreateProject(ProjectRecord{id:other.clone(),org_id:f.org.clone(),slug:"other".into(),title:"Other".into()})]}).await.unwrap();
        let credentials=PostgresCredentials::new(db.config().clone());let other_secret=encode_secret(TokenKind::Service,[84;32]);
        credentials.insert(CredentialRecord{id:format!("tok_{:026}",84).parse().unwrap(),kind:TokenKind::Service,hash:secret_hash(&other_secret),principal:CredentialPrincipal::Service{project_id:other.clone(),role:ProjectRole::Viewer},org_id:f.org.clone(),scopes:[Scope::ExecutionsRead].into(),projects:None,expires_at:None,revoked_at:None,last_used_at:None}).await.unwrap();
        let other_path=format!("/v1/projects/{other}/executions");
        let empty=call(&f.app,&other_secret,"GET",&other_path,Value::Null,None,None).await;assert_eq!(empty.status,200);assert_eq!(empty.body["items"],json!([]));
        let uri=format!("{other_path}?{}",serde_urlencoded::to_string([("cursor",cursor)]).unwrap());
        let mismatched=call(&f.app,&other_secret,"GET",&uri,Value::Null,None,None).await;assert_eq!(mismatched.status,400);assert_eq!(mismatched.body["code"],"request.invalid_cursor");
        let foreign=call(&f.app,&other_secret,"GET",&path,Value::Null,None,None).await;assert_eq!(foreign.status,403);
        // R14: open numeric ranges never include other JSON scalar types.
        for field in ["field.severity.max","field.flag.min"] {
            let uri=format!("{path}?{}",serde_urlencoded::to_string([(field,"2")]).unwrap());
            let result=call(&f.app,&f.token,"GET",&uri,Value::Null,None,None).await;
            assert_eq!(result.status,200,"{}",result.body);assert_eq!(result.body["items"],json!([]));
        }
        let old=std::mem::replace(&mut f.worker,Worker::start(f.config.clone()).unwrap());f.app=router(f.worker.client());drop(old);
        let uri=format!("{path}?{}",serde_urlencoded::to_string(filters).unwrap());
        let restarted=call(&f.app,&f.token,"GET",&uri,Value::Null,None,None).await;assert_eq!(restarted.body,found.body);
        let credentials=PostgresCredentials::new(db.config().clone());let secret=encode_secret(TokenKind::Service,[83;32]);
        credentials.insert(CredentialRecord{id:format!("tok_{:026}",83).parse().unwrap(),kind:TokenKind::Service,hash:secret_hash(&secret),principal:CredentialPrincipal::Service{project_id:f.project.clone(),role:ProjectRole::Viewer},org_id:f.org.clone(),scopes:[Scope::RunbooksRead].into(),projects:None,expires_at:None,revoked_at:None,last_used_at:None}).await.unwrap();
        let denied=call(&f.app,&secret,"GET",&path,Value::Null,None,None).await;assert_eq!(denied.status,403);
    }).await;
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn stream_reconnect_has_no_sequence_gap() {
    with_fixture(async |f, db| {
        use futures_util::StreamExt;
        use runbook_core::{ExecutionStore, catalog::TenantScope};
        let rb = f.create("streamed").await;
        f.publish(&rb, spec("streamed", "Streamed", false), "v1")
            .await;
        let start = f.start(json!({"runbook":"streamed"}), Some("stream")).await;
        assert_eq!(start.status, 201);
        let id: ExecutionId = start.body["id"].as_str().unwrap().parse().unwrap();
        let store = runbook_store_postgres::PostgresExecutionStore::new(
            db.config().clone(),
            TenantScope {
                org_id: f.org.clone(),
                project_id: f.project.clone(),
            },
        );
        async fn append(
            store: &runbook_store_postgres::PostgresExecutionStore,
            id: &ExecutionId,
            terminal: bool,
        ) {
            let run = store.load(id).await.unwrap();
            let mut projection = run.projection;
            projection.sequence += 1;
            if terminal {
                projection.status = ExecutionStatus::Succeeded;
                projection.completed_at_ms = Some(1);
            }
            store
                .commit(runbook_core::ExecutionCommit {
                    execution_id: id.clone(),
                    expected_sequence: run.revision,
                    events: vec![runbook_engine::EventEnvelope {
                        execution_id: id.clone(),
                        seq: projection.sequence,
                        at_ms: 1,
                        actor: ActorRef::System,
                        event: if terminal {
                            runbook_engine::Event::ExecutionCompleted {
                                status: ExecutionStatus::Succeeded,
                                outputs: Default::default(),
                            }
                        } else {
                            runbook_engine::Event::ManualPauseChanged { paused: false }
                        },
                    }],
                    projection,
                    timers: vec![],
                    outbox: vec![],
                    idempotency: vec![],
                    claims: vec![],
                    credentials: vec![],
                    derived_outbox: vec![],
                })
                .await
                .unwrap();
        }
        let path = format!("/v1/executions/{id}/events");
        let open = |after: &str| {
            f.app.clone().oneshot(
                Request::builder()
                    .uri(&path)
                    .header("authorization", format!("Bearer {}", f.token))
                    .header("last-event-id", after)
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        append(&store, &id, false).await;
        let (response, _) = tokio::join!(open("0"), append(&store, &id, false));
        let response = response.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut frames = response.into_body().into_data_stream();
        for seq in [1, 2] {
            let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let text = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(text.starts_with(&format!("id: {seq}\n")), "{text}");
        }
        drop(frames);
        let response = open("2").await.unwrap();
        assert_eq!(response.status(), 200);
        let mut frames = response.into_body().into_data_stream();
        let raw = runbook_store_postgres::PostgresStore::connect(db.config())
            .await
            .unwrap();
        raw.client()
            .execute(
                "SELECT pg_notify('runbook_execution',$1)",
                &[&format!("{id}:2")],
            )
            .await
            .unwrap();
        // A rolled-back event never becomes visible and its NOTIFY is discarded.
        raw.client().batch_execute("BEGIN").await.unwrap();
        raw.client()
            .execute(
                "SELECT pg_notify('runbook_execution',$1)",
                &[&format!("{id}:99")],
            )
            .await
            .unwrap();
        raw.client().batch_execute("ROLLBACK").await.unwrap();
        append(&store, &id, false).await;
        let bytes = tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .starts_with("id: 3\n")
        );
        drop(frames);
        assert_eq!(open("999").await.unwrap().status(), 400);
        for invalid in ["-1", "1.5", "18446744073709551616", "abc"] {
            assert_eq!(open(invalid).await.unwrap().status(), 400);
        }
        for foreign_org in [false, true] {
            let secret = isolated_reader(f, foreign_org).await;
            let denied = call(&f.app, &secret, "GET", &path, Value::Null, None, None).await;
            assert_eq!(denied.status, if foreign_org { 404 } else { 403 });
        }

        let replay = open("2").await.unwrap();
        let mut frames = replay.into_body().into_data_stream();
        let bytes = frames.next().await.unwrap().unwrap();
        assert!(
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .starts_with("id: 3\n")
        );
        // Revocation is checked while idle, not only on subsequent appends.
        raw.client()
            .execute(
                "UPDATE runbook.credential SET revoked_at=1 WHERE org_id=$1",
                &[&f.org.as_str()],
            )
            .await
            .unwrap();
        let closed = tokio::time::timeout(std::time::Duration::from_secs(40), frames.next())
            .await
            .unwrap();
        assert!(closed.is_none());
        // Every newly emitted event rechecks access, without waiting for the idle timer.
        raw.client()
            .execute(
                "UPDATE runbook.credential SET revoked_at=NULL WHERE org_id=$1",
                &[&f.org.as_str()],
            )
            .await
            .unwrap();
        let response = open("3").await.unwrap();
        let mut frames = response.into_body().into_data_stream();
        raw.client()
            .execute(
                "UPDATE runbook.credential SET revoked_at=1 WHERE org_id=$1",
                &[&f.org.as_str()],
            )
            .await
            .unwrap();
        append(&store, &id, false).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
                .await
                .unwrap()
                .is_none()
        );
        raw.client()
            .execute(
                "UPDATE runbook.credential SET revoked_at=NULL WHERE org_id=$1",
                &[&f.org.as_str()],
            )
            .await
            .unwrap();
        append(&store, &id, true).await;
        let response = open("4").await.unwrap();
        let mut frames = response.into_body().into_data_stream();
        let bytes = frames.next().await.unwrap().unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            text.starts_with("id: 5\nevent: ExecutionCompleted\n"),
            "{text}"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), frames.next())
                .await
                .unwrap()
                .is_none()
        );
        let response = open("5").await.unwrap();
        let mut frames = response.into_body().into_data_stream();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), frames.next())
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
}

async fn isolated_reader(f: &Fixture, foreign_org: bool) -> String {
    let n = if foreign_org { 86 } else { 85 };
    let org = if foreign_org {
        format!("org_{n:026}").parse().unwrap()
    } else {
        f.org.clone()
    };
    let project: ProjectId = format!("prj_{n:026}").parse().unwrap();
    let directory = PostgresDirectory::new(f.config.database.clone());
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
        slug: "isolated".into(),
        title: "Isolated".into(),
    }));
    directory
        .commit(DirectoryCommit {
            org_id: org.clone(),
            expected_revision: revision,
            mutations,
        })
        .await
        .unwrap();
    let credentials = PostgresCredentials::new(f.config.database.clone());
    let secret = encode_secret(TokenKind::Service, [n as u8; 32]);
    credentials
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
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn expired_stream_authorizes_before_disclosing_retention() {
    with_fixture(async |f, db| {
        use runbook_core::{
            ExecutionStore,
            catalog::{CatalogStore, DerivedUpdate, TenantScope},
            retention::RunStore,
        };
        let rb = f.create("expired").await;
        f.publish(&rb, spec("expired", "Expired", false), "v1")
            .await;
        let start = f.start(json!({"runbook":"expired"}), Some("expired")).await;
        assert_eq!(start.status, 201);
        let id: ExecutionId = start.body["id"].as_str().unwrap().parse().unwrap();
        let store = runbook_store_postgres::PostgresExecutionStore::new(
            db.config().clone(),
            TenantScope {
                org_id: f.org.clone(),
                project_id: f.project.clone(),
            },
        );
        let run = store.load(&id).await.unwrap();
        let mut projection = run.projection;
        projection.sequence += 1;
        projection.status = ExecutionStatus::Succeeded;
        projection.completed_at_ms = Some(1);
        let mut summary = store.get_run(&id).await.unwrap().unwrap();
        summary.sequence = projection.sequence;
        summary.status = ExecutionStatus::Succeeded;
        summary.completed_at_ms = Some(1);
        store
            .commit(runbook_core::ExecutionCommit {
                execution_id: id.clone(),
                expected_sequence: run.revision,
                events: vec![runbook_engine::EventEnvelope {
                    execution_id: id.clone(),
                    seq: projection.sequence,
                    at_ms: 1,
                    actor: ActorRef::System,
                    event: runbook_engine::Event::ExecutionCompleted {
                        status: ExecutionStatus::Succeeded,
                        outputs: Default::default(),
                    },
                }],
                projection,
                timers: vec![],
                outbox: vec![],
                idempotency: vec![],
                claims: vec![],
                credentials: vec![],
                derived_outbox: vec![DerivedUpdate {
                    summary,
                    assignments: vec![],
                    release_concurrency: true,
                    memory: vec![],
                }],
            })
            .await
            .unwrap();
        store.delete_execution(&id, "retention").await.unwrap();
        let path = format!("/v1/executions/{id}/events");
        let result = call(&f.app, &f.token, "GET", &path, Value::Null, None, None).await;
        assert_eq!(result.status, 410, "{}", result.body);
        assert_eq!(result.body["code"], "execution.history_expired");
        // D33: the document and a rerun of an expired run are 410 too, never
        // a 404 that hides why the original's inputs are gone.
        let report = call(
            &f.app,
            &f.token,
            "GET",
            &format!("/v1/executions/{id}/report"),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(report.status, 410, "{}", report.body);
        assert_eq!(report.body["code"], "execution.history_expired");
        let rerun = call(
            &f.app,
            &f.token,
            "POST",
            &format!("/v1/executions/{id}/rerun"),
            json!({}),
            Some("expired-rerun"),
            None,
        )
        .await;
        assert_eq!(rerun.status, 410, "{}", rerun.body);
        assert_eq!(rerun.body["code"], "execution.history_expired");
        for foreign_org in [false, true] {
            let secret = isolated_reader(f, foreign_org).await;
            let result = call(&f.app, &secret, "GET", &path, Value::Null, None, None).await;
            assert_eq!(result.status, 404);
            assert_eq!(result.body["code"], "resource.not_found");
        }
        // D16 membership revocation also conceals the retained tombstone.
        let d = PostgresDirectory::new(db.config().clone());
        let snapshot = d.snapshot(&f.org).await.unwrap();
        let replacement: UserId = format!("usr_{:026}", 88).parse().unwrap();
        d.commit(DirectoryCommit {
            org_id: f.org.clone(),
            expected_revision: snapshot.revision,
            mutations: vec![
                DirectoryMutation::CreateUser(UserRecord {
                    id: replacement,
                    name: "Replacement".into(),
                    active: true,
                    role: OrgRole::Owner,
                }),
                DirectoryMutation::SetUserActive {
                    user_id: f.user.clone(),
                    active: false,
                },
            ],
        })
        .await
        .unwrap();
        let result = call(&f.app, &f.token, "GET", &path, Value::Null, None, None).await;
        assert_eq!(result.status, 404);
    })
    .await;
}

#[path = "support/actor_routes.rs"]
mod actor_routes;

#[path = "support/rerun_routes.rs"]
mod rerun_routes;

#[path = "support/token_routes.rs"]
mod token_routes;

#[path = "support/review_routes.rs"]
mod review_routes;

/// LANG4 over HTTP: a published `call` pins the child's version (the
/// authoring routes resolve it from the catalog), the drain starts the
/// child, the views link both ways, and the child's outcome completes the
/// parent's step.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn a_call_step_runs_its_child_over_http() {
    with_fixture(async |f, _db| {
        let child_source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: child, title: Child}\nspec:\n  inputs: {target: {type: string, required: true}}\n  vars: {rows: 0}\n  steps:\n    - {id: gate, wait: {signal: go}}\n    - {id: count, set: {vars: {rows: 7}}}\n  outputs:\n    rows: \"${{ vars.rows }}\"\n";
        let parent_source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: parent, title: Parent}\nspec:\n  steps:\n    - id: db\n      call: {runbook: child, with: {target: prod}, outputs: {rows: {type: integer}}}\n";
        let (app, token, project) = (f.app.clone(), f.token.clone(), f.project.to_string());
        let validate = |source: &str| {
            let body = json!({"source": source, "project": project});
            let (app, token) = (app.clone(), token.clone());
            async move { call(&app, &token, "POST", "/v1/validate", body, None, None).await }
        };
        let unresolved = validate(parent_source).await;
        assert_eq!(unresolved.status, 200, "{}", unresolved.body);
        assert_eq!(unresolved.body["diagnostics"][0]["code"], "spec.call_unresolved");
        let child = f.create("child").await;
        f.publish(&child, child_source.into(), "child-v1").await;
        assert_eq!(validate(parent_source).await.body["diagnostics"], json!([]));
        let parent = f.create("parent").await;
        f.publish(&parent, parent_source.into(), "parent-v1").await;

        let started = f.start(json!({"runbook": "parent"}), Some("p1")).await;
        assert_eq!(started.status, 201, "{}", started.body);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        let step = |view: &Value, path: &str| {
            view["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["path"] == path)
                .cloned()
                .unwrap_or(Value::Null)
        };
        let mut view = Value::Null;
        for _ in 0..20 {
            drainer.drain_once().await.unwrap();
            view = f.get(&exe).await.body;
            if step(&view, "db")["call"]["started"] == true {
                break;
            }
        }
        let db = step(&view, "db");
        assert_eq!(db["kind"], "call", "{view}");
        assert_eq!(db["status"], "waiting");
        let child_exe = db["call"]["child_execution_id"].as_str().unwrap().to_owned();
        let child_view = f.get(&child_exe).await;
        assert_eq!(child_view.status, 200, "{}", child_view.body);
        assert_eq!(child_view.body["parent"]["execution_id"], exe.as_str());
        assert_eq!(child_view.body["parent"]["step"], "db");
        assert_eq!(child_view.body["parent"]["depth"], 1);
        assert_eq!(child_view.body["inputs"]["target"], "prod");
        // The child is activated by its own drain work item.
        for _ in 0..20 {
            if f.get(&child_exe).await.body["status"] == "waiting" {
                break;
            }
            drainer.drain_once().await.unwrap();
        }
        assert_eq!(f.get(&child_exe).await.body["status"], "waiting");

        let signal = call(
            &f.app,
            &f.token,
            "POST",
            &format!("/v1/executions/{child_exe}/signals/go"),
            json!({"payload": {}}),
            Some("go-1"),
            None,
        )
        .await;
        assert!(signal.status < 300, "{}", signal.body);
        for _ in 0..20 {
            drainer.drain_once().await.unwrap();
            view = f.get(&exe).await.body;
            if view["status"] == "succeeded" {
                break;
            }
        }
        assert_eq!(view["status"], "succeeded", "{view}");
        assert_eq!(step(&view, "db")["outputs"]["rows"], 7);
        let report = call(
            &f.app,
            &f.token,
            "GET",
            &format!("/v1/executions/{child_exe}/report"),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(report.status, 200, "{}", report.body);
        assert_eq!(report.body["header"]["parent"]["execution_id"], exe.as_str());
        let parent_report = call(
            &f.app,
            &f.token,
            "GET",
            &format!("/v1/executions/{exe}/report"),
            Value::Null,
            None,
            None,
        )
        .await;
        let section = parent_report.body["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "db")
            .cloned()
            .unwrap();
        assert_eq!(section["call"]["child_execution_id"], child_exe.as_str());
    })
    .await;
}
/// UI-1: the runbook list carries what the web catalog shows (draft
/// description and tags, whether the draft is ahead of the latest
/// published version, the newest run and the number of open runs), and
/// searches server-side: `q` over title, slug, description and tags
/// (case-insensitive substring), `tag` (all of them, exact). Both take
/// part in the cursor binding.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn runbook_list_summarizes_and_searches() {
    with_fixture(async |f, db| {
        let source = |slug: &str, title: &str, meta: &str| {
            format!(
                "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {slug}, title: {title}{meta}}}\nspec:\n  steps: [{{id: work, title: Work}}]\n"
            )
        };
        let create = async |slug: &str, src: String| {
            let r = call(
                &f.app,
                &f.token,
                "POST",
                &format!("/v1/projects/{}/runbooks", f.project),
                json!({ "source": src }),
                Some(slug),
                None,
            )
            .await;
            assert_eq!(r.status, 201, "{}", r.body);
            r.body["runbook_id"].as_str().unwrap().to_owned()
        };
        let deploy_src = source(
            "deploy",
            "Deploy API",
            ", description: Ships the API to production, tags: [prod, api]",
        );
        let deploy = create("deploy", deploy_src.clone()).await;
        f.publish(&deploy, deploy_src.clone(), "deploy-v1").await;
        let rollback_src = source("rollback", "Roll back", ", tags: [prod]");
        let rollback = create("rollback", rollback_src.clone()).await;
        f.publish(&rollback, rollback_src, "rollback-v1").await;
        let notes = create("notes", source("notes", "Notes", "")).await;
        // The deploy draft moves past version 1.
        let draft = format!("/v1/runbooks/{deploy}/draft");
        let read = call(&f.app, &f.token, "GET", &draft, Value::Null, None, None).await;
        let edited = call(
            &f.app,
            &f.token,
            "PATCH",
            &draft,
            json!({ "source": deploy_src.clone() + "# next\n" }),
            None,
            read.etag.as_deref(),
        )
        .await;
        assert_eq!(edited.status, 200, "{}", edited.body);
        // Two deploy runs: the older stays queued, the newer is cancelled.
        let older = f.start(json!({"runbook":"deploy"}), Some("older")).await;
        assert_eq!(older.status, 201, "{}", older.body);
        let newer = f.start(json!({"runbook":"deploy"}), Some("newer")).await;
        assert_eq!(newer.status, 201, "{}", newer.body);
        let newer_id = newer.body["id"].as_str().unwrap().to_owned();
        let cancelled = call(
            &f.app,
            &f.token,
            "PATCH",
            &format!("/v1/executions/{newer_id}"),
            json!({"action":"cancel"}),
            None,
            None,
        )
        .await;
        assert_eq!(cancelled.status, 200, "{}", cancelled.body);

        let path = format!("/v1/projects/{}/runbooks", f.project);
        let (app, token) = (&f.app, &f.token);
        let get = |pairs: Vec<(&str, &str)>| {
            let uri = if pairs.is_empty() {
                path.clone()
            } else {
                format!("{path}?{}", serde_urlencoded::to_string(pairs).unwrap())
            };
            async move { call(app, token, "GET", &uri, Value::Null, None, None).await }
        };
        let slugs = |r: &Reply| -> Vec<String> {
            r.body["items"]
                .as_array()
                .unwrap_or_else(|| panic!("{}", r.body))
                .iter()
                .map(|i| i["slug"].as_str().unwrap().to_owned())
                .collect()
        };
        let all = get(vec![]).await;
        assert_eq!(all.status, 200, "{}", all.body);
        // Newest runbook first (runbook ID, descending), as before.
        assert_eq!(slugs(&all), ["notes", "rollback", "deploy"]);
        let row = |slug: &str| {
            all.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["slug"] == slug)
                .unwrap()
                .clone()
        };
        let d = row("deploy");
        assert_eq!(d["runbook_id"], deploy);
        assert_eq!(d["title"], "Deploy API");
        assert_eq!(d["description"], "Ships the API to production");
        assert_eq!(d["tags"], json!(["prod", "api"]));
        assert_eq!(d["latest_version"], 1);
        assert_eq!(d["draft_ahead"], true);
        assert_eq!(d["last_run"]["id"], newer_id.as_str());
        assert_eq!(d["last_run"]["status"], "cancelled");
        assert_eq!(d["last_run"]["created_at_ms"], newer.body["created_at_ms"]);
        assert!(d["last_run"]["completed_at_ms"].is_i64(), "{d}");
        assert_eq!(d["active_runs"], 1);
        let mut keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "active_runs",
                "archived",
                "description",
                "draft_ahead",
                "last_run",
                "latest_version",
                "origin",
                "runbook_id",
                "slug",
                "tags",
                "title"
            ]
        );
        let r = row("rollback");
        assert_eq!(r["runbook_id"], rollback);
        assert_eq!(r["tags"], json!(["prod"]));
        assert_eq!(r["draft_ahead"], false);
        assert_eq!(r["active_runs"], 0);
        assert!(r.get("description").is_none() && r.get("last_run").is_none(), "{r}");
        let n = row("notes");
        assert_eq!(n["runbook_id"], notes);
        assert_eq!(n["tags"], json!([]));
        assert_eq!(n["draft_ahead"], false);
        assert!(n.get("latest_version").is_none(), "{n}");
        // One runbook's read is the same view.
        let one = call(app, token, "GET", &format!("/v1/runbooks/{deploy}"), Value::Null, None, None).await;
        assert_eq!(one.status, 200, "{}", one.body);
        assert_eq!(one.body, d);
        // UI-1c: and by its slug in the project; 404 for a slug the
        // project has no runbook for, or one no runbook can have.
        let by_slug = call(app, token, "GET", &format!("{path}/deploy"), Value::Null, None, None).await;
        assert_eq!(by_slug.status, 200, "{}", by_slug.body);
        assert_eq!(by_slug.body, d);
        for missing in ["deplo", "DEPLOY", "a%00b", "deploy@1"] {
            let read = call(app, token, "GET", &format!("{path}/{missing}"), Value::Null, None, None).await;
            assert_eq!(read.status, 404, "{missing}: {}", read.body);
            assert_eq!(read.body["code"], "resource.not_found", "{missing}");
        }

        // `q`: case-insensitive substring over title, slug, description, tags.
        for (q, expected) in [
            ("PROD", vec!["rollback", "deploy"]),
            ("ships", vec!["deploy"]),
            ("roll b", vec!["rollback"]),
            ("note", vec!["notes"]),
            ("api", vec!["deploy"]),
            ("nothing", vec![]),
            ("", vec!["notes", "rollback", "deploy"]),
        ] {
            let found = get(vec![("q", q)]).await;
            assert_eq!(found.status, 200, "{q}: {}", found.body);
            assert_eq!(slugs(&found), expected, "q={q}");
        }
        // `tag`: every listed tag, exact.
        for (tag, expected) in [
            ("prod", vec!["rollback", "deploy"]),
            ("prod,api", vec!["deploy"]),
            (" api , prod ", vec!["deploy"]),
            ("PROD", vec![]),
            ("pro", vec![]),
            ("", vec!["notes", "rollback", "deploy"]),
        ] {
            let found = get(vec![("tag", tag)]).await;
            assert_eq!(found.status, 200, "{tag}: {}", found.body);
            assert_eq!(slugs(&found), expected, "tag={tag}");
        }
        let both = get(vec![("q", "roll"), ("tag", "prod")]).await;
        assert_eq!(slugs(&both), ["rollback"]);
        // Paging over a search; its cursor is bound to that search.
        let first = get(vec![("q", "prod"), ("limit", "1")]).await;
        assert_eq!(slugs(&first), ["rollback"]);
        let cursor = first.body["next_cursor"].as_str().unwrap().to_owned();
        let second = get(vec![("q", "prod"), ("limit", "1"), ("cursor", &cursor)]).await;
        assert_eq!(second.status, 200, "{}", second.body);
        assert_eq!(slugs(&second), ["deploy"]);
        assert!(second.body["next_cursor"].is_null());
        assert_eq!(second.body["items"][0], d);
        for other in [
            vec![("q", "api"), ("limit", "1"), ("cursor", cursor.as_str())],
            vec![("limit", "1"), ("cursor", cursor.as_str())],
            vec![("q", "prod"), ("tag", "prod"), ("limit", "1"), ("cursor", cursor.as_str())],
        ] {
            let refused = get(other.clone()).await;
            assert_eq!(refused.status, 400, "{other:?}: {}", refused.body);
            assert_eq!(refused.body["code"], "request.invalid_cursor");
        }
        // An empty `q` or `tag` is the same search as none, cursor included.
        let plain = get(vec![("limit", "1")]).await;
        let cursor = plain.body["next_cursor"].as_str().unwrap().to_owned();
        let empty = get(vec![("q", ""), ("tag", " "), ("limit", "1"), ("cursor", &cursor)]).await;
        assert_eq!(empty.status, 200, "{}", empty.body);
        assert_eq!(slugs(&empty), ["rollback"]);
        // UI-1b review round 1: a cursor forged with this search's binding
        // but a position that is not a runbook ID (a NUL, which Postgres
        // refuses in text) is refused, not a 500.
        {
            use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
            let mut forged: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&cursor).unwrap()).unwrap();
            for after in ["rb_\u{0}0000000000000000000000000", "not-an-id", ""] {
                forged["after"] = json!(after);
                let forged = URL_SAFE_NO_PAD.encode(forged.to_string());
                let refused = get(vec![("limit", "1"), ("cursor", &forged)]).await;
                assert_eq!(refused.status, 400, "{after:?}: {}", refused.body);
                assert_eq!(refused.body["code"], "request.invalid_cursor");
            }
        }

        // UI-1 review: a credential with runbooks:read but not
        // executions:read gets the same rows without the run fields.
        let secret = encode_secret(TokenKind::Service, [85; 32]);
        PostgresCredentials::new(db.config().clone())
            .insert(CredentialRecord {
                id: format!("tok_{:026}", 85).parse().unwrap(),
                kind: TokenKind::Service,
                hash: secret_hash(&secret),
                principal: CredentialPrincipal::Service {
                    project_id: f.project.clone(),
                    role: ProjectRole::Viewer,
                },
                org_id: f.org.clone(),
                scopes: [Scope::RunbooksRead].into(),
                projects: None,
                expires_at: None,
                revoked_at: None,
                last_used_at: None,
            })
            .await
            .unwrap();
        let strip = |mut v: Value| {
            let row = v.as_object_mut().unwrap();
            row.remove("last_run");
            row.remove("active_runs");
            v
        };
        let narrow = call(app, &secret, "GET", &path, Value::Null, None, None).await;
        assert_eq!(narrow.status, 200, "{}", narrow.body);
        let rows = narrow.body["items"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        for (row, full) in rows.iter().zip(all.body["items"].as_array().unwrap()) {
            assert!(row.get("last_run").is_none() && row.get("active_runs").is_none(), "{row}");
            assert_eq!(row, &strip(full.clone()));
        }
        let one = call(app, &secret, "GET", &format!("/v1/runbooks/{deploy}"), Value::Null, None, None).await;
        assert_eq!(one.status, 200, "{}", one.body);
        assert_eq!(one.body, strip(d.clone()));
        let by_slug = call(app, &secret, "GET", &format!("{path}/deploy"), Value::Null, None, None).await;
        assert_eq!(by_slug.status, 200, "{}", by_slug.body);
        assert_eq!(by_slug.body, strip(d.clone()));
    })
    .await;
}

/// CF13 review round 1, item 1, natively: a run whose outputs together
/// pass the engine's budget (40 outputs, each a copy of a 60,000-byte step
/// output) fails with `execution.outputs_too_large` instead of succeeding,
/// as on Workers, and the same run can be cancelled before it completes.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn a_run_past_the_outputs_budget_fails_and_can_be_cancelled_natively() {
    with_fixture(async |f, _db| {
        let source = |slug: &str, then: &str| {
            let outputs: Vec<String> = (1..=40)
                .map(|n| format!("o{n:02}: '${{{{ steps.work.outputs.blob }}}}'"))
                .collect();
            format!(
                "apiVersion: therunbook.app/v1\nkind: Runbook\n\
                 metadata: {{id: {slug}, title: Wide outputs}}\n\
                 spec:\n  steps:\n    - id: work\n      title: Work\n      assignee: \"user:{}\"\n      \
                 outputs: {{blob: {{type: string}}}}\n{then}  outputs: {{{}}}\n",
                f.user,
                outputs.join(", ")
            )
        };
        let next = format!("    - {{id: next, title: Next, assignee: \"user:{}\"}}\n", f.user);
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        for (slug, then) in [("wideout", String::new()), ("cancelout", next)] {
            let rb = f.create(slug).await;
            f.publish(&rb, source(slug, &then), &format!("{slug}-v1")).await;
            let started = f.start(json!({"runbook": slug}), Some(slug)).await;
            assert_eq!(started.status, 201, "{}", started.body);
            let exe = started.body["id"].as_str().unwrap().to_owned();
            let mut view = f.get(&exe).await.body;
            for _ in 0..20 {
                if view["steps"][0]["status"] == "ready" {
                    break;
                }
                drainer.drain_once().await.unwrap();
                view = f.get(&exe).await.body;
            }
            let sr = view["steps"][0]["id"].as_str().unwrap().to_owned();
            let claim = call(
                &f.app,
                &f.token,
                "POST",
                &format!("/v1/step-runs/{sr}/claim"),
                json!({"session_id": "web"}),
                Some(&format!("claim-{slug}")),
                None,
            )
            .await;
            assert_eq!(claim.status, 200, "{}", claim.body);
            let reported = call(
                &f.app,
                &f.token,
                "POST",
                &format!("/v1/step-runs/{sr}/report"),
                json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded",
                       "verdict": null, "outputs": {"blob": "b".repeat(60_000)},
                       "summary": "done", "evidence": [], "logs": []}),
                Some(&format!("report-{slug}")),
                None,
            )
            .await;
            assert_eq!(reported.status, 200, "{}", reported.body);
            if slug == "cancelout" {
                let cancelled = call(
                    &f.app,
                    &f.token,
                    "PATCH",
                    &format!("/v1/executions/{exe}"),
                    json!({"action": "cancel"}),
                    Some("cancel-wide"),
                    None,
                )
                .await;
                assert_eq!(cancelled.status, 200, "{}", cancelled.body);
            }
            let expected = if slug == "cancelout" { "cancelled" } else { "failed" };
            let mut status = f.get(&exe).await.body["status"].clone();
            for _ in 0..20 {
                if status == expected {
                    break;
                }
                drainer.drain_once().await.unwrap();
                status = f.get(&exe).await.body["status"].clone();
            }
            assert_eq!(status, expected, "{slug}");
        }
    })
    .await;
}

/// CF13 review round 1, item 2, natively: a run's event log is held to its
/// budget (`RUNBOOK_LIMIT_RUN_BYTES`, here 400 KB) as on Workers: a report
/// whose data would pass it is refused with `422 execution.too_large`
/// (twice: it is not retryable) and writes nothing, while a short report
/// and a cancel still pass.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn a_run_at_its_budget_refuses_large_reports_natively() {
    let _slot = FixtureSlot::acquire();
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let f = Fixture::with_run_budget(&db, Some(400_000)).await;
        let steps: Vec<String> = (1..=3)
            .map(|n| {
                format!(
                    "{{id: s{n}, title: Step {n}, assignee: \"user:{}\"}}",
                    f.user
                )
            })
            .collect();
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {{id: budget, title: Budget}}\nspec: {{steps: [{}]}}\n",
            steps.join(", ")
        );
        let rb = f.create("budget").await;
        f.publish(&rb, source, "budget-v1").await;
        let started = f.start(json!({"runbook": "budget"}), Some("budget")).await;
        assert_eq!(started.status, 201, "{}", started.body);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        let mut report = async |path: &str, logs: Value, key: &str| {
            let mut view = f.get(&exe).await.body;
            for _ in 0..20 {
                let ready = view["steps"].as_array().unwrap().iter().any(|s| {
                    s["path"] == path && (s["status"] == "ready" || s["status"] == "claimed")
                });
                if ready {
                    break;
                }
                drainer.drain_once().await.unwrap();
                view = f.get(&exe).await.body;
            }
            let step = view["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["path"] == path)
                .cloned()
                .unwrap();
            let sr = step["id"].as_str().unwrap().to_owned();
            let claim = if step["status"] == "claimed" {
                step["claim"].clone()
            } else {
                let claim = call(
                    &f.app,
                    &f.token,
                    "POST",
                    &format!("/v1/step-runs/{sr}/claim"),
                    json!({"session_id": "web"}),
                    Some(&format!("claim-{key}")),
                    None,
                )
                .await;
                assert_eq!(claim.status, 200, "{}", claim.body);
                claim.body
            };
            call(
                &f.app,
                &f.token,
                "POST",
                &format!("/v1/step-runs/{sr}/report"),
                json!({"step_run_id": sr, "claim": claim, "status": "succeeded",
                       "verdict": null, "outputs": {}, "summary": "done",
                       "evidence": [], "logs": logs}),
                Some(&format!("report-{key}")),
                None,
            )
            .await
        };
        let logs = |n: usize| json!(vec!["l".repeat(60_000); n]);
        let first = report("s1", logs(5), "s1").await;
        assert_eq!(first.status, 200, "{}", first.body);
        let refused = report("s2", logs(3), "s2").await;
        assert_eq!(refused.status, 422, "{}", refused.body);
        assert_eq!(refused.body["code"], "execution.too_large");
        let again = report("s2", logs(3), "s2").await;
        assert_eq!(again.status, 422, "{}", again.body);
        let small = report("s2", json!(["ok"]), "s2-small").await;
        assert_eq!(small.status, 200, "{}", small.body);
        let cancelled = call(
            &f.app,
            &f.token,
            "PATCH",
            &format!("/v1/executions/{exe}"),
            json!({"action": "cancel"}),
            Some("cancel-budget"),
            None,
        )
        .await;
        assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// CF13b, item 1, natively: engine-written events were outside the run
/// budget, so a definition alone could grow a run without bound (here 15
/// `set` steps, each `DataChanged` carrying a 60,000-byte value, about
/// 900 KB, past a 400 KB budget). The engine (version 6) holds the log to a
/// ceiling of one and a half budgets (600 KB): the report that sets them
/// off answers 200, and in the same commit the run is faulted with
/// `limit.run_size_exceeded` and closes failed, its later `set` steps never
/// run. Before, it succeeded with every step.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn a_run_whose_engine_events_pass_the_ceiling_closes_itself_natively() {
    let _slot = FixtureSlot::acquire();
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let f = Fixture::with_run_budget(&db, Some(400_000)).await;
        let sets: String = (1..=15)
            .map(|n| {
                format!("    - {{id: s{n}, set: {{vars: {{a: \"${{{{ inputs.blob }}}}\"}}}}}}\n")
            })
            .collect();
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {{id: sets, title: Sets}}\nspec:\n  \
             inputs: {{blob: {{type: string, required: true}}}}\n  vars: {{a: ''}}\n  steps:\n    \
             - {{id: go, title: Go, assignee: \"user:{}\"}}\n{sets}",
            f.user
        );
        let rb = f.create("sets").await;
        f.publish(&rb, source, "sets-v1").await;
        let started = f
            .start(
                json!({"runbook": "sets", "inputs": {"blob": "b".repeat(60_000)}}),
                Some("sets"),
            )
            .await;
        assert_eq!(started.status, 201, "{}", started.body);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        let mut view = f.get(&exe).await.body;
        for _ in 0..20 {
            if view["steps"][0]["status"] == "ready" {
                break;
            }
            drainer.drain_once().await.unwrap();
            view = f.get(&exe).await.body;
        }
        let sr = view["steps"][0]["id"].as_str().unwrap().to_owned();
        let claim = call(
            &f.app,
            &f.token,
            "POST",
            &format!("/v1/step-runs/{sr}/claim"),
            json!({"session_id": "web"}),
            Some("claim-go"),
            None,
        )
        .await;
        assert_eq!(claim.status, 200, "{}", claim.body);
        let reported = call(
            &f.app,
            &f.token,
            "POST",
            &format!("/v1/step-runs/{sr}/report"),
            json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded",
                   "verdict": null, "outputs": {}, "summary": "done",
                   "evidence": [], "logs": []}),
            Some("report-go"),
            None,
        )
        .await;
        assert_eq!(reported.status, 200, "{}", reported.body);
        let view = f.get(&exe).await.body;
        assert_eq!(view["status"], "failed", "{}", view["status"]);
        let step = |path: &str| {
            view["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["path"] == path)
                .map(|s| s["status"].clone())
                .unwrap_or(Value::Null)
        };
        assert_eq!(step("s1"), "succeeded");
        assert_ne!(step("s15"), "succeeded");
        // The stored log: the fault recorded once, at most one settle round
        // past the ceiling, and the close.
        let pg = runbook_store_postgres::PostgresStore::connect(db.config())
            .await
            .unwrap();
        let row = pg
            .client()
            .query_one(
                "SELECT coalesce(sum(octet_length(body)), 0)::bigint, \
                 count(*) FILTER (WHERE body LIKE $2)::bigint \
                 FROM runbook.execution_event WHERE execution_id = $1",
                &[&exe, &format!("%{}%", runbook_engine::RUN_SIZE_EXCEEDED)],
            )
            .await
            .unwrap();
        let (logged, faults): (i64, i64) = (row.get(0), row.get(1));
        assert_eq!(faults, 1, "the fault is recorded once");
        assert!(
            (600_000..700_000).contains(&logged),
            "{logged} bytes of log"
        );
        // The closed run still answers a cancel (a no-op).
        let cancelled = call(
            &f.app,
            &f.token,
            "PATCH",
            &format!("/v1/executions/{exe}"),
            json!({"action": "cancel"}),
            Some("cancel-sets"),
            None,
        )
        .await;
        assert_eq!(cancelled.status, 200, "{}", cancelled.body);
        assert_eq!(f.get(&exe).await.body["status"], "failed");
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// CF13b review round 1, finding 1, natively: activation settled with no
/// ceiling, so `set` steps that run before the first step needing a person
/// left the run open past it (budget 400 KB, ceiling 600 KB: thirty `set`
/// steps each copying a 60,000-byte input left it `running` with about
/// 1.9 MB of log). Activation is now held to the ceiling: the run is
/// faulted and closes failed in its activation commit, about one
/// `DataChanged` past the ceiling, and the person's step never opens.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn an_activation_that_passes_the_ceiling_closes_the_run_natively() {
    let _slot = FixtureSlot::acquire();
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let f = Fixture::with_run_budget(&db, Some(400_000)).await;
        let sets: String = (1..=30)
            .map(|n| {
                format!("    - {{id: s{n}, set: {{vars: {{a: \"${{{{ inputs.blob }}}}\"}}}}}}\n")
            })
            .collect();
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {{id: sets, title: Sets}}\nspec:\n  \
             inputs: {{blob: {{type: string, required: true}}}}\n  vars: {{a: ''}}\n  steps:\n\
             {sets}    - {{id: hold, title: Hold, assignee: \"user:{}\"}}\n",
            f.user
        );
        let rb = f.create("sets").await;
        f.publish(&rb, source, "sets-v1").await;
        let started = f
            .start(
                json!({"runbook": "sets", "inputs": {"blob": "b".repeat(60_000)}}),
                Some("sets"),
            )
            .await;
        assert_eq!(started.status, 201, "{}", started.body);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        let mut view = f.get(&exe).await.body;
        for _ in 0..20 {
            if view["status"] != "queued" {
                break;
            }
            drainer.drain_once().await.unwrap();
            view = f.get(&exe).await.body;
        }
        assert_eq!(view["status"], "failed", "{}", view["status"]);
        let hold = view["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "hold")
            .map(|s| s["status"].clone())
            .unwrap_or(Value::Null);
        assert!(
            !matches!(hold.as_str(), Some("ready" | "claimed")),
            "{hold}"
        );
        let pg = runbook_store_postgres::PostgresStore::connect(db.config())
            .await
            .unwrap();
        let row = pg
            .client()
            .query_one(
                "SELECT coalesce(sum(octet_length(body)), 0)::bigint, \
                 count(*) FILTER (WHERE body LIKE $2)::bigint, \
                 count(*) FILTER (WHERE body LIKE '%\"DataChanged\"%')::bigint, \
                 count(*) FILTER (WHERE body LIKE '%\"ExecutionCompleted\"%')::bigint, \
                 count(DISTINCT body::jsonb->>'at_ms')::bigint \
                 FROM runbook.execution_event WHERE execution_id = $1",
                &[&exe, &format!("%{}%", runbook_engine::RUN_SIZE_EXCEEDED)],
            )
            .await
            .unwrap();
        let (logged, faults, changed, completed, instants): (i64, i64, i64, i64, i64) =
            (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4));
        assert_eq!(faults, 1, "the fault is recorded once");
        assert_eq!(completed, 1, "the run closes");
        assert!(changed < 30, "{changed} set steps ran");
        // Every event is the activation's: one commit at its admission time.
        assert_eq!(instants, 1, "events at {instants} instants");
        assert!(
            (600_000..700_000).contains(&logged),
            "{logged} bytes of log"
        );
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// LANG9 over HTTP: a signal wait's `outputs` schema checks the payload. An
/// invalid payload is a 422 naming `payload.<field>`, nothing is recorded and
/// the step keeps waiting; a valid one becomes the step's outputs (declared
/// fields only), readable by later steps and shown in the run document. The
/// step's labels are returned with it.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn a_signal_payload_is_checked_and_becomes_the_steps_outputs() {
    with_fixture(async |f, _db| {
        let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: onboard, title: Onboard}\nspec:\n  steps:\n    - id: connect\n      labels: {audience: customer}\n      wait:\n        signal: shopify_connected\n        outputs: {store_id: {type: string, required: true}}\n    - id: welcome\n      instructions: \"Store ${{ steps.connect.outputs.store_id }}\"\n";
        let rb = f.create("onboard").await;
        f.publish(&rb, source.into(), "onboard-v1").await;
        let started = f.start(json!({"runbook": "onboard"}), Some("o1")).await;
        assert_eq!(started.status, 201, "{}", started.body);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        for _ in 0..20 {
            if f.get(&exe).await.body["status"] == "waiting" {
                break;
            }
            drainer.drain_once().await.unwrap();
        }
        let signal = |key: &'static str, payload: Value| {
            let (app, token, exe) = (f.app.clone(), f.token.clone(), exe.clone());
            async move {
                call(
                    &app,
                    &token,
                    "POST",
                    &format!("/v1/executions/{exe}/signals/shopify_connected"),
                    json!({"payload": payload}),
                    Some(key),
                    None,
                )
                .await
            }
        };
        let invalid = signal("bad-1", json!({"plan": "pro"})).await;
        assert_eq!(invalid.status, 422, "{}", invalid.body);
        assert_eq!(invalid.body["code"], "signal.invalid_payload", "{}", invalid.body);
        assert_eq!(invalid.body["errors"][0]["path"], "payload.store_id", "{}", invalid.body);

        let document = |exe: String| {
            let (app, token) = (f.app.clone(), f.token.clone());
            async move {
                call(&app, &token, "GET", &format!("/v1/executions/{exe}/report"), Value::Null, None, None)
                    .await
            }
        };
        let doc = document(exe.clone()).await;
        let connect = doc.body["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "connect")
            .cloned()
            .unwrap();
        assert_eq!(connect["status"], "waiting", "{}", doc.body);
        assert_eq!(connect["labels"]["audience"], "customer", "{}", doc.body);

        let valid = signal("good-1", json!({"store_id": "shop_42", "extra": 1})).await;
        assert!(valid.status < 300, "{}", valid.body);
        for _ in 0..20 {
            drainer.drain_once().await.unwrap();
            let body = document(exe.clone()).await.body;
            if body["sections"].as_array().unwrap().iter().any(|s| s["path"] == "welcome") {
                break;
            }
        }
        let doc = document(exe.clone()).await;
        let section = |path: &str| {
            doc.body["sections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["path"] == path)
                .cloned()
                .unwrap_or(Value::Null)
        };
        assert_eq!(section("connect")["status"], "succeeded", "{}", doc.body);
        assert_eq!(section("connect")["outputs"], json!({"store_id": "shop_42"}), "{}", doc.body);
        assert!(section("welcome").is_object(), "{}", doc.body);
    })
    .await;
}

/// LANG9b: a published runbook's webhook trigger starts a run from the
/// caller's payload (once per `dedupe`), and its schedule starts one run per
/// tick however many passes or replicas see it.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn triggers_start_runs_from_webhooks_and_schedules() {
    with_fixture(async |f, _db| {
        let source = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: deploy-check, title: Deploy check}
spec:
  inputs:
    service: {type: string, required: true}
    version: {type: string, default: latest}
  triggers:
    schedule:
      - {id: minutely, cron: "* * * * *", inputs: {service: billing}}
    webhook:
      - id: on_deploy
        payload:
          status: {type: string, required: true}
          repository: {type: object, properties: {name: {type: string}}}
          deployment: {type: object, properties: {id: {type: integer}}}
        when: "${{ payload.status == 'success' }}"
        inputs: {service: "${{ payload.repository.name }}"}
        dedupe: "${{ payload.deployment.id }}"
  steps: [{id: check, title: Check}]
"#;
        let rb = f.create("deploy-check").await;
        f.publish(&rb, source.into(), "deploy-v1").await;
        let path = format!(
            "/v1/projects/{}/runbooks/deploy-check/triggers/on_deploy",
            f.project
        );
        let deploy = json!({
            "status": "success",
            "repository": {"name": "api", "private": true},
            "deployment": {"id": 7}
        });
        let first = call(&f.app, &f.token, "POST", &path, deploy.clone(), None, None).await;
        assert_eq!(first.status, 201, "{}", first.body);
        assert_eq!(first.body["started"], true);
        let exe = first.body["execution"]["id"].as_str().unwrap().to_owned();
        assert_eq!(first.body["execution"]["created_by"], "webhook:on_deploy");
        // The same deployment again (a retried delivery): the same run.
        let again = call(&f.app, &f.token, "POST", &path, deploy.clone(), None, None).await;
        assert_eq!(again.status, 200, "{}", again.body);
        assert_eq!(again.body["duplicate"], true);
        assert_eq!(again.body["execution"]["id"], exe.as_str());
        // `when` false: nothing starts.
        let skipped = call(
            &f.app,
            &f.token,
            "POST",
            &path,
            json!({"status": "failure", "deployment": {"id": 8}}),
            None,
            None,
        )
        .await;
        assert_eq!(skipped.status, 200, "{}", skipped.body);
        assert_eq!(skipped.body["started"], false);
        assert_eq!(skipped.body["reason"], "when");
        // A payload that breaks its schema names the field.
        let invalid = call(
            &f.app,
            &f.token,
            "POST",
            &path,
            json!({"repository": {"name": 5}}),
            None,
            None,
        )
        .await;
        assert_eq!(invalid.status, 422, "{}", invalid.body);
        assert_eq!(invalid.body["code"], "trigger.invalid_payload");
        // An unknown trigger, and no credential at all.
        let unknown = call(
            &f.app,
            &f.token,
            "POST",
            &path.replace("on_deploy", "nope"),
            deploy.clone(),
            None,
            None,
        )
        .await;
        assert_eq!(unknown.status, 404, "{}", unknown.body);
        let anonymous = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header("content-type", "application/json")
                    .body(Body::from(deploy.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous.status().as_u16(), 401);
        // A sender that can't set headers puts the token in the URL.
        let by_query = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{path}?token={}", f.token))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"status": "success", "repository": {"name": "web"}, "deployment": {"id": 9}})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(by_query.status().as_u16(), 201);

        // The schedule: one run for this minute, however many passes.
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        let pass = drainer.schedule_if_due().await.unwrap().expect("due");
        assert_eq!(pass.scheduled, 1, "{pass:?}");
        assert!(pass.failed.is_empty(), "{pass:?}");
        let started = pass.started;
        assert!(drainer.schedule_if_due().await.unwrap().is_none(), "once a minute");
        let mut replica = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        let other = replica.schedule_if_due().await.unwrap().expect("due");
        // A pass at the turn of a minute may see the next tick; never the
        // same tick twice.
        assert!(started + other.started <= 2, "{pass:?} {other:?}");
        assert_eq!(started, 1, "{pass:?}");
        let runs = call(
            &f.app,
            &f.token,
            "GET",
            &format!("/v1/projects/{}/executions", f.project),
            Value::Null,
            None,
            None,
        )
        .await;
        let scheduled = runs.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["created_by"] == "schedule:minutely")
            .count();
        assert!((1..=2).contains(&scheduled), "{}", runs.body);
    })
    .await;
}

/// The self-hosted step cap (`RUNBOOK_LIMIT_STEP_RUNS`, here 2): `/v1/meta`
/// reports it, publishing a runbook whose worst case is over it is allowed,
/// and a run that would open a third step fails with
/// `limit.step_runs_exceeded`, keeping the two it did.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn a_run_stops_at_the_servers_step_cap() {
    let _slot = FixtureSlot::acquire();
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let f = Fixture::with_step_cap(&db, 2).await;
        let meta = call(&f.app, &f.token, "GET", "/v1/meta", Value::Null, None, None).await;
        assert_eq!(meta.status, 200, "{}", meta.body);
        assert_eq!(meta.body["limits"]["step_runs"], 2);

        let rb = f.create("capped").await;
        let steps: Vec<String> = (1..=3)
            .map(|n| {
                format!(
                    "{{id: s{n}, title: Step {n}, assignee: \"user:{}\"}}",
                    f.user
                )
            })
            .collect();
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {{id: capped, title: Capped}}\n\
             spec:\n  steps: [{}]\n",
            steps.join(", ")
        );
        f.publish(&rb, source, "capped-v1").await;
        let started = f.start(json!({"runbook": "capped"}), Some("capped")).await;
        assert_eq!(started.status, 201, "{}", started.body);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        let mut drainer = runbook_server::drain::Drainer::new(f.config.clone(), Default::default());
        for n in 0..2 {
            let mut view = f.get(&exe).await.body;
            for _ in 0..20 {
                if view["steps"][n]["status"] == "ready" {
                    break;
                }
                drainer.drain_once().await.unwrap();
                view = f.get(&exe).await.body;
            }
            let sr = view["steps"][n]["id"].as_str().unwrap().to_owned();
            let claim = call(
                &f.app,
                &f.token,
                "POST",
                &format!("/v1/step-runs/{sr}/claim"),
                json!({"session_id": "web"}),
                Some(&format!("claim-{n}")),
                None,
            )
            .await;
            assert_eq!(claim.status, 200, "{}", claim.body);
            let reported = call(
                &f.app,
                &f.token,
                "POST",
                &format!("/v1/step-runs/{sr}/report"),
                json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded",
                       "verdict": null, "outputs": {}, "summary": "done",
                       "evidence": [], "logs": []}),
                Some(&format!("report-{n}")),
                None,
            )
            .await;
            assert_eq!(reported.status, 200, "{}", reported.body);
        }
        let mut view = f.get(&exe).await.body;
        for _ in 0..20 {
            if view["status"] == "failed" {
                break;
            }
            drainer.drain_once().await.unwrap();
            view = f.get(&exe).await.body;
        }
        assert_eq!(view["status"], "failed", "{view}");
        let opened: Vec<&Value> = view["steps"].as_array().unwrap().iter().collect();
        assert_eq!(opened.len(), 2, "{view}");
        assert!(opened.iter().all(|s| s["status"] == "succeeded"), "{view}");
        assert_eq!(
            view["terminal_reason"], "limit.step_runs_exceeded",
            "{view}"
        );
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// A raw GET: its status and content type (an image is not JSON).
async fn fetch(app: &Router, token: &str, path: &str) -> (u16, String) {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let kind = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    (response.status().as_u16(), kind)
}

/// A tiny PNG header of `width`×`height` (the server reads only the header).
fn png(width: u32, height: u32) -> String {
    use base64::Engine as _;
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&width.to_be_bytes());
    b.extend_from_slice(&height.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Profiles and project settings (2026-11-17): a person renames themself and
/// sets or clears their photo; anyone in the org reads it; a project admin
/// renames a project and sets its image; bad images are refused.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn people_rename_themselves_and_set_pictures_and_projects_change_settings() {
    with_fixture(async |f, _db| {
        let send = |method: &'static str, path: String, body: Value| {
            let app = f.app.clone();
            let token = f.token.clone();
            async move { call(&app, &token, method, &path, body, None, None).await }
        };
        // Name.
        let renamed = send("PATCH", "/v1/me".into(), json!({"name": "  Jordan Lee "})).await;
        assert_eq!(renamed.status, 200, "{}", renamed.body);
        assert_eq!(renamed.body["name"], "Jordan Lee");
        assert_eq!(
            send("GET", "/v1/me".into(), Value::Null).await.body["user"]["name"],
            "Jordan Lee"
        );
        assert_eq!(
            send("PATCH", "/v1/me".into(), json!({"name": " "}))
                .await
                .status,
            422
        );
        // Photo.
        let user_picture = format!("/v1/users/{}/picture", f.user);
        assert_eq!(fetch(&f.app, &f.token, &user_picture).await.0, 404);
        let set = send(
            "PUT",
            "/v1/me/picture".into(),
            json!({"data": png(256, 256)}),
        )
        .await;
        assert_eq!(set.status, 200, "{}", set.body);
        assert_eq!(set.body["content_type"], "image/png");
        assert_eq!(
            fetch(&f.app, &f.token, &user_picture).await,
            (200, "image/png".to_owned())
        );
        let too_big = send(
            "PUT",
            "/v1/me/picture".into(),
            json!({"data": png(1024, 10)}),
        )
        .await;
        assert_eq!(
            (too_big.status, too_big.body["code"].clone()),
            (422, json!("picture.too_large"))
        );
        let gif = send(
            "PUT",
            "/v1/me/picture".into(),
            json!({"data": "R0lGODlhAQABAAAAACw="}),
        )
        .await;
        assert_eq!(
            (gif.status, gif.body["code"].clone()),
            (422, json!("picture.unsupported_format"))
        );
        assert_eq!(
            send("DELETE", "/v1/me/picture".into(), Value::Null)
                .await
                .status,
            204
        );
        assert_eq!(fetch(&f.app, &f.token, &user_picture).await.0, 404);
        // Project settings and image.
        let project = format!("/v1/projects/{}", f.project);
        let titled = send("PATCH", project.clone(), json!({"title": "Payments"})).await;
        assert_eq!(titled.status, 200, "{}", titled.body);
        assert_eq!(titled.body["title"], "Payments");
        let listed = send("GET", format!("/v1/orgs/{}/projects", f.org), Value::Null).await;
        assert!(
            listed.body.to_string().contains("Payments"),
            "{}",
            listed.body
        );
        let image = format!("{project}/picture");
        assert_eq!(fetch(&f.app, &f.token, &image).await.0, 404);
        assert_eq!(
            send("PUT", image.clone(), json!({"data": png(512, 512)}))
                .await
                .status,
            200
        );
        assert_eq!(fetch(&f.app, &f.token, &image).await.0, 200);
        assert_eq!(send("DELETE", image.clone(), Value::Null).await.status, 204);
        assert_eq!(fetch(&f.app, &f.token, &image).await.0, 404);
    })
    .await;
}
