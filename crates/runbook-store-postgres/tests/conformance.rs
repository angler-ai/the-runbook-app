//! B2a.1/B2a.2 shared cases, unchanged, plus Postgres-only fault and restart cases.
//! Each test owns one database; `create` empties it so every store starts fresh.
mod support;
use runbook_core::{
    ExecutionCommit, ExecutionStore, StoreError, WriteStage, catalog::CatalogStore,
};
use runbook_store_conformance::{DirectoryFactory, StoreFactory, scope};
use runbook_store_postgres::{PostgresDirectory, PostgresExecutionStore, PostgresStore};
use std::time::Duration;
use support::factory::with_factory;

macro_rules! shared_cases {
    ($($module:ident :: $name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| runbook_store_conformance::$module::$name(f).await).await;
        }
    )*};
}
shared_cases!(
    execution::atomic_batch_or_nothing,
    execution::stale_commit_has_no_side_effects,
    execution::one_claim_generation_winner,
    execution::idempotent_response_is_payload_bound,
    execution::replay_equals_projection,
    clock::store_clock_controls_deadline_equality,
    clock::session_time_and_snapshot_are_fenced,
    clock::session_commits_at_its_begin_time,
    catalog::project_queries_never_return_foreign_rows,
    catalog::revision_precondition_rejects_stale_replace,
    catalog::derived_rows_ignore_old_or_duplicate_sequence,
    catalog::concurrency_limit_one_admits_and_queues,
    catalog::derived_rows_rollback_with_execution,
    catalog::derived_rows_cannot_release_running_execution,
    catalog::pool_limit_replacement_promotes_waiters,
    quotas::monthly_starts_are_counted_capped_and_roll_over,
);

macro_rules! directory_cases {
    ($($module:ident :: $name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| {
                runbook_store_conformance::$module::$name(f).await.unwrap();
                Ok(())
            }).await;
        }
    )*};
}
macro_rules! notify_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| {
                runbook_store_conformance::notify::$name(f).await.unwrap();
                Ok(())
            }).await;
        }
    )*};
}
notify_cases!(
    preferences_salt_and_token,
    outbox_records_once_and_claims_are_fenced,
    digest_rows_are_claimed_by_age_and_marked_sent,
);

directory_cases!(
    directory::org_role_permission_matrix,
    directory::last_active_owner_is_protected,
    directory::org_roles_are_org_local,
    directory::directory_snapshot_is_org_scoped,
    directory::directory_revision_fences_mutations,
    directory::directory_rejects_cross_org_membership,
    directory::directory_slug_uniqueness_is_org_scoped,
    directory::directory_deactivation_and_removal_are_visible,
    directory::directory_fixture_has_distinct_principals,
    directory::verified_email_key_reads_are_unique_and_atomic,
    directory::invalid_batches_and_duplicate_ids_do_not_leak_indexes,
    directory::shared_identity_deactivation_is_org_local,
    eligibility::team_membership_does_not_grant_project_access,
    eligibility::approval_requires_current_target_match,
    eligibility::unknown_static_reference_fails_publication,
    eligibility::handoff_missing_route_notifies_admins_without_broadening,
    eligibility::two_users_not_two_tokens_satisfy_quorum,
    eligibility::unresolved_handoff_without_admin_still_records_blocked_route,
);

async fn seed_directory(s: &PostgresDirectory) {
    use runbook_core::directory::*;
    use runbook_model::ProjectRole;
    use runbook_store_conformance::directory::{org, project, team, user};
    let mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org1".into(),
        },
        DirectoryMutation::CreateUser(UserRecord {
            id: user(1),
            name: "User1".into(),
            active: true,
            role: OrgRole::Member,
        }),
        DirectoryMutation::CreateProject(ProjectRecord {
            id: project(1),
            org_id: org(1),
            slug: "api".into(),
            title: "API".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: team(1),
            org_id: org(1),
            slug: "oncall".into(),
            title: "Oncall".into(),
        }),
        DirectoryMutation::GrantProjectRole {
            project_id: project(1),
            user_id: user(1),
            role: ProjectRole::Operator,
        },
        DirectoryMutation::AddTeamMember {
            team_id: team(1),
            user_id: user(1),
        },
    ];
    assert_eq!(
        s.commit(DirectoryCommit {
            org_id: org(1),
            expected_revision: 0,
            mutations
        })
        .await
        .unwrap(),
        runbook_core::CommitResult::Committed(1)
    );
}

/// A batch that writes rows and then fails must leave a separate pool looking
/// at the complete old snapshot, the old revision and no leaked key reads.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn directory_commit_failure_rolls_back_everywhere() {
    use runbook_core::directory::*;
    use runbook_store_conformance::directory::{org, team, user};
    with_factory(async |f| {
        let store = f.create_directory().await.unwrap();
        seed_directory(&store).await;
        let before = store.snapshot(&org(1)).await.unwrap();
        let failed = store
            .commit(DirectoryCommit {
                org_id: org(1),
                expected_revision: 1,
                mutations: vec![
                    DirectoryMutation::CreateUser(UserRecord {
                        id: user(9),
                        name: "Transient".into(),
                        active: true,
                        role: OrgRole::Member,
                    }),
                    DirectoryMutation::SetUserActive {
                        user_id: user(1),
                        active: false,
                    },
                    DirectoryMutation::CreateTeam(TeamRecord {
                        id: team(9),
                        org_id: org(1),
                        slug: "oncall".into(),
                        title: "Duplicate slug fails last".into(),
                    }),
                ],
            })
            .await;
        assert!(failed.is_err(), "{failed:?}");
        let observer = f.fresh_directory();
        assert_eq!(observer.snapshot(&org(1)).await.unwrap(), before);
        assert_eq!(observer.user_by_id(&user(9)).await.unwrap(), None);
        // The failed batch did not consume the revision: it is still usable.
        assert_eq!(
            observer
                .commit(DirectoryCommit {
                    org_id: org(1),
                    expected_revision: 1,
                    mutations: vec![DirectoryMutation::SetUserActive {
                        user_id: user(1),
                        active: false
                    }],
                })
                .await
                .unwrap(),
            runbook_core::CommitResult::Committed(2)
        );
        Ok(())
    })
    .await;
}

/// R8: the schema itself rejects a membership row that pairs one
/// organization's user with another organization's project or team.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn membership_rows_cannot_cross_organizations() {
    use runbook_core::directory::*;
    use runbook_store_conformance::directory::{org, project, team, user};
    with_factory(async |f| {
        let store = f.create_directory().await.unwrap();
        seed_directory(&store).await;
        store
            .commit(DirectoryCommit {
                org_id: org(2),
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Org2".into(),
                    },
                    DirectoryMutation::CreateUser(UserRecord {
                        id: user(2),
                        name: "User2".into(),
                        active: true,
                        role: OrgRole::Member,
                    }),
                    DirectoryMutation::CreateProject(ProjectRecord {
                        id: project(2),
                        org_id: org(2),
                        slug: "web".into(),
                        title: "Web".into(),
                    }),
                    DirectoryMutation::CreateTeam(TeamRecord {
                        id: team(2),
                        org_id: org(2),
                        slug: "ops".into(),
                        title: "Ops".into(),
                    }),
                ],
            })
            .await
            .unwrap();
        let admin = f.admin.client();
        // Correctly scoped rows are accepted.
        admin
            .execute(
                "INSERT INTO runbook.project_member (org_id, project_id, user_id, role) \
                 VALUES ($1, $2, $3, 'admin')",
                &[&org(2).as_str(), &project(2).as_str(), &user(2).as_str()],
            )
            .await
            .unwrap();
        // Org 2's user on org 1's project or team: rejected by the schema,
        // even though every referenced row exists on its own.
        for sql in [
            "INSERT INTO runbook.project_member (org_id, project_id, user_id, role) \
             VALUES ($1, $2, $3, 'admin')",
            "INSERT INTO runbook.team_member (org_id, team_id, user_id) VALUES ($1, $2, $3)",
        ] {
            let scoped = if sql.contains("project_member") {
                project(1).to_string()
            } else {
                team(1).to_string()
            };
            assert!(
                admin
                    .execute(
                        sql,
                        &[&org(2).as_str(), &scoped.as_str(), &user(2).as_str()]
                    )
                    .await
                    .is_err(),
                "{sql}"
            );
        }
        Ok(())
    })
    .await;
}

/// Two pools creating the same organization: the primary key decides, and
/// the loser returns Conflict rather than an error or a second revision.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn organization_creation_race_loser_conflicts() {
    use runbook_core::directory::*;
    use runbook_store_conformance::directory::org;
    with_factory(async |f| {
        let a = f.create_directory().await.unwrap();
        let b = f.fresh_directory();
        let create = |title: &str| DirectoryCommit {
            org_id: org(1),
            expected_revision: 0,
            mutations: vec![DirectoryMutation::CreateOrganization {
                title: title.into(),
            }],
        };
        let (left, right) = tokio::join!(a.commit(create("Left")), b.commit(create("Right")));
        let results = [left.unwrap(), right.unwrap()];
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == runbook_core::CommitResult::Committed(1))
                .count(),
            1,
            "{results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == runbook_core::CommitResult::Conflict)
                .count(),
            1,
            "{results:?}"
        );
        assert_eq!(a.snapshot(&org(1)).await.unwrap().revision, 1);
        Ok(())
    })
    .await;
}

/// A snapshot taken before another pool commits cannot authorize a new commit
/// at its now-stale revision.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn stale_snapshot_revision_cannot_commit() {
    use runbook_core::directory::*;
    use runbook_store_conformance::directory::{org, user};
    with_factory(async |f| {
        let store = f.create_directory().await.unwrap();
        seed_directory(&store).await;
        let stale = store.snapshot(&org(1)).await.unwrap();
        let writer = f.fresh_directory();
        assert_eq!(
            writer
                .commit(DirectoryCommit {
                    org_id: org(1),
                    expected_revision: stale.revision,
                    mutations: vec![DirectoryMutation::SetUserActive {
                        user_id: user(1),
                        active: false
                    }],
                })
                .await
                .unwrap(),
            runbook_core::CommitResult::Committed(2)
        );
        assert_eq!(
            store
                .commit(DirectoryCommit {
                    org_id: org(1),
                    expected_revision: stale.revision,
                    mutations: vec![DirectoryMutation::SetUserActive {
                        user_id: user(1),
                        active: true
                    }],
                })
                .await
                .unwrap(),
            runbook_core::CommitResult::Conflict
        );
        let current = store.snapshot(&org(1)).await.unwrap();
        assert_eq!(current.revision, 2);
        assert!(!current.users[&user(1)].active);
        Ok(())
    })
    .await;
}

fn release_batch() -> ExecutionCommit {
    use runbook_model::ExecutionStatus;
    let mut batch = runbook_store_conformance::execution::batch();
    let terminal = runbook_store_conformance::catalog::batch(1, 0, 1, ExecutionStatus::Succeeded);
    batch.projection = terminal.projection;
    batch.derived_outbox = terminal.derived_outbox;
    batch
}

/// Fail after each logical table write. A separate pool must see the complete
/// old state, and no table may hold a leftover row.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn fault_each_write_rolls_back() {
    use runbook_core::catalog::{AdmissionState, AdmitRequest};
    with_factory(async |f| {
        let id = runbook_store_conformance::execution::projection().execution_id;
        for stage in [
            WriteStage::Events,
            WriteStage::Projection,
            WriteStage::Timers,
            WriteStage::Outbox,
            WriteStage::Idempotency,
            WriteStage::Claims,
            WriteStage::DerivedRows,
        ] {
            let store = f
                .create(runbook_store_conformance::execution::projection())
                .await?;
            for (n, key) in [(1u8, "start-1"), (2, "start-2")] {
                store
                    .admit(AdmitRequest {
                        execution_id: runbook_store_conformance::catalog::id(n),
                        key: "api".into(),
                        limit: 1,
                        idempotency_key: key.into(),
                    })
                    .await?;
            }
            let before_rows = f.row_counts().await;
            let before = store.load(&id).await?;
            f.fail_after(&store, stage).await?;
            assert_eq!(
                store.commit(release_batch()).await,
                Err(StoreError::Backend("injected")),
                "{stage:?}"
            );
            let observer = f.fresh_store(scope(1)).await;
            assert_eq!(observer.load(&id).await?, before, "{stage:?}");
            assert_eq!(f.row_counts().await, before_rows, "{stage:?}");
            assert_eq!(
                observer
                    .admission(&runbook_store_conformance::catalog::id(2))
                    .await?
                    .unwrap()
                    .state,
                AdmissionState::Queued
            );
        }
        // Without a fault the same batch commits and releases the queue.
        let store = f.fresh_store(scope(1)).await;
        store.commit(release_batch()).await?;
        assert_eq!(
            store
                .admission(&runbook_store_conformance::catalog::id(2))
                .await?
                .unwrap()
                .state,
            AdmissionState::Admitted
        );
        Ok(())
    })
    .await;
}

/// Drop every connection, reopen, rebuild reducer state from the pinned
/// snapshot and stored events, and compare the projection and next command.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn restart_rebuilds_identical_state() {
    use runbook_engine::{
        EventEnvelope, ExecutionState, StartContext, apply, decide, prepare_start,
    };
    use runbook_model::ActorRef;
    use std::rc::Rc;
    with_factory(async |f| {
        let source = runbook_spec::parse(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: restart, title: Restart}\n\
             spec: {steps: [{id: first, title: First}, {id: second, title: Second}]}\n",
        )
        .unwrap();
        let compiled = runbook_spec::compile(&source, &Default::default()).unwrap();
        let bytes = compiled.snapshot_bytes().to_vec();
        let mut p = runbook_store_conformance::execution::projection();
        p.compiled_hash = *compiled.hash();
        let initial = ExecutionState::new(p.execution_id.clone(), Rc::new(compiled));
        let start = EventEnvelope {
            execution_id: p.execution_id.clone(),
            seq: 1,
            at_ms: 10,
            actor: ActorRef::System,
            event: prepare_start(&initial, &StartContext::default(), 10).unwrap(),
        };
        let running = apply(&initial, &start).unwrap();
        let next = decide(&running).unwrap();
        let store = f.create(p.clone()).await?;
        p.sequence = running.sequence;
        p.status = running.status;
        p.phase = running.phase;
        store
            .commit(ExecutionCommit {
                execution_id: p.execution_id.clone(),
                expected_sequence: 0,
                events: vec![start],
                projection: p.clone(),
                timers: vec![],
                outbox: vec![],
                idempotency: vec![],
                claims: vec![],
                credentials: vec![],
                derived_outbox: vec![],
            })
            .await?;
        let before = store.load(&p.execution_id).await?;
        drop(store);

        let reopened = f.fresh_store(scope(1)).await;
        let saved = reopened.load(&p.execution_id).await?;
        assert_eq!(saved, before);
        let definition = runbook_spec::restore(&bytes).unwrap();
        assert_eq!(*definition.hash(), saved.projection.compiled_hash);
        let mut replay = ExecutionState::new(p.execution_id.clone(), Rc::new(definition));
        for event in &saved.events {
            replay = apply(&replay, event).unwrap();
        }
        assert_eq!(replay, running);
        assert_eq!(saved.projection, p);
        assert_eq!(decide(&replay).unwrap(), next);
        Ok(())
    })
    .await;
}

/// Prove a contender is actually blocked on a lock before releasing it,
/// instead of assuming a sleep was long enough (R7). Bounded to 5 seconds.
async fn await_lock_waiter(admin: &PostgresStore) {
    for _ in 0..1000 {
        let waiting: i64 = admin
            .client()
            .query_one("SELECT count(*) FROM pg_locks WHERE NOT granted", &[])
            .await
            .unwrap()
            .get(0);
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("no lock waiter appeared within 5s");
}

/// The shared race case cannot force overlap on a real database. Here an open
/// session holds the head lock while a plain writer starts: the writer must wait,
/// then see the session's commit and return Conflict.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn competing_writer_waits_for_row_lock() {
    use runbook_core::{
        CommitResult,
        session::{ExecutionSession, SessionStore},
    };
    with_factory(async |f| {
        let id = runbook_store_conformance::execution::projection().execution_id;
        let store = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        let writer = f.fresh_store(scope(1)).await;
        let session = store.begin(&id).await?;
        let mut held = runbook_store_conformance::execution::batch();
        let time = session.transaction_time_ms();
        held.events[0].at_ms = time;
        held.outbox[0].at_ms = time;
        let (plain, locked) = tokio::join!(
            writer.commit(runbook_store_conformance::execution::batch()),
            async {
                await_lock_waiter(&f.admin).await;
                session.commit(held).await
            }
        );
        assert_eq!(locked?, CommitResult::Committed(1));
        assert_eq!(plain?, CommitResult::Conflict);
        let run = store.load(&id).await?;
        assert_eq!(run.revision, 1);
        assert_eq!(run.events[0].at_ms, time);
        Ok(())
    })
    .await;
}

/// R6: another tenant creating the execution between the owner check and
/// the head lock must not leak its snapshot through begin().
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn foreign_create_between_owner_check_and_lock_is_not_found() {
    use runbook_core::session::SessionStore;
    with_factory(async |f| {
        f.create(runbook_store_conformance::execution::projection())
            .await?;
        let target = runbook_store_conformance::catalog::id(7);
        let victim = f.fresh_store(scope(1)).await;
        let foreign = f.fresh_store(scope(2)).await;
        victim.pause_after_owner_check();
        let (begun, ()) = tokio::join!(victim.begin(&target), async {
            for _ in 0..1000 {
                if victim.owner_check_paused() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(
                victim.owner_check_paused(),
                "victim never reached the pause"
            );
            let mut p = runbook_store_conformance::execution::projection();
            p.execution_id = target.clone();
            foreign.create_execution(p).await.unwrap();
            victim.release_owner_check();
        });
        assert!(
            matches!(begun, Err(StoreError::NotFound)),
            "foreign snapshot leaked through begin()"
        );
        assert_eq!(victim.load(&target).await, Err(StoreError::NotFound));
        Ok(())
    })
    .await;
}

const NOW_MS: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

/// Review fix: a session that waited for the head lock must not take a time
/// earlier than the moment the lock was released.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn session_time_follows_lock_wait() {
    use runbook_core::session::{ExecutionSession, SessionStore};
    with_factory(async |f| {
        let id = runbook_store_conformance::execution::projection().execution_id;
        let store = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        let admin = f.admin.client();
        admin.batch_execute("BEGIN").await.unwrap();
        admin
            .query(
                "SELECT 1 FROM runbook.execution WHERE execution_id = $1 FOR UPDATE",
                &[&id.as_str()],
            )
            .await
            .unwrap();
        let (session, released_at) = tokio::join!(store.begin(&id), async {
            await_lock_waiter(&f.admin).await;
            let at: i64 = admin.query_one(NOW_MS, &[]).await.unwrap().get(0);
            admin.batch_execute("COMMIT").await.unwrap();
            at
        });
        assert!(session?.transaction_time_ms() >= released_at);
        Ok(())
    })
    .await;
}

/// Review fix: a writer stops waiting on a held lock, and an abandoned idle
/// session is ended by the server so its lock is freed.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn lock_wait_and_idle_session_are_bounded() {
    use runbook_core::{CommitResult, session::SessionStore};
    use runbook_store_conformance::execution::{batch, projection};
    with_factory(async |f| {
        let id = projection().execution_id;
        f.create(projection()).await?;
        let short_lock = PostgresExecutionStore::new(
            f.config
                .clone()
                .with_lock_timeouts(Duration::from_millis(200), Duration::from_secs(30)),
            scope(1),
        );
        let holder = short_lock.begin(&id).await?;
        let started = std::time::Instant::now();
        assert_eq!(
            short_lock.commit(batch()).await,
            Err(StoreError::Backend("postgres.lock_timeout"))
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(holder);

        let short_idle = PostgresExecutionStore::new(
            f.config
                .clone()
                .with_lock_timeouts(Duration::from_secs(10), Duration::from_millis(300)),
            scope(1),
        );
        let abandoned = short_idle.begin(&id).await?;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let writer = f.fresh_store(scope(1)).await;
        assert_eq!(writer.commit(batch()).await?, CommitResult::Committed(1));
        drop(abandoned);
        Ok(())
    })
    .await;
}

/// Review fix: losing a creation race to another tenant looks exactly like any
/// foreign execution ID, so the other tenant's run is not revealed.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn foreign_creation_race_reports_not_found() {
    use runbook_model::ExecutionStatus;
    use runbook_store_conformance::catalog::{batch, id};
    with_factory(async |f| {
        f.create(runbook_store_conformance::execution::projection())
            .await?;
        let foreign = f.fresh_store(scope(2)).await;
        let owner = scope(1);
        let admin = f.admin.client();
        admin.batch_execute("BEGIN").await.unwrap();
        admin
            .execute(
                "INSERT INTO runbook.execution_owner (execution_id, org_id, project_id) \
                 VALUES ($1, $2, $3)",
                &[
                    &id(7).as_str(),
                    &owner.org_id.as_str(),
                    &owner.project_id.as_str(),
                ],
            )
            .await
            .unwrap();
        let (raced, ()) = tokio::join!(
            foreign.commit(batch(7, 0, 1, ExecutionStatus::Running)),
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                admin.batch_execute("COMMIT").await.unwrap();
            }
        );
        assert_eq!(raced, Err(StoreError::NotFound));
        assert_eq!(foreign.load(&id(7)).await, Err(StoreError::NotFound));
        Ok(())
    })
    .await;
}
