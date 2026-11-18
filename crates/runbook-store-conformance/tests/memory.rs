use runbook_store_conformance::MemoryFactory;
fn run(f: impl std::future::Future<Output = Result<(), runbook_core::StoreError>>) {
    use std::{
        pin::pin,
        task::{Context, Poll, Waker},
    };
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v.unwrap(),
        Poll::Pending => panic!("memory yielded"),
    }
}
#[test]
fn atomic_batch_or_nothing() {
    run(runbook_store_conformance::execution::atomic_batch_or_nothing(&MemoryFactory))
}
#[test]
fn stale_commit_has_no_side_effects() {
    run(runbook_store_conformance::execution::stale_commit_has_no_side_effects(&MemoryFactory))
}
#[test]
fn one_claim_generation_winner() {
    run(runbook_store_conformance::execution::one_claim_generation_winner(&MemoryFactory))
}
#[test]
fn idempotent_response_is_payload_bound() {
    run(runbook_store_conformance::execution::idempotent_response_is_payload_bound(&MemoryFactory))
}
#[test]
fn replay_equals_projection() {
    run(runbook_store_conformance::execution::replay_equals_projection(&MemoryFactory))
}
#[test]
fn store_clock_controls_deadline_equality() {
    run(runbook_store_conformance::clock::store_clock_controls_deadline_equality(&MemoryFactory))
}

#[test]
fn project_queries_never_return_foreign_rows() {
    run(
        runbook_store_conformance::catalog::project_queries_never_return_foreign_rows(
            &MemoryFactory,
        ),
    )
}
#[test]
fn revision_precondition_rejects_stale_replace() {
    run(
        runbook_store_conformance::catalog::revision_precondition_rejects_stale_replace(
            &MemoryFactory,
        ),
    )
}
#[test]
fn derived_rows_ignore_old_or_duplicate_sequence() {
    run(
        runbook_store_conformance::catalog::derived_rows_ignore_old_or_duplicate_sequence(
            &MemoryFactory,
        ),
    )
}
#[test]
fn concurrency_limit_one_admits_and_queues() {
    run(runbook_store_conformance::catalog::concurrency_limit_one_admits_and_queues(&MemoryFactory))
}
#[test]
fn derived_rows_rollback_with_execution() {
    run(runbook_store_conformance::catalog::derived_rows_rollback_with_execution(&MemoryFactory))
}
#[test]
fn session_commits_at_its_begin_time() {
    run(runbook_store_conformance::clock::session_commits_at_its_begin_time(&MemoryFactory))
}
#[test]
fn session_time_and_snapshot_are_fenced() {
    run(runbook_store_conformance::clock::session_time_and_snapshot_are_fenced(&MemoryFactory))
}
#[test]
fn derived_rows_cannot_release_running_execution() {
    run(
        runbook_store_conformance::catalog::derived_rows_cannot_release_running_execution(
            &MemoryFactory,
        ),
    )
}

#[test]
fn due_item_has_one_current_worker() {
    run(runbook_store_conformance::queue::due_item_has_one_current_worker(&MemoryFactory))
}

#[test]
fn stale_worker_cannot_ack_successor() {
    run(runbook_store_conformance::queue::stale_worker_cannot_ack_successor(&MemoryFactory))
}

#[test]
fn blob_roundtrip_preserves_chunk_boundaries() {
    run(runbook_store_conformance::blob::blob_roundtrip_preserves_chunk_boundaries(&MemoryFactory))
}
#[test]
fn delivery_leases_are_scoped_and_fenced() {
    run(runbook_store_conformance::queue::delivery_leases_are_scoped_and_fenced(&MemoryFactory))
}
#[test]
fn shared_blobs_are_separate_and_immutable() {
    run(runbook_store_conformance::blob::shared_blobs_are_separate_and_immutable(&MemoryFactory))
}
#[test]
fn timer_replacements_require_new_generation() {
    run(runbook_store_conformance::queue::timer_replacements_require_new_generation(&MemoryFactory))
}

#[test]
fn export_contains_one_complete_run() {
    run(runbook_store_conformance::retention::export_contains_one_complete_run(&MemoryFactory))
}

#[test]
fn delete_leaves_only_tombstone() {
    run(runbook_store_conformance::retention::delete_leaves_only_tombstone(&MemoryFactory))
}

#[test]
fn finished_day_uses_utc_half_open_interval() {
    run(
        runbook_store_conformance::retention::finished_day_uses_utc_half_open_interval(
            &MemoryFactory,
        ),
    )
}

#[test]
fn deletion_is_atomic_and_rejects_open_runs() {
    run(
        runbook_store_conformance::retention::deletion_is_atomic_and_rejects_open_runs(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_stale_marks_follow_commits_and_deletions() {
    run(
        runbook_store_conformance::archives::stale_marks_follow_commits_and_deletions(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_retention_deletion_rechecks_its_file() {
    run(runbook_store_conformance::archives::retention_deletion_rechecks_its_file(&MemoryFactory))
}

#[test]
fn archive_evidence_follows_the_archive_and_retained_runs_can_be_purged() {
    run(runbook_store_conformance::archives::evidence_follows_the_archive_and_retained_runs_can_be_purged(&MemoryFactory))
}

#[test]
fn archive_finalized_evidence_marks_its_file_stale() {
    run(
        runbook_store_conformance::archives::finalized_evidence_marks_its_file_stale(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_versions_are_swept_only_behind_the_index() {
    run(
        runbook_store_conformance::archives::versions_are_swept_only_behind_the_index(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_privacy_deletions_find_their_file_after_the_summary_purge() {
    run(runbook_store_conformance::archives::privacy_deletions_find_their_file_after_the_summary_purge(&MemoryFactory))
}

#[test]
fn archive_a_record_never_clears_a_later_mark_or_misses_evidence() {
    run(
        runbook_store_conformance::archives::a_record_never_clears_a_later_mark_or_misses_evidence(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_retained_evidence_counts_until_its_file_expires() {
    run(
        runbook_store_conformance::archives::retained_evidence_counts_until_its_file_expires(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_expiry_deletes_every_upload_under_its_retained_runs() {
    run(
        runbook_store_conformance::archives::expiry_deletes_every_upload_under_its_retained_runs(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_a_live_built_folder_is_refused_for_a_retained_run() {
    run(
        runbook_store_conformance::archives::a_live_built_folder_is_refused_for_a_retained_run(
            &MemoryFactory,
        ),
    )
}

#[test]
fn archive_failing_days_back_off() {
    run(runbook_store_conformance::archives::failing_days_back_off(
        &MemoryFactory,
    ))
}

#[test]
fn archive_index_swings_only_from_the_expected_file() {
    run(
        runbook_store_conformance::archives::the_index_swings_only_from_the_expected_file(
            &runbook_core::store::memory::MemoryExecutionStore::new(
                runbook_store_conformance::scope(1),
                runbook_store_conformance::execution::projection(),
                10_000,
            ),
        ),
    )
}

#[test]
fn archive_retention_queries_respect_archives_and_tenants() {
    run(
        runbook_store_conformance::archives::retention_queries_respect_archives_and_tenants(
            &MemoryFactory,
        ),
    )
}

fn run_dir(
    f: impl std::future::Future<Output = Result<(), runbook_core::directory::DirectoryError>>,
) {
    use std::{
        pin::pin,
        task::{Context, Poll, Waker},
    };
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v.unwrap(),
        Poll::Pending => panic!("memory yielded"),
    }
}
#[test]
fn directory_snapshot_is_org_scoped() {
    run_dir(runbook_store_conformance::directory::directory_snapshot_is_org_scoped(&MemoryFactory))
}
#[test]
fn directory_revision_fences_mutations() {
    run_dir(
        runbook_store_conformance::directory::directory_revision_fences_mutations(&MemoryFactory),
    )
}
#[test]
fn directory_rejects_cross_org_membership() {
    run_dir(
        runbook_store_conformance::directory::directory_rejects_cross_org_membership(
            &MemoryFactory,
        ),
    )
}
#[test]
fn directory_slug_uniqueness_is_org_scoped() {
    run_dir(
        runbook_store_conformance::directory::directory_slug_uniqueness_is_org_scoped(
            &MemoryFactory,
        ),
    )
}
#[test]
fn directory_deactivation_and_removal_are_visible() {
    run_dir(
        runbook_store_conformance::directory::directory_deactivation_and_removal_are_visible(
            &MemoryFactory,
        ),
    )
}
#[test]
fn directory_fixture_has_distinct_principals() {
    run_dir(
        runbook_store_conformance::directory::directory_fixture_has_distinct_principals(
            &MemoryFactory,
        ),
    )
}
#[test]
fn verified_email_key_reads_are_unique_and_atomic() {
    run_dir(
        runbook_store_conformance::directory::verified_email_key_reads_are_unique_and_atomic(
            &MemoryFactory,
        ),
    )
}
#[test]
fn invalid_batches_and_duplicate_ids_do_not_leak_indexes() {
    run_dir(
        runbook_store_conformance::directory::invalid_batches_and_duplicate_ids_do_not_leak_indexes(
            &MemoryFactory,
        ),
    )
}
#[test]
fn shared_identity_deactivation_is_org_local() {
    run_dir(
        runbook_store_conformance::directory::shared_identity_deactivation_is_org_local(
            &MemoryFactory,
        ),
    )
}
#[test]
fn team_membership_does_not_grant_project_access() {
    run_dir(
        runbook_store_conformance::eligibility::team_membership_does_not_grant_project_access(
            &MemoryFactory,
        ),
    )
}
#[test]
fn approval_requires_current_target_match() {
    run_dir(
        runbook_store_conformance::eligibility::approval_requires_current_target_match(
            &MemoryFactory,
        ),
    )
}
#[test]
fn unknown_static_reference_fails_publication() {
    run_dir(
        runbook_store_conformance::eligibility::unknown_static_reference_fails_publication(
            &MemoryFactory,
        ),
    )
}
#[test]
fn handoff_missing_route_notifies_admins_without_broadening() {
    run_dir(
        runbook_store_conformance::eligibility::handoff_missing_route_notifies_admins_without_broadening(
            &MemoryFactory,
        ),
    )
}
#[test]
fn two_users_not_two_tokens_satisfy_quorum() {
    run_dir(
        runbook_store_conformance::eligibility::two_users_not_two_tokens_satisfy_quorum(
            &MemoryFactory,
        ),
    )
}
#[test]
fn unresolved_handoff_without_admin_still_records_blocked_route() {
    run_dir(
        runbook_store_conformance::eligibility::unresolved_handoff_without_admin_still_records_blocked_route(
            &MemoryFactory,
        ),
    )
}

#[test]
fn org_role_permission_matrix() {
    run_dir(runbook_store_conformance::directory::org_role_permission_matrix(&MemoryFactory));
}
#[test]
fn last_active_owner_is_protected() {
    run_dir(runbook_store_conformance::directory::last_active_owner_is_protected(&MemoryFactory));
}
#[test]
fn org_roles_are_org_local() {
    run_dir(runbook_store_conformance::directory::org_roles_are_org_local(&MemoryFactory));
}

#[test]
fn pat_and_service_authorization_is_current() {
    run_dir(async {
        runbook_store_conformance::credentials::pat_and_service_authorization_is_current(
            &MemoryFactory,
        )
        .await;
        Ok(())
    });
}

#[test]
fn credential_usage_write_is_throttled() {
    run_dir(async {
        runbook_store_conformance::credentials::credential_usage_write_is_throttled(&MemoryFactory)
            .await;
        Ok(())
    });
}

#[test]
fn secret_format_and_hash_are_pinned() {
    run_dir(async {
        runbook_store_conformance::credentials::secret_format_and_hash_are_pinned(&MemoryFactory)
            .await;
        Ok(())
    });
}

#[test]
fn indexed_severity_tracks_commits() {
    run(runbook_store_conformance::search::indexed_severity_tracks_commits(&MemoryFactory))
}
#[test]
fn search_is_tenant_scoped_and_paginated() {
    run(runbook_store_conformance::search::search_is_tenant_scoped_and_paginated(&MemoryFactory))
}

#[test]
fn pool_limit_replacement_promotes_waiters() {
    run(runbook_store_conformance::catalog::pool_limit_replacement_promotes_waiters(&MemoryFactory))
}

#[test]
fn numeric_range_ignores_non_number_values() {
    run(runbook_store_conformance::search::numeric_range_ignores_non_number_values(&MemoryFactory))
}

#[test]
fn runbook_run_stats_group_by_runbook() {
    run(runbook_store_conformance::search::runbook_run_stats_group_by_runbook(&MemoryFactory))
}

#[test]
fn inbox_tracks_current_access_and_completion() {
    run(
        runbook_store_conformance::search::inbox_tracks_current_access_and_completion(
            &MemoryFactory,
        ),
    )
}

#[test]
fn webhook_limit_is_per_project_and_tenants_are_isolated() {
    run(
        runbook_store_conformance::webhooks::limit_is_per_project_and_tenants_are_isolated(
            &runbook_core::webhooks::memory::MemoryWebhooks::new(10_000),
        ),
    )
}
#[test]
fn webhook_fan_out_is_idempotent_and_claims_are_fenced() {
    run(
        runbook_store_conformance::webhooks::fan_out_is_idempotent_and_claims_are_fenced(
            &runbook_core::webhooks::memory::MemoryWebhooks::new(10_000),
        ),
    )
}
#[test]
fn webhook_replay_rows_and_delete_cascade() {
    run(
        runbook_store_conformance::webhooks::replay_rows_and_delete_cascade(
            &runbook_core::webhooks::memory::MemoryWebhooks::new(10_000),
        ),
    )
}
#[test]
fn webhook_stale_saves_conflict() {
    run(runbook_store_conformance::webhooks::stale_saves_conflict(
        &runbook_core::webhooks::memory::MemoryWebhooks::new(10_000),
    ))
}
#[test]
fn webhook_delivery_log_purge_is_bounded_and_keeps_verdicts() {
    run(
        runbook_store_conformance::webhooks::delivery_log_purge_is_bounded_and_keeps_verdicts(
            &runbook_core::webhooks::memory::MemoryWebhooks::new(200 * 86_400_000),
        ),
    )
}

// T0 contract freeze (plan 2026-11-12 §1): cases registered red. `settle`
// expects the frozen stub's error while a case is in `pending::PENDING`.
mod t0_pending {
    use super::MemoryFactory;
    use runbook_core::store::memory::MemoryExecutionStore;
    use runbook_store_conformance::{execution::projection, pending::*, scope};
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    fn settle_now<E: StubError>(case: &str, f: impl Future<Output = Result<(), E>>) {
        match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(result) => settle(case, result),
            Poll::Pending => panic!("memory yielded"),
        }
    }
    fn store() -> MemoryExecutionStore {
        MemoryExecutionStore::new(scope(1), projection(), 10_000)
    }
    #[test]
    fn pending_list_is_well_formed() {
        for (i, p) in PENDING.iter().enumerate() {
            assert!(REGISTERED.contains(&p.case), "{} is not registered", p.case);
            assert!(PACKAGES.contains(&p.package), "{}: {}", p.case, p.package);
            assert!(!PENDING[..i].iter().any(|q| q.case == p.case), "{}", p.case);
        }
    }
    #[test]
    fn memory_stale_revision_is_refused() {
        settle_now(
            "memory_entries::stale_revision_is_refused",
            runbook_store_conformance::memory_entries::stale_revision_is_refused(&store()),
        )
    }
    #[test]
    fn memory_foreign_and_expired_entries_are_hidden() {
        settle_now(
            "memory_entries::foreign_and_expired_entries_are_hidden",
            runbook_store_conformance::memory_entries::foreign_and_expired_entries_are_hidden(
                &store(),
            ),
        )
    }
    #[test]
    fn upload_slots_count_limits_and_finalize_once() {
        settle_now(
            "uploads::slots_count_limits_and_finalize_once",
            runbook_store_conformance::uploads::slots_count_limits_and_finalize_once(
                &MemoryFactory,
            ),
        )
    }
    #[test]
    fn upload_blobs_support_head_and_ranges() {
        settle_now(
            "uploads::upload_blobs_support_head_and_ranges",
            runbook_store_conformance::uploads::upload_blobs_support_head_and_ranges(
                &MemoryFactory,
            ),
        )
    }
    #[test]
    fn archive_index_round_trip_and_stale_marks() {
        settle_now(
            "archives::index_round_trip_and_stale_marks",
            runbook_store_conformance::archives::index_round_trip_and_stale_marks(&store()),
        )
    }
    #[test]
    fn archive_blobs_are_tenant_scoped() {
        settle_now(
            "archives::archive_blobs_are_tenant_scoped",
            runbook_store_conformance::archives::archive_blobs_are_tenant_scoped(&MemoryFactory),
        )
    }
    #[test]
    fn auth_invitation_links_once_and_applies_role() {
        settle_now(
            "auth_links::invitation_links_once_and_applies_role",
            runbook_store_conformance::auth_links::invitation_links_once_and_applies_role(
                &runbook_core::directory::memory::MemoryDirectory::default(),
            ),
        )
    }
    #[test]
    fn device_poll_consumes_an_approval_once() {
        settle_now(
            "device_codes::poll_consumes_an_approval_once",
            runbook_store_conformance::device_codes::poll_consumes_an_approval_once(
                &runbook_core::credentials::memory::MemoryCredentials::default(),
            ),
        )
    }
    #[test]
    fn auth_links_refuse_uninvited_and_stale() {
        settle_now(
            "auth_links::links_refuse_uninvited_and_stale",
            runbook_store_conformance::auth_links::links_refuse_uninvited_and_stale(
                &runbook_core::directory::memory::MemoryDirectory::default(),
            ),
        )
    }
    #[test]
    fn device_denial_expiry_and_collisions() {
        settle_now(
            "device_codes::denial_expiry_and_collisions",
            runbook_store_conformance::device_codes::denial_expiry_and_collisions(
                &runbook_core::credentials::memory::MemoryCredentials::default(),
            ),
        )
    }
}

// MEM1 (plan 2026-11-12 §2.1): memory cases on the memory adapter.
mod memory_entries {
    use runbook_core::store::memory::MemoryExecutionStore;
    use runbook_store_conformance::{MemoryFactory, execution::projection, memory_entries, scope};
    fn store() -> MemoryExecutionStore {
        MemoryExecutionStore::new(scope(1), projection(), 10_000)
    }
    fn ready<T>(f: impl std::future::Future<Output = T>) -> T {
        use std::{
            pin::pin,
            task::{Context, Poll, Waker},
        };
        match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("memory yielded"),
        }
    }
    #[test]
    fn secrets_are_stored_redacted() {
        ready(memory_entries::secrets_are_stored_redacted(&store())).unwrap()
    }
    #[test]
    fn entry_quota_is_per_runbook() {
        ready(memory_entries::entry_quota_is_per_runbook(&store())).unwrap()
    }
    #[test]
    fn patches_follow_transitions_and_keep_history() {
        ready(memory_entries::patches_follow_transitions_and_keep_history(
            &store(),
        ))
        .unwrap()
    }
    #[test]
    fn history_is_bounded_lockable_and_restorable() {
        ready(memory_entries::history_is_bounded_lockable_and_restorable(
            &store(),
        ))
        .unwrap()
    }
    #[test]
    fn people_keep_their_revision_and_the_store_holds_locks() {
        ready(memory_entries::people_keep_their_revision_and_the_store_holds_locks(&store()))
            .unwrap()
    }
    #[test]
    fn agent_writes_commit_a_notice() {
        ready(memory_entries::agent_writes_commit_a_notice(&MemoryFactory)).unwrap()
    }
    #[test]
    fn runs_edit_under_a_claim_and_agents_learn_nothing_hidden() {
        let directory = runbook_core::directory::memory::MemoryDirectory::default();
        directory.set_time_ms(1_700_000_000_000);
        let store = MemoryExecutionStore::empty(
            runbook_store_conformance::portable::p(),
            1_700_000_000_000,
        );
        ready(
            memory_entries::runs_edit_under_a_claim_and_agents_learn_nothing_hidden(
                &store, &directory,
            ),
        )
    }
    #[test]
    fn search_filters_and_pages() {
        ready(memory_entries::search_filters_and_pages(&store())).unwrap()
    }
    #[test]
    fn proposals_commit_with_the_execution() {
        ready(memory_entries::proposals_commit_with_the_execution(
            &MemoryFactory,
        ))
        .unwrap()
    }
    #[test]
    fn proposals_racing_a_full_quota_are_never_lost() {
        ready(memory_entries::proposals_racing_a_full_quota_are_never_lost(&MemoryFactory)).unwrap()
    }
    #[test]
    fn runs_save_live_entries() {
        ready(memory_entries::runs_save_live_entries(&MemoryFactory)).unwrap()
    }
    #[test]
    fn runs_save_live_memory_and_agents_only_edit() {
        let directory = runbook_core::directory::memory::MemoryDirectory::default();
        directory.set_time_ms(1_700_000_000_000);
        let store = MemoryExecutionStore::empty(
            runbook_store_conformance::portable::p(),
            1_700_000_000_000,
        );
        ready(memory_entries::runs_save_live_memory_and_agents_only_edit(
            &store, &directory,
        ))
    }
    #[test]
    fn proposals_are_capped_and_reads_audited() {
        let directory = runbook_core::directory::memory::MemoryDirectory::default();
        directory.set_time_ms(1_700_000_000_000);
        let store = MemoryExecutionStore::empty(
            runbook_store_conformance::portable::p(),
            1_700_000_000_000,
        );
        ready(memory_entries::proposals_are_capped_and_reads_audited(
            &store, &directory,
        ))
    }
    #[test]
    fn memory_writes_are_held_to_the_run_ceiling() {
        let directory = runbook_core::directory::memory::MemoryDirectory::default();
        directory.set_time_ms(1_700_000_000_000);
        let store = MemoryExecutionStore::empty(
            runbook_store_conformance::portable::p(),
            1_700_000_000_000,
        );
        ready(
            runbook_store_conformance::run_ceiling::memory_writes_are_held_to_the_run_ceiling(
                &store,
                &directory,
                |budget| store.with_run_budget(budget),
            ),
        )
    }
}

// UP1 (plan 2026-11-12 §2.3): upload cases on the memory adapter. The two
// T0 cases run in `t0_pending` above.
mod uploads {
    use super::run;
    use runbook_store_conformance::{MemoryFactory, uploads};
    #[test]
    fn upload_ranges_cross_chunk_boundaries() {
        run(uploads::upload_ranges_cross_chunk_boundaries(
            &MemoryFactory,
        ))
    }
    #[test]
    fn stale_slots_are_listed_and_deleted() {
        run(uploads::stale_slots_are_listed_and_deleted(&MemoryFactory))
    }
    #[test]
    fn quotas_count_live_slots() {
        run(uploads::quotas_count_live_slots(&MemoryFactory))
    }
    #[test]
    fn storage_counts_stored_bytes() {
        run(uploads::storage_counts_stored_bytes(&MemoryFactory))
    }
    #[test]
    fn run_deletion_removes_its_uploads() {
        run(uploads::run_deletion_removes_its_uploads(&MemoryFactory))
    }
    #[test]
    fn reports_attach_finalized_uploads() {
        let directory = runbook_core::directory::memory::MemoryDirectory::default();
        directory.set_time_ms(1_700_000_000_000);
        let store = runbook_core::store::memory::MemoryExecutionStore::empty(
            runbook_store_conformance::portable::p(),
            1_700_000_000_000,
        );
        run(async {
            uploads::reports_attach_finalized_uploads(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn appended_logs_satisfy_log_evidence() {
        let directory = runbook_core::directory::memory::MemoryDirectory::default();
        directory.set_time_ms(1_700_000_000_000);
        let store = runbook_core::store::memory::MemoryExecutionStore::empty(
            runbook_store_conformance::portable::p(),
            1_700_000_000_000,
        );
        run(async {
            uploads::appended_logs_satisfy_log_evidence(&store, &directory).await;
            Ok(())
        })
    }
}

// Language completeness (plan §2.9): one store test per key that moved to
// supported (later-plans rule 1).
mod language {
    use super::run;
    use runbook_store_conformance::language;
    fn stores() -> (
        runbook_core::store::memory::MemoryExecutionStore,
        runbook_core::directory::memory::MemoryDirectory,
    ) {
        let directory = runbook_core::directory::memory::MemoryDirectory::default();
        directory.set_time_ms(1_700_000_000_000);
        let store = runbook_core::store::memory::MemoryExecutionStore::empty(
            runbook_store_conformance::portable::p(),
            1_700_000_000_000,
        );
        (store, directory)
    }
    #[test]
    fn strict_assignment_limits_who_claims() {
        let (store, directory) = stores();
        run(async {
            language::strict_assignment_limits_who_claims(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn agents_may_not_claim_strict_work_open_to_anyone() {
        let (store, directory) = stores();
        run(async {
            language::agents_may_not_claim_strict_work_open_to_anyone(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn an_unresolved_strict_assignee_is_resolved_again_at_claim() {
        let (store, directory) = stores();
        run(async {
            language::an_unresolved_strict_assignee_is_resolved_again_at_claim(&store, &directory)
                .await;
            Ok(())
        })
    }
    #[test]
    fn a_person_takes_over_an_agent_task() {
        let (store, directory) = stores();
        run(async {
            language::a_person_takes_over_an_agent_task(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn loop_while_runs_and_replays() {
        let (store, directory) = stores();
        run(async {
            language::loop_while_runs_and_replays(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn input_types_and_pattern_run_and_replay() {
        let (store, directory) = stores();
        run(async {
            language::input_types_and_pattern_run_and_replay(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn parallel_join_cancels_open_branches() {
        let (store, directory) = stores();
        run(async {
            language::parallel_join_cancels_open_branches(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn on_conflict_reject_refuses_a_full_pool() {
        let (store, directory) = stores();
        run(async {
            language::on_conflict_reject_refuses_a_full_pool(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn on_conflict_cancel_previous_with_a_wider_pool_cancels_the_oldest_holder() {
        let (store, directory) = stores();
        run(async {
            language::on_conflict_cancel_previous_with_a_wider_pool_cancels_the_oldest_holder(
                &store, &directory,
            )
            .await;
            Ok(())
        })
    }
    #[test]
    fn on_conflict_cancel_previous_leaves_other_projects_alone() {
        let (store, directory) = stores();
        let other = store.scoped(runbook_store_conformance::portable::q());
        run(async {
            language::on_conflict_cancel_previous_leaves_other_projects_alone(
                &store, &other, &directory,
            )
            .await;
            Ok(())
        })
    }
    #[test]
    fn on_conflict_cancel_previous_bounds_its_preparation() {
        let (store, directory) = stores();
        run(async {
            language::on_conflict_cancel_previous_bounds_its_preparation(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn parallel_join_clears_timers_of_cancelled_branches() {
        let (store, directory) = stores();
        run(async {
            language::parallel_join_clears_timers_of_cancelled_branches(&store, &directory).await;
            Ok(())
        })
    }
    /// LANG4: also pins the conformance trace (store-level: a call crosses
    /// executions).
    #[test]
    fn call_runs_a_child_and_takes_its_outcome() {
        let (store, directory) = stores();
        run(async {
            let trace = language::call_runs_a_child_and_takes_its_outcome(&store, &directory).await;
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../conformance/execution/call.json");
            if std::env::var_os("RUNBOOK_UPDATE_FIXTURES").is_some() {
                std::fs::write(
                    &path,
                    format!("{}\n", serde_json::to_string_pretty(&trace).unwrap()),
                )
                .unwrap();
            }
            let stored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(stored, trace, "{}", path.display());
            Ok(())
        })
    }
    macro_rules! call_cases {
        ($($name:ident),* $(,)?) => {$(
            #[test]
            fn $name() {
                let (store, directory) = stores();
                run(async {
                    language::$name(&store, &directory).await;
                    Ok(())
                })
            }
        )*};
    }
    call_cases!(
        overlapping_start_leases_never_cancel_a_wanted_child,
        a_crash_between_the_child_and_its_start_record_recovers,
        a_result_before_the_start_record_is_kept,
        a_child_started_after_its_parent_was_cancelled_never_runs,
        cancelling_the_top_of_a_call_chain_cancels_every_level,
        fan_out_calls_start_one_child_per_step_run,
        a_child_is_not_deleted_before_its_result_is_delivered,
        finished_call_items_are_no_longer_listed,
    );
    #[test]
    fn cancelling_a_parent_cancels_its_child() {
        let (store, directory) = stores();
        run(async {
            language::cancelling_a_parent_cancels_its_child(&store, &directory).await;
            Ok(())
        })
    }
    #[test]
    fn a_child_started_after_its_refusal_was_recorded_is_cancelled() {
        let (store, directory) = stores();
        let limited = store.with_quota(runbook_core::quotas::QuotaLimits {
            executions_monthly: Some(1),
        });
        run(async {
            language::a_child_started_after_its_refusal_was_recorded_is_cancelled(
                &store, &limited, &directory,
            )
            .await;
            Ok(())
        })
    }
    #[test]
    fn a_child_refused_for_quota_fails_the_call_step() {
        let (store, directory) = stores();
        let limited = store.with_quota(runbook_core::quotas::QuotaLimits {
            executions_monthly: Some(1),
        });
        run(async {
            language::a_child_refused_for_quota_fails_the_call_step(&store, &limited, &directory)
                .await;
            Ok(())
        })
    }
    /// Also pins the conformance trace (store-level: admission is not a
    /// pure-engine concern).
    #[test]
    fn on_conflict_cancel_previous_replaces_the_holders() {
        let (store, directory) = stores();
        run(async {
            let trace =
                language::on_conflict_cancel_previous_replaces_the_holders(&store, &directory)
                    .await;
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../conformance/execution/concurrency-on-conflict.json");
            if std::env::var_os("RUNBOOK_UPDATE_FIXTURES").is_some() {
                std::fs::write(
                    &path,
                    format!("{}\n", serde_json::to_string_pretty(&trace).unwrap()),
                )
                .unwrap();
            }
            let stored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(stored, trace, "{}", path.display());
            Ok(())
        })
    }
}
#[test]
fn quotas_monthly_starts_are_counted_capped_and_roll_over() {
    run(
        runbook_store_conformance::quotas::monthly_starts_are_counted_capped_and_roll_over(
            &MemoryFactory,
        ),
    )
}

#[test]
fn notify_preferences_salt_and_token() {
    run(runbook_store_conformance::notify::preferences_salt_and_token(&MemoryFactory))
}
#[test]
fn notify_outbox_records_once_and_claims_are_fenced() {
    run(
        runbook_store_conformance::notify::outbox_records_once_and_claims_are_fenced(
            &MemoryFactory,
        ),
    )
}
#[test]
fn notify_digest_rows_are_claimed_by_age_and_marked_sent() {
    run(
        runbook_store_conformance::notify::digest_rows_are_claimed_by_age_and_marked_sent(
            &MemoryFactory,
        ),
    )
}
