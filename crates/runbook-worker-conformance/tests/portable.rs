//! CF8: the D3 portable project archive through the composed Cloudflare
//! store, natively (D1 on rusqlite, one in-memory object per run behind the
//! object protocol, R2 in memory). The archive is the shared fixture
//! (`runbook_store_conformance::portable`) exported by the memory adapters:
//! importing it here and exporting again gives the same bytes, every refusal
//! leaves the target untouched, and a failure at the D1 commit undoes the
//! objects and blobs the import wrote. `cloudflare/tests/portable.test.mjs`
//! runs the same round trip in workerd through the operator route, and
//! `runbook-server/tests/cloudflare_archive.rs` from Postgres.
use runbook_core::{
    blob::{BlobKey, BlobPrefix, BlobStore},
    directory::memory::MemoryDirectory,
    memory::MemoryExecutionStore,
    portable::{self, Archive, ImportOptions, PortableError},
    webhooks::memory::MemoryWebhooks,
};
use runbook_store_conformance::portable::{self as fixture, p, q};
use runbook_worker::{compose::Objects, object::ObjectHandle, portable as cf};
use runbook_worker_conformance::{
    cross::{CrossFactory, NativeCross},
    native::run,
};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

const T0: i64 = 1_700_000_000_000;
const CREATED: i64 = 1_800_000_000_000;
const STACK: usize = 32 * 1024 * 1024;

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory adapters must be synchronous"),
    }
}

/// The shared fixture's project P, exported by the memory adapters.
fn source() -> Archive {
    source_at(T0)
}
/// The same fixture built with its clocks at `t0`.
fn source_at(t0: i64) -> Archive {
    let directory = MemoryDirectory::default();
    directory.set_time_ms(t0);
    let store = MemoryExecutionStore::empty(p(), t0);
    let webhooks = MemoryWebhooks::new(t0);
    ready(fixture::build(
        &store,
        &store.scoped(q()),
        &directory,
        &webhooks,
    ));
    let data = portable::memory::export(&store, &directory, &webhooks).unwrap();
    portable::encode(&data, CREATED).unwrap()
}

fn on_big_stack(body: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(body)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn the_fixture_round_trips_through_the_cloudflare_store_byte_identical() {
    on_big_stack(|| {
        let first = source();
        let (_, data) = portable::decode(&first).unwrap();
        assert!(
            data.rows.runs.len() >= 4,
            "the fixture has open, queued and closed runs"
        );
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            let again = portable::encode(&cf::export(&store).await.unwrap(), CREATED).unwrap();
            for (path, bytes) in &first.files {
                assert_eq!(
                    Some(bytes),
                    again.files.get(path),
                    "{path} differs:\n{}\n{}",
                    String::from_utf8_lossy(bytes),
                    String::from_utf8_lossy(again.files.get(path).map_or(&[][..], |b| b))
                );
            }
            assert_eq!(first, again);
            // The transport ZIP carries the same archive.
            let zip = cf::to_zip(&data, CREATED).unwrap();
            assert_eq!(cf::from_zip(&zip).unwrap(), data);
        });
    });
}

#[test]
fn refused_imports_leave_the_target_untouched() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            let before = portable::encode(&cf::export(&store).await.unwrap(), CREATED).unwrap();
            // The organization exists now.
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0).await,
                Err(PortableError::Conflict("import.org_exists"))
            );
            // Into the existing organization, the project is not empty.
            let into = ImportOptions {
                into_existing_org: true,
            };
            assert_eq!(
                cf::import(&store, &data, into, T0).await,
                Err(PortableError::TargetNotEmpty)
            );
            // Another project's store is refused before anything is read.
            assert_eq!(
                cf::import(&store.scoped(q()), &data, into, T0).await,
                Err(PortableError::Conflict("import.scope_mismatch"))
            );
            // A tampered archive never reaches the store.
            let mut tampered = data.clone();
            tampered.rows.runs[0].slice.scope = q();
            assert!(cf::import(&store, &tampered, into, T0).await.is_err());
            let after = portable::encode(&cf::export(&store).await.unwrap(), CREATED).unwrap();
            assert_eq!(before, after);
        });
    });
}

/// CF12b review round 2: a start record past one row is refused before any
/// write (`archive.value_too_large`); the same archive without it then
/// imports into the untouched target. CF12c: only this edition refuses it
/// (`portable::check_row_bounds`); native imports it.
#[test]
fn an_oversized_start_record_is_refused_before_any_write() {
    on_big_stack(|| {
        let (_, clean) = portable::decode(&source()).unwrap();
        let mut crafted = clean.clone();
        crafted
            .rows
            .runs
            .iter_mut()
            .find_map(|r| r.start.as_mut())
            .unwrap()
            .context
            .fields
            .insert(
                "big".into(),
                serde_json::Value::String("x".repeat(2_000_000)),
            );
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let refused = cf::import(&store, &crafted, ImportOptions::default(), T0).await;
            assert!(
                matches!(
                    refused,
                    Err(PortableError::Archive {
                        code: "archive.value_too_large",
                        ..
                    })
                ),
                "{refused:?}"
            );
            assert!(
                store
                    .list_prefix(&BlobPrefix::Shared)
                    .await
                    .unwrap()
                    .is_empty()
            );
            cf::import(&store, &clean, ImportOptions::default(), T0)
                .await
                .unwrap();
        });
    });
}

/// CF12c review round 1: each event is a row of the run's object too, and
/// a run's `ExecutionStarted` event carries its whole start context, so an
/// archive (an older native run, or one without its start record) can hold
/// an event past one row while its start record fits. It is refused before
/// any write, as the start record is. Before, `check_row_bounds` checked
/// the start record alone and the import failed at the object, mid-write.
#[test]
fn an_oversized_event_is_refused_before_any_write() {
    on_big_stack(|| {
        let (_, clean) = portable::decode(&source()).unwrap();
        let mut crafted = clean.clone();
        // The started event, edited through its JSON form.
        let started = crafted
            .rows
            .runs
            .iter_mut()
            .flat_map(|r| r.slice.run.events.iter_mut())
            .find(|e| serde_json::to_value(&**e).unwrap()["event"]["type"] == "ExecutionStarted")
            .expect("the fixture has a started run");
        let mut json = serde_json::to_value(&*started).unwrap();
        json["event"]["data"]["context"]["title"] =
            serde_json::Value::String("x".repeat(2_000_000));
        *started = serde_json::from_value(json).unwrap();
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let refused = cf::import(&store, &crafted, ImportOptions::default(), T0).await;
            assert!(
                matches!(
                    refused,
                    Err(PortableError::Archive {
                        code: "archive.value_too_large",
                        ..
                    })
                ),
                "{refused:?}"
            );
            assert!(
                store
                    .list_prefix(&BlobPrefix::Shared)
                    .await
                    .unwrap()
                    .is_empty()
            );
            cf::import(&store, &clean, ImportOptions::default(), T0)
                .await
                .unwrap();
        });
    });
}

#[test]
fn a_failed_commit_undoes_the_objects_and_blobs_it_wrote() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            // A webhook subscription ID already used elsewhere: the guards
            // pass, the objects and blobs are written, the D1 batch fails.
            let webhook = data
                .webhooks
                .first()
                .expect("the fixture has a webhook")
                .clone();
            let db = factory.db().unwrap();
            runbook_store_d1::D1::run(
                &db,
                runbook_store_d1::prepare(
                    "INSERT INTO webhook_subscription (subscription_id, org_id, project_id, \
                     created_at_ms, body) VALUES (?, 'org_other', 'prj_other', 0, '{}')",
                )
                .bind(webhook.id.as_str()),
            )
            .await
            .unwrap();
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0).await,
                Err(PortableError::Conflict("import.id_conflict"))
            );
            for unit in &data.rows.runs {
                let id = &unit.slice.run.projection.execution_id;
                let object = store.objects().object(id, &p()).unwrap();
                assert!(object.load(id).await.is_err(), "{id} was purged");
            }
            for blob in &data.rows.blobs {
                assert!(
                    store.blobs().export_get(&blob.key).await.is_err(),
                    "{:?} was deleted",
                    blob.key
                );
            }
            // With the conflict gone, the same archive imports cleanly.
            runbook_store_d1::D1::run(
                &db,
                runbook_store_d1::prepare(
                    "DELETE FROM webhook_subscription WHERE subscription_id = ?",
                )
                .bind(webhook.id.as_str()),
            )
            .await
            .unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            let some_shared = data
                .rows
                .blobs
                .iter()
                .find(|b| matches!(b.key, BlobKey::Shared { .. }))
                .expect("version snapshots travel as shared blobs");
            assert_eq!(
                store.blobs().get(&some_shared.key).await.unwrap(),
                some_shared.bytes
            );
        });
    });
}

#[test]
fn logs_moved_to_r2_travel_as_claim_logs_and_round_trip() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            // exe_6's agent claim, still held: past 64 KiB of lines the
            // store moves a chunk to R2.
            let exe6 = fixture::exe(6);
            let unit = data
                .rows
                .runs
                .iter()
                .find(|u| u.slice.run.projection.execution_id == exe6)
                .unwrap();
            let (step, claim) = unit.slice.run.claims.iter().next().unwrap();
            let lines: Vec<String> = (0..100)
                .map(|n| format!("{n:04} {}", "x".repeat(990)))
                .collect();
            runbook_core::claims::append_logs(
                &store,
                &fixture::agent(),
                &exe6,
                step,
                &runbook_model::LogsRequest {
                    claim: runbook_model::ClaimRef {
                        id: claim.claim_id.clone(),
                        generation: std::num::NonZeroU64::new(claim.generation).unwrap(),
                    },
                    logs: lines.clone(),
                },
                None,
            )
            .await
            .unwrap();
            let object = store.objects().object(&exe6, &p()).unwrap();
            assert!(
                !object.log_chunks(&exe6, step, 0).await.unwrap().is_empty(),
                "a chunk moved to R2"
            );
            let exported = cf::export(&store).await.unwrap();
            assert_eq!(
                exported.rows.blobs.len(),
                data.rows.blobs.len(),
                "no chunk is a blob"
            );
            let logs = &exported
                .rows
                .runs
                .iter()
                .find(|u| u.slice.run.projection.execution_id == exe6)
                .unwrap()
                .claim_logs;
            let all: Vec<&String> = logs.iter().flat_map(|l| &l.lines).collect();
            assert_eq!(all.len(), 2 + lines.len());
            assert_eq!(
                all.last().map(|l| l.as_str()),
                lines.last().map(String::as_str)
            );
            // Another Cloudflare store holds them inline and exports the same.
            let first = portable::encode(&exported, CREATED).unwrap();
            let other = CrossFactory::new(NativeCross);
            let (target, _) = other.language(p(), T0).await.unwrap();
            cf::import(&target, &exported, ImportOptions::default(), T0)
                .await
                .unwrap();
            let again = portable::encode(&cf::export(&target).await.unwrap(), CREATED).unwrap();
            assert_eq!(first, again);
        });
    });
}

/// 2100-01-01: the workerd fixture's clock. Its runs' timers and leases
/// fall due after it, so nothing fires between the test's import and its
/// export (workerd runs the objects' alarms; an archive whose timers are
/// already due resumes at once after import, as it should).
const FUTURE: i64 = 4_102_444_800_000;

/// `cloudflare/tests/portable.test.mjs` imports this ZIP through the
/// operator route in workerd: the fixture archive at [`FUTURE`], kept
/// current here (regenerate with `UPDATE_FIXTURES=1`).
#[test]
fn the_workerd_fixture_is_the_current_fixture_archive() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source_at(FUTURE)).unwrap();
        let zip = cf::to_zip(&data, CREATED).unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../cloudflare/tests/fixtures/portable-fixture.zip");
        if std::env::var_os("UPDATE_FIXTURES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &zip).unwrap();
        }
        let committed = std::fs::read(&path).expect("run with UPDATE_FIXTURES=1 to create it");
        assert!(
            committed == zip,
            "{} is stale: run this test with UPDATE_FIXTURES=1",
            path.display()
        );
    });
}

/// Gate fix D2: an import installs its runs held and releases them only
/// after its D1 commit, so a refused batch leaves no derived rows behind and
/// a committed one leaves no run held.
#[test]
fn imported_runs_stay_held_until_the_commit_and_are_released_after_it() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let count = |sql: &'static str| {
                let db = db.clone();
                async move {
                    use runbook_store_d1::sql::Columns;
                    runbook_store_d1::D1::all(&db, runbook_store_d1::prepare(sql))
                        .await
                        .unwrap()[0]
                        .unsigned(0)
                        .unwrap()
                }
            };
            // A refused batch (a subscription ID taken after the guards).
            let webhook = data.webhooks.first().unwrap().clone();
            runbook_store_d1::D1::run(
                &db,
                runbook_store_d1::prepare(
                    "INSERT INTO webhook_subscription (subscription_id, org_id, project_id, \
                     created_at_ms, body) VALUES (?, 'org_other', 'prj_other', 0, '{}')",
                )
                .bind(webhook.id.as_str()),
            )
            .await
            .unwrap();
            assert!(
                cf::import(&store, &data, ImportOptions::default(), T0)
                    .await
                    .is_err()
            );
            for table in [
                "SELECT count(*) FROM execution_owner",
                "SELECT count(*) FROM run_summary",
                "SELECT count(*) FROM assignment",
                "SELECT count(*) FROM run_release",
            ] {
                assert_eq!(count(table).await, 0, "{table}");
            }
            runbook_store_d1::D1::run(
                &db,
                runbook_store_d1::prepare(
                    "DELETE FROM webhook_subscription WHERE subscription_id = ?",
                )
                .bind(webhook.id.as_str()),
            )
            .await
            .unwrap();
            // A committed import releases every run.
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            assert_eq!(count("SELECT count(*) FROM run_release").await, 0);
            for unit in &data.rows.runs {
                let id = &unit.slice.run.projection.execution_id;
                let object = store.objects().object(id, &p()).unwrap();
                assert!(!object.release(id).await.unwrap(), "{id} was released");
            }
        });
    });
}

/// Gate fix D2: an import that stopped after its D1 commit (before
/// releasing) leaves `run_release` rows; the minute cron's pass releases
/// those runs.
#[test]
fn the_cron_releases_runs_an_interrupted_import_left_held() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            // As if the import had stopped before releasing one run.
            let unit = data.rows.runs.first().unwrap();
            let id = unit.slice.run.projection.execution_id.clone();
            let object = store.objects().object(&id, &p()).unwrap();
            object.purge(&id).await.unwrap();
            let mut install = cf::run_install(unit, T0);
            install.held = true;
            object.install(&id, install).await.unwrap();
            runbook_store_d1::D1::run(
                &db,
                runbook_store_d1::prepare(
                    "INSERT INTO run_release (execution_id, org_id, project_id, created_at_ms) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(id.as_str())
                .bind(p().org_id.as_str())
                .bind(p().project_id.as_str())
                .bind(T0),
            )
            .await
            .unwrap();
            let due = runbook_store_d1::portable::pending_releases(&db, T0 + 1, 25)
                .await
                .unwrap();
            assert_eq!(due, vec![(p(), id.clone())]);
            for (scope, run) in due {
                cf::release_run(&store.scoped(scope), &run).await.unwrap();
            }
            assert!(!object.release(&id).await.unwrap(), "released by the pass");
            assert!(
                runbook_store_d1::portable::pending_releases(&db, T0 + 1, 25)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
    });
}

/// Gate round 2: an import that died before its D1 commit left its runs
/// held with no D1 rows. A retry is told another import may be running
/// while the holds are fresh, replaces them once they are stale, and a
/// held run's own expiry purges it when D1 never committed it.
#[test]
fn a_failed_imports_held_orphans_neither_block_a_retry_nor_linger() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            use runbook_store_conformance::SessionFactory;
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            // The died import: every run installed held, nothing in D1.
            for unit in &data.rows.runs {
                let id = unit.slice.run.projection.execution_id.clone();
                let mut install = cf::run_install(unit, T0);
                install.held = true;
                install.import_id = "imp_dead".into();
                let object = store.objects().object(&id, &p()).unwrap();
                object.install(&id, install).await.unwrap();
            }
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0).await,
                Err(PortableError::Conflict("import.in_progress"))
            );
            // Its expiry purges a held run D1 never committed.
            let first = data.rows.runs[0].slice.run.projection.execution_id.clone();
            cf::expire_hold(&store, &first).await.unwrap();
            let object = store.objects().object(&first, &p()).unwrap();
            assert!(object.load(&first).await.is_err(), "purged");
            // Ten minutes on, a retry replaces the rest and commits.
            factory
                .advance_clock(&store, runbook_store_do::portable::HOLD_STALE_MS as u64)
                .await
                .unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            for unit in &data.rows.runs {
                let id = &unit.slice.run.projection.execution_id;
                let object = store.objects().object(id, &p()).unwrap();
                assert!(object.load(id).await.is_ok(), "{id} imported");
                assert!(!object.release(id).await.unwrap(), "{id} released");
            }
            // A committed run's expiry releases, never purges.
            cf::expire_hold(&store, &first).await.unwrap();
            assert!(
                store
                    .objects()
                    .object(&first, &p())
                    .unwrap()
                    .load(&first)
                    .await
                    .is_ok()
            );
        });
    });
}

/// Gate round 2: a run's `run_release` row commits with the control batch
/// (the import's last commit), so it exists only for a fully committed
/// import. A held run whose owner row exists but has no release row is a
/// half-committed import: its expiry leaves it held instead of releasing
/// it. A release the cron cannot finish backs off like a purge.
#[test]
fn releases_follow_only_a_fully_committed_import() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let plan = runbook_store_d1::portable::import_statements(
                &db,
                &data,
                ImportOptions::default(),
                false,
            )
            .await
            .unwrap();
            let names = |s: &[runbook_store_d1::Statement]| {
                s.iter()
                    .filter(|s| s.sql.contains("INTO run_release"))
                    .count()
            };
            assert_eq!(names(&plan.control), data.rows.runs.len());
            assert_eq!(names(&plan.shard), 0);
            // Half-committed: the shard's owner row, no control release row.
            let unit = data.rows.runs.first().unwrap();
            let id = unit.slice.run.projection.execution_id.clone();
            let mut install = cf::run_install(unit, T0);
            install.held = true;
            let object = store.objects().object(&id, &p()).unwrap();
            object.install(&id, install).await.unwrap();
            runbook_store_d1::D1::run(
                &db,
                runbook_store_d1::prepare(
                    "INSERT INTO execution_owner (execution_id, org_id, project_id) VALUES (?, ?, ?)",
                )
                .bind(id.as_str())
                .bind(p().org_id.as_str())
                .bind(p().project_id.as_str()),
            )
            .await
            .unwrap();
            cf::expire_hold(&store, &id).await.unwrap();
            assert!(
                object.release(&id).await.unwrap(),
                "still held, not released"
            );
            // A failing release backs off: due again a minute later.
            runbook_store_d1::D1::run(
                &db,
                runbook_store_d1::prepare(
                    "INSERT INTO run_release (execution_id, org_id, project_id, created_at_ms) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(id.as_str())
                .bind(p().org_id.as_str())
                .bind(p().project_id.as_str())
                .bind(T0),
            )
            .await
            .unwrap();
            runbook_store_d1::portable::defer_release(&db, &id, T0 + 10)
                .await
                .unwrap();
            let due = |at| {
                let db = db.clone();
                async move {
                    !runbook_store_d1::portable::pending_releases(&db, at, 25)
                        .await
                        .unwrap()
                        .is_empty()
                }
            };
            assert!(!due(T0 + 10 + 59_999).await);
            assert!(due(T0 + 10 + 60_001).await);
        });
    });
}

/// CF12c review round 2 (N6): the run's object keeps other JSON bodies in
/// one row too: its head (`meta.projection`: the run's result and outputs
/// and every step's outputs and summary), each claim and each archived log
/// line. Each is refused before any write, as an event is. Before, the
/// import failed at the run's object, mid-write (`do.write`).
#[test]
fn an_oversized_projection_claim_or_log_line_is_refused_before_any_write() {
    on_big_stack(|| {
        let (_, clean) = portable::decode(&source()).unwrap();
        let big = "x".repeat(2_000_000);
        let mut crafted = vec![];
        // The head: one step's summary.
        let mut data = clean.clone();
        let step = data
            .rows
            .runs
            .iter_mut()
            .find_map(|r| r.slice.run.projection.steps.first_mut())
            .expect("the fixture has a step");
        step.summary = Some(big.clone());
        crafted.push(("projection", data));
        // A claim.
        let mut data = clean.clone();
        let claim = data
            .rows
            .runs
            .iter_mut()
            .find_map(|r| r.slice.run.claims.values_mut().next())
            .expect("the fixture has a claim");
        claim.session.credential_ref = big.clone();
        crafted.push(("claim", data));
        // An archived log line.
        let mut data = clean.clone();
        let log = data
            .rows
            .runs
            .iter_mut()
            .find_map(|r| r.claim_logs.first_mut())
            .expect("the fixture has a claim log");
        log.lines.push(format!("{big}x"));
        crafted.push(("log line", data));
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            for (name, data) in &crafted {
                let refused = cf::import(&store, data, ImportOptions::default(), T0).await;
                assert!(
                    matches!(
                        refused,
                        Err(PortableError::Archive {
                            code: "archive.value_too_large",
                            ..
                        })
                    ),
                    "{name}: {refused:?}"
                );
                assert!(
                    store
                        .list_prefix(&BlobPrefix::Shared)
                        .await
                        .unwrap()
                        .is_empty(),
                    "{name}"
                );
            }
            cf::import(&store, &clean, ImportOptions::default(), T0)
                .await
                .unwrap();
        });
    });
}

/// CF13-7: the object keeps the head (the run's result and outputs) and
/// each step run in rows of their own, so an archive of a run whose whole
/// projection is past one row, while the head and each step fit, is
/// imported; before, CF12c's check measured the whole projection and
/// refused it. A single step past one row is still refused before any
/// write (above).
#[test]
fn a_run_whose_steps_together_pass_one_row_imports_when_each_fits() {
    on_big_stack(|| {
        let (_, clean) = portable::decode(&source()).unwrap();
        let mut data = clean.clone();
        let unit = data
            .rows
            .runs
            .iter_mut()
            .find(|r| !r.slice.run.projection.steps.is_empty())
            .expect("the fixture has a run with a step");
        let id = unit.slice.run.projection.execution_id.clone();
        let big = "y".repeat(1_100_000);
        let projection = &mut unit.slice.run.projection;
        projection.result = serde_json::Value::String("z".repeat(1_000_000));
        projection.steps[0].summary = Some(big.clone());
        let whole = serde_json::to_vec(&unit.slice.run.projection)
            .unwrap()
            .len();
        assert!(whole > runbook_core::MAX_ROW_BYTES, "{whole}");
        portable::check_row_bounds(&data).expect("each row fits");
        run(async move {
            let factory = CrossFactory::new(NativeCross);
            let (store, _) = factory.language(p(), T0).await.unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .expect("the run imports");
            let out = cf::export(&store).await.unwrap();
            let run = out
                .rows
                .runs
                .iter()
                .find(|r| r.slice.run.projection.execution_id == id)
                .expect("the run is exported");
            let projection = &run.slice.run.projection;
            assert_eq!(projection.steps[0].summary.as_deref(), Some(big.as_str()));
            assert_eq!(projection.result.as_str().map(str::len), Some(1_000_000));
        });
    });
}
