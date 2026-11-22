//! B4.1/C2 `/v1/tokens` rules: a new token only narrows its issuer, is
//! issued only to the caller, and listing/revocation follow D17.
use runbook_core::{
    credentials::{issuance::*, *},
    directory::{DirectorySnapshot, OrgRole, OrganizationRecord, ProjectRecord, UserRecord},
};
use runbook_model::{
    ActorKind as WireActor, CreateTokenRequest, ManagedTokenKind, OrganizationId, ProjectId,
    ProjectRole, TokenId, UserId,
};

const NOW: i64 = 1_000_000_000_000;
const DAY: i64 = 86_400_000;

fn org() -> OrganizationId {
    "org_00000000000000000000000001".parse().unwrap()
}
fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn project(n: u8) -> ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}
fn tok(n: u8) -> TokenId {
    format!("tok_{n:026}").parse().unwrap()
}
/// Owner 1 (admin of both projects), member 2 (operator of project 1).
fn snapshot() -> DirectorySnapshot {
    let mut s = DirectorySnapshot {
        org_id: org(),
        revision: 1,
        security: Default::default(),
        organization: OrganizationRecord {
            id: org(),
            title: "Org".into(),
        },
        users: Default::default(),
        projects: Default::default(),
        teams: Default::default(),
        project_members: Default::default(),
        team_members: Default::default(),
        archived_projects: Default::default(),
    };
    for (n, role) in [(1, OrgRole::Owner), (2, OrgRole::Member)] {
        s.users.insert(
            user(n),
            UserRecord {
                id: user(n),
                name: format!("u{n}"),
                active: true,
                role,
            },
        );
    }
    for (n, slug) in [(1, "api"), (2, "web")] {
        s.projects.insert(
            project(n),
            ProjectRecord {
                id: project(n),
                org_id: org(),
                slug: slug.into(),
                title: slug.into(),
            },
        );
    }
    s.project_members
        .insert((project(1), user(1)), ProjectRole::Admin);
    s.project_members
        .insert((project(2), user(1)), ProjectRole::Admin);
    s.project_members
        .insert((project(1), user(2)), ProjectRole::Operator);
    s
}
fn pat(n: u8, user_n: u8, actor: ActorKind, scopes: &[Scope]) -> CredentialRecord {
    CredentialRecord {
        id: tok(n),
        kind: TokenKind::Personal,
        hash: secret_hash(&format!("secret-{n}")),
        principal: CredentialPrincipal::User {
            user_id: user(user_n),
            actor,
        },
        org_id: org(),
        scopes: scopes.iter().copied().collect(),
        projects: None,
        expires_at: Some(NOW + 300 * DAY),
        revoked_at: None,
        last_used_at: None,
    }
}
fn all() -> Vec<Scope> {
    vec![
        Scope::TokensManage,
        Scope::ExecutionsRead,
        Scope::ExecutionsAct,
        Scope::RunbooksRead,
    ]
}
fn issue(
    caller: &CredentialRecord,
    request: CreateTokenRequest,
) -> Result<(CredentialRecord, TokenMetadata), IssueError> {
    let s = snapshot();
    let manager = authorize_management(caller, &s, NOW).unwrap();
    plan_issue(
        caller,
        &manager,
        &s,
        NOW,
        &request,
        tok(99),
        secret_hash("new"),
    )
}
fn strings(v: &[&str]) -> Option<Vec<String>> {
    Some(v.iter().map(|s| s.to_string()).collect())
}

#[test]
fn management_needs_an_active_user_with_tokens_manage() {
    let s = snapshot();
    let owner = pat(1, 1, ActorKind::Human, &all());
    assert_eq!(
        authorize_management(&owner, &s, NOW).unwrap().org_role,
        OrgRole::Owner
    );
    let no_scope = pat(2, 1, ActorKind::Human, &[Scope::ExecutionsRead]);
    assert_eq!(
        authorize_management(&no_scope, &s, NOW),
        Err(ManageError::MissingScope)
    );
    let mut revoked = owner.clone();
    revoked.revoked_at = Some(NOW - 1);
    assert_eq!(
        authorize_management(&revoked, &s, NOW),
        Err(ManageError::Auth(AuthError::Revoked))
    );
    let mut expired = owner.clone();
    expired.expires_at = Some(NOW);
    assert_eq!(
        authorize_management(&expired, &s, NOW),
        Err(ManageError::Auth(AuthError::Expired))
    );
    let mut foreign = owner.clone();
    foreign.org_id = "org_00000000000000000000000002".parse().unwrap();
    assert!(authorize_management(&foreign, &s, NOW).is_err());
    let service = CredentialRecord {
        kind: TokenKind::Service,
        principal: CredentialPrincipal::Service {
            project_id: project(1),
            role: ProjectRole::Admin,
        },
        expires_at: None,
        ..owner.clone()
    };
    assert_eq!(
        authorize_management(&service, &s, NOW),
        Err(ManageError::Auth(AuthError::Forbidden))
    );
    // D17: people manage tokens. An agent-kind PAT never does, even with
    // tokens:manage.
    let agent = pat(3, 1, ActorKind::Agent, &all());
    assert_eq!(
        authorize_management(&agent, &s, NOW),
        Err(ManageError::Auth(AuthError::Forbidden))
    );
    let mut inactive = s.clone();
    inactive.users.get_mut(&user(1)).unwrap().active = false;
    assert!(authorize_management(&owner, &inactive, NOW).is_err());
}

#[test]
fn personal_tokens_inherit_and_only_narrow_the_caller() {
    let mut caller = pat(1, 2, ActorKind::Human, &all());
    caller.projects = Some([project(1)].into());
    // Omitted options inherit the caller exactly, with the default lifetime.
    let (record, meta) = issue(&caller, CreateTokenRequest::default()).unwrap();
    assert_eq!(record.kind, TokenKind::Personal);
    assert_eq!(record.scopes, caller.scopes);
    assert_eq!(record.projects, caller.projects);
    assert_eq!(record.expires_at, Some(NOW + DEFAULT_PERSONAL_LIFETIME_MS));
    assert_eq!(
        record.principal,
        CredentialPrincipal::User {
            user_id: user(2),
            actor: ActorKind::Human
        }
    );
    assert_eq!(meta.created_by, Some(user(2)));
    assert_eq!(meta.created_at, Some(NOW));
    // Narrowing by scope and by slug.
    let (record, meta) = issue(
        &caller,
        CreateTokenRequest {
            name: Some(" ci ".into()),
            scopes: strings(&["executions:read"]),
            projects: strings(&["api"]),
            expires_at_ms: Some(NOW + 30 * DAY),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(record.scopes, [Scope::ExecutionsRead].into());
    assert_eq!(record.projects, Some([project(1)].into()));
    assert_eq!(record.expires_at, Some(NOW + 30 * DAY));
    assert_eq!(meta.name, "ci");
    // Never wider.
    for (request, expected) in [
        (
            CreateTokenRequest {
                scopes: strings(&["runbooks:write"]),
                ..Default::default()
            },
            IssueError::ExceedsCaller("scopes"),
        ),
        (
            CreateTokenRequest {
                projects: strings(&["web"]),
                ..Default::default()
            },
            IssueError::ExceedsCaller("projects"),
        ),
        (
            CreateTokenRequest {
                expires_at_ms: Some(NOW + 301 * DAY),
                ..Default::default()
            },
            IssueError::ExceedsCaller("expires_at_ms"),
        ),
        (
            CreateTokenRequest {
                user_id: Some(user(1)),
                ..Default::default()
            },
            IssueError::Forbidden,
        ),
        (
            CreateTokenRequest {
                projects: strings(&["nope"]),
                ..Default::default()
            },
            IssueError::NotFound,
        ),
        (
            CreateTokenRequest {
                scopes: strings(&[]),
                ..Default::default()
            },
            IssueError::Invalid("scopes"),
        ),
        (
            CreateTokenRequest {
                scopes: strings(&["runbooks:everything"]),
                ..Default::default()
            },
            IssueError::Invalid("scopes"),
        ),
        (
            CreateTokenRequest {
                expires_at_ms: Some(NOW),
                ..Default::default()
            },
            IssueError::Invalid("expires_at_ms"),
        ),
        (
            CreateTokenRequest {
                role: Some(ProjectRole::Viewer),
                ..Default::default()
            },
            IssueError::Invalid("role"),
        ),
        (
            CreateTokenRequest {
                name: Some("  ".into()),
                ..Default::default()
            },
            IssueError::Invalid("name"),
        ),
    ] {
        assert_eq!(
            issue(&caller, request.clone()),
            Err(expected),
            "{request:?}"
        );
    }
    // A member cannot name a project they do not belong to.
    let unrestricted = pat(2, 2, ActorKind::Human, &all());
    assert_eq!(
        issue(
            &unrestricted,
            CreateTokenRequest {
                projects: strings(&["web"]),
                ..Default::default()
            }
        ),
        Err(IssueError::NotFound)
    );
    // The same user is allowed to name themself.
    assert!(
        issue(
            &caller,
            CreateTokenRequest {
                user_id: Some(user(2)),
                ..Default::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn personal_lifetime_is_at_most_a_year() {
    let mut caller = pat(1, 1, ActorKind::Human, &all());
    caller.expires_at = Some(NOW + 3 * MAX_PERSONAL_LIFETIME_MS);
    let request = |t| CreateTokenRequest {
        expires_at_ms: Some(t),
        ..Default::default()
    };
    assert!(
        issue(
            &caller,
            request(NOW + MAX_PERSONAL_LIFETIME_MS + CLOCK_SKEW_MS)
        )
        .is_ok()
    );
    assert_eq!(
        issue(
            &caller,
            request(NOW + MAX_PERSONAL_LIFETIME_MS + CLOCK_SKEW_MS + 1)
        ),
        Err(IssueError::Invalid("expires_at_ms"))
    );
    // The default never outlives the caller.
    caller.expires_at = Some(NOW + DAY);
    let (record, _) = issue(&caller, CreateTokenRequest::default()).unwrap();
    assert_eq!(record.expires_at, Some(NOW + DAY));
}

#[test]
fn agent_tokens_never_carry_tokens_manage() {
    let human = pat(1, 2, ActorKind::Human, &all());
    // Inherited scopes drop tokens:manage for an agent-kind token.
    let (record, _) = issue(
        &human,
        CreateTokenRequest {
            actor_kind: Some(WireActor::Agent),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        record.principal,
        CredentialPrincipal::User {
            user_id: user(2),
            actor: ActorKind::Agent
        }
    );
    let mut expected: std::collections::BTreeSet<Scope> = all().into_iter().collect();
    expected.remove(&Scope::TokensManage);
    assert_eq!(record.scopes, expected);
    // Asking for it explicitly is refused.
    assert_eq!(
        issue(
            &human,
            CreateTokenRequest {
                actor_kind: Some(WireActor::Agent),
                scopes: strings(&["executions:read", "tokens:manage"]),
                ..Default::default()
            }
        ),
        Err(IssueError::Invalid("scopes"))
    );
    // An agent-kind token holding only tokens:manage inherits nothing.
    let only_manage = pat(2, 2, ActorKind::Human, &[Scope::TokensManage]);
    assert_eq!(
        issue(
            &only_manage,
            CreateTokenRequest {
                actor_kind: Some(WireActor::Agent),
                ..Default::default()
            }
        ),
        Err(IssueError::Invalid("scopes"))
    );
    // Human tokens keep inheriting it.
    let (record, _) = issue(&human, CreateTokenRequest::default()).unwrap();
    assert!(record.scopes.contains(&Scope::TokensManage));
}

#[test]
fn service_tokens_need_org_admin_and_never_exceed_the_callers_role() {
    let service = |projects: &[&str], role| CreateTokenRequest {
        kind: Some(ManagedTokenKind::Service),
        scopes: strings(&["executions:read", "executions:act"]),
        projects: strings(projects),
        role,
        ..Default::default()
    };
    let owner = pat(1, 1, ActorKind::Human, &all());
    let (record, _) = issue(&owner, service(&["api"], Some(ProjectRole::Operator))).unwrap();
    assert_eq!(record.kind, TokenKind::Service);
    assert_eq!(
        record.principal,
        CredentialPrincipal::Service {
            project_id: project(1),
            role: ProjectRole::Operator
        }
    );
    assert_eq!(record.projects, Some([project(1)].into()));
    assert_eq!(record.expires_at, None);
    for (request, expected) in [
        (service(&["api"], None), IssueError::Invalid("role")),
        (
            service(&["api", "web"], Some(ProjectRole::Viewer)),
            IssueError::Invalid("projects"),
        ),
        (
            CreateTokenRequest {
                projects: None,
                ..service(&[], Some(ProjectRole::Viewer))
            },
            IssueError::Invalid("projects"),
        ),
        (
            CreateTokenRequest {
                scopes: None,
                ..service(&["api"], Some(ProjectRole::Viewer))
            },
            IssueError::Invalid("scopes"),
        ),
        (
            CreateTokenRequest {
                scopes: strings(&["skills:write"]),
                ..service(&["api"], Some(ProjectRole::Viewer))
            },
            IssueError::ExceedsCaller("scopes"),
        ),
    ] {
        assert_eq!(issue(&owner, request.clone()), Err(expected), "{request:?}");
    }
    // A member is not an org admin (D17).
    let member = pat(2, 2, ActorKind::Human, &all());
    assert_eq!(
        issue(&member, service(&["api"], Some(ProjectRole::Viewer))),
        Err(IssueError::Forbidden)
    );
    // An org admin is admin of every project (2026-11-17), so even one
    // granted only operator may issue up to admin.
    let mut s = snapshot();
    s.users.get_mut(&user(2)).unwrap().role = OrgRole::Admin;
    let manager = authorize_management(&member, &s, NOW).unwrap();
    let plan = |role| {
        plan_issue(
            &member,
            &manager,
            &s,
            NOW,
            &service(&["api"], Some(role)),
            tok(98),
            secret_hash("svc"),
        )
    };
    assert!(plan(ProjectRole::Operator).is_ok());
    assert!(plan(ProjectRole::Editor).is_ok());
    assert!(plan(ProjectRole::Admin).is_ok());
}

#[test]
fn listing_and_revocation_follow_ownership_and_org_role() {
    let mut s = snapshot();
    s.users.insert(
        user(3),
        UserRecord {
            id: user(3),
            name: "u3".into(),
            active: true,
            role: OrgRole::Admin,
        },
    );
    s.users.insert(
        user(4),
        UserRecord {
            id: user(4),
            name: "u4".into(),
            active: true,
            role: OrgRole::Admin,
        },
    );
    let owner = authorize_management(&pat(1, 1, ActorKind::Human, &all()), &s, NOW).unwrap();
    let member = authorize_management(&pat(2, 2, ActorKind::Human, &all()), &s, NOW).unwrap();
    let admin = authorize_management(&pat(3, 3, ActorKind::Human, &all()), &s, NOW).unwrap();
    let owners_pat = pat(10, 1, ActorKind::Human, &all());
    let members_pat = pat(11, 2, ActorKind::Human, &all());
    let members_agent_pat = pat(12, 2, ActorKind::Agent, &all());
    let other_admins_pat = pat(14, 4, ActorKind::Human, &all());
    let svc = CredentialRecord {
        kind: TokenKind::Service,
        principal: CredentialPrincipal::Service {
            project_id: project(1),
            role: ProjectRole::Operator,
        },
        expires_at: None,
        ..pat(13, 1, ActorKind::Human, &all())
    };
    assert!(listed(&owner, &owners_pat) && listed(&owner, &svc));
    assert!(!listed(&owner, &members_pat));
    assert!(listed(&member, &members_pat) && listed(&member, &members_agent_pat));
    assert!(!listed(&member, &owners_pat) && !listed(&member, &svc));
    // D17 by rank: owners revoke anyone's; admins revoke members' and other
    // admins' PATs, never an owner's; members only their own.
    for (manager, target, allowed) in [
        (&owner, &members_pat, true),
        (&owner, &other_admins_pat, true),
        (&admin, &members_pat, true),
        (&admin, &other_admins_pat, true),
        (&admin, &owners_pat, false),
        (&admin, &svc, true),
        (&member, &owners_pat, false),
        (&member, &other_admins_pat, false),
        (&member, &svc, false),
        (&member, &members_agent_pat, true),
    ] {
        assert_eq!(
            may_revoke(manager, &s, target),
            allowed,
            "{:?} -> {}",
            manager.org_role,
            target.id
        );
    }
    let mut revoked = members_pat.clone();
    revoked.revoked_at = Some(NOW);
    assert!(!listed(&member, &revoked) && !may_revoke(&owner, &s, &revoked));
    // F8: a project-narrowed PAT never carries org-admin powers, even when
    // its owner is an org admin: it lists and revokes only its own tokens.
    let narrowed = CredentialRecord {
        projects: Some([project(1)].into()),
        ..pat(15, 3, ActorKind::Human, &all())
    };
    let narrowed_admin = authorize_management(&narrowed, &s, NOW).unwrap();
    let project_two_svc = CredentialRecord {
        principal: CredentialPrincipal::Service {
            project_id: project(2),
            role: ProjectRole::Operator,
        },
        ..svc.clone()
    };
    assert!(!narrowed_admin.org_admin());
    assert!(!listed(&narrowed_admin, &project_two_svc) && !listed(&narrowed_admin, &svc));
    assert!(!may_revoke(&narrowed_admin, &s, &project_two_svc));
    assert!(!may_revoke(&narrowed_admin, &s, &members_pat));
    assert!(may_revoke(&narrowed_admin, &s, &narrowed));
}

#[test]
fn views_carry_metadata_never_secrets() {
    let record = pat(1, 1, ActorKind::Agent, &[Scope::ExecutionsRead]);
    let meta = TokenMetadata {
        name: "ci".into(),
        created_at: Some(NOW),
        created_by: Some(user(1)),
        fresh_mfa_at_issue: false,
    };
    let v = view(&record, &meta);
    assert_eq!(v.scopes, vec!["executions:read".to_string()]);
    assert_eq!(v.kind, ManagedTokenKind::Personal);
    assert_eq!(v.actor_kind, Some(WireActor::Agent));
    let text = serde_json::to_string(&v).unwrap();
    assert!(!text.contains("hash") && !text.contains("secret"), "{text}");
}
