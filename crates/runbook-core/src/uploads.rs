//! Evidence uploads (PRD §14 "Uploads and images"; D49–D51). T0 freezes the
//! slot records and the store trait; UP1 implements them (with `0028_uploads`
//! and the sweeper), UP3 adds `uploads/{sniff,limits}.rs` beside this file.
//!
//! Flow: a slot is reserved with the declared name, type and size (limits
//! checked first), the client PUTs the bytes to the user-content origin under
//! `BlobKey::Upload`, and finalize checks the stored size and magic bytes.
//! A mismatch deletes the object and marks the slot rejected; slots never
//! finalized are swept after [`UPLOAD_SLOT_SWEEP_MS`].
//!
//! UP1: `signing` (D49 URLs), `verify` (the finalize decision and the seam to
//! UP3's magic-byte table) and the use cases (`usecases`, re-exported here).
pub mod signing;
mod usecases;
pub mod verify;

pub mod limits;
pub mod sniff;

pub use limits::{UploadLimits, UploadRefusal, check, check_counts};
pub use sniff::{SniffedType, mime, sniff};

use crate::{StoreError, catalog::TenantScope};
use runbook_model::{EvidenceId, ExecutionId, StepRunId};
use serde::{Deserialize, Serialize};
use std::future::Future;
pub use usecases::*;

/// D49: a signed upload URL lives 15 minutes.
pub const UPLOAD_URL_TTL_MS: i64 = 15 * 60 * 1000;
/// D49: an unfinalized slot is deleted after 24 hours.
pub const UPLOAD_SLOT_SWEEP_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    /// Slot issued; bytes may or may not have arrived.
    Pending,
    /// Size and type verified; reports may reference it by `evd_` ID.
    Finalized,
    /// Verification failed; the object was deleted.
    Rejected,
}

/// Facts finalize established from the stored bytes, never from the client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedUpload {
    pub size: u64,
    /// MIME type from the magic bytes (UP3's sniff table).
    pub content_type: String,
    /// Images only, from the header before any decode (D51).
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Whether the host's image pipeline processed the file (UP2): its
    /// re-encoded bytes are at `uploads::stored_key` and its thumbnail at
    /// `uploads::thumbnail_key`.
    pub thumbnail: bool,
    /// The thumbnail's size, counted toward storage with `size`.
    #[serde(default)]
    pub thumbnail_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub id: EvidenceId,
    pub scope: TenantScope,
    pub execution_id: ExecutionId,
    pub step_run_id: StepRunId,
    /// Display file name, sanitized by the host.
    pub name: String,
    /// What the client declared; the signed URL is bound to both.
    pub declared_type: String,
    pub declared_size: u64,
    pub state: EvidenceState,
    /// Set once finalized.
    pub verified: Option<VerifiedUpload>,
    /// Non-secret principal label of the uploader.
    pub created_by: String,
    pub created_at_ms: i64,
    pub url_expires_at_ms: i64,
    pub finalized_at_ms: Option<i64>,
}

impl EvidenceRecord {
    /// Bytes this slot counts toward the organization's storage: what is
    /// stored once finalized (the file and its thumbnail), the declared size
    /// before, nothing once rejected.
    pub fn counted_bytes(&self) -> u64 {
        match (&self.state, &self.verified) {
            (EvidenceState::Rejected, _) => 0,
            (EvidenceState::Finalized, Some(v)) => v.size.saturating_add(v.thumbnail_bytes),
            _ => self.declared_size,
        }
    }
}

/// Count limits checked atomically with the slot insert. `None` is unlimited
/// (the self-hosted default). Pending and finalized slots both count;
/// storage counts [`EvidenceRecord::counted_bytes`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadQuota {
    /// Slots whose declared type is `image/*`, per step run.
    pub images_per_step_run: Option<u32>,
    pub files_per_execution: Option<u32>,
    /// Bytes of uploads per organization (PRD §17 storage); declared sizes count.
    pub storage_bytes: Option<u64>,
}

/// How finalize ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadOutcome {
    Verified(VerifiedUpload),
    /// Stable problem code, e.g. `upload.size_mismatch`, `upload.type_mismatch`.
    Rejected {
        code: String,
    },
}

/// Tenant-scoped evidence slots. Every call filters by `scope`; a foreign slot
/// reads as absent. Implementations keep each call atomic; futures need not
/// be Send. Bytes live in the `BlobStore`, never here.
pub trait UploadStore {
    fn upload_time_ms(&self) -> impl Future<Output = Result<i64, StoreError>>;
    /// Insert a pending slot for a run this tenant owns (`NotFound`
    /// otherwise; the host has already checked the step run), unless `quota`
    /// would be exceeded (`InvalidBatch("upload.limit_reached")`).
    /// True when inserted; false when the same ID already holds an equal
    /// record (a retry); a different record under that ID is
    /// `InvalidBatch("upload.duplicate")`.
    fn reserve_evidence(
        &self,
        scope: &TenantScope,
        record: EvidenceRecord,
        quota: &UploadQuota,
    ) -> impl Future<Output = Result<bool, StoreError>>;
    fn get_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> impl Future<Output = Result<Option<EvidenceRecord>, StoreError>>;
    /// Pending to finalized or rejected, once, stamped with the store clock;
    /// a slot no longer pending is `InvalidBatch("upload.not_pending")`.
    /// Finalizing re-checks `quota.storage_bytes` with the verified bytes
    /// ([`EvidenceRecord::counted_bytes`]) under the same lock as reserve:
    /// over it is `InvalidBatch("upload.limit_reached")` and nothing changes.
    fn finalize_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
        outcome: UploadOutcome,
        quota: &UploadQuota,
    ) -> impl Future<Output = Result<EvidenceRecord, StoreError>>;
    /// One run's slots in creation order (the execution document lists the
    /// finalized ones).
    fn list_evidence(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> impl Future<Output = Result<Vec<EvidenceRecord>, StoreError>>;
    /// Up to `limit` pending slots created before `before_ms`, for the sweeper.
    fn stale_evidence(
        &self,
        scope: &TenantScope,
        before_ms: i64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<EvidenceRecord>, StoreError>>;
    /// Remove a slot row after its blob is gone. False when absent.
    fn delete_evidence(
        &self,
        scope: &TenantScope,
        id: &EvidenceId,
    ) -> impl Future<Output = Result<bool, StoreError>>;
}
