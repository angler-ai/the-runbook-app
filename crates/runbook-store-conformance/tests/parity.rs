//! Store-trait parity cases on the memory adapters (store hardening). The
//! PostgreSQL adapter runs the same cases in its own `tests/parity.rs`.
use runbook_core::{
    credentials::memory::MemoryCredentials, directory::memory::MemoryDirectory,
    memory::MemoryExecutionStore,
};
use runbook_store_conformance::{
    parity,
    portable::{p, q},
};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory adapters must be synchronous"),
    }
}
const T0: i64 = 1_700_000_000_000;

fn stores() -> (MemoryExecutionStore, MemoryExecutionStore, MemoryDirectory) {
    let directory = MemoryDirectory::default();
    directory.set_time_ms(T0);
    let p_store = MemoryExecutionStore::empty(p(), T0);
    let q_store = p_store.scoped(q());
    (p_store, q_store, directory)
}

#[test]
fn claim_store_is_fenced_and_scoped() {
    let (ps, qs, dir) = stores();
    ready(parity::claim_store_is_fenced_and_scoped(&ps, &qs, &dir));
}
#[test]
fn start_store_reads_and_cancel_are_scoped() {
    let (ps, qs, dir) = stores();
    ready(parity::start_store_reads_and_cancel_are_scoped(
        &ps, &qs, &dir,
    ));
}
#[test]
fn execution_credential_reads_are_scoped() {
    let (ps, qs, dir) = stores();
    let credentials = MemoryCredentials::default();
    ready(parity::execution_credential_reads_are_scoped(
        &ps,
        &qs,
        &dir,
        &credentials,
    ));
}
#[test]
fn org_routing_reads_are_org_scoped() {
    let (ps, qs, dir) = stores();
    ready(parity::org_routing_reads_are_org_scoped(&ps, &qs, &dir));
}
#[test]
fn catalog_publication_reads_are_scoped() {
    let (ps, qs, _) = stores();
    ready(parity::catalog_publication_reads_are_scoped(&ps, &qs));
}
#[test]
fn blob_delete_and_listing_are_scoped() {
    let (ps, qs, dir) = stores();
    ready(parity::blob_delete_and_listing_are_scoped(&ps, &qs, &dir));
}
#[test]
fn foreign_writes_and_counts_are_refused() {
    let (ps, qs, dir) = stores();
    ready(parity::foreign_writes_and_counts_are_refused(
        &ps, &qs, &dir,
    ));
}
