//! D49 signed upload URLs. Portable (wasm32): the host supplies the 32-byte
//! key from its configuration and the store clock's time; nothing here reads
//! a clock or entropy.
//!
//! A grant binds one object key (tenant, run, `evd_` ID), its exact byte size
//! and its declared type to an expiry. The signature is HMAC-SHA-256 under
//! the server key over a versioned, newline-separated message. The URL
//! carries every signed field, the tenant included (`org`, `prj`), so a host
//! checks the signature before any lookup and then requires the run to
//! belong to that tenant (UP2 fixes).
//!
//! `PUT {origin}/u/{exe}/{evd}?org=…&prj=…&size=…&type=…&expires=…&sig=…` is served by the
//! user-content origin (UP2). UP1 only signs and verifies.
//!
//! UP2 (D50) adds read grants: `GET {origin}/u/{exe}/{evd}[/thumbnail]
//! ?org=…&prj=…&expires=…&sig=…` for a finalized file or its thumbnail. They are signed
//! over their own message version, so a PUT signature never reads and a read
//! signature never uploads.
use crate::{catalog::TenantScope, webhooks::hmac_sha256};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_model::{EvidenceId, ExecutionId};
use std::{collections::BTreeMap, fmt};

/// Path prefix of upload targets on the user-content origin.
pub const UPLOAD_PATH_PREFIX: &str = "/u/";
const MESSAGE_VERSION: &str = "runbook.upload.v1";
const READ_MESSAGE_VERSION: &str = "runbook.upload.read.v1";
/// Path suffix of a thumbnail read.
pub const THUMBNAIL_SUFFIX: &str = "/thumbnail";
/// UP2: read URLs are minted per hour and stay valid one to two hours
/// ([`read_expiry`]), so a page's image URLs are stable within the hour.
pub const UPLOAD_READ_URL_TTL_MS: i64 = 60 * 60 * 1000;

/// The server's upload signing key. Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct UploadSigningKey([u8; 32]);
impl UploadSigningKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}
impl fmt::Debug for UploadSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UploadSigningKey([REDACTED])")
    }
}

/// What one signed URL allows: a single PUT of exactly `size` bytes of
/// `content_type` to the object `BlobKey::Upload { execution_id, evd_id }`
/// of `scope`, before `expires_at_ms` (store clock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadGrant {
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub evidence_id: EvidenceId,
    pub size: u64,
    pub content_type: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadUrlError {
    /// Not an upload URL, or a field is missing or unparsable.
    Malformed,
    /// The URL expired (`now >= expires_at_ms`).
    Expired,
    /// The signature does not match this grant under this key.
    Mismatch,
    /// The PUT's body length differs from the bound size.
    SizeMismatch,
    /// The PUT's `Content-Type` differs from the bound type.
    TypeMismatch,
}
impl UploadUrlError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Malformed => "upload.url_invalid",
            Self::Expired => "upload.url_expired",
            Self::Mismatch => "upload.url_invalid",
            Self::SizeMismatch => "upload.size_mismatch",
            Self::TypeMismatch => "upload.type_mismatch",
        }
    }
}

/// A declared type is one lowercase `type/subtype` token, so it can sit in
/// a query string and a signed line without escaping.
pub fn is_token_type(content_type: &str) -> bool {
    let Some((kind, sub)) = content_type.split_once('/') else {
        return false;
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+-.".contains(&b))
    };
    ok(kind) && ok(sub)
}

fn message(grant: &UploadGrant) -> String {
    format!(
        "{MESSAGE_VERSION}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        grant.scope.org_id,
        grant.scope.project_id,
        grant.execution_id,
        grant.evidence_id,
        grant.size,
        grant.content_type,
        grant.expires_at_ms
    )
}

/// The URL-safe signature of one grant.
pub fn sign(key: &UploadSigningKey, grant: &UploadGrant) -> String {
    URL_SAFE_NO_PAD.encode(hmac_sha256(&key.0, &[message(grant).as_bytes()]))
}

/// The upload target path and query (no origin).
pub fn upload_path(key: &UploadSigningKey, grant: &UploadGrant) -> String {
    format!(
        "{UPLOAD_PATH_PREFIX}{}/{}?org={}&prj={}&size={}&type={}&expires={}&sig={}",
        grant.execution_id,
        grant.evidence_id,
        grant.scope.org_id,
        grant.scope.project_id,
        grant.size,
        grant.content_type,
        grant.expires_at_ms,
        sign(key, grant)
    )
}

/// The full upload URL on the user-content origin (no trailing slash).
pub fn upload_url(origin: &str, key: &UploadSigningKey, grant: &UploadGrant) -> String {
    format!(
        "{}{}",
        origin.trim_end_matches('/'),
        upload_path(key, grant)
    )
}

/// The URL's claims, unverified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedUpload {
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub evidence_id: EvidenceId,
    pub size: u64,
    pub content_type: String,
    pub expires_at_ms: i64,
    pub signature: String,
}
impl SignedUpload {
    pub fn grant(&self) -> UploadGrant {
        UploadGrant {
            scope: self.scope.clone(),
            execution_id: self.execution_id.clone(),
            evidence_id: self.evidence_id.clone(),
            size: self.size,
            content_type: self.content_type.clone(),
            expires_at_ms: self.expires_at_ms,
        }
    }
}

/// The tenant a URL names (`org`, `prj`).
fn url_scope(query: &BTreeMap<String, String>) -> Result<TenantScope, UploadUrlError> {
    let bad = UploadUrlError::Malformed;
    Ok(TenantScope {
        org_id: query.get("org").ok_or(bad)?.parse().map_err(|_| bad)?,
        project_id: query.get("prj").ok_or(bad)?.parse().map_err(|_| bad)?,
    })
}

/// Parse `/u/{exe}/{evd}` and its query. Unknown or repeated parameters
/// are malformed (the transport refuses repeated names).
pub fn parse_upload(
    path: &str,
    query: &BTreeMap<String, String>,
) -> Result<SignedUpload, UploadUrlError> {
    let bad = UploadUrlError::Malformed;
    let rest = path.strip_prefix(UPLOAD_PATH_PREFIX).ok_or(bad)?;
    let (exe, evd) = rest.split_once('/').ok_or(bad)?;
    if query.keys().any(|k| {
        !matches!(
            k.as_str(),
            "org" | "prj" | "size" | "type" | "expires" | "sig"
        )
    }) {
        return Err(bad);
    }
    let field = |name: &str| query.get(name).ok_or(bad);
    let content_type = field("type")?.clone();
    if !is_token_type(&content_type) {
        return Err(bad);
    }
    Ok(SignedUpload {
        scope: url_scope(query)?,
        execution_id: exe.parse().map_err(|_| bad)?,
        evidence_id: evd.parse().map_err(|_| bad)?,
        size: field("size")?.parse().map_err(|_| bad)?,
        content_type,
        expires_at_ms: field("expires")?.parse().map_err(|_| bad)?,
        signature: field("sig")?.clone(),
    })
}

/// Check a signature (constant time) and the expiry against `now_ms`.
pub fn verify(
    key: &UploadSigningKey,
    grant: &UploadGrant,
    signature: &str,
    now_ms: i64,
) -> Result<(), UploadUrlError> {
    if !constant_time_eq(sign(key, grant).as_bytes(), signature.as_bytes()) {
        return Err(UploadUrlError::Mismatch);
    }
    if now_ms >= grant.expires_at_ms {
        return Err(UploadUrlError::Expired);
    }
    Ok(())
}

/// Verify one PUT: the signature and expiry, then the binding of the body's
/// length and the request's `Content-Type` to the signed size and type.
pub fn check_put(
    key: &UploadSigningKey,
    grant: &UploadGrant,
    signature: &str,
    now_ms: i64,
    body_len: u64,
    content_type: Option<&str>,
) -> Result<(), UploadUrlError> {
    verify(key, grant, signature, now_ms)?;
    let declared = content_type.map(|v| v.split(';').next().unwrap_or("").trim());
    if body_len != grant.size {
        return Err(UploadUrlError::SizeMismatch);
    }
    if declared != Some(grant.content_type.as_str()) {
        return Err(UploadUrlError::TypeMismatch);
    }
    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Which object a read grant opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadVariant {
    /// The finalized file (for images, the re-encoded one, D51).
    Original,
    /// The image's thumbnail.
    Thumbnail,
}
impl ReadVariant {
    fn label(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Thumbnail => "thumbnail",
        }
    }
}

/// What one signed read URL allows: GETs of one finalized evidence file of
/// `scope` (or its thumbnail) before `expires_at_ms` (store clock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadGrant {
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub evidence_id: EvidenceId,
    pub variant: ReadVariant,
    pub expires_at_ms: i64,
}

/// The expiry of read URLs minted at `now_ms`: the end of the next whole
/// hour, so every URL minted within one hour is the same (cacheable) and
/// lives at least an hour.
pub fn read_expiry(now_ms: i64) -> i64 {
    (now_ms.div_euclid(UPLOAD_READ_URL_TTL_MS) + 2) * UPLOAD_READ_URL_TTL_MS
}

fn read_message(grant: &ReadGrant) -> String {
    format!(
        "{READ_MESSAGE_VERSION}\n{}\n{}\n{}\n{}\n{}\n{}",
        grant.scope.org_id,
        grant.scope.project_id,
        grant.execution_id,
        grant.evidence_id,
        grant.variant.label(),
        grant.expires_at_ms
    )
}

/// The URL-safe signature of one read grant.
pub fn sign_read(key: &UploadSigningKey, grant: &ReadGrant) -> String {
    URL_SAFE_NO_PAD.encode(hmac_sha256(&key.0, &[read_message(grant).as_bytes()]))
}

/// The full read URL on the user-content origin.
pub fn read_url(origin: &str, key: &UploadSigningKey, grant: &ReadGrant) -> String {
    format!(
        "{}{UPLOAD_PATH_PREFIX}{}/{}{}?org={}&prj={}&expires={}&sig={}",
        origin.trim_end_matches('/'),
        grant.execution_id,
        grant.evidence_id,
        match grant.variant {
            ReadVariant::Original => "",
            ReadVariant::Thumbnail => THUMBNAIL_SUFFIX,
        },
        grant.scope.org_id,
        grant.scope.project_id,
        grant.expires_at_ms,
        sign_read(key, grant)
    )
}

/// A read URL's claims, unverified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedRead {
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub evidence_id: EvidenceId,
    pub variant: ReadVariant,
    pub expires_at_ms: i64,
    pub signature: String,
}
impl SignedRead {
    pub fn grant(&self) -> ReadGrant {
        ReadGrant {
            scope: self.scope.clone(),
            execution_id: self.execution_id.clone(),
            evidence_id: self.evidence_id.clone(),
            variant: self.variant,
            expires_at_ms: self.expires_at_ms,
        }
    }
}

/// Parse `/u/{exe}/{evd}` or `/u/{exe}/{evd}/thumbnail` with exactly
/// `org`, `prj`, `expires` and `sig`.
pub fn parse_read(
    path: &str,
    query: &BTreeMap<String, String>,
) -> Result<SignedRead, UploadUrlError> {
    let bad = UploadUrlError::Malformed;
    let rest = path.strip_prefix(UPLOAD_PATH_PREFIX).ok_or(bad)?;
    let (rest, variant) = match rest.strip_suffix(THUMBNAIL_SUFFIX) {
        Some(rest) => (rest, ReadVariant::Thumbnail),
        None => (rest, ReadVariant::Original),
    };
    let (exe, evd) = rest.split_once('/').ok_or(bad)?;
    if query.len() != 4 {
        return Err(bad);
    }
    let field = |name: &str| query.get(name).ok_or(bad);
    Ok(SignedRead {
        scope: url_scope(query)?,
        execution_id: exe.parse().map_err(|_| bad)?,
        evidence_id: evd.parse().map_err(|_| bad)?,
        variant,
        expires_at_ms: field("expires")?.parse().map_err(|_| bad)?,
        signature: field("sig")?.clone(),
    })
}

/// Check a read signature (constant time) and its expiry against `now_ms`.
pub fn verify_read(
    key: &UploadSigningKey,
    grant: &ReadGrant,
    signature: &str,
    now_ms: i64,
) -> Result<(), UploadUrlError> {
    if !constant_time_eq(sign_read(key, grant).as_bytes(), signature.as_bytes()) {
        return Err(UploadUrlError::Mismatch);
    }
    if now_ms >= grant.expires_at_ms {
        return Err(UploadUrlError::Expired);
    }
    Ok(())
}
