//! CF2: the ExecutionDO store's test suite, run on any [`host::Host`]:
//! natively over SQLite (`tests/native.rs`) and inside workerd over
//! `SqlStorage`, one RPC per case to the test Worker's `ConformanceDO`
//! (`cloudflare/tests/conformance.test.mjs`).
//!
//! Two lists, one registry:
//! - [`SHARED`]: the execution-scoped cases of `runbook-store-conformance`,
//!   which must equal its `EXECUTION_SCOPED` (checked natively below and
//!   in workerd, so a skipped case fails);
//! - [`BLOB`]: the blob-tier cases (`BLOB_CASES`), run against the R2 blob
//!   store over the host's buckets (in-memory natively, Miniflare's R2 in
//!   workerd) (CF4);
//! - [`DO_CASES`]: this store's own behavior (`cases.rs`).
//!
//! CF5 grows this into the full cross-store suite. Test-only: never part
//! of the api Worker.
pub mod cases;
pub mod cross;
pub mod cross_cases;
pub mod cross_registry;
pub mod d1_cases;
#[cfg(target_arch = "wasm32")]
mod edge;
pub mod host;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;

pub use runbook_store_conformance::execution_scoped::EXECUTION_SCOPED;

macro_rules! registry {
    (
        shared: [$($module:ident :: $name:ident),* $(,)?],
        blob: [$($bmodule:ident :: $bname:ident),* $(,)?],
        own: [$($case:ident),* $(,)?],
        d1_factory: [$($fmodule:ident :: $fname:ident),* $(,)?],
        d1_directory: [$($dmodule:ident :: $dname:ident),* $(,)?],
        d1_credentials: [$($cmodule:ident :: $cname:ident),* $(,)?],
        d1_webhooks: [$($wmodule:ident :: $wname:ident),* $(,)?],
        d1_memory: [$($mmodule:ident :: $mname:ident),* $(,)?],
        d1_own: [$($ocase:ident),* $(,)?] $(,)?
    ) => {
        /// The D1 store's own cases (CF3 review fixes), as `d1::function`.
        pub const D1_OWN: &[&str] = &[$(concat!("d1::", stringify!($ocase))),*];
        /// The shared D1-scoped cases (CF3), run against `runbook-store-d1`
        /// over the host's D1 (rusqlite natively, Miniflare's D1 in workerd).
        pub const D1: &[&str] = &[
            $(concat!(stringify!($fmodule), "::", stringify!($fname)),)*
            $(concat!(stringify!($dmodule), "::", stringify!($dname)),)*
            $(concat!(stringify!($cmodule), "::", stringify!($cname)),)*
            $(concat!(stringify!($wmodule), "::", stringify!($wname)),)*
            $(concat!(stringify!($mmodule), "::", stringify!($mname)),)*
        ];
        /// Run one registered D1 case on a fresh database of `host`;
        /// `None` if the name is unknown.
        pub async fn run_d1_case<H: host::Host>(
            name: &str,
            host: &H,
        ) -> Option<Result<(), String>> {
            use host::CaseOutcome;
            if !D1.contains(&name) && !D1_OWN.contains(&name) {
                return None;
            }
            let db = match host.d1().await {
                Ok(db) => db,
                Err(error) => return Some(Err(format!("{error:?}"))),
            };
            let factory = host::D1Factory(db.clone());
            Some(match name {
                $(concat!(stringify!($fmodule), "::", stringify!($fname)) =>
                    runbook_store_conformance::$fmodule::$fname(&factory).await.outcome(),)*
                $(concat!(stringify!($dmodule), "::", stringify!($dname)) =>
                    runbook_store_conformance::$dmodule::$dname(
                        &runbook_store_d1::D1Directory::new(db)).await.outcome(),)*
                $(concat!(stringify!($cmodule), "::", stringify!($cname)) =>
                    runbook_store_conformance::$cmodule::$cname(
                        &runbook_store_d1::D1Credentials::new(db)).await.outcome(),)*
                $(concat!(stringify!($wmodule), "::", stringify!($wname)) =>
                    runbook_store_conformance::$wmodule::$wname(
                        &runbook_worker::webhooks::CfWebhooks::new(db, host.webhooks()))
                        .await.outcome(),)*
                $(concat!(stringify!($mmodule), "::", stringify!($mname)) =>
                    runbook_store_conformance::$mmodule::$mname(
                        &runbook_store_d1::D1Memory::new(db)).await.outcome(),)*
                $(concat!("d1::", stringify!($ocase)) => d1_cases::$ocase(db).await.outcome(),)*
                _ => return None,
            })
        }
        /// The shared blob-tier cases, run against the R2 blob store (CF4).
        pub const BLOB: &[&str] = &[$(concat!(stringify!($bmodule), "::", stringify!($bname))),*];
        /// The shared cases this suite runs, as `module::function`.
        pub const SHARED: &[&str] = &[$(concat!(stringify!($module), "::", stringify!($name))),*];
        /// This store's own cases, as `do::function`.
        pub const DO_CASES: &[&str] = &[$(concat!("do::", stringify!($case))),*];
        /// Run one registered case on `host`; `None` if the name is unknown.
        pub async fn run_case<H: host::Host>(
            name: &str,
            host: &H,
        ) -> Option<Result<(), runbook_core::StoreError>> {
            let factory = host::HostFactory(host);
            let r2 = host::R2Factory(host);
            Some(match name {
                $(concat!(stringify!($bmodule), "::", stringify!($bname)) =>
                    runbook_store_conformance::$bmodule::$bname(&r2).await,)*
                $(concat!(stringify!($module), "::", stringify!($name)) =>
                    runbook_store_conformance::$module::$name(&factory).await,)*
                $(concat!("do::", stringify!($case)) => cases::$case(host).await,)*
                _ => return None,
            })
        }
    };
}
registry!(
    shared: [
        execution::atomic_batch_or_nothing,
        execution::stale_commit_has_no_side_effects,
        execution::one_claim_generation_winner,
        execution::idempotent_response_is_payload_bound,
        execution::replay_equals_projection,
        clock::store_clock_controls_deadline_equality,
        clock::session_time_and_snapshot_are_fenced,
        clock::session_commits_at_its_begin_time,
        queue::due_item_has_one_current_worker,
        queue::stale_worker_cannot_ack_successor,
        queue::delivery_leases_are_scoped_and_fenced,
        queue::timer_replacements_require_new_generation,
    ],
    blob: [
        blob::blob_roundtrip_preserves_chunk_boundaries,
        blob::shared_blobs_are_separate_and_immutable,
        uploads::upload_blobs_support_head_and_ranges,
        uploads::upload_ranges_cross_chunk_boundaries,
    ],
    own: [
        derived_and_notification_streams_drain_in_order_and_ack_through,
        a_notification_acked_through_the_queue_leaves_the_drain,
        the_alarm_is_the_earliest_pending_work_and_follows_every_write,
        a_failed_delivery_backs_its_stream_off_and_an_ack_resets_it,
        a_failed_commit_rolls_back_rows_and_the_alarm,
        credentials_are_hash_only_rotate_once_and_revoke_on_close,
        heartbeats_and_logs_are_fenced_and_lease_expiry_is_work,
        a_start_records_its_facts_and_activation_waits_for_admission,
        another_tenant_cannot_read_or_write_this_object,
        every_run_owned_row_carries_its_tenant_and_execution,
        migrations_apply_once_and_refuse_a_changed_history,
        reset_empties_every_table,
        a_session_commits_at_its_own_time_after_the_clock_moves,
        a_notification_page_never_acks_an_undelivered_intent,
        an_object_serves_only_its_own_execution,
        an_object_checks_its_schema_once_per_instance,
        a_write_that_keeps_the_revision_still_fences_a_session,
        sessions_in_one_millisecond_replay_instead_of_colliding,
        an_overdue_outbox_retry_never_arms_a_past_alarm,
        claiming_one_kind_leaves_other_work_unleased,
        a_time_past_the_date_range_is_refused,
        a_load_skips_delivered_outbox_rows,
        two_fresh_objects_never_share_rows,
        step_runs_are_rows_of_their_own_and_load_back_identical,
        a_head_written_before_cf13_loads_and_the_next_commit_splits_it,
        derived_assignments_are_rows_of_their_own,
        a_row_past_the_limit_is_refused_and_nothing_is_written,
        an_import_measures_every_row,
        a_row_at_the_threshold_is_written_and_one_byte_more_is_refused,
    ],
    d1_factory: [
        directory::directory_snapshot_is_org_scoped,
        directory::directory_revision_fences_mutations,
        directory::directory_rejects_cross_org_membership,
        directory::directory_slug_uniqueness_is_org_scoped,
        directory::directory_deactivation_and_removal_are_visible,
        directory::directory_fixture_has_distinct_principals,
        directory::verified_email_key_reads_are_unique_and_atomic,
        directory::invalid_batches_and_duplicate_ids_do_not_leak_indexes,
        directory::shared_identity_deactivation_is_org_local,
        directory::org_role_permission_matrix,
        directory::last_active_owner_is_protected,
        directory::org_roles_are_org_local,
        eligibility::team_membership_does_not_grant_project_access,
        eligibility::approval_requires_current_target_match,
        eligibility::unknown_static_reference_fails_publication,
        eligibility::handoff_missing_route_notifies_admins_without_broadening,
        eligibility::two_users_not_two_tokens_satisfy_quorum,
        eligibility::unresolved_handoff_without_admin_still_records_blocked_route,
        credentials::pat_and_service_authorization_is_current,
        credentials::credential_usage_write_is_throttled,
        credentials::secret_format_and_hash_are_pinned,
        notify::preferences_salt_and_token,
        notify::outbox_records_once_and_claims_are_fenced,
        notify::digest_rows_are_claimed_by_age_and_marked_sent,
    ],
    d1_directory: [
        auth_links::invitation_links_once_and_applies_role,
        auth_links::links_refuse_uninvited_and_stale,
    ],
    d1_credentials: [
        device_codes::poll_consumes_an_approval_once,
        device_codes::denial_expiry_and_collisions,
    ],
    d1_webhooks: [
        webhooks::limit_is_per_project_and_tenants_are_isolated,
        webhooks::fan_out_is_idempotent_and_claims_are_fenced,
        webhooks::replay_rows_and_delete_cascade,
        webhooks::stale_saves_conflict,
        webhooks::delivery_log_purge_is_bounded_and_keeps_verdicts,
    ],
    d1_memory: [
        memory_entries::stale_revision_is_refused,
        memory_entries::foreign_and_expired_entries_are_hidden,
        memory_entries::secrets_are_stored_redacted,
        memory_entries::entry_quota_is_per_runbook,
        memory_entries::patches_follow_transitions_and_keep_history,
        memory_entries::history_is_bounded_lockable_and_restorable,
        memory_entries::people_keep_their_revision_and_the_store_holds_locks,
        memory_entries::search_filters_and_pages,
    ],
    d1_own: [
        a_release_never_strands_a_waiter_queued_meanwhile,
        an_admission_rechecks_a_pool_released_meanwhile,
        one_replay_key_reserves_one_run,
        the_applier_is_ordered_idempotent_and_releases,
    ],
);

#[cfg(test)]
mod tests {
    #[test]
    fn every_execution_scoped_case_is_registered() {
        runbook_store_conformance::execution_scoped::assert_registered(
            "runbook-worker-conformance",
            super::SHARED,
        );
        runbook_store_conformance::execution_scoped::assert_blob_registered(
            "runbook-worker-conformance",
            super::BLOB,
        );
        runbook_store_conformance::d1_scoped::assert_d1_registered(
            "runbook-worker-conformance",
            super::D1,
        );
    }
}
