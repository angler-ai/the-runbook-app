//! R27: `/v1/me/tasks` is one indexed query. It must return exactly what
//! the previous path returned: per-project `inbox` lists merged in memory
//! (kept below as `reference_page`), page by page with cursors, over a
//! scenario with several projects, teams, escalated recipients, votes
//! already cast, handoffs, a paused run, and another tenant.
mod support;
use runbook_core::{
    ExecutionStore,
    authoring::{AuthoringFactory, AuthoringStore},
    catalog::{Assignment, AssignmentKind, CatalogStore, TenantScope},
    directory::{
        DirectoryCommit, DirectoryMutation, DirectorySnapshot, DirectoryStore, OrgRole,
        ProjectRecord, TeamRecord, UserRecord,
    },
    inbox::{InboxFilters, MeTaskRow, MeTasksPage, inbox, me_tasks_scope, merge_pages},
};
use runbook_model::{
    ExecutionStatus, OrganizationId, ProjectId, ProjectRole, RecipientRef, UserId,
};
use runbook_store_conformance::catalog;
use runbook_store_postgres::{PostgresAuthoring, PostgresDirectory};
use std::collections::BTreeMap;
use support::factory::{PostgresFactory, with_factory};

fn org(n: u8) -> OrganizationId {
    format!("org_{n:026}").parse().unwrap()
}
fn prj(n: u8) -> ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}
fn usr(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn team(n: u8) -> String {
    format!("team_{n:026}")
}
fn sr(n: u8, k: u8) -> runbook_model::StepRunId {
    format!("sr_{n:024}{k:02}").parse().unwrap()
}
fn user(n: u8) -> RecipientRef {
    RecipientRef::User(usr(n))
}
fn group(n: u8) -> RecipientRef {
    RecipientRef::Team(team(n))
}

struct Seed {
    exe: u8,
    org: u8,
    project: u8,
    status: ExecutionStatus,
    steps: Vec<Step>,
}
/// (step k, kind, recipients, voted_by, updated_at_ms)
type Step = (u8, AssignmentKind, Vec<RecipientRef>, Vec<u8>, i64);

async fn seed(f: &PostgresFactory, s: Seed) {
    let tenant = TenantScope {
        org_id: org(s.org),
        project_id: prj(s.project),
    };
    let store = f.fresh_store(tenant).await;
    let mut batch = catalog::batch(s.exe, 0, 1, s.status);
    batch.derived_outbox[0].summary.title = format!("Run {}", s.exe);
    batch.derived_outbox[0].assignments = s
        .steps
        .into_iter()
        .map(|(k, kind, recipients, voted, updated)| Assignment {
            execution_id: catalog::id(s.exe),
            step_run_id: sr(s.exe, k),
            recipients,
            kind,
            sequence: 1,
            updated_at_ms: updated,
            voted_by: voted.into_iter().map(usr).collect(),
            path: format!("step_{k}"),
            title: format!("Step {}.{k}", s.exe),
        })
        .collect();
    store.commit(batch).await.unwrap();
}

async fn directory(f: &PostgresFactory) {
    let dir = f.fresh_directory();
    let person = |n: u8, active: bool| {
        DirectoryMutation::CreateUser(UserRecord {
            id: usr(n),
            name: format!("user{n}"),
            active,
            role: OrgRole::Member,
        })
    };
    let project = |o: u8, p: u8| {
        DirectoryMutation::CreateProject(ProjectRecord {
            id: prj(p),
            org_id: org(o),
            slug: format!("p{p}"),
            title: format!("P{p}"),
        })
    };
    let squad = |o: u8, t: u8| {
        DirectoryMutation::CreateTeam(TeamRecord {
            id: team(t).parse().unwrap(),
            org_id: org(o),
            slug: format!("t{t}"),
            title: format!("T{t}"),
        })
    };
    let member = |p: u8, u: u8| DirectoryMutation::GrantProjectRole {
        project_id: prj(p),
        user_id: usr(u),
        role: ProjectRole::Operator,
    };
    let joins = |t: u8, u: u8| DirectoryMutation::AddTeamMember {
        team_id: team(t).parse().unwrap(),
        user_id: usr(u),
    };
    // Org 1: projects 1-3; alice (1) and bob (2) on oncall (team 1),
    // carol (3) and alice on leads (team 2); dave (4) is only in project 3;
    // erin (5) is deactivated. Alice is not a member of project 3.
    let mut a = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org one".into(),
        },
        person(1, true),
        person(2, true),
        person(3, true),
        person(4, true),
        person(5, false),
        project(1, 1),
        project(1, 2),
        project(1, 3),
        squad(1, 1),
        squad(1, 2),
    ];
    for (p, u) in [
        (1, 1),
        (1, 2),
        (1, 3),
        (1, 5),
        (2, 1),
        (2, 2),
        (2, 3),
        (3, 4),
    ] {
        a.push(member(p, u));
    }
    for (t, u) in [(1, 1), (1, 2), (2, 3), (2, 1), (1, 5)] {
        a.push(joins(t, u));
    }
    // Org 2 (another tenant): alice is a member there too, of project 9
    // and team 9, with open work of her own.
    let b = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org two".into(),
        },
        person(1, true),
        project(2, 9),
        squad(2, 9),
        member(9, 1),
        joins(9, 1),
    ];
    for (o, mutations) in [(1, a), (2, b)] {
        dir.commit(DirectoryCommit {
            org_id: org(o),
            expected_revision: 0,
            mutations,
        })
        .await
        .unwrap();
    }
}

async fn scenario(f: &PostgresFactory) {
    directory(f).await;
    use AssignmentKind::{Approval, Handoff, Work};
    let running = ExecutionStatus::Running;
    let seeds = vec![
        // Project 1.
        Seed {
            exe: 1,
            org: 1,
            project: 1,
            status: running,
            steps: vec![(1, Work, vec![user(1)], vec![], 100)],
        },
        // Approval expanded to oncall users; bob already voted.
        Seed {
            exe: 2,
            org: 1,
            project: 1,
            status: ExecutionStatus::Waiting,
            steps: vec![(1, Approval, vec![user(1), user(2)], vec![2], 300)],
        },
        // Escalated approval: original approvers plus team leads; alice voted.
        Seed {
            exe: 3,
            org: 1,
            project: 1,
            status: ExecutionStatus::Waiting,
            steps: vec![(1, Approval, vec![user(1), user(2), group(2)], vec![1], 300)],
        },
        // Handoff to bob only.
        Seed {
            exe: 4,
            org: 1,
            project: 1,
            status: running,
            steps: vec![(1, Handoff, vec![user(2)], vec![], 250)],
        },
        // A paused run keeps its open work for team leads.
        Seed {
            exe: 5,
            org: 1,
            project: 1,
            status: ExecutionStatus::Paused,
            steps: vec![(1, Work, vec![group(2)], vec![], 200)],
        },
        // Alice matches twice (self and team); listed once.
        Seed {
            exe: 6,
            org: 1,
            project: 1,
            status: running,
            steps: vec![(1, Work, vec![user(1), group(1)], vec![], 400)],
        },
        // Two open steps in one run with the same timestamp.
        Seed {
            exe: 7,
            org: 1,
            project: 1,
            status: running,
            steps: vec![
                (1, Work, vec![group(1)], vec![], 150),
                (2, Approval, vec![user(1)], vec![], 150),
            ],
        },
        // Legacy-shaped row: no timestamp yet.
        Seed {
            exe: 14,
            org: 1,
            project: 1,
            status: running,
            steps: vec![(1, Work, vec![user(1), user(5)], vec![], 0)],
        },
        // Project 2 (timestamps tie across projects).
        Seed {
            exe: 8,
            org: 1,
            project: 2,
            status: running,
            steps: vec![(1, Work, vec![user(1)], vec![], 300)],
        },
        Seed {
            exe: 9,
            org: 1,
            project: 2,
            status: running,
            steps: vec![(1, Handoff, vec![group(1)], vec![], 50)],
        },
        Seed {
            exe: 10,
            org: 1,
            project: 2,
            status: ExecutionStatus::Waiting,
            steps: vec![(1, Approval, vec![group(2)], vec![3], 500)],
        },
        // Project 3: alice is named but is not a project member.
        Seed {
            exe: 11,
            org: 1,
            project: 3,
            status: running,
            steps: vec![(1, Work, vec![user(1), user(4)], vec![], 600)],
        },
        // Org 2 (another tenant): alice's own work there, newest of all.
        Seed {
            exe: 12,
            org: 2,
            project: 9,
            status: running,
            steps: vec![(1, Work, vec![user(1)], vec![], 700)],
        },
        Seed {
            exe: 13,
            org: 2,
            project: 9,
            status: ExecutionStatus::Waiting,
            steps: vec![(1, Approval, vec![group(9), group(1)], vec![], 700)],
        },
    ];
    for s in seeds {
        seed(f, s).await;
    }
}

type Row = (
    ProjectId,
    String,
    String,
    String,
    String,
    String,
    AssignmentKind,
);
fn rows(page: &MeTasksPage) -> Vec<Row> {
    page.rows
        .iter()
        .map(
            |MeTaskRow {
                 assignment: a,
                 project_id,
                 execution_title,
             }| {
                (
                    project_id.clone(),
                    a.execution_id.to_string(),
                    a.step_run_id.to_string(),
                    a.path.clone(),
                    a.title.clone(),
                    execution_title.clone(),
                    a.kind,
                )
            },
        )
        .collect()
}

/// The pre-R27 `/v1/me/tasks` path, verbatim in effect: every assignment
/// of every readable project through `inbox`, run titles one by one, then
/// `merge_pages`.
async fn reference_page(
    authoring: &PostgresAuthoring,
    snapshot: &DirectorySnapshot,
    user: &UserId,
    readable: &[ProjectId],
    filters: &InboxFilters,
    cursor: Option<String>,
    limit: usize,
) -> Result<MeTasksPage, runbook_core::StoreError> {
    let mut owner = BTreeMap::new();
    let mut lists = vec![];
    for project in readable {
        let scope = TenantScope {
            org_id: snapshot.org_id.clone(),
            project_id: project.clone(),
        };
        let page = inbox(
            authoring.scoped(scope.clone()).catalog(),
            &scope,
            snapshot,
            user,
            filters.clone(),
            None,
            usize::MAX / 2,
        )
        .await?;
        for a in &page.assignments {
            owner.insert(a.step_run_id.clone(), project.clone());
        }
        lists.push(page.assignments);
    }
    let page = merge_pages(&snapshot.org_id, user, filters, lists, cursor, limit)?;
    let mut out = vec![];
    for a in page.assignments {
        let project_id = owner[&a.step_run_id].clone();
        let execution_title = authoring
            .scoped(TenantScope {
                org_id: snapshot.org_id.clone(),
                project_id: project_id.clone(),
            })
            .catalog()
            .get_run(&a.execution_id)
            .await?
            .map(|s| s.title)
            .unwrap_or_default();
        out.push(MeTaskRow {
            assignment: a,
            project_id,
            execution_title,
        });
    }
    Ok(MeTasksPage {
        rows: out,
        next_cursor: page.next_cursor,
    })
}

async fn walk(
    authoring: &PostgresAuthoring,
    snapshot: &DirectorySnapshot,
    user: &UserId,
    readable: &[ProjectId],
    filters: &InboxFilters,
    limit: usize,
) -> Vec<Row> {
    let scope = me_tasks_scope(snapshot, user, readable);
    let mut cursor: Option<String> = None;
    let mut all = vec![];
    for _ in 0..64 {
        let old = reference_page(
            authoring,
            snapshot,
            user,
            readable,
            filters,
            cursor.clone(),
            limit,
        )
        .await
        .unwrap();
        let new = authoring
            .me_tasks(&scope, filters, cursor.as_deref(), limit)
            .await
            .unwrap();
        let context = format!(
            "user {user} projects {readable:?} filters {filters:?} limit {limit} cursor {cursor:?}"
        );
        assert_eq!(rows(&new), rows(&old), "{context}");
        assert_eq!(new.rows, old.rows, "{context}");
        assert_eq!(new.next_cursor, old.next_cursor, "{context}");
        all.extend(rows(&new));
        match new.next_cursor {
            Some(next) => cursor = Some(next),
            None => return all,
        }
    }
    panic!("pagination did not terminate");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn single_query_matches_in_memory_merge_page_by_page() {
    with_factory(async |f| {
        scenario(f).await;
        let authoring = PostgresAuthoring::new(f.config.clone());
        let snapshot = PostgresDirectory::new(f.config.clone())
            .snapshot(&org(1))
            .await
            .unwrap();
        let every: Vec<ProjectId> = snapshot.projects.keys().cloned().collect();
        let filters = [
            InboxFilters::default(),
            InboxFilters {
                kinds: vec![AssignmentKind::Approval],
            },
            InboxFilters {
                kinds: vec![AssignmentKind::Handoff, AssignmentKind::Work],
            },
        ];
        // The headline case is not vacuous: alice's full inbox, newest first.
        let alice = walk(&authoring, &snapshot, &usr(1), &every, &filters[0], 100).await;
        let order: Vec<String> = alice.iter().map(|r| r.2.clone()).collect();
        let expected: Vec<String> = [
            (10, 1),
            (6, 1),
            (8, 1),
            (2, 1),
            (5, 1),
            (7, 2),
            (7, 1),
            (1, 1),
            (9, 1),
            (14, 1),
        ]
        .into_iter()
        .map(|(e, k)| sr(e, k).to_string())
        .collect();
        assert_eq!(order, expected);
        assert_eq!(alice[0].0, prj(2));
        assert_eq!(alice[0].5, "Run 10");
        for u in 1..=5 {
            for readable in [every.clone(), vec![prj(2)], vec![prj(1), prj(3)], vec![]] {
                for filter in &filters {
                    let full = walk(&authoring, &snapshot, &usr(u), &readable, filter, 100).await;
                    for limit in [1, 2, 3] {
                        assert_eq!(
                            walk(&authoring, &snapshot, &usr(u), &readable, filter, limit).await,
                            full
                        );
                    }
                }
            }
        }
        // Erin is deactivated: nothing, even where she is named directly.
        assert!(
            walk(&authoring, &snapshot, &usr(5), &every, &filters[0], 10)
                .await
                .is_empty()
        );
        // Bad and foreign cursors are rejected the same way.
        let scope = me_tasks_scope(&snapshot, &usr(1), &every);
        let first = authoring
            .me_tasks(&scope, &filters[0], None, 1)
            .await
            .unwrap();
        let cursor = first.next_cursor.unwrap();
        for (filter, cursor) in [(&filters[1], cursor.as_str()), (&filters[0], "v1.garbage")] {
            assert!(matches!(
                authoring.me_tasks(&scope, filter, Some(cursor), 1).await,
                Err(runbook_core::StoreError::InvalidBatch(
                    "request.invalid_cursor"
                ))
            ));
        }
        let bob = me_tasks_scope(&snapshot, &usr(2), &every);
        assert!(
            authoring
                .me_tasks(&bob, &filters[0], Some(&cursor), 1)
                .await
                .is_err()
        );
        Ok(())
    })
    .await;
}

/// Cross-tenant negative: org 2's rows never leak into org 1, even when the
/// caller's scope names org 2's project and team ids, and vice versa.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn single_query_never_crosses_tenants() {
    with_factory(async |f| {
        scenario(f).await;
        let authoring = PostgresAuthoring::new(f.config.clone());
        let directory = PostgresDirectory::new(f.config.clone());
        let one = directory.snapshot(&org(1)).await.unwrap();
        let two = directory.snapshot(&org(2)).await.unwrap();
        // A forged org-1 scope that names org 2's project and team.
        let mut forged = me_tasks_scope(&one, &usr(1), &[prj(1), prj(2)]);
        forged.projects.push(prj(9));
        forged.teams.push(team(9));
        let page = authoring
            .me_tasks(&forged, &InboxFilters::default(), None, 100)
            .await
            .unwrap();
        assert!(!page.rows.is_empty());
        let foreign = [catalog::id(12), catalog::id(13)];
        assert!(
            page.rows
                .iter()
                .all(|r| r.project_id != prj(9) && !foreign.contains(&r.assignment.execution_id)),
            "{page:?}"
        );
        // Org 2 sees only its own two rows, never org 1's team-1 work.
        let theirs = authoring
            .me_tasks(
                &me_tasks_scope(&two, &usr(1), &[prj(9), prj(1)]),
                &InboxFilters::default(),
                None,
                100,
            )
            .await
            .unwrap();
        let ids: Vec<_> = theirs
            .rows
            .iter()
            .map(|r| r.assignment.execution_id.clone())
            .collect();
        assert_eq!(ids, vec![catalog::id(13), catalog::id(12)]);
        assert!(theirs.rows.iter().all(|r| r.project_id == prj(9)));
        // An org-1 cursor is not valid for org 2.
        let mine = authoring
            .me_tasks(
                &me_tasks_scope(&one, &usr(1), &[prj(1)]),
                &InboxFilters::default(),
                None,
                1,
            )
            .await
            .unwrap();
        assert!(
            authoring
                .me_tasks(
                    &me_tasks_scope(&two, &usr(1), &[prj(9)]),
                    &InboxFilters::default(),
                    mine.next_cursor.as_deref(),
                    1
                )
                .await
                .is_err()
        );
        Ok(())
    })
    .await;
}

/// Migration 0025 backfills the inbox columns of rows written before it
/// exactly as the write path now fills them.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn migration_backfill_matches_write_path() {
    with_factory(async |f| {
        scenario(f).await;
        let columns = "SELECT execution_id, step_run_id, kind, updated_at_ms, \
                       recipients, voted_by FROM runbook.assignment ORDER BY 1, 2";
        type Columns = (String, String, String, i64, Vec<String>, Vec<String>);
        let read = async || -> Vec<Columns> {
            f.admin
                .client()
                .query(columns, &[])
                .await
                .unwrap()
                .iter()
                .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
                .collect()
        };
        let written = read().await;
        assert!(
            written
                .iter()
                .any(|r| r.2 == "handoff" && r.4.contains(&format!("team:{}", team(1))))
        );
        assert!(written.iter().any(|r| r.5 == vec![usr(1).to_string()]));
        f.admin
            .client()
            .batch_execute(
                "UPDATE runbook.assignment SET kind = 'work', updated_at_ms = 0, \
                 recipients = '{}', voted_by = '{}'",
            )
            .await
            .unwrap();
        let migration = include_str!("../migrations/0025_inbox_index.sql");
        let start = migration.find("UPDATE runbook.assignment").unwrap();
        let end = start + migration[start..].find(";\n").unwrap();
        f.admin
            .client()
            .batch_execute(&migration[start..=end])
            .await
            .unwrap();
        assert_eq!(read().await, written);
        Ok(())
    })
    .await;
}
