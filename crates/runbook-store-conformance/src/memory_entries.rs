//! MemoryStore cases (PRD §7, D40–D41). Registered red at T0; MEM1 makes
//! them pass in memory and MEM2 on Postgres.
use crate::scope;
use runbook_core::{
    StoreError,
    catalog::TenantScope,
    memory_entries::{
        self as usecases, MEMORY_ENTRIES_PER_RUNBOOK, MEMORY_PROPOSALS_PER_RUNBOOK, MemoryAction,
        MemoryEntryRecord, MemoryPatch, MemoryProposal, MemoryQuery, MemoryRecordPage, MemoryStore,
    },
};
use runbook_model::{
    ActorRef, ExecutionId, MemoryId, MemoryScope, MemorySource, MemoryStatus, RunbookId,
    SaveMemoryRequest, UserId,
};

pub fn mem(n: u8) -> MemoryId {
    format!("mem_{n:026}").parse().unwrap()
}
fn rb() -> RunbookId {
    "rb_00000000000000000000000001".parse().unwrap()
}
pub fn entry(scope: &TenantScope, n: u8, at: i64, expires_at_ms: Option<i64>) -> MemoryEntryRecord {
    MemoryEntryRecord {
        id: mem(n),
        scope: scope.clone(),
        memory_scope: MemoryScope::Runbook { runbook_id: rb() },
        step_id: Some("regions.error_rate".into()),
        title: format!("Fact {n}"),
        body: "eu-west needs the VPN dashboard"
            .to_owned()
            .try_into()
            .unwrap(),
        tags: vec!["vpn".into()],
        status: MemoryStatus::Active,
        source: MemorySource::Person {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        revision: 1,
        expires_at_ms,
        created_at_ms: at,
        updated_at_ms: at,
        dropped: None,
        last_writer: None,
        locked: false,
        revisions_pruned: 0,
        person_revision: 0,
        agent_edit_day: 0,
        agent_edits: 0,
    }
}
/// A pending proposal written outside a run (a service token's).
pub fn pending(scope: &TenantScope, id: MemoryId, at: i64) -> MemoryEntryRecord {
    let mut e = entry(scope, 0, at, None);
    e.id = id;
    e.status = MemoryStatus::Proposed;
    e.source = MemorySource::Service {
        token_id: "tok_00000000000000000000000001".parse().unwrap(),
    };
    e
}
fn edit(title: &str) -> MemoryPatch {
    MemoryPatch::Edit {
        title: title.into(),
        body: "updated".to_owned().try_into().unwrap(),
        tags: vec![],
        step_id: None,
        expires_at_ms: None,
    }
}

/// A patch carrying a stale revision changes nothing (the route's 412).
pub async fn stale_revision_is_refused<M: MemoryStore>(m: &M) -> Result<(), StoreError> {
    let s = scope(1);
    let now = m.memory_time_ms().await?;
    let created = m
        .create_memory(&s, entry(&s, 1, now, None), "human:usr_1", 500)
        .await?;
    assert_eq!(created.revision, 1);
    let edited = m
        .patch_memory(&s, &mem(1), 1, edit("First"), "human:usr_1")
        .await?;
    assert_eq!((edited.revision, edited.title.as_str()), (2, "First"));
    assert_eq!(
        m.patch_memory(&s, &mem(1), 1, edit("Second"), "human:usr_1")
            .await,
        Err(StoreError::InvalidBatch("memory.conflict"))
    );
    let stored = m.get_memory(&s, &mem(1)).await?.unwrap();
    assert_eq!((stored.revision, stored.title.as_str()), (2, "First"));
    assert_eq!(m.memory_revisions(&s, &mem(1)).await?.len(), 2);
    Ok(())
}

/// Another project's entry reads as absent everywhere; an expired entry is
/// still readable by ID (history) but never offered to a step or a search.
pub async fn foreign_and_expired_entries_are_hidden<M: MemoryStore>(
    m: &M,
) -> Result<(), StoreError> {
    let (mine, theirs) = (scope(1), scope(2));
    let now = m.memory_time_ms().await?;
    m.create_memory(&mine, entry(&mine, 1, now, None), "human:usr_1", 500)
        .await?;
    m.create_memory(
        &mine,
        entry(&mine, 2, now, Some(now - 1)),
        "human:usr_1",
        500,
    )
    .await?;
    assert_eq!(m.get_memory(&theirs, &mem(1)).await?, None);
    assert!(m.memory_revisions(&theirs, &mem(1)).await?.is_empty());
    assert_eq!(
        m.patch_memory(&theirs, &mem(1), 1, MemoryPatch::Archive, "human:usr_2")
            .await,
        Err(StoreError::NotFound)
    );
    let all = MemoryQuery {
        within: vec![
            MemoryScope::Runbook { runbook_id: rb() },
            MemoryScope::Project,
        ],
        ..Default::default()
    };
    assert!(
        m.search_memory(&theirs, &all, None, 50)
            .await?
            .items
            .is_empty()
    );
    let step = m
        .memory_for_step(&mine, &rb(), "regions.error_rate", 5)
        .await?;
    assert_eq!(step.iter().map(|e| &e.id).collect::<Vec<_>>(), [&mem(1)]);
    let found = m.search_memory(&mine, &all, None, 50).await?;
    assert_eq!(
        found.items.iter().map(|e| &e.id).collect::<Vec<_>>(),
        [&mem(1)]
    );
    assert!(m.get_memory(&mine, &mem(2)).await?.is_some());
    Ok(())
}

// ---------------------------------------------------------------------------
// MEM1 cases (plan 2026-11-12 §2.1). They run on every adapter: the memory
// adapter in `tests/memory.rs`, PostgreSQL in its `tests/memory_entries.rs`.
// ---------------------------------------------------------------------------

pub(crate) fn save(
    title: &str,
    body: &str,
    tags: &[&str],
    step_id: Option<&str>,
) -> SaveMemoryRequest {
    SaveMemoryRequest {
        title: title.into(),
        body_markdown: body.to_owned().try_into().unwrap(),
        tags: tags.iter().map(|t| (*t).into()).collect(),
        step_id: step_id.map(str::to_owned),
        expires_at: None,
        propose: false,
    }
}
fn author() -> UserId {
    "usr_00000000000000000000000001".parse().unwrap()
}
fn runbook_scope() -> MemoryScope {
    MemoryScope::Runbook { runbook_id: rb() }
}

/// Writes go through log-equivalent redaction: a `trb_exe_` value in a title
/// or body is stored redacted in the entry and in its revision. A retried
/// create (same ID and content) answers the stored entry; the same ID with
/// other content is refused.
pub async fn secrets_are_stored_redacted<M: MemoryStore>(m: &M) -> Result<(), StoreError> {
    let s = scope(1);
    let secret = runbook_core::credentials::encode_secret(
        runbook_core::credentials::TokenKind::Execution,
        [9; 32],
    );
    let request = save(
        &format!("Token {secret}"),
        &format!("Use the VPN dashboard.\nauthorization: Bearer {secret}\n"),
        &["vpn"],
        Some("regions.error_rate"),
    );
    let created = usecases::create(
        m,
        &s,
        mem(1),
        runbook_scope(),
        &request,
        None,
        MemorySource::Person { user_id: author() },
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    )
    .await
    .map_err(actor_store)?;
    let stored = m.get_memory(&s, &mem(1)).await?.unwrap();
    assert_eq!(created, stored);
    let history = m.memory_revisions(&s, &mem(1)).await?;
    for text in [
        stored.title.clone(),
        stored.body.as_ref().to_owned(),
        serde_json::to_string(&history).unwrap(),
    ] {
        assert!(!text.contains(&secret), "{text}");
        assert!(!text.contains("trb_exe_"), "{text}");
    }
    assert!(stored.title.contains("[REDACTED:runbook_exe]"));
    assert!(stored.body.as_ref().starts_with("Use the VPN dashboard."));
    assert_eq!(stored.status, MemoryStatus::Active);
    assert_eq!(stored.source, MemorySource::Person { user_id: author() });
    // A retry is answered from the stored entry; nothing new is written.
    let again = usecases::create(
        m,
        &s,
        mem(1),
        runbook_scope(),
        &request,
        None,
        MemorySource::Person { user_id: author() },
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    )
    .await
    .map_err(actor_store)?;
    assert_eq!(again, stored);
    assert_eq!(m.memory_revisions(&s, &mem(1)).await?.len(), 1);
    let other = usecases::create(
        m,
        &s,
        mem(1),
        runbook_scope(),
        &save("Other", "other", &[], None),
        None,
        MemorySource::Person { user_id: author() },
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    )
    .await;
    assert_eq!(
        other.map_err(|e| e.code()).unwrap_err(),
        "request.idempotency_key_reused"
    );
    Ok(())
}

fn actor_store(e: runbook_core::actor::ActorError) -> StoreError {
    match e {
        runbook_core::actor::ActorError::Store(e) => e,
        other => panic!("unexpected rejection {other:?}"),
    }
}

/// 500 proposed or active entries per runbook, checked atomically by the
/// store; the project has its own quota and archiving frees a slot.
pub async fn entry_quota_is_per_runbook<M: MemoryStore>(m: &M) -> Result<(), StoreError> {
    let s = scope(1);
    let now = m.memory_time_ms().await?;
    assert_eq!(MEMORY_ENTRIES_PER_RUNBOOK, 500);
    for n in 0..MEMORY_ENTRIES_PER_RUNBOOK {
        let mut e = entry(&s, 0, now, None);
        e.id = format!("mem_1{n:025}").parse().unwrap();
        m.create_memory(&s, e, "human:usr_1", MEMORY_ENTRIES_PER_RUNBOOK)
            .await?;
    }
    let full = usecases::create(
        m,
        &s,
        mem(1),
        runbook_scope(),
        &save("One more", "body", &[], None),
        None,
        MemorySource::Person { user_id: author() },
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    )
    .await;
    assert_eq!(
        full.map_err(|e| e.code()).unwrap_err(),
        "memory.limit_reached"
    );
    assert_eq!(m.get_memory(&s, &mem(1)).await?, None);
    // Proposals have their own quota: a full active quota still takes
    // them, but accepting one needs an active slot.
    assert_eq!(MEMORY_PROPOSALS_PER_RUNBOOK, 100);
    for n in 0..MEMORY_PROPOSALS_PER_RUNBOOK {
        let e = pending(&s, format!("mem_3{n:025}").parse().unwrap(), now);
        m.create_memory(&s, e, "service:tok_1", MEMORY_PROPOSALS_PER_RUNBOOK)
            .await?;
    }
    let over = pending(&s, mem(4), now);
    assert_eq!(
        m.create_memory(&s, over, "service:tok_1", MEMORY_PROPOSALS_PER_RUNBOOK)
            .await,
        Err(StoreError::InvalidBatch("memory.limit_reached"))
    );
    let waiting: MemoryId = format!("mem_3{:025}", 0).parse().unwrap();
    assert_eq!(
        m.patch_memory(&s, &waiting, 1, MemoryPatch::Accept, "human:usr_1")
            .await,
        Err(StoreError::InvalidBatch("memory.limit_reached"))
    );
    assert_eq!(
        m.get_memory(&s, &waiting).await?.map(|e| e.status),
        Some(MemoryStatus::Proposed)
    );
    // Project entries count separately; another tenant is unaffected.
    usecases::create(
        m,
        &s,
        mem(2),
        MemoryScope::Project,
        &save("Project fact", "body", &[], None),
        None,
        MemorySource::Person { user_id: author() },
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    )
    .await
    .map_err(actor_store)?;
    let theirs = scope(2);
    m.create_memory(&theirs, entry(&theirs, 3, now, None), "human:usr_2", 1)
        .await?;
    // Archiving frees a slot.
    let first: MemoryId = format!("mem_1{:025}", 0).parse().unwrap();
    m.patch_memory(&s, &first, 1, MemoryPatch::Archive, "human:usr_1")
        .await?;
    usecases::create(
        m,
        &s,
        mem(1),
        runbook_scope(),
        &save("One more", "body", &[], None),
        None,
        MemorySource::Person { user_id: author() },
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    )
    .await
    .map_err(actor_store)?;
    // Accepting needs a free active slot; archiving one frees it.
    let second: MemoryId = format!("mem_1{:025}", 1).parse().unwrap();
    m.patch_memory(&s, &second, 1, MemoryPatch::Archive, "human:usr_1")
        .await?;
    let accepted = m
        .patch_memory(&s, &waiting, 1, MemoryPatch::Accept, "human:usr_1")
        .await?;
    assert_eq!(accepted.status, MemoryStatus::Active);
    Ok(())
}

/// Accept, edit, reject and archive move status only along legal edges and
/// keep every prior revision; a foreign or unknown entry is NotFound.
pub async fn patches_follow_transitions_and_keep_history<M: MemoryStore>(
    m: &M,
) -> Result<(), StoreError> {
    let s = scope(1);
    let now = m.memory_time_ms().await?;
    let mut proposed = entry(&s, 1, now, None);
    proposed.status = MemoryStatus::Proposed;
    proposed.source = MemorySource::StepRun {
        execution_id: "exe_00000000000000000000000001".parse().unwrap(),
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        actor: ActorRef::System,
    };
    let exe: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
    assert!(
        m.apply_memory_proposal(
            &s,
            MemoryProposal {
                execution_id: exe.clone(),
                sequence: 4,
                entry: proposed.clone(),
            },
            500,
        )
        .await?
    );
    // Replays and older sequences of the same execution write nothing.
    for sequence in [4, 3] {
        let mut again = proposed.clone();
        again.id = mem(9);
        assert!(
            !m.apply_memory_proposal(
                &s,
                MemoryProposal {
                    execution_id: exe.clone(),
                    sequence,
                    entry: again,
                },
                500,
            )
            .await?
        );
    }
    assert_eq!(m.get_memory(&s, &mem(9)).await?, None);
    let actor = "human:usr_1";
    let illegal = |status| Err::<MemoryEntryRecord, _>(StoreError::InvalidBatch(status));
    assert_eq!(
        m.patch_memory(&s, &mem(1), 1, MemoryPatch::Accept, actor)
            .await?
            .status,
        MemoryStatus::Active
    );
    assert_eq!(
        m.patch_memory(&s, &mem(1), 2, MemoryPatch::Reject, actor)
            .await,
        illegal("memory.invalid_transition")
    );
    let edited = m
        .patch_memory(&s, &mem(1), 2, edit("Edited"), actor)
        .await?;
    assert_eq!((edited.revision, edited.status), (3, MemoryStatus::Active));
    assert_eq!(edited.body.as_ref(), "updated");
    let archived = m
        .patch_memory(&s, &mem(1), 3, MemoryPatch::Archive, actor)
        .await?;
    assert_eq!(archived.status, MemoryStatus::Archived);
    for patch in [MemoryPatch::Accept, MemoryPatch::Archive, edit("Again")] {
        assert_eq!(
            m.patch_memory(&s, &mem(1), 4, patch, actor).await,
            illegal("memory.invalid_transition")
        );
    }
    let history = m.memory_revisions(&s, &mem(1)).await?;
    assert_eq!(
        history
            .iter()
            .map(|r| (r.action, r.entry.revision, r.entry.status))
            .collect::<Vec<_>>(),
        [
            (MemoryAction::Propose, 1, MemoryStatus::Proposed),
            (MemoryAction::Accept, 2, MemoryStatus::Active),
            (MemoryAction::Edit, 3, MemoryStatus::Active),
            (MemoryAction::Archive, 4, MemoryStatus::Archived),
        ]
    );
    assert_eq!(history[0].entry.title, "Fact 1");
    assert_eq!(history[3].actor, actor);
    // A rejected proposal stays as archived history.
    let mut second = proposed.clone();
    second.id = mem(2);
    assert!(
        m.apply_memory_proposal(
            &s,
            MemoryProposal {
                execution_id: exe,
                sequence: 5,
                entry: second,
            },
            500,
        )
        .await?
    );
    let rejected = m
        .patch_memory(&s, &mem(2), 1, MemoryPatch::Reject, actor)
        .await?;
    assert_eq!(rejected.status, MemoryStatus::Archived);
    assert_eq!(
        m.memory_revisions(&s, &mem(2)).await?[1].action,
        MemoryAction::Reject
    );
    assert_eq!(
        m.patch_memory(&s, &mem(7), 1, MemoryPatch::Archive, actor)
            .await,
        Err(StoreError::NotFound)
    );
    Ok(())
}

/// Text (every word), tags (all), step ID and status narrow a search; pages
/// are newest first and a cursor is bound to its filters.
pub async fn search_filters_and_pages<M: MemoryStore>(m: &M) -> Result<(), StoreError> {
    let s = scope(1);
    let actor = "human:usr_1";
    for (n, title, body, tags, step) in [
        (
            1,
            "VPN",
            "eu-west needs the VPN dashboard",
            vec!["vpn", "eu"],
            Some("regions.error_rate"),
        ),
        (
            2,
            "Baseline",
            "Worker error rate sits near 0.5%",
            vec!["baseline"],
            Some("regions.error_rate"),
        ),
        (
            3,
            "False alarm",
            "The disk alert at 03:00 is a backup",
            vec!["alerts"],
            None,
        ),
        (
            4,
            "Dashboards",
            "Grafana dashboards live in the ops folder",
            vec!["vpn"],
            None,
        ),
    ] {
        usecases::create(
            m,
            &s,
            mem(n),
            runbook_scope(),
            &save(title, body, &tags, step),
            None,
            MemorySource::Person { user_id: author() },
            actor,
            runbook_core::memory_entries::MemoryWriter::Person,
        )
        .await
        .map_err(actor_store)?;
    }
    m.patch_memory(&s, &mem(3), 1, MemoryPatch::Archive, actor)
        .await?;
    let ids = |page: &MemoryRecordPage| page.items.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
    let all = MemoryQuery {
        within: vec![runbook_scope()],
        ..Default::default()
    };
    let s = &s;
    let search = |query: MemoryQuery| async move { m.search_memory(s, &query, None, 50).await };
    // Newest first: update time, then ID, both descending.
    let found = search(all.clone()).await?;
    assert_eq!(found.items.len(), 4);
    assert!(
        found
            .items
            .windows(2)
            .all(|w| { usecases::newest_first(&w[0], &w[1]) == std::cmp::Ordering::Less })
    );
    let text = |t: &str| MemoryQuery {
        text: Some(t.into()),
        ..all.clone()
    };
    assert_eq!(ids(&search(text("VPN dashboard")).await?), [mem(1)]);
    assert_eq!(ids(&search(text("eu-west")).await?), [mem(1)]);
    assert!(search(text("vpn sydney")).await?.items.is_empty());
    let mut dashboards = ids(&search(text("dashboards")).await?);
    dashboards.sort();
    assert_eq!(dashboards, [mem(4)]);
    let tagged = MemoryQuery {
        tags: vec!["vpn".into(), "eu".into()],
        ..all.clone()
    };
    assert_eq!(ids(&search(tagged).await?), [mem(1)]);
    let step = MemoryQuery {
        step_id: Some("regions.error_rate".into()),
        ..all.clone()
    };
    let mut by_step = ids(&search(step).await?);
    by_step.sort();
    assert_eq!(by_step, [mem(1), mem(2)]);
    let archived = MemoryQuery {
        status: Some(MemoryStatus::Archived),
        ..all.clone()
    };
    assert_eq!(ids(&search(archived).await?), [mem(3)]);
    let project_only = MemoryQuery {
        within: vec![MemoryScope::Project],
        ..Default::default()
    };
    assert!(search(project_only).await?.items.is_empty());
    let everywhere = MemoryQuery::default();
    assert_eq!(search(everywhere).await?.items.len(), 4);
    // Pages of two, then the rest; a cursor is bound to its filters.
    let first = m.search_memory(s, &all, None, 2).await?;
    let cursor = first.next_cursor.clone().expect("more");
    let second = m.search_memory(s, &all, Some(cursor.clone()), 2).await?;
    assert_eq!(second.next_cursor, None);
    let mut paged = ids(&first);
    paged.extend(ids(&second));
    assert_eq!(paged, ids(&found));
    assert_eq!(
        m.search_memory(s, &text("vpn"), Some(cursor.clone()), 2)
            .await,
        Err(StoreError::InvalidBatch("request.invalid_cursor"))
    );
    assert_eq!(
        m.search_memory(&scope(2), &all, Some(cursor), 2).await,
        Err(StoreError::InvalidBatch("request.invalid_cursor"))
    );
    Ok(())
}

fn proposed_entry(scope: &TenantScope, n: u8, exe: &ExecutionId, sr: &str) -> MemoryEntryRecord {
    let mut e = entry(scope, n, 10, None);
    e.status = MemoryStatus::Proposed;
    e.source = MemorySource::StepRun {
        execution_id: exe.clone(),
        step_run_id: sr.parse().unwrap(),
        actor: ActorRef::System,
    };
    e
}

/// D40: a proposal rides the execution commit's derived row and is applied
/// in the same transaction, once by (execution, sequence). When the
/// runbook's quota is full the whole execution commit fails: no event, no
/// entry. A derived row that does not match a `MemoryProposed` event of its
/// batch is refused.
pub async fn proposals_commit_with_the_execution<F>(f: &F) -> Result<(), StoreError>
where
    F: crate::CatalogFactory<Store: MemoryStore>,
{
    use crate::catalog::{batch, id};
    use runbook_core::ExecutionStore;
    use runbook_engine::Event;
    use runbook_model::ExecutionStatus;
    let s = f.create(crate::execution::projection()).await?;
    let sc = scope(1);
    let sr = "sr_00000000000000000000000001";
    let propose = |from: u64, n: u8| {
        let mut commit = batch(1, from, from + 1, ExecutionStatus::Running);
        commit.events[0].event = Event::MemoryProposed {
            step_run_id: sr.parse().unwrap(),
            memory_id: mem(n),
        };
        commit.derived_outbox[0].memory = vec![MemoryProposal {
            execution_id: id(1),
            sequence: from + 1,
            entry: proposed_entry(&sc, n, &id(1), sr),
        }];
        commit
    };
    s.commit(propose(0, 1)).await?;
    f.await_derived(&s, &id(1), 1, f.derived_row_delay())
        .await?;
    let stored = s.get_memory(&sc, &mem(1)).await?.expect("applied");
    assert_eq!(stored.status, MemoryStatus::Proposed);
    assert_eq!(stored.revision, 1);
    assert_eq!(
        s.memory_revisions(&sc, &mem(1)).await?[0].action,
        MemoryAction::Propose
    );
    // A redelivered row writes nothing new.
    f.redeliver_derived(&s, &id(1), 1).await?;
    assert_eq!(s.memory_revisions(&sc, &mem(1)).await?.len(), 1);
    let replay = MemoryProposal {
        execution_id: id(1),
        sequence: 1,
        entry: proposed_entry(&sc, 1, &id(1), sr),
    };
    assert!(!s.apply_memory_proposal(&sc, replay, 500).await?);
    // A row that names no MemoryProposed event of its batch is refused.
    let mut forged = propose(1, 2);
    forged.events[0].event = Event::ManualPauseChanged { paused: false };
    assert_eq!(
        s.commit(forged).await,
        Err(StoreError::InvalidBatch("store.invalid_derived_update"))
    );
    // Fill the runbook's proposal quota: the next proposal fails the whole
    // commit.
    let now = s.memory_time_ms().await?;
    for n in 0..usecases::MEMORY_PROPOSALS_PER_RUNBOOK - 1 {
        let e = pending(&sc, format!("mem_2{n:025}").parse().unwrap(), now);
        s.create_memory(&sc, e, "service:tok_1", MEMORY_PROPOSALS_PER_RUNBOOK)
            .await?;
    }
    assert_eq!(
        s.commit(propose(1, 3)).await,
        Err(StoreError::InvalidBatch("memory.limit_reached"))
    );
    assert_eq!(s.load(&id(1)).await?.revision, 1);
    assert_eq!(s.get_memory(&sc, &mem(3)).await?, None);
    Ok(())
}

/// Two runs racing for the last proposal slot of a runbook. Where the
/// catalog applies proposals inside the execution commit (memory,
/// Postgres), the second commit fails with `memory.limit_reached`. Where it
/// applies them after the run's own commit (Cloudflare: the ExecutionDO
/// checks the quota first, D1 applies later), both commits can pass the
/// check, and the loser is kept as an archived drop marker with the reason
/// code, never silently lost (the composed harness takes this branch).
pub async fn proposals_racing_a_full_quota_are_never_lost<F>(f: &F) -> Result<(), StoreError>
where
    F: crate::CatalogFactory<Store: MemoryStore>,
{
    use crate::catalog::{batch, id};
    use runbook_core::ExecutionStore;
    use runbook_engine::Event;
    use runbook_model::ExecutionStatus;
    let s = f.create(crate::execution::projection()).await?;
    let sc = scope(1);
    let sr = "sr_00000000000000000000000001";
    let now = s.memory_time_ms().await?;
    for n in 0..MEMORY_PROPOSALS_PER_RUNBOOK - 1 {
        let e = pending(&sc, format!("mem_2{n:025}").parse().unwrap(), now);
        s.create_memory(&sc, e, "service:tok_1", MEMORY_PROPOSALS_PER_RUNBOOK)
            .await?;
    }
    let propose = |run: u8, n: u8| {
        let mut commit = batch(run, 0, 1, ExecutionStatus::Running);
        commit.events[0].event = Event::MemoryProposed {
            step_run_id: sr.parse().unwrap(),
            memory_id: mem(n),
        };
        commit.derived_outbox[0].memory = vec![MemoryProposal {
            execution_id: id(run),
            sequence: 1,
            entry: proposed_entry(&sc, n, &id(run), sr),
        }];
        commit
    };
    s.commit(propose(1, 1)).await?;
    let second = s.commit(propose(2, 2)).await;
    f.await_derived(&s, &id(1), 1, f.derived_row_delay())
        .await?;
    let won = s
        .get_memory(&sc, &mem(1))
        .await?
        .expect("the first is stored");
    assert_eq!((won.status, won.dropped), (MemoryStatus::Proposed, None));
    // A host that applies each commit's row before the next request (the
    // memory and Postgres adapters, a settling Cloudflare host) refuses the
    // second commit outright.
    if second.is_err() || f.derived_row_delay().is_zero() {
        assert_eq!(
            second,
            Err(StoreError::InvalidBatch("memory.limit_reached"))
        );
        assert_eq!(s.get_memory(&sc, &mem(2)).await?, None);
        return Ok(());
    }
    f.await_derived(&s, &id(2), 1, f.derived_row_delay())
        .await?;
    let lost = s.get_memory(&sc, &mem(2)).await?.expect("a drop marker");
    assert_eq!(lost.status, MemoryStatus::Archived);
    assert_eq!(lost.dropped.as_deref(), Some("memory.limit_reached"));
    let history = s.memory_revisions(&sc, &mem(2)).await?;
    assert_eq!(
        history.iter().map(|r| r.action).collect::<Vec<_>>(),
        [MemoryAction::Drop]
    );
    // The run's document says so on the step's memory line.
    let mut sections: Vec<runbook_model::StepSection> =
        serde_json::from_value(serde_json::json!([{
            "step_run_id": sr, "path": "check", "title": "Check", "status": "claimed",
            "verdict": null, "actor": null, "summary": null, "outputs": {}, "evidence": [],
            "approval": null, "memory_read": [], "amended": false, "amendments": [],
            "stale": false, "logs": {"href": "/logs", "next_cursor": null},
            "memory_proposed": [{"memory_id": mem(1)}, {"memory_id": mem(2)}],
        }]))
        .unwrap();
    runbook_core::execution_document::mark_dropped_proposals(&s, &sc, &mut sections).await?;
    assert_eq!(
        sections[0]
            .memory_proposed
            .iter()
            .map(|p| p.dropped.as_deref())
            .collect::<Vec<_>>(),
        [None, Some("memory.limit_reached")]
    );
    // A redelivery writes nothing more, and the quota is unchanged.
    f.redeliver_derived(&s, &id(2), 1).await?;
    assert_eq!(s.memory_revisions(&sc, &mem(2)).await?.len(), 1);
    let pending = s
        .search_memory(
            &sc,
            &MemoryQuery {
                status: Some(MemoryStatus::Proposed),
                ..Default::default()
            },
            None,
            100,
        )
        .await?;
    assert_eq!(pending.items.len(), MEMORY_PROPOSALS_PER_RUNBOOK);
    Ok(())
}

fn saved_entry(scope: &TenantScope, n: u8, exe: &ExecutionId, sr: &str) -> MemoryEntryRecord {
    let mut e = proposed_entry(scope, n, exe, sr);
    e.status = MemoryStatus::Active;
    e
}

/// MEM6: a run's live save rides the execution commit's derived row like a
/// proposal (D40) and is stored active at once, recorded as a `create` by
/// the step's claim owner. It is applied once by (execution, sequence). A
/// row whose status does not match its event (`MemorySaved` for an active
/// entry, `MemoryProposed` for a proposal) is refused, and a full active
/// quota fails the whole commit on every backend.
pub async fn runs_save_live_entries<F>(f: &F) -> Result<(), StoreError>
where
    F: crate::CatalogFactory<Store: MemoryStore>,
{
    use crate::catalog::{batch, id};
    use runbook_core::ExecutionStore;
    use runbook_engine::Event;
    use runbook_model::ExecutionStatus;
    let s = f.create(crate::execution::projection()).await?;
    let sc = scope(1);
    let sr = "sr_00000000000000000000000001";
    let save = |from: u64, n: u8, live: bool| {
        let mut commit = batch(1, from, from + 1, ExecutionStatus::Running);
        let (step_run_id, memory_id) = (sr.parse().unwrap(), mem(n));
        commit.events[0].event = if live {
            Event::MemorySaved {
                step_run_id,
                memory_id,
            }
        } else {
            Event::MemoryProposed {
                step_run_id,
                memory_id,
            }
        };
        commit.derived_outbox[0].memory = vec![MemoryProposal {
            execution_id: id(1),
            sequence: from + 1,
            entry: saved_entry(&sc, n, &id(1), sr),
        }];
        commit
    };
    s.commit(save(0, 1, true)).await?;
    f.await_derived(&s, &id(1), 1, f.derived_row_delay())
        .await?;
    let stored = s.get_memory(&sc, &mem(1)).await?.expect("applied");
    assert_eq!((stored.status, stored.revision), (MemoryStatus::Active, 1));
    let history = s.memory_revisions(&sc, &mem(1)).await?;
    assert_eq!(
        history
            .iter()
            .map(|r| (r.action, r.actor.as_str()))
            .collect::<Vec<_>>(),
        [(MemoryAction::Create, "system")]
    );
    // Live at once: the step reads it without review.
    let read = usecases::read_for_step(&s, &sc, &rb(), "regions.error_rate").await?;
    assert_eq!(read.iter().map(|e| &e.id).collect::<Vec<_>>(), [&mem(1)]);
    // A redelivered row writes nothing new.
    f.redeliver_derived(&s, &id(1), 1).await?;
    assert_eq!(s.memory_revisions(&sc, &mem(1)).await?.len(), 1);
    // An active entry under a MemoryProposed event, or a proposal under a
    // MemorySaved event, is refused.
    let mismatched = save(1, 2, false);
    assert_eq!(
        s.commit(mismatched).await,
        Err(StoreError::InvalidBatch("store.invalid_derived_update"))
    );
    let mut mismatched = save(1, 2, true);
    mismatched.derived_outbox[0].memory[0].entry.status = MemoryStatus::Proposed;
    assert_eq!(
        s.commit(mismatched).await,
        Err(StoreError::InvalidBatch("store.invalid_derived_update"))
    );
    // Live saves count against an agent's active quota (review fix 6: the
    // last 50 of the 500 slots are people's), not the proposal quota:
    // with one agent slot left (far past the 100 proposals), a save is
    // stored; then the next fails the whole commit (no event, no entry),
    // while a person can still save.
    let now = s.memory_time_ms().await?;
    let agents = usecases::agent_quota(MemoryStatus::Active);
    assert_eq!(agents, MEMORY_ENTRIES_PER_RUNBOOK - 50);
    for n in 0..agents - 2 {
        let e = entry(&sc, 0, now, None);
        let e = MemoryEntryRecord {
            id: format!("mem_3{n:025}").parse().unwrap(),
            ..e
        };
        s.create_memory(&sc, e, "human:usr_1", MEMORY_ENTRIES_PER_RUNBOOK)
            .await?;
    }
    s.commit(save(1, 3, true)).await?;
    f.await_derived(&s, &id(1), 2, f.derived_row_delay())
        .await?;
    assert_eq!(
        s.get_memory(&sc, &mem(3)).await?.map(|e| e.status),
        Some(MemoryStatus::Active)
    );
    assert_eq!(
        s.commit(save(2, 4, true)).await,
        Err(StoreError::InvalidBatch("memory.limit_reached"))
    );
    assert_eq!(s.load(&id(1)).await?.revision, 2);
    assert_eq!(s.get_memory(&sc, &mem(4)).await?, None);
    let person = MemoryEntryRecord {
        id: mem(5),
        ..entry(&sc, 0, now, None)
    };
    s.create_memory(&sc, person, "human:usr_1", MEMORY_ENTRIES_PER_RUNBOOK)
        .await?;
    Ok(())
}

/// MEM6 through the core use cases: an agent holding a claim saves a live
/// entry (the step reads it at once) or, when it asks, proposes one; both
/// count toward the 20 writes of one execution. An agent edits a live
/// entry fenced by its revision, and each revision names who made it; it
/// never accepts, rejects or archives, and a proposal or archived entry is
/// absent to it. A person archives, and reverts an agent's edit by editing
/// the prior revision's content back.
pub async fn runs_save_live_memory_and_agents_only_edit<S, D>(store: &S, dir: &D)
where
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
    use runbook_core::{
        actor::begin_actor,
        claims::claim_step,
        memory_entries::{MemoryWriter, actor_label},
    };
    use runbook_model::{ClaimRequest, MemoryPatchRequest};
    directory(dir).await;
    crate::parity::publish(store, 2, "plain", PLAIN, 2).await;
    start(store, dir, &p(), 1, 2).await;
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let only = work
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
        &only,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    let scope_rb = MemoryScope::Runbook {
        runbook_id: runbook(2),
    };
    let live = save("Baseline", "Normal error rate is 0.5%", &[], Some("only"));
    let mut proposal = save("Maybe", "The dashboard moved", &[], Some("only"));
    proposal.propose = true;
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let saved = usecases::save_in_run(work, &agent(), mem(1), scope_rb.clone(), &live, None)
        .await
        .unwrap();
    assert_eq!(saved.status, MemoryStatus::Active);
    assert!(matches!(&saved.source,
        MemorySource::StepRun { execution_id, step_run_id, .. }
            if execution_id == &exe(1) && step_run_id == &only));
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let proposed = usecases::save_in_run(work, &agent(), mem(2), scope_rb.clone(), &proposal, None)
        .await
        .unwrap();
    assert_eq!(proposed.status, MemoryStatus::Proposed);
    // The step reads the live entry at once, never the proposal.
    let read = usecases::read_for_step(store, &p(), &runbook(2), "only")
        .await
        .unwrap();
    assert_eq!(read.iter().map(|e| &e.id).collect::<Vec<_>>(), [&mem(1)]);
    // Saves and proposals share the per-execution cap.
    for n in 2..usecases::MEMORY_PROPOSALS_PER_EXECUTION as u8 {
        let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
        usecases::save_in_run(work, &agent(), mem(n + 1), scope_rb.clone(), &live, None)
            .await
            .unwrap();
    }
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let refused =
        usecases::save_in_run(work, &agent(), mem(30), scope_rb.clone(), &live, None).await;
    assert_eq!(refused.unwrap_err().code(), "memory.proposal_limit");
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let step = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.id.as_ref() == Some(&only))
        .unwrap();
    assert_eq!(step.memory_proposed, [mem(2)]);
    assert_eq!(step.memory_saved.len(), 19);
    assert_eq!(step.memory_saved[0], mem(1));
    drop(work);

    // An agent edits the live entry, fenced by its revision.
    let (writer, label) = (MemoryWriter::Agent, "agent:tok_00000000000000000000000009");
    let rewrite = MemoryPatchRequest::Edit {
        entry: save("Baseline", "Normal error rate is 0.7%", &[], Some("only")),
    };
    let edited = usecases::patch(store, &p(), &mem(1), 1, &rewrite, None, label, writer)
        .await
        .unwrap();
    assert_eq!(
        (edited.revision, edited.body.as_ref()),
        (2, "Normal error rate is 0.7%")
    );
    let stale = usecases::patch(store, &p(), &mem(1), 1, &rewrite, None, label, writer).await;
    assert_eq!(stale.unwrap_err().code(), "memory.conflict");
    // Never a review or an archive. A proposal is absent to an agent, so
    // reviewing it is 404 before anything else (review fix 7).
    for (id, revision, request, code) in [
        (mem(2), 1, MemoryPatchRequest::Accept, "resource.not_found"),
        (mem(2), 1, MemoryPatchRequest::Reject, "resource.not_found"),
        (
            mem(1),
            2,
            MemoryPatchRequest::Archive,
            "memory.person_required",
        ),
        (
            mem(1),
            2,
            MemoryPatchRequest::Lock,
            "memory.person_required",
        ),
        (
            mem(1),
            2,
            MemoryPatchRequest::Restore { revision: 1 },
            "memory.person_required",
        ),
    ] {
        let refused =
            usecases::patch(store, &p(), &id, revision, &request, None, label, writer).await;
        assert_eq!(refused.unwrap_err().code(), code);
    }
    let hidden = usecases::patch(store, &p(), &mem(2), 1, &rewrite, None, label, writer).await;
    assert_eq!(hidden.unwrap_err().code(), "resource.not_found");
    // A person reverts the agent's edit to revision 1's content.
    let person = (MemoryWriter::Person, "human:usr_00000000000000000000000001");
    let history = store.memory_revisions(&p(), &mem(1)).await.unwrap();
    let first = &history[0].entry;
    let revert = MemoryPatchRequest::Edit {
        entry: SaveMemoryRequest {
            title: first.title.clone(),
            body_markdown: first.body.clone(),
            tags: first.tags.clone(),
            step_id: first.step_id.clone(),
            expires_at: None,
            propose: false,
        },
    };
    let reverted = usecases::patch(store, &p(), &mem(1), 2, &revert, None, person.1, person.0)
        .await
        .unwrap();
    assert_eq!(
        (reverted.revision, reverted.body.as_ref()),
        (3, "Normal error rate is 0.5%")
    );
    let history = store.memory_revisions(&p(), &mem(1)).await.unwrap();
    assert_eq!(
        history
            .iter()
            .map(|r| (r.action, r.actor.clone()))
            .collect::<Vec<_>>(),
        [
            (MemoryAction::Create, actor_label(&agent().actor)),
            (MemoryAction::Edit, label.to_owned()),
            (MemoryAction::Edit, person.1.to_owned()),
        ]
    );
    // A person archives; the archived entry is then absent to an agent.
    usecases::patch(
        store,
        &p(),
        &mem(1),
        3,
        &MemoryPatchRequest::Archive,
        None,
        person.1,
        person.0,
    )
    .await
    .unwrap();
    let gone = usecases::patch(store, &p(), &mem(1), 4, &rewrite, None, label, writer).await;
    assert_eq!(gone.unwrap_err().code(), "resource.not_found");
}

/// The full D40/D41 flow through the core use cases: an agent holding a
/// claim proposes; the 21st proposal of one execution is refused without
/// writing anything; a principal without a claim cannot propose; an
/// accepted entry is read for its step (exact path or step key), and the
/// read is recorded only when its refs change.
pub async fn proposals_are_capped_and_reads_audited<S, D>(store: &S, dir: &D)
where
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
    use crate::portable::{PLAIN, agent, directory, exe, human, p, rb as runbook, start};
    use runbook_core::{actor::begin_actor, claims::claim_step};
    use runbook_model::{ClaimRequest, MemoryPatchRequest};
    directory(dir).await;
    crate::parity::publish(store, 2, "plain", PLAIN, 2).await;
    start(store, dir, &p(), 1, 2).await;
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let only = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "only")
        .and_then(|s| s.id.clone())
        .unwrap();
    // No claim yet: nothing to attach a proposal to.
    let mut request = save(
        "Baseline",
        "Normal error rate is 0.5%",
        &["baseline"],
        Some("only"),
    );
    request.propose = true;
    let scope_rb = MemoryScope::Runbook {
        runbook_id: runbook(2),
    };
    let refused =
        usecases::save_in_run(work, &agent(), mem(50), scope_rb.clone(), &request, None).await;
    assert_eq!(refused.unwrap_err().code(), "memory.step_not_claimed");
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    claim_step(
        work,
        &agent(),
        &only,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    // Another runbook's scope is refused.
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let foreign = MemoryScope::Runbook {
        runbook_id: runbook(1),
    };
    let refused = usecases::save_in_run(work, &agent(), mem(51), foreign, &request, None).await;
    assert_eq!(refused.unwrap_err().code(), "auth.forbidden");
    // A person without the claim cannot propose for it either.
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let refused =
        usecases::save_in_run(work, &human(1), mem(52), scope_rb.clone(), &request, None).await;
    assert_eq!(refused.unwrap_err().code(), "memory.step_not_claimed");
    for n in 0..usecases::MEMORY_PROPOSALS_PER_EXECUTION as u8 {
        let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
        let proposed =
            usecases::save_in_run(work, &agent(), mem(n + 1), scope_rb.clone(), &request, None)
                .await
                .unwrap();
        assert_eq!(proposed.status, MemoryStatus::Proposed);
        assert!(matches!(&proposed.source,
            MemorySource::StepRun { execution_id, step_run_id, .. }
                if execution_id == &exe(1) && step_run_id == &only));
    }
    let before = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let revision = before.snapshot.revision;
    let refused =
        usecases::save_in_run(before, &agent(), mem(21), scope_rb.clone(), &request, None).await;
    assert_eq!(refused.unwrap_err().code(), "memory.proposal_limit");
    // Sessions hold the execution lock on PostgreSQL: read and release.
    let after = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    assert_eq!(after.snapshot.revision, revision);
    drop(after);
    assert_eq!(store.get_memory(&p(), &mem(21)).await.unwrap(), None);
    let proposed = MemoryQuery {
        status: Some(MemoryStatus::Proposed),
        ..Default::default()
    };
    let page = store
        .search_memory(&p(), &proposed, None, 50)
        .await
        .unwrap();
    assert_eq!(page.items.len(), usecases::MEMORY_PROPOSALS_PER_EXECUTION);
    // Proposals are invisible to the step until accepted.
    assert!(
        usecases::read_for_step(store, &p(), &runbook(2), "only")
            .await
            .unwrap()
            .is_empty()
    );
    usecases::patch(
        store,
        &p(),
        &mem(1),
        1,
        &MemoryPatchRequest::Accept,
        None,
        "human:usr_1",
        runbook_core::memory_entries::MemoryWriter::Person,
    )
    .await
    .unwrap();
    let read = usecases::read_for_step(store, &p(), &runbook(2), "only")
        .await
        .unwrap();
    assert_eq!(
        read.iter().map(|e| (&e.id, e.revision)).collect::<Vec<_>>(),
        [(&mem(1), 2)]
    );
    let refs = usecases::read_refs(&read);
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    assert!(
        usecases::record_step_read(work, &only, refs.clone(), agent().actor)
            .await
            .unwrap()
    );
    // Polling the same read adds nothing.
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let revision = work.snapshot.revision;
    assert!(
        !usecases::record_step_read(work, &only, refs.clone(), agent().actor)
            .await
            .unwrap()
    );
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    assert_eq!(work.snapshot.revision, revision);
    let step = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.id.as_ref() == Some(&only))
        .unwrap();
    assert_eq!(step.memory_read, refs);
    assert_eq!(
        step.memory_proposed.len(),
        usecases::MEMORY_PROPOSALS_PER_EXECUTION
    );
    // A step key entry applies to every iteration path of that step.
    assert_eq!(
        usecases::step_key("regions[eu-west].error_rate"),
        "regions.error_rate"
    );
    assert_eq!(usecases::step_key("a[\"x]y\"].b[2].c"), "a.b.c");
}

const AGENT: &str = "agent:tok_00000000000000000000000009:usr_00000000000000000000000001";
const PERSON: &str = "human:usr_00000000000000000000000001";

fn revision_numbers(page: &[runbook_core::memory_entries::MemoryRevisionRecord]) -> Vec<u32> {
    page.iter().map(|r| r.entry.revision).collect()
}

/// A person's patch of `id` through the use case.
async fn by_person_patch<M: MemoryStore>(
    m: &M,
    s: &TenantScope,
    id: MemoryId,
    revision: u32,
    request: runbook_model::MemoryPatchRequest,
) -> Result<MemoryEntryRecord, runbook_core::actor::ActorError> {
    usecases::patch(
        m,
        s,
        &id,
        revision,
        &request,
        Some(&[]),
        PERSON,
        usecases::MemoryWriter::Person,
    )
    .await
}

/// A save through the use case by an agent-kind token or a person.
async fn save_as<M: MemoryStore>(
    m: &M,
    s: &TenantScope,
    id: MemoryId,
    memory_scope: MemoryScope,
    request: &SaveMemoryRequest,
    writer: usecases::MemoryWriter,
) -> Result<MemoryEntryRecord, runbook_core::actor::ActorError> {
    let (source, actor) = match writer {
        usecases::MemoryWriter::Person => (MemorySource::Person { user_id: author() }, PERSON),
        usecases::MemoryWriter::Agent => (
            MemorySource::Agent {
                token_id: "tok_00000000000000000000000009".parse().unwrap(),
                sponsor_user_id: author(),
            },
            AGENT,
        ),
    };
    usecases::create(m, s, id, memory_scope, request, None, source, actor, writer).await
}

/// MEM6 review fixes 1, 2, 3 and 6 on one store:
/// - every write names its writer on the entry (`last_writer`);
/// - an entry keeps its first revision and the newest 50, records how many
///   it pruned, and reads one revision by key or a page of them;
/// - a person locks it, and restores a prior revision (unarchiving it,
///   without the step check), but never one that was pruned;
/// - agents leave the last 50 active slots to people.
pub async fn history_is_bounded_lockable_and_restorable<M: MemoryStore>(
    m: &M,
) -> Result<(), StoreError> {
    use runbook_core::memory_entries::{MEMORY_REVISIONS_KEPT, MemoryWriter, agent_quota};
    use runbook_model::MemoryPatchRequest;
    let s = scope(1);
    let now = m.memory_time_ms().await?;
    let created = m
        .create_memory(&s, entry(&s, 1, now, None), AGENT, 500)
        .await?;
    assert_eq!(created.last_writer.as_deref(), Some(AGENT));
    for n in 1..=60u32 {
        // Agents edit an entry at most 20 times a day; `system` writes
        // stand in for the rest.
        let writer = if n % 2 == 0 { PERSON } else { "system" };
        let next = m
            .patch_memory(&s, &mem(1), n, edit(&format!("Edit {n}")), writer)
            .await?;
        assert_eq!(
            (next.revision, next.last_writer.as_deref()),
            (n + 1, Some(writer))
        );
    }
    let current = m.get_memory(&s, &mem(1)).await?.unwrap();
    assert_eq!((current.revision, current.revisions_pruned), (61, 10));
    let kept = m.memory_revisions(&s, &mem(1)).await?;
    assert_eq!(kept.len(), 1 + MEMORY_REVISIONS_KEPT as usize);
    assert_eq!(revision_numbers(&kept[..3]), [1, 12, 13]);
    assert_eq!(kept[0].actor, AGENT);
    assert_eq!(m.memory_revision(&s, &mem(1), 5).await?, None);
    assert_eq!(
        m.memory_revision(&s, &mem(1), 1)
            .await?
            .map(|r| r.entry.title),
        Some("Fact 1".to_owned())
    );
    assert_eq!(
        m.memory_revision(&s, &mem(1), 61).await?.map(|r| r.actor),
        Some(PERSON.to_owned())
    );
    let page = m.memory_revision_page(&s, &mem(1), 0, 3).await?;
    assert_eq!(revision_numbers(&page), [1, 12, 13]);
    let rest = m.memory_revision_page(&s, &mem(1), 13, 100).await?;
    assert_eq!(revision_numbers(&rest), (14..=61).collect::<Vec<_>>());
    // Another tenant reads nothing.
    assert_eq!(m.memory_revision(&scope(2), &mem(1), 1).await?, None);
    assert!(
        m.memory_revision_page(&scope(2), &mem(1), 0, 10)
            .await?
            .is_empty()
    );
    // A person locks it; the lock survives an archive and a restore.
    let locked = m
        .patch_memory(&s, &mem(1), 61, MemoryPatch::Lock, PERSON)
        .await?;
    assert!(locked.locked);
    by_person_patch(m, &s, mem(1), 62, MemoryPatchRequest::Archive)
        .await
        .map_err(actor_store)?;
    // Restore unarchives with revision 1's content, though no step of the
    // runbook matches its step ID any more.
    let restored = by_person_patch(
        m,
        &s,
        mem(1),
        63,
        MemoryPatchRequest::Restore { revision: 1 },
    )
    .await
    .map_err(actor_store)?;
    assert_eq!(
        (
            restored.status,
            restored.revision,
            restored.title.as_str(),
            restored.step_id.as_deref(),
            restored.locked,
        ),
        (
            MemoryStatus::Active,
            64,
            "Fact 1",
            Some("regions.error_rate"),
            true
        )
    );
    assert_eq!(
        m.memory_revisions(&s, &mem(1))
            .await?
            .last()
            .map(|r| r.action),
        Some(MemoryAction::Restore)
    );
    let pruned = by_person_patch(
        m,
        &s,
        mem(1),
        64,
        MemoryPatchRequest::Restore { revision: 5 },
    )
    .await;
    assert_eq!(pruned.unwrap_err().code(), "memory.revision_not_found");
    let unlocked = m
        .patch_memory(&s, &mem(1), 64, MemoryPatch::Unlock, PERSON)
        .await?;
    assert!(!unlocked.locked);
    // A proposal cannot be restored (it is accepted or rejected).
    m.create_memory(&s, pending(&s, mem(2), now), "service:tok_1", 100)
        .await?;
    let refused = usecases::patch(
        m,
        &s,
        &mem(2),
        1,
        &MemoryPatchRequest::Restore { revision: 1 },
        None,
        PERSON,
        MemoryWriter::Person,
    )
    .await;
    assert_eq!(refused.unwrap_err().code(), "memory.invalid_transition");
    // Agents leave the last 50 active slots to people.
    let agents = agent_quota(MemoryStatus::Active);
    for n in 0..agents - 2 {
        let e = MemoryEntryRecord {
            id: format!("mem_4{n:025}").parse().unwrap(),
            ..entry(&s, 0, now, None)
        };
        m.create_memory(&s, e, PERSON, MEMORY_ENTRIES_PER_RUNBOOK)
            .await?;
    }
    let fact = save("Fact", "body", &[], None);
    save_as(m, &s, mem(10), runbook_scope(), &fact, MemoryWriter::Agent)
        .await
        .map_err(actor_store)?;
    let full = save_as(m, &s, mem(11), runbook_scope(), &fact, MemoryWriter::Agent).await;
    assert_eq!(full.unwrap_err().code(), "memory.limit_reached");
    save_as(m, &s, mem(12), runbook_scope(), &fact, MemoryWriter::Person)
        .await
        .map_err(actor_store)?;
    Ok(())
}

/// MEM6 review fix 3: an agent's or service's write outside a run commits a
/// webhook notice with it: `memory.saved`, `memory.proposed` or
/// `memory.edited`, identifiers only. A person's write notifies nobody, and
/// a run's edit notifies through its own event.
pub async fn agent_writes_commit_a_notice<F>(f: &F) -> Result<(), StoreError>
where
    F: crate::CatalogFactory<Store: MemoryStore + runbook_core::webhooks::ProjectOutbox>,
{
    use runbook_core::webhooks::ProjectOutbox;
    let s = f.create(crate::execution::projection()).await?;
    let sc = scope(1);
    let now = s.memory_time_ms().await?;
    s.create_memory(&sc, entry(&sc, 1, now, None), AGENT, 500)
        .await?;
    s.patch_memory(&sc, &mem(1), 1, edit("By the agent"), AGENT)
        .await?;
    s.patch_memory(&sc, &mem(1), 2, edit("By a person"), PERSON)
        .await?;
    let run = "execution:exe_00000000000000000000000001:sr_00000000000000000000000001";
    s.patch_memory(&sc, &mem(1), 3, edit("By a run"), run)
        .await?;
    s.create_memory(&sc, entry(&sc, 2, now, None), PERSON, 500)
        .await?;
    s.create_memory(&sc, pending(&sc, mem(3), now), "service:tok_1", 100)
        .await?;
    let events: Vec<_> = s
        .pending_project_events(100)
        .await?
        .into_iter()
        .filter(|e| e.event_type.starts_with("memory."))
        .collect();
    let mut seen: Vec<(String, String, u64)> = events
        .iter()
        .map(|e| {
            (
                e.event_type.clone(),
                e.data["memory_id"].as_str().unwrap_or_default().to_owned(),
                e.data["revision"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("memory.edited".to_owned(), mem(1).to_string(), 2),
            ("memory.proposed".to_owned(), mem(3).to_string(), 1),
            ("memory.saved".to_owned(), mem(1).to_string(), 1),
        ]
    );
    for event in &events {
        assert_eq!(event.runbook_id, Some(rb()));
        let data = event.data.to_string();
        assert!(!data.contains("VPN") && !data.contains("updated"), "{data}");
    }
    Ok(())
}

/// MEM6 review fixes 2, 4, 5, 6 and 7 through the core use cases:
/// - a run edits only under a live claim, its revision names the step run,
///   and the edit counts toward the run's 20 memory writes;
/// - a stale or locked edit records nothing in the run;
/// - an agent never clears or extends an expiry already set;
/// - an agent learns nothing of an entry it cannot see: every action on it
///   is 404, before any other answer;
/// - a retried save answers the entry as first created, or 404 once it is
///   archived;
/// - a step reads entries a person wrote last ahead of newer agent writes.
pub async fn runs_edit_under_a_claim_and_agents_learn_nothing_hidden<S, D>(store: &S, dir: &D)
where
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
    use runbook_core::{actor::begin_actor, claims::claim_step, memory_entries::MemoryWriter};
    use runbook_model::{ClaimRequest, MemoryPatchRequest, Timestamp};
    directory(dir).await;
    crate::parity::publish(store, 2, "plain", PLAIN, 2).await;
    start(store, dir, &p(), 1, 2).await;
    let scope_rb = MemoryScope::Runbook {
        runbook_id: runbook(2),
    };
    let mut fact = save("Baseline", "Normal error rate is 0.5%", &[], Some("only"));
    fact.expires_at = Timestamp::from_timestamp_millis(4_000_000_000_000);
    let created = save_as(
        store,
        &p(),
        mem(1),
        scope_rb.clone(),
        &fact,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    // An older entry a person wrote, for the step-order check below.
    let owner = save("Owner", "The SRE team owns it", &[], Some("only"));
    save_as(
        store,
        &p(),
        mem(5),
        scope_rb.clone(),
        &owner,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    let mut later = save("Baseline", "Normal error rate is 0.7%", &[], Some("only"));
    later.expires_at = Timestamp::from_timestamp_millis(5_000_000_000_000);
    let edit = MemoryPatchRequest::Edit { entry: later };
    // No claim: a run cannot edit.
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let refused = usecases::edit_in_run(work, &agent(), store, &p(), &mem(1), 1, &edit, None).await;
    assert_eq!(refused.unwrap_err().code(), "memory.step_not_claimed");
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let only = work
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
        &only,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        "clm_00000000000000000000000001".parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let edited = usecases::edit_in_run(work, &agent(), store, &p(), &mem(1), 1, &edit, None)
        .await
        .unwrap();
    assert_eq!(edited.revision, 2);
    assert_eq!(
        edited.last_writer,
        Some(format!("execution:{}:{only}", exe(1)))
    );
    // The creator stays the source; the person's expiry stays.
    assert!(matches!(edited.source, MemorySource::Person { .. }));
    assert_eq!(edited.expires_at_ms, created.expires_at_ms);
    // An agent's edit that leaves the expiry out keeps it too.
    let mut expiring = save("Window", "Use a 10 minute window", &[], Some("only"));
    expiring.expires_at = Timestamp::from_timestamp_millis(4_000_000_000_000);
    let window = save_as(
        store,
        &p(),
        mem(6),
        scope_rb.clone(),
        &expiring,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    let no_expiry = MemoryPatchRequest::Edit {
        entry: save("Window", "Use a 15 minute window", &[], Some("only")),
    };
    let kept = usecases::patch(
        store,
        &p(),
        &mem(6),
        1,
        &no_expiry,
        None,
        AGENT,
        MemoryWriter::Agent,
    )
    .await
    .unwrap();
    assert_eq!(kept.expires_at_ms, window.expires_at_ms);
    // Re-check fix 1: agents edit an entry at most 20 times a day; a run
    // at that limit is refused before its write is spent (checked below,
    // once the run's edit list exists).
    let mut window_revision = kept.revision;
    for _ in 1..runbook_core::memory_entries::MEMORY_AGENT_EDITS_PER_DAY {
        window_revision = usecases::patch(
            store,
            &p(),
            &mem(6),
            window_revision,
            &no_expiry,
            None,
            AGENT,
            MemoryWriter::Agent,
        )
        .await
        .unwrap()
        .revision;
    }
    macro_rules! edits {
        () => {{
            let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
            let step = work
                .runtime
                .state
                .steps
                .iter()
                .find(|s| s.id.as_ref() == Some(&only))
                .unwrap()
                .memory_edited
                .clone();
            drop(work);
            step
        }};
    }
    assert_eq!(edits!(), [mem(1)]);
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let limited = usecases::edit_in_run(
        work,
        &agent(),
        store,
        &p(),
        &mem(6),
        window_revision,
        &no_expiry,
        None,
    )
    .await;
    assert_eq!(limited.unwrap_err().code(), "memory.edit_limit");
    assert_eq!(edits!(), [mem(1)]);
    // A stale edit, or one of a locked entry, records nothing in the run.
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let stale = usecases::edit_in_run(work, &agent(), store, &p(), &mem(1), 1, &edit, None).await;
    assert_eq!(stale.unwrap_err().code(), "memory.conflict");
    // Re-check fix 4: invalid content spends no write.
    let invalid = MemoryPatchRequest::Edit {
        entry: save("Two\nlines", "body", &[], Some("only")),
    };
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let refused =
        usecases::edit_in_run(work, &agent(), store, &p(), &mem(1), 2, &invalid, None).await;
    assert_eq!(refused.unwrap_err().code(), "request.invalid");
    // Re-check fix 7: an expiry in the past (or within a minute) from
    // anyone but a person is an archive, which agents cannot do.
    let mut past = save("Baseline", "Expire it", &[], Some("only"));
    past.expires_at = Timestamp::from_timestamp_millis(1_000_000_000_000);
    let expire = MemoryPatchRequest::Edit { entry: past };
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let refused =
        usecases::edit_in_run(work, &agent(), store, &p(), &mem(1), 2, &expire, None).await;
    assert_eq!(refused.unwrap_err().code(), "memory.person_required");
    let refused = usecases::patch(
        store,
        &p(),
        &mem(1),
        2,
        &expire,
        None,
        AGENT,
        MemoryWriter::Agent,
    )
    .await;
    assert_eq!(refused.unwrap_err().code(), "memory.person_required");
    assert_eq!(edits!(), [mem(1)]);
    let lock = MemoryPatchRequest::Lock;
    usecases::patch(
        store,
        &p(),
        &mem(1),
        2,
        &lock,
        None,
        PERSON,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let locked = usecases::edit_in_run(work, &agent(), store, &p(), &mem(1), 3, &edit, None).await;
    assert_eq!(locked.unwrap_err().code(), "memory.locked");
    let locked = usecases::patch(
        store,
        &p(),
        &mem(1),
        3,
        &edit,
        None,
        AGENT,
        MemoryWriter::Agent,
    )
    .await;
    assert_eq!(locked.unwrap_err().code(), "memory.locked");
    assert_eq!(edits!(), [mem(1)]);
    let unlock = MemoryPatchRequest::Unlock;
    usecases::patch(
        store,
        &p(),
        &mem(1),
        3,
        &unlock,
        None,
        PERSON,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    // The edit counts: 19 more writes fill the run's 20.
    let live = save("Fact", "A fact", &[], Some("only"));
    for n in 0..19u8 {
        let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
        usecases::save_in_run(work, &agent(), mem(10 + n), scope_rb.clone(), &live, None)
            .await
            .unwrap();
    }
    let work = begin_actor(store, dir, p(), &exe(1)).await.unwrap();
    let full = usecases::edit_in_run(work, &agent(), store, &p(), &mem(1), 4, &edit, None).await;
    assert_eq!(full.unwrap_err().code(), "memory.proposal_limit");
    // The entry a person wrote last (mem 5) comes first in the step's
    // block, ahead of the run's newer saves; mem 1, which a run edited and
    // a person only locked and unlocked, does not (re-check fix 5).
    let read = usecases::read_for_step(store, &p(), &runbook(2), "only")
        .await
        .unwrap();
    assert_eq!(read.len(), 5);
    assert_eq!(read[0].id, mem(5));
    assert!(
        read[1..]
            .iter()
            .all(|e| !runbook_core::memory_entries::by_person(e))
    );
    // An archived entry is absent to an agent: every action is 404, even
    // with a wrong revision.
    let archive = MemoryPatchRequest::Archive;
    usecases::patch(
        store,
        &p(),
        &mem(1),
        4,
        &archive,
        None,
        PERSON,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    for request in [
        MemoryPatchRequest::Accept,
        MemoryPatchRequest::Archive,
        MemoryPatchRequest::Lock,
        edit.clone(),
    ] {
        let hidden = usecases::patch(
            store,
            &p(),
            &mem(1),
            99,
            &request,
            None,
            AGENT,
            MemoryWriter::Agent,
        )
        .await;
        assert_eq!(hidden.unwrap_err().code(), "resource.not_found");
    }
    // A retried save answers the entry as first created; once archived,
    // it is 404 to the agent.
    let agent_fact = save("Agent fact", "Checks take a minute", &[], Some("only"));
    let tenant = p();
    let agent_save = || {
        save_as(
            store,
            &tenant,
            mem(40),
            scope_rb.clone(),
            &agent_fact,
            MemoryWriter::Agent,
        )
    };
    agent_save().await.unwrap();
    let rename = MemoryPatchRequest::Edit {
        entry: save("Renamed", "Changed by a person", &[], Some("only")),
    };
    usecases::patch(
        store,
        &p(),
        &mem(40),
        1,
        &rename,
        None,
        PERSON,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    let replayed = agent_save().await.unwrap();
    assert_eq!(
        (replayed.revision, replayed.title.as_str()),
        (1, "Agent fact")
    );
    usecases::patch(
        store,
        &p(),
        &mem(40),
        2,
        &archive,
        None,
        PERSON,
        MemoryWriter::Person,
    )
    .await
    .unwrap();
    assert_eq!(agent_save().await.unwrap_err().code(), "resource.not_found");
}

/// MEM6 re-check fixes 1, 2 and 5, at the store (every store applies the
/// shared rule inside its fence):
/// - an entry keeps the newest revision whose content a person wrote, next
///   to revision 1 and the newest 50, however many writes follow;
/// - agents, services and runs edit an entry at most 20 times a day;
/// - an edit by anyone but a person, fenced on the revision a person's lock
///   produced, is refused by the store (`memory.locked`);
/// - locking and unlocking leave the last writer as it was.
pub async fn people_keep_their_revision_and_the_store_holds_locks<M: MemoryStore>(
    m: &M,
) -> Result<(), StoreError> {
    use runbook_core::memory_entries::{MEMORY_AGENT_EDITS_PER_DAY, MemoryWriter};
    use runbook_model::MemoryPatchRequest;
    let s = scope(1);
    let now = m.memory_time_ms().await?;
    m.create_memory(&s, entry(&s, 1, now, None), PERSON, 500)
        .await?;
    let corrected = m
        .patch_memory(&s, &mem(1), 1, edit("Corrected"), PERSON)
        .await?;
    assert_eq!(corrected.person_revision, 2);
    // Agents, services and runs share one daily allowance per entry.
    let mut revision = 2;
    for n in 0..MEMORY_AGENT_EDITS_PER_DAY {
        let writer = if n % 2 == 0 { AGENT } else { "service:tok_1" };
        revision = m
            .patch_memory(&s, &mem(1), revision, edit(&format!("Agent {n}")), writer)
            .await?
            .revision;
    }
    let run = "execution:exe_00000000000000000000000001:sr_00000000000000000000000001";
    for writer in [AGENT, run] {
        assert_eq!(
            m.patch_memory(&s, &mem(1), revision, edit("One more"), writer)
                .await,
            Err(StoreError::InvalidBatch("memory.edit_limit"))
        );
    }
    // Writes that are not agents' push the person's revision far into the
    // pruned middle; it stays.
    for n in 0..40 {
        revision = m
            .patch_memory(
                &s,
                &mem(1),
                revision,
                edit(&format!("System {n}")),
                "system",
            )
            .await?
            .revision;
    }
    let current = m.get_memory(&s, &mem(1)).await?.unwrap();
    assert_eq!(
        (
            current.revision,
            current.person_revision,
            current.revisions_pruned
        ),
        (62, 2, 10)
    );
    let kept = m.memory_revisions(&s, &mem(1)).await?;
    assert_eq!(kept.len(), 52);
    assert_eq!(revision_numbers(&kept[..3]), [1, 2, 13]);
    assert_eq!(
        m.memory_revision(&s, &mem(1), 2)
            .await?
            .map(|r| r.entry.title),
        Some("Corrected".to_owned())
    );
    let restored = by_person_patch(
        m,
        &s,
        mem(1),
        62,
        MemoryPatchRequest::Restore { revision: 2 },
    )
    .await
    .map_err(actor_store)?;
    assert_eq!(
        (
            restored.title.as_str(),
            restored.person_revision,
            restored.last_writer.as_deref()
        ),
        ("Corrected", 63, Some(PERSON))
    );
    // The lock race: an agent's edit fenced on the lock's own revision is
    // refused by the store; a person's is not.
    m.patch_memory(&s, &mem(1), 63, MemoryPatch::Lock, PERSON)
        .await?;
    for writer in ["service:tok_1", run, "system"] {
        assert_eq!(
            m.patch_memory(&s, &mem(1), 64, edit("Late"), writer).await,
            Err(StoreError::InvalidBatch("memory.locked"))
        );
    }
    m.patch_memory(&s, &mem(1), 64, edit("By a person"), PERSON)
        .await?;
    // Through the use case too: an agent that read before the lock and
    // names the lock's revision is refused.
    let refused = usecases::patch(
        m,
        &s,
        &mem(1),
        65,
        &MemoryPatchRequest::Edit {
            entry: save("Late", "late", &[], None),
        },
        None,
        AGENT,
        MemoryWriter::Agent,
    )
    .await;
    assert_eq!(refused.unwrap_err().code(), "memory.locked");
    // Locking an agent's entry does not make it a person's.
    m.create_memory(&s, entry(&s, 2, now, None), AGENT, 500)
        .await?;
    let locked = m
        .patch_memory(&s, &mem(2), 1, MemoryPatch::Lock, PERSON)
        .await?;
    let unlocked = m
        .patch_memory(&s, &mem(2), 2, MemoryPatch::Unlock, PERSON)
        .await?;
    assert_eq!(locked.last_writer.as_deref(), Some(AGENT));
    assert_eq!(unlocked.last_writer.as_deref(), Some(AGENT));
    assert_eq!(unlocked.person_revision, 0);
    Ok(())
}
