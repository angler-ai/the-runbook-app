//! T0 contract freeze (plan 2026-11-12 §1): error codes the in-memory and
//! Postgres adapters return from a frozen trait method whose behavior a later
//! package owns. Each code names that package, so a failure says who fills it
//! in. The codes ride the existing `Backend` error variants (no new variant,
//! so no route mapping changes); a host that reaches one answers 503.
//!
//! When a package implements its trait it deletes its stubs and its code
//! here, and the conformance crate's `PENDING` list loses its entries.

/// True for a code above: the conformance harness uses it to tell a
/// pending (expected red) case from a real failure.
pub fn is_unimplemented(code: &str) -> bool {
    code.starts_with("unimplemented.")
}
