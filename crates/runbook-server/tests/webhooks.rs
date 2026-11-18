//! C1 end to end on PostgreSQL with a local mock HTTP receiver: routes
//! through the real HTTP dispatch, the drain's fan-out and delivery, HMAC
//! signatures on the wire, approval routing to a resumed agent with the
//! runbook's params, crash-after-acceptance redelivery with the same event
//! ID, undelivered human events on the execution, and SSRF/DNS-rebinding
//! rejection when private targets are not allowed.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/examples.rs"]
mod examples;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::FutureExt;
use runbook_core::{
    catalog::TenantScope,
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, TeamRecord},
    webhooks::{SIGNATURE_TOLERANCE_S, WebhookStore, verify_signature},
};
use runbook_model::*;
use runbook_server::{
    config::{ServerConfig, WebhookConfig},
    drain::{DeliveryWorker, DrainSettings, DrainStats, DrainWorker, Drainer},
    http::{Worker, router},
    webhooks::Resolve,
};
use runbook_store_postgres::{PostgresDirectory, PostgresWebhooks};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    panic::AssertUnwindSafe,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

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
/// A real HTTP/1.1 listener on loopback that records requests and answers
/// with scripted statuses (200 once the script is empty).
#[derive(Clone)]
struct Receiver {
    port: u16,
    seen: Arc<Mutex<Vec<Received>>>,
    statuses: Arc<Mutex<VecDeque<u16>>>,
    /// Sent as `Location` with 3xx statuses.
    location: Arc<Mutex<String>>,
    /// Hold each response past the delivery timeout.
    hang: Arc<std::sync::atomic::AtomicBool>,
}
impl Receiver {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let receiver = Self {
            port: listener.local_addr().unwrap().port(),
            seen: Arc::default(),
            statuses: Arc::default(),
            location: Arc::default(),
            hang: Arc::default(),
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
                if shared.hang.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_secs(12));
                }
                let status = shared.statuses.lock().unwrap().pop_front().unwrap_or(200);
                let location = if (300..400).contains(&status) {
                    format!("Location: {}\r\n", shared.location.lock().unwrap())
                } else {
                    String::new()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} Scripted\r\n{location}Content-Length: 2\r\nConnection: close\r\n\r\nok"
                    )
                    .as_bytes(),
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
}

struct Fixture {
    config: ServerConfig,
    _worker: Worker,
    app: Router,
    token: String,
    org: OrganizationId,
    user: UserId,
    project: ProjectId,
}
struct Reply {
    status: u16,
    body: Value,
}
impl Fixture {
    async fn new(db: &database::TestDatabase, webhooks: WebhookConfig) -> Self {
        let mut config = ServerConfig::new(
            db.config().clone(),
            "https://runbooks.example",
            "127.0.0.1:0",
        )
        .unwrap();
        config.webhooks = webhooks;
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
            _worker: worker,
            app,
            token: issued["pat"].as_str().unwrap().into(),
            org: issued["org_id"].as_str().unwrap().parse().unwrap(),
            user: issued["user_id"].as_str().unwrap().parse().unwrap(),
            project: issued["project_id"].as_str().unwrap().parse().unwrap(),
        }
    }
    fn scope(&self) -> TenantScope {
        TenantScope {
            org_id: self.org.clone(),
            project_id: self.project.clone(),
        }
    }
    async fn call(&self, method: &str, path: &str, body: Value, key: Option<&str>) -> Reply {
        self.call_with(method, path, body, key, None).await
    }
    async fn call_with(
        &self,
        method: &str,
        path: &str,
        body: Value,
        key: Option<&str>,
        etag: Option<&str>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {}", self.token));
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
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .map(|v| v.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
        let mut body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        if let Some(etag) = etag {
            body["__etag"] = json!(etag);
        }
        Reply { status, body }
    }
    async fn oncall(&self) {
        let directory = PostgresDirectory::new(self.config.database.clone());
        let revision = directory.snapshot(&self.org).await.unwrap().revision;
        let team: TeamId = "team_00000000000000000000000077".parse().unwrap();
        directory
            .commit(DirectoryCommit {
                org_id: self.org.clone(),
                expected_revision: revision,
                mutations: vec![
                    DirectoryMutation::CreateTeam(TeamRecord {
                        id: team.clone(),
                        org_id: self.org.clone(),
                        slug: "oncall".into(),
                        title: "Oncall".into(),
                    }),
                    DirectoryMutation::AddTeamMember {
                        team_id: team,
                        user_id: self.user.clone(),
                    },
                ],
            })
            .await
            .unwrap();
    }
    /// Create, publish and start one run; returns its ID.
    async fn run(&self, source: &str) -> String {
        let created = self
            .call(
                "POST",
                &format!("/v1/projects/{}/runbooks", self.project),
                json!({"source": source}),
                Some("create-rb"),
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = self
            .call(
                "GET",
                &format!("/v1/runbooks/{rb}/draft"),
                Value::Null,
                None,
            )
            .await;
        let published = self
            .call_with(
                "POST",
                &format!("/v1/runbooks/{rb}/versions"),
                json!({}),
                Some("publish"),
                draft.body["__etag"].as_str(),
            )
            .await;
        assert_eq!(published.status, 201, "{}", published.body);
        let started = self
            .call(
                "POST",
                &format!("/v1/projects/{}/executions", self.project),
                json!({"runbook": rb, "tags": ["prod"]}),
                Some("start"),
            )
            .await;
        assert_eq!(started.status, 201, "{}", started.body);
        started.body["id"].as_str().unwrap().to_owned()
    }
    async fn subscribe(&self, url: &str, events: Value, key: &str) -> Reply {
        self.call(
            "POST",
            &format!("/v1/projects/{}/webhooks", self.project),
            json!({"name": key, "url": url, "events": events}),
            Some(key),
        )
        .await
    }
}

fn settings() -> DrainSettings {
    DrainSettings {
        batch: 32,
        lease_ms: 30_000,
        idle: Duration::from_millis(10),
    }
}
/// Timers and fan-out, then deliveries, until both are idle.
async fn drain(drainer: &mut Drainer) -> DrainStats {
    let mut total = DrainStats::default();
    for _ in 0..20 {
        let mut stats = drainer.drain_once().await.unwrap();
        let delivered = drainer.deliver_once().await.unwrap();
        stats.deliveries_claimed = delivered.deliveries_claimed;
        stats.delivered = delivered.delivered;
        stats.delivery_failures = delivered.delivery_failures;
        stats.delivery_errors = delivered.delivery_errors;
        total.failed += stats.failed;
        total.delivered += stats.delivered;
        total.delivery_failures += stats.delivery_failures;
        total.delivery_errors += stats.delivery_errors;
        if !stats.busy() {
            return total;
        }
    }
    panic!("drain did not quiesce");
}
fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
fn self_hosted() -> WebhookConfig {
    WebhookConfig {
        allow_private_targets: true,
        allow_link_local_targets: false,
        secret_key: Some([5; 32]),
        resolver: None,
        ..Default::default()
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
async fn with_fixture(webhooks: WebhookConfig, f: impl AsyncFnOnce(&Fixture)) {
    let _slot = FixtureSlot::acquire();
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let fixture = Fixture::new(&db, webhooks).await;
        f(&fixture).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

const GATED: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: flagship, title: Flagship}\n\
    spec:\n  agents: {params: {repo: acme/api, workflow: runbook-resume.yml}}\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Roll back?\", options: [rollback, keep], approvers: {teams: [oncall]}, min: 1}\n    \
    - {id: rollback, title: Roll back, actor: agent}\n";

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn signed_events_reach_a_local_receiver_and_resume_the_agent() {
    with_fixture(self_hosted(), async |f| {
        f.oncall().await;
        let receiver = Receiver::start();
        let created = f
            .subscribe(
                &receiver.url(),
                json!([
                    "execution.started",
                    "approval.decided",
                    "agent.work_available"
                ]),
                "relay",
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let secret = created.body["secret"].as_str().unwrap().to_owned();
        let hook = created.body["webhook"]["id"].as_str().unwrap().to_owned();
        let exe = f.run(GATED).await;
        let mut drainer = Drainer::new(f.config.clone(), settings());
        let stats = drain(&mut drainer).await;
        assert_eq!(stats.failed, 0, "{stats:?}");
        let seen = receiver.seen();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].header("Runbook-Event-Type"), "execution.started");
        for r in &seen {
            assert_eq!(
                verify_signature(
                    &secret,
                    r.header("Runbook-Timestamp"),
                    r.body.as_bytes(),
                    r.header("Runbook-Signature"),
                    now_s(),
                    SIGNATURE_TOLERANCE_S
                ),
                Ok(())
            );
            assert_eq!(r.header("Runbook-Event-Id"), r.json()["id"]);
            assert!(r.header("Runbook-Delivery-Id").starts_with("whd_"));
            assert!(!r.body.contains(&secret) && !r.body.contains("whsec_"));
        }
        // approval.requested matched no subscription: the run says so.
        let read = f
            .call("GET", &format!("/v1/executions/{exe}"), Value::Null, None)
            .await;
        assert_eq!(read.status, 200);
        let marked = &read.body["notifications_not_delivered"];
        assert_eq!(
            marked[0]["event_type"], "approval.requested",
            "{}",
            read.body
        );
        assert_eq!(marked[0]["reason"], "no_subscriber");
        let report = f
            .call(
                "GET",
                &format!("/v1/executions/{exe}/report?format=markdown"),
                Value::Null,
                None,
            )
            .await;
        assert!(
            report
                .body
                .as_str()
                .unwrap()
                .contains("Notification not delivered"),
            "{}",
            report.body
        );
        // A human decision resumes the agent through the webhook.
        let gate = read.body["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "gate")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let vote = f
            .call(
                "POST",
                &format!("/v1/step-runs/{gate}/decisions"),
                json!({"option": "rollback"}),
                Some("vote"),
            )
            .await;
        assert_eq!(vote.status, 200, "{}", vote.body);
        drain(&mut drainer).await;
        let mut seen = receiver.seen();
        seen.sort_by_key(|r| r.json()["sequence"].as_u64());
        let types: Vec<&str> = seen
            .iter()
            .map(|r| r.header("Runbook-Event-Type"))
            .collect();
        assert_eq!(
            types,
            [
                "execution.started",
                "approval.decided",
                "agent.work_available"
            ]
        );
        let work = seen[2].json();
        assert_eq!(work["data"]["reason"], "approval_decided");
        assert_eq!(
            work["data"]["params"],
            json!({"repo": "acme/api", "workflow": "runbook-resume.yml"})
        );
        assert_eq!(work["execution"]["id"], exe);
        // Delivery log and a test delivery through the routes.
        let base = format!("/v1/projects/{}/webhooks/{hook}", f.project);
        let log = f
            .call("GET", &format!("{base}/deliveries"), Value::Null, None)
            .await;
        assert_eq!(log.status, 200);
        let items = log.body["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert!(
            items
                .iter()
                .all(|d| d["state"] == "delivered" && d["attempts"][0]["status"] == 200)
        );
        assert!(!log.body.to_string().contains(&secret));
        let test = f
            .call("POST", &format!("{base}/test"), Value::Null, Some("t"))
            .await;
        assert_eq!(test.status, 202, "{}", test.body);
        drain(&mut drainer).await;
        assert_eq!(
            receiver.seen().last().unwrap().header("Runbook-Event-Type"),
            "webhook.test"
        );
        // Redirects are never followed: a 302 fails the delivery outright.
        let elsewhere = Receiver::start();
        *receiver.location.lock().unwrap() = elsewhere.url();
        receiver.statuses.lock().unwrap().push_back(302);
        let test = f
            .call("POST", &format!("{base}/test"), Value::Null, Some("t2"))
            .await;
        assert_eq!(test.status, 202, "{}", test.body);
        drain(&mut drainer).await;
        assert!(elsewhere.seen().is_empty());
        let log = f
            .call(
                "GET",
                &format!("{base}/deliveries?limit=1"),
                Value::Null,
                None,
            )
            .await;
        let newest = &log.body["items"][0];
        assert_eq!(newest["state"], "failed", "{}", log.body);
        assert_eq!(newest["failure"], "http_302");
        assert!(log.body["next_cursor"].is_string());
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn crash_after_acceptance_redelivers_the_same_event_after_restart() {
    with_fixture(self_hosted(), async |f| {
        let receiver = Receiver::start();
        f.oncall().await;
        let created = f
            .subscribe(&receiver.url(), json!(["execution.started"]), "relay")
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let secret = created.body["secret"].as_str().unwrap().to_owned();
        f.run(GATED).await;
        let short = runbook_core::webhooks::DeliverySettings {
            batch: 16,
            lease_ms: 1_500,
        };
        let mut first = Drainer::new(f.config.clone(), settings()).with_delivery(short);
        first.webhooks().fail_next_completion();
        let stats = drain(&mut first).await;
        assert_eq!(stats.delivery_errors, 1, "{stats:?}");
        assert_eq!(receiver.seen().len(), 1);
        drop(first);
        // A restarted worker waits out the lease, then sends the same event.
        let mut restarted = Drainer::new(f.config.clone(), settings()).with_delivery(short);
        drain(&mut restarted).await;
        assert_eq!(receiver.seen().len(), 1);
        tokio::time::sleep(Duration::from_millis(1_700)).await;
        let stats = drain(&mut restarted).await;
        assert_eq!(stats.delivered, 1, "{stats:?}");
        let seen = receiver.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].header("Runbook-Event-Id"),
            seen[1].header("Runbook-Event-Id")
        );
        assert_eq!(
            seen[0].header("Runbook-Delivery-Id"),
            seen[1].header("Runbook-Delivery-Id")
        );
        assert_eq!(seen[0].body, seen[1].body);
        for r in &seen {
            assert!(
                verify_signature(
                    &secret,
                    r.header("Runbook-Timestamp"),
                    r.body.as_bytes(),
                    r.header("Runbook-Signature"),
                    now_s(),
                    SIGNATURE_TOLERANCE_S
                )
                .is_ok()
            );
        }
        tokio::time::sleep(Duration::from_millis(1_700)).await;
        drain(&mut restarted).await;
        assert_eq!(
            receiver.seen().len(),
            2,
            "delivered exactly once after recovery"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn private_targets_and_dns_rebinding_are_rejected_without_the_self_hosted_setting() {
    let receiver = Receiver::start();
    let port = receiver.port;
    let lookups = Arc::new(AtomicUsize::new(0));
    let counter = lookups.clone();
    // The name resolves publicly when the subscription is created and to the
    // loopback receiver afterwards.
    let resolver: Resolve = Arc::new(move |host, _| {
        assert_eq!(host, "hooks.rebind.example");
        let first = counter.fetch_add(1, Ordering::SeqCst) == 0;
        Ok(vec![if first {
            SocketAddr::from(([93, 184, 216, 34], 443))
        } else {
            SocketAddr::from(([127, 0, 0, 1], port))
        }])
    });
    let cloud = WebhookConfig {
        allow_private_targets: false,
        allow_link_local_targets: false,
        secret_key: Some([5; 32]),
        resolver: Some(resolver),
        ..Default::default()
    };
    with_fixture(cloud, async |f| {
        f.oncall().await;
        for (url, code) in [
            (receiver.url(), "webhooks.url_https_required"),
            (
                format!("https://127.0.0.1:{port}/hook"),
                "webhooks.target_forbidden",
            ),
            ("https://localhost/hook".into(), "webhooks.target_forbidden"),
            ("https://[::1]/hook".into(), "webhooks.target_forbidden"),
            (
                "https://169.254.169.254/latest".into(),
                "webhooks.target_forbidden",
            ),
        ] {
            let reply = f.subscribe(&url, json!(["*"]), &url).await;
            assert_eq!(
                (reply.status, reply.body["code"].as_str()),
                (422, Some(code)),
                "{url}"
            );
        }
        let created = f
            .subscribe("https://hooks.rebind.example/hook", json!(["*"]), "rebind")
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let hook = created.body["webhook"]["id"].as_str().unwrap().to_owned();
        f.run(GATED).await;
        let mut drainer = Drainer::new(f.config.clone(), settings());
        let stats = drain(&mut drainer).await;
        assert!(stats.delivery_failures >= 1, "{stats:?}");
        assert!(lookups.load(Ordering::SeqCst) >= 2, "rechecked at delivery");
        assert!(
            receiver.seen().is_empty(),
            "never connected to the private address"
        );
        let webhooks = PostgresWebhooks::new(f.config.database.clone());
        let log = webhooks
            .deliveries(&f.scope(), &hook.parse().unwrap(), 10, None)
            .await
            .unwrap();
        assert!(!log.is_empty());
        for (delivery, attempts) in &log {
            assert_eq!(delivery.state, DeliveryState::Failed);
            assert_eq!(
                delivery.failure.as_deref(),
                Some("webhooks.target_forbidden")
            );
            assert_eq!(attempts.len(), 1);
        }
    })
    .await;
}

/// I2: a receiver that hangs past the delivery timeout must not delay
/// timers: deliveries run on their own worker, separate from the drain.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn timers_fire_on_time_while_a_receiver_hangs() {
    with_fixture(self_hosted(), async |f| {
        let receiver = Receiver::start();
        receiver.hang.store(true, Ordering::SeqCst);
        let created = f
            .subscribe(&receiver.url(), json!(["execution.started"]), "slow")
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let exe = f
            .run(
                "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: timed, title: Timed}\n\
                 spec:\n  execution: {timeout: 2s}\n  steps: [{id: only, title: Only}]\n",
            )
            .await;
        let fast = DrainSettings {
            batch: 16,
            lease_ms: 30_000,
            idle: Duration::from_millis(20),
        };
        let drain = DrainWorker::start(f.config.clone(), fast).unwrap();
        let deliveries = DeliveryWorker::start(f.config.clone(), fast).unwrap();
        // The execution deadline (2 s) fires while the receiver still holds
        // the started event's delivery (10 s timeout).
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        let terminal = loop {
            let read = f.call("GET", &format!("/v1/executions/{exe}"), Value::Null, None).await;
            let status = read.body["status"].as_str().unwrap_or("").to_owned();
            if matches!(status.as_str(), "failed" | "cancelled" | "succeeded") {
                break true;
            }
            if std::time::Instant::now() > deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(receiver.seen().len(), 1, "the hanging delivery was in flight");
        assert!(terminal, "the execution deadline fired on time");
        drop(drain);
        drop(deliveries);
    })
    .await;
}

const POOLED: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: pooled, title: Pooled}\n\
    spec:\n  execution: {timeout: 2h, concurrency: {key: \"${{ inputs.service }}\", limit: 1, on_conflict: queue}}\n  \
    inputs: {service: {type: string, default: api}}\n  \
    steps: [{id: only, title: Only}]\n";

/// C1 follow-ups end to end: `runbook.published` from the publish route and
/// `execution.queued` from a start queued behind its concurrency key both
/// reach a subscribed receiver, signed.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn queued_and_published_events_reach_the_receiver_signed() {
    with_fixture(self_hosted(), async |f| {
        let receiver = Receiver::start();
        let created = f
            .subscribe(
                &receiver.url(),
                json!(["execution.queued", "runbook.published"]),
                "ops",
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let secret = created.body["secret"].as_str().unwrap().to_owned();
        let first = f.run(POOLED).await;
        let second = f
            .call(
                "POST",
                &format!("/v1/projects/{}/executions", f.project),
                json!({"runbook": "pooled"}),
                Some("start-2"),
            )
            .await;
        assert_eq!(second.status, 201, "{}", second.body);
        let second = second.body["id"].as_str().unwrap().to_owned();
        let mut drainer = Drainer::new(f.config.clone(), settings());
        let stats = drain(&mut drainer).await;
        assert_eq!(stats.failed, 0, "{stats:?}");
        let seen = receiver.seen();
        let mut types: Vec<&str> = seen
            .iter()
            .map(|r| r.header("Runbook-Event-Type"))
            .collect();
        types.sort();
        assert_eq!(types, ["execution.queued", "runbook.published"], "{seen:?}");
        for r in &seen {
            assert_eq!(
                verify_signature(
                    &secret,
                    r.header("Runbook-Timestamp"),
                    r.body.as_bytes(),
                    r.header("Runbook-Signature"),
                    now_s(),
                    SIGNATURE_TOLERANCE_S
                ),
                Ok(())
            );
            assert_eq!(r.header("Runbook-Event-Id"), r.json()["id"]);
            assert!(!r.body.contains("apiVersion") && !r.body.contains("whsec_"));
        }
        let by_type = |t: &str| {
            seen.iter()
                .find(|r| r.header("Runbook-Event-Type") == t)
                .unwrap()
                .json()
        };
        let published = by_type("runbook.published");
        assert_eq!(published["execution"], Value::Null);
        assert_eq!(published["project"], f.project.as_str());
        assert_eq!(published["data"]["runbook_slug"], "pooled");
        assert_eq!(published["data"]["runbook_version"], 1);
        assert!(
            published["data"]["runbook_version_id"]
                .as_str()
                .unwrap()
                .starts_with("rbv_")
        );
        let queued = by_type("execution.queued");
        assert_eq!(queued["execution"]["id"], second.as_str());
        assert_ne!(queued["execution"]["id"], first.as_str());
        assert_eq!(queued["data"]["status"], "queued");
        assert_eq!(queued["data"]["concurrency_key"], "api");
        // Fanned out once: another pass sends nothing more.
        drain(&mut drainer).await;
        assert_eq!(receiver.seen().len(), 2);
    })
    .await;
}

/// C1 retention: the delivery worker purges delivery-log rows older than
/// 90 days (store clock), in bounded batches, and keeps recent ones.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn the_delivery_worker_purges_the_log_after_ninety_days() {
    use runbook_core::webhooks::{
        DELIVERY_LOG_RETENTION_MS, TransportResult, complete, new_delivery, test_event,
    };
    with_fixture(self_hosted(), async |f| {
        let receiver = Receiver::start();
        let created = f
            .subscribe(&receiver.url(), json!(["execution.completed"]), "log")
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let hook: WebhookId = created.body["webhook"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let webhooks = PostgresWebhooks::new(f.config.database.clone());
        let scope = f.scope();
        let now = webhooks.webhook_time_ms().await.unwrap();
        let old = now - DELIVERY_LOG_RETENTION_MS - 60_000;
        // A delivery finished 90 days ago (clock injected through its rows).
        let subscription = webhooks
            .get_subscription(&scope, &hook)
            .await
            .unwrap()
            .unwrap();
        let event = test_event(&scope, &subscription, "evt_ancient".into(), old).unwrap();
        let ancient: DeliveryId = "whd_00000000000000000000000001".parse().unwrap();
        let delivery = new_delivery(ancient.clone(), &subscription, &event, None, old);
        webhooks
            .record_event(&scope, event, vec![delivery], None)
            .await
            .unwrap();
        let lease = webhooks
            .claim_deliveries(&scope, 1, 60_000)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let ok = TransportResult::Response {
            status: 200,
            latency_ms: 3,
            snippet: "ok".into(),
        };
        webhooks
            .complete_attempt(
                &scope,
                &ancient,
                lease.generation,
                complete(&lease.delivery, &ok, old),
            )
            .await
            .unwrap();
        // And a recent one, through the test route and the worker.
        let base = format!("/v1/projects/{}/webhooks/{hook}", f.project);
        let test = f
            .call("POST", &format!("{base}/test"), Value::Null, Some("t"))
            .await;
        assert_eq!(test.status, 202, "{}", test.body);
        let mut drainer = Drainer::new(f.config.clone(), settings());
        let stats = drainer.deliver_once().await.unwrap();
        assert_eq!(stats.delivered, 1, "{stats:?}");
        assert_eq!(
            stats.purged, 3,
            "the old delivery, its attempt and its event"
        );
        let log = webhooks.deliveries(&scope, &hook, 10, None).await.unwrap();
        assert_eq!(log.len(), 1);
        assert_ne!(log[0].0.id, ancient);
        assert_eq!(log[0].1.len(), 1);
        assert_eq!(drainer.deliver_once().await.unwrap().purged, 0);
        assert_eq!(receiver.seen().len(), 1);
    })
    .await;
}

/// C1 recipe end to end: the server's delivery worker sends the flagship's
/// `agent.work_available` (after the approval) to the example GitHub
/// Actions relay, which verifies it and calls the mock GitHub API's
/// `workflow_dispatch` with the execution and event IDs.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn the_example_relay_dispatches_the_resume_workflow() {
    if !examples::python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    with_fixture(self_hosted(), async |f| {
        f.oncall().await;
        let github = examples::MockHttp::start(204);
        // Subscribe first (the secret is shown once), then start the relay
        // with it; deliveries wait for the drain below.
        let created = f
            .subscribe(
                "http://127.0.0.1:9/placeholder",
                json!(["agent.work_available"]),
                "relay",
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let secret = created.body["secret"].as_str().unwrap().to_owned();
        let hook = created.body["webhook"]["id"].as_str().unwrap().to_owned();
        let relay = examples::Program::start(
            "github-actions-relay/relay.py",
            &[
                ("RUNBOOK_WEBHOOK_SECRET", secret),
                ("GITHUB_TOKEN", "ghs_e2e".to_owned()),
                ("GITHUB_API_URL", github.url()),
                ("RELAY_ALLOWED_REPOS", "acme/api".to_owned()),
                ("RELAY_HOST", "127.0.0.1".to_owned()),
                ("RELAY_PORT", "0".to_owned()),
            ],
        );
        let moved = f
            .call(
                "PATCH",
                &format!("/v1/projects/{}/webhooks/{hook}", f.project),
                json!({"url": format!("{}/runbook", relay.url)}),
                None,
            )
            .await;
        assert_eq!(moved.status, 200, "{}", moved.body);
        let exe = f.run(GATED).await;
        let mut drainer = Drainer::new(f.config.clone(), settings());
        drain(&mut drainer).await;
        assert!(
            github.seen().is_empty(),
            "no agent work before the decision"
        );
        let read = f
            .call("GET", &format!("/v1/executions/{exe}"), Value::Null, None)
            .await;
        let gate = read.body["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "gate")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let vote = f
            .call(
                "POST",
                // F56: the deprecated singular alias still decides.
                &format!("/v1/step-runs/{gate}/decision"),
                json!({"option": "rollback"}),
                Some("vote"),
            )
            .await;
        assert_eq!(vote.status, 200, "{}", vote.body);
        let stats = drain(&mut drainer).await;
        assert_eq!(stats.delivery_failures, 0, "{stats:?}");
        let calls = github.seen();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            calls[0].path,
            "/repos/acme/api/actions/workflows/runbook-resume.yml/dispatches"
        );
        let inputs = &calls[0].json()["inputs"];
        assert_eq!(inputs["execution_id"], exe.as_str());
        assert!(inputs["event_id"].as_str().unwrap().starts_with("evt_"));
        // The relay acknowledged it: the delivery log shows one 200.
        let log = f
            .call(
                "GET",
                &format!("/v1/projects/{}/webhooks/{hook}/deliveries", f.project),
                Value::Null,
                None,
            )
            .await;
        assert_eq!(log.body["items"][0]["state"], "delivered", "{}", log.body);
        assert_eq!(log.body["items"][0]["event_id"], inputs["event_id"]);
    })
    .await;
}

// ---- Transport tests that need no database ----

fn outgoing(url: &str, secret: &str) -> runbook_core::webhooks::OutgoingRequest {
    let body = r#"{"id":"evt_00000000000000000000000001","type":"webhook.test"}"#.to_owned();
    let timestamp = now_s();
    runbook_core::webhooks::OutgoingRequest {
        url: url.into(),
        headers: vec![
            ("Runbook-Timestamp".into(), timestamp.to_string()),
            (
                "Runbook-Signature".into(),
                runbook_core::webhooks::signature_header(&[secret], timestamp, body.as_bytes()),
            ),
        ],
        body,
    }
}

/// F50: the connection goes to exactly the address vetting approved. The
/// name is unresolvable by the system (`.invalid`, RFC 6761), so the
/// delivery arrives only through the pinned answer, and it takes exactly
/// one lookup per attempt (no second, unvetted resolution).
#[tokio::test]
async fn delivery_connects_only_to_the_vetted_address() {
    use runbook_core::webhooks::{TargetPolicy, WebhookTransport};
    let receiver = Receiver::start();
    let port = receiver.port;
    let lookups = Arc::new(AtomicUsize::new(0));
    let counter = lookups.clone();
    let resolver: Resolve = Arc::new(move |host, _| {
        assert_eq!(host, "hooks.pinned.invalid");
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))])
    });
    let transport = runbook_server::webhooks::HttpTransport::new(
        TargetPolicy {
            allow_private: true,
            allow_link_local: false,
        },
        resolver,
    );
    let secret = runbook_core::webhooks::encode_webhook_secret([9; 32]);
    for attempt in 1..=2 {
        let result = transport
            .post(outgoing(
                &format!("http://hooks.pinned.invalid:{port}/hook"),
                &secret,
            ))
            .await;
        assert!(
            matches!(
                result,
                runbook_core::webhooks::TransportResult::Response { status: 200, .. }
            ),
            "{result:?}"
        );
        assert_eq!(
            lookups.load(Ordering::SeqCst),
            attempt,
            "one lookup per attempt"
        );
    }
    let seen = receiver.seen();
    assert_eq!(seen.len(), 2);
    assert!(
        verify_signature(
            &secret,
            seen[0].header("Runbook-Timestamp"),
            seen[0].body.as_bytes(),
            seen[0].header("Runbook-Signature"),
            now_s(),
            SIGNATURE_TOLERANCE_S
        )
        .is_ok(),
        "the signed delivery arrived intact"
    );
}

/// F49: a resolver failure is a retryable transport failure (30 s backoff),
/// while a forbidden answer still fails the delivery at once.
#[tokio::test]
async fn unresolved_targets_retry_and_forbidden_targets_fail() {
    use runbook_core::webhooks::{DeliveryRecord, TargetPolicy, WebhookTransport, complete};
    let record = DeliveryRecord {
        id: "whd_00000000000000000000000001".parse().unwrap(),
        subscription_id: "whk_00000000000000000000000001".parse().unwrap(),
        event_id: "evt_00000000000000000000000001".into(),
        event_type: "approval.requested".into(),
        execution_id: None,
        redelivery_of: None,
        state: DeliveryState::Pending,
        created_at_ms: 1_000,
        window_ends_at_ms: 1_000 + 86_400_000,
        next_attempt_at_ms: Some(1_000),
        attempts: 0,
        failure: None,
    };
    let secret = runbook_core::webhooks::encode_webhook_secret([9; 32]);
    let blip: Resolve = Arc::new(|_, _| Err(std::io::ErrorKind::Other.into()));
    let result = runbook_server::webhooks::HttpTransport::new(TargetPolicy::default(), blip)
        .post(outgoing("https://hooks.example.com/hook", &secret))
        .await;
    let completion = complete(&record, &result, 5_000);
    assert_eq!(completion.state, DeliveryState::Pending, "{result:?}");
    assert_eq!(completion.next_attempt_at_ms, Some(5_000 + 30_000));
    let private: Resolve = Arc::new(|_, port| Ok(vec![SocketAddr::from(([10, 0, 0, 7], port))]));
    let result = runbook_server::webhooks::HttpTransport::new(TargetPolicy::default(), private)
        .post(outgoing("https://hooks.example.com/hook", &secret))
        .await;
    let completion = complete(&record, &result, 5_000);
    assert_eq!(completion.state, DeliveryState::Failed);
    assert_eq!(
        completion.failure.as_deref(),
        Some("webhooks.target_forbidden")
    );
}
