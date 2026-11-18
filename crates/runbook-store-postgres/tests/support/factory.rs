#![allow(dead_code)]
//! Shared Postgres conformance factory: one migrated test database per case.
use super::database::TestDatabase;
use futures_util::FutureExt;
use runbook_core::{
    ExecutionProjection, StoreError, WriteStage,
    catalog::{CatalogStore, TenantScope},
};
use runbook_model::ExecutionId;
use runbook_store_conformance::{
    CatalogFactory, DirectoryFactory, NotifyFactory, SessionFactory, StoreFactory, scope,
};
use runbook_store_postgres::{
    DatabaseConfig, PostgresDirectory, PostgresExecutionStore, PostgresStore,
};
use std::{collections::BTreeMap, panic::AssertUnwindSafe, time::Duration};

const TABLES: &[&str] = &[
    "execution_owner",
    "execution",
    "execution_event",
    "execution_timer",
    "execution_timer_history",
    "execution_outbox",
    "execution_idempotency",
    "actor_replay",
    "step_run_index",
    "execution_claim",
    "execution_derived_outbox",
    "draft",
    "run_summary",
    "assignment",
    "admission_pool",
    "admission",
    "start_idempotency",
    "work_lease",
    "blob",
    "blob_chunk",
    "tombstone",
    "archive",
    "archive_run",
];

pub struct PostgresFactory {
    pub config: DatabaseConfig,
    pub admin: PostgresStore,
    pub database: String,
}
impl PostgresFactory {
    pub async fn new(db: &TestDatabase) -> Self {
        let mut admin = PostgresStore::connect(db.config()).await.unwrap();
        admin.migrate().await.unwrap();
        Self {
            config: db.config().clone(),
            admin,
            database: db.name().to_owned(),
        }
    }
    /// Raw row counts across every adapter table, bypassing tenant filters.
    pub async fn row_counts(&self) -> BTreeMap<&'static str, i64> {
        let mut counts = BTreeMap::new();
        for table in TABLES {
            let row = self
                .admin
                .client()
                .query_one(&format!("SELECT count(*) FROM runbook.{table}"), &[])
                .await
                .unwrap();
            counts.insert(*table, row.get(0));
        }
        counts
    }
    pub async fn fresh_store(&self, tenant: TenantScope) -> PostgresExecutionStore {
        PostgresExecutionStore::new(self.config.clone(), tenant)
    }
    /// Same database, independent pool: an observer that shares no connections.
    pub fn fresh_directory(&self) -> PostgresDirectory {
        PostgresDirectory::new(self.config.clone())
    }
}

const DIRECTORY_TABLES: &[&str] = &[
    "organization",
    "directory_revision",
    "user_identity",
    "org_user",
    "project",
    "team",
    "project_member",
    "team_member",
    "verified_email",
];
impl DirectoryFactory for PostgresFactory {
    type Store = PostgresDirectory;
    async fn create_directory(
        &self,
    ) -> Result<Self::Store, runbook_core::directory::DirectoryError> {
        let tables: Vec<String> = DIRECTORY_TABLES
            .iter()
            .map(|t| format!("runbook.{t}"))
            .collect();
        self.admin
            .client()
            .batch_execute(&format!("TRUNCATE {} CASCADE", tables.join(", ")))
            .await
            .map_err(|_| {
                runbook_core::directory::DirectoryError::Backend("test.truncate_failed")
            })?;
        Ok(PostgresDirectory::new(self.config.clone()))
    }
}
impl NotifyFactory for PostgresFactory {
    type Store = runbook_store_postgres::PostgresNotify;
    async fn create_notify(&self) -> Result<Self::Store, StoreError> {
        self.admin
            .client()
            .batch_execute("TRUNCATE runbook.email_delivery, runbook.email_preference CASCADE")
            .await
            .map_err(|_| StoreError::Backend("test.truncate_failed"))?;
        Ok(runbook_store_postgres::PostgresNotify::new(
            self.config.clone(),
        ))
    }
    fn set_notify_time(&self, _store: &Self::Store, _now_ms: i64) {
        // Postgres reads its own clock; the cases are clock-relative.
    }
}
impl StoreFactory for PostgresFactory {
    type Store = PostgresExecutionStore;
    async fn create(&self, initial: ExecutionProjection) -> Result<Self::Store, StoreError> {
        let tables: Vec<String> = TABLES.iter().map(|t| format!("runbook.{t}")).collect();
        self.admin
            .client()
            .batch_execute(&format!(
                "TRUNCATE {} RESTART IDENTITY CASCADE; \
                 ALTER SEQUENCE runbook.admission_queue_order RESTART",
                tables.join(", ")
            ))
            .await
            .map_err(|_| StoreError::Backend("test.truncate_failed"))?;
        let store = PostgresExecutionStore::new(self.config.clone(), scope(1));
        store.create_execution(initial).await?;
        Ok(store)
    }
    async fn fail_after(&self, store: &Self::Store, stage: WriteStage) -> Result<(), StoreError> {
        store.fail_after(stage);
        Ok(())
    }
    fn derived_row_delay(&self) -> Duration {
        Duration::ZERO
    }
}
impl SessionFactory for PostgresFactory {
    async fn backend_time_ms(&self, store: &Self::Store) -> Result<i64, StoreError> {
        store.backend_time_ms().await
    }
    async fn scoped_client(
        &self,
        store: &Self::Store,
        tenant: TenantScope,
    ) -> Result<Self::Store, StoreError> {
        Ok(store.scoped(tenant))
    }
    /// Postgres has no test clock: wait for the real one.
    async fn advance_clock(&self, _store: &Self::Store, ms: u64) -> Result<(), StoreError> {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(())
    }
}
impl runbook_store_conformance::quotas::QuotaFactory for PostgresFactory {
    async fn with_quota(
        &self,
        store: &Self::Store,
        limits: runbook_core::quotas::QuotaLimits,
    ) -> Result<Self::Store, StoreError> {
        Ok(store.with_quota(limits))
    }
    async fn next_month(&self, store: &Self::Store) -> Result<Self::Store, StoreError> {
        Ok(store.with_quota_clock_offset(32 * 86_400_000))
    }
}
impl runbook_store_conformance::BlobFactory for PostgresFactory {
    type Blobs = PostgresExecutionStore;
    async fn blobs(
        &self,
        owner: runbook_core::ExecutionProjection,
    ) -> Result<Self::Blobs, StoreError> {
        self.create(owner).await
    }
    async fn foreign_blobs(
        &self,
        blobs: &Self::Blobs,
        tenant: TenantScope,
    ) -> Result<Self::Blobs, StoreError> {
        Ok(blobs.scoped(tenant))
    }
}
impl CatalogFactory for PostgresFactory {
    async fn redeliver_derived(
        &self,
        store: &Self::Store,
        id: &ExecutionId,
        sequence: u64,
    ) -> Result<(), StoreError> {
        store.redeliver_derived(id, sequence).await
    }
    /// Derived rows commit with the execution, so they are visible immediately.
    async fn await_derived(
        &self,
        store: &Self::Store,
        id: &ExecutionId,
        sequence: u64,
        max_delay: Duration,
    ) -> Result<(), StoreError> {
        assert_eq!(max_delay, Duration::ZERO);
        assert!(
            store
                .get_run(id)
                .await?
                .is_some_and(|r| r.sequence >= sequence)
        );
        Ok(())
    }
}

/// Run one case on its own database; clean up even when an assertion panics.
pub async fn with_factory<F>(case: F)
where
    F: AsyncFnOnce(&PostgresFactory) -> Result<(), StoreError>,
{
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let factory = PostgresFactory::new(&db).await;
        case(&factory).await.unwrap();
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

impl runbook_store_conformance::WorkQueueFactory for PostgresFactory {
    async fn expire_lease(
        &self,
        _store: &Self::Store,
        lease: &runbook_core::work_queue::WorkLease,
    ) -> Result<(), StoreError> {
        use runbook_core::work_queue::WorkKind;
        let (kind, key, work_generation): (&str, &str, i64) = match &lease.id.kind {
            WorkKind::Timer { key, generation } => (
                "timer",
                key.as_str(),
                i64::try_from(*generation)
                    .map_err(|_| StoreError::Backend("test.expire_failed"))?,
            ),
            WorkKind::Notification { event_id } => ("notification", event_id.as_str(), 0),
            WorkKind::Activation => ("activation", "", 0),
            WorkKind::ClaimLease {
                step_run_id,
                generation,
            } => (
                "claim",
                step_run_id.as_str(),
                i64::try_from(*generation)
                    .map_err(|_| StoreError::Backend("test.expire_failed"))?,
            ),
        };
        self.admin
            .client()
            .execute(
                "UPDATE runbook.work_lease SET deadline_at_ms = 0 \
                 WHERE org_id = $1 AND project_id = $2 AND execution_id = $3 \
                 AND kind = $4 AND work_key = $5 AND work_generation = $6",
                &[
                    &lease.scope.org_id.as_str(),
                    &lease.scope.project_id.as_str(),
                    &lease.id.execution_id.as_str(),
                    &kind,
                    &key,
                    &work_generation,
                ],
            )
            .await
            .map_err(|_| StoreError::Backend("test.expire_failed"))?;
        Ok(())
    }
}

impl runbook_store_conformance::QueueFactory for PostgresFactory {
    /// Raw per-table row counts for one execution across every run-owned
    /// table, bypassing tenant filters (test-only inventory).
    async fn owned_record_counts(
        &self,
        _store: &Self::Store,
        id: &runbook_model::ExecutionId,
    ) -> Result<BTreeMap<String, usize>, StoreError> {
        let admin = self.admin.client();
        let mut counts = BTreeMap::new();
        for (name, table) in [
            ("catalog_owner", "execution_owner"),
            ("run_summary", "run_summary"),
            ("assignment", "assignment"),
            ("admission", "admission"),
            ("execution_head", "execution"),
            ("execution_owner", "execution_owner"),
            ("event", "execution_event"),
            ("timer", "execution_timer"),
            ("timer_history", "execution_timer_history"),
            ("outbox", "execution_outbox"),
            ("derived_outbox", "execution_derived_outbox"),
            ("idempotency", "execution_idempotency"),
            ("claim", "execution_claim"),
            ("work_lease", "work_lease"),
            ("start_record", "execution_start"),
            ("credential_family", "credential_family"),
            ("execution_credential", "execution_credential"),
            ("claim_log", "claim_log"),
            ("start_replay", "start_replay"),
            ("tombstone", "tombstone"),
            ("evidence", "evidence"),
            ("archive_run", "archive_run"),
        ] {
            let row = admin
                .query_one(
                    &format!("SELECT count(*) FROM runbook.{table} WHERE execution_id = $1"),
                    &[&id.as_str()],
                )
                .await
                .map_err(|_| StoreError::Backend("test.inventory_failed"))?;
            let count: i64 = row.get(0);
            counts.insert(name.into(), usize::try_from(count).unwrap_or_default());
        }
        for (name, sql) in [
            (
                "start_idempotency",
                "SELECT count(*) FROM runbook.start_idempotency WHERE execution_id = $1",
            ),
            (
                "queue_entry",
                "SELECT count(*) FROM runbook.admission \
                 WHERE execution_id = $1 AND queue_order IS NOT NULL",
            ),
            (
                "step_projection",
                "SELECT COALESCE(jsonb_array_length((projection::jsonb) -> 'steps'), 0)::bigint \
                 FROM runbook.execution WHERE execution_id = $1",
            ),
            (
                "blob",
                "SELECT count(*) FROM runbook.blob WHERE execution_id = $1",
            ),
            (
                "webhook_event",
                "SELECT count(*) FROM runbook.webhook_event WHERE execution_id = $1",
            ),
            (
                "webhook_delivery",
                "SELECT count(*) FROM runbook.webhook_delivery d \
                 JOIN runbook.webhook_event e USING (org_id, project_id, event_id) \
                 WHERE e.execution_id = $1",
            ),
            (
                "webhook_attempt",
                "SELECT count(*) FROM runbook.webhook_attempt a \
                 JOIN runbook.webhook_delivery d USING (delivery_id) \
                 JOIN runbook.webhook_event e USING (org_id, project_id, event_id) \
                 WHERE e.execution_id = $1",
            ),
            (
                "blob_chunk",
                "SELECT count(*) FROM runbook.blob_chunk c \
                 JOIN runbook.blob b ON b.org_id = c.org_id \
                 AND b.project_id = c.project_id AND b.key = c.key \
                 WHERE b.execution_id = $1",
            ),
        ] {
            let count: i64 = admin
                .query_opt(sql, &[&id.as_str()])
                .await
                .map_err(|_| StoreError::Backend("test.inventory_failed"))?
                .map_or(0, |row| row.get(0));
            counts.insert(name.into(), usize::try_from(count).unwrap_or_default());
        }
        Ok(counts)
    }
}
