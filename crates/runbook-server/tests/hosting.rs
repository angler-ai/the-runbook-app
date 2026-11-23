//! HOST1, no database: single-port routing by host, the health and
//! readiness probes, and the graceful stop of the HTTP side.
use axum::{
    body::{Body, to_bytes},
    http::{Request, Response},
};
use runbook_server::{
    config::{HostMatch, ServerConfig, TransportLimits},
    http::{Worker, main_router, serve_until, shared_router, usercontent_router},
};
use runbook_store_postgres::DatabaseConfig;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const APP: &str = "https://runbooks.example.test";
const FILES: &str = "https://files.example.test";

/// A database that accepts connections and never answers.
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
/// A database nothing listens on.
fn absent_database() -> DatabaseConfig {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    DatabaseConfig::from_values(Some(format!("postgres://u@127.0.0.1:{port}/db")), None).unwrap()
}

fn shared_config(database: DatabaseConfig) -> ServerConfig {
    let mut config = ServerConfig::new(database, APP, "127.0.0.1:0").unwrap();
    config.uploads.signing_key = Some(runbook_core::uploads::signing::UploadSigningKey::new(
        [9; 32],
    ));
    config.uploads.usercontent_origin = Some(FILES.into());
    config.uploads.usercontent_listen = None;
    config
}

async fn get(app: &axum::Router, host: &str, path: &str) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn text(response: Response<Body>) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// D50 on one port: the user-content host answers only `/u/*`, with its
/// sandbox headers and no cookie; the app host never serves user content.
#[tokio::test]
async fn one_port_routes_user_content_by_host_and_keeps_the_origins_apart() {
    let config = shared_config(absent_database());
    assert!(config.uploads.shared_port());
    let worker = Worker::start(config.clone()).unwrap();
    let app = main_router(worker.client(), &config);

    // The app host: discovery, no sandbox.
    let r = get(&app, "runbooks.example.test", "/.well-known/runbook").await;
    assert_eq!(r.status(), 200);
    assert!(r.headers().get("content-security-policy").is_none());
    // The user-content host: no API, no discovery, sandboxed errors.
    for path in ["/.well-known/runbook", "/v1/meta", "/health/live", "/"] {
        let r = get(&app, "files.example.test", path).await;
        assert_eq!(r.status(), 404, "{path}");
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("sandbox"), "{path}: {csp}");
        assert!(r.headers().get("set-cookie").is_none());
    }
    // A signed-file URL with a bad signature is refused by the user-content
    // handler on its host (before any database call)...
    let file = "/u/exe_00000000000000000000000001/evd_00000000000000000000000001?expires=1&sig=bad";
    let r = get(&app, "files.example.test:443", file).await;
    assert_eq!(r.status(), 403);
    assert!(text(r).await.contains("upload.url"));
    // ...and is an unknown API route on the app host.
    let r = get(&app, "runbooks.example.test", file).await;
    assert_eq!(r.status(), 404);
    assert!(r.headers().get("content-security-policy").is_none());
    // A look-alike host is the app's, not the user content's.
    let r = get(&app, "files.example.test.evil", "/.well-known/runbook").await;
    assert_eq!(r.status(), 200);

    // With its own listener configured, the main port has no user content.
    let mut separate = config.clone();
    separate.uploads.usercontent_listen = Some("127.0.0.1:0".parse().unwrap());
    let app = main_router(worker.client(), &separate);
    let r = get(&app, "files.example.test", "/.well-known/runbook").await;
    assert_eq!(r.status(), 200);
    // The router itself, for a host-matched origin with a port.
    let local = shared_router(
        worker.client(),
        HostMatch::of("http://localhost:8081").unwrap(),
    );
    assert_eq!(
        get(&local, "localhost:8081", "/v1/meta").await.status(),
        404
    );
    assert_eq!(
        get(&local, "localhost:8080", "/.well-known/runbook")
            .await
            .status(),
        200
    );
}

#[tokio::test]
async fn hsts_uses_the_configured_origin_on_app_and_files_routes() {
    let mut config = shared_config(absent_database());
    let expected = "max-age=31536000; includeSubDomains";
    let secure = Worker::start(config.clone()).unwrap();
    let app = main_router(secure.client(), &config);
    for host in ["runbooks.example.test", "files.example.test"] {
        let response = get(&app, host, "/v1/not-found").await;
        assert_eq!(
            response
                .headers()
                .get("strict-transport-security")
                .and_then(|value| value.to_str().ok()),
            Some(expected)
        );
    }
    let files = usercontent_router(secure.client());
    assert_eq!(
        get(&files, "files.example.test", "/v1/not-found")
            .await
            .headers()["strict-transport-security"],
        expected
    );
    config.origin = "http://runbooks.example.test".into();
    let local = Worker::start(config.clone()).unwrap();
    let app = main_router(local.client(), &config);
    assert!(
        !get(&app, "runbooks.example.test", "/health/live")
            .await
            .headers()
            .contains_key("strict-transport-security")
    );
}

/// Liveness never touches the database; readiness does, and says
/// `stopping` once shutdown begins.
#[tokio::test]
async fn liveness_and_readiness_probes() {
    let config = shared_config(absent_database());
    let worker = Worker::start(config.clone()).unwrap();
    let app = main_router(worker.client(), &config);
    let r = get(&app, "10.0.0.7:8080", "/health/live").await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(text(r).await, r#"{"status":"live"}"#);
    let started = Instant::now();
    let r = get(&app, "10.0.0.7:8080", "/health/ready").await;
    assert_eq!(r.status(), 503);
    assert_eq!(text(r).await, r#"{"status":"database_unavailable"}"#);
    assert!(started.elapsed() < Duration::from_secs(5));
    let head = app
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/health/live")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    let post = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/health/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post.status(), 405);
    worker.begin_shutdown();
    assert!(worker.client().shutting_down());
    let r = get(&app, "10.0.0.7:8080", "/health/ready").await;
    assert_eq!(r.status(), 503);
    assert_eq!(text(r).await, r#"{"status":"stopping"}"#);
    // Liveness stays up while the instance drains.
    assert_eq!(
        get(&app, "10.0.0.7:8080", "/health/live").await.status(),
        200
    );
}

/// A request in flight when the stop comes still gets its answer; idle
/// keep-alive connections close; no new connection is accepted; and the
/// serve loop returns once every connection is closed.
#[tokio::test]
async fn a_stop_lets_in_flight_requests_finish_then_returns() {
    let mut config = ServerConfig::new(silent_database(), APP, "127.0.0.1:0").unwrap();
    // The request below waits on the silent database until this timeout.
    config.transport = TransportLimits {
        request_timeout: Duration::from_millis(1500),
        ..TransportLimits::default()
    };
    let limits = config.transport.clone();
    let worker = Worker::start(config.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn({
        let app = main_router(worker.client(), &config);
        async move { serve_until(listener, app, &limits, stopping).await }
    });

    // An idle keep-alive connection, and one with a request in flight.
    let mut idle = tokio::net::TcpStream::connect(addr).await.unwrap();
    idle.write_all(b"GET /health/live HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = vec![0; 4096];
    let n = idle.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    let mut busy = tokio::net::TcpStream::connect(addr).await.unwrap();
    let pat = runbook_core::credentials::encode_secret(
        runbook_core::credentials::TokenKind::Personal,
        [1; 32],
    );
    busy.write_all(
        format!("GET /v1/orgs HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {pat}\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let stopped_at = Instant::now();
    stop.send_replace(true);
    // The idle connection is closed without a response.
    let n = tokio::time::timeout(Duration::from_secs(2), idle.read(&mut buf))
        .await
        .expect("idle connection closed")
        .unwrap_or(0);
    assert_eq!(n, 0);
    // The in-flight request is answered (its timeout, here) before the
    // connection closes.
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), busy.read_to_end(&mut answer))
        .await
        .expect("in-flight request answered")
        .unwrap();
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
    assert!(answer.contains("server.timeout"), "{answer}");
    // The loop returns once both are closed, and no longer accepts.
    tokio::time::timeout(Duration::from_secs(3), serving)
        .await
        .expect("serve loop returned")
        .unwrap()
        .unwrap();
    assert!(stopped_at.elapsed() < Duration::from_secs(3));
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

/// HOST1 review: readiness is single-flight and cached for about a second,
/// so a burst of probes against a hung database opens one connection, not
/// one per probe, and every probe still answers within its timeout.
#[tokio::test]
async fn readiness_probes_share_one_ping() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            count.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    let config = ServerConfig::new(
        DatabaseConfig::from_values(Some(format!("postgres://u@127.0.0.1:{port}/db")), None)
            .unwrap(),
        APP,
        "127.0.0.1:0",
    )
    .unwrap();
    let worker = Worker::start(config.clone()).unwrap();
    let app = main_router(worker.client(), &config);
    let started = Instant::now();
    let statuses = futures_util::future::join_all(
        (0..20).map(|_| async { get(&app, "x", "/health/ready").await.status() }),
    )
    .await;
    assert!(statuses.iter().all(|s| *s == 503), "{statuses:?}");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "one ping for the burst");
    // Within the cache, no new ping.
    assert_eq!(get(&app, "x", "/health/ready").await.status(), 503);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}
