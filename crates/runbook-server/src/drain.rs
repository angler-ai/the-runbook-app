//! B4.4: the background drain. Each pass claims due work tenant by tenant
//! and processes it through `runbook_core::drain::process_lease`: start
//! activations, timers and expired claim leases. Correctness comes from the
//! store's generation-fenced leases and execution compare-and-set, so any
//! number of drain workers (or restarts) apply each transition once.
//!
//! UP1: each pass also sweeps evidence slots never finalized (D49).
//!
//! ARC2 (D53–D54): the drain worker also runs the daily archive and
//! retention job ([`Drainer::archive_once`]) once the store's UTC day
//! changes, and every `RetentionConfig::interval` in between (stale
//! rewrites, retries), tenant by tenant with each project's policy.
//!
//! HOST1: any number of replicas may run these workers on one database:
//! timers, activations, claim expiry and notifications are claimed under
//! generation-fenced leases, project events and deliveries are recorded once
//! by event ID, and the archive store's row locks arbitrate racing archive
//! passes. A stopping worker ([`DrainWorker::begin_stop`]) claims nothing
//! more and releases the leases it has not processed.
//!
//! C1: the drain also fans notification intents out to webhook deliveries,
//! but never performs HTTP. Deliveries run on a separate `DeliveryWorker`
//! thread, a few per tenant per round (round-robin), so a slow or hanging
//! receiver can delay other deliveries but never timers, approval expiries
//! or claim expiries.
use crate::{Error, config::ServerConfig};
use runbook_core::{
    catalog::TenantScope,
    drain::{Processed, process_lease, process_notification},
    uploads::sweep_stale,
    webhooks::{
        DeliverySettings, PURGE_BATCH, deliver_due, process_project_events, purge_expired_log,
    },
    work_queue::{WorkKind, WorkQueue},
};
use runbook_store_postgres::{
    PostgresDirectory, PostgresExecutionStore, PostgresNotify, PostgresWebhooks, tenant_scopes,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug)]
pub struct DrainSettings {
    /// Most leases claimed per tenant per pass.
    pub batch: usize,
    /// How long a claimed lease fences other workers.
    pub lease_ms: u64,
    /// Pause after a pass that found nothing to do.
    pub idle: Duration,
}
impl Default for DrainSettings {
    fn default() -> Self {
        Self {
            batch: 16,
            lease_ms: 30_000,
            idle: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainStats {
    pub claimed: usize,
    pub activated: usize,
    pub fired: usize,
    pub claims_expired: usize,
    pub obsolete: usize,
    pub not_due: usize,
    pub retried: usize,
    pub skipped: usize,
    /// Notification intents fanned out to webhook deliveries (C1).
    pub notified: usize,
    /// Leases whose processing failed; they expire and are claimed again.
    pub failed: usize,
    /// NOTIFY1: notification emails sent, and daily summaries assembled.
    pub emails_sent: usize,
    pub email_digests: usize,
    /// Webhook deliveries claimed, and how their attempts ended (C1).
    pub deliveries_claimed: usize,
    pub delivered: usize,
    pub delivery_retries: usize,
    pub delivery_failures: usize,
    /// Attempts whose result did not commit; their lease expires and the
    /// same delivery (same event ID) is attempted again.
    pub delivery_errors: usize,
    /// Project-level events (`runbook.published`) fanned out (C1).
    pub project_events: usize,
    /// Delivery-log rows purged after the 90-day retention (C1).
    pub purged: usize,
    /// Evidence slots never finalized, removed 24 hours after issue (UP1).
    pub uploads_swept: usize,
    /// STO1: orphaned objects swept from bucket storage.
    pub blobs_swept: usize,
    /// HOST1: expired shared rate-limit windows purged.
    pub rate_windows_purged: usize,
    /// HOST1: leases given back unprocessed because the worker is stopping.
    pub released: usize,
    /// ARC2: the archive and retention job's work this pass.
    pub archive: runbook_core::archives::job::JobStats,
}
impl DrainStats {
    /// Whether this pass did anything, so the loop should go again at once.
    pub fn busy(&self) -> bool {
        self.claimed
            + self.emails_sent
            + self.email_digests
            + self.deliveries_claimed
            + self.project_events
            + self.purged
            + self.uploads_swept
            + self.blobs_swept
            + self.rate_windows_purged
            > 0
            || self.archive.busy()
    }
}

/// ARC2: each run's `report.md` is the same Markdown export the report route
/// serves (named people, D34 custom report), rendered from live data.
pub struct ArchiveReports(pub crate::executions::PostgresExecutionBackend);
impl runbook_core::archives::job::ReportSource for ArchiveReports {
    async fn report_markdown(
        &self,
        scope: &TenantScope,
        execution_id: &runbook_model::ExecutionId,
    ) -> Result<String, runbook_core::StoreError> {
        use runbook_api::executions::ExecutionBackend;
        self.0
            .report_markdown(scope, execution_id)
            .await
            .map_err(|e| match e.status {
                404 => runbook_core::StoreError::NotFound,
                _ => runbook_core::StoreError::Backend("archive.report_unavailable"),
            })
    }
}

pub(crate) use crate::slack_http::SlackHttp;

fn fanout_step_title(intent: &runbook_engine::NotificationIntent) -> Option<&str> {
    let first = intent.step_run_ids.first()?;
    intent
        .ready_steps
        .iter()
        .find(|work| &work.step_run_id == first)
        .map(|work| work.title.as_str())
}

/// One drain worker's stores. Tenant stores share a single connection pool.
pub struct Drainer {
    config: ServerConfig,
    base: Option<PostgresExecutionStore>,
    directory: PostgresDirectory,
    settings: DrainSettings,
    webhooks: PostgresWebhooks,
    notify: PostgresNotify,
    mail: Option<crate::email::NativeMail>,
    warned_no_mail: bool,
    transport: crate::webhooks::HttpTransport,
    sealer: Option<crate::webhooks::AeadSealer>,
    delivery: DeliverySettings,
    /// ARC2: the store day and host instant of the last archive pass.
    archived: Option<(i64, std::time::Instant)>,
    /// HOST1: set when the worker is stopping.
    stop: Arc<AtomicBool>,
    /// STO1: object storage handle when GCS or S3 is configured.
    objects: Option<Arc<runbook_store_objects::ObjectStorage>>,
    /// STO1: host instant of the last blob sweep.
    last_swept: Option<std::time::Instant>,
    /// LANG9b: the store-clock minute of the last schedule pass.
    scheduled: Option<i64>,
}
impl Drainer {
    pub fn new(config: ServerConfig, settings: DrainSettings) -> Self {
        let host = crate::webhooks::ServerHost::new(&config.webhooks);
        let objects = config.blob_store.build_storage().ok().flatten();
        Self {
            directory: PostgresDirectory::new(config.database.clone()),
            webhooks: PostgresWebhooks::new(config.database.clone()),
            notify: PostgresNotify::new(config.database.clone()),
            mail: config.email.clone().map(crate::email::NativeMail::new),
            warned_no_mail: false,
            transport: crate::webhooks::HttpTransport::new(host.policy, host.resolve),
            sealer: host.sealer,
            delivery: DELIVERY_ROUND,
            config,
            base: None,
            settings,
            archived: None,
            stop: Arc::default(),
            objects,
            last_swept: None,
            scheduled: None,
        }
    }
    /// HOST1: the flag that makes passes stop early (shared with the worker).
    pub fn with_stop(mut self, stop: Arc<AtomicBool>) -> Self {
        self.stop = stop;
        self
    }
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
    /// Override webhook delivery batching and lease length.
    pub fn with_delivery(mut self, delivery: DeliverySettings) -> Self {
        self.delivery = delivery;
        self
    }
    /// STO1: override object storage handle (for tests).
    pub fn with_objects(
        mut self,
        objects: Option<Arc<runbook_store_objects::ObjectStorage>>,
    ) -> Self {
        self.objects = objects;
        self
    }
    /// Test seam: the webhook store this drainer delivers from.
    #[doc(hidden)]
    pub fn webhooks(&self) -> &PostgresWebhooks {
        &self.webhooks
    }

    fn store_for(&mut self, scope: TenantScope) -> PostgresExecutionStore {
        match &self.base {
            Some(base) => base.scoped(scope),
            None => {
                let store = PostgresExecutionStore::new(self.config.database.clone(), scope)
                    .with_objects(self.objects.clone());
                self.base = Some(store.clone());
                store
            }
        }
    }

    /// Claim and process everything currently due, across all tenants.
    pub async fn drain_once(&mut self) -> Result<DrainStats, Error> {
        let mut stats = DrainStats::default();
        let scopes = tenant_scopes(&self.config.database)
            .await
            .map_err(|_| Error("drain.scopes_unavailable"))?;
        for scope in scopes {
            if self.stopping() {
                break;
            }
            let store = self.store_for(scope.clone());
            let leases = store
                .claim_due(self.settings.batch, self.settings.lease_ms)
                .await
                .map_err(|_| Error("drain.claim_failed"))?;
            stats.claimed += leases.len();
            for (at, lease) in leases.iter().enumerate() {
                if self.stopping() {
                    // HOST1: hand the rest back for another instance.
                    for lease in &leases[at..] {
                        if let Ok(true) = store.release_lease(lease).await {
                            stats.released += 1;
                        }
                    }
                    return Ok(stats);
                }
                let processed = if matches!(lease.id.kind, WorkKind::Notification { .. }) {
                    // NOTIFY1 first, then webhook fan-out (C1): both are
                    // idempotent, so a failure retries the pair safely.
                    let emailed = self.fan_out_email(&store, &scope, lease).await;
                    match emailed {
                        Ok(()) => {
                            process_notification(&store, &self.webhooks, scope.clone(), lease)
                                .await
                                .map_err(|_| ())
                        }
                        Err(()) => Err(()),
                    }
                } else {
                    process_lease(&store, &self.directory, scope.clone(), lease)
                        .await
                        .map_err(|_| ())
                };
                match processed {
                    Ok(Processed::Activated) => stats.activated += 1,
                    Ok(Processed::Fired) => stats.fired += 1,
                    Ok(Processed::ClaimExpired) => stats.claims_expired += 1,
                    Ok(Processed::Obsolete) => stats.obsolete += 1,
                    Ok(Processed::NotDue) => stats.not_due += 1,
                    Ok(Processed::Retry) => stats.retried += 1,
                    Ok(Processed::Skipped) => stats.skipped += 1,
                    Ok(Processed::Notified) => stats.notified += 1,
                    // The lease expires and another pass retries it; one bad
                    // run never stops the drain for everyone else.
                    Err(()) => stats.failed += 1,
                }
            }
            // C1: project-level outbox rows, fanned out like intents.
            match process_project_events(&store, &self.webhooks, &scope, self.settings.batch).await
            {
                Ok(done) => stats.project_events += done,
                Err(_) => stats.failed += 1,
            }
            // UP1 (D49): evidence slots still pending 24 hours after issue.
            match sweep_stale(&store, &scope, self.settings.batch).await {
                Ok(done) => stats.uploads_swept += done,
                Err(_) => stats.failed += 1,
            }
        }
        // HOST1: expired shared rate-limit windows, one bounded batch.
        match runbook_store_postgres::purge_rate_windows(&self.config.database, self.settings.batch)
            .await
        {
            Ok(done) => stats.rate_windows_purged += done,
            Err(_) => stats.failed += 1,
        }
        Ok(stats)
    }

    /// The store clock the archive job runs on (plus the debug-only test
    /// offset, `RetentionConfig::test_clock_offset_ms`).
    pub async fn archive_clock_ms(&mut self) -> Result<i64, Error> {
        let scope = TenantScope {
            org_id: "org_00000000000000000000000000"
                .parse()
                .map_err(|_| Error("drain.clock"))?,
            project_id: "prj_00000000000000000000000000"
                .parse()
                .map_err(|_| Error("drain.clock"))?,
        };
        let now = self
            .store_for(scope)
            .backend_time_ms()
            .await
            .map_err(|_| Error("drain.clock_unavailable"))?;
        Ok(now + self.config.retention.test_clock_offset_ms)
    }

    /// LANG9b: the schedule pass, once per store-clock minute. Any number
    /// of replicas may run it: each tick's start is keyed, so it starts once.
    pub async fn schedule_if_due(
        &mut self,
    ) -> Result<Option<runbook_api::triggers::SchedulePass>, Error> {
        if self.stopping() {
            return Ok(None);
        }
        let now = self.archive_clock_ms().await?;
        let minute = now.div_euclid(60_000);
        if self.scheduled == Some(minute) {
            return Ok(None);
        }
        self.scheduled = Some(minute);
        let backends = crate::backends::PostgresBackends::new(
            &self.config,
            Arc::new(tokio::sync::Semaphore::new(1)),
            None,
        );
        let pass =
            runbook_api::triggers::schedule_pass(&backends, &self.config.origin, now, || {
                crate::bootstrap::id("", now).ok()
            })
            .await
            .map_err(|_| Error("triggers.pass_failed"))?;
        for (runbook, code) in &pass.failed {
            crate::log::warning(
                "triggers",
                &format!("schedule start for {runbook} failed: {code}"),
            );
        }
        Ok(Some(pass))
    }

    /// ARC2: run the archive job when the store's UTC day changed since the
    /// last pass, or the interval passed. Returns `None` when not due.
    pub async fn archive_if_due(&mut self) -> Result<Option<DrainStats>, Error> {
        if self.stopping() {
            return Ok(None);
        }
        let now = self.archive_clock_ms().await?;
        let day = runbook_core::archives::day_of(now);
        let due = self.archived.is_none_or(|(last_day, at)| {
            last_day != day || at.elapsed() >= self.config.retention.interval
        });
        if !due {
            return Ok(None);
        }
        self.archive_once().await.map(Some)
    }

    /// One pass of the archive and retention job for every tenant, each
    /// with its project's policy (D54). A tenant's failure is counted and
    /// retried on the next pass.
    pub async fn archive_once(&mut self) -> Result<DrainStats, Error> {
        let now = self.archive_clock_ms().await?;
        let mut stats = DrainStats::default();
        let scopes = tenant_scopes(&self.config.database)
            .await
            .map_err(|_| Error("drain.scopes_unavailable"))?;
        let reports = ArchiveReports(crate::executions::PostgresExecutionBackend {
            config: self.config.database.clone(),
            objects: self.objects.clone(),
        });
        for scope in scopes {
            let store = self.store_for(scope.clone());
            let policy = self.config.retention.policy_for(&scope.project_id);
            match runbook_core::archives::job::run_once(
                &store,
                &self.directory,
                &reports,
                &scope,
                now,
                &policy,
                self.settings.batch,
            )
            .await
            {
                Ok(done) => {
                    let a = &mut stats.archive;
                    a.written += done.written;
                    a.removed += done.removed;
                    a.live_deleted += done.live_deleted;
                    a.archives_expired += done.archives_expired;
                    a.summaries_purged += done.summaries_purged;
                    a.live_skipped += done.live_skipped;
                    a.results_pending += done.results_pending;
                    a.cancels_pending += done.cancels_pending;
                    a.failed += done.failed;
                    a.first_failure = a.first_failure.or(done.first_failure);
                }
                Err(error) => {
                    stats.archive.failed += 1;
                    stats.archive.first_failure.get_or_insert(match error {
                        runbook_core::StoreError::NotFound => "store.not_found",
                        runbook_core::StoreError::InvalidBatch(code)
                        | runbook_core::StoreError::Backend(code) => code,
                    });
                }
            }
        }
        // Codes and counts only: never a tenant, run or runbook ID.
        for (count, code) in [
            (
                stats.archive.results_pending,
                runbook_core::archives::job::RESULT_UNDELIVERED,
            ),
            (
                stats.archive.cancels_pending,
                runbook_core::archives::job::CHILD_CANCEL_PENDING,
            ),
        ] {
            if count > 0 {
                crate::log::warning(
                    "archive",
                    &format!(
                        "archive job: kept {count} run(s) ({code}): {}",
                        runbook_core::archives::job::refusal_message(code).unwrap_or_default()
                    ),
                );
            }
        }
        if let Some(code) = stats.archive.first_failure {
            crate::log::warning(
                "archive",
                &format!(
                    "archive job: {} step(s) failed, first: {code}; failing days back off",
                    stats.archive.failed
                ),
            );
        }
        // A busy pass (a full batch somewhere) runs again soon; a quiet one
        // waits for the next day or interval.
        self.archived = (!stats.archive.busy()).then(|| {
            (
                runbook_core::archives::day_of(now),
                std::time::Instant::now(),
            )
        });
        Ok(stats)
    }

    /// One delivery round: at most `delivery.batch` deliveries for each
    /// tenant in turn, each under its own lease, with no store transaction
    /// open during the POST. Without a sealing key nothing is sent.
    /// NOTIFY1: record this intent's emails before the webhook fan-out
    /// acknowledges the lease. A missing run/intent is process_notification's
    /// obsolete case, not an email error.
    async fn fan_out_email(
        &self,
        store: &PostgresExecutionStore,
        scope: &TenantScope,
        lease: &runbook_core::work_queue::WorkLease,
    ) -> Result<(), ()> {
        use runbook_core::{ExecutionStore, catalog::CatalogStore};
        let WorkKind::Notification { event_id } = &lease.id.kind else {
            return Ok(());
        };
        let run = store.load(&lease.id.execution_id).await.map_err(|_| ())?;
        let Some(intent) = run.outbox.get(event_id) else {
            return Ok(());
        };
        let title = store
            .get_run(&lease.id.execution_id)
            .await
            .ok()
            .flatten()
            .map(|summary| summary.title)
            .unwrap_or_default();
        let fanned = runbook_core::notify::fanout::fan_out_email(
            &self.notify,
            &self.directory,
            scope,
            intent,
            &title,
            &self.config.origin,
            || {
                let mut bytes = [0u8; 16];
                if getrandom::fill(&mut bytes).is_ok() {
                    bytes.iter().map(|b| format!("{b:02x}")).collect()
                } else {
                    // No entropy: an empty salt matches no token, so the
                    // links fail closed instead of being guessable.
                    String::new()
                }
            },
        )
        .await
        .map_err(|_| ())?;
        let _ = fanned;
        // SLACK1 stage 1: best-effort beside the durable channels. A
        // Slack outage is logged by ureq's caller shape only as a failed
        // post; the lease still completes through webhooks/email.
        let _ = runbook_core::slack::fan_out_slack(
            &self.notify,
            &self.directory,
            &SlackHttp,
            self.config.slack.as_ref(),
            scope,
            intent,
            &title,
            fanout_step_title(intent),
            &self.config.origin,
        )
        .await;
        Ok(())
    }

    /// NOTIFY1: assemble yesterday's digests, then send due mail. Runs on
    /// the delivery worker so a slow SMTP server never delays timers.
    async fn mail_once(&mut self) -> (usize, usize, usize) {
        let Some(mail) = &self.mail else {
            if !self.warned_no_mail {
                self.warned_no_mail = true;
                crate::log::warning(
                    "notify",
                    "no mail transport is configured; notification email stays queued",
                );
            }
            return (0, 0, 0);
        };
        let digests = runbook_core::notify::deliver::assemble_daily_digest(
            &self.notify,
            &self.directory,
            &self.config.origin,
        )
        .await
        .unwrap_or(0);
        match runbook_core::notify::deliver::deliver_emails(&self.notify, mail, 25).await {
            Ok(sent) => (sent, digests, 0),
            Err(_) => (0, digests, 1),
        }
    }

    pub async fn deliver_once(&mut self) -> Result<DrainStats, Error> {
        let mut stats = DrainStats::default();
        let (sent, digests, mail_errors) = self.mail_once().await;
        stats.emails_sent += sent;
        stats.email_digests += digests;
        stats.delivery_errors += mail_errors;
        let Some(sealer) = &self.sealer else {
            return Ok(stats);
        };
        let scopes = tenant_scopes(&self.config.database)
            .await
            .map_err(|_| Error("drain.scopes_unavailable"))?;
        for scope in scopes {
            if self.stopping() {
                break;
            }
            match deliver_due(
                &self.webhooks,
                &self.transport,
                sealer,
                &scope,
                self.delivery,
            )
            .await
            {
                Ok(done) => {
                    stats.deliveries_claimed += done.claimed;
                    stats.delivered += done.delivered;
                    stats.delivery_retries += done.retried;
                    stats.delivery_failures += done.failed;
                    stats.delivery_errors += done.errors;
                }
                Err(_) => stats.delivery_errors += 1,
            }
            // Delivery-log retention, one bounded batch per tenant per round.
            match purge_expired_log(&self.webhooks, &scope, PURGE_BATCH).await {
                Ok(purged) => stats.purged += purged,
                Err(_) => stats.delivery_errors += 1,
            }
        }
        Ok(stats)
    }

    /// STO1: run the orphan blob sweeper if object storage is configured and interval passed.
    pub async fn sweep_if_due(&mut self) -> Result<Option<DrainStats>, Error> {
        if self.stopping() {
            return Ok(None);
        }
        if self.objects.is_none() {
            return Ok(None);
        }
        let due = self
            .last_swept
            .is_none_or(|at| at.elapsed() >= self.config.blob_store.sweep_interval);
        if !due {
            return Ok(None);
        }
        self.sweep_once().await.map(Some)
    }

    /// STO1: run one pass of the orphan blob sweeper.
    pub async fn sweep_once(&mut self) -> Result<DrainStats, Error> {
        let mut stats = DrainStats::default();
        let scope = TenantScope {
            org_id: "org_00000000000000000000000000"
                .parse()
                .map_err(|_| Error("drain.scope"))?,
            project_id: "prj_00000000000000000000000000"
                .parse()
                .map_err(|_| Error("drain.scope"))?,
        };
        let store = self.store_for(scope);
        match store.sweep_blobs(self.config.blob_store.sweep_age).await {
            Ok(swept) => {
                stats.blobs_swept = swept;
            }
            Err(e) => {
                crate::log::warning("blob", &format!("sweeper failed: {e:?}"));
            }
        }
        self.last_swept = Some(std::time::Instant::now());
        Ok(stats)
    }
}

/// Default webhook round: four deliveries per tenant per round, each lease
/// a minute (one attempt is at most the 10 s timeout plus 5 s of DNS).
pub const DELIVERY_ROUND: DeliverySettings = DeliverySettings {
    batch: 4,
    lease_ms: 60_000,
};

/// A drain loop on its own thread with a local executor (stores are not Send).
pub struct DrainWorker {
    stop: Option<oneshot::Sender<()>>,
    stopping: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl DrainWorker {
    /// Timers, activations, claim expiry and webhook fan-out.
    pub fn start(config: ServerConfig, settings: DrainSettings) -> Result<Self, Error> {
        config.retention.check()?;
        Self::spawn("runbook-drain", config, settings, Pass::Drain)
    }
    fn spawn(
        name: &str,
        config: ServerConfig,
        settings: DrainSettings,
        pass: Pass,
    ) -> Result<Self, Error> {
        let (stop, mut stopped) = oneshot::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let flag = stopping.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error("drain.runtime"))?;
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                runtime.block_on(async move {
                    let mut drainer = Drainer::new(config, settings).with_stop(flag);
                    loop {
                        let result = match pass {
                            Pass::Drain => match drainer.drain_once().await {
                                Ok(mut stats) => {
                                    if let Ok(Some(archive)) = drainer.archive_if_due().await {
                                        stats.archive = archive.archive;
                                    }
                                    if let Ok(Some(swept)) = drainer.sweep_if_due().await {
                                        stats.blobs_swept = swept.blobs_swept;
                                    }
                                    if let Err(error) = drainer.schedule_if_due().await {
                                        crate::log::warning("triggers", error.0);
                                    }
                                    Ok(stats)
                                }
                                other => other,
                            },
                            Pass::Deliver => drainer.deliver_once().await,
                        };
                        let busy = matches!(result, Ok(stats) if stats.busy());
                        if busy {
                            if stopped.try_recv().is_ok() {
                                return;
                            }
                            continue;
                        }
                        tokio::select! {
                            _ = &mut stopped => return,
                            _ = tokio::time::sleep(settings.idle) => {}
                        }
                    }
                });
            })
            .map_err(|_| Error("drain.worker"))?;
        Ok(Self {
            stop: Some(stop),
            stopping,
            thread: Some(thread),
        })
    }
    /// HOST1: stop claiming; the pass in progress gives back what it has
    /// not processed and the loop ends. Returns at once.
    pub fn begin_stop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
    /// HOST1: stop and wait for the loop to end, until `deadline`. A pass
    /// still running then (a database call or webhook POST in flight) is
    /// left to the process exit: its leases expire and are claimed again.
    /// Returns whether the loop ended in time.
    pub async fn finish(mut self, deadline: std::time::Instant) -> bool {
        self.begin_stop();
        let Some(thread) = self.thread.take() else {
            return true;
        };
        while !thread.is_finished() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if thread.is_finished() {
            let _ = thread.join();
            true
        } else {
            false
        }
    }
}
impl Drop for DrainWorker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Copy)]
enum Pass {
    Drain,
    Deliver,
}

/// Webhook delivery on its own thread, separate from timers (C1, I2).
pub struct DeliveryWorker(DrainWorker);
impl DeliveryWorker {
    pub fn start(config: ServerConfig, settings: DrainSettings) -> Result<Self, Error> {
        DrainWorker::spawn("runbook-webhooks", config, settings, Pass::Deliver).map(Self)
    }
    /// See [`DrainWorker::begin_stop`].
    pub fn begin_stop(&mut self) {
        self.0.begin_stop();
    }
    /// See [`DrainWorker::finish`].
    pub async fn finish(self, deadline: std::time::Instant) -> bool {
        self.0.finish(deadline).await
    }
}
