//! UI-1b: the runbook list's store reads on Postgres answer as the
//! reference and the Workers store do (the shared listing case).
mod support;
use runbook_core::authoring::AuthoringFactory;
use runbook_store_conformance::{listing, scope};
use runbook_store_postgres::PostgresAuthoring;
use support::factory::with_factory;

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn runbook_listing_pages() {
    with_factory(async |f| {
        let store = PostgresAuthoring::new(f.config.clone()).scoped(scope(1));
        let published = listing::seed(&store).await?;
        listing::runbook_listing_pages(&store, &published).await
    })
    .await;
}

/// UI-1c: `runbook_by_slug` on Postgres (a point read of the unique slug
/// index), scoped to the project.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn runbook_by_slug() {
    with_factory(async |f| {
        let factory = PostgresAuthoring::new(f.config.clone());
        let store = factory.scoped(scope(1));
        let other = factory.scoped(runbook_core::catalog::TenantScope {
            org_id: scope(1).org_id,
            project_id: scope(2).project_id,
        });
        listing::seed(&store).await?;
        listing::runbook_by_slug(&store, &other).await
    })
    .await;
}

/// UI-1c: a searched list reads keyset chunks of 32 in byte order
/// (`runbook_id COLLATE "C"`); migration 0033's `runbook_listing_order`
/// index has that order, so each chunk reads its rows from the index
/// instead of sorting the project's runbooks. 5,000 runbooks (4,500 in the
/// listed project), analyzed; the first chunk and one from the middle.
/// Prints both plans (the decisions quote them).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn a_searched_chunk_reads_the_runbook_order_index() {
    with_factory(async |f| {
        let c = f.admin.client();
        c.batch_execute(
            "INSERT INTO runbook.runbook (org_id, project_id, runbook_id, slug, origin, archived, \
               draft_source, draft_revision) \
             SELECT 'org_00000000000000000000000001', \
                    CASE WHEN i % 10 = 0 THEN 'prj_00000000000000000000000002' \
                         ELSE 'prj_00000000000000000000000001' END, \
                    'rb_' || lpad(i::text, 26, '0'), 'slug-' || i, 'ui', false, \
                    convert_to(repeat('x', 400), 'UTF8'), 1 \
             FROM generate_series(1, 5000) i; \
             ANALYZE runbook.runbook;",
        )
        .await
        .unwrap();
        let (org, project, chunk) = (
            "org_00000000000000000000000001",
            "prj_00000000000000000000000001",
            32i64,
        );
        let mut plans = vec![];
        for after in [None, Some("rb_00000000000000000000002501")] {
            let plan: Vec<String> = c
                .query(
                    &format!(
                        "EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF, SUMMARY OFF) {}",
                        runbook_store_postgres::RUNBOOK_LISTING_CHUNK_SQL
                    ),
                    &[&org, &project, &after, &chunk],
                )
                .await
                .unwrap()
                .iter()
                .map(|row| row.get(0))
                .collect();
            let plan = plan.join("\n");
            println!("after {after:?}:\n{plan}");
            plans.push(plan);
        }
        // The slug read: one probe of the unique slug index.
        let plan: Vec<String> = c
            .query(
                &format!(
                    "EXPLAIN (ANALYZE, COSTS OFF, TIMING OFF, SUMMARY OFF) {}",
                    runbook_store_postgres::RUNBOOK_BY_SLUG_SQL
                ),
                &[&org, &project, &"slug-42"],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get(0))
            .collect();
        let plan = plan.join("\n");
        println!("by slug:\n{plan}");
        assert!(
            plan.contains("Index Scan using runbook_org_id_project_id_slug_key"),
            "{plan}"
        );
        for plan in plans {
            assert!(plan.contains("runbook_listing_order"), "{plan}");
            assert!(!plan.contains("Sort"), "{plan}");
            assert!(!plan.contains("Seq Scan"), "{plan}");
        }
        Ok(())
    })
    .await;
}
