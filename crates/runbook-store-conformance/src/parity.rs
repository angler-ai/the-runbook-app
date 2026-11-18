//! Store-trait parity (store hardening): cases for trait methods the other
//! modules never call, run against every adapter. Each case takes two
//! handles on one database, `p` bound to project P (`scope(1)`) and `q`
//! bound to project Q in the same organization, and asserts both the
//! method's contract on P and that Q can never read, change or count P's
//! rows through it.
use crate::portable::{GATE, PLAIN, agent, directory, exe, human, p, rb, start, team, user};
use runbook_core::{
    StoreError,
    actor::{Replay, begin_actor},
    blob::{BlobKey, BlobPrefix, BlobStore},
    catalog::{
        AdmissionState, CatalogStore, RunbookOrigin, RunbookRecord, SkillProvenance, SkillVersion,
    },
    claims::{ClaimStore, claim_step},
    control::cancel,
    credentials::{
        CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind, secret_hash,
    },
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    execution_credentials::{ExecutionCredentialStore, MintEntropy, mint_execution_pair},
    publish::{prepare, publish_draft, publish_preview},
    retention::RunStore,
    session::{ExecutionSession, SessionStore},
    skills::SkillManifest,
    start::StartStore,
    step_lookup::StepRunLookup,
    webhooks::ProjectOutbox,
    work_queue::WorkQueue,
};
use runbook_model::{
    ClaimId, ClaimRef, ClaimRequest, MintTokenRequest, OrganizationId, RunbookId, StepRunId,
    TokenId, VersionId,
};
use std::{collections::BTreeSet, num::NonZeroU64};

fn rbv(n: u8) -> VersionId {
    format!("rbv_{n:026}").parse().unwrap()
}
fn clm(n: u8) -> ClaimId {
    format!("clm_{n:026}").parse().unwrap()
}
/// An organization no fixture row belongs to.
fn foreign_org() -> OrganizationId {
    crate::scope(9).org_id
}

/// Create and publish runbook `n` in the handle's project.
pub async fn publish<S: CatalogStore + BlobStore>(
    store: &S,
    n: u8,
    slug: &str,
    source: &str,
    version: u8,
) {
    store
        .create_runbook(
            RunbookRecord {
                runbook_id: rb(n),
                slug: slug.into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            source.into(),
        )
        .await
        .unwrap();
    let context = runbook_spec::CompileContext {
        teams: [("oncall".into(), team())].into(),
        users: [("alice".into(), crate::portable::user(1))].into(),
        ..Default::default()
    };
    let prepared = prepare(slug, source, &context, rbv(version)).unwrap();
    publish_draft(store, store, &rb(n), 1, prepared)
        .await
        .unwrap();
}

/// The materialized step run ID of step `step_id` in run `n`.
async fn step_run<S, D>(store: &S, dir: &D, n: u8, step_id: &str) -> StepRunId
where
    S: StartStore + CatalogStore + BlobStore + SessionStore,
    D: DirectoryStore,
{
    begin_actor(store, dir, p(), &exe(n))
        .await
        .unwrap()
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == step_id)
        .and_then(|s| s.id.clone())
        .expect("materialized step")
}

/// ClaimStore: claim_record, heartbeat and append_logs fence on owner and
/// generation, count lines, replay once, and are invisible to project Q.
pub async fn claim_store_is_fenced_and_scoped<S, D>(ps: &S, qs: &S, dir: &D)
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore + ClaimStore,
    D: DirectoryStore + DirectoryOperations,
{
    directory(dir).await;
    publish(ps, 2, "plain", PLAIN, 2).await;
    start(ps, dir, &p(), 1, 2).await;
    let step = step_run(ps, dir, 1, "only").await;
    let work = begin_actor(ps, dir, p(), &exe(1)).await.unwrap();
    let claim = claim_step(
        work,
        &agent(),
        &step,
        &ClaimRequest {
            take_over: false,
            session_id: "s1".into(),
            actor: None,
        },
        clm(1),
        None,
    )
    .await
    .unwrap();
    let owner = agent().actor;

    let record = ps.claim_record(&exe(1), &step).await.unwrap().unwrap();
    assert_eq!(record.claim_id, claim.id);
    assert_eq!(record.generation, claim.generation.get());
    assert_eq!(record.owner, owner);
    assert!(!record.released);
    let leased_until = record.lease_deadline_at_ms.expect("agent claims lease");
    assert_eq!(qs.claim_record(&exe(1), &step).await.unwrap(), None);
    let unknown: StepRunId = "sr_00000000000000000000000099".parse().unwrap();
    assert_eq!(ps.claim_record(&exe(1), &unknown).await.unwrap(), None);

    // Heartbeat: the owner at the current generation extends the lease.
    let extended = ps
        .heartbeat(&exe(1), &step, &claim, &owner, None)
        .await
        .unwrap()
        .expect("agent lease");
    assert!(extended >= leased_until);
    assert_eq!(
        ps.claim_record(&exe(1), &step)
            .await
            .unwrap()
            .unwrap()
            .lease_deadline_at_ms,
        Some(extended)
    );
    let stale = ClaimRef {
        id: claim.id.clone(),
        generation: NonZeroU64::new(claim.generation.get() + 1).unwrap(),
    };
    let wrong_id = ClaimRef {
        id: clm(2),
        generation: claim.generation,
    };
    for (ref_, who) in [
        (&stale, &owner),
        (&wrong_id, &owner),
        (&claim, &human(1).actor),
    ] {
        assert_eq!(
            ps.heartbeat(&exe(1), &step, ref_, who, None).await,
            Err(StoreError::InvalidBatch("queue.stale_lease")),
            "{ref_:?} {who:?}"
        );
        assert_eq!(
            ps.append_logs(&exe(1), &step, ref_, who, vec!["no".into()], None)
                .await,
            Err(StoreError::InvalidBatch("queue.stale_lease")),
            "{ref_:?} {who:?}"
        );
    }
    assert_eq!(
        qs.heartbeat(&exe(1), &step, &claim, &owner, None).await,
        Err(StoreError::NotFound)
    );

    // Logs: the count is the running total for this step's claim log.
    assert_eq!(
        ps.append_logs(
            &exe(1),
            &step,
            &claim,
            &owner,
            vec!["a".into(), "b".into()],
            None
        )
        .await,
        Ok(2)
    );
    assert_eq!(
        qs.append_logs(&exe(1), &step, &claim, &owner, vec!["q".into()], None)
            .await,
        Err(StoreError::NotFound)
    );
    let replay = Replay {
        key: "logs-1".into(),
        principal: agent().principal,
        request_hash: [5; 32],
    };
    for _ in 0..2 {
        assert_eq!(
            ps.append_logs(
                &exe(1),
                &step,
                &claim,
                &owner,
                vec!["c".into()],
                Some(&replay)
            )
            .await,
            Ok(3),
            "a replayed append writes once"
        );
    }
    let reused = Replay {
        request_hash: [6; 32],
        ..replay.clone()
    };
    assert_eq!(
        ps.append_logs(
            &exe(1),
            &step,
            &claim,
            &owner,
            vec!["c".into()],
            Some(&reused)
        )
        .await,
        Err(StoreError::InvalidBatch("request.idempotency_key_reused"))
    );
    assert_eq!(
        ps.append_logs(&exe(1), &step, &claim, &owner, vec!["d".into()], None)
            .await,
        Ok(4),
        "neither project Q, the fenced writes nor the replay appended a line"
    );
    // Review M6: every entry counts at least one byte toward the step's
    // 5 MiB, so blank lines cannot grow the log without bound.
    // Filled with lines under 1 MB each: a Durable Object row holds at most
    // 2 MB, so one 5 MiB line is not something any store can keep.
    let mut left = runbook_core::claims::MAX_STEP_LOG_BYTES as usize - 4;
    let mut lines = 4;
    while left > 0 {
        let take = left.min(1_000_000);
        left -= take;
        lines += 1;
        assert_eq!(
            ps.append_logs(&exe(1), &step, &claim, &owner, vec!["x".repeat(take)], None)
                .await,
            Ok(lines)
        );
    }
    assert_eq!(
        ps.append_logs(&exe(1), &step, &claim, &owner, vec![String::new()], None)
            .await,
        Err(StoreError::InvalidBatch("step.logs_too_large"))
    );

    // SessionStore::actor_replay: the locked session finds the saved row
    // by prefix; another operation's prefix and project Q find nothing.
    let session = ps.begin(&exe(1)).await.unwrap();
    let operation = format!("append_logs:{step}");
    let saved = session
        .actor_replay(&replay.prefix(&operation).unwrap())
        .await
        .unwrap()
        .expect("saved replay row");
    assert!(saved.key.starts_with(&replay.prefix(&operation).unwrap()));
    assert_eq!(saved.request_hash, [5; 32]);
    assert_eq!(saved.response["value"], serde_json::json!(3));
    assert_eq!(
        session
            .actor_replay(&replay.prefix(&format!("heartbeat:{step}")).unwrap())
            .await
            .unwrap(),
        None
    );
    drop(session);
    assert!(matches!(qs.begin(&exe(1)).await, Err(StoreError::NotFound)));
}

/// StartStore: start_record, find_replay and cancel_queued, scoped to P.
pub async fn start_store_reads_and_cancel_are_scoped<S, D>(ps: &S, qs: &S, dir: &D)
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore + DirectoryOperations,
{
    directory(dir).await;
    publish(ps, 1, "gate", GATE, 1).await;
    start(ps, dir, &p(), 1, 1).await;
    start(ps, dir, &p(), 2, 1).await;
    assert_eq!(
        ps.admission(&exe(2)).await.unwrap().unwrap().state,
        AdmissionState::Queued
    );

    let record = ps.start_record(&exe(1)).await.unwrap().expect("record");
    assert_eq!(record.runbook_id, rb(1));
    assert_eq!(record.version_id, rbv(1));
    assert_eq!(record.version.map(|v| v.get()), Some(1));
    assert_eq!(record.concurrency_key.as_deref(), Some("api"));
    assert_eq!(qs.start_record(&exe(1)).await.unwrap(), None);
    assert_eq!(ps.start_record(&exe(7)).await.unwrap(), None);

    let saved = ps.find_replay("start-2").await.unwrap().expect("replay");
    assert_eq!(saved.key, "start-2");
    assert_eq!(saved.principal, format!("user:{}", user(1)));
    assert_eq!(saved.request_hash, [2; 32]);
    assert_eq!(saved.outcome.execution_id, exe(2));
    assert_eq!(
        saved.outcome.admission.as_ref().map(|a| a.state),
        Some(AdmissionState::Queued)
    );
    assert_eq!(qs.find_replay("start-2").await.unwrap(), None);
    assert_eq!(ps.find_replay("start-9").await.unwrap(), None);

    // Q can neither cancel P's waiter nor learn that it exists.
    assert_eq!(
        qs.cancel_queued(&exe(2), None).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        ps.admission(&exe(2)).await.unwrap().unwrap().state,
        AdmissionState::Queued
    );
    // An admitted run is not a waiter; the waiter is removed exactly once.
    assert_eq!(ps.cancel_queued(&exe(1), None).await, Ok(false));
    assert_eq!(ps.cancel_queued(&exe(2), None).await, Ok(true));
    assert_eq!(ps.cancel_queued(&exe(2), None).await, Ok(false));
    assert_ne!(
        ps.admission(&exe(2)).await.unwrap().map(|a| a.state),
        Some(AdmissionState::Queued)
    );
}

fn issuer() -> CredentialRecord {
    CredentialRecord {
        id: "tok_00000000000000000000000200".parse().unwrap(),
        kind: TokenKind::Service,
        hash: secret_hash("trb_svc_parity-launcher"),
        principal: CredentialPrincipal::Service {
            project_id: p().project_id,
            role: runbook_model::ProjectRole::Operator,
        },
        org_id: p().org_id,
        scopes: [Scope::ExecutionsAct].into(),
        projects: None,
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
    }
}
fn tok(n: u8) -> TokenId {
    format!("tok_{n:026}").parse().unwrap()
}

/// ExecutionCredentialStore reads and CredentialStore::find_by_id.
pub async fn execution_credential_reads_are_scoped<S, D, C>(ps: &S, qs: &S, dir: &D, creds: &C)
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore + ExecutionCredentialStore,
    D: DirectoryStore + DirectoryOperations,
    C: CredentialStore,
{
    directory(dir).await;
    publish(ps, 2, "plain", PLAIN, 2).await;
    start(ps, dir, &p(), 1, 2).await;
    creds.insert(issuer()).await.unwrap();
    assert_eq!(
        creds.find_by_id(&issuer().id).await.unwrap(),
        Some(issuer())
    );
    assert_eq!(creds.find_by_id(&tok(201)).await.unwrap(), None);

    let pair = mint_execution_pair(
        ps,
        creds,
        dir,
        &issuer(),
        &exe(1),
        &MintTokenRequest {
            session_id: "sess-1".into(),
        },
        "mint-1",
        [9; 32],
        MintEntropy {
            family_id: "fam-parity".into(),
            access_token_id: tok(2),
            refresh_token_id: tok(3),
            access_random: [2; 32],
            refresh_random: [3; 32],
        },
    )
    .await
    .unwrap();

    let family = ps
        .credential_family("fam-parity")
        .await
        .unwrap()
        .expect("family");
    assert_eq!(family.execution_id, exe(1));
    assert_eq!(family.idempotency_key, "mint-1");
    assert_eq!(family.revoked_at_ms, None);
    assert_eq!(ps.credential_family("fam-none").await.unwrap(), None);
    assert_eq!(
        ps.family_by_idempotency_key(&exe(1), "mint-1")
            .await
            .unwrap(),
        Some(family.clone())
    );
    assert_eq!(
        ps.family_by_idempotency_key(&exe(1), "mint-2")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        ps.family_by_idempotency_key(&exe(2), "mint-1")
            .await
            .unwrap(),
        None
    );
    for (token, kind) in [
        (&pair.access, TokenKind::Execution),
        (&pair.refresh, TokenKind::Refresh),
    ] {
        let by_hash = ps
            .credential_by_hash(&secret_hash(&token.secret))
            .await
            .unwrap()
            .expect("token by hash");
        assert_eq!(by_hash.token_id, token.token_id);
        assert_eq!(by_hash.kind, kind);
        assert_eq!(by_hash.family_id, "fam-parity");
        assert_eq!(by_hash.expires_at_ms, token.expires_at_ms);
        assert_eq!(
            ps.credential_by_id(&token.token_id).await.unwrap(),
            Some(by_hash.clone())
        );
        // Project Q sees none of P's credential rows by any key.
        assert_eq!(
            qs.credential_by_hash(&secret_hash(&token.secret))
                .await
                .unwrap(),
            None
        );
        assert_eq!(qs.credential_by_id(&token.token_id).await.unwrap(), None);
    }
    assert_eq!(qs.credential_family("fam-parity").await.unwrap(), None);
    assert_eq!(
        qs.family_by_idempotency_key(&exe(1), "mint-1")
            .await
            .unwrap(),
        None
    );
    assert_eq!(ps.credential_by_id(&tok(99)).await.unwrap(), None);
}

/// Org-scoped routing reads: StepRunLookup, execution_project and
/// tombstone_project resolve within the organization only.
pub async fn org_routing_reads_are_org_scoped<S, D>(ps: &S, qs: &S, dir: &D)
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore + StepRunLookup + RunStore,
    D: DirectoryStore + DirectoryOperations,
{
    directory(dir).await;
    publish(ps, 2, "plain", PLAIN, 2).await;
    start(ps, dir, &p(), 1, 2).await;
    start(ps, dir, &p(), 2, 2).await;
    let step = step_run(ps, dir, 1, "only").await;
    let org = p().org_id;
    let found = Some((exe(1), p().project_id));
    // Routing reads are org-scoped by contract, so a sibling project's
    // handle resolves the same answer; another organization never does.
    assert_eq!(ps.step_run_execution(&org, &step).await.unwrap(), found);
    assert_eq!(qs.step_run_execution(&org, &step).await.unwrap(), found);
    assert_eq!(
        ps.step_run_execution(&foreign_org(), &step).await.unwrap(),
        None
    );
    let unknown: StepRunId = "sr_00000000000000000000000099".parse().unwrap();
    assert_eq!(ps.step_run_execution(&org, &unknown).await.unwrap(), None);

    assert_eq!(
        ps.execution_project(&org, &exe(1)).await.unwrap(),
        Some(p().project_id)
    );
    assert_eq!(
        qs.execution_project(&org, &exe(1)).await.unwrap(),
        Some(p().project_id)
    );
    assert_eq!(
        ps.execution_project(&foreign_org(), &exe(1)).await.unwrap(),
        None
    );
    assert_eq!(ps.execution_project(&org, &exe(9)).await.unwrap(), None);
    assert_eq!(ps.tombstone_project(&org, &exe(2)).await.unwrap(), None);

    let work = begin_actor(ps, dir, p(), &exe(2)).await.unwrap();
    cancel(ps, work, &human(4), None).await.unwrap();
    assert_eq!(
        qs.delete_execution(&exe(2), "retention").await,
        Err(StoreError::NotFound)
    );
    assert_eq!(qs.tombstone(&exe(2)).await.unwrap(), None);
    ps.delete_execution(&exe(2), "retention").await.unwrap();
    assert_eq!(ps.execution_project(&org, &exe(2)).await.unwrap(), None);
    assert_eq!(
        ps.tombstone_project(&org, &exe(2)).await.unwrap(),
        Some(p().project_id)
    );
    assert_eq!(
        ps.tombstone_project(&foreign_org(), &exe(2)).await.unwrap(),
        None
    );
    assert_eq!(qs.tombstone(&exe(2)).await.unwrap(), None);
    assert!(ps.tombstone(&exe(2)).await.unwrap().is_some());
}

/// CatalogStore publication reads: runbook, draft, versions, previews and
/// skills, plus the project-event outbox the publish writes.
pub async fn catalog_publication_reads_are_scoped<S>(ps: &S, qs: &S)
where
    S: CatalogStore + BlobStore + ProjectOutbox,
{
    let rb1: RunbookId = rb(2);
    publish(ps, 2, "plain", PLAIN, 1).await;
    let record = ps.read_runbook(&rb1).await.unwrap().expect("runbook");
    assert_eq!(record.slug, "plain");
    assert_eq!(record.origin, RunbookOrigin::Ui);
    assert_eq!(qs.read_runbook(&rb1).await.unwrap(), None);
    let draft = ps.get_draft(&rb1).await.unwrap().expect("draft");
    assert_eq!(qs.get_draft(&rb1).await.unwrap(), None);

    // A second numbered version and a preview.
    let next = PLAIN.replace("title: Only}", "title: Only v2}");
    let revision = draft.revision.get();
    assert_eq!(
        ps.save_draft(&rb1, revision, next.to_string())
            .await
            .unwrap(),
        runbook_core::CommitResult::Committed(revision + 1)
    );
    let ctx = runbook_spec::CompileContext::default();
    publish_draft(
        ps,
        ps,
        &rb1,
        revision + 1,
        prepare("plain", &next, &ctx, rbv(2)).unwrap(),
    )
    .await
    .unwrap();
    let preview = publish_preview(
        ps,
        ps,
        &rb1,
        prepare(
            "plain",
            &PLAIN.replace("title: Only}", "title: Preview}"),
            &ctx,
            rbv(3),
        )
        .unwrap(),
        Some("feature".into()),
        Some("abc123".into()),
    )
    .await
    .unwrap();
    assert_eq!(preview.runbook_version, None);
    assert_eq!(preview.branch.as_deref(), Some("feature"));

    let mut versions: Vec<_> = ps
        .list_versions(&rb1)
        .await
        .unwrap()
        .into_iter()
        .map(|v| (v.runbook_version_id, v.runbook_version.get()))
        .collect();
    versions.sort();
    assert_eq!(versions, vec![(rbv(1), 1), (rbv(2), 2)]);
    let latest = ps.resolve_version(&rb1, None).await.unwrap();
    assert_eq!(latest.runbook_version_id, rbv(2));
    let pinned = ps
        .resolve_version(
            &rb1,
            Some(runbook_model::VersionSelector::Number(
                std::num::NonZeroU32::new(1).unwrap(),
            )),
        )
        .await
        .unwrap();
    assert_eq!(pinned.runbook_version_id, rbv(1));
    let v1 = ps.version_record(&rb1, &rbv(1)).await.unwrap().unwrap();
    assert_eq!(v1.source, PLAIN);
    assert_eq!(v1.number.map(|n| n.get()), Some(1));
    let previews = ps.list_previews(&rb1).await.unwrap();
    assert_eq!(previews, vec![preview.clone()]);
    assert_eq!(
        ps.version_record(&rb1, &rbv(3))
            .await
            .unwrap()
            .unwrap()
            .number,
        None
    );

    // Project Q reaches none of it.
    assert_eq!(qs.list_versions(&rb1).await, Err(StoreError::NotFound));
    assert_eq!(qs.list_previews(&rb1).await, Err(StoreError::NotFound));
    assert_eq!(
        qs.resolve_version(&rb1, None).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(qs.version_record(&rb1, &rbv(1)).await.unwrap(), None);

    // Skills: recorded once per hash, latest per name, scoped to P.
    let skill = |hash: [u8; 32]| SkillVersion {
        name: "triage".into(),
        description: "Triage".into(),
        hash,
        manifest: SkillManifest { entries: vec![] },
        provenance: SkillProvenance {
            reference: None,
            label: "upload".into(),
        },
    };
    assert_eq!(
        ps.record_skill_version(skill([1; 32])).await.unwrap(),
        skill([1; 32])
    );
    assert_eq!(
        ps.record_skill_version(skill([1; 32])).await.unwrap(),
        skill([1; 32])
    );
    ps.record_skill_version(skill([2; 32])).await.unwrap();
    assert_eq!(
        ps.skill_version(&[1; 32]).await.unwrap(),
        Some(skill([1; 32]))
    );
    assert_eq!(ps.latest_skills().await.unwrap(), vec![skill([2; 32])]);
    assert_eq!(qs.skill_version(&[1; 32]).await.unwrap(), None);
    assert!(qs.latest_skills().await.unwrap().is_empty());
    assert!(
        ps.versions_pinning_skill(&[1; 32])
            .await
            .unwrap()
            .is_empty()
    );

    // Each newly numbered version left one project event; previews none.
    let pending = ps.pending_project_events(10).await.unwrap();
    assert_eq!(pending.len(), 2, "{pending:?}");
    assert!(pending.iter().all(|e| e.event_type == "runbook.published"));
    assert!(pending[0].at_ms <= pending[1].at_ms);
    assert!(qs.pending_project_events(10).await.unwrap().is_empty());
    assert_eq!(ps.pending_project_events(1).await.unwrap(), pending[..1]);
    // Q's ack of P's event ID is a no-op; P's ack removes exactly one row.
    qs.ack_project_event(&pending[0].event_id).await.unwrap();
    assert_eq!(ps.pending_project_events(10).await.unwrap(), pending);
    ps.ack_project_event(&pending[0].event_id).await.unwrap();
    ps.ack_project_event(&pending[0].event_id).await.unwrap();
    assert_eq!(ps.pending_project_events(10).await.unwrap(), pending[1..]);
}

/// BlobStore::delete and list_prefix: a delete removes one key, a missing
/// key is NotFound, and project Q can neither list nor delete P's blobs.
pub async fn blob_delete_and_listing_are_scoped<S, D>(ps: &S, qs: &S, dir: &D)
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore + DirectoryOperations,
{
    directory(dir).await;
    publish(ps, 2, "plain", PLAIN, 2).await;
    start(ps, dir, &p(), 1, 2).await;
    let key = |name: &str| BlobKey::Execution {
        execution_id: exe(1),
        name: name.into(),
    };
    ps.put(key("logs/0001"), b"one").await.unwrap();
    ps.put(key("logs/0002"), b"two").await.unwrap();
    let shared = BlobKey::Shared { hash: [4; 32] };
    ps.put(shared.clone(), b"shared").await.unwrap();

    assert_eq!(
        qs.list_prefix(&BlobPrefix::Execution(exe(1))).await,
        Err(StoreError::NotFound)
    );
    assert!(
        !qs.list_prefix(&BlobPrefix::Shared)
            .await
            .unwrap()
            .contains(&shared)
    );
    assert_eq!(
        qs.delete(&key("logs/0001")).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(qs.delete(&shared).await, Err(StoreError::NotFound));
    assert_eq!(ps.get(&key("logs/0001")).await.unwrap(), b"one");
    assert_eq!(ps.get(&shared).await.unwrap(), b"shared");

    ps.delete(&key("logs/0001")).await.unwrap();
    assert_eq!(ps.get(&key("logs/0001")).await, Err(StoreError::NotFound));
    assert_eq!(
        ps.delete(&key("logs/0001")).await,
        Err(StoreError::NotFound)
    );
    let mut listed = ps
        .list_prefix(&BlobPrefix::Execution(exe(1)))
        .await
        .unwrap();
    listed.sort();
    assert_eq!(listed, vec![key("logs/0002")]);
    // A deleted key can be written again with new bytes.
    ps.put(key("logs/0001"), b"again").await.unwrap();
    assert_eq!(ps.get(&key("logs/0001")).await.unwrap(), b"again");
}

/// Tenant sweep for the scoped writes and counts no other case drives from
/// a foreign handle: admission, draft save, publish, preview, skill record,
/// pinned-version and start replay lookups, start creation, and project
/// variables. Q's writes are refused or land in Q's own namespace; P's rows
/// and counts are unchanged.
pub async fn foreign_writes_and_counts_are_refused<S, D>(ps: &S, qs: &S, dir: &D)
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore + DirectoryOperations,
{
    use runbook_core::{catalog::AdmitRequest, start::prepare_start_commit};
    directory(dir).await;
    publish(ps, 1, "gate", GATE, 1).await;
    start(ps, dir, &p(), 1, 1).await;
    start(ps, dir, &p(), 2, 1).await;
    let draft = ps.get_draft(&rb(1)).await.unwrap().unwrap();
    let versions = ps.list_versions(&rb(1)).await.unwrap();

    // Admission: Q neither sees P's admissions nor counts toward P's pool.
    assert_eq!(qs.admission(&exe(1)).await.unwrap(), None);
    assert_eq!(qs.admission(&exe(2)).await.unwrap(), None);
    assert_eq!(
        qs.admit(AdmitRequest {
            execution_id: exe(8),
            key: "api".into(),
            limit: 1,
            idempotency_key: "q-admit".into(),
        })
        .await
        .unwrap()
        .state,
        AdmissionState::Admitted,
        "Q's pool named api is Q's own, not P's full pool"
    );
    assert_eq!(ps.admission(&exe(8)).await.unwrap(), None);
    assert_eq!(
        ps.admission(&exe(2)).await.unwrap().unwrap().state,
        AdmissionState::Queued
    );

    // Drafts, publication and previews of P's runbook.
    assert_eq!(
        qs.save_draft(&rb(1), draft.revision.get(), "foreign".into())
            .await,
        Err(StoreError::NotFound)
    );
    let ctx = runbook_spec::CompileContext {
        teams: [("oncall".into(), team())].into(),
        ..Default::default()
    };
    let other = GATE.replace("title: After", "title: Foreign");
    let prepared = prepare("gate", &other, &ctx, rbv(7)).unwrap();
    assert_eq!(
        publish_draft(qs, qs, &rb(1), draft.revision.get(), prepared.clone()).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        publish_preview(qs, qs, &rb(1), prepared, None, None).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(ps.get_draft(&rb(1)).await.unwrap().unwrap(), draft);
    assert_eq!(ps.list_versions(&rb(1)).await.unwrap(), versions);
    assert!(ps.list_previews(&rb(1)).await.unwrap().is_empty());
    assert_eq!(ps.version_record(&rb(1), &rbv(7)).await.unwrap(), None);

    // Skills: the same hash recorded by Q is Q's row; P's library is
    // unchanged and Q sees no version pinning it.
    let skill = |label: &str| SkillVersion {
        name: "triage".into(),
        description: label.into(),
        hash: [3; 32],
        manifest: SkillManifest { entries: vec![] },
        provenance: SkillProvenance {
            reference: None,
            label: label.into(),
        },
    };
    ps.record_skill_version(skill("p")).await.unwrap();
    assert_eq!(
        qs.record_skill_version(skill("q")).await.unwrap(),
        skill("q")
    );
    assert_eq!(ps.skill_version(&[3; 32]).await.unwrap(), Some(skill("p")));
    assert_eq!(ps.latest_skills().await.unwrap(), vec![skill("p")]);
    assert!(
        qs.versions_pinning_skill(&[3; 32])
            .await
            .unwrap()
            .is_empty()
    );

    // Starts: Q cannot create a run under P's execution ID, and P's replay
    // key names nothing in Q, so Q's start with it is a new run of Q's.
    publish(qs, 3, "gate", GATE, 3).await;
    let command = |n: u8, runbook: u8| runbook_core::start::StartCommand {
        execution_id: exe(n),
        runbook_id: rb(runbook),
        version: None,
        inputs: Default::default(),
        fields: Default::default(),
        project_vars: Default::default(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "bob".into(),
        limits: Default::default(),
        idempotency_key: "start-1".into(),
        principal: format!("user:{}", user(1)),
        request_hash: [1; 32],
    };
    assert_eq!(qs.find_replay("start-1").await.unwrap(), None);
    let hijack = prepare_start_commit(qs, qs, &command(1, 3)).await.unwrap();
    assert_eq!(qs.commit_start(hijack).await, Err(StoreError::NotFound));
    let own = prepare_start_commit(qs, qs, &command(5, 3)).await.unwrap();
    let outcome = qs.commit_start(own).await.unwrap();
    assert_eq!(outcome.execution_id, exe(5));
    assert_eq!(
        ps.find_replay("start-1")
            .await
            .unwrap()
            .unwrap()
            .outcome
            .execution_id,
        exe(1)
    );
    assert_eq!(ps.start_record(&exe(5)).await.unwrap(), None);
    assert_eq!(
        ps.list_runs()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.execution_id)
            .collect::<BTreeSet<_>>(),
        [exe(1), exe(2)].into()
    );

    // Project variables resolve only within the project's organization.
    assert_eq!(
        dir.project_variables(&p().org_id, &p().project_id)
            .await
            .unwrap()
            .revision,
        1
    );
    assert!(
        dir.project_variables(&foreign_org(), &p().project_id)
            .await
            .is_err()
    );
}
