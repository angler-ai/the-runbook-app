//! The runner tests' fixture (R0): a bootstrapped server on an owned
//! database with a webhook secret key (it seals the signing keys), called
//! in process, plus helpers to mint personal tokens with chosen scopes.
#![allow(dead_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::FutureExt;
use runbook_core::credentials::{
    ActorKind, CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind,
    encode_secret, secret_hash,
};
use runbook_model::*;
use runbook_server::{
    config::{ServerConfig, WebhookConfig},
    http::{Worker, router},
};
use runbook_store_postgres::PostgresCredentials;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    panic::AssertUnwindSafe,
    sync::atomic::{AtomicU64, Ordering},
};
use tower::ServiceExt;

pub struct Fixture {
    pub config: ServerConfig,
    pub worker: Worker,
    pub app: Router,
    /// The bootstrap owner's PAT (PRD scopes, no runner scopes).
    pub token: String,
    pub org: OrganizationId,
    pub user: UserId,
    pub project: ProjectId,
}
pub struct Registered {
    pub admin: String,
    pub install: String,
    pub directory: String,
    pub release: String,
    pub join: String,
    pub runner: String,
    pub group: String,
    pub claim: String,
}
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub headers: BTreeMap<String, String>,
}
impl Reply {
    pub fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or("")
    }
}

pub fn webhooks() -> WebhookConfig {
    WebhookConfig {
        allow_private_targets: true,
        allow_link_local_targets: false,
        secret_key: Some([5; 32]),
        resolver: None,
        ..Default::default()
    }
}

static NEXT: AtomicU64 = AtomicU64::new(1);
fn random32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).unwrap();
    bytes
}
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

impl Fixture {
    pub async fn new(db: &super::database::TestDatabase) -> Self {
        Self::with(db, |_| {}).await
    }
    pub async fn with(
        db: &super::database::TestDatabase,
        adjust: impl FnOnce(&mut ServerConfig),
    ) -> Self {
        let mut config = ServerConfig::new(
            db.config().clone(),
            "https://runbooks.example",
            "127.0.0.1:0",
        )
        .unwrap();
        config.webhooks = webhooks();
        adjust(&mut config);
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

    /// A personal token for `user` with exactly these scopes.
    pub async fn pat(&self, user: &UserId, actor: ActorKind, scopes: &[Scope]) -> String {
        self.pat_for(user, actor, scopes, None).await
    }
    /// A personal token restricted to `projects` (when given).
    pub async fn pat_for(
        &self,
        user: &UserId,
        actor: ActorKind,
        scopes: &[Scope],
        projects: Option<&[ProjectId]>,
    ) -> String {
        let secret = encode_secret(TokenKind::Personal, random32());
        let id = format!(
            "tok_{:026}",
            9_000_000 + NEXT.fetch_add(1, Ordering::Relaxed)
        );
        PostgresCredentials::new(self.config.database.clone())
            .insert(CredentialRecord {
                id: id.parse().unwrap(),
                kind: TokenKind::Personal,
                hash: secret_hash(&secret),
                principal: CredentialPrincipal::User {
                    user_id: user.clone(),
                    actor,
                },
                org_id: self.org.clone(),
                scopes: scopes.iter().copied().collect(),
                projects: projects.map(|p| p.iter().cloned().collect()),
                expires_at: Some(now_ms() + 3_600_000),
                revoked_at: None,
                last_used_at: None,
            })
            .await
            .unwrap();
        secret
    }

    /// The owner's PAT with every PRD scope plus both runner scopes.
    pub async fn admin_pat(&self) -> String {
        let mut scopes = Scope::ALL.to_vec();
        scopes.extend(Scope::RUNNER);
        self.pat(&self.user, ActorKind::Human, &scopes).await
    }
    /// A new org member (org role member), with `role` on the fixture's
    /// project when given.
    pub async fn member(&self, name: &str, role: Option<&str>) -> UserId {
        let created = self
            .call(
                "POST",
                &format!("/v1/orgs/{}/users", self.org),
                &self.token,
                json!({"name": name}),
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let user: UserId = created.body["id"].as_str().unwrap().parse().unwrap();
        if let Some(role) = role {
            let added = self
                .call(
                    "POST",
                    &format!("/v1/projects/{}/members", self.project),
                    &self.token,
                    json!({"user_id": user, "role": role}),
                )
                .await;
            assert!(added.status < 300, "{}", added.body);
        }
        user
    }
    /// Register the org's runner at `url` and join one runner.
    pub async fn register_runner(&self, url: &str) -> Registered {
        let admin = self.admin_pat().await;
        let boot = self
            .call(
                "POST",
                &format!("/v1/orgs/{}/runner/bootstrap-tokens", self.org),
                &admin,
                json!({"url": url}),
            )
            .await;
        assert_eq!(boot.status, 201, "{}", boot.body);
        let registered = self
            .call(
                "POST",
                "/v1/runner-installs/register",
                boot.body["token"].as_str().unwrap(),
                json!({"url": url, "version": "0.1.0", "contract_version": 1}),
            )
            .await;
        assert_eq!(registered.status, 201, "{}", registered.body);
        let install = registered.body["install_id"].as_str().unwrap().to_owned();
        let group = format!("rg_{:026}", NEXT.fetch_add(1, Ordering::Relaxed));
        let joined = self
            .call(
                "POST",
                &format!("/v1/runner-installs/{install}/runners"),
                registered.body["join_token"].as_str().unwrap(),
                json!({"name": "runner-1", "group_id": group, "group_name": "default", "version": "0.1.0"}),
            )
            .await;
        assert_eq!(joined.status, 201, "{}", joined.body);
        Registered {
            admin,
            install,
            directory: registered.body["directory_token"].as_str().unwrap().into(),
            release: registered.body["release_token"].as_str().unwrap().into(),
            join: registered.body["join_token"].as_str().unwrap().into(),
            runner: joined.body["runner_id"].as_str().unwrap().into(),
            group,
            claim: joined.body["claim_token"].as_str().unwrap().into(),
        }
    }
    /// Allow the fixture's project on the group and heartbeat once.
    pub async fn allow_project(&self, reg: &Registered) {
        let put = self
            .call(
                "PUT",
                &format!("/v1/orgs/{}/runner/groups/{}/projects", self.org, reg.group),
                &reg.admin,
                json!({"project_ids": [self.project]}),
            )
            .await;
        assert_eq!(put.status, 204, "{}", put.body);
        self.heartbeat(reg).await;
    }
    pub async fn heartbeat(&self, reg: &Registered) {
        let beat = self
            .call(
                "POST",
                &format!("/v1/runner-installs/{}/heartbeat", reg.install),
                &reg.directory,
                json!({"groups": [], "runners": []}),
            )
            .await;
        assert_eq!(beat.status, 204, "{}", beat.body);
    }

    /// A request with a raw JSON body; its status.
    pub async fn raw(&self, method: &str, path: &str, bearer: &str, body: String) -> u16 {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {bearer}"))
            .header("content-type", "application/json")
            .header(
                "idempotency-key",
                format!("raw{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            )
            .body(Body::from(body))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        response.status().as_u16()
    }

    pub async fn call(&self, method: &str, path: &str, bearer: &str, body: Value) -> Reply {
        self.request(method, path, Some(bearer), &[], body).await
    }
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        headers: &[(&str, &str)],
        body: Value,
    ) -> Reply {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(bearer) = bearer {
            request = request.header("authorization", format!("Bearer {bearer}"));
        }
        if method == "POST" && !headers.iter().any(|(h, _)| *h == "idempotency-key") {
            request = request.header(
                "idempotency-key",
                format!("k{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            );
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
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
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_owned(),
                    v.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let bytes = to_bytes(response.into_body(), 4 << 20).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        Reply {
            status,
            body,
            headers,
        }
    }

    /// Create and publish a runbook in the fixture's project; its ID.
    pub async fn publish(&self, token: &str, source: &str) -> Reply {
        let created = self
            .call(
                "POST",
                &format!("/v1/projects/{}/runbooks", self.project),
                token,
                json!({"source": source}),
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = self
            .call(
                "GET",
                &format!("/v1/runbooks/{rb}/draft"),
                token,
                Value::Null,
            )
            .await;
        let etag = draft.headers.get("etag").cloned().unwrap_or_default();
        let mut reply = self
            .request(
                "POST",
                &format!("/v1/runbooks/{rb}/versions"),
                Some(token),
                &[("if-match", &etag)],
                json!({}),
            )
            .await;
        reply.body["__runbook_id"] = json!(rb);
        reply
    }

    /// Start a run of a published runbook; the execution ID.
    pub async fn start(&self, token: &str, runbook: &str) -> String {
        let started = self
            .call(
                "POST",
                &format!("/v1/projects/{}/executions", self.project),
                token,
                json!({"runbook": runbook}),
            )
            .await;
        assert_eq!(started.status, 201, "{}", started.body);
        self.drain().await;
        started.body["id"].as_str().unwrap().to_owned()
    }
    /// Run the engine's timers and fan-out until idle.
    pub async fn drain(&self) {
        let mut drainer = runbook_server::drain::Drainer::new(
            self.config.clone(),
            runbook_server::drain::DrainSettings {
                batch: 32,
                lease_ms: 30_000,
                idle: std::time::Duration::from_millis(10),
            },
        );
        for _ in 0..20 {
            let stats = drainer.drain_once().await.unwrap();
            if !stats.busy() {
                return;
            }
        }
        panic!("drain did not quiesce");
    }

    pub async fn sql(&self) -> runbook_store_postgres::PostgresStore {
        runbook_store_postgres::PostgresStore::connect(&self.config.database)
            .await
            .unwrap()
    }
}

/// Run `f` against a fresh fixture on its own database.
pub async fn with_fixture(f: impl AsyncFnOnce(&Fixture)) {
    with_fixture_config(|_| {}, f).await
}
pub async fn with_fixture_config(
    adjust: impl FnOnce(&mut ServerConfig),
    f: impl AsyncFnOnce(&Fixture),
) {
    let mut db = super::database::TestDatabase::create_from_env()
        .await
        .unwrap();
    let result = AssertUnwindSafe(async {
        let fixture = Fixture::with(&db, adjust).await;
        f(&fixture).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
