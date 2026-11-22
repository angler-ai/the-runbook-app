//! The suite natively, over in-memory SQLite (rusqlite, bundled): the
//! shared execution-scoped conformance cases and the DO store's own cases,
//! one test each, through the same registry workerd runs.
use runbook_core::{CommitResult, ExecutionStore};
use runbook_store_conformance::{execution, scope};
use runbook_store_do::{DoStore, Fault};
use runbook_worker_conformance::{
    BLOB, D1, D1_OWN, DO_CASES, SHARED,
    host::Host,
    native::{Native, run},
    run_case, run_d1_case,
};

macro_rules! native {
    ($($test:ident => $case:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                run(run_case($case, &Native)).expect("registered").unwrap();
            }
        )*
        const NATIVE: &[&str] = &[$($case),*];
    };
}
native!(
    blob_blob_roundtrip_preserves_chunk_boundaries => "blob::blob_roundtrip_preserves_chunk_boundaries",
    blob_shared_blobs_are_separate_and_immutable => "blob::shared_blobs_are_separate_and_immutable",
    uploads_upload_blobs_support_head_and_ranges => "uploads::upload_blobs_support_head_and_ranges",
    uploads_upload_ranges_cross_chunk_boundaries => "uploads::upload_ranges_cross_chunk_boundaries",
    execution_atomic_batch_or_nothing => "execution::atomic_batch_or_nothing",
    execution_stale_commit_has_no_side_effects => "execution::stale_commit_has_no_side_effects",
    execution_one_claim_generation_winner => "execution::one_claim_generation_winner",
    execution_idempotent_response_is_payload_bound => "execution::idempotent_response_is_payload_bound",
    execution_replay_equals_projection => "execution::replay_equals_projection",
    clock_store_clock_controls_deadline_equality => "clock::store_clock_controls_deadline_equality",
    clock_session_time_and_snapshot_are_fenced => "clock::session_time_and_snapshot_are_fenced",
    clock_session_commits_at_its_begin_time => "clock::session_commits_at_its_begin_time",
    queue_due_item_has_one_current_worker => "queue::due_item_has_one_current_worker",
    queue_stale_worker_cannot_ack_successor => "queue::stale_worker_cannot_ack_successor",
    queue_delivery_leases_are_scoped_and_fenced => "queue::delivery_leases_are_scoped_and_fenced",
    queue_timer_replacements_require_new_generation => "queue::timer_replacements_require_new_generation",
    do_derived_and_notification_streams_drain_in_order_and_ack_through => "do::derived_and_notification_streams_drain_in_order_and_ack_through",
    do_a_notification_acked_through_the_queue_leaves_the_drain => "do::a_notification_acked_through_the_queue_leaves_the_drain",
    do_the_alarm_is_the_earliest_pending_work_and_follows_every_write => "do::the_alarm_is_the_earliest_pending_work_and_follows_every_write",
    do_a_failed_delivery_backs_its_stream_off_and_an_ack_resets_it => "do::a_failed_delivery_backs_its_stream_off_and_an_ack_resets_it",
    do_a_failed_commit_rolls_back_rows_and_the_alarm => "do::a_failed_commit_rolls_back_rows_and_the_alarm",
    do_credentials_are_hash_only_rotate_once_and_revoke_on_close => "do::credentials_are_hash_only_rotate_once_and_revoke_on_close",
    do_heartbeats_and_logs_are_fenced_and_lease_expiry_is_work => "do::heartbeats_and_logs_are_fenced_and_lease_expiry_is_work",
    do_a_start_records_its_facts_and_activation_waits_for_admission => "do::a_start_records_its_facts_and_activation_waits_for_admission",
    do_another_tenant_cannot_read_or_write_this_object => "do::another_tenant_cannot_read_or_write_this_object",
    do_every_run_owned_row_carries_its_tenant_and_execution => "do::every_run_owned_row_carries_its_tenant_and_execution",
    do_migrations_apply_once_and_refuse_a_changed_history => "do::migrations_apply_once_and_refuse_a_changed_history",
    do_reset_empties_every_table => "do::reset_empties_every_table",
    do_a_session_commits_at_its_own_time_after_the_clock_moves => "do::a_session_commits_at_its_own_time_after_the_clock_moves",
    do_a_notification_page_never_acks_an_undelivered_intent => "do::a_notification_page_never_acks_an_undelivered_intent",
    do_an_object_serves_only_its_own_execution => "do::an_object_serves_only_its_own_execution",
    do_an_object_checks_its_schema_once_per_instance => "do::an_object_checks_its_schema_once_per_instance",
    do_a_write_that_keeps_the_revision_still_fences_a_session => "do::a_write_that_keeps_the_revision_still_fences_a_session",
    do_sessions_in_one_millisecond_replay_instead_of_colliding => "do::sessions_in_one_millisecond_replay_instead_of_colliding",
    do_an_overdue_outbox_retry_never_arms_a_past_alarm => "do::an_overdue_outbox_retry_never_arms_a_past_alarm",
    do_claiming_one_kind_leaves_other_work_unleased => "do::claiming_one_kind_leaves_other_work_unleased",
    do_a_time_past_the_date_range_is_refused => "do::a_time_past_the_date_range_is_refused",
    do_a_load_skips_delivered_outbox_rows => "do::a_load_skips_delivered_outbox_rows",
    do_two_fresh_objects_never_share_rows => "do::two_fresh_objects_never_share_rows",
    do_step_runs_are_rows_of_their_own_and_load_back_identical => "do::step_runs_are_rows_of_their_own_and_load_back_identical",
    do_a_head_written_before_cf13_loads_and_the_next_commit_splits_it => "do::a_head_written_before_cf13_loads_and_the_next_commit_splits_it",
    do_derived_assignments_are_rows_of_their_own => "do::derived_assignments_are_rows_of_their_own",
    do_a_row_past_the_limit_is_refused_and_nothing_is_written => "do::a_row_past_the_limit_is_refused_and_nothing_is_written",
    do_an_import_measures_every_row => "do::an_import_measures_every_row",
    do_a_row_at_the_threshold_is_written_and_one_byte_more_is_refused => "do::a_row_at_the_threshold_is_written_and_one_byte_more_is_refused",
);

#[test]
fn every_registered_case_runs_natively() {
    let mut all: Vec<&str> = SHARED.iter().chain(BLOB).chain(DO_CASES).copied().collect();
    let mut native: Vec<&str> = NATIVE.to_vec();
    all.sort_unstable();
    native.sort_unstable();
    assert_eq!(native, all);
    runbook_store_conformance::execution_scoped::assert_registered("native", SHARED);
    runbook_store_conformance::execution_scoped::assert_blob_registered("native", BLOB);
    assert!(run(run_case("do::not_a_case", &Native)).is_none());
}

/// A Rust panic mid-commit rolls back every row and the alarm. Native only:
/// a wasm panic aborts the request (the workerd gate is CF1's atomicity test).
#[test]
fn a_panic_mid_commit_rolls_back() {
    let sql = Native.fresh().unwrap();
    let s = DoStore::open(sql.clone(), &execution::projection().execution_id, scope(1)).unwrap();
    s.create_execution(execution::projection()).unwrap();
    let before = s.table_counts().unwrap();
    s.inject(Fault::Panic);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(s.commit(execution::batch()))
    }));
    assert!(caught.is_err());
    assert_eq!(s.table_counts().unwrap(), before);
    assert_eq!(sql.alarm(), None);
    let mut b = execution::batch();
    b.claims.clear();
    assert_eq!(run(s.commit(b)).unwrap(), CommitResult::Committed(1));
}

/// `DoSql` that records every statement (for the read-shape test below).
#[derive(Clone)]
struct Recording {
    inner: runbook_worker_conformance::native::NativeSql,
    log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}
impl runbook_store_do::DoSql for Recording {
    fn exec(
        &self,
        sql: &str,
        params: &[runbook_store_do::Value],
    ) -> Result<(), runbook_core::StoreError> {
        self.log.borrow_mut().push(sql.to_owned());
        self.inner.exec(sql, params)
    }
    fn query(
        &self,
        sql: &str,
        params: &[runbook_store_do::Value],
    ) -> Result<Vec<runbook_store_do::Row>, runbook_core::StoreError> {
        self.log.borrow_mut().push(sql.to_owned());
        self.inner.query(sql, params)
    }
    fn exec_script(&self, sql: &str) -> Result<(), runbook_core::StoreError> {
        self.inner.exec_script(sql)
    }
    fn transaction<T: 'static>(
        &self,
        body: impl FnOnce() -> Result<T, runbook_core::StoreError> + 'static,
    ) -> Result<T, runbook_core::StoreError> {
        self.inner.transaction(body)
    }
    fn now_ms(&self) -> i64 {
        self.inner.now_ms()
    }
    fn set_alarm(&self, at_ms: Option<i64>) -> Result<(), runbook_core::StoreError> {
        self.inner.set_alarm(at_ms)
    }
}

/// Review fix 6: a commit reads the head and the rows its checks name,
/// never the whole run (the event log, every outbox row, every timer).
#[test]
fn a_commit_reads_only_what_it_checks() {
    use runbook_core::{ExecutionStore, TimerChange, TimerRecord};
    let recording = Recording {
        inner: Native.fresh().unwrap(),
        log: Default::default(),
    };
    let id = execution::projection().execution_id;
    let s = DoStore::open(recording.clone(), &id, scope(1)).unwrap();
    s.create_execution(execution::projection()).unwrap();
    run(s.commit(execution::batch())).unwrap();
    let mut b = execution::batch();
    b.expected_sequence = 1;
    b.projection.sequence = 2;
    b.events[0].seq = 2;
    b.outbox[0].sequence = 2;
    b.outbox[0].event_id = "evt_2".into();
    b.idempotency[0].key = "request-2".into();
    b.claims[0].expected_generation = 1;
    b.claims[0].generation = 2;
    b.timers = vec![TimerChange::Upsert(TimerRecord {
        execution_id: id.clone(),
        key: "deadline".into(),
        generation: 2,
        due_at_ms: 200,
    })];
    recording.log.borrow_mut().clear();
    run(s.commit(b)).unwrap();
    let reads: Vec<String> = recording
        .log
        .borrow()
        .iter()
        .filter(|sql| sql.trim_start().starts_with("SELECT"))
        .cloned()
        .collect();
    for scan in [
        "SELECT envelope FROM event ORDER BY seq",
        "SELECT event_id, intent FROM outbox",
        "SELECT sequence, entry FROM derived_outbox",
        "SELECT key, body FROM idempotency",
        "SELECT body FROM claim",
        "SELECT key, generation, due_at_ms FROM timer_history",
    ] {
        assert!(
            !reads.iter().any(|sql| sql.trim() == scan),
            "the commit scanned the run: {scan}\n{reads:#?}"
        );
    }
}

/// The D1-scoped cases (CF3), one test each, over native SQLite.
macro_rules! native_d1 {
    ($($test:ident => $case:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                run(run_d1_case($case, &Native)).expect("registered").unwrap();
            }
        )*
        const NATIVE_D1: &[&str] = &[$($case),*];
    };
}
native_d1!(
    d1_directory_directory_snapshot_is_org_scoped => "directory::directory_snapshot_is_org_scoped",
    d1_directory_directory_revision_fences_mutations => "directory::directory_revision_fences_mutations",
    d1_directory_directory_rejects_cross_org_membership => "directory::directory_rejects_cross_org_membership",
    d1_directory_directory_slug_uniqueness_is_org_scoped => "directory::directory_slug_uniqueness_is_org_scoped",
    d1_directory_directory_deactivation_and_removal_are_visible => "directory::directory_deactivation_and_removal_are_visible",
    d1_directory_directory_fixture_has_distinct_principals => "directory::directory_fixture_has_distinct_principals",
    d1_directory_verified_email_key_reads_are_unique_and_atomic => "directory::verified_email_key_reads_are_unique_and_atomic",
    d1_directory_invalid_batches_and_duplicate_ids_do_not_leak_indexes => "directory::invalid_batches_and_duplicate_ids_do_not_leak_indexes",
    d1_directory_shared_identity_deactivation_is_org_local => "directory::shared_identity_deactivation_is_org_local",
    d1_directory_org_role_permission_matrix => "directory::org_role_permission_matrix",
    d1_directory_last_active_owner_is_protected => "directory::last_active_owner_is_protected",
    d1_directory_org_roles_are_org_local => "directory::org_roles_are_org_local",
    d1_eligibility_team_membership_does_not_grant_project_access => "eligibility::team_membership_does_not_grant_project_access",
    d1_eligibility_approval_requires_current_target_match => "eligibility::approval_requires_current_target_match",
    d1_eligibility_unknown_static_reference_fails_publication => "eligibility::unknown_static_reference_fails_publication",
    d1_eligibility_handoff_missing_route_notifies_admins_without_broadening => "eligibility::handoff_missing_route_notifies_admins_without_broadening",
    d1_eligibility_two_users_not_two_tokens_satisfy_quorum => "eligibility::two_users_not_two_tokens_satisfy_quorum",
    d1_eligibility_unresolved_handoff_without_admin_still_records_blocked_route => "eligibility::unresolved_handoff_without_admin_still_records_blocked_route",
    d1_credentials_pat_and_service_authorization_is_current => "credentials::pat_and_service_authorization_is_current",
    d1_credentials_credential_usage_write_is_throttled => "credentials::credential_usage_write_is_throttled",
    d1_credentials_secret_format_and_hash_are_pinned => "credentials::secret_format_and_hash_are_pinned",
    d1_notify_preferences_salt_and_token => "notify::preferences_salt_and_token",
    d1_notify_outbox_records_once_and_claims_are_fenced => "notify::outbox_records_once_and_claims_are_fenced",
    d1_notify_digest_rows_are_claimed_by_age_and_marked_sent => "notify::digest_rows_are_claimed_by_age_and_marked_sent",
    d1_auth_links_invitation_links_once_and_applies_role => "auth_links::invitation_links_once_and_applies_role",
    d1_auth_links_links_refuse_uninvited_and_stale => "auth_links::links_refuse_uninvited_and_stale",
    d1_device_codes_poll_consumes_an_approval_once => "device_codes::poll_consumes_an_approval_once",
    d1_device_codes_denial_expiry_and_collisions => "device_codes::denial_expiry_and_collisions",
    d1_webhooks_limit_is_per_project_and_tenants_are_isolated => "webhooks::limit_is_per_project_and_tenants_are_isolated",
    d1_webhooks_fan_out_is_idempotent_and_claims_are_fenced => "webhooks::fan_out_is_idempotent_and_claims_are_fenced",
    d1_webhooks_replay_rows_and_delete_cascade => "webhooks::replay_rows_and_delete_cascade",
    d1_webhooks_stale_saves_conflict => "webhooks::stale_saves_conflict",
    d1_webhooks_delivery_log_purge_is_bounded_and_keeps_verdicts => "webhooks::delivery_log_purge_is_bounded_and_keeps_verdicts",
    d1_memory_entries_stale_revision_is_refused => "memory_entries::stale_revision_is_refused",
    d1_memory_entries_foreign_and_expired_entries_are_hidden => "memory_entries::foreign_and_expired_entries_are_hidden",
    d1_memory_entries_secrets_are_stored_redacted => "memory_entries::secrets_are_stored_redacted",
    d1_memory_entries_entry_quota_is_per_runbook => "memory_entries::entry_quota_is_per_runbook",
    d1_memory_entries_patches_follow_transitions_and_keep_history => "memory_entries::patches_follow_transitions_and_keep_history",
    d1_memory_entries_history_is_bounded_lockable_and_restorable => "memory_entries::history_is_bounded_lockable_and_restorable",
    d1_memory_entries_people_keep_their_revision_and_the_store_holds_locks => "memory_entries::people_keep_their_revision_and_the_store_holds_locks",
    d1_memory_entries_search_filters_and_pages => "memory_entries::search_filters_and_pages",
);

#[test]
fn every_d1_case_runs_natively() {
    runbook_store_conformance::d1_scoped::assert_d1_registered("native D1 tests", NATIVE_D1);
    assert_eq!(NATIVE_D1, D1);
}

/// The D1 store's own cases (CF3 review fixes), over native SQLite.
macro_rules! native_d1_own {
    ($($test:ident => $case:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                run(run_d1_case($case, &Native)).expect("registered").unwrap();
            }
        )*
        const NATIVE_D1_OWN: &[&str] = &[$($case),*];
    };
}
native_d1_own!(
    d1_runner_registration_and_rejoin => "d1::runner_registration_and_rejoin",
    d1_a_release_never_strands_a_waiter_queued_meanwhile => "d1::a_release_never_strands_a_waiter_queued_meanwhile",
    d1_an_admission_rechecks_a_pool_released_meanwhile => "d1::an_admission_rechecks_a_pool_released_meanwhile",
    d1_one_replay_key_reserves_one_run => "d1::one_replay_key_reserves_one_run",
    d1_the_applier_is_ordered_idempotent_and_releases => "d1::the_applier_is_ordered_idempotent_and_releases",
);

#[test]
fn every_d1_own_case_runs_natively() {
    assert_eq!(NATIVE_D1_OWN, D1_OWN);
}
