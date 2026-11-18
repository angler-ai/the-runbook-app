//! Store hardening: real races on PostgreSQL. Every contender runs on its own
//! pool (its own backend connection), so the database decides each race.
//! Each case keeps its connection use small (at most eight contenders).
//! The credential refresh race lives in `credentials.rs`
//! (`refresh_reuse_revokes_successor`).
mod support;
use futures_util::future::join_all;
use runbook_core::{
    CommitResult, ExecutionStore, TimerChange, TimerRecord,
    actor::{Replay, begin_actor},
    catalog::{AdmissionState, AdmitRequest, CatalogStore},
    claims::{append_logs, claim_step},
    start::{StartCommand, StartStore, prepare_start_commit},
    work_queue::{WorkLease, WorkQueue},
};
use runbook_model::{ClaimRef, ClaimRequest, Data, ExecutionStatus, LogsRequest};
use runbook_store_conformance::{
    SessionFactory, StoreFactory,
    catalog::{batch, id},
    execution::{self, projection},
    parity,
    portable::{self, GATE, PLAIN, agent, exe, p, rb, user},
    scope,
};
use runbook_store_postgres::{PostgresDirectory, PostgresExecutionStore};
use std::collections::{BTreeMap, BTreeSet};
use support::factory::{PostgresFactory, with_factory};

async fn contenders(f: &PostgresFactory, n: usize) -> Vec<PostgresExecutionStore> {
    let mut stores = vec![];
    for _ in 0..n {
        stores.push(f.fresh_store(scope(1)).await);
    }
    stores
}
async fn count(f: &PostgresFactory, table: &str, execution: &str) -> i64 {
    f.admin
        .client()
        .query_one(
            &format!("SELECT count(*) FROM runbook.{table} WHERE execution_id = $1"),
            &[&execution],
        )
        .await
        .unwrap()
        .get(0)
}

/// Four writers commit against the same head at once: exactly one wins,
/// every loser gets Conflict, and the head holds one copy of each row.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn commit_race_on_one_head_has_one_winner() {
    with_factory(async |f| {
        let id = projection().execution_id;
        f.create(projection()).await?;
        let stores = contenders(f, 4).await;
        let results = join_all(stores.iter().map(|s| s.commit(execution::batch()))).await;
        let results: Vec<_> = results.into_iter().collect::<Result<_, _>>()?;
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == CommitResult::Committed(1))
                .count(),
            1,
            "{results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == CommitResult::Conflict)
                .count(),
            3,
            "{results:?}"
        );
        let run = f.fresh_store(scope(1)).await.load(&id).await?;
        assert_eq!(run.revision, 1);
        assert_eq!(run.events, execution::batch().events);
        for table in [
            "execution_event",
            "execution_timer",
            "execution_outbox",
            "execution_idempotency",
            "execution_claim",
        ] {
            assert_eq!(count(f, table, id.as_str()).await, 1, "{table}");
        }
        Ok(())
    })
    .await;
}

async fn started(
    f: &PostgresFactory,
    source: &str,
    slug: &str,
) -> (PostgresExecutionStore, PostgresDirectory) {
    let store = f.fresh_store(p()).await;
    let directory = f.fresh_directory();
    portable::directory(&directory).await;
    parity::publish(&store, 1, slug, source, 1).await;
    portable::start(&store, &directory, &p(), 1, 1).await;
    (store, directory)
}
fn only_step(
    work: &runbook_core::actor::ActorWork<PostgresExecutionStore>,
) -> runbook_model::StepRunId {
    work.runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "only")
        .and_then(|s| s.id.clone())
        .unwrap()
}

/// Two agents claim one step at once, each through its own session: one
/// claim lands at generation 1, the other is refused, and the stored claim
/// is the winner's.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn claim_race_on_one_step_has_one_owner() {
    with_factory(async |f| {
        let (store, directory) = started(f, PLAIN, "plain").await;
        let step = only_step(&begin_actor(&store, &directory, p(), &exe(1)).await.unwrap());
        let revision = store.load(&exe(1)).await?.revision;
        let stores = contenders(f, 2).await;
        let racers = ["bot-a", "bot-b"];
        let results = join_all(stores.iter().zip(racers).enumerate().map(|(n, (s, name))| {
            let directory = &directory;
            let step = &step;
            async move {
                let mut who = agent();
                if let runbook_model::ActorRef::Agent {
                    name: agent_name, ..
                } = &mut who.actor
                {
                    *agent_name = name.into();
                }
                let work = begin_actor(s, directory, p(), &exe(1)).await.unwrap();
                claim_step(
                    work,
                    &who,
                    step,
                    &ClaimRequest {
                        take_over: false,
                        session_id: format!("s{n}"),
                        actor: None,
                    },
                    format!("clm_{:026}", n + 1).parse().unwrap(),
                    None,
                )
                .await
                .map(|claim| (claim, who.actor))
            }
        }))
        .await;
        let won: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(won.len(), 1, "{results:?}");
        // The loser is refused cleanly, never a backend failure.
        let lost: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
        assert_eq!(
            lost.iter().map(|e| e.code()).collect::<Vec<_>>(),
            vec!["step.not_claimable"],
            "{results:?}"
        );
        let (claim, owner) = won[0];
        assert_eq!(claim.generation.get(), 1);
        let stored = runbook_core::claims::ClaimStore::claim_record(&store, &exe(1), &step)
            .await?
            .unwrap();
        assert_eq!(stored.claim_id, claim.id);
        assert_eq!(&stored.owner, owner);
        assert_eq!(stored.generation, 1);
        // One claim event, not two.
        assert_eq!(store.load(&exe(1)).await?.revision, revision + 1);
        Ok(())
    })
    .await;
}

/// Actor replay racing on one key from two pools: claim and append_logs
/// each take effect once and both callers get the identical response.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn actor_replay_race_has_one_effect() {
    with_factory(async |f| {
        let (store, directory) = started(f, PLAIN, "plain").await;
        let step = only_step(&begin_actor(&store, &directory, p(), &exe(1)).await.unwrap());
        let revision = store.load(&exe(1)).await?.revision;
        let replay = |key: &str| Replay {
            key: key.into(),
            principal: agent().principal,
            request_hash: [7; 32],
        };
        let stores = contenders(f, 2).await;
        let claim_replay = replay("claim-1");
        let claims = join_all(stores.iter().enumerate().map(|(n, s)| {
            let (directory, step, claim_replay) = (&directory, &step, &claim_replay);
            async move {
                let work = begin_actor(s, directory, p(), &exe(1)).await.unwrap();
                claim_step(
                    work,
                    &agent(),
                    step,
                    &ClaimRequest {
                        take_over: false,
                        session_id: "s1".into(),
                        actor: None,
                    },
                    format!("clm_{:026}", n + 1).parse().unwrap(),
                    Some(claim_replay),
                )
                .await
                .unwrap()
            }
        }))
        .await;
        assert_eq!(claims[0], claims[1], "replayed claim returns the saved ref");
        assert_eq!(store.load(&exe(1)).await?.revision, revision + 1);

        let claim: ClaimRef = claims[0].clone();
        let logs_replay = replay("logs-1");
        let counts = join_all(stores.iter().map(|s| {
            let (step, claim, logs_replay) = (&step, &claim, &logs_replay);
            async move {
                append_logs(
                    s,
                    &agent(),
                    &exe(1),
                    step,
                    &LogsRequest {
                        claim: claim.clone(),
                        logs: vec!["one".into(), "two".into()],
                    },
                    Some(logs_replay),
                )
                .await
                .unwrap()
            }
        }))
        .await;
        assert_eq!(counts, vec![2, 2]);
        assert_eq!(count(f, "claim_log", exe(1).as_str()).await, 2);
        Ok(())
    })
    .await;
}

fn start_command(n: u8, key: &str) -> StartCommand {
    StartCommand {
        execution_id: exe(n),
        runbook_id: rb(1),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: key.into(),
        principal: format!("user:{}", user(1)),
        request_hash: [1; 32],
    }
}

/// Idempotent start racing with one key from three pools (each with a
/// host-generated execution ID): one run is created, and every caller gets
/// the identical saved outcome.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn idempotent_start_race_has_one_effect() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        portable::directory(&f.fresh_directory()).await;
        parity::publish(&store, 1, "gate", GATE, 1).await;
        let stores = contenders(f, 3).await;
        let mut commits = vec![];
        for (n, s) in stores.iter().enumerate() {
            let command = start_command(n as u8 + 1, "start-same");
            commits.push(prepare_start_commit(s, s, &command).await.unwrap());
        }
        let outcomes = join_all(
            stores
                .iter()
                .zip(commits)
                .map(|(s, commit)| s.commit_start(commit)),
        )
        .await;
        let outcomes: Vec<_> = outcomes.into_iter().collect::<Result<_, _>>()?;
        assert!(
            outcomes.windows(2).all(|w| w[0] == w[1]),
            "every caller sees the saved outcome: {outcomes:?}"
        );
        let winner = outcomes[0].execution_id.clone();
        let runs: Vec<_> = store
            .list_runs()
            .await?
            .into_iter()
            .map(|r| r.execution_id)
            .collect();
        assert_eq!(runs, vec![winner.clone()]);
        for n in 1..=3u8 {
            let exists = store.start_record(&exe(n)).await?.is_some();
            assert_eq!(exists, exe(n) == winner, "{n}");
        }
        assert_eq!(
            f.admin
                .client()
                .query_one("SELECT count(*) FROM runbook.start_replay", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        Ok(())
    })
    .await;
}

async fn admission_rows(f: &PostgresFactory) -> BTreeMap<String, (String, Option<i64>)> {
    f.admin
        .client()
        .query(
            "SELECT execution_id, state, queue_order FROM runbook.admission",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2))))
        .collect()
}
fn request(n: u8, limit: u32) -> AdmitRequest {
    AdmitRequest {
        execution_id: id(n),
        key: "api".into(),
        limit,
        idempotency_key: format!("admit-{n}"),
    }
}
async fn admitted(store: &PostgresExecutionStore, ns: impl Iterator<Item = u8>) -> BTreeSet<u8> {
    let mut out = BTreeSet::new();
    for n in ns {
        if store.admission(&id(n)).await.unwrap().map(|a| a.state) == Some(AdmissionState::Admitted)
        {
            out.insert(n);
        }
    }
    out
}

/// Six admits race at a pool limit of two: exactly two are admitted. Then
/// both holders finish while two newcomers arrive, all at once: the pool
/// never exceeds two, the two oldest waiters are promoted, and the
/// newcomers queue behind the remaining waiters (FIFO).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn admission_race_at_pool_limit_keeps_limit_and_fifo() {
    with_factory(async |f| {
        let store = f.create(projection()).await?;
        let stores = contenders(f, 6).await;
        let results = join_all(
            stores
                .iter()
                .zip(1..=6u8)
                .map(|(s, n)| s.admit(request(n, 2))),
        )
        .await;
        let results: Vec<_> = results.into_iter().collect::<Result<_, _>>()?;
        let first: BTreeSet<u8> = results
            .iter()
            .zip(1..=6u8)
            .filter(|(a, _)| a.state == AdmissionState::Admitted)
            .map(|(_, n)| n)
            .collect();
        assert_eq!(first.len(), 2, "{results:?}");
        assert_eq!(admitted(&store, 1..=6).await, first);
        let rows = admission_rows(f).await;
        let mut waiters: Vec<(i64, u8)> = (1..=6u8)
            .filter(|n| !first.contains(n))
            .map(|n| (rows[id(n).as_str()].1.expect("waiter has a queue order"), n))
            .collect();
        waiters.sort();
        let distinct: BTreeSet<_> = waiters.iter().map(|w| w.0).collect();
        assert_eq!(distinct.len(), 4, "queue orders are unique: {waiters:?}");

        // Holders finish and newcomers arrive concurrently.
        let finishing: Vec<u8> = first.iter().copied().collect();
        let (released, arrived) = tokio::join!(
            join_all(stores.iter().zip(&finishing).map(|(s, n)| s.commit(batch(
                *n,
                0,
                1,
                ExecutionStatus::Succeeded
            )))),
            join_all(
                stores[2..]
                    .iter()
                    .zip(7..=8u8)
                    .map(|(s, n)| s.admit(request(n, 2)))
            ),
        );
        for r in released {
            assert_eq!(r?, CommitResult::Committed(1));
        }
        for a in arrived {
            assert_eq!(
                a?.state,
                AdmissionState::Queued,
                "newcomers never jump the queue"
            );
        }
        let expected: BTreeSet<u8> = waiters[..2].iter().map(|w| w.1).collect();
        assert_eq!(admitted(&store, 1..=8).await, expected, "{waiters:?}");
        for n in &finishing {
            assert_eq!(
                store.admission(&id(*n)).await?.unwrap().state,
                AdmissionState::Released
            );
        }
        Ok(())
    })
    .await;
}

/// Seven due work items (six timers and one notification) and two workers
/// claiming in small batches at once: every item is leased by exactly one
/// worker, each ack lands once, and nothing is left to fire.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn two_workers_fire_each_due_item_once() {
    with_factory(async |f| {
        let id = projection().execution_id;
        let seeded = f.create(projection()).await?;
        let now = f.backend_time_ms(&seeded).await?;
        let mut b = execution::batch();
        b.claims.clear();
        b.idempotency.clear();
        b.timers = (0..6)
            .map(|k| {
                TimerChange::Upsert(TimerRecord {
                    execution_id: id.clone(),
                    key: format!("t{k}"),
                    generation: 1,
                    due_at_ms: now - 1,
                })
            })
            .collect();
        seeded.commit(b).await?;
        let workers = contenders(f, 2).await;
        let mut leases: Vec<(usize, WorkLease)> = vec![];
        loop {
            let round = join_all(workers.iter().map(|w| w.claim_due(2, 60_000))).await;
            let mut claimed = 0;
            for (n, r) in round.into_iter().enumerate() {
                for lease in r? {
                    claimed += 1;
                    leases.push((n, lease));
                }
            }
            if claimed == 0 {
                break;
            }
            assert!(leases.len() <= 7, "an item was leased twice: {leases:?}");
        }
        let ids: BTreeSet<_> = leases.iter().map(|(_, l)| l.id.clone()).collect();
        assert_eq!(ids.len(), 7, "{leases:?}");
        assert_eq!(leases.len(), 7, "each item is leased exactly once");
        let acks = join_all(
            leases
                .iter()
                .map(|(n, l)| workers[*n].ack(&l.id, l.generation)),
        )
        .await;
        assert!(acks.iter().all(Result::is_ok), "{acks:?}");
        for (n, l) in &leases {
            assert!(workers[*n].ack(&l.id, l.generation).await.is_err());
        }
        let restarted = f.fresh_store(scope(1)).await;
        assert!(restarted.claim_due(10, 60_000).await?.is_empty());
        Ok(())
    })
    .await;
}
