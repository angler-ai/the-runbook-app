//! Cases registered red at the T0 contract freeze (plan 2026-11-12 §1).
//!
//! Why a list and not `#[ignore]`: `scripts/check.sh` runs every ignored test
//! when `DATABASE_URL` is set (the Postgres suite), so an ignored red case
//! would break that step. Instead each case runs on every `cargo test` and
//! [`settle`] decides: a case named here must fail with its package's
//! `runbook_core::pending` code (the frozen stub); any other outcome fails
//! the test. When a package makes its case pass, the test fails until the
//! entry is deleted here, so the list cannot go stale and nothing passes by
//! accident.
use runbook_core::{
    StoreError, credentials::CredentialError, directory::DirectoryError, pending::is_unimplemented,
};
use std::fmt::Debug;

/// One red case: its `module::function` name and the package that turns it green.
#[derive(Clone, Copy, Debug)]
pub struct Pending {
    pub case: &'static str,
    pub package: &'static str,
}

pub const PACKAGES: &[&str] = &["MEM1", "UP1", "ARC1"];

pub const PENDING: &[Pending] = &[];

/// Every case the T0 freeze registered, pending or not. Adapters iterate it
/// so a backend cannot skip one (CF5 adds the workerd harness on top).
pub const REGISTERED: &[&str] = &[
    "memory_entries::stale_revision_is_refused",
    "memory_entries::foreign_and_expired_entries_are_hidden",
    "uploads::slots_count_limits_and_finalize_once",
    "uploads::upload_blobs_support_head_and_ranges",
    "archives::index_round_trip_and_stale_marks",
    "archives::archive_blobs_are_tenant_scoped",
    "auth_links::invitation_links_once_and_applies_role",
    "device_codes::poll_consumes_an_approval_once",
    "auth_links::links_refuse_uninvited_and_stale",
    "device_codes::denial_expiry_and_collisions",
];

/// Errors a frozen stub can return.
pub trait StubError: Debug {
    fn is_stub(&self) -> bool;
}
impl StubError for StoreError {
    fn is_stub(&self) -> bool {
        matches!(self, StoreError::Backend(code) if is_unimplemented(code))
    }
}
impl StubError for DirectoryError {
    fn is_stub(&self) -> bool {
        matches!(self, DirectoryError::Backend(code) if is_unimplemented(code))
    }
}
impl StubError for CredentialError {
    fn is_stub(&self) -> bool {
        matches!(self, CredentialError::Backend(code) if is_unimplemented(code))
    }
}

pub fn pending(case: &str) -> Option<&'static Pending> {
    PENDING.iter().find(|p| p.case == case)
}

/// Judge one case's result (see the module docs). Panics, with a message
/// saying what to do, whenever the outcome is not the expected one.
pub fn settle<E: StubError>(case: &str, result: Result<(), E>) {
    assert!(REGISTERED.contains(&case), "{case} is not in REGISTERED");
    match (pending(case), result) {
        (None, result) => {
            if let Err(e) = result {
                panic!("{case} failed: {e:?}");
            }
        }
        (Some(p), Ok(())) => panic!(
            "{case} now passes: {} implemented it, so delete its entry from \
             runbook-store-conformance/src/pending.rs PENDING",
            p.package
        ),
        (Some(_), Err(e)) if e.is_stub() => {}
        (Some(p), Err(e)) => panic!(
            "{case} (pending {}) failed with {e:?}, not the frozen stub: \
             finish the implementation until it passes",
            p.package
        ),
    }
}
