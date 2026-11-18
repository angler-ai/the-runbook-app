//! CF5: the cross-store tiers (catalog, queue, retention, uploads,
//! archives, language) over the composed Cloudflare store, with every run in
//! its own object reached through the object protocol.
//!
//! A case may touch many runs, and each run is one ExecutionDO. A Durable
//! Object can only use its own storage, so the case's store cannot open the
//! others' SQL; it reaches them the way the api Worker's cron does, with
//! [`RemoteObjects`] over a [`Transport`]:
//! - natively ([`InProcess`]): an in-memory SQLite database per run, each
//!   request encoded to JSON and back, so the native run checks the codec;
//! - in workerd (`edge.rs`): one real Durable Object per run, named for the
//!   case run and the execution, reached with a stub `fetch`.
//!
//! D1 and the buckets are the host's (rusqlite and memory natively,
//! Miniflare's D1 and R2 in workerd). One test clock drives the objects and
//! D1 (D1 has no clock of its own; the stores read the host's).
use crate::host::fresh_d1;
use runbook_core::{ExecutionProjection, StoreError, WriteStage, catalog::TenantScope};
use runbook_model::ExecutionId;
use runbook_store_conformance::{
    CatalogFactory, QueueFactory, SessionFactory, StoreFactory, WorkQueueFactory, scope,
};
use runbook_store_d1::{D1, D1Error, Statement, sql::Outcome};
use runbook_store_do::OUTBOX_FIRST_RETRY_MS;
use runbook_store_r2::ObjectBucket;
use runbook_worker::{
    compose::{CfStore, Objects},
    protocol::{RemoteObjects, Transport},
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    future::Future,
    rc::Rc,
    time::Duration,
};

/// A D1 whose clock is the case's test clock.
#[derive(Clone)]
pub struct ClockD1<D: D1> {
    pub inner: D,
    clock: Rc<Cell<i64>>,
}
impl<D: D1> D1 for ClockD1<D> {
    async fn all(&self, statement: Statement) -> Result<Vec<runbook_store_d1::sql::Row>, D1Error> {
        self.inner.all(statement).await
    }
    async fn run(&self, statement: Statement) -> Result<u64, D1Error> {
        self.inner.run(statement).await
    }
    async fn batch(&self, statements: Vec<Statement>) -> Result<Vec<Outcome>, D1Error> {
        self.inner.batch(statements).await
    }
    fn now_ms(&self) -> i64 {
        self.clock.get()
    }
}

/// What a host gives the cross-store tiers.
pub trait CrossHost {
    type Transport: Transport;
    type D1: D1;
    type Bucket: ObjectBucket;
    /// Objects for a fresh namespace (one per `create`), all reading
    /// `clock`.
    fn transport(&self, clock: Rc<Cell<i64>>) -> Self::Transport;
    /// D1, migrated and emptied.
    fn d1(&self) -> impl Future<Output = Result<Self::D1, StoreError>>;
    /// The two buckets, emptied.
    fn buckets(&self) -> impl Future<Output = Result<(Self::Bucket, Self::Bucket), StoreError>>;
    /// Where the test clock starts.
    fn start_ms(&self) -> i64;
}

pub type CrossStore<H> = CfStore<
    ClockD1<<H as CrossHost>::D1>,
    RemoteObjects<<H as CrossHost>::Transport>,
    <H as CrossHost>::Bucket,
>;

/// The cross-store factory. Each `create` is a fresh D1, fresh buckets and
/// a fresh object namespace; the case's other runs live in the same one.
pub struct CrossFactory<H: CrossHost> {
    host: H,
    clock: Rc<Cell<i64>>,
    d1: RefCell<Option<ClockD1<H::D1>>>,
    buckets: RefCell<Option<(H::Bucket, H::Bucket)>>,
}
impl<H: CrossHost> CrossFactory<H> {
    pub fn new(host: H) -> Self {
        let clock = Rc::new(Cell::new(host.start_ms()));
        Self {
            host,
            clock,
            d1: RefCell::default(),
            buckets: RefCell::default(),
        }
    }
    pub fn host(&self) -> &H {
        &self.host
    }
    /// The D1 of the last `create`.
    pub fn db(&self) -> Result<ClockD1<H::D1>, StoreError> {
        self.d1
            .borrow()
            .clone()
            .ok_or(StoreError::Backend("test.no_store"))
    }
    /// A fresh composed store with no runs, for scope 1 at the host's
    /// start time.
    pub async fn fresh(&self) -> Result<CrossStore<H>, StoreError> {
        self.fresh_at(scope(1), self.host.start_ms()).await
    }
    /// A fresh composed store with no runs for `tenant`, its clock at
    /// `at_ms` (the language cases build their runs through it), and a
    /// directory on the same D1.
    pub async fn language(
        &self,
        tenant: TenantScope,
        at_ms: i64,
    ) -> Result<(CrossStore<H>, runbook_store_d1::D1Directory<ClockD1<H::D1>>), StoreError> {
        let store = self.fresh_at(tenant, at_ms).await?;
        Ok((store, runbook_store_d1::D1Directory::new(self.db()?)))
    }
    async fn fresh_at(&self, tenant: TenantScope, at_ms: i64) -> Result<CrossStore<H>, StoreError> {
        self.clock.set(at_ms);
        let db = ClockD1 {
            inner: self.host.d1().await?,
            clock: self.clock.clone(),
        };
        fresh_d1(&db).await?;
        let (blobs, user_content) = self.host.buckets().await?;
        *self.d1.borrow_mut() = Some(db.clone());
        *self.buckets.borrow_mut() = Some((blobs.clone(), user_content.clone()));
        let objects = RemoteObjects::tracked(self.host.transport(self.clock.clone()));
        let mut store = CfStore::new(db, objects, tenant, blobs, user_content);
        // The host's after-request settle: each commit's derived rows reach
        // D1 before it returns (D56's bound is the alarm retry's).
        store.settle_commits(true);
        Ok(store)
    }
}

impl<H: CrossHost> StoreFactory for CrossFactory<H> {
    type Store = CrossStore<H>;
    async fn create(&self, initial: ExecutionProjection) -> Result<Self::Store, StoreError> {
        let store = self.fresh().await?;
        store.catalog().claim_owner(&initial.execution_id).await?;
        store.create_execution(initial).await?;
        Ok(store)
    }
    async fn fail_after(&self, store: &Self::Store, stage: WriteStage) -> Result<(), StoreError> {
        store.objects().arm_fault(stage);
        Ok(())
    }
    /// D56: a commit's derived rows reach D1 before its request returns;
    /// when D1 fails, the object's alarm retries from this delay.
    fn derived_row_delay(&self) -> Duration {
        Duration::from_millis(u64::try_from(OUTBOX_FIRST_RETRY_MS).unwrap_or(10_000))
    }
}
impl<H: CrossHost> SessionFactory for CrossFactory<H> {
    async fn backend_time_ms(&self, _store: &Self::Store) -> Result<i64, StoreError> {
        Ok(self.clock.get())
    }
    async fn scoped_client(
        &self,
        store: &Self::Store,
        tenant: TenantScope,
    ) -> Result<Self::Store, StoreError> {
        Ok(store.scoped(tenant))
    }
    async fn advance_clock(&self, _store: &Self::Store, ms: u64) -> Result<(), StoreError> {
        let ms = i64::try_from(ms).map_err(|_| StoreError::Backend("test.clock"))?;
        self.clock.set(self.clock.get() + ms);
        Ok(())
    }
}
impl<H: CrossHost> CatalogFactory for CrossFactory<H> {
    async fn redeliver_derived(
        &self,
        store: &Self::Store,
        id: &ExecutionId,
        sequence: u64,
    ) -> Result<(), StoreError> {
        store.redeliver_derived(id, sequence).await
    }
    /// One drain of the run's object (the host's settle), then the
    /// admission notices; the row must be in D1 afterwards. The bound is
    /// checked, never waited out.
    async fn await_derived(
        &self,
        store: &Self::Store,
        id: &ExecutionId,
        sequence: u64,
        max_delay: Duration,
    ) -> Result<(), StoreError> {
        if max_delay > self.derived_row_delay() {
            return Err(StoreError::Backend("test.delay_bound"));
        }
        let drained = store.drain_derived(id).await?;
        store.deliver_notices(&drained.promoted).await?;
        let notices = store.catalog().pending_notices(100).await?;
        store.deliver_notices(&notices).await?;
        match runbook_core::catalog::CatalogStore::get_run(store, id).await? {
            Some(run) if run.sequence >= sequence => Ok(()),
            _ => Err(StoreError::Backend("test.derived_row_not_applied")),
        }
    }
}
impl<H: CrossHost> WorkQueueFactory for CrossFactory<H> {
    async fn expire_lease(
        &self,
        store: &Self::Store,
        lease: &runbook_core::work_queue::WorkLease,
    ) -> Result<(), StoreError> {
        store
            .objects()
            .object(&lease.id.execution_id, &lease.scope)?
            .expire_lease(lease)
            .await
    }
}
impl<H: CrossHost> runbook_store_conformance::quotas::QuotaFactory for CrossFactory<H> {
    async fn with_quota(
        &self,
        store: &Self::Store,
        limits: runbook_core::quotas::QuotaLimits,
    ) -> Result<Self::Store, StoreError> {
        Ok(store.with_quota(limits))
    }
    async fn next_month(&self, store: &Self::Store) -> Result<Self::Store, StoreError> {
        self.clock.set(self.clock.get() + 32 * 86_400_000);
        Ok(store.clone())
    }
}
impl<H: CrossHost> QueueFactory for CrossFactory<H> {
    /// Raw rows a run owns, in its object, in D1 and in both buckets.
    async fn owned_record_counts(
        &self,
        store: &Self::Store,
        id: &ExecutionId,
    ) -> Result<BTreeMap<String, usize>, StoreError> {
        let mut counts = BTreeMap::new();
        let object = store.objects().object(id, store.scope())?;
        for (table, n) in object.table_counts().await? {
            counts.insert(
                format!("do.{table}"),
                usize::try_from(n).unwrap_or(usize::MAX),
            );
        }
        let db = self.db()?;
        for (table, n) in runbook_store_d1::testing::run_counts(&db, id)
            .await
            .map_err(|_| StoreError::Backend("test.d1_counts"))?
        {
            counts.insert(
                format!("d1.{table}"),
                usize::try_from(n).unwrap_or(usize::MAX),
            );
        }
        let (blobs, user_content) = self
            .buckets
            .borrow()
            .clone()
            .ok_or(StoreError::Backend("test.no_store"))?;
        for (name, bucket) in [("r2.blobs", blobs), ("r2.user_content", user_content)] {
            let mut n = 0;
            let mut cursor = None;
            loop {
                let page = bucket.list("", cursor.take()).await?;
                n += page
                    .keys
                    .iter()
                    .filter(|key| key.contains(&format!("/runs/{id}/")))
                    .count();
                match page.cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            counts.insert(name.into(), n);
        }
        // The conformance inventory names these two.
        counts.insert(
            "blob".into(),
            counts["r2.blobs"] + counts["r2.user_content"],
        );
        counts.insert(
            "work_lease".into(),
            counts.get("do.work_lease").copied().unwrap_or(0),
        );
        let tombstones = db
            .all(
                runbook_store_d1::prepare("SELECT count(*) FROM tombstone WHERE execution_id = ?")
                    .bind(id.as_str()),
            )
            .await
            .map_err(|_| StoreError::Backend("test.d1_counts"))?;
        let tombstones = tombstones
            .first()
            .and_then(|row| row.first())
            .and_then(|value| match value {
                runbook_store_d1::Value::Integer(n) => usize::try_from(*n).ok(),
                _ => None,
            })
            .unwrap_or(0);
        counts.insert("tombstone".into(), tombstones);
        Ok(counts)
    }
}

/// The native transport: one in-memory SQLite database per run, every
/// request and reply through JSON.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct InProcess {
    objects: Rc<RefCell<BTreeMap<ExecutionId, crate::native::NativeSql>>>,
    /// CF6: one OrgDO per organization.
    orgs: Rc<RefCell<BTreeMap<String, crate::native::NativeSql>>>,
    /// CF11: one WebhookDO per subscription.
    webhooks: runbook_worker::webhooks::LocalWebhooks<crate::native::NativeSql>,
    clock: Rc<Cell<i64>>,
}
#[cfg(not(target_arch = "wasm32"))]
impl Transport for InProcess {
    async fn call(
        &self,
        request: runbook_worker::protocol::Request,
    ) -> runbook_worker::protocol::Reply {
        let wire =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("test.encode"))?;
        let request: runbook_worker::protocol::Request =
            serde_json::from_str(&wire).map_err(|error| {
                if std::env::var_os("RB_D1_DEBUG").is_some() {
                    eprintln!("protocol decode: {error}: {}", &wire[..wire.len().min(600)]);
                }
                StoreError::Backend("test.decode")
            })?;
        let sql = self
            .objects
            .borrow_mut()
            .entry(request.execution_id.clone())
            .or_insert_with(|| crate::native::NativeSql::new(self.clock.get()))
            .clone();
        sql.set_time(request.clock.unwrap_or_else(|| self.clock.get()));
        let reply = match runbook_store_do::DoStore::open(sql, &request.execution_id, request.scope)
        {
            Ok(store) => runbook_worker::protocol::serve(&store, request.op, true).await,
            Err(error) => Err(error),
        };
        let wire = serde_json::to_string(&runbook_worker::protocol::into_wire(reply))
            .map_err(|_| StoreError::Backend("test.encode"))?;
        runbook_worker::protocol::decode_reply(
            serde_json::from_str(&wire).map_err(|_| StoreError::Backend("test.decode"))?,
        )
    }
    async fn call_org(
        &self,
        request: runbook_worker::quota::QuotaRequest,
    ) -> runbook_worker::protocol::Reply {
        let wire =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("test.encode"))?;
        let request: runbook_worker::quota::QuotaRequest =
            serde_json::from_str(&wire).map_err(|_| StoreError::Backend("test.decode"))?;
        let sql = self
            .orgs
            .borrow_mut()
            .entry(request.org.to_string())
            .or_insert_with(|| crate::native::NativeSql::new(self.clock.get()))
            .clone();
        let reply = runbook_store_do::org::OrgStore::open(sql)
            .and_then(|store| runbook_worker::quota::serve_quota(&store, request));
        let wire = serde_json::to_string(&runbook_worker::protocol::into_wire(reply))
            .map_err(|_| StoreError::Backend("test.encode"))?;
        runbook_worker::protocol::decode_reply(
            serde_json::from_str(&wire).map_err(|_| StoreError::Backend("test.decode"))?,
        )
    }
    async fn call_webhook(
        &self,
        request: runbook_worker::webhooks::WebhookRequest,
    ) -> runbook_worker::protocol::Reply {
        use runbook_worker::webhooks::WebhookObjects;
        self.webhooks.call(request).await
    }
    fn clock(&self) -> Option<i64> {
        Some(self.clock.get())
    }
}

/// The native cross host.
#[cfg(not(target_arch = "wasm32"))]
pub struct NativeCross;
#[cfg(not(target_arch = "wasm32"))]
impl CrossHost for NativeCross {
    type Transport = InProcess;
    type D1 = runbook_store_d1::native::NativeD1;
    type Bucket = runbook_store_r2::MemoryBucket;
    fn transport(&self, clock: Rc<Cell<i64>>) -> InProcess {
        InProcess {
            objects: Rc::default(),
            orgs: Rc::default(),
            webhooks: runbook_worker::webhooks::LocalWebhooks::default(),
            clock,
        }
    }
    async fn d1(&self) -> Result<Self::D1, StoreError> {
        Ok(runbook_store_d1::native::NativeD1::new(
            crate::native::START_MS,
        ))
    }
    async fn buckets(&self) -> Result<(Self::Bucket, Self::Bucket), StoreError> {
        Ok(Default::default())
    }
    fn start_ms(&self) -> i64 {
        crate::native::START_MS
    }
}
