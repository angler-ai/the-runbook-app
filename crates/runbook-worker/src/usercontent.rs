//! CF7: the user-content origin on Workers (D49–D50), the port of the
//! native user-content listener (`runbook-server/src/http.rs`). The api
//! Worker answers it when a request's origin is `RUNBOOK_USERCONTENT_ORIGIN`
//! and then serves only `/u/*`:
//! - `PUT /u/{exe}/{evd}?…&sig`: the signed upload, cheapest checks first
//!   (the URL, its signature and expiry by the wall clock, the headers
//!   against the signed size and type, the run's tenant and the slot's state
//!   by the store clock), and only then the body;
//! - `GET`/`HEAD /u/{exe}/{evd}[/thumbnail]?…&sig`: a finalized file,
//!   streamed from R2 in ranges;
//! - `OPTIONS`: the PUT preflight, for the app origin only.
//!
//! Everything else is 404. No cookie or credential is read, none is set, and
//! every response carries the shared safety headers
//! (`runbook_api::evidence::USERCONTENT_HEADERS`).
use super::{CfBackends, WorkersHost, WorkersObjects, ensure_schema};
use runbook_api::{
    evidence::{
        UploadSettings, UserContentReply as Reply, UserContentRequest, content_disposition,
        served_content_type, single_query, url_error, usercontent,
    },
    http::encode,
    problem::Response as ApiResponse,
};
use runbook_core::uploads::signing::{self, UPLOAD_PATH_PREFIX, UploadSigningKey};
use worker::{Env, Method, Request, Response, Result, Url};

/// The variables (or secrets) that turn uploads on.
pub const SIGNING_KEY_VAR: &str = "RUNBOOK_UPLOAD_SIGNING_KEY";
pub const ORIGIN_VAR: &str = "RUNBOOK_USERCONTENT_ORIGIN";
/// Bytes per range read while streaming a file (as natively).
const FILE_CHUNK_BYTES: u64 = 1_048_576;
/// The largest body a signed PUT may carry when the limits set none: the
/// request-body ceiling of a Free or Pro zone (100 MB). It is the zone's
/// plan that sets it, not the Workers plan (Business 200 MB, Enterprise
/// more). The limits this build advertises (`settings`) always set a
/// smaller file size, since the body is read whole into the isolate's
/// 128 MB (CF12).
const MAX_UPLOAD_BYTES: u64 = 100 * 1_048_576;

fn value(env: &Env, name: &str) -> Option<String> {
    env.secret(name)
        .ok()
        .map(|s| s.to_string())
        .or_else(|| env.var(name).ok().map(|v| v.to_string()))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// An origin's serialized form (scheme, host, effective port), or `None`.
pub fn normalize(origin: &str) -> Option<String> {
    let url = Url::parse(origin.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}

fn signing_key(env: &Env) -> Option<UploadSigningKey> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value(env, SIGNING_KEY_VAR)?)
        .ok()?;
    Some(UploadSigningKey::new(<[u8; 32]>::try_from(bytes).ok()?))
}

/// The user-content origin, when configured and not the app origin (D50).
pub fn origin(env: &Env, app: &str) -> Option<String> {
    let content = normalize(&value(env, ORIGIN_VAR)?)?;
    (Some(&content) != normalize(app).as_ref()).then_some(content)
}

/// The upload settings from the Worker's configuration. Uploads are on with
/// a signing key and a separate user-content origin. Images are refused
/// until the Worker has a pipeline that strips their metadata (D51): the
/// limits list no image types, so discovery offers none.
pub fn settings(env: &Env, app: &str) -> UploadSettings {
    let mut limits = runbook_core::uploads::limits::cloudflare();
    limits.image_types.clear();
    let signing_key = signing_key(env);
    let usercontent_origin = origin(env, app);
    UploadSettings {
        enabled: signing_key.is_some() && usercontent_origin.is_some(),
        signing_key,
        usercontent_origin,
        limits,
        sniff: Some(runbook_core::uploads::sniff::sniff),
    }
}

/// Whether `req` is addressed to the user-content origin.
pub fn addressed(env: &Env, app: &str, req: &Request) -> bool {
    let Some(content) = origin(env, app) else {
        return false;
    };
    req.url()
        .ok()
        .is_some_and(|url| url.origin().ascii_serialization() == content)
}

fn wall_clock_ms() -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let now = js_sys::Date::now() as i64;
    now
}

fn error(status: u16, code: &str, id: &str) -> Result<Response> {
    super::to_response(ApiResponse::error(status, code, id))
}

/// Serve one request on the user-content origin.
pub async fn serve(req: Request, env: Env, app: String) -> Result<Response> {
    let cors = req
        .headers()
        .get("origin")?
        .is_some_and(|o| normalize(&o).is_some_and(|o| Some(o) == normalize(&app)));
    let method = req.method();
    let mut response = dispatch(req, &env, &app, cors).await?;
    let headers = response.headers_mut();
    headers.delete("set-cookie")?;
    for (name, value) in runbook_api::evidence::USERCONTENT_HEADERS {
        headers.set(name, value)?;
    }
    if !headers.has("content-disposition")? {
        headers.set("content-disposition", "attachment")?;
    }
    if !headers.has("cache-control")? {
        headers.set("cache-control", "no-store")?;
    }
    if cors && method == Method::Put {
        allow_origin(&mut response, &app)?;
    }
    Ok(response)
}

fn allow_origin(response: &mut Response, app: &str) -> Result<()> {
    let headers = response.headers_mut();
    headers.set(
        "access-control-allow-origin",
        &normalize(app).unwrap_or_default(),
    )?;
    headers.set("vary", "Origin")
}

async fn dispatch(req: Request, env: &Env, app: &str, cors: bool) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_owned();
    let query = url.query().map(str::to_owned);
    let id = request_id();
    if !path.starts_with(UPLOAD_PATH_PREFIX) {
        return error(404, "resource.not_found", &id);
    }
    match req.method() {
        Method::Options => {
            let wants_put = req
                .headers()
                .get("access-control-request-method")?
                .is_some_and(|m| m == "PUT");
            if !cors || !wants_put {
                return error(404, "resource.not_found", &id);
            }
            let mut response = Response::empty()?.with_status(204);
            let headers = response.headers_mut();
            headers.set("access-control-allow-methods", "PUT")?;
            headers.set("access-control-allow-headers", "content-type")?;
            headers.set("access-control-max-age", "600")?;
            allow_origin(&mut response, app)?;
            Ok(response)
        }
        Method::Put => put(req, env, app, &path, query.as_deref()).await,
        Method::Get | Method::Head => {
            let head = req.method() == Method::Head;
            get(env, app, &path, query.as_deref(), head).await
        }
        _ => error(404, "resource.not_found", &id),
    }
}

/// A request ID, as natively (`req_` and a ULID).
fn request_id() -> String {
    let mut random = [0u8; 10];
    crate::notify::crypto_fill(&mut random)
        .ok()
        .and_then(|()| runbook_api::backends::encode_id("req_", wall_clock_ms(), random))
        .unwrap_or_else(|| "req_unavailable".into())
}

/// The backends a request needs, and its ID.
async fn backends(env: &Env) -> Result<(CfBackends<WorkersHost>, String)> {
    ensure_schema(env).await?;
    let host = WorkersHost::new(env, WorkersObjects::remote(env)?)?;
    Ok((CfBackends::new(host), request_id()))
}

async fn put(
    mut req: Request,
    env: &Env,
    app: &str,
    path: &str,
    query: Option<&str>,
) -> Result<Response> {
    let uploads = settings(env, app);
    let (backends, id) = backends(env).await?;
    let Some(key) = uploads.signing_key.as_ref() else {
        return error(503, "uploads.not_configured", &id);
    };
    let Some(query) = single_query(query) else {
        return error(403, "upload.url_invalid", &id);
    };
    let signed = match signing::parse_upload(path, &query) {
        Ok(signed) => signed,
        Err(e) => {
            let e = url_error(e);
            return error(e.status, e.code, &id);
        }
    };
    if let Err(e) = signing::verify(key, &signed.grant(), &signed.signature, wall_clock_ms()) {
        let e = url_error(e);
        return error(e.status, e.code, &id);
    }
    let content_type = req.headers().get("content-type")?;
    let declared = content_type
        .as_deref()
        .map(|v| v.split(';').next().unwrap_or("").trim());
    if declared != Some(signed.content_type.as_str()) {
        return error(422, "upload.type_mismatch", &id);
    }
    let length = match req.headers().get("content-length")? {
        None => None,
        Some(v) => match v.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => return error(400, "request.invalid_length", &id),
        },
    };
    let ceiling = [uploads.limits.file_bytes, uploads.limits.image_bytes]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(MAX_UPLOAD_BYTES)
        .min(MAX_UPLOAD_BYTES);
    if signed.size > ceiling {
        return error(413, "request.body_too_large", &id);
    }
    if length.is_some_and(|n| n != signed.size) {
        return error(422, "upload.size_mismatch", &id);
    }
    match usercontent(
        &backends,
        &uploads,
        UserContentRequest::PutCheck(signed.clone()),
    )
    .await
    {
        Reply::Ready => {}
        other => return reply(&backends, &uploads, other, false, &id).await,
    }
    // CF12 review round 1: never read past the signed size.
    let crate::body::Capped::Body(bytes) = crate::body::read(&mut req, signed.size).await? else {
        return error(422, "upload.size_mismatch", &id);
    };
    if bytes.len() as u64 != signed.size {
        return error(422, "upload.size_mismatch", &id);
    }
    let stored = usercontent(
        &backends,
        &uploads,
        UserContentRequest::Put {
            signed,
            content_type,
            bytes,
        },
    )
    .await;
    reply(&backends, &uploads, stored, false, &id).await
}

async fn get(
    env: &Env,
    app: &str,
    path: &str,
    query: Option<&str>,
    head: bool,
) -> Result<Response> {
    let uploads = settings(env, app);
    let (backends, id) = backends(env).await?;
    let Some(key) = uploads.signing_key.as_ref() else {
        return error(503, "uploads.not_configured", &id);
    };
    let Some(query) = single_query(query) else {
        return error(403, "upload.url_invalid", &id);
    };
    let signed = match signing::parse_read(path, &query) {
        Ok(signed) => signed,
        Err(e) => {
            let e = url_error(e);
            return error(e.status, e.code, &id);
        }
    };
    if let Err(e) = signing::verify_read(key, &signed.grant(), &signed.signature, wall_clock_ms()) {
        let e = url_error(e);
        return error(e.status, e.code, &id);
    }
    let found = usercontent(&backends, &uploads, UserContentRequest::Get(signed)).await;
    reply(&backends, &uploads, found, head, &id).await
}

async fn reply(
    backends: &CfBackends<WorkersHost>,
    uploads: &UploadSettings,
    reply: Reply,
    head: bool,
    id: &str,
) -> Result<Response> {
    match reply {
        Reply::Stored => {
            let encoded = encode(ApiResponse::json(204, serde_json::Value::Null, id));
            Ok(Response::empty()?.with_status(encoded.status))
        }
        Reply::Problem { status, code } => error(status, code, id),
        Reply::Ready | Reply::Chunk(_) => error(500, "server.response", id),
        Reply::File {
            content_type,
            name,
            inline,
            size,
            scope,
            key,
            max_age_s,
        } => {
            let mut response = if head || size == 0 {
                Response::empty()?
            } else {
                // Streamed in ranges, so a large file is never held in
                // memory; a failed range ends the body early.
                let (backends, uploads) = (backends.clone(), uploads.clone());
                Response::from_stream(futures_util::stream::unfold(0u64, move |at| {
                    let (backends, uploads) = (backends.clone(), uploads.clone());
                    let (scope, key) = (scope.clone(), key.clone());
                    async move {
                        if at >= size {
                            return None;
                        }
                        let end = (at + FILE_CHUNK_BYTES).min(size);
                        let chunk = usercontent(
                            &backends,
                            &uploads,
                            UserContentRequest::Range {
                                scope,
                                key,
                                range: at..end,
                            },
                        )
                        .await;
                        Some(match chunk {
                            Reply::Chunk(bytes) if bytes.len() as u64 == end - at => {
                                (Ok::<_, worker::Error>(bytes), end)
                            }
                            _ => (Err(worker::Error::from("evidence read failed")), size),
                        })
                    }
                }))?
            };
            let headers = response.headers_mut();
            headers.set("x-request-id", id)?;
            headers.set("content-type", &served_content_type(&content_type))?;
            headers.set("content-length", &size.to_string())?;
            headers.set("content-disposition", &content_disposition(inline, &name))?;
            headers.set("cache-control", &format!("private, max-age={max_age_s}"))?;
            Ok(response)
        }
    }
}
