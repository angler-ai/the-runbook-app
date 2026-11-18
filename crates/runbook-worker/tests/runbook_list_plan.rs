//! UI-1 review: the runbook list's per-page run read on D1 probes the
//! runbook indexes and never scans `run_summary` (SQLite's plan, natively).
use runbook_store_d1::{D1, native::NativeD1, native::run, prepare, sql::Columns};

#[test]
fn runbook_run_stats_uses_the_runbook_indexes() {
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    db.execute_script(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000) \
         INSERT INTO execution_owner (execution_id, org_id, project_id) \
         SELECT printf('exe_%026d', i), 'org_00000000000000000000000001', \
                'prj_00000000000000000000000001' FROM n; \
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000) \
         INSERT INTO run_summary (execution_id, runbook_id, sequence, status, body, created_at_ms) \
         SELECT printf('exe_%026d', i), printf('rb_%026d', i % 100), 1, \
                CASE WHEN i % 997 = 0 THEN 'running' ELSE 'succeeded' END, '{}', i FROM n; \
         ANALYZE;",
    )
    .unwrap();
    let plan: Vec<String> = run(db.all(
        prepare(format!(
            "EXPLAIN QUERY PLAN {}",
            runbook_store_d1::search::RUNBOOK_RUN_STATS_SQL
        ))
        .bind("org_00000000000000000000000001")
        .bind("prj_00000000000000000000000001")
        .bind("org_00000000000000000000000001")
        .bind("prj_00000000000000000000000001")
        .bind(100)
        .bind(r#"["rb_00000000000000000000000001","rb_00000000000000000000000002"]"#),
    ))
    .unwrap()
    .iter()
    .map(|row| row.string(3).unwrap())
    .collect();
    let plan = plan.join("\n");
    println!("{plan}");
    assert!(plan.contains("run_summary_runbook_created"), "{plan}");
    assert!(plan.contains("run_summary_runbook_active"), "{plan}");
    // No full pass over a run_summary alias (`r`, `a`) or the owners.
    for line in plan.lines().map(str::trim) {
        for alias in ["r", "a", "o", "ao"] {
            let scan = format!("SCAN {alias}");
            assert!(
                line != scan && !line.starts_with(&format!("{scan} ")),
                "{plan}"
            );
        }
    }
}

/// UI-1b review finding 3: the open-run count stops at the cap in SQL (as
/// Postgres does), so a runbook with thousands of open runs reads at most
/// the cap's index rows and owner probes per list page.
#[test]
fn the_active_run_count_stops_at_the_cap_in_sql() {
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    db.execute_script(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
         INSERT INTO execution_owner (execution_id, org_id, project_id) \
         SELECT printf('exe_%026d', i), 'org_00000000000000000000000001', \
                'prj_00000000000000000000000001' FROM n; \
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
         INSERT INTO run_summary (execution_id, runbook_id, sequence, status, body, created_at_ms) \
         SELECT printf('exe_%026d', i), printf('rb_%026d', i % 2), 1, 'running', '{}', i FROM n; \
         ANALYZE;",
    )
    .unwrap();
    let rows = run(db.all(
        prepare(runbook_store_d1::search::RUNBOOK_RUN_STATS_SQL)
            .bind("org_00000000000000000000000001")
            .bind("prj_00000000000000000000000001")
            .bind("org_00000000000000000000000001")
            .bind("prj_00000000000000000000000001")
            .bind(100)
            .bind(r#"["rb_00000000000000000000000001","rb_00000000000000000000000009"]"#),
    ))
    .unwrap();
    let counts: Vec<i64> = rows.iter().map(|row| row.integer(2).unwrap()).collect();
    assert_eq!(counts, [100, 0], "1,500 open runs count as the cap");
}

/// UI-1c: `GET /v1/projects/{p}/runbooks/{slug}` reads one row through the
/// unique slug index, never a pass over the project's runbooks.
#[test]
fn a_runbook_by_slug_is_a_point_read_of_the_slug_index() {
    let db = NativeD1::new(1_000);
    run(runbook_store_d1::migrate::migrate(&db)).unwrap();
    db.execute_script(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) \
         INSERT INTO runbook (org_id, project_id, runbook_id, slug, origin, archived, \
           draft_source, draft_revision) \
         SELECT 'org_00000000000000000000000001', 'prj_00000000000000000000000001', \
                printf('rb_%026d', i), printf('slug-%d', i), 'ui', 0, CAST('x' AS BLOB), 1 \
         FROM n; ANALYZE;",
    )
    .unwrap();
    let plan: Vec<String> = run(db.all(
        prepare(format!(
            "EXPLAIN QUERY PLAN {}",
            runbook_store_d1::listing::RUNBOOK_BY_SLUG_SQL
        ))
        .bind("org_00000000000000000000000001")
        .bind("prj_00000000000000000000000001")
        .bind("slug-42"),
    ))
    .unwrap()
    .iter()
    .map(|row| row.string(3).unwrap())
    .collect();
    let plan = plan.join("\n");
    println!("{plan}");
    assert!(
        plan.starts_with("SEARCH runbook USING INDEX ")
            && plan.contains("(org_id=? AND project_id=? AND slug=?)"),
        "{plan}"
    );
    let rows = run(db.all(
        prepare(runbook_store_d1::listing::RUNBOOK_BY_SLUG_SQL)
            .bind("org_00000000000000000000000001")
            .bind("prj_00000000000000000000000001")
            .bind("slug-42"),
    ))
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].string(0).unwrap(), "rb_00000000000000000000000042");
}
