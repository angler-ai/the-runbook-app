//! STO1: ObjectStorage wrapping `Arc<dyn ObjectStore>` and a key prefix.
use crate::layout::object_path;
use object_store::{ObjectStore, path::Path};
use runbook_core::{blob::BlobKey, catalog::TenantScope};
use std::sync::Arc;

#[derive(Clone)]
pub struct ObjectStorage {
    pub store: Arc<dyn ObjectStore>,
    pub prefix: String,
}

impl ObjectStorage {
    pub fn new(store: Arc<dyn ObjectStore>, prefix: impl Into<String>) -> Self {
        Self {
            store,
            prefix: prefix.into(),
        }
    }

    pub fn path(&self, scope: &TenantScope, key: &BlobKey, sha256: &[u8; 32]) -> Path {
        object_path(&self.prefix, scope, key, sha256)
    }
}
