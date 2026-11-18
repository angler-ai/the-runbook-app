//! Gate fix B1: the local test seams. Each is a Worker var read only by the
//! test build (`--features test-seams`, cloudflare/build-test). The deploy
//! build never reads them, and refuses to serve while any is set
//! ([`set_in`]), so a deploy cannot carry a seam it silently ignores.
//!
//! - `RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS`: runs the archive and
//!   retention pass ahead of the clock (deletes data early).
//! - `RUNBOOK_TEST_HOLD_WORK`: holds every run's background work.
//! - `WEBHOOKS_ALLOW_PRIVATE`: private and plain-http webhook targets.
//! - `SSE_KEEPALIVE_MS`, `SSE_LIFETIME_MS`: the stream's keep-alive (and so
//!   its D32 re-check) and lifetime; fixed at native's 30 s and 1 h.
//! - `CF1_FAULT_INJECTION`: the CF1/CF2 spike routes and fault injection.
//! - `RUNBOOK_TEST_RELEASE`: the release `/v1/meta` reports (UPD1: the
//!   update tests run one build as two releases).
//! - `RUNBOOK_TEST_IMPORT_SUBREQUESTS`: the subrequest budget of a portable
//!   import (CF12: the boundary test uses the fixture's own cost).
//!
//! The test build also reports the isolate's wasm memory on an import's
//! answer ([`wasm_memory_bytes`], CF12).
use worker::Env;

pub const VARS: &[&str] = &[
    "RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS",
    "RUNBOOK_TEST_HOLD_WORK",
    "WEBHOOKS_ALLOW_PRIVATE",
    "SSE_KEEPALIVE_MS",
    "SSE_LIFETIME_MS",
    "CF1_FAULT_INJECTION",
    "RUNBOOK_TEST_RELEASE",
    "RUNBOOK_TEST_IMPORT_SUBREQUESTS",
];

/// Whether this is the test build.
pub const ENABLED: bool = cfg!(feature = "test-seams");

/// A seam's value: always `None` in the deploy build.
pub fn var(env: &Env, name: &str) -> Option<String> {
    if ENABLED {
        env.var(name).ok().map(|v| v.to_string())
    } else {
        None
    }
}

/// Whether a seam is `on` (test build only).
pub fn on(env: &Env, name: &str) -> bool {
    var(env, name).is_some_and(|v| v == "on")
}

/// The deploy build: the first seam var that is set (it refuses to serve).
pub fn set_in(env: &Env) -> Option<&'static str> {
    if ENABLED {
        return None;
    }
    VARS.iter()
        .copied()
        .find(|name| env.var(name).is_ok() || env.secret(name).is_ok())
}

/// CF12: the isolate's wasm linear memory in bytes (its high-water mark: it
/// never shrinks). Test build on wasm32 only.
pub fn wasm_memory_bytes() -> Option<u64> {
    #[cfg(target_arch = "wasm32")]
    if ENABLED {
        return Some(core::arch::wasm32::memory_size(0) as u64 * 65_536);
    }
    None
}
