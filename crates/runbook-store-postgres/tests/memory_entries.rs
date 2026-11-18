//! MEM2: the MEM1 memory conformance cases on PostgreSQL (migration 0026).
//! The two T0 cases run in `t0_pending.rs`. Each case owns one database.
mod support;
use runbook_core::StoreError;
use runbook_store_conformance::{StoreFactory, execution::projection, memory_entries, portable::p};
use support::factory::with_factory;

macro_rules! store_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| {
                memory_entries::$name(&f.create(projection()).await?).await
            })
            .await;
        }
    )*};
}
store_cases!(
    secrets_are_stored_redacted,
    entry_quota_is_per_runbook,
    patches_follow_transitions_and_keep_history,
    history_is_bounded_lockable_and_restorable,
    people_keep_their_revision_and_the_store_holds_locks,
    search_filters_and_pages,
);

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn proposals_commit_with_the_execution() {
    with_factory(async |f| memory_entries::proposals_commit_with_the_execution(f).await).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn proposals_racing_a_full_quota_are_never_lost() {
    with_factory(async |f| memory_entries::proposals_racing_a_full_quota_are_never_lost(f).await)
        .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn runs_save_live_entries() {
    with_factory(async |f| memory_entries::runs_save_live_entries(f).await).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn runs_save_live_memory_and_agents_only_edit() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        memory_entries::runs_save_live_memory_and_agents_only_edit(&store, &f.fresh_directory())
            .await;
        Ok::<(), StoreError>(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn agent_writes_commit_a_notice() {
    with_factory(async |f| memory_entries::agent_writes_commit_a_notice(f).await).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn runs_edit_under_a_claim_and_agents_learn_nothing_hidden() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        memory_entries::runs_edit_under_a_claim_and_agents_learn_nothing_hidden(
            &store,
            &f.fresh_directory(),
        )
        .await;
        Ok::<(), StoreError>(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn proposals_are_capped_and_reads_audited() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        memory_entries::proposals_are_capped_and_reads_audited(&store, &f.fresh_directory()).await;
        Ok::<(), StoreError>(())
    })
    .await;
}

/// CF13b review round 1, finding 2: memory writes are held to the run
/// ceiling (each settles before it commits).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn memory_writes_are_held_to_the_run_ceiling() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        runbook_store_conformance::run_ceiling::memory_writes_are_held_to_the_run_ceiling(
            &store,
            &f.fresh_directory(),
            |budget| {
                runbook_store_postgres::PostgresExecutionStore::new(
                    f.config.clone().with_run_budget(budget),
                    p(),
                )
            },
        )
        .await;
        Ok::<(), StoreError>(())
    })
    .await;
}

/// Two writers racing for the last slot of a runbook's quota: exactly one
/// wins (the quota lock serializes them).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn quota_holds_under_concurrent_creates() {
    use runbook_core::memory_entries::{MemoryEntryRecord, MemoryStore};
    use runbook_model::{MemoryScope, MemorySource, MemoryStatus};
    with_factory(async |f| {
        let a = f.fresh_store(p()).await;
        let b = f.fresh_store(p()).await;
        let entry = |n: u8| MemoryEntryRecord {
            id: format!("mem_{n:026}").parse().unwrap(),
            scope: p(),
            memory_scope: MemoryScope::Project,
            step_id: None,
            title: format!("Fact {n}"),
            body: "body".to_owned().try_into().unwrap(),
            tags: vec![],
            status: MemoryStatus::Active,
            source: MemorySource::Person {
                user_id: "usr_00000000000000000000000001".parse().unwrap(),
            },
            revision: 1,
            expires_at_ms: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            dropped: None,
            last_writer: None,
            locked: false,
            revisions_pruned: 0,
            person_revision: 0,
            agent_edit_day: 0,
            agent_edits: 0,
        };
        let scope = p();
        let (x, y) = tokio::join!(
            a.create_memory(&scope, entry(1), "human:usr_1", 1),
            b.create_memory(&scope, entry(2), "human:usr_1", 1)
        );
        assert_eq!(
            usize::from(x.is_ok()) + usize::from(y.is_ok()),
            1,
            "{x:?} {y:?}"
        );
        let loser = if x.is_ok() { y } else { x };
        assert_eq!(loser, Err(StoreError::InvalidBatch("memory.limit_reached")));
        Ok::<(), StoreError>(())
    })
    .await;
}
