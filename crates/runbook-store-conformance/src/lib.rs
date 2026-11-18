//! Adapter-independent cases; native and Workers adapters run the same assertions.
use runbook_core::*;
use std::{future::Future, time::Duration};
pub mod blob;
pub mod catalog;
pub mod clock;
pub mod cross_scoped;
pub mod d1_scoped;
pub mod directory;
pub mod eligibility;
pub mod execution;
pub mod execution_scoped;
pub mod language;
pub mod listing;
pub mod notify;
pub mod parity;
pub mod portable;
pub mod queue;
pub mod quotas;
pub mod retention;
pub mod run_ceiling;
pub mod search;
pub mod webhooks;
// T0 contract freeze: cases registered red (see `pending`).
pub mod archives;
pub mod auth_links;
pub mod device_codes;
pub mod memory_entries;
pub mod pending;
pub mod uploads;
/// Bounds grow by tier so an adapter can run each suite as its traits land.
pub trait StoreFactory {
    type Store: ExecutionStore + Clone;
    fn create(
        &self,
        initial: ExecutionProjection,
    ) -> impl Future<Output = Result<Self::Store, StoreError>>;
    fn fail_after(
        &self,
        store: &Self::Store,
        stage: WriteStage,
    ) -> impl Future<Output = Result<(), StoreError>>;
    fn derived_row_delay(&self) -> Duration;
}
pub struct MemoryFactory;
impl StoreFactory for MemoryFactory {
    type Store = memory::MemoryExecutionStore;
    async fn create(&self, initial: ExecutionProjection) -> Result<Self::Store, StoreError> {
        Ok(memory::MemoryExecutionStore::new(scope(1), initial, 10000))
    }
    async fn fail_after(&self, store: &Self::Store, stage: WriteStage) -> Result<(), StoreError> {
        store.fail_after(stage);
        Ok(())
    }
    fn derived_row_delay(&self) -> Duration {
        Duration::ZERO
    }
}

/// NOTIFY1 tier: the email outbox and preferences.
pub trait NotifyFactory {
    type Store: runbook_core::notify::NotifyStore + Clone;
    fn create_notify(&self) -> impl Future<Output = Result<Self::Store, StoreError>>;
    fn set_notify_time(&self, store: &Self::Store, now_ms: i64);
}
impl NotifyFactory for MemoryFactory {
    type Store = runbook_core::notify::memory::MemoryNotify;
    async fn create_notify(&self) -> Result<Self::Store, StoreError> {
        Ok(runbook_core::notify::memory::MemoryNotify::new(0))
    }
    fn set_notify_time(&self, store: &Self::Store, now_ms: i64) {
        store.set_time(now_ms);
    }
}

/// Directory tier (B2.2b): independent of the execution store tiers, so an
/// adapter can land its DirectoryStore without the queue/blob traits.
pub trait DirectoryFactory {
    type Store: runbook_core::directory::DirectoryStore + Clone;
    fn create_directory(
        &self,
    ) -> impl Future<Output = Result<Self::Store, runbook_core::directory::DirectoryError>>;
}
impl DirectoryFactory for MemoryFactory {
    type Store = runbook_core::directory::memory::MemoryDirectory;
    async fn create_directory(
        &self,
    ) -> Result<Self::Store, runbook_core::directory::DirectoryError> {
        Ok(runbook_core::directory::memory::MemoryDirectory::default())
    }
}

pub fn scope(n: u8) -> runbook_core::catalog::TenantScope {
    runbook_core::catalog::TenantScope {
        org_id: format!("org_{n:026}").parse().unwrap(),
        project_id: format!("prj_{n:026}").parse().unwrap(),
    }
}
/// Session tier (CF2): what one execution's own store can serve, with no
/// catalog. The clock and queue cases need only this, so an ExecutionDO
/// store (`runbook-store-do`) runs them without D1.
pub trait SessionFactory: StoreFactory<Store: runbook_core::session::SessionStore> {
    /// Independently query backend time (Postgres clock_timestamp), never a host clock.
    fn backend_time_ms(&self, store: &Self::Store)
    -> impl Future<Output = Result<i64, StoreError>>;
    fn scoped_client(
        &self,
        store: &Self::Store,
        scope: runbook_core::catalog::TenantScope,
    ) -> impl Future<Output = Result<Self::Store, StoreError>>;
    /// Let the store clock move on by at least `ms` (a test clock moves it;
    /// a real one waits), as awaiting D1 or R2 inside a session does.
    fn advance_clock(
        &self,
        store: &Self::Store,
        ms: u64,
    ) -> impl Future<Output = Result<(), StoreError>>;
}
impl SessionFactory for MemoryFactory {
    async fn backend_time_ms(&self, store: &Self::Store) -> Result<i64, StoreError> {
        Ok(store.backend_time_ms())
    }
    async fn scoped_client(
        &self,
        store: &Self::Store,
        scope: runbook_core::catalog::TenantScope,
    ) -> Result<Self::Store, StoreError> {
        Ok(store.scoped(scope))
    }
    async fn advance_clock(&self, store: &Self::Store, ms: u64) -> Result<(), StoreError> {
        let ms = i64::try_from(ms).map_err(|_| StoreError::Backend("test.clock"))?;
        store.set_time(store.backend_time_ms() + ms);
        Ok(())
    }
}
pub trait CatalogFactory: SessionFactory<Store: runbook_core::catalog::CatalogStore> {
    fn redeliver_derived(
        &self,
        store: &Self::Store,
        id: &runbook_model::ExecutionId,
        sequence: u64,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// Adapter must enforce this maximum wait; no unbounded polling or sleeping.
    fn await_derived(
        &self,
        store: &Self::Store,
        id: &runbook_model::ExecutionId,
        sequence: u64,
        max_delay: Duration,
    ) -> impl Future<Output = Result<(), StoreError>>;
}
impl CatalogFactory for MemoryFactory {
    async fn redeliver_derived(
        &self,
        store: &Self::Store,
        id: &runbook_model::ExecutionId,
        sequence: u64,
    ) -> Result<(), StoreError> {
        store.redeliver_derived(id, sequence)
    }
    async fn await_derived(
        &self,
        store: &Self::Store,
        id: &runbook_model::ExecutionId,
        sequence: u64,
        max_delay: Duration,
    ) -> Result<(), StoreError> {
        use runbook_core::catalog::CatalogStore;
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

/// Work-queue tier (B2.3): an adapter can run the queue cases before its
/// blob and run-slice traits land. QueueFactory remains the full B2.4 tier.
/// It needs only the session tier (CF2): an ExecutionDO's queue is its own.
pub trait WorkQueueFactory: SessionFactory<Store: runbook_core::work_queue::WorkQueue> {
    fn expire_lease(
        &self,
        store: &Self::Store,
        lease: &runbook_core::work_queue::WorkLease,
    ) -> impl Future<Output = Result<(), StoreError>>;
}
impl WorkQueueFactory for MemoryFactory {
    async fn expire_lease(
        &self,
        store: &Self::Store,
        lease: &runbook_core::work_queue::WorkLease,
    ) -> Result<(), StoreError> {
        store.set_time(lease.deadline_at_ms);
        Ok(())
    }
}

pub trait QueueFactory:
    CatalogFactory
    + WorkQueueFactory<
        Store: runbook_core::work_queue::WorkQueue
                   + runbook_core::blob::BlobStore
                   + runbook_core::retention::RunStore,
    >
{
    /// Test-only raw storage inventory, not filtered public reads. Include every owned table.
    fn owned_record_counts(
        &self,
        store: &Self::Store,
        id: &runbook_model::ExecutionId,
    ) -> impl Future<Output = Result<std::collections::BTreeMap<String, usize>, StoreError>>;
}
impl QueueFactory for MemoryFactory {
    async fn owned_record_counts(
        &self,
        store: &Self::Store,
        id: &runbook_model::ExecutionId,
    ) -> Result<std::collections::BTreeMap<String, usize>, StoreError> {
        Ok(store.owned_record_counts(id))
    }
}

/// Blob tier (CF4): a `BlobStore` on its own, with no execution store, so
/// an object-store adapter (R2) runs the blob cases. `blobs` returns a
/// fresh store whose tenant (scope 1) owns `owner`'s execution;
/// `foreign_blobs` is the same storage seen by another tenant.
pub trait BlobFactory {
    type Blobs: runbook_core::blob::BlobStore + Clone;
    fn blobs(
        &self,
        owner: ExecutionProjection,
    ) -> impl Future<Output = Result<Self::Blobs, StoreError>>;
    fn foreign_blobs(
        &self,
        blobs: &Self::Blobs,
        scope: runbook_core::catalog::TenantScope,
    ) -> impl Future<Output = Result<Self::Blobs, StoreError>>;
}
impl BlobFactory for MemoryFactory {
    type Blobs = memory::MemoryExecutionStore;
    async fn blobs(&self, owner: ExecutionProjection) -> Result<Self::Blobs, StoreError> {
        self.create(owner).await
    }
    async fn foreign_blobs(
        &self,
        blobs: &Self::Blobs,
        scope: runbook_core::catalog::TenantScope,
    ) -> Result<Self::Blobs, StoreError> {
        Ok(blobs.scoped(scope))
    }
}

pub mod credentials;
pub trait CredentialFactory: DirectoryFactory {
    type Credentials: runbook_core::credentials::CredentialStore + Clone;
    fn create_credentials(
        &self,
    ) -> impl Future<Output = Result<Self::Credentials, runbook_core::credentials::CredentialError>>;
}
impl CredentialFactory for MemoryFactory {
    type Credentials = runbook_core::credentials::memory::MemoryCredentials;
    async fn create_credentials(
        &self,
    ) -> Result<Self::Credentials, runbook_core::credentials::CredentialError> {
        Ok(Default::default())
    }
}

impl quotas::QuotaFactory for MemoryFactory {
    async fn with_quota(
        &self,
        store: &Self::Store,
        limits: runbook_core::quotas::QuotaLimits,
    ) -> Result<Self::Store, StoreError> {
        Ok(store.with_quota(limits))
    }
    async fn next_month(&self, store: &Self::Store) -> Result<Self::Store, StoreError> {
        store.set_time(store.backend_time_ms() + 32 * 86_400_000);
        Ok(store.clone())
    }
}
