//! B2.4: B2a.3 blob and B2a.4 run-slice cases unchanged on Postgres, the
//! D21 shared-blob byte-digest address, and complete-tier registration.
mod support;
use runbook_core::{
    StoreError,
    blob::{BlobKey, BlobStore},
};
use runbook_store_conformance::scope;
use sha2::{Digest, Sha256};
use support::factory::with_factory;

macro_rules! shared {
    ($($module:ident :: $name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| runbook_store_conformance::$module::$name(f).await).await;
        }
    )*};
}
shared!(
    blob::blob_roundtrip_preserves_chunk_boundaries,
    blob::shared_blobs_are_separate_and_immutable,
    retention::export_contains_one_complete_run,
    retention::delete_leaves_only_tombstone,
    retention::finished_day_uses_utc_half_open_interval,
    retention::deletion_is_atomic_and_rejects_open_runs,
);

fn snapshot(default: &str, threshold: &str) -> runbook_spec::CompiledRunbook {
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: numbers, title: Numbers}}\n\
         spec:\n  inputs: {{ratio: {{type: number, default: {default}}}}}\n  steps:\n    \
         - id: check\n      title: Check\n      with: {{threshold: {threshold}}}\n"
    );
    runbook_spec::compile(&runbook_spec::parse(&source).unwrap(), &Default::default()).unwrap()
}

/// D21 (revised): a shared blob key is the SHA-256 of its bytes. Two
/// snapshots with one semantic hash but different spellings — hence
/// different bytes — live at different addresses, each immutable.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn shared_blob_key_is_a_byte_digest_address() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        let base = snapshot("3.0", "0.5");
        let spelled = snapshot("3.00", "0.50");
        assert_eq!(base.hash(), spelled.hash());
        assert_ne!(base.snapshot_bytes(), spelled.snapshot_bytes());
        let key_of = |bytes: &[u8]| BlobKey::Shared {
            hash: Sha256::digest(bytes).into(),
        };
        let (a, b) = (
            key_of(base.snapshot_bytes()),
            key_of(spelled.snapshot_bytes()),
        );
        assert_ne!(a, b);
        store.put(a.clone(), base.snapshot_bytes()).await?;
        store.put(b.clone(), spelled.snapshot_bytes()).await?;
        assert_eq!(store.get(&a).await?, base.snapshot_bytes());
        assert_eq!(store.get(&b).await?, spelled.snapshot_bytes());
        // The address is content: the same digest with other bytes is rejected.
        assert!(
            store
                .put(a.clone(), spelled.snapshot_bytes())
                .await
                .is_err()
        );
        store.put(a.clone(), base.snapshot_bytes()).await?;
        Ok(())
    })
    .await;
}

/// Every conformance tier is implemented by the one Postgres factory, with
/// zero derived-row delay. One case per module runs end to end here; each
/// case is also registered by name in its own test file.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn postgres_all_store_cases_pass() {
    use runbook_store_conformance::*;
    with_factory(async |f| {
        assert_eq!(
            StoreFactory::derived_row_delay(f),
            std::time::Duration::ZERO
        );
        execution::atomic_batch_or_nothing(f).await?;
        catalog::project_queries_never_return_foreign_rows(f).await?;
        clock::session_time_and_snapshot_are_fenced(f).await?;
        queue::due_item_has_one_current_worker(f).await?;
        blob::blob_roundtrip_preserves_chunk_boundaries(f).await?;
        retention::delete_leaves_only_tombstone(f).await?;
        directory::directory_snapshot_is_org_scoped(f)
            .await
            .unwrap();
        eligibility::approval_requires_current_target_match(f)
            .await
            .unwrap();
        Ok::<(), StoreError>(())
    })
    .await;
}

/// F31: deleting a run removes its webhook events, deliveries and attempts
/// (envelopes carry titles and recipient IDs), leaves a sibling run's rows,
/// and a fan-out that arrives after the deletion records nothing.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn delete_removes_the_runs_webhook_rows() {
    use runbook_core::webhooks::{
        AttemptCompletion, AttemptRecord, EventRecord, SealedSecret, Subscription, WebhookStore,
        new_delivery,
    };
    use runbook_model::{AttemptOutcome, DeliveryState};
    use runbook_store_conformance::catalog::id;
    with_factory(async |f| {
        let webhooks = runbook_store_postgres::PostgresWebhooks::new(f.config.clone());
        let at = webhooks.webhook_time_ms().await?;
        let subscription = Subscription {
            id: format!("whk_{:026}", 1).parse().unwrap(),
            scope: scope(1),
            name: "hook".into(),
            url: "https://hooks.example/1".into(),
            events: vec!["*".into()],
            runbooks: vec![],
            tags: vec![],
            enabled: true,
            secret: SealedSecret("sealed".into()),
            previous_secret: None,
            created_at_ms: at,
            updated_at_ms: at,
            secret_rotated_at_ms: at,
            failing_since_ms: None,
            disabled_reason: None,
            disabled_at_ms: None,
            last_failure_at_ms: None,
            revision: 0,
            notice: None,
        };
        webhooks
            .create_subscription(&scope(1), subscription.clone(), 20, None)
            .await?;
        let event = |run: u8, n: u32| EventRecord {
            event_id: format!("evt_{n}"),
            event_type: "approval.requested".into(),
            execution_id: Some(id(run)),
            step_run_id: None,
            human: true,
            created_at_ms: at,
            body: format!("{{\"id\":\"evt_{n}\",\"title\":\"Verify run {run}\"}}"),
        };
        for run in [1, 2] {
            let e = event(run, u32::from(run));
            let id = format!("whd_{:026}", run).parse().unwrap();
            let delivery = new_delivery(id, &subscription, &e, None, at);
            assert!(
                webhooks
                    .record_event(&scope(1), e, vec![delivery], None)
                    .await?
            );
        }
        for lease in webhooks.claim_deliveries(&scope(1), 10, 60_000).await? {
            let completion = AttemptCompletion {
                attempt: AttemptRecord {
                    delivery_id: lease.delivery.id.clone(),
                    number: 1,
                    at_ms: at,
                    status: Some(200),
                    latency_ms: 1,
                    outcome: AttemptOutcome::Delivered,
                    error: None,
                    snippet: None,
                },
                state: DeliveryState::Delivered,
                next_attempt_at_ms: None,
                failure: None,
                health: Some(true),
            };
            webhooks
                .complete_attempt(&scope(1), &lease.delivery.id, lease.generation, completion)
                .await?;
        }
        // The shared case seeds runs 1 and 2 without truncating webhook
        // tables, then requires zero rows of run 1 in every owned table.
        runbook_store_conformance::retention::delete_leaves_only_tombstone(f).await?;
        assert!(
            webhooks
                .execution_events(&scope(1), &id(1))
                .await?
                .is_empty()
        );
        assert_eq!(webhooks.execution_events(&scope(1), &id(2)).await?.len(), 1);
        // A late fan-out for the deleted run is refused, not recorded.
        assert!(
            !webhooks
                .record_event(&scope(1), event(1, 9), vec![], None)
                .await?
        );
        assert!(webhooks.get_event(&scope(1), "evt_9").await?.is_none());
        Ok(())
    })
    .await;
}

/// F38: two concurrent deletions of one run both return its tombstone, as
/// the memory store does; the one that waited does not read NotFound.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn concurrent_deletes_both_return_the_tombstone() {
    use runbook_core::{ExecutionStore, retention::RunStore};
    use runbook_model::ExecutionStatus;
    use runbook_store_conformance::StoreFactory;
    use runbook_store_conformance::catalog::{batch, id};
    with_factory(async |f| {
        let s = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        s.commit(batch(1, 0, 1, ExecutionStatus::Succeeded)).await?;
        let (first, second) = (f.fresh_store(scope(1)).await, f.fresh_store(scope(1)).await);
        // The second passes its owner check, then waits while the first
        // deletes and commits.
        second.pause_after_owner_check();
        let run = id(1);
        let waiting = second.delete_execution(&run, "retention");
        let racing = async {
            let mut paused = false;
            for _ in 0..1_000 {
                if second.owner_check_paused() {
                    paused = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            let won = first.delete_execution(&id(1), "retention").await;
            second.release_owner_check();
            assert!(paused, "the second deletion never reached its pause");
            won
        };
        let (lost, won) = tokio::join!(waiting, racing);
        let won = won?;
        assert_eq!(lost, Ok(won.clone()));
        assert_eq!(s.tombstone(&id(1)).await?, Some(won));
        Ok(())
    })
    .await;
}

// ---------------------------------------------------------------------------
// STO1: Object storage tests
// ---------------------------------------------------------------------------

use object_store::memory::InMemory;
use runbook_core::{
    ExecutionStore,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord, TenantScope},
};
use runbook_store_conformance::StoreFactory;
use runbook_store_objects::{
    ObjectStorage, ObjectStoreExt, sweep_orphans, sweep_orphans_with_hook,
};
use runbook_store_postgres::PostgresExecutionStore;
use std::sync::Arc;

struct PostgresObjectsFactory<'a>(&'a support::factory::PostgresFactory, Arc<ObjectStorage>);
impl<'a> runbook_store_conformance::BlobFactory for PostgresObjectsFactory<'a> {
    type Blobs = PostgresExecutionStore;
    async fn blobs(
        &self,
        owner: runbook_core::ExecutionProjection,
    ) -> Result<Self::Blobs, StoreError> {
        let store = self.0.create(owner).await?;
        Ok(store.with_objects(Some(self.1.clone())))
    }
    async fn foreign_blobs(
        &self,
        blobs: &Self::Blobs,
        tenant: TenantScope,
    ) -> Result<Self::Blobs, StoreError> {
        Ok(blobs.scoped(tenant))
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn blob_conformance_suite_with_object_store() {
    with_factory(async |f| {
        let mem = Arc::new(InMemory::new());
        let storage = Arc::new(ObjectStorage::new(mem, "runbook/"));
        let of = PostgresObjectsFactory(f, storage);
        runbook_store_conformance::blob::blob_roundtrip_preserves_chunk_boundaries(&of).await?;
        runbook_store_conformance::blob::shared_blobs_are_separate_and_immutable(&of).await?;
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sto1_blob_operations_and_immutability() {
    with_factory(async |f| {
        let mem = Arc::new(InMemory::new());
        let storage = Arc::new(ObjectStorage::new(mem.clone(), "runbook/"));
        let store = f
            .fresh_store(scope(1))
            .await
            .with_objects(Some(storage.clone()));

        // Create an execution to own the blobs
        let s = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        s.commit(runbook_store_conformance::catalog::batch(
            1,
            0,
            1,
            runbook_model::ExecutionStatus::Running,
        ))
        .await?;

        let key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(1),
            name: "test_blob.txt".into(),
        };
        let bytes = b"hello, object storage world!".to_vec();

        // 1. Put
        store.put(key.clone(), &bytes).await?;

        // Verify it was written to object store
        let hash = runbook_core::portable::sha256(&bytes);
        let path = storage.path(&scope(1), &key, &hash);
        use object_store::ObjectStoreExt;
        assert!(storage.store.get(&path).await.is_ok());

        // 2. Head
        let head = store.head(&key).await?;
        assert_eq!(head.size, bytes.len() as u64);

        // 3. Get
        let read = store.get(&key).await?;
        assert_eq!(read, bytes);

        // 4. Get range
        let range_read = store.get_range(&key, 0..5).await?;
        assert_eq!(range_read, b"hello");

        let empty_range = store.get_range(&key, 5..5).await?;
        assert!(empty_range.is_empty());

        #[allow(clippy::reversed_empty_ranges)]
        let invalid_range = store.get_range(&key, 5..2).await;
        assert!(matches!(
            invalid_range,
            Err(StoreError::InvalidBatch("blob.invalid_range"))
        ));

        let out_of_bounds = store.get_range(&key, 0..1000).await;
        assert!(matches!(
            out_of_bounds,
            Err(StoreError::InvalidBatch("blob.invalid_range"))
        ));

        // 5. Immutability: putting same bytes succeeds (idempotent)
        store.put(key.clone(), &bytes).await?;

        // Putting different bytes refuses and leaves no extra object
        let different_bytes = b"different content entirely!".to_vec();
        let different_hash = runbook_core::portable::sha256(&different_bytes);
        let different_path = storage.path(&scope(1), &key, &different_hash);

        let err = store.put(key.clone(), &different_bytes).await;
        assert!(matches!(
            err,
            Err(StoreError::InvalidBatch("blob.immutable_key"))
        ));

        // Verify the rejected object was cleaned up
        assert!(storage.store.get(&different_path).await.is_err());

        // Original object still exists and readable
        assert_eq!(store.get(&key).await?, bytes);

        // 6. Delete
        store.delete(&key).await?;
        assert!(matches!(store.get(&key).await, Err(StoreError::NotFound)));
        assert!(matches!(store.head(&key).await, Err(StoreError::NotFound)));
        // Object in store is also deleted
        assert!(storage.store.get(&path).await.is_err());

        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sto1_archive_digest_mismatch_and_foreign_tenant() {
    with_factory(async |f| {
        let mem = Arc::new(InMemory::new());
        let storage = Arc::new(ObjectStorage::new(mem.clone(), "runbook/"));
        let store = f
            .fresh_store(scope(1))
            .await
            .with_objects(Some(storage.clone()));

        // Create runbook
        let runbook_id: runbook_model::RunbookId = "rb_01234567890123456789012345".parse().unwrap();
        store
            .create_runbook(
                RunbookRecord {
                    runbook_id: runbook_id.clone(),
                    slug: "title".into(),
                    origin: RunbookOrigin::Ui,
                    archived: false,
                },
                "usr_test".into(),
            )
            .await?;

        let bytes = b"archive zip bytes".to_vec();
        let real_digest = runbook_core::portable::sha256(&bytes);
        let mut wrong_digest = real_digest;
        wrong_digest[0] ^= 0xff;

        let mismatch_key = BlobKey::Archive {
            runbook_id: runbook_id.clone(),
            day: 19000,
            digest: wrong_digest,
        };

        // Digest mismatch fails before writing object
        let err = store.put(mismatch_key.clone(), &bytes).await;
        assert!(matches!(
            err,
            Err(StoreError::InvalidBatch("blob.digest_mismatch"))
        ));

        let wrong_path = storage.path(&scope(1), &mismatch_key, &real_digest);
        use object_store::ObjectStoreExt;
        assert!(storage.store.get(&wrong_path).await.is_err());

        // Correct digest succeeds
        let correct_key = BlobKey::Archive {
            runbook_id: runbook_id.clone(),
            day: 19000,
            digest: real_digest,
        };
        store.put(correct_key.clone(), &bytes).await?;
        assert_eq!(store.get(&correct_key).await?, bytes);

        // Foreign tenant getting NotFound
        let foreign_store = f
            .fresh_store(scope(2))
            .await
            .with_objects(Some(storage.clone()));
        assert!(matches!(
            foreign_store.get(&correct_key).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            foreign_store.head(&correct_key).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            foreign_store.put(correct_key.clone(), &bytes).await,
            Err(StoreError::NotFound)
        ));

        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sto1_mixed_reads() {
    with_factory(async |f| {
        let mem = Arc::new(InMemory::new());
        let storage = Arc::new(ObjectStorage::new(mem.clone(), "runbook/"));

        // Store initially without object storage (Postgres chunks mode)
        let pg_store = f.fresh_store(scope(1)).await;

        let s = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        s.commit(runbook_store_conformance::catalog::batch(
            1,
            0,
            1,
            runbook_model::ExecutionStatus::Running,
        ))
        .await?;

        let old_key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(1),
            name: "old_pg_blob.txt".into(),
        };
        let old_bytes = b"chunked postgres blob data".to_vec();
        pg_store.put(old_key.clone(), &old_bytes).await?;

        // Switch to object storage
        let obj_store = pg_store.with_objects(Some(storage.clone()));

        let new_key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(1),
            name: "new_obj_blob.txt".into(),
        };
        let new_bytes = b"new object store blob data".to_vec();
        obj_store.put(new_key.clone(), &new_bytes).await?;

        // Read old blob through obj_store (mixed read: postgres row)
        assert_eq!(obj_store.get(&old_key).await?, old_bytes);
        assert_eq!(obj_store.get_range(&old_key, 0..7).await?, b"chunked");
        assert_eq!(obj_store.head(&old_key).await?.size, old_bytes.len() as u64);

        // Read new blob through obj_store (object row)
        assert_eq!(obj_store.get(&new_key).await?, new_bytes);
        assert_eq!(obj_store.get_range(&new_key, 0..3).await?, b"new");
        assert_eq!(obj_store.head(&new_key).await?.size, new_bytes.len() as u64);

        // Immutability on old blob through obj_store: same bytes ok, different refused
        obj_store.put(old_key.clone(), &old_bytes).await?;
        assert!(matches!(
            obj_store.put(old_key.clone(), b"tampered").await,
            Err(StoreError::InvalidBatch("blob.immutable_key"))
        ));

        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sto1_cascade_delete_and_crash_orphan_sweeper() {
    with_factory(async |f| {
        let mem = Arc::new(InMemory::new());
        let storage = Arc::new(ObjectStorage::new(mem.clone(), "runbook/"));
        let store = f
            .fresh_store(scope(1))
            .await
            .with_objects(Some(storage.clone()));

        let s = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        s.commit(runbook_store_conformance::catalog::batch(
            1,
            0,
            1,
            runbook_model::ExecutionStatus::Succeeded,
        ))
        .await?;

        let key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(1),
            name: "cascade_blob.txt".into(),
        };
        let bytes = b"cascading bytes".to_vec();
        store.put(key.clone(), &bytes).await?;

        let hash = runbook_core::portable::sha256(&bytes);
        let path = storage.path(&scope(1), &key, &hash);
        use object_store::ObjectStoreExt;
        assert!(storage.store.get(&path).await.is_ok());

        // 1. Delete execution cascades to runbook.blob row
        let run_store = f.fresh_store(scope(1)).await;
        use runbook_core::retention::RunStore;
        run_store
            .delete_execution(&runbook_store_conformance::catalog::id(1), "test")
            .await?;

        // The row is gone
        assert!(matches!(store.get(&key).await, Err(StoreError::NotFound)));
        // But the object in the bucket is now an orphan
        assert!(storage.store.get(&path).await.is_ok());

        // 2. Add another crash orphan (object written, row never inserted)
        let orphan_key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(2),
            name: "crash_orphan.txt".into(),
        };
        let orphan_bytes = b"crash orphan bytes".to_vec();
        let orphan_hash = runbook_core::portable::sha256(&orphan_bytes);
        let orphan_path = storage.path(&scope(1), &orphan_key, &orphan_hash);
        storage
            .store
            .put(&orphan_path, orphan_bytes.into())
            .await
            .unwrap();
        assert!(storage.store.get(&orphan_path).await.is_ok());

        // 3. Run sweeper with min_age 0
        let swept = sweep_orphans(f.admin.client(), &storage, std::time::Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(swept, 2);

        // Both orphans deleted
        assert!(storage.store.get(&path).await.is_err());
        assert!(storage.store.get(&orphan_path).await.is_err());

        Ok(())
    })
    .await;
}

/// S3 / MinIO test, ignored by default unless RUNBOOK_TEST_S3_ENDPOINT is set.
#[tokio::test]
#[ignore = "requires RUNBOOK_TEST_S3_ENDPOINT, e.g. MinIO"]
async fn sto1_s3_minio_integration() {
    let Some(endpoint) = std::env::var("RUNBOOK_TEST_S3_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    with_factory(async |f| {
        let config = runbook_store_objects::BlobStoreConfig {
            kind: runbook_store_objects::BlobStoreKind::S3,
            bucket: Some("runbook-test".to_owned()),
            prefix: "test/".to_owned(),
            s3_endpoint: Some(endpoint),
            s3_region: Some("us-east-1".to_owned()),
            s3_force_path_style: true,
            sweep_interval: std::time::Duration::from_secs(3600),
            sweep_age: std::time::Duration::from_secs(3600),
        };
        let store_handle = config.build_store().unwrap().unwrap();
        let storage = Arc::new(ObjectStorage::new(store_handle, config.prefix));
        let store = f
            .fresh_store(scope(1))
            .await
            .with_objects(Some(storage.clone()));

        let s = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        s.commit(runbook_store_conformance::catalog::batch(
            1,
            0,
            1,
            runbook_model::ExecutionStatus::Running,
        ))
        .await?;

        let key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(1),
            name: "minio_blob.txt".into(),
        };
        let bytes = b"minio content".to_vec();
        store.put(key.clone(), &bytes).await?;
        assert_eq!(store.get(&key).await?, bytes);
        store.delete(&key).await?;
        assert!(matches!(store.get(&key).await, Err(StoreError::NotFound)));
        Ok(())
    })
    .await;
}

/// STO1: put never deletes the object on a database error.
/// A committed row pointing at path must not have its object deleted by a failed retry.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sto1_put_failure_does_not_delete_live_object() {
    with_factory(async |f| {
        let mem = Arc::new(InMemory::new());
        let storage = Arc::new(ObjectStorage::new(mem.clone(), "runbook/"));
        let store = f
            .fresh_store(scope(1))
            .await
            .with_objects(Some(storage.clone()));

        let s = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        s.commit(runbook_store_conformance::catalog::batch(
            1,
            0,
            1,
            runbook_model::ExecutionStatus::Succeeded,
        ))
        .await?;

        let key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(1),
            name: "live_blob.txt".into(),
        };
        let bytes = b"live blob content".to_vec();

        // 1. First put commits row with hash H and object in storage
        store.put(key.clone(), &bytes).await?;
        let hash = runbook_core::portable::sha256(&bytes);
        let path = storage.path(&scope(1), &key, &hash);
        assert!(storage.store.get(&path).await.is_ok());
        assert_eq!(store.get(&key).await?, bytes);

        // 2. Second put of the same bytes whose transaction fails
        store.inject_blob_put_faults(1);
        let err = store.put(key.clone(), &bytes).await;
        assert!(err.is_err());

        // 3. The object must still exist and get must still return the bytes
        assert!(storage.store.get(&path).await.is_ok());
        assert_eq!(store.get(&key).await?, bytes);

        Ok(())
    })
    .await;
}

/// STO1: sweeper race: an orphan older than min_age that gets re-put
/// (fresh last_modified and a row) between the listing and delete must not be deleted.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn sto1_sweeper_race_does_not_delete_reput_object() {
    with_factory(async |f| {
        let mem = Arc::new(InMemory::new());
        let storage = Arc::new(ObjectStorage::new(mem.clone(), "runbook/"));
        let store = f
            .fresh_store(scope(1))
            .await
            .with_objects(Some(storage.clone()));

        let s = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        s.commit(runbook_store_conformance::catalog::batch(
            1,
            0,
            1,
            runbook_model::ExecutionStatus::Succeeded,
        ))
        .await?;

        let key = BlobKey::Execution {
            execution_id: runbook_store_conformance::catalog::id(1),
            name: "reput_blob.txt".into(),
        };
        let bytes = b"reput blob content".to_vec();
        let hash = runbook_core::portable::sha256(&bytes);
        let path = storage.path(&scope(1), &key, &hash);

        // 1. Create an orphan object in storage (no row in DB yet)
        storage
            .store
            .put(&path, bytes.clone().into())
            .await
            .unwrap();
        assert!(storage.store.get(&path).await.is_ok());

        // Let it age past min_age
        let min_age = std::time::Duration::from_millis(50);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        // 2. Run sweeper with a hook that re-puts the object and commits its row
        // between the listing and the delete.
        let store_clone = store.clone();
        let key_clone = key.clone();
        let bytes_clone = bytes.clone();
        let swept = sweep_orphans_with_hook(f.admin.client(), &storage, min_age, |_deleted_path| {
            let store_clone = store_clone.clone();
            let key_clone = key_clone.clone();
            let bytes_clone = bytes_clone.clone();
            async move {
                store_clone.put(key_clone, &bytes_clone).await.unwrap();
            }
        })
        .await
        .unwrap();

        // 3. Sweeper should not have deleted the object because it was re-put!
        assert_eq!(swept, 0);
        assert!(storage.store.get(&path).await.is_ok());
        assert_eq!(store.get(&key).await?, bytes);

        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn copy_blobs_moves_postgres_blobs_to_object_storage() {
    with_factory(async |f| {
        let store = f.fresh_store(scope(1)).await;
        let bytes = b"copy me to object storage".to_vec();
        let key = BlobKey::Shared {
            hash: Sha256::digest(&bytes).into(),
        };
        store.put(key.clone(), &bytes).await?;
        let storage = ObjectStorage::new(Arc::new(InMemory::new()), "runbook/");
        let copier = runbook_store_postgres::BlobCopier::new(f.config.clone());
        let stats = copier.copy_all(&storage, 10).await?;
        assert_eq!(stats.copied, 1);
        assert_eq!(stats.bytes, bytes.len() as u64);
        // The bytes are at the same path a fresh object-backed put uses,
        // and the chunks are gone.
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        let got = storage
            .store
            .get(&storage.path(&scope(1), &key, &hash))
            .await
            .expect("copied object exists");
        assert_eq!(got.bytes().await.unwrap().as_ref(), bytes.as_slice());
        let counts = f.row_counts().await;
        assert_eq!(counts["blob"], 1);
        assert_eq!(counts["blob_chunk"], 0);
        // A second run copies nothing.
        let again = copier.copy_all(&storage, 10).await?;
        assert_eq!(again.copied, 0);
        // And the blob still reads through an object-backed store.
        let reader = f
            .fresh_store(scope(1))
            .await
            .with_objects(Some(Arc::new(storage)));
        assert_eq!(reader.get(&key).await?, bytes);
        Ok(())
    })
    .await;
}
