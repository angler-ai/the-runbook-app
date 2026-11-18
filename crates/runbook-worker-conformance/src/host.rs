//! What a test host gives the cases: fresh object storage, a test clock and
//! a way to read the object's alarm. Natively that is an in-memory SQLite
//! database (`native.rs`); in workerd it is the case's own Durable Object
//! over `SqlStorage` (`edge.rs`). The shared conformance cases run through
//! [`HostFactory`], the DO-specific ones (`cases.rs`) through [`Host`].
use runbook_core::{ExecutionProjection, StoreError, WriteStage, catalog::TenantScope};
use runbook_store_conformance::{SessionFactory, StoreFactory, WorkQueueFactory, scope};
use runbook_store_do::{DoSql, DoStore};
use std::{future::Future, time::Duration};

pub trait Host {
    type Sql: DoSql;
    /// Storage for a fresh object. A store opened on an earlier `fresh` of
    /// the same host must not be used again (workerd fails it loudly).
    fn fresh(&self) -> Result<Self::Sql, StoreError>;
    /// The base time DO-specific cases start their clock at. In workerd it
    /// is in the future, so the alarms the cases set never fire mid-case.
    fn start_ms(&self) -> i64;
    /// Pin the store clock of `sql` (a test seam; production never does).
    fn set_time(&self, sql: &Self::Sql, now_ms: i64);
    /// The object's alarm, as the host reads it.
    fn alarm(&self, sql: &Self::Sql) -> impl Future<Output = Option<i64>>;
    /// The blob and user-content buckets, emptied (CF4).
    type Bucket: runbook_store_r2::ObjectBucket;
    fn buckets(&self) -> impl Future<Output = Result<(Self::Bucket, Self::Bucket), StoreError>>;
    /// The D1 database, migrated and emptied (CF3).
    type D1: runbook_store_d1::D1;
    fn d1(&self) -> impl Future<Output = Result<Self::D1, StoreError>>;
    /// CF11: the webhook subscriptions' objects a D1 case's store reaches.
    type Webhooks: runbook_worker::webhooks::WebhookObjects;
    fn webhooks(&self) -> Self::Webhooks;
}

/// The D1 tier (CF3) over one case's database: every store a D1 case asks
/// for shares it, so a credential fixture sees its directory.
pub struct D1Factory<D: runbook_store_d1::D1>(pub D);
impl<D: runbook_store_d1::D1> runbook_store_conformance::DirectoryFactory for D1Factory<D> {
    type Store = runbook_store_d1::D1Directory<D>;
    async fn create_directory(
        &self,
    ) -> Result<Self::Store, runbook_core::directory::DirectoryError> {
        Ok(runbook_store_d1::D1Directory::new(self.0.clone()))
    }
}
impl<D: runbook_store_d1::D1> runbook_store_conformance::NotifyFactory for D1Factory<D> {
    type Store = runbook_store_d1::D1Notify<D>;
    async fn create_notify(&self) -> Result<Self::Store, runbook_core::StoreError> {
        Ok(runbook_store_d1::D1Notify::new(self.0.clone()))
    }
    fn set_notify_time(&self, _store: &Self::Store, _now_ms: i64) {
        // D1 reads the host clock; the cases are clock-relative.
    }
}
impl<D: runbook_store_d1::D1> runbook_store_conformance::CredentialFactory for D1Factory<D> {
    type Credentials = runbook_store_d1::D1Credentials<D>;
    async fn create_credentials(
        &self,
    ) -> Result<Self::Credentials, runbook_core::credentials::CredentialError> {
        Ok(runbook_store_d1::D1Credentials::new(self.0.clone()))
    }
}

/// A fresh, migrated, empty D1 database's schema check and reset.
pub async fn fresh_d1<D: runbook_store_d1::D1>(db: &D) -> Result<(), StoreError> {
    runbook_store_d1::migrate::migrate(db)
        .await
        .map_err(|_| StoreError::Backend("test.d1_migrate"))?;
    runbook_store_d1::testing::reset(db)
        .await
        .map_err(|_| StoreError::Backend("test.d1_reset"))
}

/// What a case returned, as one outcome: the D1 cases return `()` (they
/// assert), a store error, or a directory or credential error.
pub trait CaseOutcome {
    fn outcome(self) -> Result<(), String>;
}
impl CaseOutcome for () {
    fn outcome(self) -> Result<(), String> {
        Ok(())
    }
}
impl<E: std::fmt::Debug> CaseOutcome for Result<(), E> {
    fn outcome(self) -> Result<(), String> {
        self.map_err(|error| format!("{error:?}"))
    }
}

/// The blob tier over any host's buckets: each `blobs` empties them and
/// gives scope 1 the owner's execution.
pub struct R2Factory<'a, H: Host>(pub &'a H);
impl<H: Host> runbook_store_conformance::BlobFactory for R2Factory<'_, H> {
    type Blobs = runbook_store_r2::R2BlobStore<H::Bucket, runbook_store_r2::MemoryOwners>;
    async fn blobs(&self, owner: ExecutionProjection) -> Result<Self::Blobs, StoreError> {
        let (blobs, user_content) = self.0.buckets().await?;
        let owners = runbook_store_r2::MemoryOwners::default();
        owners.own_execution(owner.execution_id, scope(1));
        Ok(runbook_store_r2::R2BlobStore::new(
            blobs,
            user_content,
            owners,
            scope(1),
        ))
    }
    async fn foreign_blobs(
        &self,
        blobs: &Self::Blobs,
        tenant: TenantScope,
    ) -> Result<Self::Blobs, StoreError> {
        Ok(blobs.scoped(tenant))
    }
}

/// The shared conformance tiers over any host: one fresh object per create.
pub struct HostFactory<'a, H: Host>(pub &'a H);

impl<H: Host> StoreFactory for HostFactory<'_, H> {
    type Store = DoStore<H::Sql>;
    async fn create(&self, initial: ExecutionProjection) -> Result<Self::Store, StoreError> {
        let store = DoStore::open(self.0.fresh()?, &initial.execution_id, scope(1))?;
        store.create_execution(initial)?;
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
impl<H: Host> SessionFactory for HostFactory<'_, H> {
    async fn backend_time_ms(&self, store: &Self::Store) -> Result<i64, StoreError> {
        Ok(store.now_ms())
    }
    async fn scoped_client(
        &self,
        store: &Self::Store,
        tenant: TenantScope,
    ) -> Result<Self::Store, StoreError> {
        Ok(store.scoped(tenant))
    }
    async fn advance_clock(&self, store: &Self::Store, ms: u64) -> Result<(), StoreError> {
        let ms = i64::try_from(ms).map_err(|_| StoreError::Backend("test.clock"))?;
        self.0.set_time(store.backend(), store.now_ms() + ms);
        Ok(())
    }
}
impl<H: Host> WorkQueueFactory for HostFactory<'_, H> {
    async fn expire_lease(
        &self,
        store: &Self::Store,
        lease: &runbook_core::work_queue::WorkLease,
    ) -> Result<(), StoreError> {
        store.expire_lease(lease)
    }
}
