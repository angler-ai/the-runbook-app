//! CF5: every cross-store case (`cross_registry::CROSS`, which must equal
//! the shared `CROSS_CASES`) and the composed store's own cases, over the
//! composed Cloudflare store natively: every run in its own in-memory
//! object behind the object protocol (`cross::InProcess`, JSON both ways),
//! D1 on rusqlite, R2 in memory. Inside workerd the same registry runs over
//! real Durable Objects, one RPC per case
//! (`cloudflare/tests/conformance.test.mjs`).
use runbook_worker_conformance::{
    cross::NativeCross,
    cross_registry::{CROSS, CROSS_OWN, run_cross_case},
    native::run,
};

/// Debug builds of the deepest cases (LANG3's start path) need more than a
/// test thread's default 2 MiB stack.
const STACK: usize = 32 * 1024 * 1024;

#[test]
fn every_cross_store_case_passes_natively() {
    let failed = std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(|| {
            let mut failed = vec![];
            for name in CROSS.iter().chain(CROSS_OWN) {
                match std::panic::catch_unwind(|| run(run_cross_case(name, NativeCross))) {
                    Ok(Some(Ok(()))) => {}
                    Ok(Some(Err(error))) => failed.push(format!("{name}: {error}")),
                    Ok(None) => failed.push(format!("{name}: unregistered")),
                    Err(_) => failed.push(format!("{name}: panicked")),
                }
            }
            failed
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(
        failed.is_empty(),
        "{} of {} failed:\n{}",
        failed.len(),
        CROSS.len() + CROSS_OWN.len(),
        failed.join("\n")
    );
}

#[test]
fn an_unknown_case_is_refused() {
    assert!(run(run_cross_case("execution::not_a_case", NativeCross)).is_none());
}
