//! CF3: the cross-execution stores of the Cloudflare edition on D1 (D56).
//!
//! The adapters are plain Rust over [`sql::D1`], a small async trait
//! (`all`, `run`, an atomic `batch`, the host clock). On Workers it is
//! `workers::WorkersD1` over the `DB` binding; native tests use
//! `native::NativeD1` over rusqlite (feature `native`).
//!
//! D1 has no interactive transactions. Every write that depends on what it
//! read is planned from one consistent read batch, then written as one
//! batch that starts with guards ([`sql::guard`]) re-checking what the plan
//! relied on (a revision, a state, an absent row). A failed guard aborts the
//! whole batch, so a racing writer turns this one into a conflict, never a
//! partial write.
pub mod directory;
pub mod migrate;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod native;
pub mod sql;
pub mod testing;
#[cfg(target_arch = "wasm32")]
pub mod workers;

pub use directory::D1Directory;
pub use sql::{D1, D1Error, Statement, Value, guard, prepare};
pub mod admission;
pub mod applier;
pub mod archives;
pub mod bootstrap;
pub mod catalog;
pub mod credentials;
pub mod listing;
pub mod me_tasks;
pub mod memory;
pub mod notify;
pub mod portable;
pub mod replay;
pub mod retention;
pub mod search;
pub mod sources;
pub mod start;
pub mod uploads;
pub mod webhooks;

pub use catalog::D1Catalog;
pub use credentials::D1Credentials;
pub use memory::D1Memory;
pub use notify::D1Notify;
pub use uploads::D1Uploads;
pub use webhooks::D1Webhooks;

use runbook_core::StoreError;

/// Every guard code the adapters use, so a guard's code can surface as a
/// `&'static str` error code (`StoreError::InvalidBatch`). A test checks
/// that every `guard("…")` in the sources is listed.
pub(crate) const GUARD_CODES: &[&str] = &[
    "conflict",
    "duplicate",
    "not_found",
    "invitation",
    "replayed",
    "recorded",
    "health",
    "webhooks.replay_exists",
    "webhooks.invalid_delivery",
    "webhooks.limit_reached",
    "webhooks.conflict",
    "webhooks.stale_lease",
    "memory.duplicate",
    "import.project_conflict",
    "import.target_not_empty",
    "import.id_conflict",
    "memory.limit_reached",
    "memory.applied",
    "memory.conflict",
    "catalog.admission_stale",
    "catalog.admission_conflict",
    "catalog.execution_scope_conflict",
    "catalog.draft_conflict",
    "catalog.publish_race",
    "store.execution_exists",
    "bootstrap.already_initialized",
    "upload.limit_reached",
    "upload.not_pending",
    "catalog.admission_not_released",
    "archive.not_current",
    "archive.conflict",
    "archive.blob_missing",
    "archive.run_gone",
    "archive.member_live",
];

/// The `'static` form of a guard code (`d1.guard` for an unlisted one).
pub(crate) fn static_code(code: &str) -> &'static str {
    GUARD_CODES
        .iter()
        .find(|known| **known == code)
        .copied()
        .unwrap_or("d1.guard")
}

/// A D1 failure as a store error. A guard's code becomes the batch's
/// `InvalidBatch` code; a racing primary key is a backend unique violation,
/// as on Postgres; driver text never leaves the adapter.
/// The store error of a D1 error. Natively, `RB_D1_DEBUG=1` prints what
/// the codes hide (the SQL error's message; where a column read failed): a
/// debugging aid for adapter work, never on in Workers.
/// A D1 error as a store error (guard codes become `InvalidBatch`).
pub fn sql_store_error(error: D1Error) -> StoreError {
    store_error(error)
}
pub(crate) fn store_error(error: D1Error) -> StoreError {
    match error {
        D1Error::Guard(code) if code == "not_found" => StoreError::NotFound,
        D1Error::Guard(code) => StoreError::InvalidBatch(static_code(&code)),
        D1Error::Unique => StoreError::Backend("d1.unique_violation"),
        D1Error::Foreign => StoreError::Backend("d1.foreign_key"),
        D1Error::Range => {
            #[cfg(not(target_arch = "wasm32"))]
            if std::env::var_os("RB_D1_DEBUG").is_some() {
                eprintln!("d1 range: {}", std::backtrace::Backtrace::force_capture());
            }
            StoreError::Backend("d1.decode")
        }
        D1Error::Other(_message) => {
            #[cfg(not(target_arch = "wasm32"))]
            if std::env::var_os("RB_D1_DEBUG").is_some() {
                eprintln!("d1 error: {_message}");
            }
            StoreError::Backend("d1.database_error")
        }
    }
}

#[cfg(test)]
mod tests {
    /// Every `guard("code", …)` literal in the sources is in `GUARD_CODES`.
    #[test]
    fn every_guard_code_is_listed() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.ends_with("lib.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let mut rest = source.as_str();
            while let Some(at) = rest.find("guard(") {
                rest = &rest[at + "guard(".len()..];
                let trimmed = rest.trim_start();
                if let Some(literal) = trimmed.strip_prefix('"') {
                    let code = &literal[..literal.find('"').unwrap()];
                    assert!(
                        super::GUARD_CODES.contains(&code),
                        "{}: guard code {code:?} is not in GUARD_CODES",
                        path.display()
                    );
                }
            }
        }
    }
}
