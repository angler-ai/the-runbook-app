//! C1 webhooks on the memory stores under a fake clock, with a recording
//! transport standing in for the receiver: fan-out filters, signatures,
//! at-least-once redelivery, approval routing, agent resumption, rotation,
//! retry policy, secret hygiene and undelivered human events.
use runbook_core::{
    StoreError,
    actor::{ActorPrincipal, begin_actor},
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    decisions::vote,
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, TeamRecord, UserRecord,
        memory::MemoryDirectory,
    },
    drain::{Processed, process_lease, process_notification},
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, prepare_start_commit},
    webhooks::{
        self, DeliverySettings, OutgoingRequest, SealedSecret, SecretSealer, Subscription,
        TransportResult, WebhookStore, WebhookTransport, memory::MemoryWebhooks,
    },
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{
    ActorRef, Data, DecisionRequest, DeliveryState, ExecutionId, ProjectRole, RunbookId, UserId,
    WebhookId,
};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::VecDeque,
    future::Future,
    pin::pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory stores must be synchronous"),
    }
}
fn scope() -> TenantScope {
    TenantScope {
        org_id: "org_00000000000000000000000001".parse().unwrap(),
        project_id: "prj_00000000000000000000000001".parse().unwrap(),
    }
}
fn usr(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn exe() -> ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
fn team(n: u8) -> runbook_model::TeamId {
    format!("team_{n:026}").parse().unwrap()
}
fn whk(n: u8) -> WebhookId {
    format!("whk_{n:026}").parse().unwrap()
}
const START_MS: i64 = 1_700_000_000_000;

/// Reversible but never the raw secret: what a host KMS seal looks like.
struct TestSealer;
impl SecretSealer for TestSealer {
    fn seal(&self, context: &str, secret: &str) -> Result<SealedSecret, StoreError> {
        Ok(SealedSecret(format!(
            "sealed:{context}:{}",
            secret.chars().rev().collect::<String>()
        )))
    }
    fn open(&self, context: &str, sealed: &SealedSecret) -> Result<String, StoreError> {
        sealed
            .0
            .strip_prefix(&format!("sealed:{context}:"))
            .map(|s| s.chars().rev().collect())
            .ok_or(StoreError::Backend("sealer.context"))
    }
}

/// The local mock receiver: records every request and answers from a
/// script per URL (default 200).
#[derive(Clone, Default)]
struct Receiver {
    seen: Rc<RefCell<Vec<OutgoingRequest>>>,
    script: Rc<RefCell<VecDeque<TransportResult>>>,
}
impl Receiver {
    fn respond(&self, result: TransportResult) {
        self.script.borrow_mut().push_back(result);
    }
    fn received(&self, url: &str) -> Vec<(String, Value)> {
        self.seen
            .borrow()
            .iter()
            .filter(|r| r.url == url)
            .map(|r| {
                (
                    header(r, "Runbook-Event-Type"),
                    serde_json::from_str(&r.body).unwrap(),
                )
            })
            .collect()
    }
    /// Delivery order is not guaranteed; receivers order by `sequence`.
    fn ordered(&self, url: &str) -> Vec<(String, Value)> {
        let mut received = self.received(url);
        received.sort_by_key(|(_, body)| body["sequence"].as_u64());
        received
    }
}
impl WebhookTransport for Receiver {
    async fn post(&self, request: OutgoingRequest) -> TransportResult {
        self.seen.borrow_mut().push(request);
        self.script
            .borrow_mut()
            .pop_front()
            .unwrap_or(TransportResult::Response {
                status: 200,
                latency_ms: 5,
                snippet: "ok".into(),
            })
    }
}
fn header(r: &OutgoingRequest, name: &str) -> String {
    r.headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

struct Env {
    store: MemoryExecutionStore,
    directory: MemoryDirectory,
    webhooks: MemoryWebhooks,
    receiver: Receiver,
    now: i64,
}
impl Env {
    fn new(source: &str) -> Self {
        let projection = runbook_core::ExecutionProjection {
            execution_id: "exe_00000000000000000000000090".parse().unwrap(),
            runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
            version_id: "rbv_00000000000000000000000090".parse().unwrap(),
            compiled_hash: [0; 32],
            engine_version: runbook_engine::ENGINE_VERSION,
            sequence: 0,
            status: runbook_model::ExecutionStatus::Queued,
            phase: runbook_engine::ExecutionPhase::Ordinary,
            completed_at_ms: None,
            result: Value::Null,
            outputs: Default::default(),
            steps: vec![],
        };
        let store = MemoryExecutionStore::new(scope(), projection, START_MS);
        let directory = MemoryDirectory::default();
        let mut mutations = vec![
            DirectoryMutation::CreateOrganization {
                title: "Org".into(),
            },
            DirectoryMutation::CreateProject(ProjectRecord {
                id: scope().project_id,
                org_id: scope().org_id,
                slug: "api".into(),
                title: "API".into(),
            }),
        ];
        // Dave is deactivated (D16): a member of both teams who must never
        // be notified.
        for (n, name) in [(1u8, "alice"), (2, "bob"), (3, "carol"), (4, "dave")] {
            mutations.push(DirectoryMutation::CreateUser(UserRecord {
                id: usr(n),
                name: name.into(),
                active: n != 4,
                role: runbook_core::directory::OrgRole::Member,
            }));
            mutations.push(DirectoryMutation::GrantProjectRole {
                project_id: scope().project_id,
                user_id: usr(n),
                role: ProjectRole::Operator,
            });
        }
        // oncall = alice, bob, dave; eng-leads = carol, dave; retired =
        // dave only (no active member).
        for (t, slug, members) in [
            (1u8, "oncall", &[1u8, 2, 4][..]),
            (2, "eng-leads", &[3, 4]),
            (3, "retired", &[4]),
        ] {
            mutations.push(DirectoryMutation::CreateTeam(TeamRecord {
                id: team(t),
                org_id: scope().org_id,
                slug: slug.into(),
                title: slug.into(),
            }));
            for n in members {
                mutations.push(DirectoryMutation::AddTeamMember {
                    team_id: team(t),
                    user_id: usr(*n),
                });
            }
        }
        ready(directory.commit(DirectoryCommit {
            org_id: scope().org_id,
            expected_revision: 0,
            mutations,
        }))
        .unwrap();
        let env = Self {
            store,
            directory,
            webhooks: MemoryWebhooks::new(START_MS),
            receiver: Receiver::default(),
            now: START_MS,
        };
        env.start(source, &[]);
        env
    }
    fn start(&self, source: &str, tags: &[&str]) {
        let rb: RunbookId = "rb_00000000000000000000000001".parse().unwrap();
        ready(self.store.create_runbook(
            RunbookRecord {
                runbook_id: rb.clone(),
                slug: "flagship".into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            source.into(),
        ))
        .unwrap();
        let context = runbook_spec::CompileContext {
            users: [("alice", 1u8), ("bob", 2), ("carol", 3), ("dave", 4)]
                .map(|(name, n)| (name.to_owned(), usr(n)))
                .into(),
            teams: [
                ("oncall".to_owned(), team(1)),
                ("eng-leads".to_owned(), team(2)),
                ("retired".to_owned(), team(3)),
            ]
            .into(),
            ..Default::default()
        };
        let prepared = prepare(
            "flagship",
            source,
            &context,
            "rbv_00000000000000000000000001".parse().unwrap(),
        )
        .unwrap();
        ready(publish_draft(&self.store, &self.store, &rb, 1, prepared)).unwrap();
        // Version 1 is published before any subscription exists.
        ready(webhooks::process_project_events(
            &self.store,
            &self.webhooks,
            &scope(),
            64,
        ))
        .unwrap();
        self.start_run(exe(), "k1", tags);
    }
    fn start_run(
        &self,
        id: ExecutionId,
        key: &str,
        tags: &[&str],
    ) -> runbook_core::start::StartOutcome {
        let command = StartCommand {
            url: format!("https://runbook.example/v1/executions/{id}"),
            execution_id: id,
            runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
            version: None,
            inputs: Data::new(),
            fields: Data::new(),
            project_vars: Data::new(),
            title: Some("Deploy api".into()),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            created_by: "alice".into(),
            limits: Default::default(),
            idempotency_key: key.into(),
            principal: "user:alice".into(),
            request_hash: [1; 32],
        };
        let commit = ready(prepare_start_commit(&self.store, &self.store, &command)).unwrap();
        ready(self.store.commit_start(commit)).unwrap()
    }
    fn set_time(&mut self, at: i64) {
        self.now = at;
        self.store.set_time(at);
        self.webhooks.set_time(at);
    }
    fn subscribe(
        &self,
        n: u8,
        events: &[&str],
        configure: impl FnOnce(&mut Subscription),
    ) -> String {
        let secret = webhooks::encode_webhook_secret([n; 32]);
        let mut subscription = Subscription {
            id: whk(n),
            scope: scope(),
            name: format!("hook {n}"),
            url: format!("https://hooks.example/{n}"),
            events: events.iter().map(|e| e.to_string()).collect(),
            runbooks: vec![],
            tags: vec![],
            enabled: true,
            secret: TestSealer.seal(whk(n).as_str(), &secret).unwrap(),
            previous_secret: None,
            created_at_ms: self.now,
            updated_at_ms: self.now,
            secret_rotated_at_ms: self.now,
            failing_since_ms: None,
            last_failure_at_ms: None,
            disabled_reason: None,
            disabled_at_ms: None,
            revision: 0,
            notice: None,
        };
        configure(&mut subscription);
        ready(self.webhooks.create_subscription(
            &scope(),
            subscription,
            webhooks::SUBSCRIPTION_LIMIT,
            None,
        ))
        .unwrap();
        secret
    }
    /// Drain every due lease and delivery until nothing is left.
    fn pump(&self) {
        for _ in 0..100 {
            let leases = ready(self.store.claim_due(64, 60_000)).unwrap();
            let mut busy = !leases.is_empty();
            busy |= ready(webhooks::process_project_events(
                &self.store,
                &self.webhooks,
                &scope(),
                64,
            ))
            .unwrap()
                > 0;
            for lease in &leases {
                match &lease.id.kind {
                    WorkKind::Notification { .. } => {
                        let done = ready(process_notification(
                            &self.store,
                            &self.webhooks,
                            scope(),
                            lease,
                        ))
                        .unwrap();
                        assert!(matches!(done, Processed::Notified | Processed::Obsolete));
                    }
                    _ => {
                        ready(process_lease(&self.store, &self.directory, scope(), lease)).unwrap();
                    }
                }
            }
            let stats = ready(webhooks::deliver_due(
                &self.webhooks,
                &self.receiver,
                &TestSealer,
                &scope(),
                DeliverySettings::default(),
            ))
            .unwrap();
            busy |= stats.claimed > 0;
            if !busy {
                return;
            }
        }
        panic!("work did not quiesce");
    }
    fn step(&self, step_id: &str) -> runbook_model::StepRunId {
        let w = ready(begin_actor(&self.store, &self.directory, scope(), &exe())).unwrap();
        w.runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == step_id)
            .and_then(|s| s.id.clone())
            .unwrap()
    }
    fn vote(&self, n: u8, step_id: &str, option: &str) {
        let step = self.step(step_id);
        let w = ready(begin_actor(&self.store, &self.directory, scope(), &exe())).unwrap();
        ready(vote(
            w,
            &ActorPrincipal {
                principal: format!("user:{}", usr(n)),
                actor: ActorRef::Human { user_id: usr(n) },
                credential_ref: "pat".into(),
            },
            &step,
            &DecisionRequest {
                option: option.into(),
                comment: None,
                session_id: None,
                actor: None,
            },
            None,
        ))
        .unwrap();
    }
}

const FLAGSHIP: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: flagship, title: Flagship}\n\
    spec:\n  agents: {params: {repo: acme/api, workflow: runbook-resume.yml, literal: '${{ untouched }}', n: 1.50}}\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Roll back?\", options: [rollback, keep], approvers: {users: [alice, bob]}, timeout: 1m, on_timeout: escalate, escalate_to: [\"user:carol\"], escalation_timeout: 1h}\n    \
    - id: rollback\n      title: Roll back\n      actor: agent\n";

fn types(received: &[(String, Value)]) -> Vec<String> {
    received.iter().map(|(t, _)| t.clone()).collect()
}

#[test]
fn two_subscriptions_receive_exactly_their_events() {
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &["approval.requested"], |_| {});
    env.subscribe(2, &["execution.started", "agent.work_available"], |_| {});
    env.subscribe(3, &["*"], |s| {
        s.runbooks = vec!["rb_00000000000000000000000009".parse().unwrap()]
    });
    env.subscribe(4, &["*"], |s| s.tags = vec!["prod".into()]);
    env.pump();
    let one = env.receiver.received("https://hooks.example/1");
    let two = env.receiver.received("https://hooks.example/2");
    assert_eq!(types(&one), ["approval.requested"]);
    assert_eq!(types(&two), ["execution.started"]);
    assert!(env.receiver.received("https://hooks.example/3").is_empty());
    assert!(env.receiver.received("https://hooks.example/4").is_empty());
    env.vote(1, "gate", "rollback");
    env.pump();
    let two = env.receiver.received("https://hooks.example/2");
    assert_eq!(types(&two), ["execution.started", "agent.work_available"]);
    assert_eq!(
        types(&env.receiver.received("https://hooks.example/1")).len(),
        1
    );
    // Every body is the thin v1 envelope.
    let (_, body) = &two[1];
    for key in [
        "id",
        "type",
        "version",
        "created_at",
        "org",
        "project",
        "execution",
        "sequence",
        "data",
    ] {
        assert!(body.get(key).is_some(), "{key} missing: {body}");
    }
    assert_eq!(body["version"], 1);
    assert_eq!(body["execution"]["id"], exe().as_str());
    assert_eq!(body["execution"]["runbook_version"], 1);
    assert!(body["id"].as_str().unwrap().starts_with("evt_"));
}

#[test]
fn tag_and_runbook_filters_select_matching_runs() {
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &["execution.started"], |s| {
        s.runbooks = vec!["rb_00000000000000000000000001".parse().unwrap()]
    });
    env.subscribe(2, &["execution.started"], |s| {
        s.tags = vec!["staging".into()]
    });
    env.pump();
    assert_eq!(env.receiver.received("https://hooks.example/1").len(), 1);
    assert!(env.receiver.received("https://hooks.example/2").is_empty());
}

#[test]
fn alice_and_bob_receive_the_request_and_carol_the_escalation() {
    let mut env = Env::new(FLAGSHIP);
    env.subscribe(1, &["approval.requested", "approval.escalated"], |_| {});
    env.pump();
    let received = env.receiver.received("https://hooks.example/1");
    assert_eq!(types(&received), ["approval.requested"]);
    let recipients = &received[0].1["data"]["recipients"];
    assert_eq!(
        recipients,
        &serde_json::json!([
            {"type":"user","id":usr(1)},
            {"type":"user","id":usr(2)},
        ])
    );
    assert_eq!(
        received[0].1["data"]["step_run_id"],
        env.step("gate").as_str()
    );
    env.set_time(START_MS + 60_000);
    env.pump();
    let received = env.receiver.received("https://hooks.example/1");
    assert_eq!(
        types(&received),
        ["approval.requested", "approval.escalated"]
    );
    assert_eq!(
        received[1].1["data"]["recipients"],
        serde_json::json!([{"type":"user","id":usr(3)}])
    );
}

/// The flagship's team routing: on-call approves, `eng-leads` is escalated
/// to, and the run pauses when the escalation expires.
const TEAMS: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: flagship, title: Flagship}\n\
    spec:\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Roll back?\", options: [rollback, keep], approvers: {teams: [oncall]}, timeout: 30m, on_timeout: escalate, escalate_to: [\"team:eng-leads\"], escalation_timeout: 30m}\n    \
    - id: rollback\n      title: Roll back\n      actor: agent\n";

#[test]
fn escalation_and_expiry_name_the_escalation_teams_active_users() {
    let mut env = Env::new(TEAMS);
    env.subscribe(
        1,
        &[
            "approval.requested",
            "approval.escalated",
            "approval.expired",
        ],
        |_| {},
    );
    env.pump();
    env.set_time(START_MS + 30 * 60_000);
    env.pump();
    env.set_time(START_MS + 60 * 60_000);
    env.pump();
    let received = env.receiver.ordered("https://hooks.example/1");
    assert_eq!(
        types(&received),
        [
            "approval.requested",
            "approval.escalated",
            "approval.expired"
        ]
    );
    let user = |n: u8| serde_json::json!({"type":"user","id":usr(n)});
    // Teams expand to their active users for every human approval event;
    // deactivated Dave is in both teams and is never named.
    assert_eq!(
        received[0].1["data"]["recipients"],
        serde_json::json!([user(1), user(2)])
    );
    for (_, body) in &received[1..] {
        assert_eq!(
            body["data"]["recipients"],
            serde_json::json!([user(3)]),
            "{body}"
        );
        // The escalation team stays identifiable.
        assert_eq!(
            body["data"]["targets"],
            serde_json::json!([{"type":"team","id":team(2)}]),
            "{body}"
        );
        assert_eq!(body["data"]["step_run_id"], env.step("gate").as_str());
    }
}

/// A deactivated user named explicitly, and an escalation team with no
/// active member.
const SPARSE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: flagship, title: Flagship}\n\
    spec:\n  steps:\n    \
    - id: gate\n      approval: {prompt: \"Roll back?\", options: [rollback, keep], approvers: {users: [alice, dave]}, timeout: 30m, on_timeout: escalate, escalate_to: [\"user:dave\", \"team:retired\"], escalation_timeout: 30m}\n    \
    - id: rollback\n      title: Roll back\n      actor: agent\n";

#[test]
fn deactivated_users_are_never_named_and_an_empty_team_stays_visible() {
    let mut env = Env::new(SPARSE);
    env.subscribe(
        1,
        &[
            "approval.requested",
            "approval.escalated",
            "approval.expired",
        ],
        |_| {},
    );
    env.pump();
    env.set_time(START_MS + 30 * 60_000);
    env.pump();
    env.set_time(START_MS + 60 * 60_000);
    env.pump();
    let received = env.receiver.ordered("https://hooks.example/1");
    assert_eq!(
        types(&received),
        [
            "approval.requested",
            "approval.escalated",
            "approval.expired"
        ]
    );
    // Explicitly named Dave is deactivated: only Alice is named.
    assert_eq!(
        received[0].1["data"]["recipients"],
        serde_json::json!([{"type":"user","id":usr(1)}])
    );
    // The escalation reaches no active user: Dave is dropped and the empty
    // team stays the recipient instead of an empty list.
    for (_, body) in &received[1..] {
        assert_eq!(
            body["data"]["recipients"],
            serde_json::json!([{"type":"team","id":team(3)}]),
            "{body}"
        );
    }
}

#[test]
fn work_available_follows_the_decision_with_untouched_params() {
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &["agent.work_available", "approval.decided"], |_| {});
    env.pump();
    assert!(env.receiver.received("https://hooks.example/1").is_empty());
    env.vote(2, "gate", "rollback");
    env.pump();
    let received = env.receiver.ordered("https://hooks.example/1");
    assert_eq!(
        types(&received),
        ["approval.decided", "agent.work_available"]
    );
    assert_eq!(received[0].1["data"]["option"], "rollback");
    let data = &received[1].1["data"];
    assert_eq!(data["reason"], "approval_decided");
    assert_eq!(data["ready_steps"][0]["path"], "rollback");
    assert_eq!(data["ready_steps"][0]["title"], "Roll back");
    // spec.agents.params passes through untouched, including the literal
    // expression text and the exact decimal.
    let raw = env
        .receiver
        .seen
        .borrow()
        .iter()
        .find(|r| header(r, "Runbook-Event-Type") == "agent.work_available")
        .unwrap()
        .body
        .clone();
    assert!(
        raw.contains(r#""params":{"literal":"${{ untouched }}","n":1.50,"repo":"acme/api","workflow":"runbook-resume.yml"}"#),
        "{raw}"
    );
    assert!(
        received[1].1["sequence"].as_u64().unwrap() > received[0].1["sequence"].as_u64().unwrap()
    );
}

#[test]
fn signatures_verify_and_fail_after_tampering_or_when_stale() {
    let env = Env::new(FLAGSHIP);
    let secret = env.subscribe(1, &["*"], |_| {});
    env.pump();
    let request = env.receiver.seen.borrow()[0].clone();
    let timestamp = header(&request, "Runbook-Timestamp");
    let signature = header(&request, "Runbook-Signature");
    let now = START_MS / 1000;
    assert_eq!(timestamp, now.to_string());
    assert!(signature.starts_with("v1=") && !signature.contains(','));
    let verify = |body: &str, ts: &str, at: i64| {
        webhooks::verify_signature(&secret, ts, body.as_bytes(), &signature, at, 300)
    };
    assert_eq!(verify(&request.body, &timestamp, now + 10), Ok(()));
    let tampered = request.body.replacen("approval", "approvaI", 1);
    assert_eq!(
        verify(&tampered, &timestamp, now),
        Err(webhooks::SignatureError::Mismatch)
    );
    assert_eq!(
        verify(&request.body, &(now - 1).to_string(), now),
        Err(webhooks::SignatureError::Mismatch)
    );
    assert_eq!(
        verify(&request.body, &timestamp, now + 301),
        Err(webhooks::SignatureError::Stale)
    );
    assert_eq!(
        webhooks::verify_signature(
            &webhooks::encode_webhook_secret([9; 32]),
            &timestamp,
            request.body.as_bytes(),
            &signature,
            now,
            300
        ),
        Err(webhooks::SignatureError::Mismatch)
    );
    for name in [
        "Runbook-Event-Id",
        "Runbook-Event-Type",
        "Runbook-Delivery-Id",
    ] {
        assert!(!header(&request, name).is_empty(), "{name}");
    }
    let body: Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(
        header(&request, "Runbook-Event-Id"),
        body["id"].as_str().unwrap()
    );
}

#[test]
fn crash_after_acceptance_redelivers_the_same_event_id() {
    let mut env = Env::new(FLAGSHIP);
    env.subscribe(1, &["execution.started"], |_| {});
    // Activation and fan-out only; the delivery is then attempted by hand.
    for lease in ready(env.store.claim_due(64, 60_000)).unwrap() {
        ready(process_lease(&env.store, &env.directory, scope(), &lease)).unwrap();
    }
    for lease in ready(env.store.claim_due(64, 60_000)).unwrap() {
        ready(process_notification(
            &env.store,
            &env.webhooks,
            scope(),
            &lease,
        ))
        .unwrap();
    }
    env.webhooks.fail_next_completion();
    let first = ready(webhooks::deliver_due(
        &env.webhooks,
        &env.receiver,
        &TestSealer,
        &scope(),
        DeliverySettings::default(),
    ))
    .unwrap();
    assert_eq!((first.claimed, first.errors), (1, 1));
    // Restarted worker: nothing until the lease expires, then the same event.
    env.pump();
    assert_eq!(env.receiver.seen.borrow().len(), 1);
    env.set_time(START_MS + 60_001);
    env.pump();
    let seen = env.receiver.seen.borrow().clone();
    assert_eq!(seen.len(), 2);
    for name in ["Runbook-Event-Id", "Runbook-Delivery-Id"] {
        assert_eq!(header(&seen[0], name), header(&seen[1], name));
    }
    assert_eq!(seen[0].body, seen[1].body);
    // Delivered now; never sent a third time.
    env.set_time(START_MS + 600_000);
    env.pump();
    assert_eq!(env.receiver.seen.borrow().len(), 2);
    let delivered = ready(env.webhooks.deliveries(&scope(), &whk(1), 10, None)).unwrap();
    assert_eq!(delivered[0].0.state, DeliveryState::Delivered);
}

#[test]
fn rotation_signs_with_both_secrets_for_a_day_then_only_the_new_one() {
    let mut env = Env::new(FLAGSHIP);
    let old = env.subscribe(
        1,
        &[
            "execution.started",
            "approval.escalated",
            "approval.decided",
        ],
        |_| {},
    );
    let new = webhooks::encode_webhook_secret([77; 32]);
    let mut subscription = ready(env.webhooks.get_subscription(&scope(), &whk(1)))
        .unwrap()
        .unwrap();
    subscription.rotate(TestSealer.seal(whk(1).as_str(), &new).unwrap(), START_MS);
    ready(env.webhooks.save_subscription(&scope(), subscription, None)).unwrap();
    env.pump();
    let check = |request: &OutgoingRequest, secret: &str| {
        let ts = header(request, "Runbook-Timestamp");
        webhooks::verify_signature(
            secret,
            &ts,
            request.body.as_bytes(),
            &header(request, "Runbook-Signature"),
            ts.parse().unwrap(),
            300,
        )
    };
    let first = env.receiver.seen.borrow()[0].clone();
    assert_eq!(
        header(&first, "Runbook-Signature").matches("v1=").count(),
        2
    );
    assert_eq!(check(&first, &new), Ok(()));
    assert_eq!(check(&first, &old), Ok(()));
    // The escalation fires at one minute, still inside the overlap.
    env.set_time(START_MS + 60_000);
    env.pump();
    let second = env.receiver.seen.borrow()[1].clone();
    assert_eq!(check(&second, &old), Ok(()));
    env.set_time(START_MS + 86_400_000 + 1_000);
    env.vote(1, "gate", "rollback");
    env.pump();
    let last = env.receiver.seen.borrow().last().unwrap().clone();
    assert_eq!(header(&last, "Runbook-Event-Type"), "approval.decided");
    assert_eq!(header(&last, "Runbook-Signature").matches("v1=").count(), 1);
    assert_eq!(check(&last, &new), Ok(()));
    assert_eq!(check(&last, &old), Err(webhooks::SignatureError::Mismatch));
}

#[test]
fn retries_back_off_and_fail_by_status_class() {
    let mut env = Env::new(FLAGSHIP);
    env.subscribe(1, &["execution.started"], |_| {});
    env.receiver.respond(TransportResult::Response {
        status: 503,
        latency_ms: 10,
        snippet: "down".into(),
    });
    env.receiver
        .respond(TransportResult::Timeout { latency_ms: 10_000 });
    env.receiver.respond(TransportResult::Response {
        status: 429,
        latency_ms: 10,
        snippet: String::new(),
    });
    env.pump();
    let pending =
        |env: &Env| ready(env.webhooks.deliveries(&scope(), &whk(1), 10, None)).unwrap()[0].clone();
    let (record, attempts) = pending(&env);
    assert_eq!(record.state, DeliveryState::Pending);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, Some(503));
    assert_eq!(record.next_attempt_at_ms, Some(START_MS + 10 + 30_000));
    env.set_time(START_MS + 30_010);
    env.pump();
    let (record, _) = pending(&env);
    assert_eq!(
        record.next_attempt_at_ms,
        Some(START_MS + 30_010 + 10_000 + 60_000)
    );
    env.set_time(record.next_attempt_at_ms.unwrap());
    env.pump();
    env.set_time(pending(&env).0.next_attempt_at_ms.unwrap());
    env.pump();
    let (record, attempts) = pending(&env);
    assert_eq!(record.state, DeliveryState::Delivered);
    assert_eq!(attempts.len(), 4);

    // Any other 4xx fails at once; 3xx too (redirects are not followed).
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &["execution.started"], |_| {});
    env.receiver.respond(TransportResult::Response {
        status: 404,
        latency_ms: 3,
        snippet: "nope".into(),
    });
    env.pump();
    let (record, attempts) = pending(&env);
    assert_eq!(record.state, DeliveryState::Failed);
    assert_eq!(record.failure.as_deref(), Some("http_404"));
    assert_eq!(attempts.len(), 1);

    // Retries stop 24 hours after the delivery was created.
    let mut env = Env::new(FLAGSHIP);
    env.subscribe(1, &["execution.started"], |_| {});
    for _ in 0..64 {
        env.receiver.respond(TransportResult::Failed {
            error: "connect",
            latency_ms: 1,
        });
    }
    let mut guard = 0;
    loop {
        env.pump();
        let (record, _) = pending(&env);
        let Some(next) = record.next_attempt_at_ms else {
            break;
        };
        assert!(next <= START_MS + 86_400_000);
        env.set_time(next);
        guard += 1;
        assert!(guard < 64);
    }
    let (record, attempts) = pending(&env);
    assert_eq!(record.state, DeliveryState::Failed);
    assert_eq!(record.failure.as_deref(), Some("retries_exhausted"));
    assert!(
        attempts.len() > 10 && attempts.len() < 40,
        "{}",
        attempts.len()
    );
}

#[test]
fn three_days_of_continuous_failure_disable_the_subscription() {
    let mut env = Env::new(FLAGSHIP);
    env.subscribe(1, &["*"], |_| {});
    for _ in 0..200 {
        env.receiver.respond(TransportResult::Response {
            status: 500,
            latency_ms: 1,
            snippet: String::new(),
        });
    }
    env.pump();
    let at = |env: &Env| {
        ready(env.webhooks.get_subscription(&scope(), &whk(1)))
            .unwrap()
            .unwrap()
    };
    assert_eq!(at(&env).failing_since_ms, Some(START_MS));
    // A test delivery two days in keeps failing but does not yet disable.
    env.set_time(START_MS + 2 * 86_400_000);
    let event = webhooks::test_event(&scope(), &at(&env), "evt_test".into(), env.now).unwrap();
    let delivery = webhooks::new_delivery(
        "whd_00000000000000000000000001".parse().unwrap(),
        &at(&env),
        &event,
        None,
        env.now,
    );
    ready(
        env.webhooks
            .record_event(&scope(), event, vec![delivery], None),
    )
    .unwrap();
    env.pump();
    assert!(at(&env).enabled);
    env.set_time(START_MS + 3 * 86_400_000);
    let event = webhooks::test_event(&scope(), &at(&env), "evt_test2".into(), env.now).unwrap();
    let delivery = webhooks::new_delivery(
        "whd_00000000000000000000000002".parse().unwrap(),
        &at(&env),
        &event,
        None,
        env.now,
    );
    ready(
        env.webhooks
            .record_event(&scope(), event, vec![delivery], None),
    )
    .unwrap();
    env.pump();
    let disabled = at(&env);
    assert!(!disabled.enabled);
    assert_eq!(
        disabled.disabled_reason.as_deref(),
        Some("delivery_failing")
    );
    assert_eq!(disabled.disabled_at_ms, Some(START_MS + 3 * 86_400_000));
    // The durable admin notice names when, since when and how it failed.
    let notice = disabled.notice.clone().expect("admin notice");
    assert_eq!(notice.kind, "webhook.disabled");
    assert_eq!(notice.at_ms, START_MS + 3 * 86_400_000);
    assert_eq!(notice.failing_since_ms, START_MS);
    assert_eq!(notice.last_failure.as_deref(), Some("http_500"));
    assert!(notice.message.contains("hook 1"), "{}", notice.message);
    assert_eq!(disabled.view().admin_notice, Some(notice));
    // Further failures leave it as recorded; re-enabling acknowledges it.
    let mut again = disabled.clone();
    again.record_attempt(false, START_MS + 4 * 86_400_000, Some("timeout"));
    assert_eq!(again.notice, disabled.notice);
    again.reenable();
    assert!(again.enabled && again.notice.is_none() && again.failing_since_ms.is_none());
    assert_eq!(again.view().admin_notice, None);
}

#[test]
fn no_secret_material_in_bodies_or_delivery_logs() {
    let mut env = Env::new(FLAGSHIP);
    let secret = env.subscribe(1, &["*"], |_| {});
    let sealed = ready(env.webhooks.get_subscription(&scope(), &whk(1)))
        .unwrap()
        .unwrap()
        .secret
        .0;
    env.receiver.respond(TransportResult::Response {
        status: 500,
        latency_ms: 1,
        snippet: format!("echo Authorization: Bearer {secret} token=trb_pat_x"),
    });
    env.pump();
    env.set_time(START_MS + 120_000);
    env.vote(1, "gate", "rollback");
    env.pump();
    let needle = secret.trim_start_matches("whsec_");
    for request in env.receiver.seen.borrow().iter() {
        assert!(!request.body.contains(needle) && !request.body.contains("whsec_"));
        assert!(!request.body.contains(&sealed));
        for (_, value) in &request.headers {
            assert!(!value.contains(needle) && !value.contains(&sealed));
        }
    }
    let log = ready(env.webhooks.deliveries(&scope(), &whk(1), 100, None)).unwrap();
    let rendered =
        serde_json::to_string(&log.iter().map(|(d, a)| d.view(a)).collect::<Vec<_>>()).unwrap();
    assert!(
        !rendered.contains(needle) && !rendered.contains("whsec_"),
        "{rendered}"
    );
    assert!(rendered.contains("[REDACTED"), "{rendered}");
    let view = serde_json::to_string(
        &ready(env.webhooks.get_subscription(&scope(), &whk(1)))
            .unwrap()
            .unwrap()
            .view(),
    )
    .unwrap();
    assert!(!view.contains(needle) && !view.contains(&sealed) && !view.contains("sealed:"));
    assert!(
        !format!(
            "{:?}",
            ready(env.webhooks.get_subscription(&scope(), &whk(1)))
        )
        .contains(&sealed)
    );
}

#[test]
fn undelivered_human_events_are_marked_on_the_execution() {
    // No subscriber at all.
    let env = Env::new(FLAGSHIP);
    env.pump();
    let marked = ready(webhooks::undelivered_for(&env.webhooks, &scope(), &exe())).unwrap();
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert_eq!(marked[0].event_type, "approval.requested");
    assert_eq!(marked[0].reason, "no_subscriber");
    assert_eq!(marked[0].step_run_id.as_ref(), Some(&env.step("gate")));

    // A subscriber that rejects it.
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &["approval.requested"], |_| {});
    env.receiver.respond(TransportResult::Response {
        status: 410,
        latency_ms: 1,
        snippet: String::new(),
    });
    env.pump();
    let marked = ready(webhooks::undelivered_for(&env.webhooks, &scope(), &exe())).unwrap();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].reason, "delivery_failed");

    // Pending is not yet a verdict; a successful delivery clears it.
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &["approval.requested"], |_| {});
    env.receiver.respond(TransportResult::Response {
        status: 502,
        latency_ms: 1,
        snippet: String::new(),
    });
    env.pump();
    assert!(
        ready(webhooks::undelivered_for(&env.webhooks, &scope(), &exe()))
            .unwrap()
            .is_empty()
    );
    let delivered = Env::new(FLAGSHIP);
    delivered.subscribe(1, &["approval.requested"], |_| {});
    delivered.pump();
    assert!(
        ready(webhooks::undelivered_for(
            &delivered.webhooks,
            &scope(),
            &exe()
        ))
        .unwrap()
        .is_empty()
    );
    // Non-human events never count.
    assert!(
        webhooks::HUMAN_EVENTS
            .iter()
            .all(|e| !e.starts_with("execution."))
    );
}

#[test]
fn fan_out_is_idempotent_across_a_crash_before_acknowledgement() {
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &["execution.started"], |_| {});
    for lease in ready(env.store.claim_due(64, 60_000)).unwrap() {
        ready(process_lease(&env.store, &env.directory, scope(), &lease)).unwrap();
    }
    let leases = ready(env.store.claim_due(64, 60_000)).unwrap();
    let started = leases
        .iter()
        .find(|l| matches!(&l.id.kind, WorkKind::Notification { .. }))
        .unwrap()
        .clone();
    // Two workers process the same lease (the first "crashed" before its ack).
    for _ in 0..2 {
        ready(process_notification(
            &env.store,
            &env.webhooks,
            scope(),
            &started,
        ))
        .unwrap();
    }
    for lease in &leases {
        ready(process_notification(
            &env.store,
            &env.webhooks,
            scope(),
            lease,
        ))
        .unwrap();
    }
    env.pump();
    assert_eq!(env.receiver.received("https://hooks.example/1").len(), 1);
}

#[test]
fn targets_reject_private_and_plain_http_unless_self_hosted() {
    use webhooks::{TargetPolicy, parse_target};
    let cloud = TargetPolicy::default();
    let self_hosted = TargetPolicy {
        allow_private: true,
        allow_link_local: false,
    };
    assert!(parse_target("https://hooks.example.com/x", cloud).is_ok());
    for (url, code) in [
        ("http://hooks.example.com/x", "webhooks.url_https_required"),
        ("https://127.0.0.1/x", "webhooks.target_forbidden"),
        ("https://10.0.0.5/x", "webhooks.target_forbidden"),
        (
            "https://169.254.169.254/latest",
            "webhooks.target_forbidden",
        ),
        ("https://[::1]:8443/x", "webhooks.target_forbidden"),
        (
            "https://[::ffff:192.168.0.1]/x",
            "webhooks.target_forbidden",
        ),
        ("https://[2002:7f00:1::1]/x", "webhooks.target_forbidden"),
        (
            "https://[2001:0:4136:e378::1]/x",
            "webhooks.target_forbidden",
        ),
        ("https://localhost/x", "webhooks.target_forbidden"),
        ("https://0.0.0.0/x", "webhooks.target_forbidden"),
        (
            "https://user:pw@hooks.example.com/x",
            "webhooks.url_credentials",
        ),
        ("ftp://hooks.example.com/x", "webhooks.url_invalid"),
        ("https://hooks.example.com/x#frag", "webhooks.url_invalid"),
        ("not a url", "webhooks.url_invalid"),
    ] {
        assert_eq!(parse_target(url, cloud).unwrap_err(), code, "{url}");
    }
    let local = parse_target("http://127.0.0.1:9000/hook", self_hosted).unwrap();
    assert_eq!((local.https, local.port), (false, 9000));
    assert!(parse_target("https://[::1]/x", self_hosted).is_ok());
    // Self-hosted still keeps cloud metadata and other link-local targets
    // out unless link-local is allowed separately.
    for url in [
        "http://169.254.169.254/latest/meta-data",
        "https://[fe80::1]/x",
        "https://[::ffff:169.254.169.254]/x",
    ] {
        assert_eq!(
            parse_target(url, self_hosted).unwrap_err(),
            "webhooks.target_forbidden",
            "{url}"
        );
        assert!(
            parse_target(
                url,
                TargetPolicy {
                    allow_private: true,
                    allow_link_local: true
                }
            )
            .is_ok(),
            "{url}"
        );
    }
    for ip in [
        "169.254.169.254",
        "fe80::1",
        "10.0.0.1",
        "2002:a00:1::1",
        "2001:0:4136:e378::1",
    ] {
        let ip = ip.parse().unwrap();
        assert!(!webhooks::address_allowed(ip, cloud), "{ip}");
    }
    assert!(webhooks::address_allowed(
        "10.0.0.1".parse().unwrap(),
        self_hosted
    ));
    assert!(!webhooks::address_allowed(
        "169.254.169.254".parse().unwrap(),
        self_hosted
    ));
    assert!(!webhooks::address_allowed(
        "fe80::1".parse().unwrap(),
        self_hosted
    ));
}

/// I1: each delivery is claimed under its own lease, so a receiver that
/// takes most of the timeout on every attempt never pushes a later attempt
/// past its lease: every result is recorded and nothing is re-sent.
#[test]
fn a_slow_receiver_never_outlives_a_delivery_lease() {
    struct Slow {
        inner: Receiver,
        webhooks: MemoryWebhooks,
        clock: std::cell::Cell<i64>,
    }
    impl WebhookTransport for Slow {
        async fn post(&self, request: OutgoingRequest) -> TransportResult {
            // Nine seconds per attempt, on the store clock.
            self.clock.set(self.clock.get() + 9_000);
            self.webhooks.set_time(self.clock.get());
            self.inner.post(request).await
        }
    }
    let env = Env::new(FLAGSHIP);
    env.subscribe(1, &[], |_| {});
    let subscription = ready(env.webhooks.get_subscription(&scope(), &whk(1)))
        .unwrap()
        .unwrap();
    for n in 0..12u32 {
        let event =
            webhooks::test_event(&scope(), &subscription, format!("evt_slow{n}"), env.now).unwrap();
        let delivery = webhooks::new_delivery(
            format!("whd_{:026}", n + 1).parse().unwrap(),
            &subscription,
            &event,
            None,
            env.now,
        );
        ready(
            env.webhooks
                .record_event(&scope(), event, vec![delivery], None),
        )
        .unwrap();
    }
    let slow = Slow {
        inner: env.receiver.clone(),
        webhooks: env.webhooks.clone(),
        clock: std::cell::Cell::new(env.now),
    };
    let stats = ready(webhooks::deliver_due(
        &env.webhooks,
        &slow,
        &TestSealer,
        &scope(),
        DeliverySettings {
            batch: 16,
            lease_ms: 30_000,
        },
    ))
    .unwrap();
    assert_eq!((stats.delivered, stats.errors), (12, 0), "{stats:?}");
    let ids: std::collections::BTreeSet<String> = env
        .receiver
        .seen
        .borrow()
        .iter()
        .map(|r| header(r, "Runbook-Event-Id"))
        .collect();
    assert_eq!((env.receiver.seen.borrow().len(), ids.len()), (12, 12));
    for (record, attempts) in ready(env.webhooks.deliveries(&scope(), &whk(1), 50, None)).unwrap() {
        assert_eq!(record.state, DeliveryState::Delivered);
        assert_eq!((record.attempts, attempts.len()), (1, 1));
    }
}

const POOLED: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: flagship, title: Flagship}\n\
    spec:\n  execution: {timeout: 2h, concurrency: {key: \"${{ inputs.service }}\", limit: 1, on_conflict: queue}}\n  \
    inputs: {service: {type: string, default: api}}\n  \
    steps: [{id: only, title: Only}]\n";

fn verified(env: &Env, secret: &str, event_type: &str) -> Vec<Value> {
    env.receiver
        .seen
        .borrow()
        .iter()
        .filter(|r| header(r, "Runbook-Event-Type") == event_type)
        .map(|r| {
            assert_eq!(
                webhooks::verify_signature(
                    secret,
                    &header(r, "Runbook-Timestamp"),
                    r.body.as_bytes(),
                    &header(r, "Runbook-Signature"),
                    env.now / 1000,
                    webhooks::SIGNATURE_TOLERANCE_S,
                ),
                Ok(())
            );
            serde_json::from_str(&r.body).unwrap()
        })
        .collect()
}

#[test]
fn a_start_queued_behind_its_key_emits_execution_queued_in_its_commit() {
    let env = Env::new(POOLED);
    let secret = env.subscribe(1, &["execution.queued", "execution.started"], |_| {});
    let second: ExecutionId = "exe_00000000000000000000000002".parse().unwrap();
    let outcome = env.start_run(second.clone(), "k2", &[]);
    assert_eq!(
        outcome.admission.map(|a| a.state),
        Some(runbook_core::catalog::AdmissionState::Queued)
    );
    // Durable with the start itself, before any worker ran.
    let queued = ready(runbook_core::ExecutionStore::load(&env.store, &second)).unwrap();
    assert_eq!(queued.revision, 0);
    let intents: Vec<_> = queued.outbox.values().collect();
    assert_eq!(intents.len(), 1, "{intents:?}");
    assert_eq!(intents[0].kind, "execution.queued");
    assert_eq!(intents[0].sequence, 0);
    // The admitted run is not queued, and a replayed start writes nothing.
    let first = ready(runbook_core::ExecutionStore::load(&env.store, &exe())).unwrap();
    assert!(first.outbox.values().all(|i| i.kind != "execution.queued"));
    env.start_run(second.clone(), "k2", &[]);
    assert_eq!(
        ready(runbook_core::ExecutionStore::load(&env.store, &second))
            .unwrap()
            .outbox
            .len(),
        1
    );
    env.pump();
    let bodies = verified(&env, &secret, "execution.queued");
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert_eq!(body["execution"]["id"], second.as_str());
    assert_eq!(body["execution"]["runbook_version"], 1);
    assert_eq!(body["sequence"], 0);
    assert_eq!(body["data"]["status"], "queued");
    assert_eq!(body["data"]["concurrency_key"], "api");
    assert_eq!(body["id"], intents[0].event_id.as_str());
    // Only the admitted run started.
    let started = verified(&env, &secret, "execution.started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["execution"]["id"], exe().as_str());
}

#[test]
fn publishing_a_new_version_emits_runbook_published_without_source() {
    let env = Env::new(FLAGSHIP);
    let secret = env.subscribe(1, &["runbook.published"], |_| {});
    env.subscribe(2, &["runbook.published"], |s| {
        s.runbooks = vec!["rb_00000000000000000000000009".parse().unwrap()]
    });
    env.subscribe(3, &["*"], |s| s.tags = vec!["prod".into()]);
    env.pump();
    assert!(verified(&env, &secret, "runbook.published").is_empty());
    let rb: RunbookId = "rb_00000000000000000000000001".parse().unwrap();
    let source = FLAGSHIP.replace("title: Roll back", "title: Roll back now");
    ready(env.store.save_draft(&rb, 1, source.clone())).unwrap();
    let publish = |version: &str| {
        let prepared = prepare(
            "flagship",
            &source,
            &runbook_spec::CompileContext {
                users: [("alice", 1u8), ("bob", 2), ("carol", 3)]
                    .map(|(name, n)| (name.to_owned(), usr(n)))
                    .into(),
                ..Default::default()
            },
            version.parse().unwrap(),
        )
        .unwrap();
        ready(publish_draft(&env.store, &env.store, &rb, 2, prepared)).unwrap()
    };
    let v2 = publish("rbv_00000000000000000000000002");
    assert_eq!(v2.runbook_version.get(), 2);
    // Durable in the publish commit; a semantic duplicate adds nothing.
    let pending = ready(webhooks::ProjectOutbox::pending_project_events(
        &env.store, 10,
    ))
    .unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].event_type, "runbook.published");
    // A webhook created after the publish, while its event still waits in
    // the outbox, does not receive it when the drain catches up.
    let at_ms = pending[0].at_ms;
    env.subscribe(4, &["runbook.published"], |s| s.created_at_ms = at_ms + 1);
    let duplicate = publish("rbv_00000000000000000000000003");
    assert_eq!(duplicate, v2);
    assert_eq!(
        ready(webhooks::ProjectOutbox::pending_project_events(
            &env.store, 10
        ))
        .unwrap()
        .len(),
        1
    );
    env.pump();
    assert!(
        ready(webhooks::ProjectOutbox::pending_project_events(
            &env.store, 10
        ))
        .unwrap()
        .is_empty()
    );
    let bodies = verified(&env, &secret, "runbook.published");
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert_eq!(body["id"], pending[0].event_id.as_str());
    assert_eq!(body["execution"], Value::Null);
    assert_eq!(body["sequence"], 0);
    assert_eq!(
        body["data"],
        serde_json::json!({
            "runbook": rb,
            "runbook_slug": "flagship",
            "runbook_version_id": "rbv_00000000000000000000000002",
            "runbook_version": 2,
        })
    );
    let raw = &env
        .receiver
        .seen
        .borrow()
        .iter()
        .find(|r| header(r, "Runbook-Event-Type") == "runbook.published")
        .unwrap()
        .body
        .clone();
    assert!(
        !raw.contains("apiVersion") && !raw.contains("Roll back"),
        "{raw}"
    );
    // Runbook and tag filters apply.
    assert!(env.receiver.received("https://hooks.example/2").is_empty());
    assert!(env.receiver.received("https://hooks.example/3").is_empty());
    assert!(env.receiver.received("https://hooks.example/4").is_empty());
}

#[test]
fn the_delivery_log_is_purged_after_ninety_days_keeping_human_verdicts() {
    let mut env = Env::new(FLAGSHIP);
    for n in 1..=3 {
        env.subscribe(n, &["execution.started", "approval.requested"], |_| {});
    }
    env.pump();
    let log = |env: &Env| {
        (1..=3)
            .flat_map(|n| ready(env.webhooks.deliveries(&scope(), &whk(n), 100, None)).unwrap())
            .collect::<Vec<_>>()
    };
    let human = |event_type: &str| webhooks::HUMAN_EVENTS.contains(&event_type);
    let before = log(&env);
    assert_eq!(before.len(), 6);
    assert!(
        before
            .iter()
            .all(|(d, a)| d.state == DeliveryState::Delivered && a.len() == 1)
    );
    // Nothing is due before the 90 days are up.
    env.set_time(START_MS + webhooks::DELIVERY_LOG_RETENTION_MS);
    assert_eq!(
        ready(webhooks::purge_expired_log(&env.webhooks, &scope(), 500)).unwrap(),
        0
    );
    env.set_time(START_MS + webhooks::DELIVERY_LOG_RETENTION_MS + 1);
    // One bounded batch removes at most `limit` deliveries.
    let cutoff = env.now - webhooks::DELIVERY_LOG_RETENTION_MS;
    assert!(ready(env.webhooks.purge_delivery_log(&scope(), cutoff, 1)).unwrap() > 0);
    let after_one = log(&env);
    assert_eq!(
        after_one
            .iter()
            .filter(|(d, _)| !human(&d.event_type))
            .count(),
        2
    );
    ready(webhooks::purge_expired_log(&env.webhooks, &scope(), 500)).unwrap();
    let after = log(&env);
    // Non-human deliveries are gone; human deliveries stay as the verdict,
    // without their attempt detail.
    assert_eq!(after.len(), 3, "{after:?}");
    assert!(
        after
            .iter()
            .all(|(d, a)| human(&d.event_type) && a.is_empty())
    );
    assert!(
        ready(webhooks::undelivered_for(&env.webhooks, &scope(), &exe()))
            .unwrap()
            .is_empty(),
        "the human event still counts as notified"
    );
    assert_eq!(
        ready(webhooks::purge_expired_log(&env.webhooks, &scope(), 500)).unwrap(),
        0
    );
}
