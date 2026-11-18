//! Hash-only credentials and current authorization, shared by both adapters.
use crate::{
    CredentialFactory,
    directory::{org, project, user},
};
use runbook_core::{CommitResult, credentials::*, directory::*};
use runbook_model::ProjectRole;

pub fn record(n: u8) -> CredentialRecord {
    CredentialRecord {
        id: format!("tok_{n:026}").parse().unwrap(),
        kind: TokenKind::Personal,
        hash: secret_hash(&encode_secret(TokenKind::Personal, [n; 32])),
        principal: CredentialPrincipal::User {
            user_id: user(1),
            actor: ActorKind::Human,
        },
        org_id: org(n),
        scopes: [Scope::ExecutionsAct, Scope::RunbooksPublish].into(),
        projects: Some([project(n)].into()),
        expires_at: Some(1_000_000),
        revoked_at: None,
        last_used_at: None,
    }
}
async fn fixture<F: CredentialFactory>(f: &F) -> F::Store {
    let d = f.create_directory().await.unwrap();
    for n in [1, 2] {
        assert_eq!(
            d.commit(DirectoryCommit {
                org_id: org(n),
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Test".into()
                    },
                    DirectoryMutation::CreateUser(UserRecord {
                        id: user(1),
                        name: "Alice".into(),
                        active: true,
                        role: OrgRole::Member
                    }),
                    DirectoryMutation::CreateProject(ProjectRecord {
                        id: project(n),
                        org_id: org(n),
                        slug: "api".into(),
                        title: "API".into()
                    }),
                    DirectoryMutation::GrantProjectRole {
                        project_id: project(n),
                        user_id: user(1),
                        role: ProjectRole::Operator
                    },
                ],
            })
            .await
            .unwrap(),
            CommitResult::Committed(1)
        );
    }
    d
}
pub async fn pat_and_service_authorization_is_current<F: CredentialFactory>(f: &F) {
    let d = fixture(f).await;
    let s = f.create_credentials().await.unwrap();
    let r = record(1);
    s.insert(r.clone()).await.unwrap();
    assert_eq!(s.find_by_hash(&r.hash).await.unwrap(), Some(r.clone()));
    let a = d.snapshot(&org(1)).await.unwrap();
    assert_eq!(
        authorize(&r, &a, 100, &project(1), Scope::ExecutionsAct),
        Ok(Principal::Human { user_id: user(1) })
    );
    assert_eq!(
        authorize(&r, &a, 100, &project(1), Scope::RunbooksPublish),
        Err(AuthError::Forbidden)
    ); // role intersection
    assert_eq!(
        authorize(&r, &a, 100, &project(1), Scope::ExecutionsRead),
        Err(AuthError::Forbidden)
    ); // scope intersection
    assert_eq!(
        authorize(&r, &a, 1_000_000, &project(1), Scope::ExecutionsAct),
        Err(AuthError::Expired)
    );
    let mut narrowed = r.clone();
    narrowed.projects = Some(Default::default());
    assert_eq!(
        authorize(&narrowed, &a, 100, &project(1), Scope::ExecutionsAct),
        Err(AuthError::Forbidden)
    );
    assert_eq!(
        authorize(&r, &a, 100, &project(2), Scope::ExecutionsAct),
        Err(AuthError::Forbidden)
    );
    let mut agent = r.clone();
    agent.principal = CredentialPrincipal::User {
        user_id: user(1),
        actor: ActorKind::Agent,
    };
    assert_eq!(
        authorize(&agent, &a, 100, &project(1), Scope::ExecutionsAct),
        Ok(Principal::Agent { sponsor: user(1) })
    );
    let mut svc = r.clone();
    svc.id = format!("tok_{:026}", 3).parse().unwrap();
    svc.kind = TokenKind::Service;
    svc.hash = secret_hash(&encode_secret(TokenKind::Service, [3; 32]));
    svc.principal = CredentialPrincipal::Service {
        project_id: project(1),
        role: ProjectRole::Operator,
    };
    s.insert(svc.clone()).await.unwrap();
    assert_eq!(
        authorize(&svc, &a, 100, &project(1), Scope::ExecutionsAct),
        Ok(Principal::Service {
            token_id: svc.id.clone(),
            project_id: project(1)
        })
    );
    let b = d.snapshot(&org(2)).await.unwrap();
    assert_eq!(
        authorize(&svc, &b, 100, &project(2), Scope::ExecutionsAct),
        Err(AuthError::Forbidden)
    );
    let mut invalid = svc.clone();
    invalid.principal = r.principal.clone();
    assert_eq!(
        authorize(&invalid, &a, 100, &project(1), Scope::ExecutionsAct),
        Err(AuthError::InvalidCredential)
    );
    d.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::SetUserActive {
            user_id: user(1),
            active: false,
        }],
    })
    .await
    .unwrap();
    let a = d.snapshot(&org(1)).await.unwrap();
    assert_eq!(
        authorize(&r, &a, 100, &project(1), Scope::ExecutionsAct),
        Err(AuthError::Forbidden)
    );
    assert!(
        authorize(
            &record(2),
            &d.snapshot(&org(2)).await.unwrap(),
            100,
            &project(2),
            Scope::ExecutionsAct
        )
        .is_ok()
    );
    assert!(authorize(&svc, &a, 100, &project(1), Scope::ExecutionsAct).is_ok()); // no human sponsor
    // Tenant-scoped revocation cannot affect another org; repeat preserves first timestamp.
    assert!(!s.revoke(&org(2), &r.id, 200).await.unwrap());
    assert!(s.revoke(&org(1), &r.id, 200).await.unwrap());
    assert!(!s.revoke(&org(1), &r.id, 300).await.unwrap());
    let revoked = s.find_by_hash(&r.hash).await.unwrap().unwrap();
    assert_eq!(revoked.revoked_at, Some(200));
    assert_eq!(
        authorize(&revoked, &a, 100, &project(1), Scope::ExecutionsAct),
        Err(AuthError::Revoked)
    );
    // Re-read roles on every authorization, rather than storing the PAT's role.
    d.commit(DirectoryCommit {
        org_id: org(2),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::GrantProjectRole {
            project_id: project(2),
            user_id: user(1),
            role: ProjectRole::Viewer,
        }],
    })
    .await
    .unwrap();
    assert_eq!(
        authorize(
            &record(2),
            &d.snapshot(&org(2)).await.unwrap(),
            100,
            &project(2),
            Scope::ExecutionsAct
        ),
        Err(AuthError::Forbidden)
    );
}
pub async fn credential_usage_write_is_throttled<F: CredentialFactory>(f: &F) {
    let s = f.create_credentials().await.unwrap();
    let r = record(1);
    s.insert(r.clone()).await.unwrap();
    assert!(!s.touch_last_used(&org(2), &r.id, 1_000).await.unwrap());
    let mut writes = 0;
    for n in 0..100 {
        writes += usize::from(
            s.touch_last_used(&org(1), &r.id, 1_000 + n * 600)
                .await
                .unwrap(),
        );
    }
    assert_eq!(writes, 1);
    assert!(!s.touch_last_used(&org(1), &r.id, 999).await.unwrap());
    assert!(!s.touch_last_used(&org(1), &r.id, 300_999).await.unwrap());
    assert!(s.touch_last_used(&org(1), &r.id, 301_000).await.unwrap());
    assert_eq!(
        s.find_by_hash(&r.hash).await.unwrap().unwrap().last_used_at,
        Some(301_000)
    );
    assert!(!should_touch_last_used(Some(i64::MAX), i64::MIN));
    assert!(should_touch_last_used(Some(i64::MIN), i64::MAX));
}
pub async fn secret_format_and_hash_are_pinned<F: CredentialFactory>(f: &F) {
    // Independently calculated using Python base64.urlsafe_b64encode/hashlib.sha256.
    let raw = std::array::from_fn(|i| i as u8);
    let secret = encode_secret(TokenKind::Personal, raw);
    assert!(secret == "trb_pat_AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8");
    let hash = secret_hash(&secret);
    let hex: String = hash.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    assert!(hex == "36c24d76b58094293cb70a7dfb150977dc34536d61370c84480d236b1150253c");
    for (kind, prefix) in [
        (TokenKind::Personal, "trb_pat_"),
        (TokenKind::Service, "trb_svc_"),
        (TokenKind::Execution, "trb_exe_"),
        (TokenKind::Refresh, "trb_exr_"),
    ] {
        let encoded = encode_secret(kind, raw);
        assert!(encoded.starts_with(prefix));
        assert_eq!(encoded.len(), 51);
        let (parsed, bytes) = parse_secret(&encoded).unwrap();
        assert_eq!(parsed, kind);
        assert!(bytes.as_bytes() == &raw);
        assert!(!format!("{bytes:?}").contains("0, 1, 2"));
    }
    for invalid in [
        "trb_pat_short".to_owned(),
        format!("{secret}="),
        secret.replace("Hh8", "Hh9"),
        secret.replace("pat", "ref"),
        secret.replace("AA", "++"),
    ] {
        assert!(parse_secret(&invalid).is_err());
    }
    let refresh = encode_secret(TokenKind::Refresh, raw);
    assert!(refresh == "trb_exr_AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8");
    let refresh_hex: String = secret_hash(&refresh)
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(refresh_hex == "20c9f5972dc3e93441850c642feee6f47545d19d90ef9a503a19c367dc2925e8");
    let s = f.create_credentials().await.unwrap();
    let mut r = record(1);
    r.hash = hash;
    s.insert(r.clone()).await.unwrap();
    let loaded = s.find_by_hash(&hash).await.unwrap().unwrap();
    assert_eq!(loaded, r);
    assert!(!format!("{loaded:?}").contains(&secret));
    assert!(!serde_json::to_string(&loaded).unwrap().contains(&secret));
    assert_eq!(s.insert(r.clone()).await, Err(CredentialError::Duplicate));
    let mut duplicate = r.clone();
    duplicate.id = format!("tok_{:026}", 9).parse().unwrap();
    assert_eq!(s.insert(duplicate).await, Err(CredentialError::Duplicate));
    assert_eq!(
        s.find_by_hash(&secret_hash("not a credential"))
            .await
            .unwrap(),
        None
    );
}
