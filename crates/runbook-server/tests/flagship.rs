//! C4: the unmodified flagship integration gate (Gate C exit).
//!
//! `conformance/spec/post-deploy.yaml` (the PRD example, byte for byte) is
//! published through the real authoring routes and run end to end on
//! PostgreSQL through the real HTTP router, the drain and the webhook
//! delivery worker, with:
//!
//! - a directory built through the directory routes: Alice and Bob in
//!   `oncall`, Carol in `eng-leads`, all project operators;
//! - independent credentials minted through `POST /v1/tokens`: one human
//!   PAT each, and a launcher service token (issued by the bootstrap owner)
//!   that mints execution credential pairs for agent sessions;
//! - the two pinned skill fixtures the flagship names
//!   (`./skills/check-error-rates`, `./skills/rollback-service`);
//! - two webhook subscriptions on real loopback receivers: a relay for
//!   `agent.work_available` and run lifecycle, and a notifier for human
//!   events. Every delivery is checked with `verify_signature` on arrival.
//!
//! Elapsed time (30-minute approvals, 5-minute loop delay, 10-minute agent
//! lease, 2-hour deadline, 30-minute cleanup) is simulated with a clock owned
//! by the test database: every store reads `clock_timestamp()`, and the
//! owned database's `search_path` resolves it to a shifted copy first. No
//! other database is touched.
//!
//! C4 coverage:
//! - Queueing: `flagship_queues_per_service_and_runs_services_concurrently`.
//! - Healthy path, identities, checklist, step reads (`with`, skills,
//!   instructions per audience), default report JSON/Markdown, severity
//!   query: `flagship_healthy_path_with_independent_identities`.
//! - Unhealthy/keep, loop recovery, team-membership approvals, human
//!   deliveries, indexed severity: `flagship_unhealthy_keep_after_loop_recovery`.
//! - Loop exhaustion, manual rollback, Session A exits on a wait, server
//!   restart, Session B gets a new pair and the brief, cleanup dispatch:
//!   `flagship_loop_exhaustion_restart_and_fresh_session_rollback`.
//! - Escalation to Carol, expiry pause, late decision while open:
//!   `flagship_escalation_to_carol_expiry_and_late_decision`.
//! - Deadline expiry, late decision rejected, cleanup inside and past
//!   `cleanup_timeout`: `flagship_deadline_rejects_late_decision_and_bounds_cleanup`.
//! - Unavailable agent (lease expiry, redispatch, stale report refused) and
//!   failure handoff to on-call: `flagship_unavailable_agent_is_redispatched_then_handed_off`.
//! - Lost refresh response, family revocation, claim recovery:
//!   `flagship_lost_refresh_response_revokes_family_and_recovers_the_claim`.
//! - Amendment and rerun: `flagship_amendment_and_rerun_preserve_outcome_and_signatures`.
//! - Deliberate auto-rollback and an explicit report template, in the PRD's
//!   documented companion variant: `companion_auto_rollback_with_custom_report`.
//!
//! Gaps (product behaviour found while writing this gate; not faked):
//! - Deliberate auto-rollback is not expressible in the unmodified flagship
//!   (its approval has no `auto` option); it is run in the PRD §3 variant.
//!   That automatic decision emits no `approval.decided` webhook, though
//!   PRD §9 says the event covers system-policy decisions.
//! - The flagship has no `spec.report`, so the explicit template is rendered
//!   in that companion variant only.
//! - No login flow issues a person's first credential, so each person's
//!   `tokens:manage` seed PAT is inserted into the credential store; their
//!   working PATs and the launcher service token are minted through
//!   `POST /v1/tokens`, and every call authenticates over HTTP.
//! - No GitHub Actions relay example exists to point at a mock GitHub API;
//!   the recording relay plus the launcher token stand in for it.
//! - External operation IDs cannot be reconciled (no such report field), so
//!   "no blind duplicate rollback" is shown only as a refused stale report.
//! - Memory across runs is DEFERRED with B10.
//!
//! Production fixes found by this gate (separate `fix: ... (C4)` commits,
//! each with its own failing test): iteration paths prefixed twice in the
//! brief/document/inbox; the advance-time wakeup never re-armed after an
//! escalation (expiry and cleanup timeout never fired from the drain);
//! handed-off agent tasks unclaimable by people; step reads showing
//! `skill: null` for skills named through a `spec.skills` alias; the run
//! view naming steps by node ID (three `error_rate`s) and dropping B9a
//! amendment facts; run search items losing `created_at_ms` once
//! events were appended; titles and approval prompts shown raw
//! (`Error rate in ${{ region }}`) in the brief, document and inbox; and
//! `approval.escalated`/`approval.expired` naming the `eng-leads` team
//! instead of its active users.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::FutureExt;
use runbook_core::{
    credentials::*,
    webhooks::{SIGNATURE_TOLERANCE_S, verify_signature},
};
use runbook_model::*;
use runbook_server::{
    config::{ServerConfig, WebhookConfig},
    drain::{DrainSettings, Drainer},
    http::{Worker, router},
};
use runbook_store_postgres::{PostgresCredentials, PostgresStore};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

/// The PRD flagship, byte for byte (conformance/README.md).
const FLAGSHIP: &str = include_str!("../../../conformance/spec/post-deploy.yaml");
const MINUTE: i64 = 60_000;
const PARAMS: &str = r#"{"repo":"acme/api","workflow":"runbook-resume.yml"}"#;

// ---------------------------------------------------------------------------
// Recording receiver: a real HTTP/1.1 listener on loopback.
// ---------------------------------------------------------------------------
#[derive(Clone, Debug)]
struct Received {
    headers: BTreeMap<String, String>,
    body: String,
}
impl Received {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
            .unwrap_or("")
    }
}
#[derive(Clone)]
struct Receiver {
    port: u16,
    seen: Arc<Mutex<Vec<Received>>>,
}
impl Receiver {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let receiver = Self {
            port: listener.local_addr().unwrap().port(),
            seen: Arc::default(),
        };
        let shared = receiver.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                    if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(i);
                    }
                };
                let Some(head_end) = head_end else { continue };
                let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
                let headers: BTreeMap<String, String> = head
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let length: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = buffer[head_end + 4..].to_vec();
                while body.len() < length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                    }
                }
                shared.seen.lock().unwrap().push(Received {
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        receiver
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }
    fn seen(&self) -> Vec<Received> {
        self.seen.lock().unwrap().clone()
    }
    /// Envelopes of one type for one execution, in delivery order.
    fn events(&self, kind: &str, exe: &str) -> Vec<Value> {
        self.seen()
            .iter()
            .map(Received::json)
            .filter(|e| e["type"] == kind && e["execution"]["id"] == exe)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// HTTP helper.
// ---------------------------------------------------------------------------
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
    text: String,
    /// The raw body, for binary exports (REP2's PDF).
    bytes: Vec<u8>,
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
    let mut request = Request::builder().method(method).uri(path);
    if !token.is_empty() {
        request = request.header("authorization", format!("Bearer {token}"));
    }
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
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().into());
    let bytes = to_bytes(response.into_body(), 4 * 1_048_576).await.unwrap();
    Reply {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
        bytes: bytes.to_vec(),
        etag,
    }
}

// ---------------------------------------------------------------------------
// Fixture.
// ---------------------------------------------------------------------------
struct Person {
    id: UserId,
    token: String,
}
struct Fixture {
    config: ServerConfig,
    worker: Worker,
    app: Router,
    drainer: Drainer,
    clock: PostgresStore,
    owner: String,
    org: OrganizationId,
    project: ProjectId,
    alice: Person,
    bob: Person,
    carol: Person,
    launcher: String,
    relay: Receiver,
    relay_secret: String,
    notifier: Receiver,
    notifier_secret: String,
    /// Deliveries already signature-checked, per receiver.
    verified: (usize, usize),
}
fn settings() -> DrainSettings {
    DrainSettings {
        batch: 64,
        lease_ms: 30_000,
        idle: Duration::from_millis(10),
    }
}
impl Fixture {
    async fn new(db: &database::TestDatabase) -> Self {
        // A test-owned clock for the owned database only: every store reads
        // `clock_timestamp()`, which resolves to this shifted copy first.
        let clock = PostgresStore::connect(db.config()).await.unwrap();
        clock
            .client()
            .batch_execute(&format!(
                "CREATE SCHEMA rb_test_clock;\
                 CREATE TABLE rb_test_clock.skew(ms bigint NOT NULL);\
                 INSERT INTO rb_test_clock.skew VALUES (0);\
                 CREATE FUNCTION rb_test_clock.clock_timestamp() RETURNS timestamptz \
                 LANGUAGE sql VOLATILE AS $$ SELECT pg_catalog.clock_timestamp() \
                 + (SELECT ms FROM rb_test_clock.skew) * interval '1 millisecond' $$;\
                 ALTER DATABASE {} SET search_path = rb_test_clock, pg_catalog, public;",
                db.name()
            ))
            .await
            .unwrap();
        let mut config = ServerConfig::new(
            db.config().clone(),
            "https://runbooks.example",
            "127.0.0.1:0",
        )
        .unwrap();
        // Self-hosted webhook settings (RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS,
        // RUNBOOK_WEBHOOK_SECRET_KEY): loopback receivers are allowed.
        config.webhooks = WebhookConfig {
            allow_private_targets: true,
            allow_link_local_targets: false,
            secret_key: Some([5; 32]),
            resolver: None,
            ..Default::default()
        };
        let issued = runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let worker = Worker::start(config.clone()).unwrap();
        let app = router(worker.client());
        let owner: String = issued["pat"].as_str().unwrap().into();
        let org: OrganizationId = issued["org_id"].as_str().unwrap().parse().unwrap();
        let project: ProjectId = issued["project_id"].as_str().unwrap().parse().unwrap();
        let admin = |method: &'static str, path: String, body: Value, key: String| {
            let app = app.clone();
            let owner = owner.clone();
            async move {
                let r = call(&app, &owner, method, &path, body, Some(&key), None).await;
                assert!(r.status < 300, "{method} {path}: {} {}", r.status, r.text);
                r.body
            }
        };
        // Directory through the real routes: three people, two teams.
        let mut people = vec![];
        for name in ["Alice", "Bob", "Carol"] {
            let user = admin(
                "POST",
                format!("/v1/orgs/{org}/users"),
                json!({"name": name}),
                format!("user-{name}"),
            )
            .await;
            let id: UserId = user["id"].as_str().unwrap().parse().unwrap();
            admin(
                "POST",
                format!("/v1/projects/{project}/members"),
                json!({"user_id": id, "role": "operator"}),
                format!("member-{name}"),
            )
            .await;
            people.push(id);
        }
        for (slug, title, members) in [
            (
                "oncall",
                "On-call",
                vec![people[0].clone(), people[1].clone()],
            ),
            ("eng-leads", "Engineering leads", vec![people[2].clone()]),
        ] {
            let team = admin(
                "POST",
                format!("/v1/orgs/{org}/teams"),
                json!({"slug": slug, "title": title}),
                format!("team-{slug}"),
            )
            .await;
            let team = team["id"].as_str().unwrap().to_owned();
            for member in members {
                admin(
                    "POST",
                    format!("/v1/orgs/{org}/teams/{team}/members"),
                    json!({"user_id": member}),
                    format!("team-{slug}-{member}"),
                )
                .await;
            }
        }
        // Independent credentials, issued through `POST /v1/tokens`. No
        // login flow exists yet, so each person's first credential (a
        // `tokens:manage` seed, standing in for a login) is inserted as an
        // operator would; each person then mints their working PAT from it,
        // narrowed to the execution scopes and this project.
        let credentials = PostgresCredentials::new(config.database.clone());
        let mut tokens = vec![];
        for (n, user) in people.iter().enumerate() {
            let seed = encode_secret(TokenKind::Personal, [0x40 + n as u8; 32]);
            credentials
                .insert(CredentialRecord {
                    id: format!("tok_{:026}", 40 + n).parse().unwrap(),
                    kind: TokenKind::Personal,
                    hash: secret_hash(&seed),
                    principal: CredentialPrincipal::User {
                        user_id: user.clone(),
                        actor: runbook_core::credentials::ActorKind::Human,
                    },
                    org_id: org.clone(),
                    scopes: [
                        Scope::ExecutionsRead,
                        Scope::ExecutionsStart,
                        Scope::ExecutionsAct,
                        Scope::TokensManage,
                    ]
                    .into(),
                    projects: Some([project.clone()].into()),
                    expires_at: Some(i64::MAX),
                    revoked_at: None,
                    last_used_at: None,
                })
                .await
                .unwrap();
            let minted = call(
                &app,
                &seed,
                "POST",
                "/v1/tokens",
                json!({"name": "flagship", "scopes": ["executions:read", "executions:start", "executions:act"]}),
                Some("flagship-pat"),
                None,
            )
            .await;
            assert_eq!(minted.status, 201, "{}", minted.text);
            assert_eq!(minted.body["token"]["user_id"], user.as_str());
            assert_eq!(minted.body["token"]["projects"], json!([project]));
            tokens.push(minted.body["secret"].as_str().unwrap().to_owned());
        }
        // The trusted launcher: a project service token (D30), issued by the
        // bootstrap owner (an org owner, D17) with an operator role.
        let launcher = admin(
            "POST",
            "/v1/tokens".into(),
            json!({"kind": "service", "name": "launcher", "scopes": ["executions:read", "executions:act"], "projects": [project], "role": "operator"}),
            "launcher".into(),
        )
        .await;
        assert_eq!(launcher["token"]["role"], "operator");
        let launcher = launcher["secret"].as_str().unwrap().to_owned();
        assert!(launcher.starts_with("trb_svc_"));
        // Skill fixtures named by the flagship's `./skills/...` aliases.
        use base64::Engine;
        for (name, body) in [
            (
                "check-error-rates",
                "Query the error rate for `region` over `window` and attach the dashboard link.",
            ),
            (
                "rollback-service",
                "Roll the service back to the previous release; record the operation ID.",
            ),
        ] {
            let skill = format!("---\nname: {name}\ndescription: {name} fixture.\n---\n{body}\n");
            admin(
                "POST",
                format!("/v1/projects/{project}/skills"),
                json!({"files":[{"path":"SKILL.md","content":base64::engine::general_purpose::STANDARD.encode(skill)}]}),
                format!("skill-{name}"),
            )
            .await;
        }
        // Webhooks: a launcher relay and a human notifier.
        let relay = Receiver::start();
        let notifier = Receiver::start();
        let mut secrets = vec![];
        for (name, receiver, events) in [
            (
                "relay",
                &relay,
                json!([
                    "agent.work_available",
                    "execution.started",
                    "execution.completed"
                ]),
            ),
            (
                "notifier",
                &notifier,
                json!([
                    "approval.requested",
                    "approval.escalated",
                    "approval.expired",
                    "approval.decided",
                    "step.blocked"
                ]),
            ),
        ] {
            let created = admin(
                "POST",
                format!("/v1/projects/{project}/webhooks"),
                json!({"name": name, "url": receiver.url(), "events": events}),
                format!("hook-{name}"),
            )
            .await;
            secrets.push(created["secret"].as_str().unwrap().to_owned());
        }
        let [alice, bob, carol]: [String; 3] = tokens.try_into().unwrap();
        let fixture = Self {
            drainer: Drainer::new(config.clone(), settings()),
            config,
            worker,
            app,
            clock,
            owner,
            org,
            project,
            alice: Person {
                id: people[0].clone(),
                token: alice,
            },
            bob: Person {
                id: people[1].clone(),
                token: bob,
            },
            carol: Person {
                id: people[2].clone(),
                token: carol,
            },
            launcher,
            relay,
            relay_secret: secrets[0].clone(),
            notifier,
            notifier_secret: secrets[1].clone(),
            verified: (0, 0),
        };
        fixture.publish(FLAGSHIP, "flagship").await;
        fixture
    }
    /// Create and publish a runbook through the authoring routes.
    async fn publish(&self, source: &str, key: &str) -> Value {
        let created = self
            .call(
                &self.owner,
                "POST",
                &format!("/v1/projects/{}/runbooks", self.project),
                json!({"source": source}),
                Some(&format!("create-{key}")),
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.text);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = self
            .call(
                &self.owner,
                "GET",
                &format!("/v1/runbooks/{rb}/draft"),
                Value::Null,
                None,
            )
            .await;
        assert_eq!(draft.body["source"], source, "{}", draft.text);
        let published = call(
            &self.app,
            &self.owner,
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            json!({}),
            Some(&format!("publish-{key}")),
            draft.etag.as_deref(),
        )
        .await;
        assert_eq!(published.status, 201, "{}", published.text);
        published.body
    }
    async fn call(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Value,
        key: Option<&str>,
    ) -> Reply {
        call(&self.app, token, method, path, body, key, None).await
    }
    async fn get(&self, token: &str, path: &str) -> Reply {
        self.call(token, "GET", path, Value::Null, None).await
    }
    async fn start(&self, token: &str, service: &str, version: &str, key: &str) -> String {
        let r = self
            .call(
                token,
                "POST",
                &format!("/v1/projects/{}/executions", self.project),
                json!({"runbook":"post-deploy","inputs":{"service":service,"version":version}}),
                Some(key),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.text);
        r.body["id"].as_str().unwrap().to_owned()
    }
    async fn run(&self, exe: &str) -> Value {
        let r = self
            .get(&self.owner, &format!("/v1/executions/{exe}"))
            .await;
        assert_eq!(r.status, 200, "{}", r.text);
        r.body
    }
    /// The execution document (`GET /report`), read by the project owner.
    async fn document(&self, exe: &str) -> Value {
        let r = self
            .get(&self.owner, &format!("/v1/executions/{exe}/report"))
            .await;
        assert_eq!(r.status, 200, "{}", r.text);
        r.body
    }
    /// The document section for a step path (iteration paths included).
    async fn section(&self, exe: &str, path: &str) -> Value {
        let document = self.document(exe).await;
        document["sections"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|s| s["path"] == path)
            .unwrap_or_else(|| panic!("no section {path}: {document}"))
            .clone()
    }
    async fn sr(&self, exe: &str, path: &str) -> String {
        self.section(exe, path).await["step_run_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    async fn team(&self, slug: &str) -> String {
        let teams = self
            .get(&self.owner, &format!("/v1/orgs/{}/teams", self.org))
            .await;
        teams.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["slug"] == slug)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    /// Timers, activations and fan-out, then deliveries, until quiet. Every
    /// new delivery's signature is verified on arrival.
    async fn drain(&mut self) {
        for _ in 0..50 {
            let stats = self.drainer.drain_once().await.unwrap();
            assert_eq!(stats.failed, 0, "{stats:?}");
            let delivered = self.drainer.deliver_once().await.unwrap();
            assert_eq!(
                delivered.delivery_failures + delivered.delivery_errors,
                0,
                "{delivered:?}"
            );
            if !stats.busy() && !delivered.busy() {
                break;
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + self.skew_ms().await / 1000;
        for (receiver, secret, verified) in [
            (&self.relay, &self.relay_secret, &mut self.verified.0),
            (&self.notifier, &self.notifier_secret, &mut self.verified.1),
        ] {
            let seen = receiver.seen();
            for r in &seen[*verified..] {
                assert_eq!(
                    verify_signature(
                        secret,
                        r.header("Runbook-Timestamp"),
                        r.body.as_bytes(),
                        r.header("Runbook-Signature"),
                        now,
                        SIGNATURE_TOLERANCE_S
                    ),
                    Ok(()),
                    "{}",
                    r.body
                );
                assert_eq!(r.header("Runbook-Event-Id"), r.json()["id"]);
                assert_eq!(r.header("Runbook-Event-Type"), r.json()["type"]);
                assert!(!r.body.contains("trb_") && !r.body.contains("whsec_"));
            }
            *verified = seen.len();
        }
    }
    async fn skew_ms(&self) -> i64 {
        self.clock
            .client()
            .query_one("SELECT ms FROM rb_test_clock.skew", &[])
            .await
            .unwrap()
            .get(0)
    }
    /// Move the database clock forward.
    async fn advance(&self, ms: i64) {
        self.clock
            .client()
            .execute("UPDATE rb_test_clock.skew SET ms = ms + $1", &[&ms])
            .await
            .unwrap();
    }
    /// Stop the HTTP worker and the drain, and start new ones against the
    /// same database (a server restart).
    fn restart(&mut self) {
        let old = std::mem::replace(
            &mut self.worker,
            Worker::start(self.config.clone()).unwrap(),
        );
        drop(old);
        self.app = router(self.worker.client());
        self.drainer = Drainer::new(self.config.clone(), settings());
    }
    /// `agent.work_available` deliveries for a run: (reason, ready paths).
    fn work_available(&self, exe: &str) -> Vec<(String, Vec<String>)> {
        self.relay
            .events("agent.work_available", exe)
            .iter()
            .map(|e| {
                assert_eq!(
                    e["data"]["params"],
                    serde_json::from_str::<Value>(PARAMS).unwrap()
                );
                (
                    e["data"]["reason"].as_str().unwrap().to_owned(),
                    e["data"]["ready_steps"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|s| s["path"].as_str().unwrap().to_owned())
                        .collect(),
                )
            })
            .collect()
    }
}

const MAX_FIXTURES: usize = 2;
static FIXTURES: Mutex<usize> = Mutex::new(0);
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
async fn with_fixture(f: impl AsyncFnOnce(&mut Fixture)) {
    let _slot = FixtureSlot::acquire();
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut fixture = Fixture::new(&db).await;
        f(&mut fixture).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

// ---------------------------------------------------------------------------
// Actors: agent sessions with execution credentials, people with PATs.
// ---------------------------------------------------------------------------
struct Session {
    exe: String,
    id: String,
    access: String,
    refresh: String,
    family: String,
}
fn agent(session: &str) -> Value {
    json!({"session_id": session, "actor": {"kind": "agent", "agent_name": "claude", "model": "opus"}})
}
fn report(
    sr: &str,
    claim: &Value,
    status: &str,
    verdict: Option<&str>,
    outputs: Value,
    evidence: Value,
) -> Value {
    json!({"step_run_id": sr, "claim": claim, "status": status, "verdict": verdict,
        "outputs": outputs, "summary": format!("{status} {}", verdict.unwrap_or("-")),
        "evidence": evidence, "logs": []})
}
fn rate(i: usize, rate: f64) -> (Value, Value) {
    (
        json!({"rate": rate}),
        json!([format!("https://dashboards.example/region-{i}")]),
    )
}
impl Fixture {
    /// The trusted launcher (service token) mints a fresh pair for a session.
    async fn mint(&self, exe: &str, session: &str) -> Session {
        let r = self
            .call(
                &self.launcher,
                "POST",
                &format!("/v1/executions/{exe}/tokens"),
                json!({"session_id": session}),
                Some(&format!("mint-{session}")),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.text);
        let access: String = r.body["access_token"].as_str().unwrap().into();
        let refresh: String = r.body["refresh_token"].as_str().unwrap().into();
        assert!(access.starts_with("trb_exe_") && refresh.starts_with("trb_exr_"));
        Session {
            exe: exe.into(),
            id: session.into(),
            access,
            refresh,
            family: r.body["family_id"].as_str().unwrap().into(),
        }
    }
    async fn next(&self, token: &str, exe: &str) -> Value {
        let r = self.get(token, &format!("/v1/executions/{exe}/next")).await;
        assert_eq!(r.status, 200, "{}", r.text);
        r.body
    }
    /// Ready work offered to this credential's audience, by path.
    async fn ready(&self, token: &str, exe: &str) -> BTreeMap<String, String> {
        self.next(token, exe).await["ready_work"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                (
                    w["path"].as_str().unwrap().to_owned(),
                    w["step_run_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
    async fn try_claim(&self, token: &str, sr: &str, body: Value) -> Reply {
        let key = format!("claim-{sr}-{}", body["session_id"].as_str().unwrap());
        self.call(
            token,
            "POST",
            &format!("/v1/step-runs/{sr}/claim"),
            body,
            Some(&key),
        )
        .await
    }
    async fn claim(&self, token: &str, sr: &str, body: Value) -> Value {
        let r = self.try_claim(token, sr, body).await;
        assert_eq!(r.status, 200, "claim {sr}: {}", r.text);
        r.body
    }
    async fn report(&self, token: &str, sr: &str, body: Value, key: &str) -> Reply {
        self.call(
            token,
            "POST",
            &format!("/v1/step-runs/{sr}/report"),
            body,
            Some(key),
        )
        .await
    }
    /// One agent step taken from the brief: claim, read the step, report.
    async fn agent_step(
        &self,
        s: &Session,
        path: &str,
        verdict: Option<&str>,
        outputs: Value,
        evidence: Value,
    ) -> Value {
        let ready = self.ready(&s.access, &s.exe).await;
        let sr = ready
            .get(path)
            .unwrap_or_else(|| panic!("{path} not ready for the agent: {ready:?}"))
            .clone();
        let claim = self.claim(&s.access, &sr, agent(&s.id)).await;
        let read = self.get(&s.access, &format!("/v1/step-runs/{sr}")).await;
        assert_eq!(read.status, 200, "{}", read.text);
        let r = self
            .report(
                &s.access,
                &sr,
                report(&sr, &claim, "succeeded", verdict, outputs, evidence),
                &format!("report-{sr}"),
            )
            .await;
        assert_eq!(r.status, 200, "report {path}: {}", r.text);
        read.body
    }
    async fn decide(&self, token: &str, sr: &str, option: &str, key: &str) -> Reply {
        self.call(
            token,
            "POST",
            &format!("/v1/step-runs/{sr}/decisions"),
            json!({"option": option, "comment": format!("{option} ({key})")}),
            Some(key),
        )
        .await
    }
    /// Alice claims the human smoke check, ticks the checklist, reports pass.
    async fn alice_login_flow(&self, exe: &str) {
        let ready = self.ready(&self.alice.token, exe).await;
        let sr = ready["login_flow"].clone();
        let claim = self
            .claim(&self.alice.token, &sr, json!({"session_id": "alice-web"}))
            .await;
        let ticks = json!({"claim": claim, "checklist": [
            {"title":"Login page loads","completed":true},
            {"title":"Dashboard shows data","completed":true}]});
        let r = self
            .call(
                &self.alice.token,
                "POST",
                &format!("/v1/step-runs/{sr}/checklist"),
                ticks,
                Some(&format!("ticks-{sr}")),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text);
        let r = self
            .report(
                &self.alice.token,
                &sr,
                report(&sr, &claim, "succeeded", Some("pass"), json!({}), json!([])),
                &format!("report-{sr}"),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text);
    }
    /// Smoke (agent + Alice) and the three region checks with these rates.
    async fn checks(&mut self, s: &Session, health: u16, rates: [f64; 3]) {
        let verdict = |ok: bool| Some(if ok { "pass" } else { "fail" });
        self.agent_step(
            s,
            "api_health",
            verdict(health == 200),
            json!({"status_code": health}),
            json!([]),
        )
        .await;
        self.alice_login_flow(&s.exe).await;
        self.drain().await;
        for (i, r) in rates.into_iter().enumerate() {
            let (outputs, evidence) = rate(i, r);
            self.agent_step(
                s,
                &format!("regions[{i}].error_rate"),
                verdict(r <= 0.01),
                outputs,
                evidence,
            )
            .await;
        }
        self.drain().await;
    }
    /// The finally step, by an agent session.
    async fn notify(&mut self, s: &Session) {
        self.agent_step(s, "notify", None, json!({}), json!([]))
            .await;
        self.drain().await;
    }
}
fn status(run: &Value) -> (&str, &Value, &Value) {
    (
        run["status"].as_str().unwrap(),
        &run["result"],
        &run["fields"]["severity"],
    )
}

// ---------------------------------------------------------------------------
// C4 bullet 1: queueing per service, other services concurrent.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_queues_per_service_and_runs_services_concurrently() {
    with_fixture(async |f| {
        let first = f.start(&f.alice.token, "api", "v1.0.1", "api-1").await;
        let second = f.start(&f.bob.token, "api", "v1.0.2", "api-2").await;
        let web = f.start(&f.alice.token, "web", "v2.0.0", "web-1").await;
        f.drain().await;
        assert_eq!(f.run(&first).await["status"], "running");
        assert_eq!(
            f.run(&web).await["status"],
            "running",
            "another service runs concurrently"
        );
        let queued = f.run(&second).await;
        assert_eq!(queued["status"], "queued");
        assert_eq!(queued["admission"]["state"], "queued");
        assert_eq!(queued["steps"], json!([]));
        // Nothing to dispatch or mint for a queued run (D31).
        assert!(f.work_available(&second).is_empty());
        let refused = f
            .call(
                &f.launcher,
                "POST",
                &format!("/v1/executions/{second}/tokens"),
                json!({"session_id": "early"}),
                Some("early"),
            )
            .await;
        assert_eq!(
            (refused.status, refused.body["code"].as_str()),
            (409, Some("execution.not_started"))
        );
        // The first api run completes healthy; the queued one is promoted.
        let s = f.mint(&first, "api-1").await;
        f.checks(&s, 200, [0.001, 0.002, 0.003]).await;
        assert_eq!(
            f.run(&second).await["status"],
            "queued",
            "still queued while the first is open"
        );
        f.notify(&s).await;
        assert_eq!(
            status(&f.run(&first).await),
            ("succeeded", &json!("healthy"), &json!("none"))
        );
        let promoted = f.run(&second).await;
        assert_eq!(promoted["status"], "running", "{promoted}");
        assert_eq!(promoted["admission"]["state"], "admitted");
        assert_eq!(
            f.work_available(&second),
            vec![("initial".into(), vec!["api_health".into()])]
        );
        assert_eq!(f.run(&web).await["status"], "running");
        assert_eq!(f.relay.events("execution.started", &second).len(), 1);
    })
    .await;
}

// ---------------------------------------------------------------------------
// C4 bullets 2 and 8: healthy path, identities, step reads and the report.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_healthy_path_with_independent_identities() {
    with_fixture(async |f| {
        let exe = f.start(&f.alice.token, "api", "v1.2.3", "start").await;
        f.drain().await;
        assert_eq!(f.work_available(&exe), vec![("initial".into(), vec!["api_health".into()])]);
        assert_eq!(f.relay.events("execution.started", &exe)[0]["execution"]["title"], "Verify api v1.2.3");
        let s = f.mint(&exe, "session-a").await;
        // D32: the agent's credential sees agent work only, and no step it
        // has not claimed; the human sees the human check.
        let agent_ready = f.ready(&s.access, &exe).await;
        assert_eq!(agent_ready.keys().collect::<Vec<_>>(), ["api_health"]);
        let human_ready = f.ready(&f.alice.token, &exe).await;
        assert_eq!(human_ready.keys().collect::<Vec<_>>(), ["login_flow"]);
        let login = human_ready["login_flow"].clone();
        assert_eq!(f.get(&s.access, &format!("/v1/step-runs/{login}")).await.status, 403);
        let human_read = f.get(&f.alice.token, &format!("/v1/step-runs/{login}")).await;
        assert_eq!(human_read.body["checklist"], json!(["Login page loads", "Dashboard shows data"]));
        // The agent's instructions render the run's inputs.
        let health = f.agent_step(&s, "api_health", Some("pass"), json!({"status_code": 200}), json!([])).await;
        assert_eq!(health["instructions"], "Call /healthz on api and record the status code.");
        assert_eq!(health["output_schema"]["required"], json!(["status_code"]));
        // Alice: a success report with open checklist items is refused; Bob
        // cannot act on Alice's claim.
        let claim = f.claim(&f.alice.token, &login, json!({"session_id": "alice-web"})).await;
        let early = f
            .report(&f.alice.token, &login, report(&login, &claim, "succeeded", Some("pass"), json!({}), json!([])), "early")
            .await;
        assert_eq!((early.status, early.body["code"].as_str()), (422, Some("step.checklist_incomplete")));
        let foreign = f
            .call(&f.bob.token, "POST", &format!("/v1/step-runs/{login}/checklist"),
                json!({"claim": claim, "checklist": [{"title":"Login page loads","completed":true},{"title":"Dashboard shows data","completed":true}]}),
                Some("bob-ticks"))
            .await;
        assert!(foreign.status >= 400, "{}", foreign.text);
        let ticks = json!({"claim": claim, "checklist": [{"title":"Login page loads","completed":true},{"title":"Dashboard shows data","completed":true}]});
        assert_eq!(f.call(&f.alice.token, "POST", &format!("/v1/step-runs/{login}/checklist"), ticks, Some("ticks")).await.status, 200);
        let done = f
            .report(&f.alice.token, &login, report(&login, &claim, "succeeded", Some("pass"), json!({}), json!([])), "done")
            .await;
        assert_eq!(done.status, 200, "{}", done.text);
        f.drain().await;
        // Titles render per iteration (`Error rate in ${{ region }}`): the
        // brief agrees with the agent work notices.
        let ready_titles = |items: &Value| {
            let mut titles: Vec<String> = items
                .as_array()
                .unwrap()
                .iter()
                .map(|w| w["title"].as_str().unwrap().to_owned())
                .filter(|t| t.starts_with("Error rate"))
                .collect();
            titles.sort();
            titles
        };
        let expected = ["Error rate in eu-west", "Error rate in us-east", "Error rate in us-west"];
        assert_eq!(ready_titles(&f.next(&s.access, &exe).await["ready_work"]), expected);
        let noticed: Vec<Value> = f
            .relay
            .events("agent.work_available", &exe)
            .iter()
            .flat_map(|e| e["data"]["ready_steps"].as_array().unwrap().clone())
            .collect();
        assert_eq!(ready_titles(&Value::Array(noticed)), expected);
        // Regions: `with` context and the pinned skill reach the agent.
        let mut regions = vec![];
        for (i, name) in ["us-west", "us-east", "eu-west"].iter().enumerate() {
            let (outputs, evidence) = rate(i, 0.001);
            let read = f
                .agent_step(&s, &format!("regions[{i}].error_rate"), Some("pass"), outputs, evidence)
                .await;
            assert_eq!(read["parameters"], json!({"region": name, "window": "10m"}));
            assert!(read["skill"]["body"].as_str().unwrap().contains("Query the error rate"), "{read}");
            regions.push(read);
        }
        f.drain().await;
        // Healthy: stabilize is skipped, the default case marks the run.
        let brief = f.next(&s.access, &exe).await;
        assert_eq!(brief["ready_work"][0]["path"], "notify");
        assert!(brief["completed_steps"].as_array().unwrap().iter().any(|c| c["path"] == "stabilize" && c["status"] == "skipped"));
        f.notify(&s).await;
        let run = f.run(&exe).await;
        assert_eq!(status(&run), ("succeeded", &json!("healthy"), &json!("none")));
        let completed = f.relay.events("execution.completed", &exe);
        assert_eq!(completed[0]["data"], json!({"status": "succeeded", "result": "healthy"}));
        assert!(f.work_available(&exe).iter().any(|(reason, paths)| reason == "cleanup" && paths == &["notify"]));
        // Real identities in the document.
        let alice_section = f.section(&exe, "login_flow").await;
        assert_eq!(alice_section["actor"], json!({"kind": "human", "user_id": f.alice.id}));
        let health = f.section(&exe, "api_health").await;
        assert_eq!(health["actor"]["kind"], "agent");
        assert_eq!(health["actor"]["session_id"], "session-a");
        assert_eq!(health["actor"]["principal_id"], format!("execution:{}", s.family));
        assert_eq!(f.section(&exe, "regions[2].error_rate").await["evidence"], json!(["https://dashboards.example/region-2"]));
        assert_eq!(f.section(&exe, "regions[2].error_rate").await["title"], "Error rate in eu-west");
        // Default report (the flagship has no `spec.report`): JSON and Markdown.
        let document = f.document(&exe).await;
        assert_eq!(document["header"]["status"], "succeeded");
        assert_eq!(document["header"]["result"], "healthy");
        // B1 additive fields on the flagship document and run view.
        assert!(document["header"]["runbook_id"].is_string(), "{document}");
        assert_eq!(document["header"]["runbook_slug"], "post-deploy");
        assert_eq!(document["header"]["project_id"], f.project.as_str());
        assert!(document["header"]["created_at_ms"].is_number(), "{document}");
        assert!(document["header"]["created_by"].is_string(), "{document}");
        assert!(document["header"]["started_at_ms"].is_number(), "{document}");
        assert!(document["header"]["completed_at_ms"].is_number(), "{document}");
        assert_eq!(run["runbook_slug"], "post-deploy");
        assert!(run["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["kind"] == "task" && s["completed_at_ms"].is_number()), "{run}");
        assert!(document["sections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["kind"] == "task" && s["completed_at_ms"].is_number()), "{document}");
        let markdown = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=markdown")).await;
        assert_eq!(markdown.status, 200);
        assert!(markdown.text.contains("Health endpoint returns 200"), "{}", markdown.text);
        assert!(markdown.text.contains("## Error rate in eu\\-west (regions\\[2\\]\\.error\\_rate)"), "{}", markdown.text);
        assert!(!markdown.text.contains("## Report"), "no custom template");
        // People are named in the export, as on the page (walkthrough finding).
        assert!(markdown.text.contains("- Actor: Alice\n"), "{}", markdown.text);
        assert!(!markdown.text.contains("human usr"), "{}", markdown.text);
        // REP2: the JUnit and PDF exports of the same flagship document.
        let junit = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=junit")).await;
        assert_eq!(junit.status, 200, "{}", junit.text);
        assert!(junit.text.contains("<testsuite name=\"Verify api v1.2.3\""), "{}", junit.text);
        assert!(junit.text.contains("<testcase classname=\"post-deploy\" name=\"smoke\""), "{}", junit.text);
        assert!(junit.text.contains("Actor: Alice"), "{}", junit.text);
        assert!(!junit.text.contains("human usr"), "{}", junit.text);
        let pdf = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=pdf")).await;
        assert_eq!(pdf.status, 200, "{}", pdf.text);
        runbook_report_pdf::inspect::check_structure(&pdf.bytes).unwrap();
        let text = runbook_report_pdf::inspect::extract_text(&pdf.bytes).replace(['\n', '\u{c}'], " ");
        assert!(text.contains("Health endpoint returns 200"), "{text}");
        assert!(text.contains("Error rate in eu-west (regions[2].error_rate)"), "{text}");
        assert!(text.contains("Actor: Alice"), "{text}");
        // Indexed severity query.
        let found = f
            .get(&f.bob.token, &format!("/v1/projects/{}/executions?runbook=post-deploy&field.severity=%22none%22", f.project))
            .await;
        assert_eq!(found.status, 200, "{}", found.text);
        assert_eq!(found.body["items"][0]["id"], exe.as_str());
        let _ = &s.refresh;
    })
    .await;
}

// ---------------------------------------------------------------------------
// C4 bullets 3, 4 and 8: unhealthy/keep after a recovered loop.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_unhealthy_keep_after_loop_recovery() {
    with_fixture(async |f| {
        let exe = f.start(&f.bob.token, "api", "v2.0.0", "start").await;
        f.drain().await;
        let a = f.mint(&exe, "session-a").await;
        f.checks(&a, 200, [0.001, 0.002, 0.05]).await;
        // Loop iteration 1 still fails; the agent session exits on the delay.
        f.agent_step(
            &a,
            "stabilize#1.recheck",
            Some("fail"),
            json!({"rate": 0.02}),
            json!([]),
        )
        .await;
        f.drain().await;
        assert_eq!(f.next(&a.access, &exe).await["next_action"], "wait");
        f.advance(6 * MINUTE).await;
        f.drain().await;
        let work = f.work_available(&exe);
        assert_eq!(
            work.last().unwrap(),
            &("timer".to_owned(), vec!["stabilize#2.recheck".to_owned()])
        );
        let b = f.mint(&exe, "session-b").await;
        f.agent_step(
            &b,
            "stabilize#2.recheck",
            Some("pass"),
            json!({"rate": 0.005}),
            json!([]),
        )
        .await;
        f.drain().await;
        // Recovered, but the worst finding stays `fail` (PRD §3).
        let stabilize = f.run(&exe).await["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "stabilize")
            .unwrap()
            .clone();
        assert_eq!(
            (
                stabilize["satisfied"].clone(),
                stabilize["exhausted"].clone(),
                stabilize["verdict"].clone()
            ),
            (json!(true), json!(false), json!("fail"))
        );
        // The approval goes to the on-call team's members.
        let signoff = f.sr(&exe, "signoff").await;
        let requested = f.notifier.events("approval.requested", &exe);
        assert_eq!(requested.len(), 1);
        assert_eq!(requested[0]["data"]["step_run_id"], signoff.as_str());
        let mut recipients: Vec<_> = requested[0]["data"]["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["type"].as_str().unwrap().to_owned(),
                    r["id"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        recipients.sort();
        let mut expected = vec![
            ("user".to_owned(), f.alice.id.to_string()),
            ("user".to_owned(), f.bob.id.to_string()),
        ];
        expected.sort();
        assert_eq!(recipients, expected);
        for (who, listed) in [(&f.alice.token, 1), (&f.bob.token, 1), (&f.carol.token, 0)] {
            let tasks = f.get(who, "/v1/me/tasks?kind=approval").await;
            assert_eq!(
                tasks.body["items"].as_array().unwrap().len(),
                listed,
                "{}",
                tasks.text
            );
        }
        // Membership decides: Carol is not an approver before escalation.
        let carol = f
            .decide(&f.carol.token, &signoff, "rollback", "carol")
            .await;
        assert_eq!(
            (carol.status, carol.body["code"].as_str()),
            (403, Some("approval.not_eligible"))
        );
        let bob = f.decide(&f.bob.token, &signoff, "keep", "bob").await;
        assert_eq!(bob.status, 200, "{}", bob.text);
        assert_eq!(bob.body["by"], "human");
        assert_eq!(bob.body["user_id"], f.bob.id.as_str());
        f.drain().await;
        let decided = f.notifier.events("approval.decided", &exe);
        assert_eq!(decided[0]["data"]["decided_by"], f.bob.id.as_str());
        assert_eq!(decided[0]["data"]["automatic"], false);
        f.notify(&b).await;
        assert_eq!(
            status(&f.run(&exe).await),
            ("succeeded", &json!("kept"), &json!("major"))
        );
        // The rollback was skipped; the brief records Bob's signature.
        assert_eq!(f.section(&exe, "rollback").await["status"], "skipped");
        let brief = f.next(&f.alice.token, &exe).await;
        assert_eq!(brief["decisions"][0]["user_id"], f.bob.id.as_str());
        assert_eq!(brief["decisions"][0]["option"], "keep");
        // Indexed severity query finds the run under `major` only.
        let query = |value: &str| {
            format!(
                "/v1/projects/{}/executions?runbook=post-deploy&field.severity=%22{value}%22",
                f.project
            )
        };
        let major = f.get(&f.alice.token, &query("major")).await;
        assert_eq!(
            major.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["id"].clone())
                .collect::<Vec<_>>(),
            vec![json!(exe)]
        );
        assert_eq!(
            f.get(&f.alice.token, &query("none")).await.body["items"],
            json!([])
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// C4 bullets 3, 4 and 5: loop exhaustion, restart, fresh session rollback.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_loop_exhaustion_restart_and_fresh_session_rollback() {
    with_fixture(async |f| {
        let exe = f.start(&f.alice.token, "web", "v3.1.0", "start").await;
        f.drain().await;
        let a = f.mint(&exe, "session-a").await;
        f.checks(&a, 200, [0.001, 0.03, 0.002]).await;
        for n in 1..=3 {
            if n > 1 {
                f.advance(6 * MINUTE).await;
                f.drain().await;
            }
            f.agent_step(
                &a,
                &format!("stabilize#{n}.recheck"),
                Some("fail"),
                json!({"rate": 0.04}),
                json!([]),
            )
            .await;
            f.drain().await;
        }
        let stabilize = f.run(&exe).await["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "stabilize")
            .unwrap()
            .clone();
        assert_eq!(
            (
                stabilize["satisfied"].clone(),
                stabilize["exhausted"].clone(),
                stabilize["verdict"].clone()
            ),
            (json!(false), json!(true), json!("fail"))
        );
        // Session A exits on the approval wait.
        let waiting = f.next(&a.access, &exe).await;
        assert_eq!(waiting["next_action"], "wait");
        assert!(waiting["ready_work"].as_array().unwrap().is_empty());
        let signoff = f.sr(&exe, "signoff").await;
        // Server restart: new HTTP worker and drain on the same database.
        f.restart();
        let decided = f
            .decide(&f.alice.token, &signoff, "rollback", "alice")
            .await;
        assert_eq!(decided.status, 200, "{}", decided.text);
        f.drain().await;
        assert_eq!(
            f.work_available(&exe).last().unwrap(),
            &("approval_decided".to_owned(), vec!["rollback".to_owned()])
        );
        // Session B: a new pair, distinct from session A's.
        let b = f.mint(&exe, "session-b").await;
        assert_ne!(a.family, b.family);
        assert_ne!(a.access, b.access);
        let brief = f.get(&b.access, &format!("/v1/executions/{exe}")).await;
        assert_eq!(brief.status, 200, "{}", brief.text);
        assert_eq!(brief.body["next_action"], "claim_ready_work");
        assert_eq!(brief.body["ready_work"][0]["path"], "rollback");
        assert_eq!(brief.body["decisions"][0]["user_id"], f.alice.id.as_str());
        assert_eq!(brief.body["decisions"][0]["option"], "rollback");
        assert!(
            brief.body["completed_steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["path"] == "stabilize#3.recheck")
        );
        assert!(
            !brief.text.contains("Post the execution report link"),
            "future instructions stay withheld"
        );
        let read = f
            .agent_step(&b, "rollback", None, json!({}), json!([]))
            .await;
        assert!(
            read["skill"]["body"]
                .as_str()
                .unwrap()
                .contains("Roll the service back"),
            "{read}"
        );
        f.drain().await;
        // The finally notification is freshly dispatched to the relay.
        assert_eq!(
            f.work_available(&exe).last().unwrap(),
            &("cleanup".to_owned(), vec!["notify".to_owned()])
        );
        f.notify(&b).await;
        assert_eq!(
            status(&f.run(&exe).await),
            ("succeeded", &json!("rolled_back"), &json!("major"))
        );
        assert_eq!(
            f.section(&exe, "rollback").await["actor"]["session_id"],
            "session-b"
        );
        assert_eq!(
            f.relay.events("execution.completed", &exe)[0]["data"]["result"],
            "rolled_back"
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// C4 bullets 3 and 4: escalation to Carol, expiry, late decision.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_escalation_to_carol_expiry_and_late_decision() {
    with_fixture(async |f| {
        let exe = f.start(&f.alice.token, "worker", "v4.0.0", "start").await;
        f.drain().await;
        let a = f.mint(&exe, "session-a").await;
        f.checks(&a, 503, [0.001, 0.002, 0.003]).await;
        let signoff = f.sr(&exe, "signoff").await;
        let leads = f.team("eng-leads").await;
        // 30 minutes without a decision: escalation to team:eng-leads.
        f.advance(31 * MINUTE).await;
        f.drain().await;
        // Like the request, the escalation names the team's active users;
        // the team itself stays in `targets`.
        let escalated = f.notifier.events("approval.escalated", &exe);
        assert_eq!(escalated.len(), 1);
        assert_eq!(
            escalated[0]["data"]["recipients"],
            json!([{"type": "user", "id": f.carol.id}])
        );
        assert_eq!(
            escalated[0]["data"]["targets"],
            json!([{"type": "team", "id": leads}])
        );
        let members = f
            .get(
                &f.owner,
                &format!("/v1/orgs/{}/teams/{leads}/members", f.org),
            )
            .await;
        assert_eq!(members.body["items"], json!([{"user_id": f.carol.id}]));
        let inbox = f.get(&f.carol.token, "/v1/me/tasks").await;
        assert_eq!(
            inbox.body["items"][0]["step_run_id"],
            signoff.as_str(),
            "{}",
            inbox.text
        );
        assert_eq!(inbox.body["items"][0]["title"], "Roll back or keep?");
        // The prompt renders the run's inputs in the document.
        assert_eq!(
            f.section(&exe, "signoff").await["approval"]["prompt"],
            "Checks failed for v4.0.0. Choose an action."
        );
        // Another 30 minutes: the gate expires and pauses the run (default
        // on_expired: pause), from the drain alone.
        f.advance(30 * MINUTE).await;
        f.drain().await;
        let expired = f.notifier.events("approval.expired", &exe);
        assert_eq!(expired.len(), 1);
        assert_eq!(
            expired[0]["data"]["recipients"],
            json!([{"type": "user", "id": f.carol.id}])
        );
        assert_eq!(
            expired[0]["data"]["targets"],
            json!([{"type": "team", "id": leads}])
        );
        assert_eq!(f.run(&exe).await["status"], "paused");
        assert_eq!(f.section(&exe, "signoff").await["status"], "expired");
        // PRD §6: a still-eligible human may decide late while the run is
        // open; it resolves the gate and lifts the expiry pause.
        let late = f.decide(&f.carol.token, &signoff, "keep", "carol").await;
        assert_eq!(late.status, 200, "{}", late.text);
        assert_eq!(late.body["user_id"], f.carol.id.as_str());
        f.drain().await;
        assert_eq!(f.run(&exe).await["status"], "running");
        let b = f.mint(&exe, "session-b").await;
        f.notify(&b).await;
        assert_eq!(
            status(&f.run(&exe).await),
            ("succeeded", &json!("kept"), &json!("major"))
        );
        assert_eq!(
            f.section(&exe, "signoff").await["outputs"]["decided_by"],
            f.carol.id.as_str()
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// C4 bullet 3: execution deadline, rejected late decision, cleanup bounds.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_deadline_rejects_late_decision_and_bounds_cleanup() {
    with_fixture(async |f| {
        let api = f.start(&f.alice.token, "api", "v5.0.0", "api").await;
        let web = f.start(&f.alice.token, "web", "v5.0.0", "web").await;
        f.drain().await;
        for exe in [&api, &web] {
            let s = f.mint(exe, &format!("checks-{exe}")).await;
            f.checks(&s, 500, [0.001, 0.002, 0.003]).await;
        }
        let signoff = f.sr(&api, "signoff").await;
        // Two hours pass with nobody deciding: escalation, expiry, then the
        // execution deadline. Cleanup (the finally step) starts.
        f.advance(121 * MINUTE).await;
        f.drain().await;
        for exe in [&api, &web] {
            let run = f.run(exe).await;
            assert_eq!(run["phase"], "cleanup", "{run}");
            assert_eq!(f.notifier.events("approval.expired", exe).len(), 1);
            assert_eq!(
                f.work_available(exe).last().unwrap(),
                &("cleanup".to_owned(), vec!["notify".to_owned()])
            );
        }
        // A late decision at or after the deadline is rejected.
        for who in [&f.carol.token, &f.alice.token] {
            let late = f
                .decide(who, &signoff, "rollback", &format!("late-{who}"))
                .await;
            assert_eq!(
                (late.status, late.body["code"].as_str()),
                (409, Some("execution.deadline_exceeded")),
                "{}",
                late.text
            );
        }
        // api: an agent posts the report inside cleanup_timeout.
        let s = f.mint(&api, "cleanup").await;
        f.notify(&s).await;
        assert_eq!(
            status(&f.run(&api).await),
            ("failed", &json!("undetermined"), &json!("none"))
        );
        assert_eq!(f.section(&api, "notify").await["status"], "succeeded");
        // web: nobody does; cleanup_timeout (30m) ends the run anyway.
        f.advance(31 * MINUTE).await;
        f.drain().await;
        assert_eq!(
            status(&f.run(&web).await),
            ("failed", &json!("undetermined"), &json!("none"))
        );
        assert_eq!(f.section(&web, "notify").await["status"], "cancelled");
        for exe in [&api, &web] {
            assert_eq!(
                f.relay.events("execution.completed", exe)[0]["data"]["status"],
                "failed"
            );
        }
    })
    .await;
}

// ---------------------------------------------------------------------------
// C4 bullets 3, 4 and 6: unavailable agent, redispatch, handoff to on-call.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_unavailable_agent_is_redispatched_then_handed_off() {
    with_fixture(async |f| {
        let exe = f.start(&f.alice.token, "api", "v6.0.0", "start").await;
        f.drain().await;
        f.alice_login_flow(&exe).await;
        let a = f.mint(&exe, "session-a").await;
        let health = f.ready(&a.access, &exe).await["api_health"].clone();
        let stale = f.claim(&a.access, &health, agent("session-a")).await;
        // Session A disappears; its ten-minute lease expires and the step
        // is redispatched.
        f.advance(11 * MINUTE).await;
        f.drain().await;
        assert_eq!(
            f.work_available(&exe).last().unwrap(),
            &("lease_expired".to_owned(), vec!["api_health".to_owned()])
        );
        let refused = f
            .report(
                &a.access,
                &health,
                report(
                    &health,
                    &stale,
                    "succeeded",
                    Some("pass"),
                    json!({"status_code": 200}),
                    json!([]),
                ),
                "stale",
            )
            .await;
        assert_eq!(
            (refused.status, refused.body["code"].as_str()),
            (409, Some("claim.stale_generation"))
        );
        // Session B cannot perform the check either: failure hands it off.
        let b = f.mint(&exe, "session-b").await;
        let claim = f.claim(&b.access, &health, agent("session-b")).await;
        assert_eq!(claim["generation"], 2);
        let failed = f
            .report(
                &b.access,
                &health,
                report(&health, &claim, "failed", None, json!({}), json!([])),
                "failed",
            )
            .await;
        assert_eq!(failed.status, 200, "{}", failed.text);
        f.drain().await;
        let oncall = f.team("oncall").await;
        let blocked = f.notifier.events("step.blocked", &exe);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0]["data"]["step_run_id"], health.as_str());
        assert_eq!(blocked[0]["data"]["reason"], "failed");
        assert_eq!(
            blocked[0]["data"]["recipients"],
            json!([{"type": "team", "id": oncall}])
        );
        // The continuation is human work for on-call only.
        assert!(!f.ready(&b.access, &exe).await.contains_key("api_health"));
        let bob_brief = f.next(&f.bob.token, &exe).await;
        assert!(
            bob_brief["ready_work"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["path"] == "api_health" && w["actor_kind"] == "human"),
            "{bob_brief}"
        );
        let tasks = f.get(&f.bob.token, "/v1/me/tasks").await;
        assert!(
            tasks.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["step_run_id"] == health.as_str() && t["kind"] == "human_task"),
            "{}",
            tasks.text
        );
        let again = f.try_claim(&b.access, &health, agent("session-b2")).await;
        assert_eq!(
            (again.status, again.body["code"].as_str()),
            (422, Some("claim.actor_mismatch"))
        );
        let carol = f
            .try_claim(&f.carol.token, &health, json!({"session_id": "carol-web"}))
            .await;
        assert_eq!(
            (carol.status, carol.body["code"].as_str()),
            (403, Some("auth.forbidden"))
        );
        let bob_claim = f
            .claim(&f.bob.token, &health, json!({"session_id": "bob-web"}))
            .await;
        let done = f
            .report(
                &f.bob.token,
                &health,
                report(
                    &health,
                    &bob_claim,
                    "succeeded",
                    Some("pass"),
                    json!({"status_code": 200}),
                    json!([]),
                ),
                "bob",
            )
            .await;
        assert_eq!(done.status, 200, "{}", done.text);
        f.drain().await;
        let section = f.section(&exe, "api_health").await;
        assert_eq!(section["status"], "succeeded");
        assert_eq!(
            section["actor"],
            json!({"kind": "human", "user_id": f.bob.id})
        );
        assert_eq!(f.section(&exe, "smoke").await["verdict"], "pass");
        // The run carries on to the region checks for agents.
        assert_eq!(f.ready(&b.access, &exe).await.len(), 3);
    })
    .await;
}

// ---------------------------------------------------------------------------
// C4 bullet 6: lost refresh response, family revocation, claim recovery.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_lost_refresh_response_revokes_family_and_recovers_the_claim() {
    with_fixture(async |f| {
        let exe = f.start(&f.alice.token, "web", "v7.0.0", "start").await;
        f.drain().await;
        let a = f.mint(&exe, "session-a").await;
        let health = f.ready(&a.access, &exe).await["api_health"].clone();
        let held = f.claim(&a.access, &health, agent("session-a")).await;
        // The refresh succeeds but its response is lost; the client retries.
        let path = format!("/v1/executions/{exe}/tokens/refresh");
        let body = json!({"refresh_token": a.refresh});
        let lost = f
            .call("", "POST", &path, body.clone(), Some("refresh-1"))
            .await;
        assert_eq!(lost.status, 201, "{}", lost.text);
        let retry = f.call("", "POST", &path, body, Some("refresh-1")).await;
        assert_eq!(
            (retry.status, retry.body["code"].as_str()),
            (409, Some("credentials.refresh_reused"))
        );
        // The whole family is revoked: the lost pair and the original.
        let brief = format!("/v1/executions/{exe}");
        assert_eq!(
            f.get(lost.body["access_token"].as_str().unwrap(), &brief)
                .await
                .status,
            401
        );
        assert_eq!(f.get(&a.access, &brief).await.status, 401);
        let refused = f
            .report(
                &a.access,
                &health,
                report(
                    &health,
                    &held,
                    "succeeded",
                    Some("pass"),
                    json!({"status_code": 200}),
                    json!([]),
                ),
                "revoked",
            )
            .await;
        assert_eq!(refused.status, 401);
        // Recovery: the launcher mints a new family. The step stays claimed
        // by the lost session until its lease expires, then is redispatched.
        let r = f.mint(&exe, "session-r").await;
        assert_ne!(r.family, a.family);
        let busy = f.try_claim(&r.access, &health, agent("session-r")).await;
        assert_eq!(
            (busy.status, busy.body["code"].as_str()),
            (409, Some("step.not_claimable"))
        );
        f.advance(11 * MINUTE).await;
        f.drain().await;
        assert_eq!(
            f.work_available(&exe).last().unwrap(),
            &("lease_expired".to_owned(), vec!["api_health".to_owned()])
        );
        let fresh = f.try_claim(&r.access, &health, agent("session-r2")).await;
        assert_eq!(fresh.status, 200, "{}", fresh.text);
        assert_eq!(fresh.body["generation"], 2);
        // A stale report on the old claim is never accepted.
        let stale = f
            .report(
                &r.access,
                &health,
                report(
                    &health,
                    &held,
                    "succeeded",
                    Some("pass"),
                    json!({"status_code": 200}),
                    json!([]),
                ),
                "stale",
            )
            .await;
        assert_eq!(
            (stale.status, stale.body["code"].as_str()),
            (409, Some("claim.stale_generation"))
        );
        let ok = f
            .report(
                &r.access,
                &health,
                report(
                    &health,
                    &fresh.body,
                    "succeeded",
                    Some("pass"),
                    json!({"status_code": 200}),
                    json!([]),
                ),
                "fresh",
            )
            .await;
        assert_eq!(ok.status, 200, "{}", ok.text);
        let section = f.section(&exe, "api_health").await;
        assert_eq!(section["actor"]["session_id"], "session-r2");
        assert_eq!(
            section["actor"]["principal_id"],
            format!("execution:{}", r.family)
        );
        // The audit trail (the event stream from sequence 0) never holds a
        // secret, though it records both credential families.
        let events = event_history(f, &exe).await;
        assert!(
            events.contains(&a.family) && events.contains(&r.family),
            "{events}"
        );
        for secret in [&a.access, &a.refresh, &r.access, &r.refresh] {
            assert!(!events.contains(secret.as_str()));
        }
        assert!(!events.contains(lost.body["refresh_token"].as_str().unwrap()));
    })
    .await;
}

/// Every stored event so far, read from the SSE stream until it idles.
async fn event_history(f: &Fixture, exe: &str) -> String {
    use futures_util::StreamExt;
    let request = Request::builder()
        .uri(format!("/v1/executions/{exe}/events"))
        .header("authorization", format!("Bearer {}", f.owner))
        .header("last-event-id", "0")
        .body(Body::empty())
        .unwrap();
    let response = f.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let mut frames = response.into_body().into_data_stream();
    let mut text = String::new();
    while let Ok(Some(Ok(bytes))) =
        tokio::time::timeout(Duration::from_millis(500), frames.next()).await
    {
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    text
}

// ---------------------------------------------------------------------------
// C4 bullet 7: amendment and rerun through the real endpoints.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn flagship_amendment_and_rerun_preserve_outcome_and_signatures() {
    with_fixture(async |f| {
        let exe = f.start(&f.alice.token, "api", "v8.0.0", "start").await;
        f.drain().await;
        let a = f.mint(&exe, "session-a").await;
        f.checks(&a, 200, [0.001, 0.002, 0.05]).await;
        f.agent_step(&a, "stabilize#1.recheck", Some("pass"), json!({"rate": 0.004}), json!([])).await;
        f.drain().await;
        let signoff = f.sr(&exe, "signoff").await;
        assert_eq!(f.decide(&f.bob.token, &signoff, "rollback", "bob").await.status, 200);
        f.drain().await;
        f.agent_step(&a, "rollback", None, json!({}), json!([])).await;
        f.drain().await;
        f.notify(&a).await;
        let before = f.document(&exe).await;
        assert_eq!((before["header"]["status"].clone(), before["header"]["result"].clone()), (json!("succeeded"), json!("rolled_back")));
        let signature = f.section(&exe, "signoff").await;
        // D35: only a human project admin amends; approvals never.
        let region = f.sr(&exe, "regions[2].error_rate").await;
        let body = json!({"to": {"status": "succeeded", "verdict": "pass", "outputs": {"rate": 0.009}, "summary": "dashboard misread"}, "reason": "wrong dashboard panel"});
        let path = format!("/v1/step-runs/{region}/overrides");
        let denied = f.call(&f.alice.token, "POST", &path, body.clone(), Some("alice-amend")).await;
        assert_eq!(denied.status, 403, "{}", denied.text);
        let denied = f.call(&a.access, "POST", &path, body.clone(), Some("agent-amend")).await;
        assert!(denied.status == 401 || denied.status == 403, "{}", denied.text);
        let amended = f.call(&f.owner, "POST", &path, body.clone(), Some("amend")).await;
        assert_eq!(amended.status, 200, "{}", amended.text);
        assert_eq!(amended.body["from"]["outputs"]["rate"], 0.05);
        assert_eq!(amended.body["from"]["verdict"], "fail");
        assert_eq!(amended.body["to"]["outputs"]["rate"], 0.009);
        let replay = f.call(&f.owner, "POST", &path, body, Some("amend")).await;
        assert_eq!(replay.body, amended.body, "D20 replay");
        let approval = f
            .call(&f.owner, "POST", &format!("/v1/step-runs/{signoff}/overrides"),
                json!({"to": {"status": "succeeded", "verdict": null, "outputs": {"decision": "keep"}, "summary": "x"}, "reason": "rewrite"}),
                Some("amend-approval"))
            .await;
        assert!((400..500).contains(&approval.status), "{}", approval.text);
        // Terminal outcome and the original signature are preserved.
        let after = f.document(&exe).await;
        assert_eq!(after["header"]["status"], "succeeded");
        assert_eq!(after["header"]["result"], "rolled_back");
        assert_eq!(after["header"]["fields"]["severity"], "major");
        assert_eq!(after["header"]["amended"], true);
        assert_eq!(after["header"]["amendments"].as_array().unwrap().len(), 1);
        assert_eq!(after["header"]["amendments"][0]["by"], json!({"kind": "human", "user_id": issued_owner(f).await}));
        assert_eq!(f.section(&exe, "signoff").await, signature);
        let brief = f.next(&f.alice.token, &exe).await;
        assert_eq!(brief["decisions"][0]["user_id"], f.bob.id.as_str());
        assert_eq!(brief["decisions"][0]["option"], "rollback");
        assert_eq!(f.section(&exe, "regions[2].error_rate").await["outputs"]["rate"], 0.009);
        // The run view carries the same paths and amendment facts.
        let view = f.run(&exe).await;
        for key in ["amended", "amendments", "stale_step_ids"] {
            assert_eq!(view[key], after["header"][key], "{key}");
        }
        let step = view["steps"].as_array().unwrap().iter().find(|s| s["id"] == region.as_str()).unwrap();
        assert_eq!((step["path"].clone(), step["amended"].clone()), (json!("regions[2].error_rate"), json!(true)));
        let search = f.get(&f.owner, &format!("/v1/projects/{}/executions", f.project)).await;
        let item = search.body["items"].as_array().unwrap().iter().find(|i| i["id"] == exe.as_str()).unwrap();
        assert!(item["created_at_ms"].as_i64().unwrap() > 0, "{item}");
        // Rerun: a new run from the original start facts and version.
        let rerun = f.call(&f.alice.token, "POST", &format!("/v1/executions/{exe}/rerun"), json!({}), Some("rerun")).await;
        assert_eq!(rerun.status, 201, "{}", rerun.text);
        let second = rerun.body["id"].as_str().unwrap().to_owned();
        assert_ne!(second, exe);
        assert_eq!(rerun.body["rerun_of"], exe.as_str());
        assert_eq!(rerun.body["inputs"], before["header"]["inputs"]);
        assert_eq!(rerun.body["runbook_version_id"], before["header"]["runbook_version_id"]);
        f.drain().await;
        assert_eq!(f.run(&second).await["status"], "running");
        assert_eq!(f.work_available(&second), vec![("initial".into(), vec!["api_health".into()])]);
        let original = f.document(&exe).await;
        assert_eq!((original["header"]["status"].clone(), original["header"]["amended"].clone()), (json!("succeeded"), json!(true)));
    })
    .await;
}
/// The bootstrap owner's user ID, read through the directory routes.
async fn issued_owner(f: &Fixture) -> String {
    let users = f.get(&f.owner, &format!("/v1/orgs/{}/users", f.org)).await;
    users.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["role"] == "owner")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

// ---------------------------------------------------------------------------
// Companion fixture: the PRD §3 deliberate auto-rollback variant, plus an
// explicit report template (the flagship itself declares neither).
// ---------------------------------------------------------------------------
fn companion() -> String {
    let swap = |s: String, a: &str, b: &str| {
        assert_eq!(s.matches(a).count(), 1, "{a}");
        s.replace(a, b)
    };
    let s = swap(
        FLAGSHIP.to_owned(),
        "  id: post-deploy\n",
        "  id: post-deploy-auto\n",
    );
    let s = swap(
        s,
        "                  options: [rollback, keep]\n",
        "                  options:\n                    - { id: rollback, title: Roll back, auto: true }\n                    - { id: keep, title: Keep }\n",
    );
    let s = swap(
        s,
        "                  on_timeout: escalate\n                  escalate_to: [team:eng-leads]\n                  escalation_timeout: 30m\n",
        "                  on_timeout: { decide: rollback }\n",
    );
    let s = swap(
        s,
        "                destructive: true\n",
        "                destructive: true\n                allow_auto_decision: true\n",
    );
    swap(
        s,
        "  finally:\n",
        "  report: \"Deploy ${{ inputs.service }} ${{ inputs.version }} finished as ${{ outputs.result }}.\"\n  finally:\n",
    )
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn companion_auto_rollback_with_custom_report() {
    with_fixture(async |f| {
        f.publish(&companion(), "companion").await;
        let started = f
            .call(&f.alice.token, "POST", &format!("/v1/projects/{}/executions", f.project),
                json!({"runbook": "post-deploy-auto", "inputs": {"service": "worker", "version": "v9"}}), Some("start"))
            .await;
        assert_eq!(started.status, 201, "{}", started.text);
        let exe = started.body["id"].as_str().unwrap().to_owned();
        f.drain().await;
        let a = f.mint(&exe, "session-a").await;
        f.checks(&a, 500, [0.001, 0.002, 0.003]).await;
        let signoff = f.sr(&exe, "signoff").await;
        // Before its timeout, no decision; after it, the system policy
        // decides rollback, visibly (expiry, then an automatic decision).
        let live = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=markdown")).await;
        assert!(!live.text.contains("## Report"), "custom report renders only once terminal");
        let live = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=junit")).await;
        assert!(!live.text.contains("finished as"), "D34: {}", live.text);
        f.advance(31 * MINUTE).await;
        f.drain().await;
        assert_eq!(f.notifier.events("approval.expired", &exe).len(), 1);
        assert_eq!(f.work_available(&exe).last().unwrap(), &("auto_decision".to_owned(), vec!["rollback".to_owned()]));
        let section = f.section(&exe, "signoff").await;
        assert_eq!(section["outputs"]["decision"], "rollback");
        assert_eq!(section["step_run_id"], signoff.as_str());
        let b = f.mint(&exe, "session-b").await;
        f.agent_step(&b, "rollback", None, json!({}), json!([])).await;
        f.drain().await;
        f.notify(&b).await;
        assert_eq!(status(&f.run(&exe).await), ("succeeded", &json!("rolled_back"), &json!("major")));
        let markdown = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=markdown")).await;
        assert_eq!(markdown.status, 200);
        assert!(markdown.text.contains("## Report\n\nDeploy worker v9 finished as rolled"), "{}", markdown.text);
        // REP2: the custom report reaches the JUnit and PDF exports too, once terminal.
        let junit = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=junit")).await;
        assert!(junit.text.contains("<system-out>Deploy worker v9 finished as rolled"), "{}", junit.text);
        let pdf = f.get(&f.owner, &format!("/v1/executions/{exe}/report?format=pdf")).await;
        let text = runbook_report_pdf::inspect::extract_text(&pdf.bytes).replace('\n', " ");
        assert!(text.contains("Report Deploy worker v9 finished as rolled"), "{text}");
    })
    .await;
}
