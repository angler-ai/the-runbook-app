//! CF4: `runbook_core::blob::BlobStore` on object storage (R2, D57).
//!
//! The logic (key layout, tenancy, immutability, ranges) is plain Rust over
//! two small traits: [`ObjectBucket`] (R2 on Workers, an in-memory map in
//! tests) and [`BlobOwners`] (who owns an execution or runbook: D1 on
//! Cloudflare, CF3). Two buckets: the main blob bucket, and a separate
//! user-content bucket for uploads and evidence, served from its own
//! hostname (D57).
pub mod bucket;
pub mod layout;
pub mod owners;
mod store;
#[cfg(target_arch = "wasm32")]
pub mod workers;

pub use bucket::{Listing, MemoryBucket, ObjectBucket};
pub use owners::{BlobOwners, MemoryOwners};
pub use store::R2BlobStore;
