//! AUTH2 (D46): how a request's credential resolves to the record it
//! authorizes as, for bearer tokens and linked browser sessions.
use runbook_api::{Credential, CredentialFailure, Request, ResolvedSession};
use runbook_core::{
    auth_links::{is_session_record, session_credential},
    credentials::{
        ActorKind, CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind,
        encode_secret, memory::MemoryCredentials, secret_hash,
    },
    directory::{DirectorySnapshot, OrgRole, OrganizationRecord, ProjectRecord, UserRecord},
};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, UserId};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn run<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory yielded"),
    }
}
fn org() -> OrganizationId {
    format!("org_{:026}", 1).parse().unwrap()
}
fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn project(n: u8) -> ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}
fn request(credential: Option<Credential>) -> Request {
    Request {
        method: "GET".into(),
        path: "/v1/me".into(),
        query: Default::default(),
        body: serde_json::Value::Null,
        credential,
        idempotency_key: None,
        request_id: "req_1".into(),
        new_id: "unused".into(),
        if_match: None,
        client_ip: None,
    }
}
fn session(principal: Option<CredentialRecord>) -> Credential {
    Credential::Session(ResolvedSession {
        auth_user_id: "ba_1".into(),
        email: "alice@example.test".into(),
        email_verified: true,
        name: None,
        picture_url: None,
        session_id: "sess_1".into(),
        expires_at_ms: 10_000,
        mfa_verified_at: None,
        mfa_method: None,
        mfa_enrolled: Some(false),
        principal: principal.map(Box::new),
        scopes: None,
    })
}
fn snapshot(role: OrgRole, projects: &[(u8, ProjectRole)]) -> DirectorySnapshot {
    let mut s = DirectorySnapshot {
        org_id: org(),
        revision: 1,
        security: Default::default(),
        organization: OrganizationRecord {
            id: org(),
            title: "Org".into(),
        },
        users: BTreeMap::from([(
            user(1),
            UserRecord {
                id: user(1),
                name: "Alice".into(),
                active: true,
                role,
            },
        )]),
        projects: BTreeMap::new(),
        teams: BTreeMap::new(),
        project_members: BTreeMap::new(),
        team_members: BTreeSet::new(),
        archived_projects: Default::default(),
    };
    for (n, project_role) in projects {
        s.projects.insert(
            project(*n),
            ProjectRecord {
                id: project(*n),
                org_id: org(),
                slug: format!("p{n}"),
                title: "P".into(),
            },
        );
        s.project_members
            .insert((project(*n), user(1)), *project_role);
    }
    s
}

#[test]
fn bearer_and_session_resolve_to_their_records() {
    let store = MemoryCredentials::default();
    let secret = encode_secret(TokenKind::Personal, [7; 32]);
    let record = CredentialRecord {
        id: format!("tok_{:026}", 1).parse().unwrap(),
        kind: TokenKind::Personal,
        hash: secret_hash(&secret),
        principal: CredentialPrincipal::User {
            user_id: user(1),
            actor: ActorKind::Human,
        },
        org_id: org(),
        scopes: [Scope::RunbooksRead].into(),
        projects: None,
        expires_at: Some(10),
        revoked_at: None,
        last_used_at: None,
    };
    run(store.insert(record.clone())).unwrap();
    let resolve = |c| run(request(c).credential_record(&store));
    assert_eq!(resolve(None), Err(CredentialFailure::Required));
    assert_eq!(
        resolve(Some(Credential::Bearer(secret.clone()))),
        Ok(record.clone())
    );
    // A stored record answers only to a secret of its own kind's prefix.
    let service = encode_secret(TokenKind::Service, [8; 32]);
    let mut mislabeled = record.clone();
    mislabeled.id = format!("tok_{:026}", 2).parse().unwrap();
    mislabeled.hash = secret_hash(&service);
    run(store.insert(mislabeled)).unwrap();
    assert_eq!(
        resolve(Some(Credential::Bearer(service))),
        Err(CredentialFailure::Invalid)
    );
    assert_eq!(
        resolve(Some(Credential::Bearer("not-a-token".into()))),
        Err(CredentialFailure::Invalid)
    );
    // An unlinked session authenticates nothing; a linked one is its principal.
    assert_eq!(
        resolve(Some(session(None))),
        Err(CredentialFailure::Invalid)
    );
    let principal = session_credential(&snapshot(OrgRole::Member, &[]), &user(1), "s", 99);
    assert_eq!(
        resolve(Some(session(principal.clone()))),
        Ok(principal.unwrap())
    );
    assert!(request(Some(session(None))).is_session());
    assert!(request(Some(session(None))).bearer().is_none());
    assert!(!request(Some(Credential::Bearer(secret))).is_session());
}

#[test]
fn session_principal_is_human_and_role_bounded() {
    let viewer = session_credential(
        &snapshot(OrgRole::Member, &[(1, ProjectRole::Viewer)]),
        &user(1),
        "sess_a",
        5_000,
    )
    .unwrap();
    assert_eq!(viewer.kind, TokenKind::Personal);
    assert_eq!(
        viewer.principal,
        CredentialPrincipal::User {
            user_id: user(1),
            actor: ActorKind::Human
        }
    );
    assert_eq!(viewer.expires_at, Some(5_000));
    assert_eq!(viewer.projects, None);
    viewer.validate().unwrap();
    assert_eq!(
        viewer.scopes,
        [
            Scope::RunbooksRead,
            Scope::ExecutionsRead,
            Scope::SkillsRead,
            Scope::TokensManage,
            // R0-2: every member reads the runner.
            Scope::RunnerRead
        ]
        .into()
    );
    let operator = session_credential(
        &snapshot(
            OrgRole::Member,
            &[(1, ProjectRole::Viewer), (2, ProjectRole::Operator)],
        ),
        &user(1),
        "sess_a",
        5_000,
    )
    .unwrap();
    assert!(operator.scopes.contains(&Scope::ExecutionsAct));
    assert!(!operator.scopes.contains(&Scope::RunbooksWrite));
    assert!(!operator.scopes.contains(&Scope::ProjectAdmin));
    let admin =
        session_credential(&snapshot(OrgRole::Admin, &[]), &user(1), "sess_a", 5_000).unwrap();
    // An org admin is admin of every project without a grant (2026-11-17),
    // and administers the runner (R0-2).
    let mut all: std::collections::BTreeSet<Scope> = Scope::ALL.into();
    all.extend(Scope::RUNNER);
    assert_eq!(admin.scopes, all);
    let project_admin = session_credential(
        &snapshot(OrgRole::Member, &[(1, ProjectRole::Admin)]),
        &user(1),
        "sess_a",
        5_000,
    )
    .unwrap();
    // A project admin is not an org admin: runner:read only.
    let mut all: std::collections::BTreeSet<Scope> = Scope::ALL.into();
    all.insert(Scope::RunnerRead);
    assert_eq!(project_admin.scopes, all);
    // Distinct sessions get distinct, stable, never-stored token IDs.
    assert_eq!(viewer.id, operator.id);
    let other = session_credential(&snapshot(OrgRole::Member, &[]), &user(1), "sess_b", 1).unwrap();
    assert_ne!(other.id, viewer.id);
    // Inactive or unknown users get no principal.
    let mut inactive = snapshot(OrgRole::Owner, &[]);
    inactive.users.get_mut(&user(1)).unwrap().active = false;
    assert_eq!(session_credential(&inactive, &user(1), "s", 1), None);
    assert_eq!(session_credential(&inactive, &user(2), "s", 1), None);
}

#[test]
fn a_session_record_is_told_apart_from_a_stored_token() {
    let session =
        session_credential(&snapshot(OrgRole::Member, &[]), &user(1), "sess_a", 5_000).unwrap();
    assert!(is_session_record(&session));
    let secret = encode_secret(TokenKind::Personal, [7; 32]);
    let stored = CredentialRecord {
        id: format!("tok_{:026}", 1).parse().unwrap(),
        hash: secret_hash(&secret),
        ..session.clone()
    };
    assert!(!is_session_record(&stored));
    // The session's ID with another hash is not a session's either.
    let mismatched = CredentialRecord {
        hash: secret_hash(&secret),
        ..session
    };
    assert!(!is_session_record(&mismatched));
}
