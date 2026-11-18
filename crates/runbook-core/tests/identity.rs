use runbook_core::identity::{ProjectAction, effective_permission, role_allows};
use runbook_model::ProjectRole;

#[test]
fn project_role_permission_matrix() {
    use ProjectAction::*;
    for (role, expected) in [
        (ProjectRole::Viewer, [true, false, false, false, false]),
        (ProjectRole::Operator, [true, true, false, false, false]),
        (ProjectRole::Editor, [true, true, true, true, false]),
        (ProjectRole::Admin, [true; 5]),
    ] {
        for (action, allowed) in [Read, Run, Write, Publish, Manage]
            .into_iter()
            .zip(expected)
        {
            assert_eq!(role_allows(role, action), allowed, "{role:?}/{action:?}");
        }
    }
}

#[test]
fn token_grants_only_narrow_roles() {
    use ProjectAction::*;
    assert!(!effective_permission(ProjectRole::Editor, Publish, false));
    assert!(!effective_permission(ProjectRole::Viewer, Publish, true));
    assert!(effective_permission(ProjectRole::Editor, Write, true));
    for role in [
        ProjectRole::Viewer,
        ProjectRole::Operator,
        ProjectRole::Editor,
        ProjectRole::Admin,
    ] {
        for action in [Read, Run, Write, Publish, Manage] {
            assert!(!effective_permission(role, action, false));
            assert_eq!(
                effective_permission(role, action, true),
                role_allows(role, action)
            );
        }
    }
}
