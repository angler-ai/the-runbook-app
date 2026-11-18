//! CF5: every cross-store case by name, run over a [`CrossHost`] (natively
//! in `tests/cross.rs`, in workerd one RPC per case). [`CROSS`] must equal
//! `runbook_store_conformance::cross_scoped::CROSS_CASES`, checked in both
//! places, so a case cannot be skipped. [`CROSS_OWN`] lists the composed
//! store's own cases (`cross_cases.rs`).
//!
//! Cases take different arguments, so each is listed under how it is
//! called: a factory; a store from `create`; a store and a directory at the
//! portable tenant; two tenants' stores and a directory (`parity`).
use crate::cross::{CrossFactory, CrossHost};
use crate::host::CaseOutcome;
use runbook_store_conformance::{
    StoreFactory, execution,
    portable::{p, q},
};

/// The portable tenant's clock (the memory harness's).
const T0: i64 = 1_700_000_000_000;

/// LANG3: the store-level trace every adapter must produce.
const ON_CONFLICT_TRACE: &str =
    include_str!("../../../conformance/execution/concurrency-on-conflict.json");
/// LANG4: the `call` trace every adapter must produce.
const CALL_TRACE: &str = include_str!("../../../conformance/execution/call.json");

macro_rules! cross_registry {
    (
        factory: [$($fm:ident :: $fc:ident),* $(,)?],
        store: [$($sm:ident :: $sc:ident),* $(,)?],
        with_directory: [$($dm:ident :: $dc:ident),* $(,)?],
        parity: [$($pc:ident),* $(,)?],
        own: [$($oc:ident),* $(,)?] $(,)?
    ) => {
        /// The shared cross-store cases this harness runs.
        pub const CROSS: &[&str] = &[
            $(concat!(stringify!($fm), "::", stringify!($fc)),)*
            $(concat!(stringify!($sm), "::", stringify!($sc)),)*
            $(concat!(stringify!($dm), "::", stringify!($dc)),)*
            $(concat!("parity::", stringify!($pc)),)*
            "language::on_conflict_cancel_previous_leaves_other_projects_alone",
            "language::on_conflict_cancel_previous_replaces_the_holders",
            "language::call_runs_a_child_and_takes_its_outcome",
            "language::a_child_refused_for_quota_fails_the_call_step",
            "language::a_child_started_after_its_refusal_was_recorded_is_cancelled",
            "parity::execution_credential_reads_are_scoped",
            "parity::catalog_publication_reads_are_scoped",
        ];
        /// The composed store's own cases, as `cf::function`.
        pub const CROSS_OWN: &[&str] = &[$(concat!("cf::", stringify!($oc))),*];

        /// Run one registered cross-store case on a fresh factory over
        /// `host`; `None` if the name is unknown.
        pub async fn run_cross_case<H: CrossHost>(name: &str, host: H) -> Option<Result<(), String>> {
            let f = CrossFactory::new(host);
            let f = &f;
            // Each case's future is boxed: one match over every case would
            // otherwise be one future the size of the largest case.
            let case: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + '_>> = match name {
                $(concat!(stringify!($fm), "::", stringify!($fc)) =>
                    Box::pin(async move { runbook_store_conformance::$fm::$fc(f).await.outcome() }),)*
                $(concat!(stringify!($sm), "::", stringify!($sc)) => Box::pin(async move {
                    match f.create(execution::projection()).await {
                        Ok(store) => runbook_store_conformance::$sm::$sc(&store).await.outcome(),
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),)*
                $(concat!(stringify!($dm), "::", stringify!($dc)) => Box::pin(async move {
                    match f.language(p(), T0).await {
                        Ok((store, directory)) => {
                            let _ = runbook_store_conformance::$dm::$dc(&store, &directory).await;
                            Ok(())
                        }
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),)*
                $(concat!("parity::", stringify!($pc)) => Box::pin(async move {
                    match f.language(p(), T0).await {
                        Ok((ps, directory)) => {
                            let qs = ps.scoped(q());
                            runbook_store_conformance::parity::$pc(&ps, &qs, &directory).await;
                            Ok(())
                        }
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),)*
                "language::on_conflict_cancel_previous_leaves_other_projects_alone" => Box::pin(async move {
                    match f.language(p(), T0).await {
                        Ok((store, directory)) => {
                            let other = store.scoped(q());
                            runbook_store_conformance::language::on_conflict_cancel_previous_leaves_other_projects_alone(
                                &store, &other, &directory,
                            )
                            .await;
                            Ok(())
                        }
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),
                "language::on_conflict_cancel_previous_replaces_the_holders" => Box::pin(async move {
                    match f.language(p(), T0).await {
                        Ok((store, directory)) => {
                            let trace = runbook_store_conformance::language::on_conflict_cancel_previous_replaces_the_holders(
                                &store, &directory,
                            )
                            .await;
                            match serde_json::from_str::<serde_json::Value>(ON_CONFLICT_TRACE) {
                                Ok(pinned) if pinned == trace => Ok(()),
                                Ok(_) => Err(format!("trace differs from the pinned fixture: {trace}")),
                                Err(error) => Err(format!("fixture: {error}")),
                            }
                        }
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),
                "language::call_runs_a_child_and_takes_its_outcome" => Box::pin(async move {
                    match f.language(p(), T0).await {
                        Ok((store, directory)) => {
                            let trace = runbook_store_conformance::language::call_runs_a_child_and_takes_its_outcome(
                                &store, &directory,
                            )
                            .await;
                            match serde_json::from_str::<serde_json::Value>(CALL_TRACE) {
                                Ok(pinned) if pinned == trace => Ok(()),
                                Ok(_) => Err(format!("trace differs from the pinned fixture: {trace}")),
                                Err(error) => Err(format!("fixture: {error}")),
                            }
                        }
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),
                "language::a_child_refused_for_quota_fails_the_call_step" => Box::pin(async move {
                    match f.language(p(), T0).await {
                        Ok((store, directory)) => {
                            let limited = store.with_quota(runbook_core::quotas::QuotaLimits {
                                executions_monthly: Some(1),
                            });
                            runbook_store_conformance::language::a_child_refused_for_quota_fails_the_call_step(
                                &store, &limited, &directory,
                            )
                            .await;
                            Ok(())
                        }
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),
                "language::a_child_started_after_its_refusal_was_recorded_is_cancelled" => Box::pin(async move {
                    match f.language(p(), T0).await {
                        Ok((store, directory)) => {
                            let limited = store.with_quota(runbook_core::quotas::QuotaLimits {
                                executions_monthly: Some(1),
                            });
                            runbook_store_conformance::language::a_child_started_after_its_refusal_was_recorded_is_cancelled(
                                &store, &limited, &directory,
                            )
                            .await;
                            Ok(())
                        }
                        Err(error) => Err(format!("{error:?}")),
                    }
                }),
                "parity::execution_credential_reads_are_scoped" => Box::pin(async move { match f.language(p(), T0).await {
                    Ok((ps, directory)) => {
                        let qs = ps.scoped(q());
                        match f.db() {
                            Ok(db) => {
                                let credentials = runbook_store_d1::D1Credentials::new(db);
                                runbook_store_conformance::parity::execution_credential_reads_are_scoped(
                                    &ps, &qs, &directory, &credentials,
                                )
                                .await;
                                Ok(())
                            }
                            Err(error) => Err(format!("{error:?}")),
                        }
                    }
                    Err(error) => Err(format!("{error:?}")),
                }}),
                "parity::catalog_publication_reads_are_scoped" => Box::pin(async move { match f.language(p(), T0).await {
                    Ok((ps, _)) => {
                        let qs = ps.scoped(q());
                        runbook_store_conformance::parity::catalog_publication_reads_are_scoped(&ps, &qs)
                            .await;
                        Ok(())
                    }
                    Err(error) => Err(format!("{error:?}")),
                }}),
                $(concat!("cf::", stringify!($oc)) =>
                    Box::pin(async move { crate::cross_cases::$oc(f).await.outcome() }),)*
                _ => return None,
            };
            Some(case.await)
        }
    };
}

cross_registry!(
    factory: [
        archives::archive_blobs_are_tenant_scoped,
        archives::stale_marks_follow_commits_and_deletions,
        archives::retention_queries_respect_archives_and_tenants,
        archives::retention_deletion_rechecks_its_file,
        archives::evidence_follows_the_archive_and_retained_runs_can_be_purged,
        archives::finalized_evidence_marks_its_file_stale,
        archives::failing_days_back_off,
        archives::versions_are_swept_only_behind_the_index,
        archives::privacy_deletions_find_their_file_after_the_summary_purge,
        archives::a_record_never_clears_a_later_mark_or_misses_evidence,
        archives::retained_evidence_counts_until_its_file_expires,
        archives::expiry_deletes_every_upload_under_its_retained_runs,
        archives::a_live_built_folder_is_refused_for_a_retained_run,
        catalog::project_queries_never_return_foreign_rows,
        catalog::revision_precondition_rejects_stale_replace,
        catalog::derived_rows_ignore_old_or_duplicate_sequence,
        catalog::concurrency_limit_one_admits_and_queues,
        catalog::derived_rows_rollback_with_execution,
        catalog::derived_rows_cannot_release_running_execution,
        catalog::pool_limit_replacement_promotes_waiters,
        clock::store_clock_controls_deadline_equality,
        clock::session_time_and_snapshot_are_fenced,
        clock::session_commits_at_its_begin_time,
        execution::atomic_batch_or_nothing,
        execution::stale_commit_has_no_side_effects,
        execution::one_claim_generation_winner,
        execution::idempotent_response_is_payload_bound,
        execution::replay_equals_projection,
        memory_entries::proposals_commit_with_the_execution,
        memory_entries::proposals_racing_a_full_quota_are_never_lost,
        memory_entries::runs_save_live_entries,
        memory_entries::agent_writes_commit_a_notice,
        queue::due_item_has_one_current_worker,
        queue::stale_worker_cannot_ack_successor,
        queue::delivery_leases_are_scoped_and_fenced,
        queue::timer_replacements_require_new_generation,
        quotas::monthly_starts_are_counted_capped_and_roll_over,
        retention::export_contains_one_complete_run,
        retention::delete_leaves_only_tombstone,
        retention::finished_day_uses_utc_half_open_interval,
        retention::deletion_is_atomic_and_rejects_open_runs,
        search::indexed_severity_tracks_commits,
        search::search_is_tenant_scoped_and_paginated,
        search::numeric_range_ignores_non_number_values,
        search::inbox_tracks_current_access_and_completion,
        search::runbook_run_stats_group_by_runbook,
        uploads::slots_count_limits_and_finalize_once,
        uploads::stale_slots_are_listed_and_deleted,
        uploads::quotas_count_live_slots,
        uploads::storage_counts_stored_bytes,
        uploads::run_deletion_removes_its_uploads,
    ],
    store: [
        archives::index_round_trip_and_stale_marks,
        archives::the_index_swings_only_from_the_expected_file,
    ],
    with_directory: [
        language::loop_while_runs_and_replays,
        language::strict_assignment_limits_who_claims,
        language::a_person_takes_over_an_agent_task,
        language::agents_may_not_claim_strict_work_open_to_anyone,
        language::an_unresolved_strict_assignee_is_resolved_again_at_claim,
        language::input_types_and_pattern_run_and_replay,
        language::parallel_join_cancels_open_branches,
        language::parallel_join_clears_timers_of_cancelled_branches,
        language::on_conflict_reject_refuses_a_full_pool,
        language::on_conflict_cancel_previous_with_a_wider_pool_cancels_the_oldest_holder,
        language::on_conflict_cancel_previous_bounds_its_preparation,
        language::cancelling_a_parent_cancels_its_child,
        language::overlapping_start_leases_never_cancel_a_wanted_child,
        language::a_crash_between_the_child_and_its_start_record_recovers,
        language::a_result_before_the_start_record_is_kept,
        language::a_child_started_after_its_parent_was_cancelled_never_runs,
        language::cancelling_the_top_of_a_call_chain_cancels_every_level,
        language::fan_out_calls_start_one_child_per_step_run,
        language::a_child_is_not_deleted_before_its_result_is_delivered,
        language::finished_call_items_are_no_longer_listed,
        memory_entries::proposals_are_capped_and_reads_audited,
        memory_entries::runs_save_live_memory_and_agents_only_edit,
        memory_entries::runs_edit_under_a_claim_and_agents_learn_nothing_hidden,
        uploads::reports_attach_finalized_uploads,
        uploads::appended_logs_satisfy_log_evidence,
    ],
    parity: [
        claim_store_is_fenced_and_scoped,
        start_store_reads_and_cancel_are_scoped,
        org_routing_reads_are_org_scoped,
        blob_delete_and_listing_are_scoped,
        foreign_writes_and_counts_are_refused,
    ],
    own: [
        step_logs_move_to_r2_in_chunks,
        a_log_chunk_put_before_a_deploy_is_recorded,
        quota_reservations_are_compensated,
        quota_sweep_refunds_runs_that_never_ran,
    ],
);

#[cfg(test)]
mod tests {
    #[test]
    fn every_cross_store_case_is_registered() {
        runbook_store_conformance::cross_scoped::assert_cross_registered(
            "runbook-worker-conformance",
            super::CROSS,
        );
    }
}
