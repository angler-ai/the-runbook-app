//! AUTH2 (D43–D46): the auth child, the `/api/auth` proxy, session cookies,
//! CSRF, identity linking and the internal routes, against a stub child that
//! speaks the D44 contract (`support/auth_stub.rs`).
#[path = "support/auth_stub.rs"]
mod auth_stub;
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use auth_stub::Files;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::{FutureExt, StreamExt};
use runbook_core::{
    auth_links::{AuthLinkStore, INVITATION_TTL_MS, InvitationRecord, InvitationState},
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, OrgRole},
    directory_operations::DirectoryOperations,
};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, UserId};
use runbook_server::{
    config::{AuthConfig, ServerConfig},
    http::{PeerAddr, Worker, router},
};
use runbook_store_postgres::{DatabaseConfig, PostgresDirectory};
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, time::Duration};
use tower::ServiceExt;

const ORIGIN: &str = "https://runbooks.example";
const COOKIE: &str = "__Host-runbook.session_token";

/// The stub child's entry point: a no-op unless this binary was started as
/// the auth child.
#[test]
fn auth_stub_child() {
    if std::env::var_os(auth_stub::ENV).is_some() {
        auth_stub::run();
    }
}

fn offline() -> DatabaseConfig {
    DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None).unwrap()
}
fn auth_config(files: &Files) -> AuthConfig {
    let mut auth = AuthConfig::new(auth_stub::command("auth_stub_child"));
    auth.child_env = files.env();
    auth.methods = vec!["email".into()];
    auth
}
fn config(database: DatabaseConfig, auth: Option<AuthConfig>) -> ServerConfig {
    let mut config = ServerConfig::new(database, ORIGIN, "127.0.0.1:0").unwrap();
    config.auth = auth;
    config
}
fn start(config: ServerConfig) -> (Worker, Router) {
    let worker = Worker::start(config).unwrap();
    if let Some(child) = worker.client().auth_child() {
        assert!(child.wait_ready(Duration::from_secs(20)), "no ready line");
    }
    let app = router(worker.client());
    (worker, app)
}
fn loopback() -> PeerAddr {
    PeerAddr("127.0.0.1:50000".parse().unwrap())
}

struct Reply {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Value,
}
async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    Reply {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}
fn cookie(token: &str) -> String {
    format!("{COOKIE}={token}")
}

#[test]
fn session_staleness_is_bounded_by_one_minute() {
    // D32 + D44: a revoked session's stream ends within 60 s: at most one
    // cache lifetime plus one recheck interval.
    let limits = runbook_server::config::TransportLimits::default();
    let auth = AuthConfig::new(vec!["x".into()]);
    assert_eq!(auth.session_cache_ttl, Duration::from_secs(30));
    assert!(limits.stream_recheck + auth.session_cache_ttl <= Duration::from_secs(60));
}

/// D43: without RUNBOOK_AUTH_SECRET there is no proxy and no internal route.
#[tokio::test]
async fn without_auth_the_auth_paths_do_not_exist() {
    let (_worker, app) = start(config(offline(), None));
    for (method, path) in [
        ("GET", "/api/auth/get-session"),
        ("POST", "/api/auth/sign-out"),
        ("POST", "/internal/can-register"),
        ("POST", "/v1/device/code"),
        ("POST", "/v1/device/token"),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(loopback());
        let r = send(&app, request).await;
        assert_eq!(r.status, 404, "{method} {path}");
    }
    let r = send(
        &app,
        Request::builder()
            .uri("/.well-known/runbook")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.body["auth"]["methods"], json!([]));
    assert_eq!(r.body["auth"]["device_authorization_endpoint"], Value::Null);
}

/// D43: the proxy forwards method, path, query and body; replaces the
/// client-address headers with the peer; strips the internal secret; passes
/// both Set-Cookie headers; never follows a redirect; bounds the body.
#[tokio::test]
async fn the_proxy_forwards_auth_requests_to_the_child() {
    let files = Files::new("proxy");
    let (worker, app) = start(config(offline(), Some(auth_config(&files))));
    let secret = worker.client().auth_child().unwrap().secret().to_owned();
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/auth/sign-in/email-otp?x=1")
        .header("content-type", "application/json")
        .header("x-forwarded-for", "203.0.113.9")
        .header("x-runbook-client-ip", "203.0.113.10")
        .header("x-runbook-internal-secret", &secret)
        .header("x-runbook-auth-nonce", "chosen-by-the-client")
        .header("origin", ORIGIN)
        .body(Body::from(r#"{"email":"a@example.test","otp":"123456"}"#))
        .unwrap();
    request
        .extensions_mut()
        .insert(PeerAddr("192.0.2.7:4000".parse().unwrap()));
    let r = send(&app, request).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["method"], "POST");
    assert_eq!(r.body["path"], "/api/auth/sign-in/email-otp");
    assert_eq!(r.body["query"], "x=1");
    assert_eq!(
        r.body["body"],
        r#"{"email":"a@example.test","otp":"123456"}"#
    );
    let seen = &r.body["headers"];
    // auth/README.md: the peer as x-runbook-client-ip, nothing the client
    // claimed about its address, never the internal secret.
    assert_eq!(seen["x-runbook-client-ip"], "192.0.2.7");
    assert!(seen.get("x-forwarded-for").is_none(), "{seen}");
    assert_eq!(seen["origin"], ORIGIN);
    assert!(seen.get("x-runbook-internal-secret").is_none(), "{seen}");
    let cookies: Vec<_> = r.headers.get_all("set-cookie").iter().collect();
    assert_eq!(cookies.len(), 2);
    // Gate fix 2: the server's nonce went to the child, the proof stays here.
    assert!(
        seen["x-runbook-auth-nonce"]
            .as_str()
            .is_some_and(|n| n.len() == 32),
        "{seen}"
    );
    assert!(!r.headers.contains_key("x-runbook-auth-proof"));
    assert_eq!(r.headers["cache-control"], "no-store");
    assert!(r.headers.contains_key("x-request-id"));

    let r = send(
        &app,
        Request::builder()
            .uri("/api/auth/redirect")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 302);
    assert_eq!(
        r.headers["location"],
        "https://accounts.example.test/o/oauth2"
    );

    let r = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/auth/sign-in/email-otp")
            .body(Body::from(vec![b'x'; 70 * 1024]))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 413);
    // `/api/authx` is not the auth prefix.
    let r = send(
        &app,
        Request::builder()
            .uri("/api/authx")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 404);
    // The discovery document lists the configured methods.
    let r = send(
        &app,
        Request::builder()
            .uri("/.well-known/runbook")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.body["auth"]["methods"], json!(["email"]));
}

/// Sign-in gate fix 1: behind a trusted proxy (RUNBOOK_TRUSTED_PROXIES)
/// each client is its own rate-limit bucket and its own
/// `x-runbook-client-ip`; an untrusted peer's forwarding headers are
/// ignored; and no client-sent address header reaches the child.
#[tokio::test]
async fn a_trusted_proxy_names_each_client() {
    let files = Files::new("trusted");
    let mut config = config(offline(), Some(auth_config(&files)));
    config.transport.trusted_proxies =
        runbook_server::client_ip::TrustedProxies::parse("10.0.0.0/8").unwrap();
    let (_worker, app) = start(config);
    let via = |peer: &str, path: &str, xff: Option<&str>| {
        let mut builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("origin", ORIGIN)
            .header("x-real-ip", "192.0.2.50")
            .header("cf-connecting-ip", "192.0.2.51")
            .header("true-client-ip", "192.0.2.52");
        if let Some(xff) = xff {
            builder = builder
                .header("x-forwarded-for", xff)
                .header("forwarded", "for=192.0.2.53");
        }
        let mut request = builder.body(Body::from("{}")).unwrap();
        request
            .extensions_mut()
            .insert(PeerAddr(peer.parse().unwrap()));
        request
    };
    // The child sees the client the proxy named, and none of the headers.
    let r = send(
        &app,
        via(
            "10.0.0.5:4000",
            "/api/auth/echo",
            Some("1.2.3.4, 198.51.100.7"),
        ),
    )
    .await;
    let seen = &r.body["headers"];
    assert_eq!(seen["x-runbook-client-ip"], "198.51.100.7", "{seen}");
    for header in [
        "x-forwarded-for",
        "forwarded",
        "x-real-ip",
        "cf-connecting-ip",
        "true-client-ip",
    ] {
        assert!(seen.get(header).is_none(), "{header}: {seen}");
    }
    // An untrusted peer is itself, whatever it claims.
    let r = send(
        &app,
        via("203.0.113.5:4000", "/api/auth/echo", Some("198.51.100.7")),
    )
    .await;
    assert_eq!(r.body["headers"]["x-runbook-client-ip"], "203.0.113.5");

    // Per-client device-code buckets behind the proxy (the store is
    // offline, so an admitted request answers 503, a limited one 429).
    let code = async |peer: &str, xff: Option<&str>| {
        send(&app, via(peer, "/v1/device/code", xff)).await.status
    };
    for n in 0..20 {
        assert_ne!(code("10.0.0.5:1", Some("198.51.100.7")).await, 429, "{n}");
    }
    assert_eq!(code("10.0.0.6:1", Some("198.51.100.7")).await, 429);
    assert_ne!(code("10.0.0.5:1", Some("198.51.100.8")).await, 429);
    // Spoofing from outside: one bucket per peer, whatever the header says.
    for n in 0..20 {
        let xff = format!("198.51.100.{}", 100 + n);
        assert_ne!(code("203.0.113.9:1", Some(&xff)).await, 429, "{n}");
    }
    assert_eq!(code("203.0.113.9:2", Some("198.51.100.200")).await, 429);
}

/// Sign-in gate fix 2: the child binds its own port and reports it in its
/// ready line, read again on every restart, so whatever squats the old port
/// after a crash never hears from the server.
#[tokio::test]
async fn a_restarted_child_is_reached_on_its_new_port_only() {
    let files = Files::new("squat");
    let mut auth = auth_config(&files);
    auth.restart_min = Duration::from_millis(50);
    auth.session_cache_ttl = Duration::ZERO;
    let (worker, app) = start(config(offline(), Some(auth)));
    let client = worker.client();
    let child = client.auth_child().unwrap();
    let old = child.port().expect("a ready child has a port");
    let pid = child.pid().unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    // Take the old port before the child comes back.
    let mut squatter = None;
    for _ in 0..200 {
        if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", old)) {
            squatter = Some(l);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let squatter = squatter.expect("the old port is free once the child is gone");
    squatter.set_nonblocking(true).unwrap();
    for _ in 0..400 {
        if child.restarts() >= 1 && child.ready() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(child.ready(), "the child came back");
    assert_ne!(child.port(), Some(old));
    let echo = Request::builder()
        .method("GET")
        .uri("/api/auth/echo")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, echo).await.status, 200);
    files.put_session("tok-squat", "ba_squat", "squat@example.test");
    let me = Request::builder()
        .uri("/v1/me")
        .header("cookie", cookie("tok-squat"))
        .body(Body::empty())
        .unwrap();
    // The session resolves through the new port (the offline store then
    // answers whatever it answers; never auth.unavailable).
    assert_ne!(send(&app, me).await.body["code"], "auth.unavailable");
    assert_eq!(
        squatter.accept().map(|_| ()).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "the squatter got a connection"
    );
}

/// Sign-in gate fix 2: every answer from the child must carry the HMAC of
/// the request's nonce under the per-boot secret. A listener that cannot
/// prove it (a port squatter) is refused: the proxy answers 502 and a
/// session cookie resolves to `auth.unavailable`, never a session.
#[tokio::test]
async fn a_child_answer_without_the_proof_is_refused() {
    let files = Files::new("noproof");
    let mut auth = auth_config(&files);
    auth.child_env.push(("STUB_NO_PROOF".into(), "1".into()));
    let (_worker, app) = start(config(offline(), Some(auth)));
    files.put_session("tok-forged", "ba_forged", "forged@example.test");
    let echo = Request::builder()
        .method("GET")
        .uri("/api/auth/echo")
        .body(Body::empty())
        .unwrap();
    let r = send(&app, echo).await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (502, Some("auth.unavailable"))
    );
    let me = Request::builder()
        .uri("/v1/me")
        .header("cookie", cookie("tok-forged"))
        .body(Body::empty())
        .unwrap();
    let r = send(&app, me).await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (503, Some("auth.unavailable"))
    );
}

/// Sign-in gate fix 5: the proxy forwards only plain paths: no `..`, no
/// empty segment, no encoded dot, slash or backslash, no backslash.
#[tokio::test]
async fn the_proxy_refuses_traversal() {
    let files = Files::new("traversal");
    let (_worker, app) = start(config(offline(), Some(auth_config(&files))));
    for path in [
        "/api/auth/../internal/session",
        "/api/auth/x/../../internal/session",
        "/api/auth/..",
        "/api/auth//internal/session",
        "/api/auth/%2e%2e/internal/session",
        "/api/auth/%2E%2E/internal/session",
        "/api/auth/.%2e/internal",
        "/api/auth/x%2finternal",
        "/api/auth/x%2Finternal",
        "/api/auth/x%5cinternal",
        "/api/auth/x%5Cinternal",
        "/api/auth/x\\..\\internal",
        "/api/auth/./sign-out",
        "/api/auth/x?next=%2e%2e%2f",
    ] {
        let r = send(
            &app,
            Request::builder()
                .method("POST")
                .uri(path)
                .header("origin", ORIGIN)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        if path.contains('?') {
            // The query is the child's to read; only the path is checked.
            assert_eq!(r.status, 200, "{path}");
        } else {
            assert_eq!(
                (r.status, r.body["code"].as_str()),
                (404, Some("resource.not_found")),
                "{path}"
            );
        }
    }
    let forwarded: Vec<String> = files
        .log()
        .iter()
        .filter_map(|l| l["path"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        forwarded,
        vec!["/api/auth/x".to_owned()],
        "only the plain path reached the child"
    );
}

/// The internal route answers only the auth child: loopback peer, no
/// forwarding headers, the per-boot secret. Everything else is a 404, like
/// any unknown route; other `/internal/*` paths never exist.
#[tokio::test]
async fn internal_routes_are_loopback_and_secret_only() {
    let files = Files::new("internal");
    let (worker, app) = start(config(offline(), Some(auth_config(&files))));
    let secret = worker.client().auth_child().unwrap().secret().to_owned();
    let make =
        |peer: Option<&str>, secret: Option<&str>, extra: Option<(&str, &str)>, path: &str| {
            let mut builder = Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json");
            if let Some(secret) = secret {
                builder = builder.header("x-runbook-internal-secret", secret);
            }
            if let Some((k, v)) = extra {
                builder = builder.header(k, v);
            }
            let mut request = builder
                .body(Body::from(r#"{"email":"a@example.test"}"#))
                .unwrap();
            if let Some(peer) = peer {
                request
                    .extensions_mut()
                    .insert(PeerAddr(peer.parse().unwrap()));
            }
            request
        };
    let can = "/internal/can-register";
    for request in [
        make(Some("127.0.0.1:1"), None, None, can),
        make(Some("127.0.0.1:1"), Some("wrong"), None, can),
        make(Some("192.0.2.1:1"), Some(&secret), None, can),
        make(None, Some(&secret), None, can),
        make(
            Some("127.0.0.1:1"),
            Some(&secret),
            Some(("x-forwarded-for", "192.0.2.1")),
            can,
        ),
        make(
            Some("127.0.0.1:1"),
            Some(&secret),
            Some(("forwarded", "for=192.0.2.1")),
            can,
        ),
        make(
            Some("127.0.0.1:1"),
            Some(&secret),
            None,
            "/internal/session",
        ),
        make(Some("127.0.0.1:1"), Some(&secret), None, "/internal/users"),
    ] {
        let r = send(&app, request).await;
        assert_eq!(r.status, 404, "{}", r.body);
        assert_eq!(r.body["code"], "resource.not_found");
    }
    // Through the gate, the route reaches the store (offline here: 503).
    let r = send(&app, make(Some("[::1]:1"), Some(&secret), None, can)).await;
    assert_eq!(r.status, 503, "{}", r.body);
}

/// D46 before any storage: a cookie POST with no Origin, a foreign Origin or
/// no X-Runbook-CSRF is 403 auth.csrf; an Authorization header means the
/// cookie is ignored (and so is CSRF); /hooks ignores cookies. (`/mcp`
/// ignoring them is tested in `tests/mcp_http.rs`.)
#[tokio::test]
async fn cookie_writes_need_same_origin_and_the_csrf_header() {
    let files = Files::new("csrf");
    files.put_session("tok-a", "ba_a", "a@example.test");
    let (_worker, app) = start(config(offline(), Some(auth_config(&files))));
    let post = |headers: &[(&str, &str)]| {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/orgs/org_00000000000000000000000001/teams")
            .header("cookie", cookie("tok-a"))
            .header("content-type", "application/json")
            .header("idempotency-key", "k");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder
            .body(Body::from(r#"{"slug":"t","title":"T"}"#))
            .unwrap()
    };
    for headers in [
        vec![],
        vec![("x-runbook-csrf", "1")],
        vec![("origin", "https://evil.example"), ("x-runbook-csrf", "1")],
        vec![("origin", ORIGIN)],
        vec![("sec-fetch-site", "cross-site"), ("x-runbook-csrf", "1")],
    ] {
        let r = send(&app, post(&headers)).await;
        assert_eq!(
            (r.status, r.body["code"].as_str()),
            (403, Some("auth.csrf")),
            "{headers:?}"
        );
    }
    let before = files.log().len();
    // With a bearer, the cookie is never consulted: no CSRF, no session call.
    let r = send(
        &app,
        post(&[("authorization", "Bearer trb_pat_not-a-real-token")]),
    )
    .await;
    assert_eq!(r.status, 401, "{}", r.body);
    let r = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/hooks/github")
            .header("cookie", cookie("tok-a"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 404, "{}", r.body);
    assert_eq!(
        files.log().len(),
        before,
        "the child was asked: {:?}",
        files.log()
    );
}

/// With the auth child unreachable, a cookie request is 503
/// auth.unavailable (never anonymous), and discovery still answers.
#[tokio::test]
async fn an_unreachable_auth_child_fails_closed_but_discovery_answers() {
    let files = Files::new("down");
    let mut auth = auth_config(&files);
    auth.command = vec!["/nonexistent/runbook-auth".into()];
    let worker = Worker::start(config(offline(), Some(auth))).unwrap();
    let app = router(worker.client());
    let get = |path: &str| {
        Request::builder()
            .uri(path)
            .header("cookie", cookie("tok-a"))
            .body(Body::empty())
            .unwrap()
    };
    let r = send(&app, get("/v1/me")).await;
    assert_eq!(
        (r.status, r.body["code"].as_str()),
        (503, Some("auth.unavailable"))
    );
    for path in ["/.well-known/runbook", "/v1/meta"] {
        assert_eq!(send(&app, get(path)).await.status, 200, "{path}");
    }
}

// ---------------------------------------------------------------- Postgres

struct Fixture {
    worker: Worker,
    app: Router,
    files: Files,
    pat: String,
    org: OrganizationId,
    owner: UserId,
    project: ProjectId,
    db: DatabaseConfig,
}
impl Fixture {
    async fn new(db: &database::TestDatabase, tune: impl FnOnce(&mut ServerConfig)) -> Self {
        let files = Files::new("pg");
        let mut config = config(db.config().clone(), Some(auth_config(&files)));
        tune(&mut config);
        let issued = runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let (worker, app) = start(config);
        let fixture = Self {
            worker,
            app,
            files,
            pat: issued["pat"].as_str().unwrap().into(),
            org: issued["org_id"].as_str().unwrap().parse().unwrap(),
            owner: issued["user_id"].as_str().unwrap().parse().unwrap(),
            project: issued["project_id"].as_str().unwrap().parse().unwrap(),
            db: db.config().clone(),
        };
        let directory = PostgresDirectory::new(fixture.db.clone());
        let revision = directory.snapshot(&fixture.org).await.unwrap().revision;
        directory
            .commit(DirectoryCommit {
                org_id: fixture.org.clone(),
                expected_revision: revision,
                mutations: vec![DirectoryMutation::BindVerifiedEmail {
                    user_id: fixture.owner.clone(),
                    email: "owner@example.test".into(),
                }],
            })
            .await
            .unwrap();
        fixture
    }
    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> Reply {
        let mut builder = Request::builder().uri(path);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        send(&self.app, builder.body(Body::empty()).unwrap()).await
    }
    async fn me(&self, token: &str) -> Reply {
        self.get("/v1/me", &[("cookie", &cookie(token))]).await
    }
    async fn can_register(&self, email: &str) -> bool {
        let secret = self
            .worker
            .client()
            .auth_child()
            .unwrap()
            .secret()
            .to_owned();
        let mut request = Request::builder()
            .method("POST")
            .uri("/internal/can-register")
            .header("content-type", "application/json")
            .header("x-runbook-internal-secret", secret)
            .body(Body::from(json!({"email": email}).to_string()))
            .unwrap();
        request.extensions_mut().insert(loopback());
        let r = send(&self.app, request).await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["allowed"].as_bool().unwrap()
    }
}
async fn with_fixture(tune: impl FnOnce(&mut ServerConfig), f: impl AsyncFnOnce(&mut Fixture)) {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut fixture = Fixture::new(&db, tune).await;
        f(&mut fixture).await;
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// D46: a session authorizes as its linked human with role-bounded scopes;
/// cookie writes need CSRF; a bearer wins over a cookie; DELETE /v1/me/token
/// is not a session's sign-out.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_session_authorizes_as_its_human_and_bearer_wins() {
    with_fixture(
        |_| {},
        async |f| {
            f.files
                .put_session("tok-owner", "ba_owner", "Owner@Example.test");
            let me = f.me("tok-owner").await;
            assert_eq!(me.status, 200, "{}", me.body);
            assert_eq!(me.body["user"]["id"], f.owner.as_str());
            assert_eq!(me.body["credential"]["kind"], "session");
            assert_eq!(me.body["credential"]["actor_kind"], "human");
            assert_eq!(me.body["org"]["role"], "owner");
            assert!(
                me.body["credential"]["scopes"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("project:admin"))
            );
            // An unknown cookie authenticates nothing.
            assert_eq!(f.me("tok-unknown").await.status, 401);
            // Bearer plus cookie: the bearer only.
            let both = f
                .get(
                    "/v1/me",
                    &[
                        ("cookie", &cookie("tok-owner")),
                        ("authorization", &format!("Bearer {}", f.pat)),
                    ],
                )
                .await;
            assert_eq!(both.body["credential"]["kind"], "personal");
            let bad = f
                .get(
                    "/v1/me",
                    &[
                        ("cookie", &cookie("tok-owner")),
                        ("authorization", "Bearer trb_pat_x"),
                    ],
                )
                .await;
            assert_eq!(bad.status, 401);
            // A cookie write: CSRF first, then the directory route itself.
            let team = |headers: &[(&str, &str)], key: &str| {
                let mut builder = Request::builder()
                    .method("POST")
                    .uri(format!("/v1/orgs/{}/teams", f.org))
                    .header("cookie", cookie("tok-owner"))
                    .header("content-type", "application/json")
                    .header("idempotency-key", key);
                for (k, v) in headers {
                    builder = builder.header(*k, *v);
                }
                builder
                    .body(Body::from(r#"{"slug":"oncall","title":"On call"}"#))
                    .unwrap()
            };
            let r = send(
                &f.app,
                team(
                    &[("origin", "https://evil.example"), ("x-runbook-csrf", "1")],
                    "a",
                ),
            )
            .await;
            assert_eq!(r.body["code"], "auth.csrf");
            let r = send(&f.app, team(&[], "a")).await;
            assert_eq!(r.body["code"], "auth.csrf");
            let r = send(
                &f.app,
                team(&[("origin", ORIGIN), ("x-runbook-csrf", "1")], "a"),
            )
            .await;
            assert_eq!(r.status, 201, "{}", r.body);
            let r = send(
                &f.app,
                team(
                    &[("sec-fetch-site", "same-origin"), ("x-runbook-csrf", "1")],
                    "a",
                ),
            )
            .await;
            assert_eq!(r.status, 201, "replay: {}", r.body);
            let r = send(
                &f.app,
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/me/token")
                    .header("cookie", cookie("tok-owner"))
                    .header("origin", ORIGIN)
                    .header("x-runbook-csrf", "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 403);
            assert_eq!(f.me("tok-owner").await.status, 200);
        },
    )
    .await;
}

/// D44: a proxied sign-out stops the session at once on this instance,
/// although the cache would otherwise trust it for its whole lifetime.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sign_out_stops_the_session_at_once() {
    with_fixture(
        |c| c.auth.as_mut().unwrap().session_cache_ttl = Duration::from_secs(3600),
        async |f| {
            f.files
                .put_session("tok-owner", "ba_owner", "owner@example.test");
            assert_eq!(f.me("tok-owner").await.status, 200);
            // Revoked elsewhere: still trusted from the cache (the D44 bound).
            f.files.remove_session("tok-owner");
            assert_eq!(f.me("tok-owner").await.status, 200);
            f.files
                .put_session("tok-owner", "ba_owner", "owner@example.test");
            let r = send(
                &f.app,
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/sign-out")
                    .header("cookie", cookie("tok-owner"))
                    .header("origin", ORIGIN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(r.status, 200, "{}", r.body);
            let me = f.me("tok-owner").await;
            assert_eq!(
                (me.status, me.body["code"].as_str()),
                (401, Some("auth.required"))
            );
            // Gate fix 6: a successful revoke of any session (which one
            // the proxy cannot tell) clears every cached session at once.
            for (n, route) in ["revoke-session", "revoke-sessions", "revoke-other-sessions"]
                .into_iter()
                .enumerate()
            {
                let other = format!("tok-other-{n}");
                f.files
                    .put_session(&other, "ba_owner", "owner@example.test");
                f.files
                    .put_session("tok-actor", "ba_owner", "owner@example.test");
                assert_eq!(f.me(&other).await.status, 200, "{route}");
                f.files.remove_session(&other);
                assert_eq!(f.me(&other).await.status, 200, "{route}: cached");
                let r = send(
                    &f.app,
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/auth/{route}"))
                        .header("cookie", cookie("tok-actor"))
                        .header("origin", ORIGIN)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await;
                assert_eq!(r.status, 200, "{route}: {}", r.body);
                assert_eq!(f.me(&other).await.status, 401, "{route}: evicted");
            }
        },
    )
    .await;
}

/// D45: can-register is invitation-only plus the allowed-domain check;
/// first use links the invited email with the invitation's roles in one
/// commit and consumes the invitation; an uninvited session gets nothing.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn an_invited_email_links_with_its_role_and_an_uninvited_one_cannot() {
    with_fixture(
        |c| c.auth.as_mut().unwrap().allowed_domains = vec!["example.test".into()],
        async |f| {
            let directory = PostgresDirectory::new(f.db.clone());
            let now = directory.current_time_ms().await.unwrap();
            let invite = |n: u8, email: &str| InvitationRecord {
                id: format!("inv_{n:026}").parse().unwrap(),
                org_id: f.org.clone(),
                email: email.into(),
                role: OrgRole::Admin,
                projects: vec![(f.project.clone(), ProjectRole::Editor)],
                invited_by: f.owner.clone(),
                created_at_ms: now,
                expires_at_ms: now + INVITATION_TTL_MS,
                state: InvitationState::Pending,
                accepted_by: None,
                closed_at_ms: None,
                name: None,
            };
            directory.create_invitation(&f.org, invite(1, "new.admin@example.test")).await.unwrap();
            directory.create_invitation(&f.org, invite(2, "outsider@other.test")).await.unwrap();
            assert!(f.can_register("New.Admin@example.test").await);
            assert!(f.can_register("owner@example.test").await);
            assert!(!f.can_register("stranger@example.test").await);
            assert!(!f.can_register("outsider@other.test").await, "domain not allowed");

            f.files.put_session("tok-new", "ba_new", "new.admin@example.test");
            let me = f.me("tok-new").await;
            assert_eq!(me.status, 200, "{}", me.body);
            assert_eq!(me.body["org"]["role"], "admin");
            let user: UserId = me.body["user"]["id"].as_str().unwrap().parse().unwrap();
            let snapshot = directory.snapshot(&f.org).await.unwrap();
            assert_eq!(
                snapshot.project_members.get(&(f.project.clone(), user.clone())),
                Some(&ProjectRole::Editor)
            );
            let listed = directory.list_invitations(&f.org).await.unwrap();
            let accepted = listed.iter().find(|i| i.email == "new.admin@example.test").unwrap();
            assert_eq!(accepted.state, InvitationState::Accepted);
            assert_eq!(accepted.accepted_by.as_ref(), Some(&user));
            // The link is stable: the same person again, no second user.
            assert_eq!(f.me("tok-new").await.body["user"]["id"], user.as_str());
            assert_eq!(directory.snapshot(&f.org).await.unwrap().users.len(), snapshot.users.len());

            // A session for an uninvited email (the hook would have refused
            // it) authenticates nothing and changes nothing.
            f.files.put_session("tok-stranger", "ba_stranger", "stranger@example.test");
            let me = f.me("tok-stranger").await;
            assert_eq!((me.status, me.body["code"].as_str()), (403, Some("auth.forbidden")));
            assert_eq!(directory.auth_link("ba_stranger").await.unwrap(), None);
            assert_eq!(directory.snapshot(&f.org).await.unwrap().revision, snapshot.revision);
            // An unverified email is never a session.
            f.files.update(|s| {
                s.insert("tok-unverified".into(), json!({"auth_user_id":"ba_u","email":"owner@example.test","email_verified":false,"session_id":"s","expires_at":now + 3_600_000}));
            });
            assert_eq!(f.me("tok-unverified").await.status, 401);
        },
    )
    .await;
}

/// D43: the child's registration hook reaches Rust's internal route over
/// real loopback with the per-boot secret.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_child_reaches_can_register_over_loopback() {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let files = Files::new("loop");
        let mut config = config(db.config().clone(), Some(auth_config(&files)));
        config.listen = addr;
        runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let limits = config.transport.clone();
        let (worker, app) = start(config);
        tokio::spawn(async move { runbook_server::http::serve_on(listener, app, &limits).await });
        let base = worker.client().auth_child().unwrap().base_url().unwrap();
        let check = |email: &str| {
            let url = format!("{base}/api/auth/register-check?email={email}");
            async move {
                tokio::task::spawn_blocking(move || {
                    let text = ureq::get(&url).call().unwrap().into_string().unwrap();
                    serde_json::from_str::<Value>(&text).unwrap()
                })
                .await
                .unwrap()
            }
        };
        let answer = check("nobody@example.test").await;
        assert_eq!(answer["status"], 200, "{answer}");
        assert_eq!(answer["body"], r#"{"allowed":false}"#);
        drop(worker);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// D43: the child is restarted after a crash and sessions survive it (they
/// live in the auth service's store); dropping the worker stops the child.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_child_restarts_after_a_crash_without_losing_sessions() {
    with_fixture(
        |c| {
            let auth = c.auth.as_mut().unwrap();
            auth.session_cache_ttl = Duration::ZERO;
            auth.restart_min = Duration::from_millis(50);
        },
        async |f| {
            f.files
                .put_session("tok-owner", "ba_owner", "owner@example.test");
            assert_eq!(f.me("tok-owner").await.status, 200);
            let client = f.worker.client();
            let child = client.auth_child().unwrap();
            let pid = child.pid().unwrap();
            assert!(
                std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            for _ in 0..200 {
                if child.restarts() >= 1 && child.pid().is_some_and(|p| p != pid) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert!(child.restarts() >= 1);
            assert!(child.wait_ready(Duration::from_secs(20)), "no ready line");
            let me = f.me("tok-owner").await;
            assert_eq!(me.status, 200, "{}", me.body);
            assert_eq!(me.body["credential"]["kind"], "session");
        },
    )
    .await;
}

/// D32 under a session: an open event stream asks again within the recheck
/// interval (plus the cache lifetime) and ends once the session is gone.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn an_event_stream_under_a_session_ends_when_the_session_does() {
    with_fixture(
        |c| {
            c.auth.as_mut().unwrap().session_cache_ttl = Duration::from_millis(200);
            c.transport.stream_recheck = Duration::from_millis(400);
        },
        async |f| {
            let bearer = format!("Bearer {}", f.pat);
            let call = |method: &str, path: String, body: Value, key: &str, etag: Option<String>| {
                let mut builder = Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", &bearer)
                    .header("idempotency-key", key)
                    .header("content-type", "application/json");
                if let Some(etag) = etag {
                    builder = builder.header("if-match", etag);
                }
                builder.body(Body::from(body.to_string())).unwrap()
            };
            let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: watch, title: Watch}\nspec:\n  steps: [{id: work, title: Work}]\n";
            let r = send(&f.app, call("POST", format!("/v1/projects/{}/runbooks", f.project), json!({"source": source}), "rb", None)).await;
            assert_eq!(r.status, 201, "{}", r.body);
            let rb = r.body["runbook_id"].as_str().unwrap().to_owned();
            let etag = r.headers["etag"].to_str().unwrap().to_owned();
            let r = send(&f.app, call("POST", format!("/v1/runbooks/{rb}/versions"), json!({}), "v1", Some(etag))).await;
            assert_eq!(r.status, 201, "{}", r.body);
            let r = send(&f.app, call("POST", format!("/v1/projects/{}/executions", f.project), json!({"runbook":"watch"}), "run", None)).await;
            assert_eq!(r.status, 201, "{}", r.body);
            let exe = r.body["id"].as_str().unwrap().to_owned();

            f.files.put_session("tok-owner", "ba_owner", "owner@example.test");
            let response = f
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/v1/executions/{exe}/events"))
                        .header("cookie", cookie("tok-owner"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let mut frames = response.into_body().into_data_stream();
            // Valid session: the stream stays open across several rechecks.
            let open_until = tokio::time::Instant::now() + Duration::from_millis(1500);
            while let Ok(frame) = tokio::time::timeout_at(open_until, frames.next()).await {
                assert!(frame.is_some(), "stream ended while the session was valid");
            }
            f.files.remove_session("tok-owner");
            let ended = tokio::time::timeout(Duration::from_secs(5), async {
                while frames.next().await.is_some() {}
            })
            .await;
            assert!(ended.is_ok(), "stream outlived its session");
        },
    )
    .await;
}

/// D45 invitation routes: owner or admin only, admins never invite owners,
/// retries answer the same invitation, members are refused, revocation is
/// once, and accepted invitations show as accepted.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn invitations_are_managed_by_owners_and_admins() {
    with_fixture(|_| {}, async |f| {
        let path = format!("/v1/orgs/{}/invitations", f.org);
        let bearer = format!("Bearer {}", f.pat);
        let request = |method: &str, path: &str, auth: (&str, &str), body: Option<Value>, key: Option<&str>| {
            let mut builder = Request::builder().method(method).uri(path).header(auth.0, auth.1);
            if auth.0 == "cookie" {
                builder = builder.header("origin", ORIGIN).header("x-runbook-csrf", "1");
            }
            if let Some(key) = key {
                builder = builder.header("idempotency-key", key);
            }
            match body {
                Some(body) => builder
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
                None => builder.body(Body::empty()).unwrap(),
            }
        };
        let owner = ("authorization", bearer.as_str());
        let invite = |email: &str, role: &str| json!({"email": email, "role": role, "projects": [{"project": "default", "role": "operator"}]});
        let r = send(&f.app, request("POST", &path, owner, Some(invite("Admin@Example.test", "admin")), None)).await;
        assert_eq!(r.status, 428, "{}", r.body);
        let r = send(&f.app, request("POST", &path, owner, Some(invite("Admin@Example.test", "admin")), Some("a"))).await;
        assert_eq!(r.status, 201, "{}", r.body);
        assert_eq!(r.body["email"], "admin@example.test");
        assert_eq!(r.body["state"], "pending");
        assert_eq!(r.body["projects"][0]["project"], f.project.as_str());
        let first = r.body["id"].clone();
        let r = send(&f.app, request("POST", &path, owner, Some(invite("admin@example.test", "admin")), Some("a2"))).await;
        assert_eq!((r.status, &r.body["id"]), (201, &first), "a retry answers the same invitation");
        let r = send(&f.app, request("POST", &path, owner, Some(invite("admin@example.test", "member")), Some("b"))).await;
        assert_eq!((r.status, r.body["code"].as_str()), (409, Some("invitation.exists")));
        let r = send(&f.app, request("POST", &path, owner, Some(invite("owner@example.test", "member")), Some("c"))).await;
        assert_eq!((r.status, r.body["code"].as_str()), (409, Some("invitation.already_member")));
        let r = send(&f.app, request("POST", &path, owner, Some(json!({"email":"x@example.test","projects":[{"project":"nope","role":"viewer"}]})), Some("d"))).await;
        assert_eq!(r.status, 404);
        let r = send(&f.app, request("POST", &path, owner, Some(json!({"email":"not-an-email"})), Some("e"))).await;
        assert_eq!(r.status, 422);
        let r = send(&f.app, request("POST", &path, owner, Some(json!({"email":"m@example.test"})), Some("f"))).await;
        assert_eq!((r.status, &r.body["role"]), (201, &json!("member")));

        // The invited admin signs in; as admin they invite members, not owners.
        f.files.put_session("tok-admin", "ba_admin", "admin@example.test");
        assert_eq!(f.me("tok-admin").await.body["org"]["role"], "admin");
        let admin = ("cookie", &*cookie("tok-admin"));
        let admin = (admin.0, admin.1);
        let r = send(&f.app, request("POST", &path, admin, Some(invite("boss@example.test", "owner")), Some("g"))).await;
        assert_eq!((r.status, r.body["code"].as_str()), (403, Some("auth.forbidden")));
        let r = send(&f.app, request("POST", &path, admin, Some(invite("helper@example.test", "member")), Some("h"))).await;
        assert_eq!(r.status, 201, "{}", r.body);
        let helper = r.body["id"].as_str().unwrap().to_owned();
        // A member can neither list nor invite.
        f.files.put_session("tok-m", "ba_m", "m@example.test");
        assert_eq!(f.me("tok-m").await.body["org"]["role"], "member");
        let member = ("cookie", &*cookie("tok-m"));
        let member = (member.0, member.1);
        assert_eq!(send(&f.app, request("GET", &path, member, None, None)).await.status, 403);
        assert_eq!(send(&f.app, request("POST", &path, member, Some(invite("z@example.test", "member")), Some("i"))).await.status, 403);

        // Listing: newest first, accepted ones marked, paged.
        let r = send(&f.app, request("GET", &path, owner, None, None)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let states: Vec<_> = r.body["items"].as_array().unwrap().iter().map(|i| (i["email"].as_str().unwrap().to_owned(), i["state"].as_str().unwrap().to_owned())).collect();
        assert_eq!(states.len(), 3);
        assert_eq!(states[0], ("helper@example.test".into(), "pending".into()));
        assert!(states.contains(&("admin@example.test".into(), "accepted".into())));
        assert!(states.contains(&("m@example.test".into(), "accepted".into())));
        let r = send(&f.app, request("GET", &format!("{path}?limit=2"), owner, None, None)).await;
        assert_eq!(r.body["items"].as_array().unwrap().len(), 2);
        let cursor = r.body["next_cursor"].as_str().unwrap().to_owned();
        let r = send(&f.app, request("GET", &format!("{path}?limit=2&cursor={cursor}"), owner, None, None)).await;
        assert_eq!(r.body["items"].as_array().unwrap().len(), 1);
        assert!(r.body["next_cursor"].is_null());

        // Revocation: once, owner or admin, this org only.
        let one = format!("{path}/{helper}");
        assert_eq!(send(&f.app, request("DELETE", &one, member, None, None)).await.status, 403);
        assert_eq!(send(&f.app, request("DELETE", &one, admin, None, None)).await.status, 204);
        assert_eq!(send(&f.app, request("DELETE", &one, owner, None, None)).await.status, 404);
        let foreign = format!("/v1/orgs/org_{:026}/invitations", 9);
        assert_eq!(send(&f.app, request("GET", &foreign, owner, None, None)).await.status, 404);
        assert_eq!(send(&f.app, request("PATCH", &path, owner, None, None)).await.status, 405);
    })
    .await;
}

// ------------------------------------------------------------------ AUTH3

fn device_post(path: &str, body: Value, peer: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(PeerAddr(peer.parse().unwrap()));
    }
    request
}
fn approve(token: &str, body: Value, key: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/device/approve")
        .header("cookie", cookie(token))
        .header("origin", ORIGIN)
        .header("x-runbook-csrf", "1")
        .header("idempotency-key", key)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
async fn poll(app: &Router, device_code: &str) -> Reply {
    send(
        app,
        device_post(
            "/v1/device/token",
            json!({"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code":device_code}),
            None,
        ),
    )
    .await
}

fn lookup(token: &str, user_code: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/device/lookup")
        .header("cookie", cookie(token))
        .header("origin", ORIGIN)
        .header("x-runbook-csrf", "1")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "user_code": user_code }).to_string()))
        .unwrap()
}

/// Sign-in gate fix 3: the approval page can show who asked: the client
/// name (marked unverified), the address that asked (the client a trusted
/// proxy names) and the scopes asked for. Only a signed-in person with
/// CSRF reads it, and a wrong code counts as a miss.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_approval_page_sees_who_asked() {
    with_fixture(
        |c| {
            c.transport.trusted_proxies =
                runbook_server::client_ip::TrustedProxies::parse("10.0.0.0/8").unwrap();
        },
        async |f| {
            let mut request = device_post(
                "/v1/device/code",
                json!({"client_name":"runbook CLI on laptop","scopes":["runbooks:read","executions:start"]}),
                Some("10.0.0.5:4000"),
            );
            request
                .headers_mut()
                .insert("x-forwarded-for", "198.51.100.44".parse().unwrap());
            let r = send(&f.app, request).await;
            assert_eq!(r.status, 200, "{}", r.body);
            let user_code = r.body["user_code"].as_str().unwrap().to_owned();
            f.files
                .put_session("tok-owner", "ba_owner", "owner@example.test");
            let r = send(&f.app, lookup("tok-owner", &user_code.to_lowercase())).await;
            assert_eq!(r.status, 200, "{}", r.body);
            assert_eq!(r.body["user_code"], user_code.as_str());
            assert_eq!(r.body["client_name"], "runbook CLI on laptop");
            assert_eq!(r.body["client_name_verified"], false);
            assert_eq!(r.body["requester_ip"], "198.51.100.44");
            assert_eq!(
                r.body["requested_scopes"],
                json!(["runbooks:read", "executions:start"])
            );
            assert!(r.body["expires_at_ms"].as_i64().unwrap() > r.body["created_at_ms"].as_i64().unwrap());
            assert_eq!(r.headers["cache-control"], "no-store");
            // Never by a bearer token, never without CSRF.
            let by_bearer = Request::builder()
                .method("POST")
                .uri("/v1/device/lookup")
                .header("authorization", format!("Bearer {}", f.pat))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "user_code": user_code }).to_string()))
                .unwrap();
            assert_eq!(send(&f.app, by_bearer).await.status, 403);
            let mut no_csrf = lookup("tok-owner", &user_code);
            no_csrf.headers_mut().remove("x-runbook-csrf");
            assert_eq!(send(&f.app, no_csrf).await.body["code"], "auth.csrf");
            // A decided code is no longer shown; that and unknown codes are
            // misses: five a minute, then 429.
            let r = send(
                &f.app,
                approve("tok-owner", json!({"user_code":user_code,"approve":false}), "deny"),
            )
            .await;
            assert_eq!(r.status, 204, "{}", r.body);
            assert_eq!(send(&f.app, lookup("tok-owner", &user_code)).await.status, 404);
            for n in 0..4 {
                assert_eq!(send(&f.app, lookup("tok-owner", "ZZZZ-ZZZZ")).await.status, 404, "miss {n}");
            }
            assert_eq!(send(&f.app, lookup("tok-owner", "ZZZZ-ZZZZ")).await.status, 429);
        },
    )
    .await;
}

/// D47: the grant end to end. Pending, slow_down, approval only by a session
/// with CSRF, scopes narrowed to the request and the approver, one token for
/// one poll, an ordinary listed PAT, and no reuse of either code.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn device_login_issues_a_narrowed_personal_token() {
    with_fixture(|_| {}, async |f| {
        let r = f.get("/.well-known/runbook", &[]).await;
        assert_eq!(r.body["auth"]["device_authorization_endpoint"], format!("{ORIGIN}/v1/device/code"));
        let r = send(&f.app, device_post("/v1/device/code", json!({"client_name":"runbook CLI on laptop","scopes":["runbooks:read","executions:read","tokens:manage"]}), None)).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let device_code = r.body["device_code"].as_str().unwrap().to_owned();
        let user_code = r.body["user_code"].as_str().unwrap().to_owned();
        assert_eq!(user_code.len(), 9);
        assert_eq!(&user_code[4..5], "-");
        assert!(user_code.replace('-', "").bytes().all(|b| b"BCDFGHJKLMNPQRSTVWXZ23456789".contains(&b)), "{user_code}");
        assert_eq!(r.body["verification_uri"], format!("{ORIGIN}/device"));
        assert_eq!(r.body["verification_uri_complete"], format!("{ORIGIN}/device?user_code={user_code}"));
        assert_eq!((r.body["expires_in"].as_u64(), r.body["interval"].as_u64()), (Some(600), Some(5)));
        assert_eq!(r.headers["cache-control"], "no-store");

        let r = poll(&f.app, &device_code).await;
        assert_eq!((r.status, r.body["code"].as_str(), r.body["error"].as_str()), (400, Some("device.authorization_pending"), Some("authorization_pending")));
        let r = poll(&f.app, &device_code).await;
        assert_eq!((r.body["code"].as_str(), r.body["interval"].as_u64()), (Some("device.slow_down"), Some(10)));

        // Only a browser session with CSRF approves.
        f.files.put_session("tok-owner", "ba_owner", "owner@example.test");
        let by_bearer = Request::builder()
            .method("POST")
            .uri("/v1/device/approve")
            .header("authorization", format!("Bearer {}", f.pat))
            .header("idempotency-key", "x")
            .header("content-type", "application/json")
            .body(Body::from(json!({"user_code":user_code,"approve":true}).to_string()))
            .unwrap();
        assert_eq!(send(&f.app, by_bearer).await.status, 403);
        let no_csrf = Request::builder()
            .method("POST")
            .uri("/v1/device/approve")
            .header("cookie", cookie("tok-owner"))
            .header("idempotency-key", "x")
            .header("content-type", "application/json")
            .body(Body::from(json!({"user_code":user_code,"approve":true}).to_string()))
            .unwrap();
        assert_eq!(send(&f.app, no_csrf).await.body["code"], "auth.csrf");
        let mut no_key = approve("tok-owner", json!({"user_code":user_code,"approve":true}), "k");
        no_key.headers_mut().remove("idempotency-key");
        assert_eq!(send(&f.app, no_key).await.status, 428);
        // Never wider than the CLI asked.
        let r = send(&f.app, approve("tok-owner", json!({"user_code":user_code,"approve":true,"scopes":["runbooks:write"]}), "k1")).await;
        assert_eq!(r.status, 422, "{}", r.body);
        // Narrowed further by the approver: one scope, one project.
        let typed = user_code.replace('-', "").to_lowercase();
        let r = send(&f.app, approve("tok-owner", json!({"user_code":typed,"approve":true,"scopes":["runbooks:read"],"projects":["default"]}), "k2")).await;
        assert_eq!(r.status, 204, "{}", r.body);
        let r = send(&f.app, approve("tok-owner", json!({"user_code":user_code,"approve":false}), "k3")).await;
        assert_eq!(r.status, 404, "a decided code is gone");

        let r = poll(&f.app, &device_code).await;
        assert_eq!(r.status, 200, "{}", r.body);
        let pat = r.body["access_token"].as_str().unwrap().to_owned();
        assert!(pat.starts_with("trb_pat_"));
        assert_eq!(r.body["token_type"], "bearer");
        assert_eq!(r.body["scope"], "runbooks:read");
        let days = r.body["expires_in"].as_u64().unwrap() / 86_400;
        assert!((89..=90).contains(&days), "{days}");
        let token_id = r.body["token_id"].as_str().unwrap().to_owned();
        // Exactly one poll gets it.
        let again = poll(&f.app, &device_code).await;
        assert_eq!(again.body["code"], "device.expired_token");
        // An ordinary PAT: who it is, what it may do, listed and revocable.
        let me = f.get("/v1/me", &[("authorization", &format!("Bearer {pat}"))]).await;
        assert_eq!(me.status, 200, "{}", me.body);
        assert_eq!(me.body["credential"]["kind"], "personal");
        assert_eq!(me.body["credential"]["actor_kind"], "human");
        assert_eq!(me.body["credential"]["scopes"], json!(["runbooks:read"]));
        assert_eq!(me.body["credential"]["projects"], json!([f.project.as_str()]));
        assert_eq!(me.body["user"]["id"], f.owner.as_str());
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let expires = me.body["credential"]["expires_at_ms"].as_i64().unwrap() - wall;
        assert!((89 * 86_400_000..=90 * 86_400_000 + 60_000).contains(&expires), "{expires}");
        let tokens = f.get("/v1/tokens", &[("authorization", &format!("Bearer {}", f.pat))]).await;
        let listed = tokens.body["items"].as_array().unwrap().iter().find(|t| t["id"] == token_id.as_str()).cloned().unwrap();
        assert_eq!(listed["name"], "Device login: runbook CLI on laptop");
        assert!(!tokens.body.to_string().contains(&pat));
    })
    .await;
}

/// D47: denial, expiry, unknown codes, approver-bounded scopes, the per-IP
/// code limit and the approval-miss limit.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn device_codes_deny_expire_and_are_rate_limited() {
    use runbook_core::{
        credentials::{Scope, secret_hash},
        device_codes::{DeviceCodeRecord, DeviceCodeState, DeviceCodeStore},
    };
    with_fixture(
        |_| {},
        async |f| {
            let code =
                |peer: Option<&str>| send(&f.app, device_post("/v1/device/code", json!({}), peer));
            f.files
                .put_session("tok-owner", "ba_owner", "owner@example.test");

            // Denied.
            let r = code(None).await;
            let (device, user) = (
                r.body["device_code"].as_str().unwrap().to_owned(),
                r.body["user_code"].as_str().unwrap().to_owned(),
            );
            assert_eq!(
                send(
                    &f.app,
                    approve("tok-owner", json!({"user_code":user,"approve":false}), "d")
                )
                .await
                .status,
                204
            );
            let r = poll(&f.app, &device).await;
            assert_eq!(
                (r.body["code"].as_str(), r.body["error"].as_str()),
                (Some("device.access_denied"), Some("access_denied"))
            );

            // Expired: nothing approves it and polls say expired_token.
            let credentials = runbook_store_postgres::PostgresCredentials::new(f.db.clone());
            let directory = PostgresDirectory::new(f.db.clone());
            let now = directory.current_time_ms().await.unwrap();
            credentials
                .insert_device_code(DeviceCodeRecord {
                    device_code_hash: secret_hash("old-device-code"),
                    user_code_hash: secret_hash("BCDFGHJK"),
                    client_name: "old".into(),
                    requester_ip: None,
                    requested_scopes: [Scope::RunbooksRead].into(),
                    created_at_ms: now - 700_000,
                    expires_at_ms: now - 100_000,
                    interval_ms: 5_000,
                    last_polled_at_ms: None,
                    state: DeviceCodeState::Pending,
                })
                .await
                .unwrap();
            assert_eq!(
                send(
                    &f.app,
                    approve(
                        "tok-owner",
                        json!({"user_code":"BCDF-GHJK","approve":true}),
                        "e"
                    )
                )
                .await
                .status,
                404
            );
            assert_eq!(
                poll(&f.app, "old-device-code").await.body["code"],
                "device.expired_token"
            );
            assert_eq!(
                poll(&f.app, "never-issued").await.body["code"],
                "device.expired_token"
            );
            let r = send(
                &f.app,
                device_post(
                    "/v1/device/token",
                    json!({"grant_type":"password","device_code":device}),
                    None,
                ),
            )
            .await;
            assert_eq!(
                (r.status, r.body["error"].as_str()),
                (400, Some("unsupported_grant_type"))
            );
            let r = send(
                &f.app,
                device_post("/v1/device/code", json!({"scopes":["root"]}), None),
            )
            .await;
            assert_eq!(r.status, 422);

            // The approver's roles bound the token: a project viewer cannot
            // grant write, and by default grants only what they hold.
            directory
                .create_invitation(
                    &f.org,
                    InvitationRecord {
                        id: format!("inv_{:026}", 7).parse().unwrap(),
                        org_id: f.org.clone(),
                        email: "viewer@example.test".into(),
                        role: OrgRole::Member,
                        projects: vec![(f.project.clone(), ProjectRole::Viewer)],
                        invited_by: f.owner.clone(),
                        created_at_ms: now,
                        expires_at_ms: now + INVITATION_TTL_MS,
                        state: InvitationState::Pending,
                        accepted_by: None,
                        closed_at_ms: None,
                        name: None,
                    },
                )
                .await
                .unwrap();
            f.files
                .put_session("tok-viewer", "ba_viewer", "viewer@example.test");
            let r = send(
                &f.app,
                device_post(
                    "/v1/device/code",
                    json!({"scopes":["runbooks:read","runbooks:write"]}),
                    None,
                ),
            )
            .await;
            let (device, user) = (
                r.body["device_code"].as_str().unwrap().to_owned(),
                r.body["user_code"].as_str().unwrap().to_owned(),
            );
            let r = send(
                &f.app,
                approve(
                    "tok-viewer",
                    json!({"user_code":user,"approve":true,"scopes":["runbooks:write"]}),
                    "v1",
                ),
            )
            .await;
            assert_eq!(
                (r.status, r.body["code"].as_str()),
                (403, Some("auth.forbidden"))
            );
            assert_eq!(
                send(
                    &f.app,
                    approve("tok-viewer", json!({"user_code":user,"approve":true}), "v2")
                )
                .await
                .status,
                204
            );
            let r = poll(&f.app, &device).await;
            assert_eq!(r.body["scope"], "runbooks:read", "{}", r.body);

            // Per-IP code limit (20 an hour); another address is unaffected.
            for n in 0..20 {
                assert_eq!(
                    code(Some("198.51.100.7:1000")).await.status,
                    200,
                    "code {n}"
                );
            }
            let r = code(Some("198.51.100.7:1001")).await;
            assert_eq!(
                (r.status, r.body["code"].as_str()),
                (429, Some("limit.rate"))
            );
            assert!(r.body["retry_after"].as_u64().unwrap() <= 3600);
            assert_eq!(code(Some("198.51.100.8:1000")).await.status, 200);

            // Guessing user codes: five misses a minute (one was the expired
            // code above), then 429.
            for n in 0..4 {
                let r = send(
                    &f.app,
                    approve(
                        "tok-owner",
                        json!({"user_code":"ZZZZ-ZZZZ","approve":true}),
                        &format!("m{n}"),
                    ),
                )
                .await;
                assert_eq!(r.status, 404, "miss {n}");
            }
            let r = send(
                &f.app,
                approve(
                    "tok-owner",
                    json!({"user_code":"ZZZZ-ZZZZ","approve":true}),
                    "m5",
                ),
            )
            .await;
            assert_eq!(r.status, 429);
        },
    )
    .await;
}
