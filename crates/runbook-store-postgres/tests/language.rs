//! Language completeness (plan §2.9) on PostgreSQL: the store test for each
//! key that moved from rejected to supported (later-plans rule 1). Each case
//! owns one database.
mod support;
use runbook_core::StoreError;
use runbook_store_conformance::{language, portable::p};
use support::factory::with_factory;

macro_rules! language_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| {
                let store = f.fresh_store(p()).await;
                language::$name(&store, &f.fresh_directory()).await;
                Ok::<(), StoreError>(())
            })
            .await;
        }
    )*};
}
language_cases!(
    loop_while_runs_and_replays,
    strict_assignment_limits_who_claims,
    a_person_takes_over_an_agent_task,
    agents_may_not_claim_strict_work_open_to_anyone,
    an_unresolved_strict_assignee_is_resolved_again_at_claim,
    input_types_and_pattern_run_and_replay,
    parallel_join_cancels_open_branches,
    parallel_join_clears_timers_of_cancelled_branches,
    on_conflict_reject_refuses_a_full_pool,
    on_conflict_cancel_previous_with_a_wider_pool_cancels_the_oldest_holder,
    on_conflict_cancel_previous_bounds_its_preparation,
    cancelling_a_parent_cancels_its_child,
    overlapping_start_leases_never_cancel_a_wanted_child,
    a_crash_between_the_child_and_its_start_record_recovers,
    a_result_before_the_start_record_is_kept,
    a_child_started_after_its_parent_was_cancelled_never_runs,
    cancelling_the_top_of_a_call_chain_cancels_every_level,
    fan_out_calls_start_one_child_per_step_run,
    a_child_is_not_deleted_before_its_result_is_delivered,
    finished_call_items_are_no_longer_listed,
);

/// LANG4 `call`: the child's run and outcome, matching the trace the
/// memory run pins (`conformance/execution/call.json`).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn call_runs_a_child_and_takes_its_outcome() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let trace =
            language::call_runs_a_child_and_takes_its_outcome(&store, &f.fresh_directory()).await;
        let pinned: serde_json::Value =
            serde_json::from_str(include_str!("../../../conformance/execution/call.json")).unwrap();
        assert_eq!(trace, pinned);
        Ok::<(), StoreError>(())
    })
    .await;
}

/// LANG4 re-check 3: a child whose refusal was recorded meanwhile is
/// cancelled.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_child_started_after_its_refusal_was_recorded_is_cancelled() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let limited = store.with_quota(runbook_core::quotas::QuotaLimits {
            executions_monthly: Some(1),
        });
        language::a_child_started_after_its_refusal_was_recorded_is_cancelled(
            &store,
            &limited,
            &f.fresh_directory(),
        )
        .await;
        Ok::<(), StoreError>(())
    })
    .await;
}

/// LANG4: a child counts toward the monthly run quota; refused, it fails
/// the call step.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_child_refused_for_quota_fails_the_call_step() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let limited = store.with_quota(runbook_core::quotas::QuotaLimits {
            executions_monthly: Some(1),
        });
        language::a_child_refused_for_quota_fails_the_call_step(
            &store,
            &limited,
            &f.fresh_directory(),
        )
        .await;
        Ok::<(), StoreError>(())
    })
    .await;
}

/// LANG3: the same key in another project is another pool.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn on_conflict_cancel_previous_leaves_other_projects_alone() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let other = f
            .fresh_store(runbook_store_conformance::portable::q())
            .await;
        language::on_conflict_cancel_previous_leaves_other_projects_alone(
            &store,
            &other,
            &f.fresh_directory(),
        )
        .await;
        Ok::<(), StoreError>(())
    })
    .await;
}

/// LANG3 `cancel_previous` (the memory run pins its trace).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn on_conflict_cancel_previous_replaces_the_holders() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        language::on_conflict_cancel_previous_replaces_the_holders(&store, &f.fresh_directory())
            .await;
        Ok::<(), StoreError>(())
    })
    .await;
}

/// LANG3 atomicity: four `cancel_previous` starts race for one key held by
/// run 1, whose `finally` keeps it holding the slot while it cleans up. Each
/// loser of a pool race prepares again, so every start commits (or, after
/// its retry budget, answers `execution.concurrency_contended` having
/// written nothing). At the end run 1 still holds the key, exactly one
/// start waits first in line and every other start was cancelled before it
/// began; when run 1's cleanup ends, the waiting start takes the slot.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn on_conflict_cancel_previous_races_keep_one_holder_and_one_waiter() {
    use futures_util::future::join_all;
    use runbook_core::{ExecutionStore, catalog::AdmissionState, catalog::CatalogStore};
    use runbook_store_conformance::portable::exe;
    with_factory(async |f| {
        let dir = f.fresh_directory();
        let setup = f.fresh_store(p()).await;
        language::on_conflict_race_setup(&setup, &dir).await;
        let mut stores = vec![];
        for _ in 0..4 {
            stores.push(f.fresh_store(p()).await);
        }
        let results = join_all(
            stores
                .iter()
                .zip(2u8..)
                .map(|(s, n)| language::on_conflict_race_start(s, &dir, n)),
        )
        .await;
        let mut started = vec![];
        for (result, n) in results.into_iter().zip(2u8..) {
            match result {
                Ok(_) => started.push(n),
                Err(runbook_core::start::StartError::ConcurrencyContended) => {
                    assert!(matches!(
                        setup.load(&exe(n)).await,
                        Err(StoreError::NotFound)
                    ));
                }
                Err(other) => panic!("run {n}: {other:?}"),
            }
        }
        assert!(!started.is_empty());
        assert_eq!(
            setup.admission(&exe(1)).await?.map(|a| a.state),
            Some(AdmissionState::Admitted)
        );
        let mut waiting = vec![];
        for n in &started {
            match setup.admission(&exe(*n)).await?.map(|a| a.state) {
                Some(AdmissionState::Queued) => waiting.push(*n),
                Some(AdmissionState::Released) => assert_eq!(
                    setup.load(&exe(*n)).await?.projection.status,
                    runbook_model::ExecutionStatus::Cancelled,
                    "run {n}"
                ),
                other => panic!("run {n}: {other:?}"),
            }
        }
        assert_eq!(waiting.len(), 1, "{started:?}");
        let active: i32 = f
            .admin
            .client()
            .query_one(
                "SELECT active FROM runbook.admission_pool WHERE key = 'prod'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(active, 1);
        // Run 1's cleanup ends: the waiting start takes the slot.
        language::on_conflict_race_finish_cleanup(&setup, &dir).await;
        assert_eq!(
            setup.admission(&exe(waiting[0])).await?.map(|a| a.state),
            Some(AdmissionState::Admitted)
        );
        assert_eq!(setup.admission_pool("prod").await?.holders.len(), 1);
        Ok::<(), StoreError>(())
    })
    .await;
}

async fn reject_race(warm: bool) {
    use futures_util::future::join_all;
    use runbook_core::{ExecutionStore, catalog::AdmissionState, catalog::CatalogStore};
    use runbook_store_conformance::portable::exe;
    with_factory(async |f| {
        let dir = f.fresh_directory();
        let setup = f.fresh_store(p()).await;
        language::on_conflict_reject_race_setup(&setup, &dir, warm).await;
        let mut stores = vec![];
        for _ in 0..4 {
            stores.push(f.fresh_store(p()).await);
        }
        let results = join_all(
            stores
                .iter()
                .zip(1u8..)
                .map(|(s, n)| language::on_conflict_reject_race_start(s, &dir, n)),
        )
        .await;
        let mut admitted = 0;
        for (result, n) in results.into_iter().zip(1u8..) {
            match result {
                Ok(outcome) => {
                    assert_eq!(
                        outcome.admission.map(|a| a.state),
                        Some(AdmissionState::Admitted),
                        "run {n}"
                    );
                    admitted += 1;
                }
                Err(e) if e.to_string() == runbook_core::concurrency::CONFLICT_CODE => {
                    assert!(matches!(
                        setup.load(&exe(n)).await,
                        Err(StoreError::NotFound)
                    ));
                    assert_eq!(setup.admission(&exe(n)).await?, None);
                }
                Err(other) => panic!("run {n}: {other:?}"),
            }
        }
        assert_eq!(admitted, 1);
        Ok::<(), StoreError>(())
    })
    .await;
}

/// LANG3 `reject` on a brand-new key: four starts race, one is admitted and
/// the rest are refused; none is queued.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn on_conflict_reject_races_on_a_new_key_admit_one() {
    reject_race(false).await;
}

/// The same on a key whose pool already exists and is idle: the locked pool
/// row is what serializes the starts.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn on_conflict_reject_races_on_an_idle_key_admit_one() {
    reject_race(true).await;
}
