//! CF7 (B2): the web UI on the app origin from the Worker's static assets
//! (the `ASSETS` binding over `web/dist`), as the native server serves
//! `RUNBOOK_WEB_DIR` (`runbook-server/src/web_assets.rs`):
//! - paths the API owns (`runbook_api::web::RESERVED`, raw or decoded) and
//!   every method but GET and HEAD never reach the files;
//! - a path that names a file serves it; any other path is a client route
//!   and serves `index.html`, except under `assets/`, where a missing file
//!   stays a 404 (a stale hashed bundle must not load HTML);
//! - `index.html` is `no-store`, hashed `assets/*` immutable for a year,
//!   other files revalidate, and every response carries the CSP,
//!   `Referrer-Policy` and `nosniff`.
//!
//! The Worker serves assets too so HTTPS app responses share the same
//! transport security headers.
use runbook_api::web::{CONTENT_SECURITY_POLICY, IMMUTABLE, api_owns};
use worker::{Fetcher, Headers, Method, Request, RequestInit, Response, Result};

/// Whether the web UI answers `method` on `path` (the API answers the rest).
pub fn serves(method: &Method, path: &str) -> bool {
    matches!(method, Method::Get | Method::Head) && !api_owns(path)
}

async fn get(assets: &Fetcher, req: &Request, path: &str) -> Result<Response> {
    let url = req.url()?;
    let headers = Headers::new();
    for name in [
        "if-none-match",
        "if-modified-since",
        "accept",
        "accept-encoding",
    ] {
        if let Some(value) = req.headers().get(name)? {
            headers.set(name, &value)?;
        }
    }
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers);
    assets
        .fetch(
            format!("{}{path}", url.origin().ascii_serialization()),
            Some(init),
        )
        .await
}

/// Serve `req` from the static assets.
pub async fn serve(assets: Fetcher, req: Request) -> Result<Response> {
    let path = req.url()?.path().to_owned();
    let mut index = path == "/" || path == "/index.html";
    let mut response = get(&assets, &req, if index { "/index.html" } else { &path }).await?;
    if response.status_code() == 404 && !path.starts_with("/assets/") {
        index = true;
        response = get(&assets, &req, "/index.html").await?;
    }
    let headers = Headers::new();
    for (name, value) in response.headers().entries() {
        headers.append(&name, &value)?;
    }
    headers.set("content-security-policy", CONTENT_SECURITY_POLICY)?;
    headers.set("referrer-policy", "no-referrer")?;
    headers.set("x-content-type-options", "nosniff")?;
    let cache = if index {
        "no-store"
    } else if path.starts_with("/assets/") {
        IMMUTABLE
    } else {
        "no-cache"
    };
    headers.set("cache-control", cache)?;
    Ok(response.with_headers(headers))
}
