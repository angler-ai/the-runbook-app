//! UP1 upload use cases: reserve a slot, finalize it, attach finalized files
//! to a report, and sweep slots never finalized. Hosts authenticate, check
//! the caller holds the step run's claim ([`check_claim`]), sign the URL
//! (`signing`) and pass the magic-byte table (`verify::Sniff`).
use super::{
    EvidenceRecord, EvidenceState, UPLOAD_SLOT_SWEEP_MS, UPLOAD_URL_TTL_MS, UploadOutcome,
    UploadQuota, UploadStore,
    signing::is_token_type,
    verify::{FILE_TYPES, HEIC_TYPES, IMAGE_TYPES, SNIFF_PREFIX_BYTES, Sniff, judge},
};
use crate::{
    StoreError,
    actor::ActorWork,
    blob::{BlobKey, BlobStore},
    catalog::TenantScope,
    session::SessionStore,
};
use runbook_model::{
    CreateEvidenceRequest, EvidenceFileRef, EvidenceId, EvidenceView, ExecutionId, StepRunId,
    StepStatus, UploadLimits,
};
use std::future::Future;

/// Longest stored display name, in UTF-8 bytes.
pub const EVIDENCE_NAME_BYTES: usize = 255;

/// Why an upload operation stopped. `Rejected` codes are stable problem codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    Rejected(&'static str),
    Store(StoreError),
}
impl UploadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Rejected(code) => code,
            Self::Store(StoreError::NotFound) => "resource.not_found",
            Self::Store(StoreError::InvalidBatch(code)) => code,
            Self::Store(StoreError::Backend(_)) => "storage.unavailable",
        }
    }
}
impl From<StoreError> for UploadError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Whether a declared type is an image under these limits. `Err` refuses the
/// type: HEIC for now (D51), SVG and everything outside the tables always.
pub fn classify_type(limits: &UploadLimits, content_type: &str) -> Result<bool, &'static str> {
    if !is_token_type(content_type) || HEIC_TYPES.contains(&content_type) {
        return Err("upload.type_unsupported");
    }
    if IMAGE_TYPES.contains(&content_type) && limits.image_types.iter().any(|t| t == content_type) {
        return Ok(true);
    }
    if FILE_TYPES.contains(&content_type) {
        return Ok(false);
    }
    Err("upload.type_unsupported")
}

/// A display name with path separators and control characters replaced,
/// trimmed and cut to [`EVIDENCE_NAME_BYTES`] on a character boundary. A name
/// with nothing but separators, dots or spaces is `upload.invalid_name`.
pub fn sanitize_name(name: &str) -> Result<String, &'static str> {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut out = cleaned.trim().trim_start_matches('.').to_owned();
    if out.len() > EVIDENCE_NAME_BYTES {
        let mut end = EVIDENCE_NAME_BYTES;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    // Nothing but separators, dots or spaces names nothing.
    if out
        .chars()
        .all(|c| c == '_' || c == '.' || c.is_whitespace())
    {
        return Err("upload.invalid_name");
    }
    Ok(out)
}

/// The count limits the store checks with the insert.
pub fn quota(limits: &UploadLimits) -> UploadQuota {
    UploadQuota {
        images_per_step_run: limits.images_per_step_run,
        files_per_execution: limits.files_per_execution,
        storage_bytes: limits.storage_bytes,
    }
}

/// The slot ID for one POST: the same principal retrying with the same
/// `Idempotency-Key` on the same step run gets the same slot.
pub fn evidence_id_for(
    scope: &TenantScope,
    principal: &str,
    step_run_id: &StepRunId,
    idempotency_key: &str,
) -> EvidenceId {
    crate::webhooks::derived_id(
        "evd_",
        0,
        &[
            scope.org_id.as_str(),
            scope.project_id.as_str(),
            principal,
            step_run_id.as_str(),
            idempotency_key,
        ],
    )
    .parse()
    .expect("derived IDs are ULID-shaped")
}

/// D32 for uploads: the caller must hold the step run's current claim (an
/// execution credential's family or the claiming person), on a claimed step
/// whose lease has not lapsed.
pub fn check_claim<S: SessionStore>(
    work: &ActorWork<S>,
    principal: &str,
    step_run_id: &StepRunId,
) -> Result<(), UploadError> {
    let claim = work
        .snapshot
        .claims
        .get(step_run_id)
        .ok_or(UploadError::Rejected("step.not_claimed"))?;
    let claimed = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.id.as_ref() == Some(step_run_id))
        .is_some_and(|s| s.status == StepStatus::Claimed);
    if claim.released
        || claim
            .lease_deadline_at_ms
            .is_some_and(|t| t <= work.at_ms())
        || !claimed
    {
        return Err(UploadError::Rejected("step.not_claimed"));
    }
    if claim.session.principal != principal {
        return Err(UploadError::Rejected("auth.forbidden"));
    }
    Ok(())
}

/// One slot request, already authorized for `step_run_id`.
pub struct NewSlot<'a> {
    pub scope: &'a TenantScope,
    pub execution_id: &'a ExecutionId,
    pub step_run_id: &'a StepRunId,
    pub id: EvidenceId,
    pub request: &'a CreateEvidenceRequest,
    pub limits: &'a UploadLimits,
    /// Non-secret principal label (`human:usr_…`, `execution:fam_…`).
    pub created_by: &'a str,
}

/// Check the limits, then reserve the slot (D49). A retry of the same
/// request returns the slot it made; anything else under that ID is
/// `upload.duplicate`.
pub async fn reserve<S: UploadStore>(
    store: &S,
    slot: NewSlot<'_>,
) -> Result<EvidenceRecord, UploadError> {
    let NewSlot {
        scope,
        execution_id,
        step_run_id,
        id,
        request,
        limits,
        created_by,
    } = slot;
    let image = classify_type(limits, &request.content_type).map_err(UploadError::Rejected)?;
    let name = sanitize_name(&request.name).map_err(UploadError::Rejected)?;
    if request.size == 0 {
        return Err(UploadError::Rejected("upload.empty"));
    }
    let cap = if image {
        limits.image_bytes
    } else {
        limits.file_bytes
    };
    if cap.is_some_and(|cap| request.size > cap) {
        return Err(UploadError::Rejected("upload.too_large"));
    }
    let same = |r: &EvidenceRecord| {
        &r.execution_id == execution_id
            && &r.step_run_id == step_run_id
            && r.name == name
            && r.declared_type == request.content_type
            && r.declared_size == request.size
            && r.created_by == created_by
    };
    if let Some(existing) = store.get_evidence(scope, &id).await? {
        return if same(&existing) {
            Ok(existing)
        } else {
            Err(UploadError::Rejected("upload.duplicate"))
        };
    }
    let now = store.upload_time_ms().await?;
    let record = EvidenceRecord {
        id: id.clone(),
        scope: scope.clone(),
        execution_id: execution_id.clone(),
        step_run_id: step_run_id.clone(),
        name: name.clone(),
        declared_type: request.content_type.clone(),
        declared_size: request.size,
        state: EvidenceState::Pending,
        verified: None,
        created_by: created_by.to_owned(),
        created_at_ms: now,
        url_expires_at_ms: now + UPLOAD_URL_TTL_MS,
        finalized_at_ms: None,
    };
    if store
        .reserve_evidence(scope, record.clone(), &quota(limits))
        .await?
    {
        return Ok(record);
    }
    // A concurrent retry inserted first: answer with its slot.
    match store.get_evidence(scope, &id).await? {
        Some(existing) if same(&existing) => Ok(existing),
        _ => Err(UploadError::Rejected("upload.duplicate")),
    }
}

/// The object a slot's bytes live under.
pub fn object_key(record: &EvidenceRecord) -> BlobKey {
    BlobKey::Upload {
        execution_id: record.execution_id.clone(),
        evd_id: record.id.clone(),
    }
}

async fn discard<S: BlobStore>(store: &S, key: &BlobKey) -> Result<(), StoreError> {
    match store.delete(key).await {
        Ok(()) | Err(StoreError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

/// D51: what a host's image pipeline made of one verified image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessedImage {
    /// The re-encoded image, with no metadata; it replaces the original.
    pub bytes: Vec<u8>,
    /// Its type, which may differ from the upload's (a GIF becomes a PNG).
    pub content_type: String,
    pub width: u32,
    pub height: u32,
    pub thumbnail: Vec<u8>,
}

/// The seam to the host's image pipeline (D51; native in `runbook-server`).
/// `Ok(None)` stores the image as uploaded: `()` does that, for hosts and
/// tests without a pipeline. `Err` is a stable rejection code.
pub trait ImagePipeline {
    fn process(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> impl Future<Output = Result<Option<ProcessedImage>, &'static str>>;
}
impl ImagePipeline for () {
    async fn process(
        &self,
        _bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<Option<ProcessedImage>, &'static str> {
        Ok(None)
    }
}

/// An `evd_`-shaped ID derived from a slot's, for the objects UP2 stores
/// beside its upload. Derived, so no column records them.
fn derived(id: &EvidenceId, purpose: &str) -> EvidenceId {
    crate::webhooks::derived_id("evd_", 0, &[purpose, id.as_str()])
        .parse()
        .expect("derived IDs are ULID-shaped")
}

/// The ID a slot's thumbnail is stored under (UP2).
pub fn thumbnail_id(id: &EvidenceId) -> EvidenceId {
    derived(id, "runbook.upload.thumbnail.v1")
}

/// The ID a processed image's re-encoded bytes are stored under (UP2).
pub fn processed_id(id: &EvidenceId) -> EvidenceId {
    derived(id, "runbook.upload.processed.v1")
}

fn upload_key(record: &EvidenceRecord, evd_id: EvidenceId) -> BlobKey {
    BlobKey::Upload {
        execution_id: record.execution_id.clone(),
        evd_id,
    }
}

/// The object a finalized image's thumbnail lives under: an upload object
/// of the same run, so the run's prefix (export, retention) owns it too.
pub fn thumbnail_key(record: &EvidenceRecord) -> BlobKey {
    upload_key(record, thumbnail_id(&record.id))
}

/// Where a slot's re-encoded image would live.
fn processed_key(record: &EvidenceRecord) -> BlobKey {
    upload_key(record, processed_id(&record.id))
}

/// The object a finalized slot serves: the re-encoded image when the
/// pipeline processed it (`VerifiedUpload::thumbnail`), the upload itself
/// otherwise.
pub fn stored_key(record: &EvidenceRecord) -> BlobKey {
    match &record.verified {
        Some(v) if v.thumbnail => processed_key(record),
        _ => object_key(record),
    }
}

/// Finalize (D49) without an image pipeline: images are stored as uploaded.
pub async fn finalize<S: UploadStore + BlobStore>(
    store: &S,
    scope: &TenantScope,
    step_run_id: &StepRunId,
    id: &EvidenceId,
    sniff: Sniff,
) -> Result<EvidenceRecord, UploadError> {
    let unlimited = UploadLimits {
        image_types: IMAGE_TYPES.iter().map(|t| (*t).to_owned()).collect(),
        image_bytes: None,
        file_bytes: None,
        image_max_dimension: None,
        image_max_pixels: None,
        thumbnail_max_dimension: None,
        images_per_step_run: None,
        files_per_execution: None,
        storage_bytes: None,
        client_resize_long_edge: 0,
    };
    finalize_with(store, scope, step_run_id, id, sniff, &(), &unlimited).await
}

/// Reject a slot: mark it rejected with `code`, then delete its object and
/// anything derived from it, and return the code. Marking first means a
/// slot another finalize already finished keeps its files.
async fn reject<S: UploadStore + BlobStore>(
    store: &S,
    scope: &TenantScope,
    record: &EvidenceRecord,
    code: &'static str,
) -> Result<EvidenceRecord, UploadError> {
    match store
        .finalize_evidence(
            scope,
            &record.id,
            UploadOutcome::Rejected { code: code.into() },
            &UploadQuota::default(),
        )
        .await
    {
        Ok(_) => {}
        Err(StoreError::InvalidBatch("upload.not_pending")) => {
            return match store.get_evidence(scope, &record.id).await? {
                Some(done) if done.state == EvidenceState::Finalized => finished(store, done).await,
                _ => Err(UploadError::Rejected(code)),
            };
        }
        Err(error) => return Err(error.into()),
    }
    discard(store, &object_key(record)).await?;
    discard(store, &processed_key(record)).await?;
    discard(store, &thumbnail_key(record)).await?;
    Err(UploadError::Rejected(code))
}

/// Put derived bytes: the same bytes again succeed (a retry, or a
/// concurrent finalize of the same upload: encoding is deterministic);
/// different bytes replace a leftover of an earlier attempt.
async fn put_derived<S: BlobStore>(
    store: &S,
    key: BlobKey,
    bytes: &[u8],
) -> Result<(), StoreError> {
    match store.put(key.clone(), bytes).await {
        Err(StoreError::InvalidBatch("blob.immutable_key")) => {
            discard(store, &key).await?;
            store.put(key, bytes).await
        }
        other => other,
    }
}

/// Finalize (D49, D51): the stored size must equal the declared size and the
/// magic bytes must match the declared type; otherwise the object is
/// deleted, the slot is marked rejected and the code is returned. A verified
/// image then goes through `images`. Its re-encoded bytes and thumbnail are
/// stored under derived keys ([`stored_key`], [`thumbnail_key`]) and must
/// fit `limits.image_bytes` (`upload.processed_too_large`); the slot is
/// marked with their size, type and dimensions (the store re-checks
/// storage: `upload.limit_reached`); only then is the original, with its
/// metadata, deleted. A refusal rejects the slot the same way. Finalizing a
/// finalized slot returns it again (and deletes a leftover original).
/// `store` must be scoped to `scope`.
///
/// Every step before the mark is repeatable, so a failure leaves the slot
/// pending with its upload intact, and concurrent finalizes of one slot
/// write the same bytes and answer with the same record.
pub async fn finalize_with<S: UploadStore + BlobStore, P: ImagePipeline>(
    store: &S,
    scope: &TenantScope,
    step_run_id: &StepRunId,
    id: &EvidenceId,
    sniff: Sniff,
    images: &P,
    limits: &UploadLimits,
) -> Result<EvidenceRecord, UploadError> {
    let record = store
        .get_evidence(scope, id)
        .await?
        .filter(|r| &r.step_run_id == step_run_id)
        .ok_or(UploadError::Store(StoreError::NotFound))?;
    match record.state {
        EvidenceState::Finalized => return finished(store, record).await,
        EvidenceState::Rejected => return Err(UploadError::Rejected("upload.not_pending")),
        EvidenceState::Pending => {}
    }
    let key = object_key(&record);
    let stored = match store.head(&key).await {
        Ok(head) => head.size,
        Err(StoreError::NotFound) => return Err(UploadError::Rejected("upload.not_uploaded")),
        Err(error) => return Err(error.into()),
    };
    let sniffed = if stored == record.declared_size {
        let prefix = store
            .get_range(&key, 0..stored.min(SNIFF_PREFIX_BYTES))
            .await?;
        sniff(&prefix)
    } else {
        None
    };
    let mut verified = match judge(&record.declared_type, record.declared_size, stored, sniffed) {
        Ok(verified) => verified,
        Err(code) => return reject(store, scope, &record, code).await,
    };
    if IMAGE_TYPES.contains(&verified.content_type.as_str()) {
        let bytes = store.get(&key).await?;
        if bytes.len() as u64 != record.declared_size {
            return reject(store, scope, &record, "upload.size_mismatch").await;
        }
        match images.process(bytes, &verified.content_type).await {
            Ok(None) => {}
            Ok(Some(image)) => {
                let total = (image.bytes.len() as u64).saturating_add(image.thumbnail.len() as u64);
                if limits.image_bytes.is_some_and(|cap| total > cap) {
                    return reject(store, scope, &record, "upload.processed_too_large").await;
                }
                put_derived(store, thumbnail_key(&record), &image.thumbnail).await?;
                put_derived(store, processed_key(&record), &image.bytes).await?;
                verified = super::VerifiedUpload {
                    size: image.bytes.len() as u64,
                    content_type: image.content_type,
                    width: Some(image.width),
                    height: Some(image.height),
                    thumbnail: true,
                    thumbnail_bytes: image.thumbnail.len() as u64,
                };
            }
            Err(code) => return reject(store, scope, &record, code).await,
        }
    }
    match store
        .finalize_evidence(scope, id, UploadOutcome::Verified(verified), &quota(limits))
        .await
    {
        Ok(done) => finished(store, done).await,
        Err(StoreError::InvalidBatch("upload.limit_reached")) => {
            reject(store, scope, &record, "upload.limit_reached").await
        }
        // A concurrent finalize of the same bytes won; answer with its result.
        Err(StoreError::InvalidBatch("upload.not_pending")) => {
            match store.get_evidence(scope, id).await? {
                Some(done) if done.state == EvidenceState::Finalized => finished(store, done).await,
                _ => Err(UploadError::Rejected("upload.not_pending")),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// A finalized slot: a processed image's original (still carrying its
/// metadata) is deleted; a failure here leaves it for the next finalize
/// call or the run's deletion, and it is never served.
async fn finished<S: BlobStore>(
    store: &S,
    record: EvidenceRecord,
) -> Result<EvidenceRecord, UploadError> {
    if record.verified.as_ref().is_some_and(|v| v.thumbnail) {
        discard(store, &object_key(&record)).await?;
    }
    Ok(record)
}

/// D49 sweeper, run from the drain: delete up to `limit` slots of `scope`
/// still pending 24 hours after they were issued (store clock), object
/// first. Returns how many slots were removed. `store` is scoped to `scope`.
pub async fn sweep_stale<S: UploadStore + BlobStore>(
    store: &S,
    scope: &TenantScope,
    limit: usize,
) -> Result<usize, StoreError> {
    let now = store.upload_time_ms().await?;
    let stale = store
        .stale_evidence(scope, now - UPLOAD_SLOT_SWEEP_MS, limit)
        .await?;
    let mut removed = 0;
    for slot in stale {
        discard(store, &object_key(&slot)).await?;
        // A finalize interrupted after storing derived objects leaves them.
        discard(store, &processed_key(&slot)).await?;
        discard(store, &thumbnail_key(&slot)).await?;
        if store.delete_evidence(scope, &slot.id).await? {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Report evidence that names `evd_` IDs (D49): each must be a finalized
/// file of this step run. Returns the files with their verified types, in
/// report order, without repeats. Link entries are left to the URL check.
pub async fn report_files<S: UploadStore>(
    store: &S,
    scope: &TenantScope,
    execution_id: &ExecutionId,
    step_run_id: &StepRunId,
    evidence: &[String],
) -> Result<Vec<EvidenceFileRef>, UploadError> {
    let mut files: Vec<EvidenceFileRef> = Vec::new();
    for entry in evidence {
        let Ok(id) = entry.parse::<EvidenceId>() else {
            continue;
        };
        if files.iter().any(|f| f.id == id) {
            continue;
        }
        let record = store
            .get_evidence(scope, &id)
            .await?
            .filter(|r| &r.execution_id == execution_id && &r.step_run_id == step_run_id)
            .ok_or(UploadError::Rejected("evidence.not_found"))?;
        let verified = match (&record.state, record.verified) {
            (EvidenceState::Finalized, Some(verified)) => verified,
            _ => return Err(UploadError::Rejected("evidence.not_finalized")),
        };
        files.push(EvidenceFileRef {
            id,
            content_type: verified.content_type,
        });
    }
    Ok(files)
}

/// Report a step whose evidence may name uploads: resolve the `evd_`
/// entries against `store` (finalized files of this step run, D49), then
/// validate and commit like `claims::report_step`. CF12b review round 2:
/// when only appended lines could meet required `log` evidence, `store` is
/// asked for them (`claims::logs_appended`).
pub async fn report_with_uploads<S: SessionStore + UploadStore + crate::claims::ClaimStore>(
    store: &S,
    work: ActorWork<S>,
    principal: &crate::actor::ActorPrincipal,
    request: &runbook_model::ReportRequest,
    replay: Option<&crate::actor::Replay>,
) -> Result<(), crate::actor::ActorError> {
    use crate::actor::ActorError;
    let files = report_files(
        store,
        &work.runtime.scope,
        &work.runtime.state.id,
        &request.step_run_id,
        &request.evidence,
    )
    .await
    .map_err(|e| match e {
        UploadError::Store(error) => ActorError::Store(error),
        UploadError::Rejected(code) => ActorError::rejected(code),
    })?;
    let appended = if crate::claims::needs_appended_logs(&work, request) {
        crate::claims::logs_appended(store, &work.runtime.state.id, &request.step_run_id).await?
    } else {
        false
    };
    crate::claims::report_step_logged(work, principal, request, files, appended, replay).await
}

/// The wire view. The host passes signed read URLs for a finalized file and
/// its thumbnail (UP2, D50).
pub fn view(
    record: &EvidenceRecord,
    url: Option<String>,
    thumbnail_url: Option<String>,
) -> EvidenceView {
    let verified = record.verified.as_ref();
    EvidenceView {
        id: record.id.clone(),
        step_run_id: record.step_run_id.clone(),
        name: record.name.clone(),
        content_type: verified
            .map_or_else(|| record.declared_type.clone(), |v| v.content_type.clone()),
        size: verified.map_or(record.declared_size, |v| v.size),
        state: match record.state {
            EvidenceState::Pending => runbook_model::EvidenceState::Pending,
            EvidenceState::Finalized => runbook_model::EvidenceState::Finalized,
            EvidenceState::Rejected => runbook_model::EvidenceState::Rejected,
        },
        width: verified.and_then(|v| v.width),
        height: verified.and_then(|v| v.height),
        url,
        thumbnail_url,
        created_at_ms: record.created_at_ms,
        finalized_at_ms: record.finalized_at_ms,
    }
}
