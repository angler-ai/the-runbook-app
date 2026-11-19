//! An org owner or admin is admin of every project in the org without a
//! grant (2026-11-17); a member holds only what was granted; a
//! deactivated admin and another org's project get nothing.
use runbook_core::{
    directory::{DirectorySnapshot, OrgRole, OrganizationRecord, ProjectRecord, UserRecord},
    identity::{ProjectAction, project_access},
};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, UserId};

fn org(n: u8) -> OrganizationId {
    format!("org_{n:026}").parse().unwrap()
}
fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn project(n: u8) -> ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}

/// Admin 1 (no grants), owner 2 (no grants), member 3 (viewer of project 1),
/// admin 4 (deactivated); projects 1 and 2 in the org, 9 in another org.
fn snapshot() -> DirectorySnapshot {
    let mut s = DirectorySnapshot {
        org_id: org(1),
        revision: 1,
        organization: OrganizationRecord {
            id: org(1),
            title: "Org".into(),
        },
        users: Default::default(),
        projects: Default::default(),
        teams: Default::default(),
        project_members: Default::default(),
        team_members: Default::default(),
        archived_projects: Default::default(),
    };
    for (n, role, active) in [
        (1, OrgRole::Admin, true),
        (2, OrgRole::Owner, true),
        (3, OrgRole::Member, true),
        (4, OrgRole::Admin, false),
    ] {
        s.users.insert(
            user(n),
            UserRecord {
                id: user(n),
                name: format!("u{n}"),
                active,
                role,
            },
        );
    }
    for (n, owner) in [(1, org(1)), (2, org(1)), (9, org(2))] {
        s.projects.insert(
            project(n),
            ProjectRecord {
                id: project(n),
                org_id: owner,
                slug: format!("p{n}"),
                title: format!("P{n}"),
            },
        );
    }
    s.project_members
        .insert((project(1), user(3)), ProjectRole::Viewer);
    s
}

#[test]
fn org_admins_and_owners_are_admin_of_every_project() {
    let s = snapshot();
    for n in [1, 2] {
        for p in [1, 2] {
            assert_eq!(
                s.project_role(&project(p), &user(n)),
                Some(ProjectRole::Admin)
            );
            assert!(project_access(
                &s,
                &project(p),
                &user(n),
                ProjectAction::Manage
            ));
        }
    }
}

#[test]
fn members_hold_only_their_grants() {
    let s = snapshot();
    assert_eq!(
        s.project_role(&project(1), &user(3)),
        Some(ProjectRole::Viewer)
    );
    assert!(project_access(
        &s,
        &project(1),
        &user(3),
        ProjectAction::Read
    ));
    assert!(!project_access(
        &s,
        &project(1),
        &user(3),
        ProjectAction::Run
    ));
    assert_eq!(s.project_role(&project(2), &user(3)), None);
    assert!(!project_access(
        &s,
        &project(2),
        &user(3),
        ProjectAction::Read
    ));
}

#[test]
fn a_deactivated_admin_and_another_orgs_project_get_nothing() {
    let s = snapshot();
    assert_eq!(s.project_role(&project(1), &user(4)), None);
    assert_eq!(s.project_role(&project(9), &user(1)), None);
    assert!(!project_access(
        &s,
        &project(9),
        &user(1),
        ProjectAction::Read
    ));
}
