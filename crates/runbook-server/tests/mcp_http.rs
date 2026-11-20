//! MCP1: the remote MCP endpoint, `/mcp` (MCP Streamable HTTP, stateless,
//! JSON responses only). Offline tests cover what is decided before any
//! credential lookup; the Postgres suite covers authentication, the tools
//! and execution-credential isolation through the real router and drain.
#[path = "support/auth_stub.rs"]
mod auth_stub;
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response},
};
use futures_util::FutureExt;
use runbook_core::{
    credentials::{
        ActorKind, CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind,
        encode_secret, secret_hash,
    },
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, OrgRole, UserRecord},
};
use runbook_model::{OrganizationId, UserId};
use runbook_server::{
    config::{AuthConfig, ServerConfig},
    drain::{DrainSettings, Drainer},
    http::{Worker, router},
};
use runbook_store_postgres::{DatabaseConfig, PostgresCredentials, PostgresDirectory};
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, time::Duration};
use tower::ServiceExt;

const ORIGIN: &str = "https://runbooks.example";
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
metadata: {id: mcp-run, title: MCP run}\n\
spec:\n  steps:\n    - id: check\n      title: Check\n      actor: agent\n      \
instructions: Check the service.\n      outputs:\n        status_code: {type: integer, required: true}\n    \
- id: notify\n      title: Notify\n      actor: agent\n      instructions: Post the result.\n";

struct Reply {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Value,
    text: String,
}

async fn reply(response: Response<Body>) -> Reply {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 4 * 1_048_576).await.unwrap();
    Reply {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// A POST to `/mcp` as an MCP client sends it.
fn post(token: Option<&str>) -> axum::http::request::Builder {
    let mut request = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    reply(app.clone().oneshot(request).await.unwrap()).await
}

async fn rpc(app: &Router, token: &str, message: Value) -> Reply {
    send(
        app,
        post(Some(token))
            .header("mcp-protocol-version", "2025-06-18")
            .body(Body::from(message.to_string()))
            .unwrap(),
    )
    .await
}

fn initialize(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "initialize",
           "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}})
}
fn tool(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}
/// The control line of a tool result.
fn control(r: &Reply) -> Value {
    let text = r.body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("not a tool result: {}", r.text));
    serde_json::from_str(text.lines().next().unwrap()).unwrap()
}

fn offline_app() -> (Worker, Router) {
    let config = ServerConfig::new(
        DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None)
            .unwrap(),
        ORIGIN,
        "127.0.0.1:8080",
    )
    .unwrap();
    let worker = Worker::start(config).unwrap();
    let app = router(worker.client());
    (worker, app)
}

fn assert_no_session(r: &Reply) {
    assert!(
        !r.headers.contains_key("mcp-session-id"),
        "the server must not issue a session"
    );
}

#[tokio::test]
async fn get_and_delete_are_405_without_a_session() {
    let (_worker, app) = offline_app();
    for method in ["GET", "DELETE", "PUT"] {
        let r = send(
            &app,
            Request::builder()
                .method(method)
                .uri("/mcp")
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, 405, "{method}: {}", r.text);
        assert_eq!(r.headers["allow"], "POST");
        assert_no_session(&r);
    }
}

#[tokio::test]
async fn no_bearer_is_401_with_a_bearer_challenge() {
    let (_worker, app) = offline_app();
    let message = initialize(1);
    let r = send(
        &app,
        post(None).body(Body::from(message.to_string())).unwrap(),
    )
    .await;
    assert_eq!(r.status, 401, "{}", r.text);
    assert!(
        r.headers["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Bearer"),
    );
    assert_no_session(&r);
    // Another scheme is no bearer either.
    let r = send(
        &app,
        post(None)
            .header("authorization", "Basic YWxpY2U6c2VjcmV0")
            .body(Body::from(message.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 401, "{}", r.text);
}

/// The stub auth child's entry point (`support/auth_stub.rs`): a no-op
/// unless this binary was started as the child.
#[test]
fn auth_stub_child() {
    if std::env::var_os(auth_stub::ENV).is_some() {
        auth_stub::run();
    }
}

/// D46: with sign-in configured, a cookie that resolves on `/v1` (the child
/// is asked about it there) is never even looked up on `/mcp`, even with the
/// right Origin and CSRF header: no bearer is 401.
#[tokio::test]
async fn a_session_cookie_alone_is_refused() {
    let files = auth_stub::Files::new("mcp-cookie");
    files.put_session("tok-a", "ba_a", "a@example.test");
    let mut auth = AuthConfig::new(auth_stub::command("auth_stub_child"));
    auth.child_env = files.env();
    auth.methods = vec!["email".into()];
    let mut config = ServerConfig::new(
        DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None)
            .unwrap(),
        ORIGIN,
        "127.0.0.1:0",
    )
    .unwrap();
    config.auth = Some(auth);
    let worker = Worker::start(config).unwrap();
    let client = worker.client();
    let child = client.auth_child().expect("auth child");
    assert!(child.wait_ready(Duration::from_secs(20)), "no ready line");
    let app = router(worker.client());
    let cookie = "__Host-runbook.session_token=tok-a";
    // On /v1 the same cookie is resolved with the child.
    let before = files.log().len();
    let r = send(
        &app,
        Request::builder()
            .uri("/v1/me")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        files.log().len() > before,
        "the cookie should resolve on /v1: {}",
        r.text
    );
    let before = files.log().len();
    let message = initialize(1);
    let r = send(
        &app,
        post(None)
            .header("cookie", cookie)
            .header("origin", ORIGIN)
            .header("x-runbook-csrf", "1")
            .body(Body::from(message.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 401, "{}", r.text);
    assert!(r.headers.contains_key("www-authenticate"));
    assert_eq!(
        files.log().len(),
        before,
        "the child was asked: {:?}",
        files.log()
    );
}

#[tokio::test]
async fn a_foreign_origin_is_refused_before_anything_else() {
    let (_worker, app) = offline_app();
    let message = initialize(1);
    for origin in [
        "https://evil.example",
        "http://runbooks.example",
        "null",
        "https://runbooks.example.evil.example",
    ] {
        let r = send(
            &app,
            post(Some("trb_pat_x"))
                .header("origin", origin)
                .body(Body::from(message.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, 403, "{origin}: {}", r.text);
        assert_no_session(&r);
    }
    // GET with a foreign origin is refused too, not just 405.
    let r = send(
        &app,
        Request::builder()
            .method("GET")
            .uri("/mcp")
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.text);
}

#[tokio::test]
async fn meta_advertises_the_endpoint() {
    let (_worker, app) = offline_app();
    let r = send(
        &app,
        Request::builder()
            .uri("/v1/meta")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["capabilities"]["mcp"], true, "{}", r.text);
}

#[tokio::test]
async fn the_web_ui_never_answers_for_mcp() {
    // Only the exact path is the endpoint; `/mcp/x` is an unknown API route.
    let (_worker, app) = offline_app();
    let r = send(
        &app,
        Request::builder()
            .uri("/mcp/x")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 404, "{}", r.text);
}

// ---------------------------------------------------------------- Postgres

struct Fixture {
    _worker: Worker,
    app: Router,
    drainer: Drainer,
    owner: String,
    service: String,
    project: String,
    org: String,
    database: DatabaseConfig,
}

async fn http(
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
    send(app, request.body(body).unwrap()).await
}

impl Fixture {
    async fn new(db: &database::TestDatabase) -> Self {
        Self::with(db, |_| {}).await
    }
    async fn with(db: &database::TestDatabase, tweak: impl FnOnce(&mut ServerConfig)) -> Self {
        let mut config = ServerConfig::new(db.config().clone(), ORIGIN, "127.0.0.1:0").unwrap();
        tweak(&mut config);
        let issued = runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let worker = Worker::start(config.clone()).unwrap();
        let app = router(worker.client());
        let fixture = Self {
            drainer: Drainer::new(
                config.clone(),
                DrainSettings {
                    batch: 64,
                    lease_ms: 30_000,
                    idle: Duration::from_millis(10),
                },
            ),
            _worker: worker,
            app,
            owner: issued["pat"].as_str().unwrap().into(),
            service: issued["service"].as_str().unwrap().into(),
            project: issued["project_id"].as_str().unwrap().into(),
            org: issued["org_id"].as_str().unwrap().into(),
            database: db.config().clone(),
        };
        let created = http(
            &fixture.app,
            &fixture.owner,
            "POST",
            &format!("/v1/projects/{}/runbooks", fixture.project),
            json!({"source": SPEC}),
            Some("create"),
            None,
        )
        .await;
        assert_eq!(created.status, 201, "{}", created.text);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = http(
            &fixture.app,
            &fixture.owner,
            "GET",
            &format!("/v1/runbooks/{rb}/draft"),
            Value::Null,
            None,
            None,
        )
        .await;
        let published = http(
            &fixture.app,
            &fixture.owner,
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            json!({}),
            Some("publish"),
            draft.headers.get("etag").and_then(|v| v.to_str().ok()),
        )
        .await;
        assert_eq!(published.status, 201, "{}", published.text);
        fixture
    }
    async fn drain(&mut self) {
        for _ in 0..20 {
            let stats = self.drainer.drain_once().await.unwrap();
            if !stats.busy() {
                break;
            }
        }
    }
    /// Start a run over `/mcp` with the owner's PAT, activate it, and mint
    /// an execution pair for it with the service token.
    async fn run(&mut self, n: u64) -> (String, String) {
        let started = rpc(
            &self.app,
            &self.owner,
            tool(
                n,
                "start_execution",
                json!({"project": self.project, "runbook": "mcp-run", "idempotency_key": format!("start-{n}")}),
            ),
        )
        .await;
        assert_eq!(started.status, 200, "{}", started.text);
        let exe = control(&started)["execution_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{}", started.text))
            .to_owned();
        self.drain().await;
        let minted = http(
            &self.app,
            &self.service,
            "POST",
            &format!("/v1/executions/{exe}/tokens"),
            json!({"session_id": format!("agent-{n}")}),
            Some(&format!("mint-{n}")),
            None,
        )
        .await;
        assert_eq!(minted.status, 201, "{}", minted.text);
        (
            exe,
            minted.body["access_token"].as_str().unwrap().to_owned(),
        )
    }
}

fn with_db<F>(body: F)
where
    F: for<'a> FnOnce(&'a database::TestDatabase) -> futures_util::future::LocalBoxFuture<'a, ()>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut db = database::TestDatabase::create_from_env().await.unwrap();
        let result = AssertUnwindSafe(body(&db)).catch_unwind().await;
        db.finish(result).await;
    });
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn a_pat_initializes_and_lists_tools_without_a_session() {
    with_db(|db| {
        Box::pin(async move {
            let f = Fixture::new(db).await;
            let r = rpc(&f.app, &f.owner, initialize(1)).await;
            assert_eq!(r.status, 200, "{}", r.text);
            assert_eq!(r.headers["content-type"], "application/json");
            assert_no_session(&r);
            assert_eq!(r.body["id"], 1);
            assert_eq!(r.body["result"]["protocolVersion"], "2025-06-18");
            assert_eq!(r.body["result"]["serverInfo"]["name"], "runbook");
            assert!(r.body["result"]["capabilities"]["resources"].is_object());
            let r = rpc(
                &f.app,
                &f.owner,
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            assert_eq!(
                r.body["result"]["tools"],
                json!(runbook_mcp::tool_definitions())
            );
            // A notification or a client response is accepted with no body.
            for message in [
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 9, "result": {}}),
            ] {
                let r = rpc(&f.app, &f.owner, message).await;
                assert_eq!(r.status, 202, "{}", r.text);
                assert!(r.text.is_empty(), "{}", r.text);
                assert_no_session(&r);
            }
            // The service token works the same way.
            let r = rpc(
                &f.app,
                &f.service,
                tool(3, "list_runbooks", json!({"project": f.project})),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            assert_eq!(control(&r)["items"][0]["slug"], "mcp-run", "{}", r.text);
        })
    });
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn transport_rules_are_enforced() {
    with_db(|db| {
        Box::pin(async move {
            let f = Fixture::new(db).await;
            let message = initialize(1);
            // A well-formed but unknown token, a refresh token and garbage.
            for token in [
                format!("trb_pat_{}", "A".repeat(43)),
                format!("trb_exr_{}", "A".repeat(43)),
                format!("trb_exe_{}", "A".repeat(43)),
                "not-a-token".into(),
            ] {
                let r = rpc(&f.app, &token, message.clone()).await;
                assert_eq!(r.status, 401, "{token}: {}", r.text);
                assert!(
                    r.headers["www-authenticate"]
                        .to_str()
                        .unwrap()
                        .contains("invalid_token"),
                );
            }
            // The configured origin is accepted.
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .header("origin", ORIGIN)
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            // An unsupported protocol version header.
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .header("mcp-protocol-version", "1999-01-01")
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 400, "{}", r.text);
            // 2025-03-26 allows batches, which this endpoint refuses, so
            // only 2025-06-18 is served here; no header at all is fine.
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .header("mcp-protocol-version", "2025-03-26")
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 400, "{}", r.text);
            let older = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "old", "version": "1"}}});
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .body(Body::from(older.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            assert_eq!(
                r.body["result"]["protocolVersion"], "2025-06-18",
                "{}",
                r.text
            );
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 200, "no header: {}", r.text);
            // Accept must allow JSON; a missing Accept is anything.
            let r = send(
                &f.app,
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("accept", "text/html")
                    .header("authorization", format!("Bearer {}", f.owner))
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 406, "{}", r.text);
            let r = send(
                &f.app,
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {}", f.owner))
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            // JSON only.
            let r = send(
                &f.app,
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("content-type", "text/plain")
                    .header("authorization", format!("Bearer {}", f.owner))
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 415, "{}", r.text);
            // Batches are refused (2025-06-18 removed them; stdio refuses them too).
            let batch = json!([initialize(1), initialize(2)]);
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .body(Body::from(batch.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 400, "{}", r.text);
            assert_eq!(r.body["error"]["code"], -32600);
            // Malformed JSON is a parse error.
            let r = send(
                &f.app,
                post(Some(&f.owner)).body(Body::from("{nope")).unwrap(),
            )
            .await;
            assert_eq!(r.status, 400, "{}", r.text);
            assert_eq!(r.body["error"]["code"], -32700);
            // The body limit applies as on every other JSON route.
            let big = json!({"jsonrpc": "2.0", "id": 1, "method": "ping", "params": {"pad": "x".repeat(3 * 1_048_576)}});
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .header("content-length", big.to_string().len())
                    .body(Body::from(big.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 413, "{}", r.text);
            // A cookie next to a bearer changes nothing: the bearer is used.
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .header("cookie", "runbook.session_token=abc")
                    .body(Body::from(message.to_string()))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            assert_eq!(r.headers["x-ratelimit-limit"], "unlimited");
            assert!(r.headers.contains_key("x-request-id"));
        })
    });
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn an_execution_token_acts_only_on_its_own_execution() {
    with_db(|db| {
        Box::pin(async move {
            let mut f = Fixture::new(db).await;
            let (exe_a, token_a) = f.run(1).await;
            let (exe_b, token_b) = f.run(2).await;
            // B's ready step, seen with B's own token.
            let next_b = rpc(
                &f.app,
                &token_b,
                tool(1, "get_next_steps", json!({"execution_id": exe_b})),
            )
            .await;
            let sr_b = control(&next_b)["ready_work"][0]["step_run_id"]
                .as_str()
                .unwrap_or_else(|| panic!("{}", next_b.text))
                .to_owned();
            // A's token can neither read B nor claim B's step.
            let r = rpc(
                &f.app,
                &token_a,
                tool(2, "get_next_steps", json!({"execution_id": exe_b})),
            )
            .await;
            assert_eq!(r.status, 200);
            assert_eq!(r.body["result"]["isError"], true, "{}", r.text);
            assert_eq!(control(&r)["error"]["status"], 403, "{}", r.text);
            let r = rpc(
                &f.app,
                &token_a,
                tool(
                    3,
                    "claim_step",
                    json!({"step_run_id": sr_b, "session_id": "agent-1", "agent_name": "intruder"}),
                ),
            )
            .await;
            assert_eq!(r.body["result"]["isError"], true, "{}", r.text);
            assert_eq!(control(&r)["error"]["status"], 403, "{}", r.text);
            // Nor read the catalog: no resources, no source.
            let r = rpc(
                &f.app,
                &token_a,
                json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
            )
            .await;
            assert_eq!(r.body["result"]["resources"], json!([]), "{}", r.text);
            let r = rpc(
                &f.app,
                &token_a,
                tool(
                    5,
                    "get_runbook_source",
                    json!({"runbook": "mcp-run", "project": f.project}),
                ),
            )
            .await;
            assert_eq!(r.body["result"]["isError"], true, "{}", r.text);

            // A's own loop works, and after the final report the revoked
            // credential answers with exit, statelessly.
            let mut id = 10;
            let mut reported = None;
            loop {
                let brief = rpc(
                    &f.app,
                    &token_a,
                    tool(id, "get_next_steps", json!({"execution_id": exe_a})),
                )
                .await;
                if control(&brief)["next_action"] != "claim_ready_work" {
                    break;
                }
                let item = control(&brief)["ready_work"][0].clone();
                let sr = item["step_run_id"].as_str().unwrap().to_owned();
                let claimed = rpc(
                    &f.app,
                    &token_a,
                    tool(
                        id + 1,
                        "claim_step",
                        json!({"step_run_id": sr, "session_id": "agent-1", "agent_name": "claude", "model": "opus"}),
                    ),
                )
                .await;
                let claim = control(&claimed)["claim"].clone();
                assert!(claim.is_object(), "{}", claimed.text);
                let step = rpc(
                    &f.app,
                    &token_a,
                    tool(id + 2, "get_step", json!({"step_run_id": sr})),
                )
                .await;
                assert!(step.body["result"]["isError"] != true, "{}", step.text);
                let outputs = if item["path"] == "check" {
                    json!({"status_code": 200})
                } else {
                    json!({})
                };
                let report = rpc(
                    &f.app,
                    &token_a,
                    tool(
                        id + 3,
                        "report_step",
                        json!({"step_run_id": sr, "claim": claim, "status": "succeeded", "outputs": outputs}),
                    ),
                )
                .await;
                assert!(report.body["result"]["isError"] != true, "{}", report.text);
                f.drain().await;
                reported = Some(report);
                id += 4;
            }
            let next = reported.expect("A had agent work");
            assert_eq!(control(&next)["next_action"], "exit", "{}", next.text);
            let after = rpc(
                &f.app,
                &token_a,
                tool(id, "get_next_steps", json!({"execution_id": exe_a})),
            )
            .await;
            assert_eq!(after.status, 200, "{}", after.text);
            assert_eq!(control(&after)["next_action"], "exit", "{}", after.text);
        })
    });
}

/// A PAT stored directly, with the given lifetime and revocation.
async fn insert_pat(
    database: &DatabaseConfig,
    n: u8,
    org: &str,
    user: &str,
    expires_at: i64,
    revoked_at: Option<i64>,
) -> String {
    let secret = encode_secret(TokenKind::Personal, [n; 32]);
    PostgresCredentials::new(database.clone())
        .insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: user.parse().unwrap(),
                actor: ActorKind::Human,
            },
            org_id: org.parse().unwrap(),
            scopes: [
                Scope::RunbooksRead,
                Scope::ExecutionsRead,
                Scope::ExecutionsStart,
                Scope::ExecutionsAct,
            ]
            .into(),
            projects: None,
            expires_at: Some(expires_at),
            revoked_at,
            last_used_at: None,
        })
        .await
        .unwrap();
    secret
}

/// A second organization with its owner; returns the owner's user ID.
async fn foreign_org(database: &DatabaseConfig, n: u8) -> (String, String) {
    let org: OrganizationId = format!("org_{n:026}").parse().unwrap();
    let user: UserId = format!("usr_{n:026}").parse().unwrap();
    PostgresDirectory::new(database.clone())
        .commit(DirectoryCommit {
            org_id: org.clone(),
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: "Foreign".into(),
                },
                DirectoryMutation::CreateUser(UserRecord {
                    id: user.clone(),
                    name: "Foreign owner".into(),
                    active: true,
                    role: OrgRole::Owner,
                }),
            ],
        })
        .await
        .unwrap();
    (org.to_string(), user.to_string())
}

fn rpc_error(r: &Reply) -> (i64, Value) {
    (
        r.body["error"]["code"]
            .as_i64()
            .unwrap_or_else(|| panic!("not an error: {}", r.text)),
        r.body["error"]["data"].clone(),
    )
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn messages_in_flight_are_capped_and_the_rest_answer_busy_at_once() {
    with_db(|db| {
        Box::pin(async move {
            let f = Fixture::with(db, |c| c.transport.mcp_in_flight = 1).await;
            // Message 1 holds the only slot while its body trickles in.
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
            let stream = futures_util::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|chunk| (chunk, rx))
            });
            let first = f.app.clone().oneshot(
                post(Some(&f.owner))
                    .body(Body::from_stream(stream))
                    .unwrap(),
            );
            let first = tokio::spawn(first);
            tx.send(Ok(b"{\"jsonrpc\":\"2.0\",".to_vec()))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            let started = std::time::Instant::now();
            let r = rpc(&f.app, &f.owner, initialize(2)).await;
            assert_eq!(r.status, 503, "{}", r.text);
            assert_eq!(rpc_error(&r).1["code"], "server.busy");
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "not refused at once"
            );
            tx.send(Ok(b"\"id\":1,\"method\":\"ping\"}".to_vec()))
                .await
                .unwrap();
            drop(tx);
            let first = reply(first.await.unwrap().unwrap()).await;
            assert_eq!(first.status, 200, "{}", first.text);
            // The slot is free again.
            let r = rpc(&f.app, &f.owner, initialize(3)).await;
            assert_eq!(r.status, 200, "{}", r.text);
        })
    });
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn a_walk_past_the_call_cap_ends_in_an_error() {
    with_db(|db| {
        Box::pin(async move {
            let f = Fixture::with(db, |c| c.transport.mcp_calls_per_message = 2).await;
            // orgs, projects, runbooks: three calls.
            let r = rpc(
                &f.app,
                &f.owner,
                json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            let (code, data) = rpc_error(&r);
            assert_eq!(code, -32603, "{}", r.text);
            assert_eq!(data["code"], "limit.mcp_calls", "{}", r.text);
        })
    });
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn a_family_revoked_on_a_live_run_is_an_invalid_token() {
    with_db(|db| {
        Box::pin(async move {
            let mut f = Fixture::new(db).await;
            let (exe, token) = f.run(1).await;
            // A second pair for the same run, whose family a person revokes.
            let family = {
                let minted = http(
                    &f.app,
                    &f.service,
                    "POST",
                    &format!("/v1/executions/{exe}/tokens"),
                    json!({"session_id": "agent-revoked"}),
                    Some("mint-revoked"),
                    None,
                )
                .await;
                assert_eq!(minted.status, 201, "{}", minted.text);
                (
                    minted.body["family_id"].as_str().unwrap().to_owned(),
                    minted.body["access_token"].as_str().unwrap().to_owned(),
                )
            };
            let revoked = http(
                &f.app,
                &f.service,
                "DELETE",
                &format!("/v1/executions/{exe}/credentials/{}", family.0),
                Value::Null,
                None,
                None,
            )
            .await;
            assert!(revoked.status < 300, "{}", revoked.text);
            // The run is still live: a revoked family is not "the run ended".
            let r = rpc(&f.app, &family.1, initialize(1)).await;
            assert_eq!(r.status, 401, "{}", r.text);
            assert!(
                r.headers["www-authenticate"]
                    .to_str()
                    .unwrap()
                    .contains("invalid_token")
            );
            // The other family still works.
            let r = rpc(&f.app, &token, initialize(2)).await;
            assert_eq!(r.status, 200, "{}", r.text);
        })
    });
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn credentials_see_only_what_rest_lets_them_see() {
    with_db(|db| {
        Box::pin(async move {
            let mut f = Fixture::new(db).await;
            let listed = rpc(
                &f.app,
                &f.owner,
                json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}),
            )
            .await;
            let uri = listed.body["result"]["resources"][0]["uri"]
                .as_str()
                .unwrap_or_else(|| panic!("{}", listed.text))
                .to_owned();
            let rb = uri.rsplit('/').next().unwrap().to_owned();

            // An execution token reads no runbook and gets no prompt.
            let (_, token) = f.run(1).await;
            let r = rpc(
                &f.app,
                &token,
                json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": uri}}),
            )
            .await;
            assert!(r.body["result"].is_null(), "{}", r.text);
            assert_eq!(rpc_error(&r).1["status"], 403, "{}", r.text);
            let r = rpc(
                &f.app,
                &token,
                json!({"jsonrpc": "2.0", "id": 3, "method": "prompts/get",
                       "params": {"name": "run_runbook", "arguments": {"runbook": rb, "project": f.project}}}),
            )
            .await;
            assert!(r.body["result"].is_null(), "{}", r.text);

            // A PAT of another organization: the runbook does not exist for it.
            let (org2, user2) = foreign_org(&f.database, 62).await;
            let foreign = insert_pat(&f.database, 62, &org2, &user2, i64::MAX, None).await;
            let r = rpc(
                &f.app,
                &foreign,
                json!({"jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": {"uri": uri}}),
            )
            .await;
            assert_eq!(rpc_error(&r).0, -32002, "{}", r.text);
            let r = rpc(
                &f.app,
                &foreign,
                json!({"jsonrpc": "2.0", "id": 5, "method": "resources/list"}),
            )
            .await;
            assert_eq!(r.body["result"]["resources"], json!([]), "{}", r.text);

            // Revoked and expired PATs of a real user are refused outright.
            let owner_user = {
                let me = http(&f.app, &f.owner, "GET", "/v1/me", Value::Null, None, None).await;
                me.body["user"]["id"].as_str().unwrap().to_owned()
            };
            let revoked = insert_pat(&f.database, 63, &f.org, &owner_user, i64::MAX, Some(1)).await;
            let expired = insert_pat(&f.database, 64, &f.org, &owner_user, 1, None).await;
            for token in [&revoked, &expired] {
                let r = rpc(&f.app, token, initialize(6)).await;
                assert_eq!(r.status, 401, "{}", r.text);
                assert!(
                    r.headers["www-authenticate"]
                        .to_str()
                        .unwrap()
                        .contains("invalid_token")
                );
            }

            // A service token is bound to its project: a second project's
            // runbook is not among its resources.
            let project2 = http(
                &f.app,
                &f.owner,
                "POST",
                &format!("/v1/orgs/{}/projects", f.org),
                json!({"slug": "second", "title": "Second"}),
                Some("project-2"),
                None,
            )
            .await;
            assert_eq!(project2.status, 201, "{}", project2.text);
            let project2 = project2.body["id"].as_str().unwrap().to_owned();
            let created = http(
                &f.app,
                &f.owner,
                "POST",
                &format!("/v1/projects/{project2}/runbooks"),
                json!({"source": SPEC.replace("mcp-run", "elsewhere")}),
                Some("create-2"),
                None,
            )
            .await;
            assert_eq!(created.status, 201, "{}", created.text);
            let rb2 = created.body["runbook_id"].as_str().unwrap().to_owned();
            let draft = http(
                &f.app,
                &f.owner,
                "GET",
                &format!("/v1/runbooks/{rb2}/draft"),
                Value::Null,
                None,
                None,
            )
            .await;
            let published = http(
                &f.app,
                &f.owner,
                "POST",
                &format!("/v1/runbooks/{rb2}/versions"),
                json!({}),
                Some("publish-2"),
                draft.headers.get("etag").and_then(|v| v.to_str().ok()),
            )
            .await;
            assert_eq!(published.status, 201, "{}", published.text);
            let names = |r: &Reply| -> Vec<String> {
                r.body["result"]["resources"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{}", r.text))
                    .iter()
                    .map(|x| x["name"].as_str().unwrap().to_owned())
                    .collect()
            };
            let all = rpc(
                &f.app,
                &f.owner,
                json!({"jsonrpc": "2.0", "id": 7, "method": "resources/list"}),
            )
            .await;
            assert_eq!(names(&all), ["mcp-run", "elsewhere"], "{}", all.text);
            let scoped = rpc(
                &f.app,
                &f.service,
                json!({"jsonrpc": "2.0", "id": 8, "method": "resources/list"}),
            )
            .await;
            assert_eq!(names(&scoped), ["mcp-run"], "{}", scoped.text);
        })
    });
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn a_chunked_oversized_body_is_refused() {
    with_db(|db| {
        Box::pin(async move {
            let f = Fixture::new(db).await;
            let chunk = vec![b' '; 256 * 1024];
            let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
                (0..12).map(|_| Ok(chunk.clone())).collect();
            let r = send(
                &f.app,
                post(Some(&f.owner))
                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 413, "{}", r.text);
        })
    });
}

/// CF0 review: a long `/mcp` message holds a worker slot only while each of
/// its API calls runs, as before CF0, so REST requests interleave with it.
/// Holding one slot per message let a few long messages starve REST.
#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn long_mcp_messages_leave_worker_slots_for_rest() {
    with_db(|db| {
        Box::pin(async move {
            let f = Fixture::with(db, |c| {
                c.transport.max_in_flight = 2;
                c.transport.mcp_in_flight = 8;
            })
            .await;
            // `resources/list` makes one API call per project.
            for i in 0..120 {
                let r = http(
                    &f.app,
                    &f.owner,
                    "POST",
                    &format!("/v1/orgs/{}/projects", f.org),
                    json!({"slug": format!("p{i}"), "title": format!("P{i}")}),
                    Some(&format!("project-{i}")),
                    None,
                )
                .await;
                assert_eq!(r.status, 201, "{}", r.text);
            }
            let list = json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"});
            let started = std::time::Instant::now();
            let r = rpc(&f.app, &f.owner, list.clone()).await;
            assert_eq!(r.status, 200, "{}", r.text);
            let walk = started.elapsed();
            // As many walks as worker slots, then a REST request mid-walk.
            let walks: Vec<_> = (0..2)
                .map(|_| {
                    let (app, owner, list) = (f.app.clone(), f.owner.clone(), list.clone());
                    tokio::spawn(async move { rpc(&app, &owner, list).await.status })
                })
                .collect();
            tokio::time::sleep(walk / 4).await;
            assert!(walks.iter().all(|w| !w.is_finished()), "walks too short");
            let me = http(&f.app, &f.owner, "GET", "/v1/me", Value::Null, None, None).await;
            assert_eq!(me.status, 200, "{}", me.text);
            assert!(
                walks.iter().all(|w| !w.is_finished()),
                "REST waited for a whole /mcp message (one walk: {walk:?})"
            );
            for w in walks {
                assert_eq!(w.await.unwrap(), 200);
            }
        })
    });
}

/// Universal search, and so the MCP `search` tool, offers only active
/// memory: a proposal and an archived entry stay out for every caller, as
/// on the memory routes. `fetch` refuses them too.
#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn search_and_fetch_offer_only_active_memory() {
    with_db(|db| {
        Box::pin(async move {
            let f = Fixture::new(db).await;
            let save = |title: &'static str, propose: bool, key: &'static str| {
                let (app, owner, project) = (f.app.clone(), f.owner.clone(), f.project.clone());
                async move {
                    let r = http(
                        &app,
                        &owner,
                        "POST",
                        &format!("/v1/projects/{project}/memory"),
                        json!({"title": title, "body_markdown": format!("{title}: zebra crossing notes."),
                               "tags": [], "propose": propose}),
                        Some(key),
                        None,
                    )
                    .await;
                    assert_eq!(r.status, 201, "{}", r.text);
                    r.body["id"].as_str().unwrap().to_owned()
                }
            };
            let active = save("Zebra live fact", false, "mem-active").await;
            let proposed = save("Zebra proposed fact", true, "mem-proposed").await;
            let archived = save("Zebra archived fact", false, "mem-archived").await;
            let r = http(
                &f.app,
                &f.owner,
                "PATCH",
                &format!("/v1/memory/{archived}"),
                json!({"action": "archive"}),
                Some("mem-archive"),
                Some("\"r1\""),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.text);
            assert_eq!(r.body["status"], "archived", "{}", r.text);

            let agent = http(
                &f.app,
                &f.owner,
                "POST",
                "/v1/tokens",
                json!({"name": "research", "actor_kind": "agent"}),
                Some("agent-token"),
                None,
            )
            .await;
            assert_eq!(agent.status, 201, "{}", agent.text);
            let agent = agent.body["secret"].as_str().unwrap().to_owned();

            // The universal search, for a person and for an agent.
            for token in [&f.owner, &agent] {
                let r = http(
                    &f.app,
                    token,
                    "GET",
                    &format!("/v1/orgs/{}/search?q=zebra", f.org),
                    Value::Null,
                    None,
                    None,
                )
                .await;
                assert_eq!(r.status, 200, "{}", r.text);
                let ids: Vec<&str> = r.body["memory"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m["memory_id"].as_str().unwrap())
                    .collect();
                assert_eq!(ids, vec![active.as_str()], "{}", r.text);
            }

            // MCP `search` over /mcp, as the agent and as the owner.
            for (n, token) in [(1, &agent), (2, &f.owner)] {
                let r = rpc(&f.app, token, tool(n, "search", json!({"query": "zebra"}))).await;
                assert_eq!(r.status, 200, "{}", r.text);
                let results = &r.body["result"]["structuredContent"]["results"];
                let ids: Vec<&str> = results
                    .as_array()
                    .unwrap_or_else(|| panic!("{}", r.text))
                    .iter()
                    .map(|m| m["id"].as_str().unwrap())
                    .collect();
                assert_eq!(ids, vec![active.as_str()], "{}", r.text);
                assert!(!r.text.contains("proposed fact") && !r.text.contains("archived fact"));
            }

            // `fetch`: the live entry reads; the others are not found.
            let r = rpc(&f.app, &agent, tool(3, "fetch", json!({"id": active}))).await;
            let doc = &r.body["result"]["structuredContent"];
            assert!(
                doc["text"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{}", r.text))
                    .contains("zebra crossing notes")
            );
            assert_eq!(
                doc["url"],
                format!("{ORIGIN}/p/default/memory"),
                "{}",
                r.text
            );
            for (n, token) in [(4, &agent), (5, &f.owner)] {
                for id in [&proposed, &archived] {
                    let r = rpc(&f.app, token, tool(n, "fetch", json!({"id": id}))).await;
                    assert_eq!(r.body["result"]["isError"], true, "{}", r.text);
                    assert_eq!(control(&r)["next_action"], "check_ids", "{}", r.text);
                    assert!(!r.text.contains("zebra crossing"), "{}", r.text);
                }
            }
        })
    });
}
