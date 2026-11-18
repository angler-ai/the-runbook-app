//! CF1 Workers spike (plan 2026-11-12 §2.6, D55): proves the portable Rust
//! core runs inside a Rust Durable Object on workerd.
//!
//! - [`spike`] is plain `runbook-core`/`runbook-engine` code: compile the
//!   spike runbook, build start and alarm-advance commits, replay events.
//!   It builds natively and on wasm32 and is unit-tested natively against
//!   the in-memory `ExecutionStore` fake.
//! - `edge` (wasm32 only) is the Worker fetch handler, the SQLite-backed
//!   `ExecutionDO` implementing `ExecutionStore` in one `transactionSync`,
//!   and the D1/R2 binding probes. `cloudflare/tests` drives it under
//!   workerd through Miniflare.
//!
//! LOCAL ONLY: built with `worker-build` and run by `wrangler dev` or
//! Miniflare. Nothing here is deployed.
pub mod archive_index;
pub mod archives;
pub mod body;
pub mod bootstrap;
pub mod cloudflare;
pub mod compose;
pub mod cron;
pub mod delivery;
pub mod import_gate;
pub mod limits;
pub mod object;
pub mod owners;
pub mod portable;
pub mod protocol;
pub mod quota;
pub mod reconcile;
pub mod run_search;
pub mod sealer;
pub mod settle;
pub mod shards;
pub mod spike;
pub mod webhooks;

// The api Worker's entry points (`fetch`, `scheduled`, `queue`) and its
// ExecutionDO class. Off for crates that link this one into another Worker
// (the conformance Worker, CF5), which has its own.
#[cfg(all(target_arch = "wasm32", feature = "edge"))]
mod auth;
#[cfg(all(target_arch = "wasm32", feature = "edge"))]
mod edge;
#[cfg(all(target_arch = "wasm32", feature = "edge"))]
mod live;
#[cfg(all(target_arch = "wasm32", feature = "edge"))]
mod notify;
#[cfg(all(target_arch = "wasm32", feature = "edge"))]
mod seams;
