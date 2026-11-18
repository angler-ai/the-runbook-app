//! The Cron Trigger's work (CF4, D58), portable: the `scheduled` handler in
//! `edge` runs these passes over whatever hosts it has.
//!
//! - **Archives and retention.** ARC's `archives::job::run_once` for every
//!   tenant [`ArchiveHost::tenants`] lists, at the host's clock and with
//!   each tenant's retention policy. One tenant failing does not stop the
//!   others; its days back off inside the job and retry on a later pass.
//! - **Project events (CF9).** `runbook.published` waits in D1's project
//!   outbox; the minute pass fans it out to the tenant's subscriptions
//!   ([`project_events_pass`]) before redelivery attempts it.
//! - **Webhook redelivery.** Deliveries waiting out their backoff become
//!   due between messages; natively the drain loop polls for them. Since
//!   CF11 each subscription's WebhookDO arms its alarm for its next due
//!   delivery; [`redelivery_pass`] remains for hosts that list tenants.
//! - **Delivery-log retention (CF11)**, daily: [`delivery_log_pass`].
//! - **Replay responses kept in R2 (CF12b)**, daily: [`replay_pass`].
//!
//! On Cloudflare both hosts read D1 (the projects, the archive index, the
//! webhook tables): CF3 implements them. Until then the scheduled handler
//! reports each pass as unconfigured.
use crate::shards::D1Shards;
use runbook_core::{
    StoreError,
    archives::job::{ArchiveStore, JobStats, ReportSource, RetentionPolicy, run_once},
    catalog::TenantScope,
    directory::DirectoryStore,
    webhooks::{
        DeliverySettings, DeliveryStats, SecretSealer, WebhookStore, WebhookTransport, deliver_due,
    },
};
use std::future::Future;

/// Most days, deletions and purges one tenant's pass takes (as native).
pub const ARCHIVE_BATCH: usize = 16;

/// Everything the archive pass needs from its host.
pub trait ArchiveHost {
    type Store: ArchiveStore;
    type Directory: DirectoryStore;
    type Reports: ReportSource;
    /// The tenants this pass covers.
    fn tenants(&self) -> impl Future<Output = Result<Vec<TenantScope>, StoreError>>;
    /// One tenant's composed store (D1 index and catalog, DOs, R2).
    fn store(&self, scope: &TenantScope) -> Self::Store;
    fn directory(&self) -> &Self::Directory;
    fn reports(&self) -> &Self::Reports;
    /// The tenant's retention (plan or operator defaults, D54).
    fn policy(&self, scope: &TenantScope) -> RetentionPolicy;
    /// The store clock the pass runs at.
    fn now_ms(&self) -> i64;
}

/// What one archive pass did, over every tenant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchivePass {
    pub tenants: usize,
    pub job: JobStats,
    /// Tenants whose pass stopped early, with the code.
    pub failed: Vec<(TenantScope, &'static str)>,
}

fn code(error: &StoreError) -> &'static str {
    match error {
        StoreError::NotFound => "store.not_found",
        StoreError::InvalidBatch(code) | StoreError::Backend(code) => code,
    }
}

fn add(total: &mut JobStats, one: &JobStats) {
    total.written += one.written;
    total.removed += one.removed;
    total.live_deleted += one.live_deleted;
    total.live_skipped += one.live_skipped;
    total.results_pending += one.results_pending;
    total.cancels_pending += one.cancels_pending;
    total.archives_expired += one.archives_expired;
    total.summaries_purged += one.summaries_purged;
    total.versions_swept += one.versions_swept;
    total.failed += one.failed;
}

/// Run the archive and retention job once for every tenant.
pub async fn archive_pass<H: ArchiveHost>(host: &H) -> Result<ArchivePass, StoreError> {
    let tenants = host.tenants().await?;
    let mut pass = ArchivePass {
        tenants: tenants.len(),
        ..ArchivePass::default()
    };
    let now = host.now_ms();
    for scope in tenants {
        let store = host.store(&scope);
        let policy = host.policy(&scope);
        match run_once(
            &store,
            host.directory(),
            host.reports(),
            &scope,
            now,
            &policy,
            ARCHIVE_BATCH,
        )
        .await
        {
            Ok(stats) => add(&mut pass.job, &stats),
            Err(error) => pass.failed.push((scope, code(&error))),
        }
    }
    Ok(pass)
}

/// Everything the redelivery pass needs from its host.
pub trait DeliveryHost {
    type Webhooks: WebhookStore;
    type Transport: WebhookTransport;
    type Sealer: SecretSealer;
    /// Tenants that may have deliveries due.
    fn tenants(&self) -> impl Future<Output = Result<Vec<TenantScope>, StoreError>>;
    fn webhooks(&self) -> &Self::Webhooks;
    fn transport(&self) -> &Self::Transport;
    fn sealer(&self) -> &Self::Sealer;
}

/// Attempt every tenant's due deliveries once.
pub async fn redelivery_pass<H: DeliveryHost>(host: &H) -> Result<DeliveryStats, StoreError> {
    let mut total = DeliveryStats::default();
    for scope in host.tenants().await? {
        let stats = deliver_due(
            host.webhooks(),
            host.transport(),
            host.sealer(),
            &scope,
            DeliverySettings::default(),
        )
        .await?;
        total.claimed += stats.claimed;
        total.delivered += stats.delivered;
        total.retried += stats.retried;
        total.failed += stats.failed;
        total.errors += stats.errors;
    }
    Ok(total)
}

/// Project events (`runbook.published`) one tenant's pass fans out.
pub const PROJECT_EVENT_BATCH: usize = 100;

/// CF9 fix: fan out every tenant's pending project events (the D1
/// `project_event_outbox`) to its subscriptions, as the native drain does
/// (`process_project_events`); each delivery's WebhookDO attempts it.
/// Returns how many events went out. One tenant failing does not stop the
/// others.
pub async fn project_events_pass<D: runbook_store_d1::D1, W: WebhookStore>(
    shards: &D1Shards<D>,
    webhooks: &W,
) -> Result<usize, StoreError> {
    let mut total = 0;
    for (db, scope) in tenants(
        shards,
        "SELECT DISTINCT org_id, project_id FROM project_event_outbox",
    )
    .await?
    {
        let catalog = runbook_store_d1::D1Catalog::new(db, scope.clone());
        if let Ok(n) = runbook_core::webhooks::process_project_events(
            &catalog,
            webhooks,
            &scope,
            PROJECT_EVENT_BATCH,
        )
        .await
        {
            total += n;
        }
    }
    Ok(total)
}

/// Delivery-log rows one tenant's retention pass drops at most.
pub const DELIVERY_LOG_BATCH: usize = 500;

/// CF11: delivery-log retention as natively
/// (`DELIVERY_LOG_RETENTION_MS`, `purge_expired_log`), for every tenant
/// with a subscription: each subscription's WebhookDO drops its old
/// finished deliveries and attempts, and D1 its old event records and
/// replay rows. Returns how many rows went.
pub async fn delivery_log_pass<D: runbook_store_d1::D1, W: WebhookStore>(
    shards: &D1Shards<D>,
    webhooks: &W,
) -> Result<usize, StoreError> {
    let mut total = 0;
    for (_, scope) in tenants(
        shards,
        "SELECT DISTINCT org_id, project_id FROM webhook_subscription",
    )
    .await?
    {
        if let Ok(n) =
            runbook_core::webhooks::purge_expired_log(webhooks, &scope, DELIVERY_LOG_BATCH).await
        {
            total += n;
        }
    }
    Ok(total)
}

/// The tenants `sql` lists on each shard, with that shard: only the
/// organizations the shard map gives that shard (CF11), so rows left on a
/// shard an organization moved off are not acted on.
async fn tenants<D: runbook_store_d1::D1>(
    shards: &D1Shards<D>,
    sql: &'static str,
) -> Result<Vec<(D, TenantScope)>, StoreError> {
    use runbook_store_d1::sql::Columns;
    let mut out = Vec::new();
    for (binding, db) in shards.data_shards() {
        let rows = db
            .all(runbook_store_d1::prepare(sql))
            .await
            .map_err(|_| StoreError::Backend("d1.tenants"))?;
        for row in &rows {
            let (Ok(org_id), Ok(project_id)) = (row.parse(0), row.parse(1)) else {
                continue;
            };
            let scope = TenantScope { org_id, project_id };
            if shards.owns(&binding, &scope.org_id) {
                out.push((db.clone(), scope));
            }
        }
    }
    Ok(out)
}

/// CF12b: request replay responses kept in R2 go with their rows. The
/// authoring writer sweeps its organization's expired ones before each
/// write; this daily pass sweeps the rest (an organization with no later
/// authoring write), oldest first, in batches of
/// `runbook_store_d1::replay::SWEEP_BATCH` rows until one comes back short
/// or `CRON_SWEEP_PASSES` have run (review round 1), on the control
/// database. Returns how many rows went and how many were kept because
/// their object's delete failed.
pub async fn replay_pass<H: crate::cloudflare::CfHost>(
    backends: &crate::cloudflare::CfBackends<H>,
    now: i64,
) -> Result<runbook_store_d1::replay::Swept, StoreError> {
    let blobs = crate::cloudflare::CfReplayBlobs(backends.clone());
    runbook_store_d1::replay::sweep(
        &backends.control(),
        None,
        now,
        &blobs,
        runbook_store_d1::replay::CRON_SWEEP_PASSES,
    )
    .await
    .map_err(|_| StoreError::Backend("d1.replay_sweep"))
}

/// NOTIFY1: assemble yesterday's digests, then send due mail. Runs on
/// the minute pass; assembly is keyed by UTC day, so repeats are no-ops.
pub async fn email_pass<
    D: runbook_store_d1::D1,
    T: runbook_core::notify::deliver::MailTransport,
>(
    db: &D,
    transport: &T,
    origin: &str,
) -> Result<(usize, usize), StoreError> {
    let notify = runbook_store_d1::D1Notify::new(db.clone());
    let directory = runbook_store_d1::D1Directory::new(db.clone());
    let digests =
        runbook_core::notify::deliver::assemble_daily_digest(&notify, &directory, origin).await?;
    let sent = runbook_core::notify::deliver::deliver_emails(&notify, transport, 25).await?;
    Ok((sent, digests))
}
