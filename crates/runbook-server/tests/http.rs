use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use runbook_server::{
    config::ServerConfig,
    http::{Worker, router},
};
use runbook_store_postgres::DatabaseConfig;
use tower::ServiceExt;
#[tokio::test]
async fn discovery_uses_configured_origin() {
    for origin in ["https://one.example.test", "https://two.example.test"] {
        let cfg = ServerConfig::new(
            DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None)
                .unwrap(),
            origin,
            "127.0.0.1:8080",
        )
        .unwrap();
        let worker = Worker::start(cfg).unwrap();
        let app = router(worker.client());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/.well-known/runbook")
                    .header("host", "evil.example")
                    .header("forwarded", "host=evil.example;proto=http")
                    .header("x-forwarded-host", "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.headers().contains_key("x-request-id"));
        assert_eq!(response.headers()["x-ratelimit-limit"], "unlimited");
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["api_base_url"], format!("{origin}/v1"));
        // MCP1: the worker-only MCP routes are unknown from outside, with
        // or without a bearer.
        for (method, path) in [
            ("GET", "/internal/users"),
            ("GET", "/internal/mcp/authenticate"),
            ("POST", "/internal/mcp/authenticate"),
            (
                "GET",
                "/internal/mcp/step-execution?sr=sr_00000000000000000000000001",
            ),
            ("POST", "/internal/mcp/step-execution"),
            ("GET", "/api/auth"),
            ("GET", "/hooks"),
            ("GET", "/bootstrap"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header(
                            "authorization",
                            format!("Bearer trb_pat_{}", "A".repeat(43)),
                        )
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 404);
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            assert!(response.headers().contains_key("x-request-id"));
        }
    }
}
#[test]
fn transport_is_send_without_send_store_bounds() {
    fn check<T: Send + Sync>() {}
    check::<runbook_server::http::Client>();
}
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn directory_replay_and_vars_survive_transport_restart() {
    use futures_util::FutureExt;
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result=std::panic::AssertUnwindSafe(async {
        let config=ServerConfig::new(db.config().clone(),"https://api.example.test","127.0.0.1:8080").unwrap();
        let issued=runbook_server::bootstrap::run(&config,&Default::default()).await.unwrap();let mut bytes=Vec::new();issued.write_once(&mut bytes).unwrap();let issued:serde_json::Value=serde_json::from_slice(&bytes).unwrap();
        let token=issued["pat"].as_str().unwrap();let path=format!("/v1/orgs/{}/users",issued["org_id"].as_str().unwrap());
        let make=|path:&str|Request::builder().method("POST").uri(path).header("authorization",format!("Bearer {token}")).header("content-type","application/json").header("idempotency-key","repeat").body(Body::from(r#"{"name":"Alice"}"#)).unwrap();
        let worker=Worker::start(config.clone()).unwrap();let app=router(worker.client());
        let (a,b)=tokio::join!(app.clone().oneshot(make(&path)),app.clone().oneshot(make(&path)));
        let (a,b)=(a.unwrap(),b.unwrap());assert_eq!(a.status(),201);assert_eq!(b.status(),201);
        let a=to_bytes(a.into_body(),65536).await.unwrap();let b=to_bytes(b.into_body(),65536).await.unwrap();assert!(a==b);
        drop(app);drop(worker);
        let worker=Worker::start(config.clone()).unwrap();let app=router(worker.client());
        let replay=app.clone().oneshot(make(&path)).await.unwrap();assert_eq!(replay.status(),201);assert!(to_bytes(replay.into_body(),65536).await.unwrap()==a);
        let vars=format!("/v1/projects/{}/vars",issued["project_id"].as_str().unwrap());
        let response=app.clone().oneshot(Request::builder().uri(&vars).header("authorization",format!("Bearer {token}")).body(Body::empty()).unwrap()).await.unwrap();assert_eq!(response.status(),200);let etag=response.headers()["etag"].clone();
        let put=||Request::builder().method("PUT").uri(&vars).header("authorization",format!("Bearer {token}")).header("content-type","application/json").header("if-match",etag.clone()).body(Body::from(r#"{"declarations":{"region":{"type":"string"}},"values":{"region":"us-east"}}"#)).unwrap();
        let (a,b)=tokio::join!(app.clone().oneshot(put()),app.clone().oneshot(put()));
        let mut statuses=vec![a.unwrap().status().as_u16(),b.unwrap().status().as_u16()];statuses.sort();assert_eq!(statuses,vec![200,412]);
        let invalid=app.clone().oneshot(Request::builder().method("POST").uri(&path).header("authorization",format!("Bearer {token}")).header("content-type","text/plain").body(Body::from("not json")).unwrap()).await.unwrap();assert_eq!(invalid.status(),415);assert_eq!(invalid.headers()["content-type"],"application/problem+json");assert!(invalid.headers().contains_key("x-request-id"));
        drop(app);drop(worker);
    }).catch_unwind().await;
    db.finish(result).await;
}

fn offline_config() -> ServerConfig {
    ServerConfig::new(
        DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None)
            .unwrap(),
        "https://api.example.test",
        "127.0.0.1:8080",
    )
    .unwrap()
}

/// F52: only the exact events route streams; a signal named `events` is an
/// ordinary actor POST (here refused for its missing credential, not 405).
#[tokio::test]
async fn a_signal_named_events_reaches_the_signal_route() {
    let worker = Worker::start(offline_config()).unwrap();
    let app = router(worker.client());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/executions/exe_00000000000000000000000001/signals/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["code"], "auth.required");
}

/// F96: every response, success, problem or stream refusal, says nosniff.
#[tokio::test]
async fn every_response_disables_content_sniffing() {
    let worker = Worker::start(offline_config()).unwrap();
    let app = router(worker.client());
    for (method, uri) in [
        ("GET", "/.well-known/runbook"),
        ("GET", "/no/such/route"),
        (
            "GET",
            "/v1/executions/exe_00000000000000000000000001/events",
        ),
        (
            "POST",
            "/v1/executions/exe_00000000000000000000000001/events",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .map(|v| v.to_str().unwrap()),
            Some("nosniff"),
            "{method} {uri}"
        );
    }
}

// ---- F45/F54: the transport over a real socket ----

use runbook_server::config::TransportLimits;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A database address that accepts connections and never answers, so every
/// store call hangs until the request timeout.
fn silent_database() -> DatabaseConfig {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    DatabaseConfig::from_values(Some(format!("postgres://u@127.0.0.1:{port}/db")), None).unwrap()
}

fn config_with(database: DatabaseConfig, transport: TransportLimits) -> ServerConfig {
    let mut config =
        ServerConfig::new(database, "https://api.example.test", "127.0.0.1:0").unwrap();
    config.transport = transport;
    config
}

/// Serve on an ephemeral port; returns the address and the worker to keep.
async fn listen(config: ServerConfig) -> (std::net::SocketAddr, Worker) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let limits = config.transport.clone();
    let worker = Worker::start(config).unwrap();
    let app = router(worker.client());
    tokio::spawn(async move { runbook_server::http::serve_on(listener, app, &limits).await });
    (addr, worker)
}

/// Write raw bytes and read the status line, or None when the server closed
/// the socket without answering within `wait`.
async fn exchange(stream: &mut tokio::net::TcpStream, bytes: &[u8], wait: Duration) -> Option<u16> {
    stream.write_all(bytes).await.unwrap();
    let mut buf = vec![0; 4096];
    let n = tokio::time::timeout(wait, stream.read(&mut buf))
        .await
        .expect("the server neither answered nor closed the connection")
        .unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]);
    text.strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
}

/// F45: a 2 MiB body is refused from its headers: tokenless with 401, and
/// over the non-upload cap with 413, without waiting for the body.
#[tokio::test]
async fn oversized_and_tokenless_bodies_are_refused_before_reading() {
    let (addr, _worker) = listen(config_with(silent_database(), TransportLimits::default())).await;
    let length = 2 * 1_048_576 + 65_537;
    for (auth, expected) in [("", 401), ("authorization: Bearer trb_pat_x\r\n", 413)] {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "POST /v1/orgs/org_00000000000000000000000001/users HTTP/1.1\r\nhost: x\r\n{auth}content-type: application/json\r\ncontent-length: {length}\r\n\r\n{{\"name\""
        );
        // Only a few body bytes are sent; a server that buffers first hangs.
        let status = exchange(&mut stream, head.as_bytes(), Duration::from_secs(2)).await;
        assert_eq!(status, Some(expected), "{auth:?}");
    }
}

/// F45: headers that never finish do not hold a socket.
#[tokio::test]
async fn a_slow_header_is_cut_off() {
    let limits = TransportLimits {
        header_read_timeout: Duration::from_millis(200),
        ..Default::default()
    };
    let (addr, _worker) = listen(config_with(silent_database(), limits)).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let started = std::time::Instant::now();
    let status = exchange(
        &mut stream,
        b"GET /.well-known/runbook HTTP/1.1\r\nhost: x\r\n",
        Duration::from_secs(3),
    )
    .await;
    assert!(matches!(status, None | Some(408)), "{status:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// F45: connections beyond the cap wait until one closes.
#[tokio::test]
async fn connections_beyond_the_cap_wait_for_a_free_slot() {
    let limits = TransportLimits {
        max_connections: 1,
        ..Default::default()
    };
    let (addr, _worker) = listen(config_with(silent_database(), limits)).await;
    let idle = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut waiting = tokio::net::TcpStream::connect(addr).await.unwrap();
    waiting
        .write_all(b"GET /.well-known/runbook HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = vec![0; 4096];
    assert!(
        tokio::time::timeout(Duration::from_millis(300), waiting.read(&mut buf))
            .await
            .is_err(),
        "served while the only slot was held"
    );
    drop(idle);
    let n = tokio::time::timeout(Duration::from_secs(2), waiting.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
}

async fn problem(app: &axum::Router, request: Request<Body>) -> (u16, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    (status, v["code"].as_str().unwrap_or_default().to_owned())
}

fn pat() -> String {
    runbook_core::credentials::encode_secret(
        runbook_core::credentials::TokenKind::Personal,
        [1; 32],
    )
}

/// F54: two Authorization headers authenticate nobody.
#[tokio::test]
async fn duplicate_authorization_headers_are_refused() {
    let worker = Worker::start(offline_config()).unwrap();
    let app = router(worker.client());
    let request = Request::builder()
        .uri("/v1/orgs")
        .header("authorization", format!("Bearer {}", pat()))
        .header("authorization", format!("Bearer {}", pat()))
        .body(Body::empty())
        .unwrap();
    assert_eq!(problem(&app, request).await, (401, "auth.required".into()));
}

/// F54: a request the store never answers times out with 503, a full queue
/// answers 503 server.busy at once, and the stream cap answers
/// 503 stream.capacity.
#[tokio::test]
async fn timeout_busy_and_stream_capacity_answer_503() {
    let limits = TransportLimits {
        request_timeout: Duration::from_millis(400),
        max_in_flight: 1,
        queue_depth: 1,
        max_streams: 1,
        ..Default::default()
    };
    let worker = Worker::start(config_with(silent_database(), limits)).unwrap();
    let app = router(worker.client());
    let get = |uri: &str| {
        Request::builder()
            .uri(uri)
            .header("authorization", format!("Bearer {}", pat()))
            .body(Body::empty())
            .unwrap()
    };
    // One request runs (and hangs), one waits for the slot, one is queued;
    // they are staggered so the executor takes each before the next.
    let after = |ms: u64| {
        let (app, request) = (app.clone(), get("/v1/orgs"));
        async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            problem(&app, request).await
        }
    };
    let started = std::time::Instant::now();
    let (a, b, c, d) = tokio::join!(after(0), after(50), after(100), after(150));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the configured timeout applies"
    );
    assert_eq!(d, (503, "server.busy".into()));
    for timed_out in [a, b, c] {
        assert_eq!(timed_out, (503, "server.timeout".into()));
    }
    let events = "/v1/executions/exe_00000000000000000000000001/events";
    let first = problem(&app, get(events));
    let second = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        problem(&app, get(events)).await
    };
    let (_, second) = tokio::join!(first, second);
    assert_eq!(second, (503, "stream.capacity".into()));
}

/// Later-plans rule 5: every operation the contract marks planned answers
/// 501 request.not_implemented before authentication or the body, with or
/// without a credential (T0 freeze: device, evidence, archives; memory
/// shipped with MEM3, invitations with AUTH2). No database is touched.
#[tokio::test]
async fn planned_operations_answer_501() {
    let cfg = ServerConfig::new(
        DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None)
            .unwrap(),
        "https://one.example.test",
        "127.0.0.1:8080",
    )
    .unwrap();
    let worker = Worker::start(cfg).unwrap();
    let app = router(worker.client());
    // ARC3 shipped the last T0 routes (archives); the guard stays for the
    // next planned ones. The archive routes now authenticate first.
    for path in [
        "/v1/runbooks/rb_00000000000000000000000001/archives",
        "/v1/runbooks/rb_00000000000000000000000001/archives/2026-11-12",
        "/v1/executions/exe_00000000000000000000000001/archive",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{path}");
    }
    assert!(
        !runbook_model::PLANNED_ROUTES
            .iter()
            .any(|r| r.path.contains("/memory") || r.path.contains("/invitations"))
    );
    for route in runbook_model::PLANNED_ROUTES {
        let uri = route
            .path
            .replace("{rb}", "rb_00000000000000000000000001")
            .replace("{p}", "prj_00000000000000000000000001")
            .replace("{mem}", "mem_00000000000000000000000001")
            .replace("{org}", "org_00000000000000000000000001")
            .replace("{inv}", "inv_00000000000000000000000001")
            .replace("{sr}", "sr_00000000000000000000000001")
            .replace("{evd}", "evd_00000000000000000000000001")
            .replace("{date}", "2026-11-12");
        for bearer in [None, Some("Bearer trb_pat_unknown")] {
            let mut request = Request::builder().method(route.method).uri(&uri);
            if let Some(bearer) = bearer {
                request = request.header("authorization", bearer);
            }
            let body = if route.request.is_some() {
                request = request.header("content-type", "application/json");
                Body::from("{}")
            } else {
                Body::empty()
            };
            let response = app
                .clone()
                .oneshot(request.body(body).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), 501, "{} {uri}", route.method);
            assert_eq!(
                response.headers()["content-type"],
                "application/problem+json"
            );
            let body = to_bytes(response.into_body(), 65536).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["code"], "request.not_implemented");
        }
    }
    // Another method on a planned path is not planned: the usual 404.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/device/code")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}
