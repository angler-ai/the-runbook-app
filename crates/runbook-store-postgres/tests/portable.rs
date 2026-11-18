//! D3 portable project archive on PostgreSQL: Postgres -> archive -> memory
//! -> archive and memory -> Postgres -> archive are byte-identical, a run
//! exported mid-flight resumes after import, and a failed import leaves no
//! rows behind.
mod support;
use runbook_core::{
    actor::begin_actor,
    decisions::vote,
    directory::memory::MemoryDirectory,
    drain::{Processed, process_lease},
    memory::MemoryExecutionStore,
    portable::{self, Archive, ImportOptions, PortableError},
    step_lookup::StepRunLookup,
    webhooks::{WebhookStore, memory::MemoryWebhooks},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{DecisionRequest, ExecutionStatus};
use runbook_store_conformance::portable::{self as fixture, exe, human, p, q, rb, webhook};
use runbook_store_postgres::{DatabaseConfig, PostgresWebhooks, export_project, import_project};
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use support::factory::{PostgresFactory, with_factory};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory adapters must be synchronous"),
    }
}
const T0: i64 = 1_700_000_000_000;
const CREATED: i64 = 1_800_000_000_000;

struct Memory {
    store: MemoryExecutionStore,
    directory: MemoryDirectory,
    webhooks: MemoryWebhooks,
}
fn memory() -> Memory {
    memory_at(T0)
}
fn memory_at(now: i64) -> Memory {
    let directory = MemoryDirectory::default();
    directory.set_time_ms(now);
    Memory {
        store: MemoryExecutionStore::empty(p(), now),
        directory,
        webhooks: MemoryWebhooks::new(now),
    }
}
fn memory_archive(m: &Memory) -> Archive {
    portable::encode(
        &portable::memory::export(&m.store, &m.directory, &m.webhooks).unwrap(),
        CREATED,
    )
    .unwrap()
}
async fn postgres_archive(config: &DatabaseConfig) -> Archive {
    portable::encode(&export_project(config, &p()).await.unwrap(), CREATED).unwrap()
}
async fn import(config: &DatabaseConfig, archive: &Archive) -> Result<(), PortableError> {
    import_with(config, archive, ImportOptions::default()).await
}
async fn import_with(
    config: &DatabaseConfig,
    archive: &Archive,
    options: ImportOptions,
) -> Result<(), PortableError> {
    let (_, data) = portable::decode(archive)?;
    import_project(config, &data, options).await
}
const INTO_EXISTING: ImportOptions = ImportOptions {
    into_existing_org: true,
};
/// Every stored row of one execution, byte for byte, plus its tombstone.
async fn victim_rows(f: &PostgresFactory, id: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for sql in [
        "SELECT projection || revision::text FROM runbook.execution WHERE execution_id = $1",
        "SELECT body FROM runbook.run_summary WHERE execution_id = $1",
        "SELECT body FROM runbook.execution_event WHERE execution_id = $1 ORDER BY seq",
        "SELECT body FROM runbook.assignment WHERE execution_id = $1 ORDER BY ordinal",
        "SELECT body FROM runbook.execution_start WHERE execution_id = $1",
        "SELECT outcome FROM runbook.start_replay WHERE execution_id = $1",
        "SELECT request || state FROM runbook.admission WHERE execution_id = $1",
        "SELECT body FROM runbook.tombstone WHERE execution_id = $1",
        "SELECT org_id || project_id FROM runbook.execution_owner WHERE execution_id = $1",
    ] {
        for row in f.admin.client().query(sql, &[&id]).await.unwrap() {
            rows.push(row.get::<_, String>(0));
        }
        rows.push("|".into());
    }
    rows
}
async fn directory_rows(f: &PostgresFactory) -> Vec<String> {
    let mut rows = Vec::new();
    for sql in [
        "SELECT org_id || title FROM runbook.organization ORDER BY 1",
        "SELECT user_id || name FROM runbook.user_identity ORDER BY 1",
        "SELECT org_id || user_id || name || active::text || org_role FROM runbook.org_user ORDER BY 1",
        "SELECT team_id || org_id || slug || title FROM runbook.team ORDER BY 1",
        "SELECT project_id || user_id || role FROM runbook.project_member ORDER BY 1",
        "SELECT team_id || user_id FROM runbook.team_member ORDER BY 1",
    ] {
        for row in f.admin.client().query(sql, &[]).await.unwrap() {
            rows.push(row.get::<_, String>(0));
        }
    }
    rows
}
fn contains(archive: &Archive, needle: &str) -> bool {
    archive.files.values().any(|bytes| {
        bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    })
}
async fn count(f: &PostgresFactory, table: &str) -> i64 {
    f.admin
        .client()
        .query_one(&format!("SELECT count(*) FROM runbook.{table}"), &[])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn postgres_archive_round_trips_through_memory() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let webhooks = PostgresWebhooks::new(f.config.clone());
        fixture::build(&store, &store.scoped(q()), &f.fresh_directory(), &webhooks).await;
        let first = postgres_archive(&f.config).await;
        // Postgres -> archive -> memory -> archive is byte-identical.
        let target = memory();
        let (_, data) = portable::decode(&first).unwrap();
        portable::memory::import(
            &target.store,
            &target.directory,
            &target.webhooks,
            &data,
            ImportOptions::default(),
        )
        .unwrap();
        assert_eq!(memory_archive(&target), first);
        assert_eq!(data.rows.runs.len(), 4);
        // D42: version 2 with P's memory (an edited entry, a proposal).
        assert_eq!(data.rows.memory.len(), 2);
        assert_eq!(data.rows.memory[0].revisions.len(), 2);
        assert!(data.rows.memory[1].proposal_sequence.is_some());
        assert!(!contains(&first, "project Q memory body"));
        assert_eq!(data.rows.runs[3].claim_logs[0].lines.len(), 2);
        assert_eq!(data.rows.pools[0].queue, [exe(2)]);
        // Never project Q's rows, never a secret.
        for foreign in [
            q().project_id.to_string(),
            exe(5).to_string(),
            rb(3).to_string(),
            webhook(2).to_string(),
            "foreign project log".into(),
            fixture::SEALED_SECRET.into(),
            fixture::SEALED_PREVIOUS.into(),
            fixture::SEALED_SECRET_Q.into(),
            "trb_".into(),
        ] {
            assert!(!contains(&first, &foreign), "archive contains {foreign}");
        }
        // Export is a read: a second export is identical.
        assert_eq!(postgres_archive(&f.config).await, first);
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn memory_archive_round_trips_through_postgres_and_resumes() {
    with_factory(async |f| {
        // Built so the one-hour deadline falls a few seconds after import
        // on the database clock: the approval is still open when imported.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let source = memory_at(now - 3_600_000 + 4_000);
        ready(fixture::build(
            &source.store,
            &source.store.scoped(q()),
            &source.directory,
            &source.webhooks,
        ));
        let first = memory_archive(&source);
        import(&f.config, &first).await.unwrap();
        assert_eq!(postgres_archive(&f.config).await, first);

        // The run exported mid-flight resumes on Postgres: the approval
        // decision works and the pending deadline timer fires.
        let store = f.fresh_store(p()).await;
        let directory = f.fresh_directory();
        let work = begin_actor(&store, &directory, p(), &exe(1)).await.unwrap();
        let gate = work
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "gate")
            .and_then(|s| s.id.clone())
            .unwrap();
        // Step URLs resolve through the rebuilt step index.
        assert_eq!(
            store.step_run_execution(&p().org_id, &gate).await.unwrap(),
            Some((exe(1), p().project_id))
        );
        vote(
            work,
            &human(1),
            &gate,
            &DecisionRequest {
                option: "keep".into(),
                comment: None,
                session_id: None,
                actor: None,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            fixture::status(&store, 1).await.unwrap(),
            ExecutionStatus::Running
        );
        // The pending deadline comes due on the database clock and fires.
        let mut timer = None;
        for _ in 0..80 {
            timer = store
                .claim_due(32, 60_000)
                .await
                .unwrap()
                .into_iter()
                .find(|l| {
                    l.id.execution_id == exe(1) && matches!(l.id.kind, WorkKind::Timer { .. })
                });
            if timer.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let timer = timer.expect("deadline timer comes due");
        assert_eq!(
            process_lease(&store, &directory, p(), &timer)
                .await
                .unwrap(),
            Processed::Fired
        );
        assert_eq!(
            fixture::status(&store, 1).await.unwrap(),
            ExecutionStatus::Failed
        );
        // The released admission promotes the queued run, which activates.
        let activation = store
            .claim_due(32, 60_000)
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.id.execution_id == exe(2) && l.id.kind == WorkKind::Activation)
            .expect("queued run promoted");
        assert_eq!(
            process_lease(&store, &directory, p(), &activation)
                .await
                .unwrap(),
            Processed::Activated
        );
        // It waits on its human approval (R50).
        assert_eq!(
            fixture::status(&store, 2).await.unwrap(),
            ExecutionStatus::Waiting
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn failed_import_rolls_back_and_non_empty_targets_are_refused() {
    with_factory(async |f| {
        let source = memory();
        ready(fixture::build(
            &source.store,
            &source.store.scoped(q()),
            &source.directory,
            &source.webhooks,
        ));
        let good = memory_archive(&source);

        // Tampered: rejected before the database is touched.
        let mut tampered = good.clone();
        tampered
            .files
            .get_mut("runbook_versions.ndjson")
            .unwrap()
            .push(b'\n');
        assert!(matches!(
            import(&f.config, &tampered).await,
            Err(PortableError::Archive {
                code: "archive.size_mismatch",
                ..
            })
        ));

        // The last rows written conflict (a subscription ID held by another
        // project): the whole transaction rolls back.
        let webhooks = PostgresWebhooks::new(f.config.clone());
        let mut foreign = q();
        foreign.project_id = "prj_00000000000000000000000077".parse().unwrap();
        let mut taken = ready(source.webhooks.get_subscription(&p(), &webhook(1)))
            .unwrap()
            .unwrap();
        taken.scope = foreign.clone();
        webhooks
            .create_subscription(&foreign, taken, 20, None)
            .await
            .unwrap();
        assert_eq!(
            import(&f.config, &good).await,
            Err(PortableError::Conflict("import.id_conflict"))
        );
        for table in [
            "organization",
            "project",
            "runbook",
            "runbook_version",
            "execution_owner",
            "execution_event",
            "admission_pool",
            "blob",
            "tombstone",
            "project_variables",
        ] {
            assert_eq!(count(f, table).await, 0, "{table} kept rows");
        }
        webhooks
            .delete_subscription(&foreign, &webhook(1))
            .await
            .unwrap();

        // With the conflict gone the same archive imports; a second import
        // into the now non-empty project is refused and changes nothing.
        import(&f.config, &good).await.unwrap();
        let events = count(f, "execution_event").await;
        assert_eq!(
            import(&f.config, &good).await,
            Err(PortableError::Conflict("import.org_exists"))
        );
        assert_eq!(
            import_with(&f.config, &good, INTO_EXISTING).await,
            Err(PortableError::TargetNotEmpty)
        );
        assert_eq!(count(f, "execution_event").await, events);
        assert_eq!(postgres_archive(&f.config).await, good);
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn crafted_archives_cannot_touch_a_victim_run() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let webhooks = PostgresWebhooks::new(f.config.clone());
        fixture::build(&store, &store.scoped(q()), &f.fresh_directory(), &webhooks).await;
        let victim = exe(5).to_string();
        let before = victim_rows(f, &victim).await;
        let directory = directory_rows(f).await;
        let projects = count(f, "project").await;

        // A new organization whose archive tombstones the victim run.
        let mut tombstone = fixture::attacker_archive();
        tombstone.scope.org_id = "org_00000000000000000000000009".parse().unwrap();
        tombstone.directory.organization.id = tombstone.scope.org_id.clone();
        tombstone.directory.project.org_id = tombstone.scope.org_id.clone();
        tombstone
            .directory
            .users
            .retain(|u| u.id == fixture::attacker());
        tombstone
            .directory
            .project_members
            .retain(|m| m.user_id == fixture::attacker());
        tombstone.directory.teams.clear();
        tombstone.directory.team_members.clear();
        tombstone
            .rows
            .tombstones
            .push(runbook_core::retention::Tombstone {
                scope: tombstone.scope.clone(),
                execution_id: exe(5),
                runbook_id: rb(3),
                deleted_at_ms: T0,
                reason: "frozen".into(),
            });
        let archive = portable::encode(&tombstone, CREATED).unwrap();
        assert_eq!(
            import(&f.config, &archive).await,
            Err(PortableError::Conflict("import.id_conflict"))
        );

        // P's own export with a run summary renamed to the victim, handed
        // straight to import (no decode): rejected before any write.
        let mut summary = export_project(&f.config, &p()).await.unwrap();
        summary.rows.runs[2]
            .slice
            .catalog
            .summary
            .as_mut()
            .unwrap()
            .execution_id = exe(5);
        assert!(matches!(
            import_project(&f.config, &summary, INTO_EXISTING).await,
            Err(PortableError::Archive {
                code: "archive.foreign_id",
                ..
            })
        ));
        assert_eq!(victim_rows(f, &victim).await, before);
        assert_eq!(directory_rows(f).await, directory);
        assert_eq!(count(f, "project").await, projects);
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn existing_org_is_refused_by_default_and_never_escalated() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let webhooks = PostgresWebhooks::new(f.config.clone());
        fixture::build(&store, &store.scoped(q()), &f.fresh_directory(), &webhooks).await;
        let crafted = fixture::attacker_archive();
        let archive = portable::encode(&crafted, CREATED).unwrap();
        let before = directory_rows(f).await;
        assert_eq!(
            import(&f.config, &archive).await,
            Err(PortableError::Conflict("import.org_exists"))
        );
        assert_eq!(directory_rows(f).await, before);

        import_with(&f.config, &archive, INTO_EXISTING)
            .await
            .unwrap();
        let after = directory_rows(f).await;
        // Every directory row that existed is unchanged.
        for row in &before {
            assert!(after.contains(row), "changed: {row}");
        }
        let role: String = f
            .admin
            .client()
            .query_one(
                "SELECT org_role FROM runbook.org_user WHERE user_id = $1",
                &[&fixture::attacker().as_str()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(role, "member");
        let hijack: i64 = f
            .admin
            .client()
            .query_one(
                "SELECT count(*) FROM runbook.team_member WHERE team_id = $1 AND user_id = $2",
                &[&fixture::team().as_str(), &fixture::attacker().as_str()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(hijack, 0);
        let alice: String = f
            .admin
            .client()
            .query_one(
                "SELECT org_role FROM runbook.org_user WHERE user_id = $1",
                &[&fixture::user(1).as_str()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(alice, "member");

        // A user ID that exists on the instance but not in the target
        // organization (here: a new organization) is rejected.
        let mut stranger = fixture::attacker_archive();
        stranger.scope.org_id = "org_00000000000000000000000008".parse().unwrap();
        stranger.scope.project_id = "prj_00000000000000000000000088".parse().unwrap();
        stranger.directory.organization.id = stranger.scope.org_id.clone();
        stranger.directory.project.id = stranger.scope.project_id.clone();
        stranger.directory.project.org_id = stranger.scope.org_id.clone();
        stranger.directory.teams.clear();
        stranger.directory.team_members.clear();
        let archive = portable::encode(&stranger, CREATED).unwrap();
        assert_eq!(
            import(&f.config, &archive).await,
            Err(PortableError::Conflict("import.user_exists"))
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn import_waits_for_writers_holding_the_existing_project() {
    use runbook_core::directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord,
    };
    with_factory(async |f| {
        // The organization and an empty project P exist on the target.
        f.fresh_directory()
            .commit(DirectoryCommit {
                org_id: p().org_id,
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Org".into(),
                    },
                    DirectoryMutation::CreateProject(ProjectRecord {
                        id: p().project_id,
                        org_id: p().org_id,
                        slug: "api".into(),
                        title: "API".into(),
                    }),
                ],
            })
            .await
            .unwrap();
        let source = memory();
        ready(fixture::build(
            &source.store,
            &source.store.scoped(q()),
            &source.directory,
            &source.webhooks,
        ));
        let mut data =
            portable::memory::export(&source.store, &source.directory, &source.webhooks).unwrap();
        data.variables = None;
        // A live writer is mid-transaction on P (an uncommitted row whose
        // foreign key holds P's project row).
        let writer = runbook_store_postgres::PostgresStore::connect(&f.config)
            .await
            .unwrap();
        writer
            .client()
            .batch_execute(&format!(
                "BEGIN; INSERT INTO runbook.project_variables (org_id, project_id, body, revision) \
                 VALUES ('{}', '{}', '{{\"declarations\":{{}},\"values\":{{}}}}', 1)",
                p().org_id,
                p().project_id
            ))
            .await
            .unwrap();
        // The import locks P's row first, so it waits instead of reading
        // "empty" and merging with the writer's data.
        let impatient = f.config.clone().with_lock_timeouts(
            std::time::Duration::from_millis(300),
            std::time::Duration::from_secs(30),
        );
        assert_eq!(
            import_project(&impatient, &data, INTO_EXISTING).await,
            Err(PortableError::Store(runbook_core::StoreError::Backend(
                "postgres.lock_timeout"
            )))
        );
        assert_eq!(count(f, "runbook").await, 0);
        writer.client().batch_execute("ROLLBACK").await.unwrap();
        drop(writer);
        import_project(&f.config, &data, INTO_EXISTING)
            .await
            .unwrap();
        assert_eq!(count(f, "runbook").await, 2);
        Ok(())
    })
    .await;
}

/// F35 (R44): an authoring commit holds the organization's directory
/// revision lock while it writes catalog rows that have no foreign key to
/// the project. The import takes that lock before its emptiness check, so
/// it waits, then sees the committed row and refuses, instead of merging.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn import_waits_for_an_authoring_commit_then_refuses() {
    use runbook_core::directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord,
    };
    with_factory(async |f| {
        f.fresh_directory()
            .commit(DirectoryCommit {
                org_id: p().org_id,
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Org".into(),
                    },
                    DirectoryMutation::CreateProject(ProjectRecord {
                        id: p().project_id,
                        org_id: p().org_id,
                        slug: "api".into(),
                        title: "API".into(),
                    }),
                ],
            })
            .await
            .unwrap();
        let source = memory();
        ready(fixture::build(
            &source.store,
            &source.store.scoped(q()),
            &source.directory,
            &source.webhooks,
        ));
        let mut data =
            portable::memory::export(&source.store, &source.directory, &source.webhooks).unwrap();
        data.variables = None;
        // As commit_authoring does: lock the revision row, then write.
        let writer = runbook_store_postgres::PostgresStore::connect(&f.config)
            .await
            .unwrap();
        writer
            .client()
            .batch_execute(&format!(
                "BEGIN; SELECT revision FROM runbook.directory_revision \
                 WHERE org_id = '{org}' FOR UPDATE; \
                 INSERT INTO runbook.draft (org_id, project_id, key, revision, source) \
                 VALUES ('{org}', '{project}', 'live', 1, 'draft')",
                org = p().org_id,
                project = p().project_id
            ))
            .await
            .unwrap();
        let observer = runbook_store_postgres::PostgresStore::connect(&f.config)
            .await
            .unwrap();
        let commit = async {
            // Bounded: commit once the import waits on a lock, or give up.
            let mut parked = false;
            for _ in 0..1_000 {
                let waiting: i64 = observer
                    .client()
                    .query_one(
                        "SELECT count(*) FROM pg_stat_activity \
                         WHERE datname = current_database() AND wait_event_type = 'Lock'",
                        &[],
                    )
                    .await
                    .unwrap()
                    .get(0);
                if waiting > 0 {
                    parked = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            writer.client().batch_execute("COMMIT").await.unwrap();
            parked
        };
        let (imported, parked) =
            tokio::join!(import_project(&f.config, &data, INTO_EXISTING), commit);
        assert!(parked, "the import never waited for the authoring commit");
        assert_eq!(imported, Err(PortableError::TargetNotEmpty));
        assert_eq!(count(f, "runbook").await, 0);
        assert_eq!(count(f, "draft").await, 1);
        Ok(())
    })
    .await;
}

/// The archive the schema version 1 encoder wrote (checked in with the
/// conformance crate).
fn v1_fixture() -> Archive {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../runbook-store-conformance/tests/fixtures/archive-v1");
    let mut archive = Archive::default();
    for dir in [root.clone(), root.join("blobs")] {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                let name = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                archive.files.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    archive
}

/// D42: a version 1 archive imports into PostgreSQL and re-exports as
/// version 2, byte-identical to the memory adapters' re-export.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn version_1_archive_imports_into_postgres() {
    with_factory(async |f| {
        let v1 = v1_fixture();
        let (manifest, data) = portable::decode(&v1).unwrap();
        assert_eq!(manifest.schema_version, portable::SCHEMA_VERSION_V1);
        import(&f.config, &v1).await.unwrap();
        let from_postgres = postgres_archive(&f.config).await;
        let target = memory();
        portable::memory::import(
            &target.store,
            &target.directory,
            &target.webhooks,
            &data,
            ImportOptions::default(),
        )
        .unwrap();
        assert_eq!(from_postgres, memory_archive(&target));
        let (manifest, again) = portable::decode(&from_postgres).unwrap();
        assert_eq!(manifest.schema_version, portable::SCHEMA_VERSION);
        assert_eq!(again, data);
        assert_eq!(count(f, "memory_entry").await, 0);
        Ok(())
    })
    .await;
}

/// A memory archive with entries imports into PostgreSQL and exports back
/// byte-identically; importing it again is refused (not empty, IDs taken).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn memory_rows_round_trip_through_postgres() {
    with_factory(async |f| {
        let source = memory();
        fixture::build(
            &source.store,
            &source.store.scoped(q()),
            &source.directory,
            &source.webhooks,
        )
        .await;
        let first = memory_archive(&source);
        import(&f.config, &first).await.unwrap();
        assert_eq!(postgres_archive(&f.config).await, first);
        assert_eq!(count(f, "memory_entry").await, 2);
        assert_eq!(count(f, "memory_revision").await, 3);
        assert!(matches!(
            import_with(&f.config, &first, INTO_EXISTING).await,
            Err(PortableError::TargetNotEmpty)
        ));
        Ok(())
    })
    .await;
}

/// CF8: Postgres -> archive -> local Workers -> archive is byte-identical
/// apart from the manifest's `created_at_ms`. The Workers leg is
/// `cloudflare/scripts/portable-roundtrip.mjs`: the built api Worker under
/// workerd, the archive imported and exported through the operator route.
/// It needs Node and `cloudflare/build`, so it runs when
/// `RUNBOOK_CF_ROUNDTRIP=1` (`CHECK_CLOUDFLARE=1 scripts/check.sh` sets it
/// after building the Worker) and is skipped, saying so, otherwise.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn postgres_archive_round_trips_through_local_workers() {
    if std::env::var("RUNBOOK_CF_ROUNDTRIP").as_deref() != Ok("1") {
        eprintln!(
            "SKIPPED postgres_archive_round_trips_through_local_workers: set RUNBOOK_CF_ROUNDTRIP=1 \
             with a built Worker (cd cloudflare && npm run build)"
        );
        return;
    }
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let webhooks = PostgresWebhooks::new(f.config.clone());
        fixture::build(&store, &store.scoped(q()), &f.fresh_directory(), &webhooks).await;
        let first = postgres_archive(&f.config).await;
        let dir = std::env::temp_dir().join(format!("runbook-cf8-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (input, output) = (dir.join("postgres.zip"), dir.join("workers.zip"));
        let entries: Vec<(String, Vec<u8>)> = first.files.clone().into_iter().collect();
        std::fs::write(
            &input,
            runbook_core::archives::zip::write(&entries, 0).unwrap(),
        )
        .unwrap();
        let cloudflare = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cloudflare");
        let run = std::process::Command::new("node")
            .arg("scripts/portable-roundtrip.mjs")
            .arg(&input)
            .arg(&output)
            .current_dir(&cloudflare)
            .output()
            .expect("node runs");
        assert!(
            run.status.success(),
            "the Workers leg failed:\n{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        let bytes = std::fs::read(&output).unwrap();
        let zip = runbook_core::archives::zip::Zip::parse(&bytes).unwrap();
        let mut again = Archive::default();
        for entry in &zip.entries {
            again
                .files
                .insert(entry.name.clone(), zip.read(&entry.name).unwrap().unwrap());
        }
        let _ = std::fs::remove_dir_all(&dir);
        let manifest = |archive: &Archive| {
            let mut value: serde_json::Value =
                serde_json::from_slice(&archive.files[portable::MANIFEST]).unwrap();
            value.as_object_mut().unwrap().remove("created_at_ms");
            value
        };
        assert_eq!(manifest(&again), manifest(&first));
        assert_eq!(
            first.files.keys().collect::<Vec<_>>(),
            again.files.keys().collect::<Vec<_>>()
        );
        for (path, bytes) in &first.files {
            if path != portable::MANIFEST {
                assert!(again.files[path] == *bytes, "{path} differs");
            }
        }
        // And the Workers export decodes to the same project.
        assert_eq!(
            portable::decode(&again).unwrap().1,
            portable::decode(&first).unwrap().1
        );
        Ok(())
    })
    .await;
}

/// CF12c (CF12b's final review, item 2): a start record past one Durable
/// Object or D1 row imports into Postgres, which holds it, as an older
/// native export may carry one. Only the Cloudflare edition refuses it.
/// Before, the whole archive was refused (`archive.value_too_large`).
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn an_oversized_start_record_imports_natively() {
    with_factory(async |f| {
        let source = memory();
        fixture::build(
            &source.store,
            &source.store.scoped(q()),
            &source.directory,
            &source.webhooks,
        )
        .await;
        let mut data =
            portable::memory::export(&source.store, &source.directory, &source.webhooks).unwrap();
        data.rows
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
        import_project(&f.config, &data, ImportOptions::default())
            .await
            .unwrap();
        // It exports again unchanged.
        assert_eq!(export_project(&f.config, &p()).await.unwrap(), data);
        Ok::<(), runbook_core::StoreError>(())
    })
    .await;
}
