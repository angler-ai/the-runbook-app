//! AuthLinkStore cases (D45). Registered red at T0; AUTH2 makes them pass
//! on both adapters.
use crate::directory::{org, project, user};
use runbook_core::{
    CommitResult,
    auth_links::{AuthLinkStore, InvitationRecord, InvitationState, LinkRequest},
    directory::*,
};
use runbook_model::{InvitationId, ProjectRole};

fn inv(n: u8) -> InvitationId {
    format!("inv_{n:026}").parse().unwrap()
}
fn invitation(n: u8, email: &str, now: i64) -> InvitationRecord {
    InvitationRecord {
        id: inv(n),
        org_id: org(1),
        email: email.into(),
        role: OrgRole::Admin,
        projects: vec![(project(1), ProjectRole::Operator)],
        invited_by: user(1),
        created_at_ms: now,
        expires_at_ms: now + runbook_core::auth_links::INVITATION_TTL_MS,
        state: InvitationState::Pending,
        accepted_by: None,
        closed_at_ms: None,
        name: None,
    }
}

/// An invitation is found by email, consumed by the first link (which creates
/// the user with the invited roles), and the link resolves afterwards.
pub async fn invitation_links_once_and_applies_role<D: DirectoryStore + AuthLinkStore>(
    d: &D,
) -> Result<(), DirectoryError> {
    let now = 1_000_000;
    assert_eq!(
        d.commit(DirectoryCommit {
            org_id: org(1),
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: "Org1".into()
                },
                DirectoryMutation::CreateUser(UserRecord {
                    id: user(1),
                    name: "Owner".into(),
                    active: true,
                    role: OrgRole::Owner,
                }),
                DirectoryMutation::CreateProject(ProjectRecord {
                    id: project(1),
                    org_id: org(1),
                    slug: "api".into(),
                    title: "API".into(),
                }),
            ],
        })
        .await?,
        CommitResult::Committed(1)
    );
    let email = "new.member@example.test";
    d.create_invitation(&org(1), invitation(1, email, now))
        .await?;
    assert_eq!(
        d.create_invitation(&org(1), invitation(2, email, now))
            .await,
        Err(DirectoryError::Duplicate)
    );
    assert_eq!(d.invitations_for_email(email, now).await?.len(), 1);
    assert!(
        d.invitations_for_email(email, now + runbook_core::auth_links::INVITATION_TTL_MS)
            .await?
            .is_empty()
    );
    let request = LinkRequest {
        auth_user_id: "ba_user_1".into(),
        verified_email: email.into(),
        user_id: user(2),
        name: "New Member".into(),
        invitation: Some(inv(1)),
        now_ms: now + 1,
    };
    let link = d.link_identity(&org(1), request.clone()).await?;
    assert_eq!(
        (link.user_id.clone(), link.org_id.clone()),
        (user(2), org(1))
    );
    assert_eq!(d.auth_link("ba_user_1").await?, Some(link.clone()));
    assert_eq!(d.link_identity(&org(1), request).await?, link);
    assert!(d.invitations_for_email(email, now + 2).await?.is_empty());
    let snapshot = d.snapshot(&org(1)).await?;
    assert_eq!(snapshot.users[&user(2)].role, OrgRole::Admin);
    assert_eq!(
        snapshot.project_members.get(&(project(1), user(2))),
        Some(&ProjectRole::Operator)
    );
    assert_eq!(
        d.user_by_verified_email(email).await?.map(|u| u.id),
        Some(user(2))
    );
    let listed = d.list_invitations(&org(1)).await?;
    assert_eq!(listed[0].state, InvitationState::Accepted);
    assert_eq!(listed[0].accepted_by, Some(user(2)));
    assert!(!d.revoke_invitation(&org(1), &inv(1), now + 3).await?);
    assert_eq!(d.auth_link("ba_user_unknown").await?, None);
    // MCP2 re-check 1: the reverse lookup a person's connected apps use.
    assert_eq!(
        d.auth_users_of(&user(2)).await?,
        vec!["ba_user_1".to_owned()]
    );
    assert!(d.auth_users_of(&user(9)).await?.is_empty());

    // Sign-in gate fix 7: a non-ASCII address is one address however it is
    // cased: invited in capitals, it links with the lowercase form the auth
    // service (JavaScript toLowerCase) reports, and is found either way.
    d.create_invitation(&org(1), invitation(3, "Zoë.ÖLANDER@Exämple.TEST", now))
        .await?;
    assert_eq!(
        d.invitations_for_email("zoë.ölander@exämple.test", now)
            .await?
            .len(),
        1
    );
    let link = d
        .link_identity(
            &org(1),
            LinkRequest {
                auth_user_id: "ba_user_3".into(),
                verified_email: "zoë.ölander@exämple.test".into(),
                user_id: user(3),
                name: "Zoë".into(),
                invitation: Some(inv(3)),
                now_ms: now + 4,
            },
        )
        .await?;
    assert_eq!(link.user_id, user(3));
    assert_eq!(
        d.user_by_verified_email("ZOË.ÖLANDER@EXÄMPLE.TEST")
            .await?
            .map(|u| u.id),
        Some(user(3))
    );
    Ok(())
}

/// AUTH2 refusals (D45): an existing verified member links without an
/// invitation; an unknown email without one, an expired or revoked
/// invitation, another user's email, and an `auth_user_id` already linked
/// to someone else are all refused, with nothing committed.
pub async fn links_refuse_uninvited_and_stale<D: DirectoryStore + AuthLinkStore>(
    d: &D,
) -> Result<(), DirectoryError> {
    let now = 1_000_000;
    d.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 0,
        mutations: vec![
            DirectoryMutation::CreateOrganization {
                title: "Org1".into(),
            },
            DirectoryMutation::CreateUser(UserRecord {
                id: user(1),
                name: "Owner".into(),
                active: true,
                role: OrgRole::Owner,
            }),
            DirectoryMutation::BindVerifiedEmail {
                user_id: user(1),
                email: "Owner@Example.test".into(),
            },
            DirectoryMutation::CreateProject(ProjectRecord {
                id: project(1),
                org_id: org(1),
                slug: "api".into(),
                title: "API".into(),
            }),
        ],
    })
    .await?;
    let request =
        |auth: &str, email: &str, who: u8, invitation: Option<InvitationId>| LinkRequest {
            auth_user_id: auth.into(),
            verified_email: email.into(),
            user_id: user(who),
            name: "Someone".into(),
            invitation,
            now_ms: now,
        };
    // Uninvited stranger: refused, and no user appears.
    assert_eq!(
        d.link_identity(&org(1), request("ba_x", "stranger@example.test", 3, None))
            .await,
        Err(DirectoryError::Invalid("invitation"))
    );
    assert_eq!(
        d.user_by_verified_email("stranger@example.test").await?,
        None
    );
    // Someone claiming the owner's email for a different user: refused.
    assert_eq!(
        d.link_identity(&org(1), request("ba_x", "owner@example.test", 3, None))
            .await,
        Err(DirectoryError::Invalid("user"))
    );
    // The owner links without an invitation (case-insensitive email).
    let revision = d.snapshot(&org(1)).await?.revision;
    let link = d
        .link_identity(&org(1), request("ba_owner", "OWNER@example.test", 1, None))
        .await?;
    assert_eq!(
        (link.user_id.clone(), link.email.as_str()),
        (user(1), "owner@example.test")
    );
    assert_eq!(d.snapshot(&org(1)).await?.revision, revision + 1);
    // The same auth user cannot be linked to another directory user.
    assert_eq!(
        d.link_identity(&org(1), request("ba_owner", "x@example.test", 4, None))
            .await,
        Err(DirectoryError::Duplicate)
    );
    // Expired and revoked invitations cannot be consumed.
    let mut expired = invitation(5, "late@example.test", now - 10);
    expired.expires_at_ms = now;
    d.create_invitation(&org(1), expired).await?;
    assert_eq!(
        d.link_identity(
            &org(1),
            request("ba_late", "late@example.test", 5, Some(inv(5)))
        )
        .await,
        Err(DirectoryError::Invalid("invitation"))
    );
    d.create_invitation(&org(1), invitation(6, "gone@example.test", now))
        .await?;
    assert!(d.revoke_invitation(&org(1), &inv(6), now).await?);
    assert!(!d.revoke_invitation(&org(1), &inv(6), now).await?);
    assert!(
        !d.revoke_invitation(&org(2), &inv(6), now)
            .await
            .unwrap_or(false)
    );
    assert!(
        d.invitations_for_email("gone@example.test", now)
            .await?
            .is_empty()
    );
    assert_eq!(
        d.link_identity(
            &org(1),
            request("ba_gone", "gone@example.test", 6, Some(inv(6)))
        )
        .await,
        Err(DirectoryError::Invalid("invitation"))
    );
    // An invitation for another email cannot be borrowed.
    d.create_invitation(&org(1), invitation(7, "real@example.test", now))
        .await?;
    assert_eq!(
        d.link_identity(
            &org(1),
            request("ba_thief", "thief@example.test", 7, Some(inv(7)))
        )
        .await,
        Err(DirectoryError::Invalid("invitation"))
    );
    assert_eq!(d.auth_link("ba_thief").await?, None);
    assert!(!d.snapshot(&org(1)).await?.users.contains_key(&user(7)));
    // Invitations name existing projects of the organization only.
    let mut foreign = invitation(8, "p@example.test", now);
    foreign.projects = vec![(project(9), ProjectRole::Viewer)];
    assert_eq!(
        d.create_invitation(&org(1), foreign).await,
        Err(DirectoryError::NotFound)
    );
    assert_eq!(
        d.list_invitations(&org(1))
            .await?
            .iter()
            .map(|i| i.state)
            .collect::<Vec<_>>(),
        vec![
            InvitationState::Pending,
            InvitationState::Revoked,
            InvitationState::Pending
        ]
    );
    Ok(())
}
