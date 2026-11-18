//! CF13b review round 1: the run ceiling
//! ([`runbook_core::run_budget::run_ceiling`]) over a store's own run
//! budget. Not a cross-store case ([`crate::cross_scoped`]): each case sets
//! the budget through `with_budget`, which the composed Cloudflare store
//! takes from its host, so the memory and Postgres adapters run it.
use crate::memory_entries::{mem, save};
use runbook_core::memory_entries::{self as usecases, MemoryStore};
use runbook_model::{MemoryScope, MemoryStatus};

/// CF13b review round 1, finding 2: a run's memory writes (a proposal or
/// saved entry, a recorded read, an edit) committed without a settle, so
/// they were never held to the run ceiling ([`runbook_core::run_budget::run_ceiling`])
/// until some later request settled the run. Each now settles first: a
/// write that takes an engine version 6 run past its ceiling commits, and
/// in the same commit the run is faulted (`limit.run_size_exceeded`) and
/// closes failed. Here the budget is set so the ceiling sits within a few
/// bytes of each run's log after its claim; a proposal on one run and a
/// recorded read on the other each cross it.
pub async fn memory_writes_are_held_to_the_run_ceiling<S, D>(
    store: &S,
    dir: &D,
    with_budget: impl Fn(u64) -> S,
) where
    S: runbook_core::start::StartStore
        + runbook_core::catalog::CatalogStore
        + runbook_core::blob::BlobStore
        + runbook_core::work_queue::WorkQueue
        + runbook_core::session::SessionStore
        + runbook_core::ExecutionStore
        + MemoryStore,
    D: runbook_core::directory::DirectoryStore
        + runbook_core::directory_operations::DirectoryOperations,
{
    use crate::portable::{PLAIN, agent, directory, exe, p, rb as runbook, start};
    use runbook_core::{actor::begin_actor, claims::claim_step, run_budget};
    use runbook_model::{ClaimRequest, ExecutionStatus, MemoryReadRef};
    directory(dir).await;
    crate::parity::publish(store, 2, "plain", PLAIN, 2).await;
    let mut only = vec![];
    for n in [1, 2] {
        start(store, dir, &p(), n, 2).await;
        let work = begin_actor(store, dir, p(), &exe(n)).await.unwrap();
        let step = work
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "only")
            .and_then(|s| s.id.clone())
            .unwrap();
        claim_step(
            work,
            &agent(),
            &step,
            &ClaimRequest {
                take_over: false,
                session_id: "s1".into(),
                actor: None,
            },
            format!("clm_{n:026}").parse().unwrap(),
            None,
        )
        .await
        .unwrap();
        only.push(step);
    }
    // A store whose ceiling is the run's log now, within a few bytes.
    let tight = async |n: u8| {
        let work = begin_actor(store, dir, p(), &exe(n)).await.unwrap();
        let logged = run_budget::log_bytes(&work.snapshot.events);
        drop(work);
        let mut budget = logged * 2 / 3;
        while run_budget::run_ceiling(budget) < logged {
            budget += 1;
        }
        assert!(run_budget::run_ceiling(budget) < logged + 4);
        with_budget(budget)
    };
    let closed_for_size = async |n: u8| {
        let work = begin_actor(store, dir, p(), &exe(n)).await.unwrap();
        let state = &work.runtime.state;
        let closed = state.status == ExecutionStatus::Failed
            && state
                .runtime_errors
                .iter()
                .filter(|f| f.code == runbook_engine::RUN_SIZE_EXCEEDED)
                .count()
                == 1;
        let status = state.status;
        drop(work);
        assert!(closed, "run {n} is {status:?}");
    };
    // A proposal crosses the ceiling: it is saved, and the run closes.
    let held = tight(1).await;
    let mut request = save(
        "Baseline",
        "Normal error rate is 0.5%",
        &["b"],
        Some("only"),
    );
    request.propose = true;
    let scope_rb = MemoryScope::Runbook {
        runbook_id: runbook(2),
    };
    let work = begin_actor(&held, dir, p(), &exe(1)).await.unwrap();
    let proposed = usecases::save_in_run(work, &agent(), mem(1), scope_rb, &request, None)
        .await
        .unwrap();
    assert_eq!(proposed.status, MemoryStatus::Proposed);
    closed_for_size(1).await;
    // A recorded read crosses it on the other run.
    let held = tight(2).await;
    let work = begin_actor(&held, dir, p(), &exe(2)).await.unwrap();
    let refs = vec![MemoryReadRef {
        memory_id: mem(1),
        revision: 1,
    }];
    assert!(
        usecases::record_step_read(work, &only[1], refs, agent().actor)
            .await
            .unwrap()
    );
    closed_for_size(2).await;
}
