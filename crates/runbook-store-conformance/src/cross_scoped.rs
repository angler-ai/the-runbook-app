//! The cross-store subset of the shared cases (CF5, plan 2026-11-12 §2.6):
//! every shared case except the blob tier ([`crate::execution_scoped::BLOB_CASES`],
//! run against a bucket alone) and the D1-scoped ones
//! ([`crate::d1_scoped::D1_CASES`], run against D1 alone). On Cloudflare
//! these run over the composed store (D1, R2 and one ExecutionDO per run),
//! inside workerd. The execution-scoped cases are here too: the composed
//! store must pass them through its object protocol as well.
//!
//! [`ALL_CASES`] is every shared case; a test re-derives it from this
//! crate's sources (every `pub async fn` of a case module that is not a
//! listed [`HELPERS`] function), so a new case fails until it is listed,
//! and every harness calls [`assert_cross_registered`] with what it runs.

/// `pub async fn`s of the case modules that are fixtures, not cases.
pub const HELPERS: &[&str] = &[
    "archives::seeded_day",
    "archives::seeded_runs",
    "archives::record_first",
    "archives::put_version",
    "archives::version_stored",
    "archives::pending_upload",
    "archives::finalize_upload",
    "archives::record_file",
    "language::on_conflict_race_setup",
    "language::on_conflict_race_finish_cleanup",
    "language::on_conflict_race_start",
    "language::on_conflict_reject_race_setup",
    "language::on_conflict_reject_race_start",
    "language::drain_calls",
    "parity::publish",
    "portable::directory",
    "portable::start",
    "portable::build",
    "portable::status",
];

/// The case modules ([`ALL_CASES`] is derived from these sources).
pub const MODULES: &[(&str, &str)] = &[
    ("archives", include_str!("archives.rs")),
    ("auth_links", include_str!("auth_links.rs")),
    ("blob", include_str!("blob.rs")),
    ("catalog", include_str!("catalog.rs")),
    ("clock", include_str!("clock.rs")),
    ("credentials", include_str!("credentials.rs")),
    ("device_codes", include_str!("device_codes.rs")),
    ("directory", include_str!("directory.rs")),
    ("eligibility", include_str!("eligibility.rs")),
    ("execution", include_str!("execution.rs")),
    ("language", include_str!("language.rs")),
    ("memory_entries", include_str!("memory_entries.rs")),
    ("notify", include_str!("notify.rs")),
    ("parity", include_str!("parity.rs")),
    ("portable", include_str!("portable.rs")),
    ("queue", include_str!("queue.rs")),
    ("quotas", include_str!("quotas.rs")),
    ("retention", include_str!("retention.rs")),
    ("search", include_str!("search.rs")),
    ("uploads", include_str!("uploads.rs")),
    ("webhooks", include_str!("webhooks.rs")),
];

/// Every shared cross-store case, as `module::function`.
pub const CROSS_CASES: &[&str] = &[
    "archives::index_round_trip_and_stale_marks",
    "archives::the_index_swings_only_from_the_expected_file",
    "archives::archive_blobs_are_tenant_scoped",
    "archives::stale_marks_follow_commits_and_deletions",
    "archives::retention_queries_respect_archives_and_tenants",
    "archives::retention_deletion_rechecks_its_file",
    "archives::evidence_follows_the_archive_and_retained_runs_can_be_purged",
    "archives::finalized_evidence_marks_its_file_stale",
    "archives::failing_days_back_off",
    "archives::versions_are_swept_only_behind_the_index",
    "archives::privacy_deletions_find_their_file_after_the_summary_purge",
    "archives::a_record_never_clears_a_later_mark_or_misses_evidence",
    "archives::retained_evidence_counts_until_its_file_expires",
    "archives::expiry_deletes_every_upload_under_its_retained_runs",
    "archives::a_live_built_folder_is_refused_for_a_retained_run",
    "catalog::project_queries_never_return_foreign_rows",
    "catalog::revision_precondition_rejects_stale_replace",
    "catalog::derived_rows_ignore_old_or_duplicate_sequence",
    "catalog::concurrency_limit_one_admits_and_queues",
    "catalog::derived_rows_rollback_with_execution",
    "catalog::derived_rows_cannot_release_running_execution",
    "catalog::pool_limit_replacement_promotes_waiters",
    "clock::store_clock_controls_deadline_equality",
    "clock::session_time_and_snapshot_are_fenced",
    "clock::session_commits_at_its_begin_time",
    "execution::atomic_batch_or_nothing",
    "execution::stale_commit_has_no_side_effects",
    "execution::one_claim_generation_winner",
    "execution::idempotent_response_is_payload_bound",
    "execution::replay_equals_projection",
    "language::loop_while_runs_and_replays",
    "language::strict_assignment_limits_who_claims",
    "language::a_person_takes_over_an_agent_task",
    "language::agents_may_not_claim_strict_work_open_to_anyone",
    "language::an_unresolved_strict_assignee_is_resolved_again_at_claim",
    "language::input_types_and_pattern_run_and_replay",
    "language::parallel_join_cancels_open_branches",
    "language::parallel_join_clears_timers_of_cancelled_branches",
    "language::on_conflict_reject_refuses_a_full_pool",
    "language::on_conflict_cancel_previous_replaces_the_holders",
    "language::on_conflict_cancel_previous_with_a_wider_pool_cancels_the_oldest_holder",
    "language::on_conflict_cancel_previous_leaves_other_projects_alone",
    "language::on_conflict_cancel_previous_bounds_its_preparation",
    "language::call_runs_a_child_and_takes_its_outcome",
    "language::cancelling_a_parent_cancels_its_child",
    "language::a_child_refused_for_quota_fails_the_call_step",
    "language::a_child_started_after_its_refusal_was_recorded_is_cancelled",
    "language::overlapping_start_leases_never_cancel_a_wanted_child",
    "language::a_crash_between_the_child_and_its_start_record_recovers",
    "language::a_result_before_the_start_record_is_kept",
    "language::a_child_started_after_its_parent_was_cancelled_never_runs",
    "language::cancelling_the_top_of_a_call_chain_cancels_every_level",
    "language::fan_out_calls_start_one_child_per_step_run",
    "language::a_child_is_not_deleted_before_its_result_is_delivered",
    "language::finished_call_items_are_no_longer_listed",
    "memory_entries::proposals_commit_with_the_execution",
    "memory_entries::proposals_racing_a_full_quota_are_never_lost",
    "memory_entries::proposals_are_capped_and_reads_audited",
    "memory_entries::runs_save_live_entries",
    "memory_entries::runs_save_live_memory_and_agents_only_edit",
    "memory_entries::agent_writes_commit_a_notice",
    "memory_entries::runs_edit_under_a_claim_and_agents_learn_nothing_hidden",
    "parity::claim_store_is_fenced_and_scoped",
    "parity::start_store_reads_and_cancel_are_scoped",
    "parity::execution_credential_reads_are_scoped",
    "parity::org_routing_reads_are_org_scoped",
    "parity::catalog_publication_reads_are_scoped",
    "parity::blob_delete_and_listing_are_scoped",
    "parity::foreign_writes_and_counts_are_refused",
    "queue::due_item_has_one_current_worker",
    "queue::stale_worker_cannot_ack_successor",
    "queue::delivery_leases_are_scoped_and_fenced",
    "queue::timer_replacements_require_new_generation",
    "quotas::monthly_starts_are_counted_capped_and_roll_over",
    "retention::export_contains_one_complete_run",
    "retention::delete_leaves_only_tombstone",
    "retention::finished_day_uses_utc_half_open_interval",
    "retention::deletion_is_atomic_and_rejects_open_runs",
    "search::indexed_severity_tracks_commits",
    "search::search_is_tenant_scoped_and_paginated",
    "search::numeric_range_ignores_non_number_values",
    "search::inbox_tracks_current_access_and_completion",
    "search::runbook_run_stats_group_by_runbook",
    "uploads::slots_count_limits_and_finalize_once",
    "uploads::stale_slots_are_listed_and_deleted",
    "uploads::quotas_count_live_slots",
    "uploads::storage_counts_stored_bytes",
    "uploads::run_deletion_removes_its_uploads",
    "uploads::reports_attach_finalized_uploads",
    "uploads::appended_logs_satisfy_log_evidence",
];

/// Every shared case of the [`MODULES`], as `module::function`.
pub fn all_cases() -> Vec<String> {
    let mut out = vec![];
    for (module, source) in MODULES {
        let mut rest = *source;
        while let Some(at) = rest.find("pub async fn ") {
            rest = &rest[at + "pub async fn ".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let full = format!("{module}::{name}");
            if !name.is_empty() && !HELPERS.contains(&full.as_str()) {
                out.push(full);
            }
        }
    }
    out
}

/// Panic unless `registered` is exactly [`CROSS_CASES`] (any order, no
/// duplicates), naming what is missing or unknown.
pub fn assert_cross_registered(harness: &str, registered: &[&str]) {
    let missing: Vec<_> = CROSS_CASES
        .iter()
        .filter(|case| !registered.contains(case))
        .collect();
    let unknown: Vec<_> = registered
        .iter()
        .filter(|case| !CROSS_CASES.contains(case))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let duplicated: Vec<_> = registered.iter().filter(|c| !seen.insert(**c)).collect();
    assert!(
        missing.is_empty() && unknown.is_empty() && duplicated.is_empty(),
        "{harness}: cross-store cases not registered: {missing:?}; unknown: {unknown:?}; \
         registered twice: {duplicated:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shared case is in exactly one tier list (the execution-scoped
    /// ones also in the cross list), and nothing else is listed.
    #[test]
    fn every_shared_case_has_a_tier() {
        let all = all_cases();
        let blob = crate::execution_scoped::BLOB_CASES;
        let d1 = crate::d1_scoped::D1_CASES;
        let mut expected: Vec<&str> = CROSS_CASES.iter().chain(blob).chain(d1).copied().collect();
        expected.sort_unstable();
        let mut derived: Vec<&str> = all.iter().map(String::as_str).collect();
        derived.sort_unstable();
        assert_eq!(
            derived, expected,
            "list each new case in its tier (or in HELPERS)"
        );
        for case in crate::execution_scoped::EXECUTION_SCOPED {
            assert!(
                CROSS_CASES.contains(case),
                "{case}: the composed store runs it too"
            );
        }
        for helper in HELPERS {
            let (module, name) = helper.split_once("::").unwrap();
            let source = MODULES.iter().find(|(m, _)| *m == module).unwrap().1;
            assert!(
                source.contains(&format!("pub async fn {name}")),
                "{helper} is gone"
            );
        }
    }
}
