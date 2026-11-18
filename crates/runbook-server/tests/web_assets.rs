//! B2: runbook-server serves the built web UI from RUNBOOK_WEB_DIR on the
//! API's origin (A4: single origin, no CORS). No PostgreSQL needed: unknown
//! routes answer 404 before any store access.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response},
};
use runbook_server::{
    config::ServerConfig,
    http::{Worker, router},
};
use runbook_store_postgres::DatabaseConfig;
use std::path::{Path, PathBuf};
use tower::ServiceExt;

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const INDEX: &str = "<!doctype html><title>The Runbook</title><div id=root></div>";

fn config(web_dir: Option<PathBuf>) -> ServerConfig {
    let mut config = ServerConfig::new(
        DatabaseConfig::from_values(Some("postgres://unused@localhost/unused".into()), None)
            .unwrap(),
        "https://api.example.test",
        "127.0.0.1:8080",
    )
    .unwrap();
    config.web_dir = web_dir;
    config
}

/// A throwaway dist/ tree beside a secret file that must never be served.
struct Site {
    base: PathBuf,
}
impl Site {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "runbook-web-assets-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dist = base.join("dist");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::create_dir_all(dist.join("icons")).unwrap();
        std::fs::write(dist.join("index.html"), INDEX).unwrap();
        std::fs::write(dist.join("assets/index-AbC123.js"), "console.log(1)").unwrap();
        std::fs::write(dist.join("assets/index-AbC123.css"), "body{}").unwrap();
        std::fs::write(dist.join("assets/runbook-XyZ789.wasm"), b"\0asm\x01\0\0\0").unwrap();
        std::fs::write(dist.join("assets/inter-latin-400.woff2"), b"wOF2").unwrap();
        std::fs::write(dist.join("assets/inter-latin-400.woff"), b"wOFF").unwrap();
        std::fs::write(dist.join("manifest.webmanifest"), "{}").unwrap();
        std::fs::write(dist.join("icons/icon.svg"), "<svg/>").unwrap();
        std::fs::write(dist.join("icons/icon-192.png"), b"\x89PNG").unwrap();
        std::fs::write(base.join("secret.txt"), "TOP-SECRET").unwrap();
        Self { base }
    }
    fn dist(&self) -> PathBuf {
        self.base.join("dist")
    }
}
impl Drop for Site {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn app(web_dir: Option<&Path>) -> (Worker, Router) {
    let worker = Worker::start(config(web_dir.map(Path::to_path_buf))).unwrap();
    let app = router(worker.client());
    (worker, app)
}

async fn call(app: &Router, method: &str, uri: &str) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body(response: Response<Body>) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

fn header<'a>(response: &'a Response<Body>, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .to_str()
        .unwrap()
}

fn assert_no_cors(response: &Response<Body>, what: &str) {
    for name in response.headers().keys() {
        assert!(
            !name.as_str().starts_with("access-control-"),
            "{what} sent {name}"
        );
    }
}

async fn assert_problem_404(app: &Router, method: &str, uri: &str) {
    let response = call(app, method, uri).await;
    assert_eq!(response.status(), 404, "{method} {uri}");
    assert_eq!(
        header(&response, "content-type"),
        "application/problem+json",
        "{method} {uri}"
    );
    assert_eq!(header(&response, "x-content-type-options"), "nosniff");
    assert_no_cors(&response, uri);
    let text = body(response).await;
    assert!(!text.contains("TOP-SECRET") && !text.contains("<div id=root>"));
}

#[tokio::test]
async fn index_and_client_routes_serve_the_spa_with_security_headers() {
    let site = Site::new("spa");
    let (_worker, app) = app(Some(&site.dist()));
    for uri in [
        "/",
        "/index.html",
        "/tasks",
        "/runs/exe_00000000000000000000000001",
        "/projects/default/runbooks/post-deploy?tab=versions",
        "/icons/",
        "/v1x",
    ] {
        let response = call(&app, "GET", uri).await;
        assert_eq!(response.status(), 200, "{uri}");
        assert_eq!(
            header(&response, "content-type"),
            "text/html; charset=utf-8"
        );
        assert_eq!(header(&response, "cache-control"), "no-store");
        assert_eq!(header(&response, "content-security-policy"), CSP);
        assert_eq!(header(&response, "referrer-policy"), "no-referrer");
        assert_eq!(header(&response, "x-content-type-options"), "nosniff");
        assert_no_cors(&response, uri);
        assert_eq!(body(response).await, INDEX, "{uri}");
    }
}

#[tokio::test]
async fn files_carry_content_types_and_hashed_assets_are_immutable() {
    let site = Site::new("types");
    let (_worker, app) = app(Some(&site.dist()));
    for (uri, content_type, cache) in [
        (
            "/assets/index-AbC123.js",
            "text/javascript; charset=utf-8",
            "public, max-age=31536000, immutable",
        ),
        (
            "/assets/index-AbC123.css",
            "text/css; charset=utf-8",
            "public, max-age=31536000, immutable",
        ),
        (
            "/assets/runbook-XyZ789.wasm",
            "application/wasm",
            "public, max-age=31536000, immutable",
        ),
        (
            "/assets/inter-latin-400.woff2",
            "font/woff2",
            "public, max-age=31536000, immutable",
        ),
        (
            "/assets/inter-latin-400.woff",
            "font/woff",
            "public, max-age=31536000, immutable",
        ),
        (
            "/manifest.webmanifest",
            "application/manifest+json",
            "no-cache",
        ),
        ("/icons/icon.svg", "image/svg+xml", "no-cache"),
        ("/icons/icon-192.png", "image/png", "no-cache"),
    ] {
        let response = call(&app, "GET", uri).await;
        assert_eq!(response.status(), 200, "{uri}");
        assert_eq!(header(&response, "content-type"), content_type, "{uri}");
        assert_eq!(header(&response, "cache-control"), cache, "{uri}");
        assert_eq!(header(&response, "x-content-type-options"), "nosniff");
        assert_no_cors(&response, uri);
        let expected = std::fs::read(site.dist().join(&uri[1..])).unwrap();
        assert_eq!(
            to_bytes(response.into_body(), 1 << 20).await.unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn head_returns_headers_without_a_body() {
    let site = Site::new("head");
    let (_worker, app) = app(Some(&site.dist()));
    for (uri, length, content_type) in [
        ("/", INDEX.len(), "text/html; charset=utf-8"),
        ("/tasks", INDEX.len(), "text/html; charset=utf-8"),
        ("/assets/runbook-XyZ789.wasm", 8, "application/wasm"),
    ] {
        let response = call(&app, "HEAD", uri).await;
        assert_eq!(response.status(), 200, "{uri}");
        assert_eq!(header(&response, "content-type"), content_type);
        assert_eq!(header(&response, "content-length"), length.to_string());
        assert_no_cors(&response, uri);
        assert!(body(response).await.is_empty(), "{uri}");
    }
    let response = call(&app, "HEAD", "/").await;
    assert_eq!(header(&response, "content-security-policy"), CSP);
}

#[tokio::test]
async fn missing_assets_are_404_not_the_spa() {
    let site = Site::new("missing");
    let (_worker, app) = app(Some(&site.dist()));
    for uri in [
        "/assets/index-gone.js",
        "/assets/runbook-old.wasm",
        "/assets/",
        "/assets",
    ] {
        assert_problem_404(&app, "GET", uri).await;
    }
    assert_problem_404(&app, "HEAD", "/assets/index-gone.js").await;
}

#[tokio::test]
async fn api_paths_never_fall_through_to_files() {
    let site = Site::new("api");
    // A file shadowing each reserved prefix must never be served.
    for dir in [
        "v1",
        ".well-known",
        "mcp",
        "api/auth",
        "hooks",
        "healthz",
        "internal",
        "bootstrap",
    ] {
        std::fs::create_dir_all(site.dist().join(dir)).unwrap();
        std::fs::write(site.dist().join(dir).join("x"), "TOP-SECRET").unwrap();
    }
    std::fs::write(site.dist().join("mcp.html"), "TOP-SECRET").unwrap();
    let (_worker, app) = app(Some(&site.dist()));
    for uri in [
        "/v1/does-not-exist",
        "/v1/x",
        "/v1",
        "/.well-known/x",
        "/.well-known/nothing-here",
        "/mcp/x",
        "/api/auth",
        "/api/auth/x",
        "/api/x",
        "/hooks",
        "/hooks/x",
        "/healthz",
        "/healthz/x",
        "/internal/users",
        "/internal/x",
        "/bootstrap",
        "/bootstrap/x",
        "/%76%31/x",
    ] {
        assert_problem_404(&app, "GET", uri).await;
    }
    // MCP1: `/mcp` is the remote MCP endpoint, which has no GET stream.
    let response = call(&app, "GET", "/mcp").await;
    assert_eq!(response.status(), 405);
    assert_eq!(header(&response, "content-type"), "application/json");
    assert_no_cors(&response, "/mcp");
    let text = body(response).await;
    assert!(!text.contains("TOP-SECRET") && !text.contains("<div id=root>"));
    // Discovery still answers from the API.
    let response = call(&app, "GET", "/.well-known/runbook").await;
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), "application/json");
    assert_no_cors(&response, "discovery");
    // Other methods on client routes are the API's 404, never the SPA.
    assert_problem_404(&app, "POST", "/tasks").await;
    assert_problem_404(&app, "DELETE", "/").await;
}

#[tokio::test]
async fn path_traversal_is_rejected() {
    let site = Site::new("traversal");
    let (_worker, app) = app(Some(&site.dist()));
    for uri in [
        "/../secret.txt",
        "/assets/../../secret.txt",
        "/icons/../../secret.txt",
        "/%2e%2e/secret.txt",
        "/%2E%2E/secret.txt",
        "/assets/%2e%2e/%2e%2e/secret.txt",
        "/.%2e/secret.txt",
        "/assets%2f..%2f..%2fsecret.txt",
        "/%2e%2e%2fsecret.txt",
        "//etc/passwd",
        "/%2fetc%2fpasswd",
        &format!("/{}", site.base.join("secret.txt").display()),
        "/..%5csecret.txt",
        "/%5c..%5csecret.txt",
        "/assets\\..\\..\\secret.txt",
        "/icons/./icon.svg",
        "/index.html%00.png",
        "/%ff",
        "/%zz",
        "/.hidden",
    ] {
        let response = call(&app, "GET", uri).await;
        assert_eq!(response.status(), 404, "{uri}");
        assert_eq!(
            header(&response, "content-type"),
            "application/problem+json",
            "{uri}"
        );
        assert_no_cors(&response, uri);
        let text = body(response).await;
        assert!(!text.contains("TOP-SECRET"), "{uri}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_escaping_the_web_dir_are_not_followed() {
    let site = Site::new("symlink");
    let dist = site.dist();
    std::os::unix::fs::symlink(site.base.join("secret.txt"), dist.join("leak.txt")).unwrap();
    std::os::unix::fs::symlink(&site.base, dist.join("assets/up")).unwrap();
    std::os::unix::fs::symlink(&site.base, dist.join("outside")).unwrap();
    // A link that stays inside the directory is fine.
    std::os::unix::fs::symlink(dist.join("icons/icon.svg"), dist.join("logo.svg")).unwrap();
    let (_worker, app) = app(Some(&dist));
    for uri in ["/leak.txt", "/assets/up/secret.txt", "/outside/secret.txt"] {
        let response = call(&app, "GET", uri).await;
        assert_eq!(response.status(), 404, "{uri}");
        assert!(!body(response).await.contains("TOP-SECRET"), "{uri}");
    }
    let response = call(&app, "GET", "/logo.svg").await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await, "<svg/>");
}

#[tokio::test]
async fn without_a_web_dir_the_root_is_the_api_404() {
    let (_worker, app) = app(None);
    for uri in ["/", "/index.html", "/tasks", "/assets/index-AbC123.js"] {
        assert_problem_404(&app, "GET", uri).await;
        let response = call(&app, "GET", uri).await;
        assert!(response.headers().get("content-security-policy").is_none());
        assert_eq!(header(&response, "cache-control"), "no-store");
        assert!(response.headers().contains_key("x-request-id"));
    }
}

#[test]
fn a_web_dir_without_index_html_is_refused_at_start() {
    let site = Site::new("noindex");
    std::fs::remove_file(site.dist().join("index.html")).unwrap();
    let error = Worker::start(config(Some(site.dist()))).err().unwrap();
    assert_eq!(error.0, "config.invalid_web_dir");
    let error = Worker::start(config(Some(site.base.join("nope"))))
        .err()
        .unwrap();
    assert_eq!(error.0, "config.invalid_web_dir");
}

async fn call_with(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> Response<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// U25: every file carries a strong ETag and Last-Modified; a conditional
/// GET or HEAD that matches answers 304 with the same caching and security
/// headers and no body, so a revalidating browser never re-downloads.
#[tokio::test]
async fn validators_answer_conditional_requests_with_304() {
    let site = Site::new("validators");
    let (_worker, app) = app(Some(&site.dist()));
    for (uri, cache) in [
        ("/icons/icon.svg", "no-cache"),
        ("/manifest.webmanifest", "no-cache"),
        (
            "/assets/index-AbC123.js",
            "public, max-age=31536000, immutable",
        ),
        ("/tasks", "no-store"),
    ] {
        let first = call(&app, "GET", uri).await;
        assert_eq!(first.status(), 200, "{uri}");
        let etag = header(&first, "etag").to_owned();
        assert!(
            etag.starts_with('"') && etag.ends_with('"') && etag.len() > 2,
            "{uri}: a strong ETag, got {etag}"
        );
        let modified = header(&first, "last-modified").to_owned();
        assert!(modified.ends_with(" GMT"), "{uri}: {modified}");
        for method in ["GET", "HEAD"] {
            for value in [
                etag.clone(),
                format!("W/{etag}"),
                format!("\"other\", {etag}"),
                "*".to_owned(),
            ] {
                let again = call_with(&app, method, uri, &[("if-none-match", &value)]).await;
                assert_eq!(again.status(), 304, "{method} {uri} If-None-Match {value}");
                assert_eq!(header(&again, "etag"), etag, "{uri}");
                assert_eq!(header(&again, "last-modified"), modified, "{uri}");
                assert_eq!(header(&again, "cache-control"), cache, "{uri}");
                assert_eq!(header(&again, "content-security-policy"), CSP);
                assert_eq!(header(&again, "referrer-policy"), "no-referrer");
                assert_eq!(header(&again, "x-content-type-options"), "nosniff");
                assert_no_cors(&again, uri);
                assert!(body(again).await.is_empty(), "{uri}");
            }
            let since = call_with(&app, method, uri, &[("if-modified-since", &modified)]).await;
            assert_eq!(since.status(), 304, "{method} {uri} If-Modified-Since");
        }
        // A different tag, or an older date, is a full response.
        let other = call_with(&app, "GET", uri, &[("if-none-match", "\"nope\"")]).await;
        assert_eq!(other.status(), 200, "{uri}");
        assert_eq!(header(&other, "etag"), etag);
        assert!(!body(other).await.is_empty());
        let old = call_with(
            &app,
            "GET",
            uri,
            &[("if-modified-since", "Thu, 01 Jan 1970 00:00:01 GMT")],
        )
        .await;
        assert_eq!(old.status(), 200, "{uri}");
        // If-None-Match wins over If-Modified-Since (RFC 9110 13.2.2).
        let both = call_with(
            &app,
            "GET",
            uri,
            &[
                ("if-none-match", "\"nope\""),
                ("if-modified-since", &modified),
            ],
        )
        .await;
        assert_eq!(both.status(), 200, "{uri}");
    }
    // Same bytes, same tag; different files, different tags.
    let js = call(&app, "GET", "/assets/index-AbC123.js").await;
    let css = call(&app, "GET", "/assets/index-AbC123.css").await;
    assert_ne!(header(&js, "etag"), header(&css, "etag"));
}

/// U25: bytes are kept between requests, but a file replaced on disk is
/// served fresh with a new tag, and its old tag no longer matches.
#[tokio::test]
async fn a_changed_file_gets_a_new_etag_and_body() {
    let site = Site::new("changed");
    let (_worker, app) = app(Some(&site.dist()));
    let first = call(&app, "GET", "/icons/icon.svg").await;
    let etag = header(&first, "etag").to_owned();
    assert_eq!(body(first).await, "<svg/>");
    std::fs::write(
        site.dist().join("icons/icon.svg"),
        "<svg viewBox=\"0 0 1 1\"/>",
    )
    .unwrap();
    let second = call_with(&app, "GET", "/icons/icon.svg", &[("if-none-match", &etag)]).await;
    assert_eq!(second.status(), 200);
    assert_ne!(header(&second, "etag"), etag);
    assert_eq!(body(second).await, "<svg viewBox=\"0 0 1 1\"/>");
}

/// U25: conditional headers never change what is refused or missing.
#[tokio::test]
async fn conditional_headers_keep_refusals() {
    let site = Site::new("conditional-refusals");
    let (_worker, app) = app(Some(&site.dist()));
    for uri in [
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/assets/index-gone.js",
        "/v1/x",
    ] {
        let response = call_with(&app, "GET", uri, &[("if-none-match", "*")]).await;
        assert_eq!(response.status(), 404, "{uri}");
        assert!(!body(response).await.contains("TOP-SECRET"), "{uri}");
    }
}
