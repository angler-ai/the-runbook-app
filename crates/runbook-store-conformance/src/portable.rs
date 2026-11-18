//! D3 project-archive fixture, built through the public store traits so the
//! memory and PostgreSQL adapters hold the same logical content. Project P
//! (`scope(1)`) carries every exported table; project Q in the same
//! organization carries foreign rows an export of P must never contain.
use runbook_core::{
    StoreError,
    actor::{ActorPrincipal, begin_actor},
    blob::{BlobKey, BlobStore},
    catalog::{
        CatalogStore, RunbookOrigin, RunbookRecord, SkillProvenance, SkillVersion, TenantScope,
    },
    claims::{ClaimStore, append_logs, claim_step},
    control::cancel,
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, OrgRole, ProjectRecord, TeamRecord,
        UserRecord,
    },
    directory_operations::{
        DirectoryOperation, DirectoryOperations, StoredResponse, VariablesWrite,
    },
    publish::{prepare, publish_draft},
    retention::RunStore,
    session::SessionStore,
    skills::SkillManifest,
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    webhooks::{PreviousSecret, SealedSecret, Subscription, WebhookStore},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{
    ActorRef, ClaimRequest, Data, ExecutionId, LogsRequest, ProjectRole, RunbookId, TeamId, UserId,
    VersionId, WebhookId,
};

pub fn p() -> TenantScope {
    crate::scope(1)
}
pub fn q() -> TenantScope {
    TenantScope {
        org_id: crate::scope(1).org_id,
        project_id: crate::scope(2).project_id,
    }
}
pub fn exe(n: u8) -> ExecutionId {
    format!("exe_{n:026}").parse().unwrap()
}
pub fn rb(n: u8) -> RunbookId {
    format!("rb_{n:026}").parse().unwrap()
}
pub fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
pub fn team() -> TeamId {
    "team_00000000000000000000000001".parse().unwrap()
}
/// A team no runbook or run of P references; P's export leaves it out.
pub fn platform_team() -> TeamId {
    "team_00000000000000000000000002".parse().unwrap()
}
pub fn webhook(n: u8) -> WebhookId {
    format!("whk_{n:026}").parse().unwrap()
}
fn rbv(n: u8) -> VersionId {
    format!("rbv_{n:026}").parse().unwrap()
}
pub fn agent() -> ActorPrincipal {
    ActorPrincipal {
        principal: "service:runner".into(),
        actor: ActorRef::Agent {
            principal_id: "runner".into(),
            session_id: "s1".into(),
            name: "bot".into(),
            model: None,
        },
        credential_ref: "svc-1".into(),
    }
}
pub fn human(n: u8) -> ActorPrincipal {
    ActorPrincipal {
        principal: format!("user:{}", user(n)),
        actor: ActorRef::Human { user_id: user(n) },
        credential_ref: format!("pat-{n}"),
    }
}
/// Sealed-secret text the fixture stores; an archive must never contain it.
pub const SEALED_SECRET: &str = "v1.SEALED-SIGNING-SECRET-P";
pub const SEALED_PREVIOUS: &str = "v1.SEALED-PREVIOUS-SECRET-P";
pub const SEALED_SECRET_Q: &str = "v1.SEALED-SIGNING-SECRET-Q";

/// Waits on an approval (oncall, one vote) under a one-hour deadline, in a
/// concurrency pool of one: the first run is admitted, the second queued.
pub const GATE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: gate, title: Gate}\n\
    spec:\n  execution: {timeout: 1h, concurrency: {key: api, limit: 1}}\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Go?\", options: [keep, stop], approvers: {teams: [oncall]}, min: 1}\n    \
    - id: after\n      title: After\n";
pub const PLAIN: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: plain, title: Plain}\n\
    spec: {steps: [{id: only, title: Only}]}\n";
const WEB: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: web, title: Web}\n\
    spec: {steps: [{id: only, title: Only}]}\n";

pub async fn directory<D: DirectoryStore + DirectoryOperations>(directory: &D) {
    let org = p().org_id;
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org".into(),
        },
        DirectoryMutation::CreateProject(ProjectRecord {
            id: p().project_id,
            org_id: org.clone(),
            slug: "api".into(),
            title: "API".into(),
        }),
        DirectoryMutation::CreateProject(ProjectRecord {
            id: q().project_id,
            org_id: org.clone(),
            slug: "web".into(),
            title: "Web".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: team(),
            org_id: org.clone(),
            slug: "oncall".into(),
            title: "On call".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: platform_team(),
            org_id: org.clone(),
            slug: "platform".into(),
            title: "Platform".into(),
        }),
    ];
    for (n, name, role) in [
        (1, "alice", OrgRole::Member),
        (2, "bob", OrgRole::Member),
        (3, "carol", OrgRole::Owner),
        (4, "dave", OrgRole::Member),
    ] {
        mutations.push(DirectoryMutation::CreateUser(UserRecord {
            id: user(n),
            name: name.into(),
            active: true,
            role,
        }));
    }
    for (project, n, role) in [
        (p(), 1, ProjectRole::Operator),
        (p(), 4, ProjectRole::Admin),
        (q(), 2, ProjectRole::Operator),
    ] {
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: project.project_id,
            user_id: user(n),
            role,
        });
    }
    for n in [1, 2] {
        for team_id in [team(), platform_team()] {
            mutations.push(DirectoryMutation::AddTeamMember {
                team_id,
                user_id: user(n),
            });
        }
    }
    directory
        .commit(DirectoryCommit {
            org_id: org.clone(),
            expected_revision: 0,
            mutations,
        })
        .await
        .unwrap();
    let variables = serde_json::from_value(serde_json::json!({
        "declarations": {"region": {"type": "string"}},
        "values": {"region": "us-east"}
    }))
    .unwrap();
    directory
        .apply_operation(DirectoryOperation {
            commit: DirectoryCommit {
                org_id: org,
                expected_revision: 1,
                mutations: vec![],
            },
            variables: Some(VariablesWrite {
                project_id: p().project_id,
                expected_revision: 0,
                variables,
            }),
            replay: None,
            response: StoredResponse {
                status: 200,
                body: serde_json::Value::Null,
                etag: None,
            },
        })
        .await
        .unwrap();
}

async fn publish<S: CatalogStore + BlobStore>(store: &S, n: u8, slug: &str, source: &str) {
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
        ..Default::default()
    };
    let prepared = prepare(slug, source, &context, rbv(n)).unwrap();
    publish_draft(store, store, &rb(n), 1, prepared)
        .await
        .unwrap();
}

/// Start a run; activate it when it is admitted and `activate_now`.
pub async fn start<S, D>(store: &S, directory: &D, scope: &TenantScope, n: u8, runbook: u8)
where
    S: StartStore + CatalogStore + BlobStore + WorkQueue + SessionStore,
    D: DirectoryStore,
{
    let command = StartCommand {
        execution_id: exe(n),
        runbook_id: rb(runbook),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec!["fixture".into()],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: format!("start-{n}"),
        principal: format!("user:{}", user(1)),
        request_hash: [n; 32],
    };
    let commit = prepare_start_commit(store, store, &command).await.unwrap();
    store.commit_start(commit).await.unwrap();
    for lease in store.claim_due(16, 60_000).await.unwrap() {
        if lease.id.kind == WorkKind::Activation && lease.id.execution_id == exe(n) {
            activate(store, directory, scope.clone(), &lease)
                .await
                .unwrap();
        }
    }
}

fn subscription(scope: &TenantScope, id: WebhookId, sealed: &str, now: i64) -> Subscription {
    Subscription {
        id,
        scope: scope.clone(),
        name: "Pager".into(),
        url: "https://hooks.example.test/runbook".into(),
        events: vec!["approval.requested".into()],
        runbooks: vec![],
        tags: vec![],
        enabled: true,
        secret: SealedSecret(sealed.into()),
        previous_secret: Some(PreviousSecret {
            sealed: SealedSecret(SEALED_PREVIOUS.into()),
            expires_at_ms: now + 86_400_000,
        }),
        created_at_ms: now,
        updated_at_ms: now,
        secret_rotated_at_ms: now,
        failing_since_ms: None,
        last_failure_at_ms: None,
        disabled_reason: None,
        disabled_at_ms: None,
        revision: 0,
        notice: None,
    }
}

/// Build P and Q. `p_store` and `q_store` are handles on one database bound
/// to P and Q. Runs in P: exe_1 waits on its approval under a pending
/// deadline timer; exe_2 is queued behind it; exe_3 was cancelled and
/// deleted (a tombstone); exe_6 is claimed by an agent with log lines and
/// proposed a project memory entry; exe_4 was cancelled and kept. P's
/// memory also holds a person's edited runbook entry (D42). Q holds exe_5
/// and a memory entry of its own.
pub async fn build<S, D, W>(p_store: &S, q_store: &S, dir: &D, webhooks: &W)
where
    S: StartStore
        + CatalogStore
        + BlobStore
        + WorkQueue
        + SessionStore
        + RunStore
        + ClaimStore
        + runbook_core::memory_entries::MemoryStore,
    D: DirectoryStore + DirectoryOperations,
    W: WebhookStore,
{
    directory(dir).await;
    publish(p_store, 1, "gate", GATE).await;
    publish(p_store, 2, "plain", PLAIN).await;
    publish(q_store, 3, "web", WEB).await;
    p_store
        .save_draft(&rb(1), 1, format!("{GATE}# next revision\n"))
        .await
        .unwrap();
    p_store
        .replace_draft("notes", 0, "free-form notes".into())
        .await
        .unwrap();
    let skill_bytes = b"# Triage\nRead the dashboard first.\n";
    let skill_hash = runbook_core::portable::sha256(skill_bytes);
    p_store
        .put(BlobKey::Shared { hash: skill_hash }, skill_bytes)
        .await
        .unwrap();
    p_store
        .record_skill_version(SkillVersion {
            name: "triage".into(),
            description: "Triage".into(),
            hash: skill_hash,
            manifest: SkillManifest { entries: vec![] },
            provenance: SkillProvenance {
                reference: None,
                label: "upload".into(),
            },
        })
        .await
        .unwrap();
    start(p_store, dir, &p(), 1, 1).await;
    start(p_store, dir, &p(), 2, 1).await;
    for n in [3, 4] {
        start(p_store, dir, &p(), n, 2).await;
        let work = begin_actor(p_store, dir, p(), &exe(n)).await.unwrap();
        cancel(p_store, work, &human(4), None).await.unwrap();
    }
    p_store
        .delete_execution(&exe(3), "retention window ended")
        .await
        .unwrap();
    p_store
        .put(
            BlobKey::Execution {
                execution_id: exe(1),
                name: "logs/0001".into(),
            },
            b"step started\n",
        )
        .await
        .unwrap();
    // exe_6: an agent holds a leased claim and has streamed log lines.
    start(p_store, dir, &p(), 6, 2).await;
    let work = begin_actor(p_store, dir, p(), &exe(6)).await.unwrap();
    let only = work
        .runtime
        .state
        .steps
        .iter()
        .find(|s| s.step_id == "only")
        .and_then(|s| s.id.clone())
        .unwrap();
    let claim = claim_step(
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
    append_logs(
        p_store,
        &agent(),
        &exe(6),
        &only,
        &LogsRequest {
            claim,
            logs: vec!["checking disk".into(), "disk ok".into()],
        },
        None,
    )
    .await
    .unwrap();
    memory(p_store, q_store, dir).await;
    start(q_store, dir, &q(), 5, 3).await;
    q_store
        .put(
            BlobKey::Execution {
                execution_id: exe(5),
                name: "logs/0001".into(),
            },
            b"foreign project log\n",
        )
        .await
        .unwrap();
    let now = webhooks.webhook_time_ms().await.unwrap();
    webhooks
        .create_subscription(
            &p(),
            subscription(&p(), webhook(1), SEALED_SECRET, now),
            20,
            None,
        )
        .await
        .unwrap();
    webhooks
        .create_subscription(
            &q(),
            subscription(&q(), webhook(2), SEALED_SECRET_Q, now),
            20,
            None,
        )
        .await
        .unwrap();
}

pub fn memory_id(n: u8) -> runbook_model::MemoryId {
    format!("mem_{n:026}").parse().unwrap()
}
fn note(title: &str, body: &str, step_id: Option<&str>) -> runbook_model::SaveMemoryRequest {
    runbook_model::SaveMemoryRequest {
        title: title.into(),
        body_markdown: body.to_owned().try_into().unwrap(),
        tags: vec!["fixture".into()],
        step_id: step_id.map(str::to_owned),
        expires_at: None,
        propose: false,
    }
}
/// D42 fixture memory: in P, alice's runbook entry (created, then edited)
/// and exe_6's agent proposal for the project; in Q, one entry.
async fn memory<S, D>(p_store: &S, q_store: &S, dir: &D)
where
    S: StartStore + BlobStore + SessionStore + runbook_core::memory_entries::MemoryStore,
    D: DirectoryStore,
{
    use runbook_core::memory_entries as m;
    use runbook_model::{MemoryPatchRequest, MemoryScope};
    let actor = format!("human:{}", user(1));
    let runbook = MemoryScope::Runbook { runbook_id: rb(1) };
    let note1 = note("Gate owner", "On-call approves the gate.", Some("gate"));
    m::create(
        p_store,
        &p(),
        memory_id(1),
        runbook,
        &note1,
        None,
        runbook_model::MemorySource::Person { user_id: user(1) },
        &actor,
        m::MemoryWriter::Person,
    )
    .await
    .unwrap();
    let edit = MemoryPatchRequest::Edit {
        entry: note(
            "Gate owner",
            "On-call or a lead approves the gate.",
            Some("gate"),
        ),
    };
    m::patch(
        p_store,
        &p(),
        &memory_id(1),
        1,
        &edit,
        None,
        &actor,
        m::MemoryWriter::Person,
    )
    .await
    .unwrap();
    let work = begin_actor(p_store, dir, p(), &exe(6)).await.unwrap();
    let mut proposal = note(
        "Disk check",
        "Disk usage normally sits near 40%.",
        Some("only"),
    );
    proposal.propose = true;
    m::save_in_run(
        work,
        &agent(),
        memory_id(2),
        MemoryScope::Project,
        &proposal,
        None,
    )
    .await
    .unwrap();
    let foreign = note("Foreign memory", "project Q memory body", None);
    m::create(
        q_store,
        &q(),
        memory_id(3),
        MemoryScope::Project,
        &foreign,
        None,
        runbook_model::MemorySource::Person { user_id: user(2) },
        &format!("human:{}", user(2)),
        m::MemoryWriter::Person,
    )
    .await
    .unwrap();
}

/// Is the run still open (its approval undecided)?
pub async fn status<S: runbook_core::ExecutionStore>(
    store: &S,
    n: u8,
) -> Result<runbook_model::ExecutionStatus, StoreError> {
    Ok(store.load(&exe(n)).await?.projection.status)
}

/// The attacker's user in a crafted archive.
pub fn attacker() -> UserId {
    "usr_00000000000000000000000066".parse().unwrap()
}
/// A crafted archive naming the victim organization (`p().org_id`) with the
/// attacker as Owner and admin, the victim's user alice as Owner, the
/// victim's existing team oncall listing the attacker, and a new team.
pub fn attacker_archive() -> runbook_core::portable::ProjectData {
    use runbook_core::{
        directory::OrganizationRecord,
        portable::{DirectorySlice, ProjectData, ProjectMember, StoreRows, TeamMember},
    };
    let scope = TenantScope {
        org_id: p().org_id,
        project_id: "prj_00000000000000000000000066".parse().unwrap(),
    };
    let new_team: TeamId = "team_00000000000000000000000066".parse().unwrap();
    let mut data = ProjectData {
        directory: DirectorySlice {
            organization: OrganizationRecord {
                id: scope.org_id.clone(),
                title: "Taken over".into(),
            },
            project: ProjectRecord {
                id: scope.project_id.clone(),
                org_id: scope.org_id.clone(),
                slug: "evil".into(),
                title: "Evil".into(),
            },
            users: vec![
                UserRecord {
                    id: user(1),
                    name: "alice".into(),
                    active: true,
                    role: OrgRole::Owner,
                },
                UserRecord {
                    id: attacker(),
                    name: "mallory".into(),
                    active: true,
                    role: OrgRole::Owner,
                },
            ],
            teams: vec![
                TeamRecord {
                    id: team(),
                    org_id: scope.org_id.clone(),
                    slug: "oncall".into(),
                    title: "Hijacked".into(),
                },
                TeamRecord {
                    id: new_team.clone(),
                    org_id: scope.org_id.clone(),
                    slug: "evil-team".into(),
                    title: "Evil team".into(),
                },
            ],
            project_members: vec![
                ProjectMember {
                    user_id: user(1),
                    role: ProjectRole::Admin,
                },
                ProjectMember {
                    user_id: attacker(),
                    role: ProjectRole::Admin,
                },
            ],
            team_members: vec![
                TeamMember {
                    team_id: team(),
                    user_id: attacker(),
                },
                TeamMember {
                    team_id: new_team.clone(),
                    user_id: attacker(),
                },
            ],
        },
        scope,
        variables: None,
        rows: StoreRows::default(),
        webhooks: vec![],
    };
    runbook_core::portable::normalize(&mut data);
    data
}
