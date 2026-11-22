//! NOTIFY1 fan-out: recipients expand through teams, only active people
//! with verified addresses get rows, preferences decide now/digest/drop,
//! and a failed run with no named recipients mails its owners.
use runbook_core::{
    catalog::TenantScope,
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, OrgRole, ProjectRecord, TeamRecord,
        UserRecord, memory::MemoryDirectory,
    },
    notify::{
        EmailPreference, EmailState, NotifyStore, fanout::fan_out_email, memory::MemoryNotify,
    },
};
use runbook_engine::NotificationIntent;
use runbook_model::{ProjectRole, RecipientRef, UserId};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory stores must be synchronous"),
    }
}
fn org() -> runbook_model::OrganizationId {
    "org_00000000000000000000000001".parse().unwrap()
}
fn project() -> runbook_model::ProjectId {
    "prj_00000000000000000000000001".parse().unwrap()
}
fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn team_id() -> runbook_model::TeamId {
    "team_00000000000000000000000001".parse().unwrap()
}
fn scope() -> TenantScope {
    TenantScope {
        org_id: org(),
        project_id: project(),
    }
}

fn intent(kind: &str, recipients: Vec<RecipientRef>) -> NotificationIntent {
    NotificationIntent {
        event_id: format!("evt_{kind}"),
        kind: kind.into(),
        execution_id: "exe_00000000000000000000000001".parse().unwrap(),
        sequence: 3,
        at_ms: 1_700_000_000_000,
        step_run_ids: vec!["sr_00000000000000000000000001".parse().unwrap()],
        recipients,
        reason: None,
        link: "/x/exe_00000000000000000000000001".into(),
        ready_steps: vec![runbook_model::ReadyWork {
            step_run_id: "sr_00000000000000000000000001".parse().unwrap(),
            path: "signoff".into(),
            title: "Sign off".into(),
            actor_kind: runbook_model::ActorKind::Human,
            agent_id: None,
            group_id: None,
        }],
        params: None,
        detail: None,
    }
}

async fn seeded() -> MemoryDirectory {
    let directory = MemoryDirectory::default();
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Acme".into(),
        },
        DirectoryMutation::CreateUser(UserRecord {
            id: user(1),
            name: "Olivia Owner".into(),
            active: true,
            role: OrgRole::Owner,
        }),
        DirectoryMutation::CreateUser(UserRecord {
            id: user(2),
            name: "Mia Member".into(),
            active: true,
            role: OrgRole::Member,
        }),
        DirectoryMutation::CreateUser(UserRecord {
            id: user(3),
            name: "Ned NoMail".into(),
            active: true,
            role: OrgRole::Member,
        }),
        DirectoryMutation::CreateProject(ProjectRecord {
            id: project(),
            org_id: org(),
            slug: "prod".into(),
            title: "Production".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: team_id(),
            org_id: org(),
            slug: "oncall".into(),
            title: "Oncall".into(),
        }),
        DirectoryMutation::AddTeamMember {
            team_id: team_id(),
            user_id: user(2),
        },
        DirectoryMutation::AddTeamMember {
            team_id: team_id(),
            user_id: user(3),
        },
        DirectoryMutation::GrantProjectRole {
            project_id: project(),
            user_id: user(1),
            role: ProjectRole::Admin,
        },
        DirectoryMutation::BindVerifiedEmail {
            user_id: user(1),
            email: "olivia@example.test".into(),
        },
        DirectoryMutation::BindVerifiedEmail {
            user_id: user(2),
            email: "mia@example.test".into(),
        },
    ];
    directory
        .commit(DirectoryCommit {
            org_id: org(),
            expected_revision: 0,
            mutations: std::mem::take(&mut mutations),
        })
        .await
        .unwrap();
    directory
}

#[test]
fn fan_out_expands_teams_skips_unverified_and_honours_preferences() {
    ready(async {
        let directory = seeded().await;
        let notify = MemoryNotify::new(1_700_000_001_000);
        notify
            .set_email_preference(&user(1), EmailPreference::None)
            .await
            .unwrap();
        notify
            .set_email_preference(&user(2), EmailPreference::DailySummary)
            .await
            .unwrap();
        let out = fan_out_email(
            &notify,
            &directory,
            &scope(),
            &intent(
                "approval.requested",
                vec![
                    RecipientRef::User(user(1)),
                    RecipientRef::Team("oncall".into()),
                ],
            ),
            "Notify run",
            "https://runs.example.test",
            || "salt-1".into(),
        )
        .await
        .unwrap();
        // Olivia opted out, Ned has no verified address: only Mia, digested.
        assert_eq!(out.recorded, 1);
        assert_eq!(out.digest, 1);
        let claimed = notify.claim_digest(i64::MAX, 10, 60_000).await.unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].record.email, "mia@example.test");
        assert_eq!(claimed[0].record.state, EmailState::Digest);
        assert!(
            claimed[0]
                .record
                .subject
                .contains("Approval needed: Sign off")
        );
        assert!(
            claimed[0].record.text.contains("Acme / Production")
                || claimed[0].record.subject.contains("Acme / Production")
        );
    });
}

#[test]
fn a_failed_run_with_no_recipients_mails_its_owners() {
    ready(async {
        let directory = seeded().await;
        let notify = MemoryNotify::new(1_700_000_001_000);
        let mut failed = intent("execution.completed", vec![]);
        failed.detail = Some(serde_json::json!({"status": "failed"}));
        let out = fan_out_email(
            &notify,
            &directory,
            &scope(),
            &failed,
            "Notify run",
            "https://runs.example.test",
            || "salt-1".into(),
        )
        .await
        .unwrap();
        assert_eq!(out.recorded, 1);
        let leases = notify.claim_emails(10, 60_000).await.unwrap();
        assert_eq!(leases[0].record.email, "olivia@example.test");
        assert!(leases[0].record.subject.contains("Run failed"));
    });
}
