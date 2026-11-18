//! B3.3: shared D22 search cases on Postgres, the typed field contract,
//! and the unchanged flagship's indexed severity end to end.
mod support;
use runbook_core::{
    StoreError,
    catalog::CatalogStore,
    search::{
        DEFAULT_MAX_INDEXED_FIELDS, FieldFilter, RunFilters, project_search_fields,
        validate_field_update,
    },
};
use runbook_model::Data;
use runbook_store_conformance::scope;
use support::factory::with_factory;

macro_rules! shared {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| runbook_store_conformance::search::$name(f).await).await;
        }
    )*};
}
shared!(
    indexed_severity_tracks_commits,
    search_is_tenant_scoped_and_paginated,
    numeric_range_ignores_non_number_values,
    inbox_tracks_current_access_and_completion,
    runbook_run_stats_group_by_runbook,
);

fn fields_schema(source: &str) -> runbook_spec::DataSchema {
    let spec = runbook_spec::parse(source).unwrap();
    let context = runbook_spec::CompileContext {
        skills: [
            ("./skills/check-error-rates".into(), [1; 32]),
            ("./skills/rollback-service".into(), [2; 32]),
        ]
        .into(),
        teams: [
            (
                "oncall".into(),
                "team_00000000000000000000000001".parse().unwrap(),
            ),
            (
                "eng-leads".into(),
                "team_00000000000000000000000002".parse().unwrap(),
            ),
        ]
        .into(),
        ..Default::default()
    };
    runbook_spec::compile(&spec, &context)
        .unwrap()
        .ir()
        .fields
        .clone()
}
fn data(pairs: &[(&str, serde_json::Value)]) -> Data {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect()
}

/// The typed contract (core rules both adapters rely on): indexed-count
/// policy, explicit unindexed-filter failure, locked and mistyped edits.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn field_index_contract_is_typed() {
    with_factory(async |f| {
        // Tenth indexed field succeeds; the eleventh fails policy.
        let make = |count: usize| {
            let fields: Vec<String> = (0..count)
                .map(|i| format!("f{i}: {{type: string, indexed: true}}"))
                .collect();
            format!(
                "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: x, title: X}}\n\
                 spec:\n  fields: {{{}}}\n  steps: [{{id: only, title: Only}}]\n",
                fields.join(", ")
            )
        };
        let ten = fields_schema(&make(10));
        let values: Vec<(&str, serde_json::Value)> = (0..10)
            .map(|i| {
                (
                    ["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9"][i],
                    serde_json::json!("v"),
                )
            })
            .collect();
        let projected =
            project_search_fields(&ten, &data(&values), DEFAULT_MAX_INDEXED_FIELDS).unwrap();
        assert_eq!(projected.len(), 10);
        // The compiler itself refuses an eleventh indexed field; the
        // projection policy independently refuses above its configured max.
        let eleven_spec = runbook_spec::parse(&make(11)).unwrap();
        let compile_errors =
            runbook_spec::compile(&eleven_spec, &runbook_spec::CompileContext::default())
                .unwrap_err();
        assert!(
            compile_errors
                .iter()
                .any(|d| d.code == "spec.indexed_field_limit"),
            "{compile_errors:?}"
        );
        let two = fields_schema(&make(2));
        let errors = project_search_fields(&two, &Data::new(), 1).unwrap_err();
        assert_eq!(errors[0].code, "query.too_many_indexed_fields");

        // Locked edits and wrong types fail without mutation; the flagship
        // fields schema drives the typed checks.
        let flagship = include_str!("../../../conformance/spec/post-deploy.yaml");
        let schema = fields_schema(flagship);
        let current = data(&[("severity", serde_json::json!("none"))]);
        let wrong = validate_field_update(
            &schema,
            &current,
            &data(&[("severity", serde_json::json!(42))]),
        )
        .unwrap_err();
        assert!(
            wrong.iter().any(|d| d.code.starts_with("data.")),
            "{wrong:?}"
        );
        let locked_schema = fields_schema(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: x, title: X}\n\
             spec:\n  fields: {ticket: {type: string, locked: true}}\n  \
             steps: [{id: only, title: Only}]\n",
        );
        let errors = validate_field_update(
            &locked_schema,
            &data(&[("ticket", serde_json::json!("a"))]),
            &data(&[("ticket", serde_json::json!("b"))]),
        )
        .unwrap_err();
        assert_eq!(errors[0].code, "data.locked_field");
        let merged = validate_field_update(
            &schema,
            &current,
            &data(&[("severity", serde_json::json!("major"))]),
        )
        .unwrap();
        assert_eq!(merged["severity"], serde_json::json!("major"));

        // Filtering a field no run projects fails explicitly.
        let store = f.fresh_store(scope(1)).await;
        assert_eq!(
            store
                .search_runs(
                    RunFilters {
                        fields: vec![FieldFilter::Equals {
                            field: "ticket".into(),
                            value: serde_json::json!("x"),
                        }],
                        ..Default::default()
                    },
                    None,
                    10,
                )
                .await,
            Err(StoreError::InvalidBatch("query.field_not_indexed"))
        );
        Ok(())
    })
    .await;
}

/// The flagship's own indexed severity field, projected by the typed
/// contract and searched through real commits.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn flagship_indexed_severity_end_to_end() {
    use runbook_core::{ExecutionCommit, ExecutionStore};
    use runbook_model::ExecutionStatus;
    use runbook_store_conformance::{StoreFactory, catalog, execution};
    with_factory(async |f| {
        let flagship = include_str!("../../../conformance/spec/post-deploy.yaml");
        let spec = runbook_spec::parse(flagship).unwrap();
        let context = runbook_spec::CompileContext {
            skills: [
                ("./skills/check-error-rates".into(), [1; 32]),
                ("./skills/rollback-service".into(), [2; 32]),
            ]
            .into(),
            teams: [
                (
                    "oncall".into(),
                    "team_00000000000000000000000001".parse().unwrap(),
                ),
                (
                    "eng-leads".into(),
                    "team_00000000000000000000000002".parse().unwrap(),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let compiled = runbook_spec::compile(&spec, &context).unwrap();
        let store = f.create(execution::projection()).await?;
        for (n, severity) in [(1u8, "major"), (2, "minor")] {
            let fields = validate_field_update(
                &compiled.ir().fields,
                &Data::new(),
                &data(&[("severity", serde_json::json!(severity))]),
            )
            .unwrap();
            let projected =
                project_search_fields(&compiled.ir().fields, &fields, DEFAULT_MAX_INDEXED_FIELDS)
                    .unwrap();
            assert_eq!(projected.len(), 1);
            let mut batch: ExecutionCommit = catalog::batch(n, 0, 1, ExecutionStatus::Succeeded);
            let summary = &mut batch.derived_outbox[0].summary;
            summary.title = "Verify api v2".into();
            summary.fields = projected;
            summary.created_at_ms = 5_000 + i64::from(n);
            store.commit(batch).await?;
        }
        let major = store
            .search_runs(
                RunFilters {
                    fields: vec![FieldFilter::Equals {
                        field: "severity".into(),
                        value: serde_json::json!("major"),
                    }],
                    ..Default::default()
                },
                None,
                10,
            )
            .await?;
        assert_eq!(major.runs.len(), 1);
        assert_eq!(major.runs[0].execution_id, catalog::id(1));
        assert_eq!(major.runs[0].fields["severity"], serde_json::json!("major"));
        Ok(())
    })
    .await;
}

/// UI-1 review: the runbook list's per-page run read probes indexes, never
/// scans `run_summary`. 20,000 runs over 200 runbooks (a few open), one
/// tenant, analyzed; the page asks for 25 runbooks. The newest run comes
/// from `run_summary_runbook_created`, the active count from the partial
/// `run_summary_runbook_active`. Prints the plan (the decisions quote it).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn runbook_run_stats_uses_the_runbook_indexes() {
    with_factory(async |f| {
        let c = f.admin.client();
        c.batch_execute(
            "INSERT INTO runbook.execution_owner (execution_id, org_id, project_id) \
             SELECT 'exe_' || lpad(i::text, 26, '0'), 'org_00000000000000000000000001', \
                    'prj_00000000000000000000000001' \
             FROM generate_series(1, 20000) i; \
             INSERT INTO runbook.run_summary \
               (execution_id, runbook_id, sequence, status, completed_at_ms, body, created_at_ms) \
             SELECT 'exe_' || lpad(i::text, 26, '0'), 'rb_' || lpad((i % 200)::text, 26, '0'), 1, \
                    CASE WHEN i % 997 = 0 THEN 'running' WHEN i % 3 = 0 THEN 'failed' \
                         ELSE 'succeeded' END, \
                    NULL, '{}', i \
             FROM generate_series(1, 20000) i; \
             ANALYZE runbook.execution_owner; ANALYZE runbook.run_summary;",
        )
        .await
        .unwrap();
        let ids: Vec<String> = (0..25).map(|n| format!("rb_{n:026}")).collect();
        let (org, project, cap) = (
            "org_00000000000000000000000001",
            "prj_00000000000000000000000001",
            100i64,
        );
        let plan: Vec<String> = c
            .query(
                &format!(
                    "EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF, SUMMARY OFF) {}",
                    runbook_store_postgres::RUNBOOK_RUN_STATS_SQL
                ),
                &[&org, &project, &ids, &cap],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        let plan = plan.join("\n");
        println!("{plan}");
        assert!(!plan.contains("Seq Scan on run_summary"), "{plan}");
        assert!(plan.contains("run_summary_runbook_created"), "{plan}");
        assert!(plan.contains("run_summary_runbook_active"), "{plan}");
        Ok(())
    })
    .await;
}
