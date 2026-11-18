//! B2: the built web UI (RUNBOOK_WEB_DIR, normally `web/dist`) served on the
//! API's origin, so the browser needs no CORS (A4).
//!
//! - API prefixes ([`RESERVED`]) never fall through to files: those requests,
//!   and every method other than GET and HEAD, go to the API dispatch, which
//!   answers unknown paths with its problem+json 404.
//! - A path that names a file under the directory serves it. Any other path is
//!   a client route and serves `index.html`, except under `assets/`, where a
//!   missing file is the API's 404 (a stale hashed bundle must not load HTML).
//! - Hashed `assets/*` are immutable for a year; `index.html` is `no-store`;
//!   other files revalidate. Every response carries the CSP and
//!   `Referrer-Policy`; the router adds `X-Content-Type-Options` (F96).
//! - U25: every file has a strong `ETag` (SHA-256 of its bytes) and a
//!   `Last-Modified`; a matching `If-None-Match` (or, without one,
//!   `If-Modified-Since`) answers 304 with the same headers and no body. A
//!   file's bytes are read once and kept while its size and modification time
//!   stay the same (the directory is the image's immutable build output, so
//!   the cache is bounded by it); the path checks run on every request.
//! - The path is percent-decoded once and refused (the API's 404) when any
//!   segment is `.`/`..`, hidden, empty (`//`), or holds a backslash, a colon
//!   or a control character. The file must resolve, symlinks followed, inside
//!   the canonical directory; one that escapes it is refused, never replaced
//!   by `index.html`.
use crate::Error;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderValue, Method, Response, header},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use runbook_api::web::percent_decode;
pub use runbook_api::web::{CONTENT_SECURITY_POLICY, IMMUTABLE, RESERVED, is_reserved};

#[derive(Debug)]
pub struct WebAssets {
    /// Canonical, so a resolved file can be checked with `starts_with`.
    root: PathBuf,
    /// Served files by resolved path, valid while size and mtime match.
    cache: Mutex<HashMap<PathBuf, Arc<Cached>>>,
}

/// One file's bytes and validators.
#[derive(Debug)]
struct Cached {
    len: u64,
    modified: Option<SystemTime>,
    bytes: Bytes,
    etag: HeaderValue,
    last_modified: Option<HeaderValue>,
}

enum Lookup {
    File(Arc<Cached>),
    Missing,
    Refused,
}

impl WebAssets {
    /// The directory must exist and hold `index.html`; otherwise the server
    /// refuses to start (`config.invalid_web_dir`) rather than serve 404s.
    pub fn open(dir: &Path) -> Result<Self, Error> {
        let root = dir
            .canonicalize()
            .map_err(|_| Error("config.invalid_web_dir"))?;
        if !root.is_dir() || !root.join("index.html").is_file() {
            return Err(Error("config.invalid_web_dir"));
        }
        Ok(Self {
            root,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The static response for `path`, or None when the API answers it.
    /// `headers` are the request's, for conditional requests (U25).
    pub async fn respond(
        self: &Arc<Self>,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
    ) -> Option<Response<Body>> {
        let head = match *method {
            Method::GET => false,
            Method::HEAD => true,
            _ => return None,
        };
        if is_reserved(path) {
            return None;
        }
        let decoded = percent_decode(path)?;
        if is_reserved(&decoded) {
            return None;
        }
        let segments = segments(&decoded)?;
        let asset = segments.first().is_some_and(|s| *s == "assets");
        let name = segments.last().map_or("index.html", |s| s).to_owned();
        let relative: PathBuf = segments.iter().collect();
        let this = Arc::clone(self);
        let (name, file) = tokio::task::spawn_blocking(move || {
            if relative.as_os_str().is_empty() {
                return this.index();
            }
            match this.lookup(&relative) {
                Lookup::File(file) => Some((name, file)),
                Lookup::Refused => None,
                Lookup::Missing if asset => None,
                Lookup::Missing => this.index(),
            }
        })
        .await
        .ok()??;
        Some(file_response(&name, asset, &file, head, headers))
    }

    fn index(&self) -> Option<(String, Arc<Cached>)> {
        match self.read(&self.root.join("index.html")) {
            Lookup::File(file) => Some(("index.html".to_owned(), file)),
            _ => None,
        }
    }

    /// The cached file at `resolved` (already checked to be inside the
    /// root), re-read when its size or modification time changed.
    fn read(&self, resolved: &Path) -> Lookup {
        let meta = match std::fs::metadata(resolved) {
            Ok(meta) if meta.is_file() => meta,
            // A directory (`/icons/`) is a client route, never a listing.
            Ok(_) => return Lookup::Missing,
            Err(_) => return Lookup::Refused,
        };
        let modified = meta.modified().ok();
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache
            .get(resolved)
            .filter(|c| c.len == meta.len() && c.modified == modified)
        {
            return Lookup::File(Arc::clone(hit));
        }
        drop(cache);
        let Ok(bytes) = std::fs::read(resolved) else {
            return Lookup::Refused;
        };
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let hex: String = digest.as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let file = Arc::new(Cached {
            len: meta.len(),
            modified,
            etag: HeaderValue::from_str(&format!("\"{hex}\"")).expect("hex is a header value"),
            last_modified: modified
                .and_then(|m| HeaderValue::from_str(&httpdate::fmt_http_date(m)).ok()),
            bytes: Bytes::from(bytes),
        });
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(resolved.to_path_buf(), Arc::clone(&file));
        Lookup::File(file)
    }

    fn lookup(&self, relative: &Path) -> Lookup {
        let joined = self.root.join(relative);
        let resolved = match joined.canonicalize() {
            Ok(resolved) => resolved,
            // A dangling link is refused; anything else absent is a route.
            Err(_) if joined.symlink_metadata().is_ok() => return Lookup::Refused,
            Err(_) => return Lookup::Missing,
        };
        if !resolved.starts_with(&self.root) {
            return Lookup::Refused;
        }
        self.read(&resolved)
    }
}

/// The non-empty segments of a decoded path, or None when any is unsafe.
fn segments(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    let last = parts.len() - 1;
    let mut out = Vec::with_capacity(parts.len());
    for (i, part) in parts.into_iter().enumerate() {
        if part.is_empty() {
            // Only a trailing slash (`/`, `/icons/`) may leave one empty.
            if i == last {
                continue;
            }
            return None;
        }
        if part.starts_with('.')
            || part
                .chars()
                .any(|c| matches!(c, '\\' | ':') || c.is_control())
        {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// Percent-decodes once. Invalid escapes or non-UTF-8 results are None.
fn content_type(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// RFC 9110 13.1.2/13.1.3 for GET and HEAD: If-None-Match (weak
/// comparison, `*` matches any file) decides when present; otherwise
/// If-Modified-Since at one-second resolution.
fn not_modified(file: &Cached, headers: &HeaderMap) -> bool {
    let tag = file.etag.as_bytes();
    let inm: Vec<&HeaderValue> = headers.get_all(header::IF_NONE_MATCH).iter().collect();
    if !inm.is_empty() {
        return inm.iter().filter_map(|v| v.to_str().ok()).any(|v| {
            v.split(',').map(str::trim).any(|candidate| {
                candidate == "*"
                    || candidate.strip_prefix("W/").unwrap_or(candidate).as_bytes() == tag
            })
        });
    }
    let (Some(modified), Some(since)) = (
        file.modified,
        headers
            .get(header::IF_MODIFIED_SINCE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| httpdate::parse_http_date(v).ok()),
    ) else {
        return false;
    };
    // Last-Modified is sent in whole seconds; compare at that resolution.
    let seconds = |t: SystemTime| {
        t.duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .ok()
    };
    matches!((seconds(modified), seconds(since)), (Some(m), Some(s)) if m <= s)
}

fn file_response(
    name: &str,
    asset: bool,
    file: &Cached,
    head: bool,
    request: &HeaderMap,
) -> Response<Body> {
    let cache = if asset {
        IMMUTABLE
    } else if name == "index.html" {
        "no-store"
    } else {
        "no-cache"
    };
    let mut builder = Response::builder()
        .header(header::CACHE_CONTROL, cache)
        .header(header::ETAG, file.etag.clone())
        .header(header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY)
        .header(header::REFERRER_POLICY, "no-referrer");
    if let Some(modified) = &file.last_modified {
        builder = builder.header(header::LAST_MODIFIED, modified.clone());
    }
    if not_modified(file, request) {
        return builder
            .status(304)
            .body(Body::empty())
            .expect("static response");
    }
    builder
        .status(200)
        .header(header::CONTENT_TYPE, content_type(name))
        .header(header::CONTENT_LENGTH, file.bytes.len())
        .body(if head {
            Body::empty()
        } else {
            Body::from(file.bytes.clone())
        })
        .expect("static response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_prefixes_match_whole_segments() {
        for path in [
            "/v1",
            "/v1/",
            "/v1/meta",
            "/.well-known/runbook",
            "/mcp",
            "/api/auth/sign-in",
            "/hooks/x",
            "/healthz",
            "/internal/users",
            "/bootstrap",
        ] {
            assert!(is_reserved(path), "{path}");
        }
        for path in ["/", "/v1x", "/tasks", "/mcpx", "/apis", "/assets/x.js"] {
            assert!(!is_reserved(path), "{path}");
        }
    }

    #[test]
    fn unsafe_segments_are_refused() {
        assert_eq!(segments("/"), Some(vec![]));
        assert_eq!(segments("/icons/"), Some(vec!["icons"]));
        assert_eq!(
            segments("/assets/index-1.js"),
            Some(vec!["assets", "index-1.js"])
        );
        for path in [
            "",
            "relative",
            "/..",
            "/a/../b",
            "/./a",
            "//etc/passwd",
            "/a//b",
            "/.env",
            "/a\\b",
            "/c:/windows",
            "/a\0b",
            "/a\nb",
        ] {
            assert_eq!(segments(path), None, "{path:?}");
        }
    }

    #[test]
    fn percent_decoding_is_strict_and_single_pass() {
        assert_eq!(percent_decode("/%2e%2E/x").as_deref(), Some("/../x"));
        assert_eq!(percent_decode("/%252e").as_deref(), Some("/%2e"));
        assert_eq!(percent_decode("/caf%C3%A9").as_deref(), Some("/café"));
        for path in ["/%", "/%2", "/%zz", "/%+f", "/%ff", "/%c3"] {
            assert_eq!(percent_decode(path), None, "{path}");
        }
    }

    #[test]
    fn content_types_cover_the_built_ui() {
        for (name, expected) in [
            ("index.html", "text/html; charset=utf-8"),
            ("index-1.js", "text/javascript; charset=utf-8"),
            ("index-1.CSS", "text/css; charset=utf-8"),
            ("runbook-1.wasm", "application/wasm"),
            ("manifest.webmanifest", "application/manifest+json"),
            ("icon.svg", "image/svg+xml"),
            ("inter.woff2", "font/woff2"),
            ("inter.woff", "font/woff"),
            ("README", "application/octet-stream"),
        ] {
            assert_eq!(content_type(name), expected, "{name}");
        }
    }
}
