//! B7a memory-side credential lifetimes at exact boundaries (fake clock).
use runbook_core::{
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
    credentials::{
        CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind,
        memory::MemoryCredentials, secret_hash,
    },
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, UserRecord,
        memory::MemoryDirectory,
    },
    execution_credentials::{
        ExecutionCredentialError, MintEntropy, mint_execution_pair, refresh_execution_pair,
        verify_execution_token,
    },
    memory::MemoryExecutionStore,
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::WorkQueue,
};
use runbook_model::{Data, ProjectRole, RefreshRequest, TokenId};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory store must be synchronous"),
    }
}
fn scope() -> TenantScope {
    TenantScope {
        org_id: "org_00000000000000000000000001".parse().unwrap(),
        project_id: "prj_00000000000000000000000001".parse().unwrap(),
    }
}
const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: cred-check, title: Cred check}\n\
    spec: {steps: [{id: only, title: Only}]}\n";
const START: i64 = 1_000;

fn setup() -> (
    MemoryExecutionStore,
    MemoryDirectory,
    MemoryCredentials,
    CredentialRecord,
) {
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
        result: serde_json::Value::Null,
        outputs: Default::default(),
        steps: vec![],
    };
    let store = MemoryExecutionStore::new(scope(), projection, START);
    let directory = MemoryDirectory::default();
    ready(directory.commit(DirectoryCommit {
        org_id: scope().org_id,
        expected_revision: 0,
        mutations: vec![
            DirectoryMutation::CreateOrganization {
                title: "Org".into(),
            },
            DirectoryMutation::CreateProject(ProjectRecord {
                id: scope().project_id,
                org_id: scope().org_id,
                slug: "api".into(),
                title: "API".into(),
            }),
            DirectoryMutation::CreateUser(UserRecord {
                id: "usr_00000000000000000000000001".parse().unwrap(),
                name: "alice".into(),
                active: true,
                role: runbook_core::directory::OrgRole::Member,
            }),
            DirectoryMutation::GrantProjectRole {
                project_id: scope().project_id,
                user_id: "usr_00000000000000000000000001".parse().unwrap(),
                role: ProjectRole::Operator,
            },
        ],
    }))
    .unwrap();
    let credentials = MemoryCredentials::default();
    let issuer = CredentialRecord {
        id: "tok_00000000000000000000000200".parse().unwrap(),
        kind: TokenKind::Service,
        hash: secret_hash("trb_svc_launcher"),
        principal: CredentialPrincipal::Service {
            project_id: scope().project_id,
            role: ProjectRole::Operator,
        },
        org_id: scope().org_id,
        scopes: [Scope::ExecutionsAct].into(),
        projects: None,
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
    };
    ready(credentials.insert(issuer.clone())).unwrap();
    let rb: runbook_model::RunbookId = "rb_00000000000000000000000001".parse().unwrap();
    ready(store.create_runbook(
        RunbookRecord {
            runbook_id: rb.clone(),
            slug: "cred-check".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        },
        SPEC.into(),
    ))
    .unwrap();
    let prepared = prepare(
        "cred-check",
        SPEC,
        &runbook_spec::CompileContext::default(),
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    ready(publish_draft(&store, &store, &rb, 1, prepared)).unwrap();
    let command = StartCommand {
        execution_id: "exe_00000000000000000000000001".parse().unwrap(),
        runbook_id: rb,
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "start-1".into(),
        principal: "user:alice".into(),
        request_hash: [1; 32],
    };
    let commit = ready(prepare_start_commit(&store, &store, &command)).unwrap();
    ready(store.commit_start(commit)).unwrap();
    let claim = ready(store.claim_due(1, 60_000)).unwrap().remove(0);
    ready(activate(&store, &directory, scope(), &claim)).unwrap();
    (store, directory, credentials, issuer)
}
fn entropy(n: u8) -> MintEntropy {
    MintEntropy {
        family_id: format!("fam-{n}"),
        access_token_id: format!("tok_{:026}", n * 2).parse().unwrap(),
        refresh_token_id: format!("tok_{:026}", n * 2 + 1).parse().unwrap(),
        access_random: [n; 32],
        refresh_random: [n.wrapping_add(50); 32],
    }
}
fn exe() -> runbook_model::ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}

#[test]
fn lifetimes_and_reuse_at_exact_boundaries() {
    let (store, directory, credentials, issuer) = setup();
    let request = runbook_model::MintTokenRequest {
        session_id: "s1".into(),
    };
    let pair = ready(mint_execution_pair(
        &store,
        &credentials,
        &directory,
        &issuer,
        &exe(),
        &request,
        "m1",
        [2; 32],
        entropy(1),
    ))
    .unwrap();
    assert_eq!(pair.access.expires_at_ms, START + 3_600_000);
    assert_eq!(pair.refresh.expires_at_ms, START + 30 * 24 * 3_600_000);
    ready(verify_execution_token(
        &store,
        &credentials,
        &directory,
        &pair.access.secret,
        START + 3_600_000 - 1,
    ))
    .unwrap();
    assert!(
        ready(verify_execution_token(
            &store,
            &credentials,
            &directory,
            &pair.access.secret,
            START + 3_600_000,
        ))
        .is_err()
    );
    // Refresh near the cap never extends past start + 30 days.
    store.set_time(START + 29 * 24 * 3_600_000);
    let rotated = ready(refresh_execution_pair(
        &store,
        &credentials,
        &directory,
        &RefreshRequest {
            refresh_token: pair.refresh.secret.clone(),
        },
        entropy(2),
    ))
    .unwrap();
    assert_eq!(rotated.refresh.expires_at_ms, START + 30 * 24 * 3_600_000);
    // Reuse of the consumed token revokes the successor too.
    assert!(matches!(
        ready(refresh_execution_pair(
            &store,
            &credentials,
            &directory,
            &RefreshRequest {
                refresh_token: pair.refresh.secret,
            },
            entropy(3),
        )),
        Err(ExecutionCredentialError::Rejected(
            "credentials.refresh_reused"
        ))
    ));
    assert!(
        ready(verify_execution_token(
            &store,
            &credentials,
            &directory,
            &rotated.access.secret,
            START + 29 * 24 * 3_600_000,
        ))
        .is_err()
    );
    let _tok: TokenId = "tok_00000000000000000000000001".parse().unwrap();
}

fn close_run(store: &MemoryExecutionStore, directory: &MemoryDirectory) {
    use runbook_core::actor::{ActorPrincipal, begin_actor};
    let work = ready(begin_actor(store, directory, scope(), &exe())).unwrap();
    let principal = ActorPrincipal {
        principal: "user:usr_00000000000000000000000001".into(),
        actor: runbook_model::ActorRef::Human {
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
        },
        credential_ref: "pat".into(),
    };
    ready(runbook_core::control::cancel(store, work, &principal, None)).unwrap();
}

// F25: closing the run ends every family; F18: refresh-token reuse after
// close is still recorded as an audit event on the closed run.
#[test]
fn close_ends_the_family_and_reuse_after_close_is_recorded() {
    use runbook_core::ExecutionStore;
    let (store, directory, credentials, issuer) = setup();
    let request = runbook_model::MintTokenRequest {
        session_id: "s1".into(),
    };
    let pair = ready(mint_execution_pair(
        &store,
        &credentials,
        &directory,
        &issuer,
        &exe(),
        &request,
        "m1",
        [2; 32],
        entropy(1),
    ))
    .unwrap();
    let rotated = ready(refresh_execution_pair(
        &store,
        &credentials,
        &directory,
        &RefreshRequest {
            refresh_token: pair.refresh.secret.clone(),
        },
        entropy(2),
    ))
    .unwrap();
    close_run(&store, &directory);
    assert!(matches!(
        ready(verify_execution_token(
            &store,
            &credentials,
            &directory,
            &rotated.access.secret,
            START + 1,
        )),
        Err(ExecutionCredentialError::Auth(_))
    ));
    let reuse = ready(refresh_execution_pair(
        &store,
        &credentials,
        &directory,
        &RefreshRequest {
            refresh_token: pair.refresh.secret,
        },
        entropy(3),
    ));
    assert!(
        matches!(
            reuse,
            Err(ExecutionCredentialError::Rejected(
                "credentials.refresh_reused"
            ))
        ),
        "{reuse:?}"
    );
    let run = ready(store.load(&exe())).unwrap();
    assert_eq!(
        run.events.last().unwrap().event.name(),
        "ExecutionTokenReuseDetected"
    );
}

// F25: the issuing credential is rechecked on every use; revoking it ends
// the execution tokens it minted.
#[test]
fn revoking_the_issuer_ends_its_execution_tokens() {
    let (store, directory, credentials, issuer) = setup();
    let request = runbook_model::MintTokenRequest {
        session_id: "s1".into(),
    };
    let pair = ready(mint_execution_pair(
        &store,
        &credentials,
        &directory,
        &issuer,
        &exe(),
        &request,
        "m1",
        [2; 32],
        entropy(1),
    ))
    .unwrap();
    ready(verify_execution_token(
        &store,
        &credentials,
        &directory,
        &pair.access.secret,
        START + 1,
    ))
    .unwrap();
    assert!(ready(credentials.revoke(&scope().org_id, &issuer.id, START + 2)).unwrap());
    assert!(
        ready(verify_execution_token(
            &store,
            &credentials,
            &directory,
            &pair.access.secret,
            START + 3,
        ))
        .is_err()
    );
}
