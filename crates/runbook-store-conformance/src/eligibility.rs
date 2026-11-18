//! B1.3 eligibility cases, moved unchanged from runbook-core tests (B2.2b).
//! Each case builds the same seeded fixture through a generic DirectoryStore.
use crate::DirectoryFactory;
use runbook_core::{directory::*, identity::*};
use runbook_engine::{HandoffRequest, HandoffSource};
use runbook_model::{
    OrganizationId, ProjectId, ProjectRole, RecipientRef, ReportStatus, TeamId, UserId,
};
use runbook_spec::ir::Recipient;
pub fn org() -> OrganizationId {
    crate::directory::org(1)
}
pub fn project() -> ProjectId {
    crate::directory::project(1)
}
pub fn user(n: u8) -> UserId {
    crate::directory::user(n)
}
pub fn team() -> TeamId {
    crate::directory::team(1)
}
async fn fixture<F: DirectoryFactory>(f: &F) -> Result<F::Store, DirectoryError> {
    let store = f.create_directory().await?;
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Test".into(),
        },
        DirectoryMutation::CreateProject(ProjectRecord {
            id: project(),
            org_id: org(),
            slug: "api".into(),
            title: "API".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: team(),
            org_id: org(),
            slug: "oncall".into(),
            title: "Oncall".into(),
        }),
    ];
    for (n, name, role) in [
        (1, "alice", Some(ProjectRole::Operator)),
        (2, "bob", Some(ProjectRole::Operator)),
        (3, "admin", Some(ProjectRole::Admin)),
        (4, "viewer", Some(ProjectRole::Viewer)),
        (5, "outsider", None),
    ] {
        mutations.push(DirectoryMutation::CreateUser(UserRecord {
            id: user(n),
            name: name.into(),
            active: true,
            role: OrgRole::Member,
        }));
        if let Some(role) = role {
            mutations.push(DirectoryMutation::GrantProjectRole {
                project_id: project(),
                user_id: user(n),
                role,
            })
        }
        if n != 3 {
            mutations.push(DirectoryMutation::AddTeamMember {
                team_id: team(),
                user_id: user(n),
            })
        }
    }
    store
        .commit(DirectoryCommit {
            org_id: org(),
            expected_revision: 0,
            mutations,
        })
        .await
        .unwrap();
    Ok(store)
}
fn targets() -> Vec<RecipientRef> {
    vec![RecipientRef::Team(team().to_string())]
}
pub async fn team_membership_does_not_grant_project_access<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let store = fixture(f).await?;
    let s = store.snapshot(&org()).await.unwrap();
    assert!(!project_access(
        &s,
        &project(),
        &user(5),
        ProjectAction::Read
    ));
    assert!(!project_access(
        &s,
        &project(),
        &user(4),
        ProjectAction::Run
    ));
    assert!(project_access(&s, &project(), &user(1), ProjectAction::Run));
    let foreign = "prj_00000000000000000000000009".parse().unwrap();
    assert!(!project_access(&s, &foreign, &user(3), ProjectAction::Read));
    store
        .commit(DirectoryCommit {
            org_id: org(),
            expected_revision: 1,
            mutations: vec![DirectoryMutation::SetUserActive {
                user_id: user(1),
                active: false,
            }],
        })
        .await
        .unwrap();
    assert!(!project_access(
        &store.snapshot(&org()).await.unwrap(),
        &project(),
        &user(1),
        ProjectAction::Run
    ));
    Ok(())
}
pub async fn approval_requires_current_target_match<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let store = fixture(f).await?;
    let s = store.snapshot(&org()).await.unwrap();
    assert!(!vote_eligibility(&s, &project(), &user(3), &targets()).eligible);
    let yes = vote_eligibility(&s, &project(), &user(1), &targets());
    assert!(yes.eligible);
    assert_eq!(yes.directory_revision, 1);
    assert_eq!(yes.matched_recipients, targets());
    store
        .commit(DirectoryCommit {
            org_id: org(),
            expected_revision: 1,
            mutations: vec![DirectoryMutation::RemoveTeamMember {
                team_id: team(),
                user_id: user(1),
            }],
        })
        .await
        .unwrap();
    let no = vote_eligibility(
        &store.snapshot(&org()).await.unwrap(),
        &project(),
        &user(1),
        &targets(),
    );
    assert!(!no.eligible);
    assert_eq!(no.directory_revision, 2);
    assert!(no.matched_recipients.is_empty());
    Ok(())
}
pub async fn unknown_static_reference_fails_publication<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = fixture(f).await?.snapshot(&org()).await.unwrap();
    for r in [
        "team:missing".into(),
        format!("user:{}", user(9)),
        "unknown:oncall".into(),
    ] {
        assert!(resolve_recipients(&s, &project(), &[r]).is_err())
    }
    assert_eq!(
        resolve_recipients(
            &s,
            &project(),
            &["team:oncall".into(), format!("team:{}", team())]
        )
        .unwrap(),
        targets()
    );
    assert_eq!(
        resolve_recipients(&s, &project(), &[format!("user:{}", user(1))]).unwrap(),
        vec![RecipientRef::User(user(1))]
    );
    Ok(())
}
fn request() -> HandoffRequest {
    HandoffRequest {
        step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
        base_sequence: 1,
        attempt: 1,
        reason: ReportStatus::Blocked,
        assignee: None,
        owners: vec![],
        reads: vec![],
        strict: false,
    }
}
pub async fn handoff_missing_route_notifies_admins_without_broadening<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = fixture(f).await?.snapshot(&org()).await.unwrap();
    let r = handoff_recipients(&s, &project(), request(), &Default::default());
    assert_eq!(r.source, HandoffSource::Unresolved);
    assert!(r.eligible_recipients.is_empty());
    assert_eq!(r.notification_recipients, vec![RecipientRef::User(user(3))]);
    let mut q = request();
    q.assignee = Some(Recipient::User(user(1).to_string()));
    q.owners = vec![Recipient::User(user(2).to_string())];
    let r = handoff_recipients(&s, &project(), q, &Default::default());
    assert_eq!(r.source, HandoffSource::Assignee);
    assert_eq!(r.eligible_recipients, vec![RecipientRef::User(user(1))]);
    let mut q = request();
    q.assignee = Some(Recipient::User(user(9).to_string()));
    q.owners = vec![Recipient::User(user(2).to_string())];
    assert_eq!(
        handoff_recipients(&s, &project(), q, &Default::default()).source,
        HandoffSource::Owners
    );
    // LANG7 review round 1: a user written by name resolves through the
    // runbook's bindings (pinned at publish), never by a looser match.
    let bindings = [("alice".to_string(), user(1))].into();
    let mut q = request();
    q.owners = vec![Recipient::User("alice".into())];
    let r = handoff_recipients(&s, &project(), q.clone(), &bindings);
    assert_eq!(r.source, HandoffSource::Owners);
    assert_eq!(r.eligible_recipients, vec![RecipientRef::User(user(1))]);
    assert_eq!(
        handoff_recipients(&s, &project(), q, &Default::default()).source,
        HandoffSource::Unresolved
    );
    Ok(())
}

#[path = "../../runbook-engine/tests/support/mod.rs"]
mod engine_support;
pub async fn two_users_not_two_tokens_satisfy_quorum<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    use runbook_engine::{ResolvedVote, apply_vote};
    use runbook_model::ActorRef;
    let store = fixture(f).await?;
    let mut context = runbook_spec::CompileContext::default();
    context.teams.insert("oncall".into(), team());
    let mut trace = engine_support::run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [keep], approvers: {teams: [oncall]}, min: 2}}]}",
        &[],
        context,
    );
    for (n, token) in [(1, "pat-a"), (1, "pat-b")] {
        let s = store.snapshot(&org()).await.unwrap();
        let vote = ResolvedVote {
            at_ms: trace.state.at_ms,
            base_sequence: trace.state.sequence,
            step_run_id: trace.step("gate").id.clone().unwrap(),
            actor: ActorRef::Human { user_id: user(n) },
            credential_ref: token.into(),
            option: "keep".into(),
            comment: None,
            eligibility: vote_eligibility(&s, &project(), &user(n), &targets()),
        };
        for event in apply_vote(&trace.state, &vote).unwrap() {
            trace.append(event, vote.actor.clone(), trace.state.at_ms)
        }
        trace.settle();
    }
    assert_eq!(trace.count("ApprovalVoted"), 1);
    assert_eq!(trace.count("ApprovalDecided"), 0);
    store
        .commit(DirectoryCommit {
            org_id: org(),
            expected_revision: 1,
            mutations: vec![DirectoryMutation::RemoveTeamMember {
                team_id: team(),
                user_id: user(1),
            }],
        })
        .await
        .unwrap();
    let s = store.snapshot(&org()).await.unwrap();
    let make_vote = |n| ResolvedVote {
        at_ms: trace.state.at_ms,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        actor: ActorRef::Human { user_id: user(n) },
        credential_ref: "pat-current".into(),
        option: "keep".into(),
        comment: None,
        eligibility: vote_eligibility(&s, &project(), &user(n), &targets()),
    };
    assert_eq!(
        apply_vote(&trace.state, &make_vote(1)).unwrap_err().code(),
        "approval.not_eligible"
    );
    let vote = make_vote(2);
    for event in apply_vote(&trace.state, &vote).unwrap() {
        trace.append(event, vote.actor.clone(), trace.state.at_ms)
    }
    trace.settle();
    assert_eq!(trace.count("ApprovalVoted"), 2);
    assert_eq!(trace.count("ApprovalDecided"), 1);
    let votes = &trace.step("gate").approval.as_ref().unwrap().votes;
    assert_eq!(votes[0].vote.eligibility.directory_revision, 1);
    assert_eq!(votes[1].vote.eligibility.directory_revision, 2);
    assert_eq!(trace.state, trace.replay());
    Ok(())
}

pub async fn unresolved_handoff_without_admin_still_records_blocked_route<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    use runbook_engine::{Command, Event, WorkReport, decide};
    use runbook_model::{ActorRef, StepStatus};
    let store = fixture(f).await?;
    store
        .commit(DirectoryCommit {
            org_id: org(),
            expected_revision: 1,
            mutations: vec![DirectoryMutation::SetUserActive {
                user_id: user(3),
                active: false,
            }],
        })
        .await
        .unwrap();
    let s = store.snapshot(&org()).await.unwrap();
    let mut trace = engine_support::run_case("spec: {steps: [{id: work, title: Work}]}", &[]);
    trace.append(
        Event::StepReported {
            step_run_id: trace.step("work").id.clone().unwrap(),
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Blocked,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        ActorRef::Human { user_id: user(1) },
        trace.state.at_ms,
    );
    let request = decide(&trace.state)
        .unwrap()
        .into_iter()
        .find_map(|c| {
            if let Command::ResolveHandoff { request } = c {
                Some(*request)
            } else {
                None
            }
        })
        .unwrap();
    trace.append(
        Event::StepHandedOff {
            handoff: Box::new(handoff_recipients(
                &s,
                &project(),
                request,
                &Default::default(),
            )),
        },
        ActorRef::System,
        trace.state.at_ms,
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    trace.settle();
    assert_eq!(trace.step("work").status, StepStatus::Waiting);
    assert_eq!(
        trace.step("work").routing_error.as_deref(),
        Some("handoff.no_recipient")
    );
    assert!(!trace.step("work").recovery_pending);
    assert!(
        trace
            .step("work")
            .handoff
            .as_ref()
            .unwrap()
            .eligible_recipients
            .is_empty()
    );
    assert_eq!(trace.count("StepBlockedNotificationQueued"), 1);
    assert_eq!(trace.state, trace.replay());
    Ok(())
}
