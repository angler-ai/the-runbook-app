//! Evidence upload routes (PRD §14 "Uploads and images", §9; D49–D51, D32).
//!
//! On the main listener:
//! - `POST /v1/step-runs/{sr}/evidence` reserves a slot and returns a signed
//!   PUT URL on the user-content origin.
//! - `POST /v1/step-runs/{sr}/evidence/{evd}/finalize` checks the stored
//!   object's size and magic bytes, then runs images through the native
//!   pipeline (UP2, D51: dimensions from the header, metadata-free
//!   re-encode, thumbnail) and returns the file with signed read URLs.
//! - `GET /v1/executions/{exe}/evidence` (UP2) lists a run's finalized files
//!   with signed read URLs, for whoever may read the run (`executions:read`).
//!
//! The first two need the step run's current claim: an execution access
//! credential of the claiming family, or the claiming person (PAT or session
//! with `executions:act`). Anything else is 403.
//!
//! On the user-content listener (UP2, D50), [`usercontent`] serves the signed
//! `PUT /u/{exe}/{evd}` (bytes in) and the signed `GET /u/{exe}/{evd}` and
//! `GET /u/{exe}/{evd}/thumbnail` (finalized bytes out). The main listener
//! no longer knows `/u/*`.
use crate::{
    Request,
    backends::Backends,
    execution_credentials::{access, storage},
    executions::ExecutionApiError as Error,
    problem::Response,
};
use runbook_core::{
    StoreError,
    actor::begin_actor,
    blob::{BlobKey, BlobStore},
    catalog::TenantScope,
    credentials::{
        AuthError, CredentialStore, Principal, Scope, TokenKind, authorize, parse_secret,
    },
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    step_lookup::StepRunLookup,
    uploads::{
        self, EvidenceRecord, EvidenceState, NewSlot, UploadError, UploadStore, classify_type,
        signing::{
            self, ReadGrant, ReadVariant, SignedRead, SignedUpload, UploadGrant, UploadSigningKey,
            UploadUrlError,
        },
    },
};
use runbook_model::*;
use serde_json::{Value, json};

/// The upload settings the routes need (D49–D50): a plain copy of the
/// host's configuration.
#[derive(Clone)]
pub struct UploadSettings {
    /// Slots can be issued and finalized (`capabilities.uploads`): a
    /// signing key, a sniffer and a separate user-content origin.
    pub enabled: bool,
    pub signing_key: Option<UploadSigningKey>,
    pub usercontent_origin: Option<String>,
    pub limits: UploadLimits,
    pub sniff: Option<runbook_core::uploads::verify::Sniff>,
}

const SLOT: &str = "/v1/step-runs/{sr}/evidence";
const FINALIZE: &str = "/v1/step-runs/{sr}/evidence/{evd}/finalize";
const LIST: &str = "/v1/executions/{exe}/evidence";

pub fn recognizes(path: &str) -> bool {
    [SLOT, FINALIZE, LIST]
        .iter()
        .any(|pattern| crate::directory::match_path(pattern, path).is_some())
}

pub async fn handle<B: Backends>(backends: &B, uploads: &UploadSettings, r: Request) -> Response {
    let result = if crate::directory::match_path(LIST, &r.path).is_some() {
        list(backends, uploads, &r).await
    } else {
        dispatch(backends, uploads, &r).await
    };
    match result {
        Ok((status, body)) => Response::json(status, body, &r.request_id),
        Err(e) => {
            let mut out = Response::error(e.status, e.code, &r.request_id);
            out.body["errors"] = json!(e.diagnostics);
            out
        }
    }
}

fn not_found() -> Error {
    Error::new(404, "resource.not_found")
}
fn forbidden() -> Error {
    Error::new(403, "auth.forbidden")
}
fn invalid() -> Error {
    Error::new(401, "auth.invalid_credential")
}

/// Map an upload use-case error to the route's problem.
fn rejection(e: UploadError) -> Error {
    let code = e.code();
    let status = match code {
        "resource.not_found" => 404,
        "auth.forbidden" => 403,
        "storage.unavailable" => 503,
        "step.not_claimed"
        | "upload.duplicate"
        | "upload.not_uploaded"
        | "upload.not_pending"
        | "blob.immutable_key" => 409,
        _ => 422,
    };
    Error::new(status, code)
}

/// Who is calling, for which run, under which principal key (the claim's
/// session principal must equal it).
struct Caller {
    scope: TenantScope,
    execution_id: ExecutionId,
    principal: String,
    /// The execution secret, rechecked after the execution lock.
    execution_secret: Option<String>,
}

async fn caller<B: Backends>(backends: &B, r: &Request, sr: &StepRunId) -> Result<Caller, Error> {
    if let Some(secret) = r.bearer()
        && parse_secret(secret).is_ok_and(|(kind, _)| kind == TokenKind::Execution)
    {
        let verified = access(backends, secret).await?;
        let store = backends.execution(verified.scope.clone());
        let found = store
            .step_run_execution(&verified.scope.org_id, sr)
            .await
            .map_err(storage)?;
        if found
            != Some((
                verified.principal.execution_id.clone(),
                verified.scope.project_id.clone(),
            ))
        {
            return Err(forbidden());
        }
        return Ok(Caller {
            principal: format!("execution:{}", verified.principal.family_id),
            execution_id: verified.principal.execution_id,
            scope: verified.scope,
            execution_secret: Some(secret.to_owned()),
        });
    }
    // A person: PAT or session, executions:act on the run's project.
    let credentials = backends.credentials();
    let record = r.credential_record(&credentials).await.map_err(|e| {
        let (status, code) = e.status();
        Error::new(status, code)
    })?;
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(storage)?;
    if record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err(invalid());
    }
    let lookup = backends.execution(TenantScope {
        org_id: record.org_id.clone(),
        project_id: "prj_00000000000000000000000000"
            .parse()
            .expect("zero project ID"),
    });
    let (execution_id, project_id) = lookup
        .step_run_execution(&record.org_id, sr)
        .await
        .map_err(storage)?
        .ok_or_else(not_found)?;
    let snapshot = directory.snapshot(&record.org_id).await.map_err(storage)?;
    let principal = authorize(&record, &snapshot, now, &project_id, Scope::ExecutionsAct).map_err(
        |e| match e {
            AuthError::Forbidden if !record.scopes.contains(&Scope::ExecutionsAct) => {
                Error::new(403, "auth.scope_missing")
            }
            AuthError::Forbidden => forbidden(),
            _ => invalid(),
        },
    )?;
    let Principal::Human { .. } = &principal else {
        return Err(forbidden());
    };
    credentials
        .touch_last_used(&record.org_id, &record.id, now)
        .await
        .map_err(storage)?;
    Ok(Caller {
        principal: crate::actor_store::principal_key(&principal),
        scope: TenantScope {
            org_id: record.org_id.clone(),
            project_id,
        },
        execution_id,
        execution_secret: None,
    })
}

async fn dispatch<B: Backends>(
    backends: &B,
    uploads: &UploadSettings,
    r: &Request,
) -> Result<(u16, Value), Error> {
    let (finalize, params) = [SLOT, FINALIZE]
        .iter()
        .find_map(|pattern| {
            crate::directory::match_path(pattern, &r.path).map(|p| (*pattern == FINALIZE, p))
        })
        .ok_or_else(not_found)?;
    if r.method != "POST" {
        return Err(Error::new(405, "request.method_not_allowed"));
    }
    if !uploads.enabled {
        return Err(Error::new(503, "uploads.not_configured"));
    }
    let (Some(key), Some(sniff), Some(usercontent)) = (
        uploads.signing_key.as_ref(),
        uploads.sniff,
        uploads.usercontent_origin.as_deref(),
    ) else {
        return Err(Error::new(503, "uploads.not_configured"));
    };
    let sr: StepRunId = params["sr"].parse().map_err(|_| not_found())?;
    let who = caller(backends, r, &sr).await?;
    let store = backends.execution(who.scope.clone());
    let work = begin_actor(
        &store,
        &backends.directory(),
        who.scope.clone(),
        &who.execution_id,
    )
    .await
    .map_err(crate::actor_store::actor_error)?;
    // Recheck revocation after waiting for the execution lock.
    if let Some(secret) = &who.execution_secret {
        access(backends, secret).await?;
    }
    uploads::check_claim(&work, &who.principal, &sr).map_err(rejection)?;
    drop(work);
    if finalize {
        let id: EvidenceId = params["evd"].parse().map_err(|_| not_found())?;
        // The slot must be this step run's before anything is held for it.
        let slot = store
            .get_evidence(&who.scope, &id)
            .await
            .map_err(storage)?
            .filter(|r| r.step_run_id == sr)
            .ok_or_else(not_found)?;
        let _image = if slot.state == EvidenceState::Pending
            && classify_type(&uploads.limits, &slot.declared_type) == Ok(true)
        {
            Some(
                backends
                    .try_image_slot()
                    .ok_or_else(|| Error::new(503, "server.busy"))?,
            )
        } else {
            None
        };
        let images = backends.images();
        let record = uploads::finalize_with(
            &store,
            &who.scope,
            &sr,
            &id,
            sniff,
            &images,
            &uploads.limits,
        )
        .await
        .map_err(rejection)?;
        let now = store.upload_time_ms().await.map_err(storage)?;
        return Ok((
            200,
            serde_json::to_value(signed_view(key, usercontent, &record, now)).map_err(storage)?,
        ));
    }
    let idempotency = r
        .idempotency_key
        .as_deref()
        .filter(|k| !k.trim().is_empty() && k.len() <= 256)
        .ok_or_else(|| Error::new(428, "request.idempotency_key_required"))?;
    let request: CreateEvidenceRequest =
        serde_json::from_value(r.body.clone()).map_err(|_| Error::new(422, "request.invalid"))?;
    let id = uploads::evidence_id_for(&who.scope, &who.principal, &sr, idempotency);
    let record = uploads::reserve(
        &store,
        NewSlot {
            scope: &who.scope,
            execution_id: &who.execution_id,
            step_run_id: &sr,
            id,
            request: &request,
            limits: &uploads.limits,
            created_by: &who.principal,
        },
    )
    .await
    .map_err(rejection)?;
    let grant = UploadGrant {
        scope: who.scope.clone(),
        execution_id: record.execution_id.clone(),
        evidence_id: record.id.clone(),
        size: record.declared_size,
        content_type: record.declared_type.clone(),
        expires_at_ms: record.url_expires_at_ms,
    };
    let view = EvidenceSlotView {
        evidence_id: record.id.clone(),
        upload_url: signing::upload_url(usercontent, key, &grant),
        headers: [
            ("Content-Type".to_owned(), record.declared_type.clone()),
            (
                "Content-Length".to_owned(),
                record.declared_size.to_string(),
            ),
        ]
        .into(),
        expires_at_ms: record.url_expires_at_ms,
    };
    Ok((201, serde_json::to_value(view).map_err(storage)?))
}

/// The wire view with signed read URLs once finalized (D50).
fn signed_view(
    key: &UploadSigningKey,
    usercontent: &str,
    record: &EvidenceRecord,
    now_ms: i64,
) -> EvidenceView {
    let grant = |variant| ReadGrant {
        scope: record.scope.clone(),
        execution_id: record.execution_id.clone(),
        evidence_id: record.id.clone(),
        variant,
        expires_at_ms: signing::read_expiry(now_ms),
    };
    let finalized = record.state == EvidenceState::Finalized;
    let thumbnail = finalized && record.verified.as_ref().is_some_and(|v| v.thumbnail);
    uploads::view(
        record,
        finalized.then(|| signing::read_url(usercontent, key, &grant(ReadVariant::Original))),
        thumbnail.then(|| signing::read_url(usercontent, key, &grant(ReadVariant::Thumbnail))),
    )
}

/// `GET /v1/executions/{exe}/evidence`: the run's finalized files in upload
/// order, with read URLs. Readers need `executions:read` on the run's
/// project (PAT, service token or session); a run outside the caller's
/// reach is 404. Execution tokens get their files' URLs from finalize.
async fn list<B: Backends>(
    backends: &B,
    uploads: &UploadSettings,
    r: &Request,
) -> Result<(u16, Value), Error> {
    let params = crate::directory::match_path(LIST, &r.path).ok_or_else(not_found)?;
    if r.method != "GET" {
        return Err(Error::new(405, "request.method_not_allowed"));
    }
    if !uploads.enabled {
        return Err(Error::new(503, "uploads.not_configured"));
    }
    let (Some(key), Some(usercontent)) = (
        uploads.signing_key.as_ref(),
        uploads.usercontent_origin.as_deref(),
    ) else {
        return Err(Error::new(503, "uploads.not_configured"));
    };
    if r.bearer()
        .is_some_and(|s| parse_secret(s).is_ok_and(|(k, _)| k == TokenKind::Execution))
    {
        return Err(forbidden());
    }
    let exe: ExecutionId = params["exe"].parse().map_err(|_| not_found())?;
    let credentials = backends.credentials();
    let record = r.credential_record(&credentials).await.map_err(|e| {
        let (status, code) = e.status();
        Error::new(status, code)
    })?;
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(storage)?;
    if record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err(invalid());
    }
    // The scope check needs no resource, so it discloses nothing.
    if !record.scopes.contains(&Scope::ExecutionsRead) {
        return Err(Error::new(403, "auth.scope_missing"));
    }
    let scope = backends
        .execution_owner(&record.org_id, &exe)
        .await
        .map_err(storage)?
        .filter(|s| s.org_id == record.org_id)
        .ok_or_else(not_found)?;
    let snapshot = directory.snapshot(&record.org_id).await.map_err(storage)?;
    authorize(
        &record,
        &snapshot,
        now,
        &scope.project_id,
        Scope::ExecutionsRead,
    )
    .map_err(|e| match e {
        AuthError::Forbidden => not_found(),
        _ => invalid(),
    })?;
    credentials
        .touch_last_used(&record.org_id, &record.id, now)
        .await
        .map_err(storage)?;
    let store = backends.execution(scope.clone());
    let now = store.upload_time_ms().await.map_err(storage)?;
    let items: Vec<EvidenceView> = store
        .list_evidence(&scope, &exe)
        .await
        .map_err(storage)?
        .iter()
        .filter(|r| r.state == EvidenceState::Finalized)
        .map(|r| signed_view(key, usercontent, r, now))
        .collect();
    Ok((
        200,
        serde_json::to_value(EvidencePage { items }).map_err(storage)?,
    ))
}

/// One request on the user-content listener (D50). The transport has
/// already parsed the URL and checked its signature and expiry by the
/// wall clock, before any database call; the worker re-checks against the
/// store. Cookies and credentials never reach it.
pub enum UserContentRequest {
    /// Before any body byte: the run belongs to the URL's tenant, the URL
    /// is unexpired by the store clock, and the slot still takes bytes.
    PutCheck(SignedUpload),
    /// The signed upload: everything again, then the raw body.
    Put {
        signed: SignedUpload,
        content_type: Option<String>,
        bytes: Vec<u8>,
    },
    /// A signed read: which object, and how to serve it.
    Get(SignedRead),
    /// One range of an object a `Get` authorized (streamed responses).
    Range {
        scope: TenantScope,
        key: BlobKey,
        range: std::ops::Range<u64>,
    },
}

/// What the user-content listener answers.
pub enum UserContentReply {
    /// The pre-body checks passed.
    Ready,
    /// 204: the bytes are stored.
    Stored,
    /// A file to stream with `Range` calls.
    File {
        content_type: String,
        /// The display name for `Content-Disposition`.
        name: String,
        /// Images are shown inline; everything else downloads.
        inline: bool,
        size: u64,
        scope: TenantScope,
        key: BlobKey,
        /// Seconds the URL has left (browser cache lifetime).
        max_age_s: i64,
    },
    Chunk(Vec<u8>),
    Problem {
        status: u16,
        code: &'static str,
    },
}

pub async fn usercontent<B: Backends>(
    backends: &B,
    uploads: &UploadSettings,
    request: UserContentRequest,
) -> UserContentReply {
    let result = if !uploads.enabled {
        Err(Error::new(503, "uploads.not_configured"))
    } else {
        match request {
            UserContentRequest::PutCheck(signed) => put_check(backends, uploads, &signed)
                .await
                .map(|_| UserContentReply::Ready),
            UserContentRequest::Put {
                signed,
                content_type,
                bytes,
            } => put(backends, uploads, &signed, content_type.as_deref(), &bytes)
                .await
                .map(|()| UserContentReply::Stored),
            UserContentRequest::Get(signed) => read(backends, uploads, &signed).await,
            UserContentRequest::Range { scope, key, range } => backends
                .execution(scope)
                .get_range(&key, range)
                .await
                .map(UserContentReply::Chunk)
                .map_err(|e| match e {
                    StoreError::NotFound => not_found(),
                    _ => Error::new(503, "storage.unavailable"),
                }),
        }
    };
    result.unwrap_or_else(|e| UserContentReply::Problem {
        status: e.status,
        code: e.code,
    })
}

/// The safety headers every user-content response carries, errors
/// included (D50), on every edition. A response also gets
/// `Content-Disposition: attachment` and `Cache-Control: no-store` unless it
/// set its own, and never a `Set-Cookie`.
pub const USERCONTENT_HEADERS: [(&str, &str); 4] = [
    ("x-content-type-options", "nosniff"),
    (
        "content-security-policy",
        "sandbox; default-src 'none'; frame-ancestors 'none'",
    ),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
];

/// `attachment` or `inline` with an ASCII fallback name and the UTF-8 name
/// (RFC 6266, RFC 8187).
pub fn content_disposition(inline: bool, name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b".-_~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!(
        "{}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}",
        if inline { "inline" } else { "attachment" }
    )
}

/// A signed URL's query as one value per name; a repeated name (or an
/// unparsable query) is refused.
pub fn single_query(query: Option<&str>) -> Option<std::collections::BTreeMap<String, String>> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(query.unwrap_or("")).ok()?;
    let mut map = std::collections::BTreeMap::new();
    for (key, value) in pairs {
        if map.insert(key, value).is_some() {
            return None;
        }
    }
    Some(map)
}

/// A served file's `Content-Type`: text gets an explicit UTF-8 charset.
pub fn served_content_type(content_type: &str) -> String {
    if content_type.starts_with("text/") {
        format!("{content_type}; charset=utf-8")
    } else {
        content_type.to_owned()
    }
}

pub fn url_error(e: UploadUrlError) -> Error {
    match e {
        UploadUrlError::SizeMismatch | UploadUrlError::TypeMismatch => Error::new(422, e.code()),
        _ => Error::new(403, e.code()),
    }
}

/// The run must belong to the tenant the URL names (and signs): an unknown
/// or foreign run fails like a bad signature, disclosing nothing.
async fn url_tenant<B: Backends>(
    backends: &B,
    scope: &TenantScope,
    execution_id: &ExecutionId,
) -> Result<B::Execution, Error> {
    let owner = backends
        .execution_owner(&scope.org_id, execution_id)
        .await
        .map_err(storage)?;
    if owner.as_ref() != Some(scope) {
        return Err(Error::new(403, "upload.url_invalid"));
    }
    Ok(backends.execution(scope.clone()))
}

/// D49 before the body: tenant, signature and expiry by the store clock,
/// and a pending slot of this run.
async fn put_check<B: Backends>(
    backends: &B,
    uploads: &UploadSettings,
    signed: &SignedUpload,
) -> Result<(B::Execution, EvidenceRecord, i64), Error> {
    let key = uploads
        .signing_key
        .as_ref()
        .ok_or_else(|| Error::new(503, "uploads.not_configured"))?;
    let store = url_tenant(backends, &signed.scope, &signed.execution_id).await?;
    let now = store.upload_time_ms().await.map_err(storage)?;
    signing::verify(key, &signed.grant(), &signed.signature, now).map_err(url_error)?;
    let record = store
        .get_evidence(&signed.scope, &signed.evidence_id)
        .await
        .map_err(storage)?
        .filter(|r| r.execution_id == signed.execution_id)
        .ok_or_else(not_found)?;
    if record.state != EvidenceState::Pending {
        return Err(Error::new(409, "upload.not_pending"));
    }
    Ok((store, record, now))
}

/// D49 PUT: the pre-body checks again, then the body's length and type
/// against the signed size and type.
async fn put<B: Backends>(
    backends: &B,
    uploads: &UploadSettings,
    signed: &SignedUpload,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<(), Error> {
    let (store, record, now) = put_check(backends, uploads, signed).await?;
    let key = uploads
        .signing_key
        .as_ref()
        .ok_or_else(|| Error::new(503, "uploads.not_configured"))?;
    signing::check_put(
        key,
        &signed.grant(),
        &signed.signature,
        now,
        bytes.len() as u64,
        content_type,
    )
    .map_err(url_error)?;
    store
        .put(uploads::object_key(&record), bytes)
        .await
        .map_err(|e| match e {
            StoreError::InvalidBatch(code) => Error::new(409, code),
            StoreError::NotFound => not_found(),
            StoreError::Backend(_) => Error::new(503, "storage.unavailable"),
        })
}

/// The name a file is served under: a GIF the pipeline stored as PNG gets
/// a `.png` name.
fn served_name(name: &str, content_type: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if content_type == "image/png" && lower.ends_with(".gif") {
        format!("{}.png", &name[..name.len() - 4])
    } else {
        name.to_owned()
    }
}

/// D50 GET: a finalized file (or its thumbnail) under a valid read URL;
/// the transport streams it with `Range` calls.
async fn read<B: Backends>(
    backends: &B,
    uploads: &UploadSettings,
    signed: &SignedRead,
) -> Result<UserContentReply, Error> {
    let key = uploads
        .signing_key
        .as_ref()
        .ok_or_else(|| Error::new(503, "uploads.not_configured"))?;
    let store = url_tenant(backends, &signed.scope, &signed.execution_id).await?;
    let now = store.upload_time_ms().await.map_err(storage)?;
    signing::verify_read(key, &signed.grant(), &signed.signature, now).map_err(url_error)?;
    let record = store
        .get_evidence(&signed.scope, &signed.evidence_id)
        .await
        .map_err(storage)?
        .filter(|r| r.execution_id == signed.execution_id && r.state == EvidenceState::Finalized)
        .ok_or_else(not_found)?;
    let verified = record.verified.as_ref().ok_or_else(not_found)?;
    let (object, content_type) = match signed.variant {
        ReadVariant::Original => (uploads::stored_key(&record), verified.content_type.clone()),
        ReadVariant::Thumbnail if verified.thumbnail => (
            uploads::thumbnail_key(&record),
            verified.content_type.clone(),
        ),
        ReadVariant::Thumbnail => return Err(not_found()),
    };
    let size = store
        .head(&object)
        .await
        .map_err(|e| match e {
            StoreError::NotFound => not_found(),
            _ => Error::new(503, "storage.unavailable"),
        })?
        .size;
    Ok(UserContentReply::File {
        inline: content_type.starts_with("image/"),
        name: served_name(&record.name, &content_type),
        content_type,
        size,
        scope: signed.scope.clone(),
        key: object,
        max_age_s: ((signed.expires_at_ms - now) / 1000).max(0),
    })
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    #[test]
    fn disposition_has_an_ascii_fallback_and_the_utf8_name() {
        assert_eq!(
            content_disposition(false, "deploy log.txt"),
            "attachment; filename=\"deploy log.txt\"; filename*=UTF-8''deploy%20log.txt"
        );
        assert_eq!(
            content_disposition(true, "é\"x.png"),
            "inline; filename=\"__x.png\"; filename*=UTF-8''%C3%A9%22x.png"
        );
    }
    #[test]
    fn a_repeated_query_name_is_refused() {
        assert!(single_query(Some("a=1&a=2")).is_none());
        assert_eq!(single_query(Some("a=1&b=2")).map(|m| m.len()), Some(2));
        assert_eq!(single_query(None).map(|m| m.len()), Some(0));
    }
    #[test]
    fn text_is_served_as_utf8() {
        assert_eq!(
            served_content_type("text/plain"),
            "text/plain; charset=utf-8"
        );
        assert_eq!(served_content_type("application/pdf"), "application/pdf");
    }
}
