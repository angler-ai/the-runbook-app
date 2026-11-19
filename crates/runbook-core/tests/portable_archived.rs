//! Review fix (2026-11-18): an archived project stays archived through an
//! export and import, instead of arriving live with its triggers on.
use runbook_core::directory::{DirectoryMutation, OrganizationRecord, ProjectRecord};
use runbook_core::portable::{DirectorySlice, ImportOptions, directory_commit};
use std::collections::BTreeSet;

fn slice(archived: bool) -> DirectorySlice {
    let org = "org_00000000000000000000000001".parse().unwrap();
    DirectorySlice {
        organization: OrganizationRecord {
            id: "org_00000000000000000000000001".parse().unwrap(),
            title: "Acme".into(),
        },
        project: ProjectRecord {
            id: "prj_00000000000000000000000001".parse().unwrap(),
            org_id: org,
            slug: "app".into(),
            title: "App".into(),
        },
        archived,
        users: vec![],
        teams: vec![],
        project_members: vec![],
        team_members: vec![],
    }
}

fn archive_mutations(archived: bool) -> Vec<DirectoryMutation> {
    directory_commit(
        &slice(archived),
        None,
        &BTreeSet::new(),
        ImportOptions {
            into_existing_org: false,
        },
    )
    .unwrap()
    .unwrap()
    .mutations
    .into_iter()
    .filter(|m| matches!(m, DirectoryMutation::SetProjectArchived { .. }))
    .collect()
}

#[test]
fn an_archived_project_is_imported_archived() {
    let found = archive_mutations(true);
    assert_eq!(found.len(), 1);
    assert!(matches!(
        &found[0],
        DirectoryMutation::SetProjectArchived { archived: true, .. }
    ));
}

#[test]
fn a_live_project_gets_no_archive_mutation() {
    assert!(archive_mutations(false).is_empty());
}
