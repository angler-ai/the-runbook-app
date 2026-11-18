//! HOST1: `serve` from start to exit. Startup migrates the database (under
//! its advisory lock), binds, starts the background workers and serves.
//! SIGTERM (or SIGINT) starts a shutdown bounded by
//! `ServerConfig::shutdown_timeout` (8 s by default, inside Cloud Run's 10 s):
//!
//! 1. readiness answers 503 and open event streams end;
//! 2. the drain and delivery workers stop claiming; the drain releases the
//!    leases it claimed but has not processed, so another instance takes
//!    them at once (a delivery in progress finishes, or its lease expires
//!    and the same event is delivered again: deliveries are at least once);
//! 3. no new connection is accepted, in-flight requests finish and their
//!    connections close;
//! 4. the auth child gets SIGTERM, then SIGKILL after a short grace;
//! 5. the process exits 0.
//!
//! Whatever is still running at the deadline is abandoned: every piece of
//! state is in Postgres, so work is delayed (a lease expires), never lost.
//! A second signal exits at once.
use crate::{Error, config::ServerConfig, log};
use std::time::{Duration, Instant};

/// Time kept back from the deadline for stopping the auth child.
const AUTH_STOP_RESERVE: Duration = Duration::from_millis(1500);

/// `runbook-server serve`.
pub async fn run(config: ServerConfig) -> Result<(), Error> {
    let shutdown = shutdown_signal()?;
    serve_until(config, shutdown).await
}

/// `serve` until `shutdown` resolves, then shut down within the configured
/// timeout. Tests pass their own trigger.
pub async fn serve_until(
    mut config: ServerConfig,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<(), Error> {
    log::init(config.log_format);
    if let Some(warning) = config.insecure_origin_warning() {
        log::warning("server", warning);
    }
    if let Some(warning) = config.uploads.warning(&config.origin) {
        log::warning("server", warning);
    }
    if let Some(warning) = config.uploads.cookie_warning(&config.origin) {
        log::warning("server", warning);
    }
    for warning in config.database.tls_warnings() {
        log::warning("server", warning);
    }
    for warning in &config.theme_warnings {
        log::warning("server", warning);
    }
    let listeners = crate::http::Listeners::bind(&mut config).await?;
    log::info("server", "database migrations are current");
    // Timers, activations and claim expiry run beside the HTTP server;
    // webhook deliveries run on their own thread so a slow receiver never
    // delays a timer.
    let mut drain =
        crate::drain::DrainWorker::start(config.clone(), crate::drain::DrainSettings::default())?;
    let mut deliveries = crate::drain::DeliveryWorker::start(
        config.clone(),
        crate::drain::DrainSettings::default(),
    )?;
    let limits = config.transport.clone();
    let worker = crate::http::Worker::start(config.clone())?;
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let main = crate::http::serve_until(
        listeners.main,
        crate::http::main_router(worker.client(), &config),
        &limits,
        stopping.clone(),
    );
    let (files_client, files_limits) = (worker.client(), limits.clone());
    let files = async move {
        match listeners.files {
            Some(files) => {
                crate::http::serve_until(
                    files,
                    crate::http::usercontent_router(files_client),
                    &files_limits,
                    stopping,
                )
                .await
            }
            None => Ok(()),
        }
    };
    log::info(
        "server",
        &format!(
            "listening on {}{}",
            config.listen,
            if config.uploads.shared_port() {
                " (user content on the same port, by host)"
            } else if config.uploads.usercontent_listen.is_some() {
                " (user content on its own listener)"
            } else {
                ""
            }
        ),
    );
    let serving = async { tokio::try_join!(main, files).map(|_| ()) };
    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => return result,
        () = shutdown => {}
    }

    let started = Instant::now();
    let deadline = started + config.shutdown_timeout;
    log::info("server", "shutting down: draining requests and workers");
    worker.begin_shutdown();
    drain.begin_stop();
    deliveries.begin_stop();
    stop.send_replace(true);
    let http_deadline = deadline
        .checked_sub(AUTH_STOP_RESERVE)
        .unwrap_or(deadline)
        .max(started);
    let drained = tokio::time::timeout_at(http_deadline.into(), &mut serving)
        .await
        .is_ok();
    if !drained {
        log::warning(
            "server",
            "shutdown: requests still open at the deadline were closed",
        );
    }
    let (drain_done, deliveries_done) = tokio::join!(
        drain.finish(http_deadline),
        deliveries.finish(http_deadline)
    );
    if !(drain_done && deliveries_done) {
        log::warning(
            "server",
            "shutdown: a background pass was still running; its leases expire and another instance retries",
        );
    }
    if let Some(child) = worker.client().auth_child() {
        let grace = deadline
            .saturating_duration_since(Instant::now())
            .min(AUTH_STOP_RESERVE);
        child.stop(grace).await;
    }
    drop(worker);
    log::info(
        "server",
        &format!(
            "stopped in {} ms",
            Instant::now().duration_since(started).as_millis()
        ),
    );
    Ok(())
}

/// Resolves on the first SIGTERM or SIGINT; a second one exits at once.
fn shutdown_signal() -> Result<impl std::future::Future<Output = ()>, Error> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).map_err(|_| Error("server.signals"))?;
    let mut int = signal(SignalKind::interrupt()).map_err(|_| Error("server.signals"))?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        tokio::spawn(async move {
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            log::warning("server", "second signal: exiting now");
            std::process::exit(1);
        });
    })
}
