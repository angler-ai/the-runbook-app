//! Typed namespaces keep execution evidence separate from shared immutable snapshots.
use crate::StoreError;
use runbook_model::{EvidenceId, ExecutionId, RunbookId};
use serde::{Deserialize, Serialize};
use std::{future::Future, ops::Range};
pub const BLOB_CHUNK_BYTES: usize = 65_536;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BlobKey {
    Execution {
        execution_id: ExecutionId,
        name: String,
    },
    Shared {
        hash: [u8; 32],
    },
    /// An evidence file's bytes (D49). It belongs to its run's prefix, so a
    /// run is exported, archived or deleted with its uploads (UP1).
    Upload {
        execution_id: ExecutionId,
        evd_id: EvidenceId,
    },
    /// One runbook's daily ZIP; `day` is the UTC day number since the Unix
    /// epoch, as in `RunStore::finished_on` (D53, ARC1). Content-addressed
    /// by `digest` (SHA-256 of the bytes), so a rewrite is a new key and the
    /// index row swings to it atomically.
    Archive {
        runbook_id: RunbookId,
        day: i64,
        digest: [u8; 32],
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobPrefix {
    Execution(ExecutionId),
    Shared,
    /// Every stored version of one runbook's file for one day (ARC2 cleanup
    /// of versions an interrupted rewrite left behind).
    Archive {
        runbook_id: RunbookId,
        day: i64,
    },
}
/// Object metadata without the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobHead {
    pub size: u64,
}
pub trait BlobStore {
    fn put(&self, key: BlobKey, bytes: &[u8]) -> impl Future<Output = Result<(), StoreError>>;
    fn get(&self, key: &BlobKey) -> impl Future<Output = Result<Vec<u8>, StoreError>>;
    /// Bytes `range.start..range.end` of one object, for ZIP central-directory
    /// reads (ARC1) and R2 range requests. `start <= end <= size`, otherwise
    /// `InvalidBatch("blob.invalid_range")`; an absent key is `NotFound`.
    fn get_range(
        &self,
        key: &BlobKey,
        range: Range<u64>,
    ) -> impl Future<Output = Result<Vec<u8>, StoreError>>;
    /// Size of one object without reading it (finalize's size check, D49).
    fn head(&self, key: &BlobKey) -> impl Future<Output = Result<BlobHead, StoreError>>;
    fn delete(&self, key: &BlobKey) -> impl Future<Output = Result<(), StoreError>>;
    fn list_prefix(
        &self,
        prefix: &BlobPrefix,
    ) -> impl Future<Output = Result<Vec<BlobKey>, StoreError>>;
}
