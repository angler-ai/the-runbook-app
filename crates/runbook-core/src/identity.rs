//! Project role grants; credential scope checks may only narrow these grants.
//! Active-user, tenant membership and step-target checks are separate requirements.
use runbook_model::ProjectRole;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectAction {
    Read,
    Run,
    Write,
    Publish,
    Manage,
}

pub fn role_allows(role: ProjectRole, action: ProjectAction) -> bool {
    match role {
        ProjectRole::Viewer => matches!(action, ProjectAction::Read),
        ProjectRole::Operator => matches!(action, ProjectAction::Read | ProjectAction::Run),
        ProjectRole::Editor => !matches!(action, ProjectAction::Manage),
        ProjectRole::Admin => true,
    }
}

/// `token_allows` must come from verified credential scopes, never request JSON.
pub fn effective_permission(role: ProjectRole, action: ProjectAction, token_allows: bool) -> bool {
    token_allows && role_allows(role, action)
}

use crate::directory::{DirectoryError, DirectorySnapshot};
use runbook_engine::{HandoffRequest, HandoffSource, ResolvedHandoff, VoteEligibility};
use runbook_model::{ProjectId, RecipientRef, TeamId, UserId};

pub fn project_access(
    s: &DirectorySnapshot,
    project: &ProjectId,
    user: &UserId,
    action: ProjectAction,
) -> bool {
    s.projects
        .get(project)
        .is_some_and(|p| p.org_id == s.org_id)
        && s.users.get(user).is_some_and(|u| u.active)
        && s.project_role(project, user)
            .is_some_and(|role| role_allows(role, action))
}
fn matches_user(s: &DirectorySnapshot, user: &UserId, target: &RecipientRef) -> bool {
    match target {
        RecipientRef::User(id) => id == user,
        RecipientRef::Team(id) => id.parse::<TeamId>().ok().is_some_and(|id| {
            s.teams.get(&id).is_some_and(|t| t.org_id == s.org_id)
                && s.team_members.contains(&(id, user.clone()))
        }),
    }
}
/// Targets are the pinned approvers plus any currently activated escalation targets.
pub fn vote_eligibility(
    s: &DirectorySnapshot,
    project: &ProjectId,
    user: &UserId,
    targets: &[RecipientRef],
) -> VoteEligibility {
    let mut matched = vec![];
    if project_access(s, project, user, ProjectAction::Run) {
        for target in targets {
            if matches_user(s, user, target) && !matched.contains(target) {
                matched.push(target.clone());
            }
        }
    }
    VoteEligibility {
        eligible: !matched.is_empty(),
        directory_revision: s.revision,
        matched_recipients: matched,
    }
}
/// Static existence validation does not require a user to be active or eligible:
/// those facts are rechecked at each action, rather than frozen at publication.
pub fn resolve_recipients(
    s: &DirectorySnapshot,
    project: &ProjectId,
    refs: &[String],
) -> Result<Vec<RecipientRef>, DirectoryError> {
    if !s
        .projects
        .get(project)
        .is_some_and(|p| p.org_id == s.org_id)
    {
        return Err(DirectoryError::NotFound);
    }
    let mut out = vec![];
    for reference in refs {
        let (kind, value) = reference
            .split_once(':')
            .ok_or(DirectoryError::Invalid("recipient"))?;
        let resolved = match kind {
            "user" => {
                let id = value
                    .parse::<UserId>()
                    .map_err(|_| DirectoryError::NotFound)?;
                if !s.users.contains_key(&id) {
                    return Err(DirectoryError::NotFound);
                }
                RecipientRef::User(id)
            }
            "team" => {
                let team = if let Ok(id) = value.parse::<TeamId>() {
                    s.teams.get(&id)
                } else {
                    s.teams.values().find(|t| t.slug == value)
                }
                .filter(|t| t.org_id == s.org_id)
                .ok_or(DirectoryError::NotFound)?;
                RecipientRef::Team(team.id.to_string())
            }
            _ => return Err(DirectoryError::Invalid("recipient")),
        };
        if !out.contains(&resolved) {
            out.push(resolved)
        }
    }
    Ok(out)
}
/// The runbook's user bindings: the user each `user:<name>` it names was
/// pinned to at publish (`CompiledRunbook::users`).
pub type UserBindings = std::collections::BTreeMap<String, UserId>;

/// The targets among `refs` that name at least one active user with project
/// run access (a handoff's or a strict assignee's eligible recipients). A
/// user written by name resolves through the runbook's `users` bindings (a
/// name an expression produced that the runbook never bound is read as a
/// user ID).
fn eligible_targets(
    s: &DirectorySnapshot,
    project: &ProjectId,
    refs: &[runbook_spec::ir::Recipient],
    users: &UserBindings,
) -> Vec<RecipientRef> {
    use runbook_spec::ir::Recipient;
    let mut eligible = vec![];
    for r in refs {
        let text = match r {
            Recipient::User(v) => match users.get(v) {
                Some(id) => format!("user:{id}"),
                None => format!("user:{v}"),
            },
            Recipient::Team(v) => format!("team:{v}"),
        };
        for target in resolve_recipients(s, project, &[text]).unwrap_or_default() {
            if !eligible.contains(&target)
                && s.users.keys().any(|user| {
                    project_access(s, project, user, ProjectAction::Run)
                        && matches_user(s, user, &target)
                })
            {
                eligible.push(target);
            }
        }
    }
    eligible
}
/// Who hears about a routing error: the project's own active admins, else
/// (a project nobody administers directly) the org's owners and admins, who
/// are admins of every project. Not both: org admins would otherwise hear
/// about every project's routing errors.
fn project_admins(s: &DirectorySnapshot, project: &ProjectId) -> Vec<RecipientRef> {
    let explicit: Vec<RecipientRef> = s
        .users
        .values()
        .filter(|u| {
            u.active
                && s.project_members.get(&(project.clone(), u.id.clone()))
                    == Some(&runbook_model::ProjectRole::Admin)
        })
        .map(|u| RecipientRef::User(u.id.clone()))
        .collect();
    if !explicit.is_empty() {
        return explicit;
    }
    s.users
        .keys()
        .filter(|u| project_access(s, project, u, ProjectAction::Manage))
        .cloned()
        .map(RecipientRef::User)
        .collect()
}
/// LANG7: a strict task's assignee, resolved for its attempt. Nobody
/// eligible leaves the step with a routing error and notifies the project
/// admins; it never falls back to the owners.
pub fn assignee_recipients(
    s: &DirectorySnapshot,
    project: &ProjectId,
    request: runbook_engine::AssigneeRequest,
    users: &UserBindings,
) -> runbook_engine::ResolvedAssignee {
    let eligible = eligible_targets(s, project, request.assignee.as_slice(), users);
    runbook_engine::ResolvedAssignee {
        notification_recipients: if eligible.is_empty() {
            project_admins(s, project)
        } else {
            vec![]
        },
        eligible_recipients: eligible,
        request,
        directory_revision: s.revision,
    }
}
/// LANG7: whether `user` may claim a strict task resolved as `resolved`:
/// an active user with project run access who is the assignee or a member
/// of the assignee team, checked against the live directory.
pub fn strict_assignee_allows(
    s: &DirectorySnapshot,
    project: &ProjectId,
    user: &UserId,
    resolved: &runbook_engine::ResolvedAssignee,
) -> bool {
    vote_eligibility(s, project, user, &resolved.eligible_recipients).eligible
}
pub fn handoff_recipients(
    s: &DirectorySnapshot,
    project: &ProjectId,
    request: HandoffRequest,
    users: &UserBindings,
) -> ResolvedHandoff {
    let resolve = |refs: &[runbook_spec::ir::Recipient]| eligible_targets(s, project, refs, users);
    let mut source = HandoffSource::Assignee;
    let mut eligible = resolve(request.assignee.as_slice());
    // LANG7: a strict task never hands off past its assignee.
    if eligible.is_empty() && !request.strict {
        source = HandoffSource::Owners;
        eligible = resolve(&request.owners);
    }
    let unresolved = eligible.is_empty();
    let notifications = if unresolved {
        project_admins(s, project)
    } else {
        eligible.clone()
    };
    ResolvedHandoff {
        request,
        source: if unresolved {
            HandoffSource::Unresolved
        } else {
            source
        },
        eligible_recipients: eligible,
        notification_recipients: notifications,
        directory_revision: s.revision,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrgAction {
    CreateUser,
    CreateTeam,
    CreateProject,
    ManageMemberships,
    ManageTokens,
}
pub fn org_action_allowed(role: crate::directory::OrgRole, _action: OrgAction) -> bool {
    matches!(
        role,
        crate::directory::OrgRole::Owner | crate::directory::OrgRole::Admin
    )
}
