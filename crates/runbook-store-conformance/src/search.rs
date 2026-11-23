//! D22 catalog search cases: same filters, order and cursor rules on every
//! adapter. Summaries arrive only through execution commits.
use crate::*;
use runbook_core::catalog::CatalogStore;
use runbook_core::search::{FieldFilter, RunFilters};
use runbook_model::{Data, ExecutionStatus};

fn seeded_batch(
    n: u8,
    from: u64,
    to: u64,
    severity: &str,
    created_at_ms: i64,
    title: &str,
) -> ExecutionCommit {
    let mut batch = catalog::batch(n, from, to, ExecutionStatus::Running);
    let summary = &mut batch.derived_outbox[0].summary;
    summary.title = title.into();
    summary.tags = vec!["deploy".into()];
    summary.fields = Data::from([(
        "severity".to_string(),
        serde_json::Value::String(severity.into()),
    )]);
    summary.created_by = "operator".into();
    summary.created_at_ms = created_at_ms;
    batch
}
fn severity_filter(value: &str) -> RunFilters {
    RunFilters {
        fields: vec![FieldFilter::Equals {
            field: "severity".into(),
            value: serde_json::Value::String(value.into()),
        }],
        ..Default::default()
    }
}
fn ids(page: &runbook_core::search::RunPage) -> Vec<runbook_model::ExecutionId> {
    page.runs
        .iter()
        .map(|run| run.execution_id.clone())
        .collect()
}

pub async fn indexed_severity_tracks_commits<F: CatalogFactory>(f: &F) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    s.commit(seeded_batch(1, 0, 1, "major", 100, "Verify api"))
        .await?;
    s.commit(seeded_batch(2, 0, 1, "minor", 200, "Verify web"))
        .await?;
    f.await_derived(&s, &catalog::id(1), 1, f.derived_row_delay())
        .await?;
    f.await_derived(&s, &catalog::id(2), 1, f.derived_row_delay())
        .await?;
    assert_eq!(
        ids(&s.search_runs(severity_filter("major"), None, 10).await?),
        vec![catalog::id(1)]
    );
    // A committed update is reflected after reconnect; an older derived
    // sequence redelivered afterwards does not move it back.
    let reconnected = f.scoped_client(&s, scope(1)).await?;
    reconnected
        .commit(seeded_batch(1, 1, 2, "minor", 100, "Verify api"))
        .await?;
    f.await_derived(&s, &catalog::id(1), 2, f.derived_row_delay())
        .await?;
    f.redeliver_derived(&s, &catalog::id(1), 1).await?;
    assert!(
        reconnected
            .search_runs(severity_filter("major"), None, 10)
            .await?
            .runs
            .is_empty()
    );
    assert_eq!(
        ids(&reconnected
            .search_runs(severity_filter("minor"), None, 10)
            .await?),
        vec![catalog::id(2), catalog::id(1)]
    );
    // An injected commit failure leaves search unchanged.
    f.fail_after(&s, WriteStage::DerivedRows).await?;
    assert!(
        s.commit(seeded_batch(1, 2, 3, "major", 100, "Verify api"))
            .await
            .is_err()
    );
    assert!(
        s.search_runs(severity_filter("major"), None, 10)
            .await?
            .runs
            .is_empty()
    );
    Ok(())
}

pub async fn search_is_tenant_scoped_and_paginated<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    let q = f.scoped_client(&s, scope(2)).await?;
    for (n, created) in [(1u8, 100i64), (2, 200), (3, 300), (4, 300)] {
        s.commit(seeded_batch(n, 0, 1, "minor", created, "Verify deploy"))
            .await?;
        f.await_derived(&s, &catalog::id(n), 1, f.derived_row_delay())
            .await?;
    }
    q.commit(seeded_batch(5, 0, 1, "minor", 400, "Verify deploy"))
        .await?;
    let filters = RunFilters {
        text: Some("verify".into()),
        ..Default::default()
    };
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = s.search_runs(filters.clone(), cursor, 2).await?;
        seen.extend(ids(&page));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        seen,
        vec![
            catalog::id(4),
            catalog::id(3),
            catalog::id(2),
            catalog::id(1)
        ]
    );
    // A cursor minted for one scope or filter set never widens another.
    let first = s.search_runs(filters.clone(), None, 2).await?;
    let forged = first.next_cursor.unwrap();
    assert_eq!(
        q.search_runs(filters.clone(), Some(forged.clone()), 2)
            .await,
        Err(StoreError::InvalidBatch("request.invalid_cursor"))
    );
    let mut other_filters = filters.clone();
    other_filters.text = Some("other".into());
    assert_eq!(
        s.search_runs(other_filters, Some(forged), 2).await,
        Err(StoreError::InvalidBatch("request.invalid_cursor"))
    );
    Ok(())
}

/// R14: a numeric range never matches string or boolean field values,
/// on either adapter (JSONB type ordering must not widen the range).
pub async fn numeric_range_ignores_non_number_values<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create(execution::projection()).await?;
    for (n, value) in [
        (1u8, serde_json::json!(5)),
        (2, serde_json::json!("zzz")),
        (3, serde_json::json!(true)),
    ] {
        let mut batch = catalog::batch(n, 0, 1, ExecutionStatus::Succeeded);
        batch.derived_outbox[0].summary.fields = Data::from([("amount".to_string(), value)]);
        s.commit(batch).await?;
        f.await_derived(&s, &catalog::id(n), 1, f.derived_row_delay())
            .await?;
    }
    for (min, max) in [
        (Some(serde_json::json!(0)), Some(serde_json::json!(10))),
        (Some(serde_json::json!(0)), None),
        (None, Some(serde_json::json!(10))),
    ] {
        let page = s
            .search_runs(
                RunFilters {
                    fields: vec![FieldFilter::Range {
                        field: "amount".into(),
                        min,
                        max,
                    }],
                    ..Default::default()
                },
                None,
                10,
            )
            .await?;
        assert_eq!(ids(&page), vec![catalog::id(1)]);
    }
    Ok(())
}

/// B8f: the inbox tracks current team membership and completion, with
/// kind filters, deduped multi-team matches and bound cursors.
pub async fn inbox_tracks_current_access_and_completion<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    use runbook_core::{
        catalog::{Assignment, AssignmentKind},
        directory::{DirectorySnapshot, OrganizationRecord, ProjectRecord, TeamRecord, UserRecord},
        inbox::{InboxFilters, inbox},
    };
    use runbook_model::RecipientRef;
    let s = f.create(execution::projection()).await?;
    let user = |n: u8| -> runbook_model::UserId { format!("usr_{n:026}").parse().unwrap() };
    let team = |n: u8| -> runbook_model::TeamId { format!("team_{n:026}").parse().unwrap() };
    let seed = |n: u8, updated: i64, assignment: Assignment| {
        let mut batch = catalog::batch(n, 0, 1, ExecutionStatus::Running);
        batch.derived_outbox[0].assignments = vec![assignment];
        batch.derived_outbox[0].summary.created_at_ms = updated;
        batch
    };
    let assignment =
        |n: u8, kind: AssignmentKind, recipients: Vec<RecipientRef>, updated: i64| Assignment {
            execution_id: catalog::id(n),
            step_run_id: format!("sr_{n:026}").parse().unwrap(),
            recipients,
            kind,
            sequence: 1,
            updated_at_ms: updated,
            voted_by: vec![],
            path: String::new(),
            title: String::new(),
        };
    s.commit(seed(
        1,
        100,
        assignment(
            1,
            AssignmentKind::Work,
            vec![RecipientRef::User(user(1))],
            100,
        ),
    ))
    .await?;
    f.await_derived(&s, &catalog::id(1), 1, f.derived_row_delay())
        .await?;
    s.commit(seed(
        2,
        300,
        assignment(
            2,
            AssignmentKind::Approval,
            vec![
                RecipientRef::Team(team(1).to_string()),
                RecipientRef::Team(team(2).to_string()),
            ],
            300,
        ),
    ))
    .await?;
    f.await_derived(&s, &catalog::id(2), 1, f.derived_row_delay())
        .await?;
    s.commit(seed(
        3,
        200,
        assignment(
            3,
            AssignmentKind::Work,
            vec![RecipientRef::User(user(3))],
            200,
        ),
    ))
    .await?;
    f.await_derived(&s, &catalog::id(3), 1, f.derived_row_delay())
        .await?;

    let snapshot = |teams_of_alice: &[u8]| {
        let users = [(1u8, "alice"), (2, "bob"), (3, "carol")]
            .into_iter()
            .map(|(n, name)| {
                (
                    user(n),
                    UserRecord {
                        id: user(n),
                        name: name.into(),
                        active: true,
                        role: runbook_core::directory::OrgRole::Member,
                    },
                )
            })
            .collect();
        DirectorySnapshot {
            org_id: scope(1).org_id,
            revision: 1,
            security: Default::default(),
            organization: OrganizationRecord {
                id: scope(1).org_id.clone(),
                title: "Org".into(),
            },
            users,
            projects: [(
                scope(1).project_id.clone(),
                ProjectRecord {
                    id: scope(1).project_id.clone(),
                    org_id: scope(1).org_id.clone(),
                    slug: "api".into(),
                    title: "API".into(),
                },
            )]
            .into(),
            teams: [1u8, 2]
                .into_iter()
                .map(|n| {
                    (
                        team(n),
                        TeamRecord {
                            id: team(n),
                            org_id: scope(1).org_id.clone(),
                            slug: format!("t{n}"),
                            title: "Team".into(),
                        },
                    )
                })
                .collect(),
            project_members: [1u8, 2, 3]
                .into_iter()
                .map(|n| {
                    (
                        (scope(1).project_id.clone(), user(n)),
                        runbook_model::ProjectRole::Operator,
                    )
                })
                .collect(),
            team_members: teams_of_alice
                .iter()
                .map(|n| (team(*n), user(1)))
                .chain([(team(1), user(2))])
                .collect(),
            archived_projects: Default::default(),
        }
    };
    // Alice (direct + both teams) sees runs 2 then 1, deduped; carol only hers.
    let page = inbox(
        &s,
        &scope(1),
        &snapshot(&[1, 2]),
        &user(1),
        InboxFilters::default(),
        None,
        1,
    )
    .await?;
    assert_eq!(page.assignments.len(), 1);
    assert_eq!(page.assignments[0].execution_id, catalog::id(2));
    let foreign_cursor = page.next_cursor.clone().unwrap();
    let page = inbox(
        &s,
        &scope(1),
        &snapshot(&[1, 2]),
        &user(1),
        InboxFilters::default(),
        page.next_cursor,
        10,
    )
    .await?;
    assert_eq!(page.assignments.len(), 1);
    assert_eq!(page.assignments[0].execution_id, catalog::id(1));
    assert_eq!(page.next_cursor, None);
    // Losing team 2 keeps team 1 (still a match); losing both drops run 2.
    let page = inbox(
        &s,
        &scope(1),
        &snapshot(&[1]),
        &user(1),
        InboxFilters::default(),
        None,
        10,
    )
    .await?;
    assert_eq!(page.assignments.len(), 2);
    let page = inbox(
        &s,
        &scope(1),
        &snapshot(&[]),
        &user(1),
        InboxFilters::default(),
        None,
        10,
    )
    .await?;
    assert_eq!(page.assignments.len(), 1);
    // Alice's cursor is invalid for carol and for other filters.
    assert_eq!(
        inbox(
            &s,
            &scope(1),
            &snapshot(&[1, 2]),
            &user(3),
            InboxFilters::default(),
            Some(foreign_cursor.clone()),
            10
        )
        .await,
        Err(StoreError::InvalidBatch("request.invalid_cursor"))
    );
    assert_eq!(
        inbox(
            &s,
            &scope(1),
            &snapshot(&[1, 2]),
            &user(1),
            InboxFilters {
                kinds: vec![AssignmentKind::Approval]
            },
            Some(foreign_cursor),
            10,
        )
        .await,
        Err(StoreError::InvalidBatch("request.invalid_cursor"))
    );
    // Completion replaces assignments wholesale: run 2 leaves the inbox.
    let mut done = catalog::batch(2, 1, 2, ExecutionStatus::Succeeded);
    done.derived_outbox[0].assignments = vec![];
    s.commit(done).await?;
    f.await_derived(&s, &catalog::id(2), 2, f.derived_row_delay())
        .await?;
    let page = inbox(
        &s,
        &scope(1),
        &snapshot(&[1, 2]),
        &user(1),
        InboxFilters::default(),
        None,
        10,
    )
    .await?;
    assert_eq!(page.assignments.len(), 1);
    assert_eq!(page.assignments[0].execution_id, catalog::id(1));
    Ok(())
}

/// UI-1 review: the runbook list's run facts for a page of runbooks in one
/// read: the newest run in D22 order (created time, then execution ID,
/// both descending), the active runs counted up to a cap, only this
/// tenant's runs, and every requested ID present.
pub async fn runbook_run_stats_group_by_runbook<F: CatalogFactory>(
    f: &F,
) -> Result<(), StoreError> {
    use runbook_core::search::RunbookRunStats;
    use runbook_model::RunbookId;
    let a = execution::projection().runbook_id;
    let b: RunbookId = "rb_00000000000000000000000002".parse().unwrap();
    let none: RunbookId = "rb_00000000000000000000000003".parse().unwrap();
    let s = f.create(execution::projection()).await?;
    let q = f.scoped_client(&s, scope(2)).await?;
    let run = |n: u8, runbook: &RunbookId, status: ExecutionStatus, created: i64| {
        let mut batch = catalog::batch(n, 0, 1, status);
        batch.projection.runbook_id = runbook.clone();
        let summary = &mut batch.derived_outbox[0].summary;
        summary.runbook_id = runbook.clone();
        summary.created_at_ms = created;
        batch
    };
    for (n, runbook, status, created) in [
        (1u8, &a, ExecutionStatus::Succeeded, 100i64),
        (2, &a, ExecutionStatus::Running, 300),
        (3, &a, ExecutionStatus::Queued, 300),
        (4, &a, ExecutionStatus::Waiting, 200),
        (5, &a, ExecutionStatus::Paused, 50),
        (6, &b, ExecutionStatus::Failed, 500),
    ] {
        s.commit(run(n, runbook, status, created)).await?;
        f.await_derived(&s, &catalog::id(n), 1, f.derived_row_delay())
            .await?;
    }
    // Another tenant's newer, active run of the same runbook ID.
    q.commit(run(7, &a, ExecutionStatus::Running, 900)).await?;
    f.await_derived(&q, &catalog::id(7), 1, f.derived_row_delay())
        .await?;
    let ids = [a.clone(), b.clone(), none.clone()];
    let stats = s.runbook_run_stats(&ids, 100).await?;
    assert_eq!(stats.len(), 3, "{stats:?}");
    let last = |id: &RunbookId| {
        stats[id]
            .last_run
            .as_ref()
            .map(|run| (run.execution_id.clone(), run.status, run.created_at_ms))
    };
    // Execution 3 ties execution 2 on created time and has the higher ID.
    assert_eq!(
        last(&a),
        Some((catalog::id(3), ExecutionStatus::Queued, 300))
    );
    assert_eq!(stats[&a].active_runs, 4);
    assert_eq!(
        last(&b),
        Some((catalog::id(6), ExecutionStatus::Failed, 500))
    );
    assert_eq!(stats[&b].active_runs, 0);
    assert_eq!(stats[&none], RunbookRunStats::default());
    // The full summary is the one search returns.
    let searched = s
        .search_runs(
            RunFilters {
                runbook_id: Some(a.clone()),
                ..Default::default()
            },
            None,
            1,
        )
        .await?;
    assert_eq!(stats[&a].last_run.as_ref(), searched.runs.first());
    // The cap bounds the count; an empty request reads nothing.
    assert_eq!(s.runbook_run_stats(&ids, 2).await?[&a].active_runs, 2);
    assert!(s.runbook_run_stats(&[], 100).await?.is_empty());
    // The other tenant sees only its own run.
    let theirs = q.runbook_run_stats(&ids, 100).await?;
    assert_eq!(
        theirs[&a]
            .last_run
            .as_ref()
            .map(|run| run.execution_id.clone()),
        Some(catalog::id(7))
    );
    assert_eq!(theirs[&a].active_runs, 1);
    assert_eq!(theirs[&b], RunbookRunStats::default());
    Ok(())
}
