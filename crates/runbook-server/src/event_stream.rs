//! Subscribe before catching up; notifications are never event payloads.
//!
//! F48: every stream shares one LISTEN connection ([`LiveHub`]); a
//! credential holds at most `streams_per_credential` streams, all
//! credentials at most `max_streams`; a stream closes after
//! `stream_lifetime` and the client resumes with `Last-Event-ID`.
use crate::executions::PostgresExecutionBackend;
use runbook_api::{Request, executions::ExecutionService, problem::Response};
use runbook_core::{ExecutionStore, StoreError, retention::RunStore};
use runbook_store_postgres::{
    DatabaseConfig, LiveEvent, LiveEvents, PostgresCredentials, PostgresDirectory,
    PostgresExecutionStore,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot, watch};
pub(crate) type Service =
    ExecutionService<PostgresDirectory, PostgresCredentials, PostgresExecutionBackend>;
pub(crate) type Frames = mpsc::Receiver<Result<Vec<u8>, std::io::Error>>;
pub(crate) struct StreamCall {
    pub request: Request,
    /// AUTH2: the session cookie behind a session credential, so the
    /// stream can ask the auth service again at each recheck (D32).
    pub cookie: Option<String>,
    pub after: u64,
    pub reply: oneshot::Sender<Result<Frames, Response>>,
}
/// How long a new stream waits for the shared LISTEN connection.
const LISTEN_WAIT: Duration = Duration::from_secs(5);

/// One LISTEN connection for every stream of this executor. It connects
/// on first use and reconnects after a failure; streams that miss a hint
/// while it reconnects catch up at their periodic recheck.
pub(crate) struct LiveHub {
    config: DatabaseConfig,
    hints: broadcast::Sender<LiveEvent>,
    connected: watch::Sender<bool>,
    started: Cell<bool>,
}
impl LiveHub {
    pub(crate) fn new(config: DatabaseConfig) -> Rc<Self> {
        Rc::new(Self {
            config,
            hints: broadcast::channel(1024).0,
            connected: watch::channel(false).0,
            started: Cell::new(false),
        })
    }
    /// Subscribe first, then wait for the connection, so no hint between
    /// this call and the caller's catch-up read is lost.
    async fn subscribe(self: &Rc<Self>) -> Option<broadcast::Receiver<LiveEvent>> {
        if !self.started.replace(true) {
            tokio::task::spawn_local(self.clone().run());
        }
        let hints = self.hints.subscribe();
        let mut connected = self.connected.subscribe();
        tokio::time::timeout(LISTEN_WAIT, connected.wait_for(|up| *up))
            .await
            .ok()?
            .ok()?;
        Some(hints)
    }
    async fn run(self: Rc<Self>) {
        loop {
            if let Ok(mut live) = LiveEvents::listen(&self.config).await {
                self.connected.send_replace(true);
                while let Ok(hint) = live.next().await {
                    let _ = self.hints.send(hint);
                }
                self.connected.send_replace(false);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

/// Stream admission: the global and per-credential caps, and the lifetime.
pub(crate) struct Streams {
    pub service: Service,
    pub hub: Rc<LiveHub>,
    pub global: Arc<Semaphore>,
    pub per_credential: usize,
    pub lifetime: Duration,
    /// D32: the longest gap between credential rechecks.
    pub recheck: Duration,
    pub sessions: Option<Arc<crate::session::Sessions>>,
    /// CF0: session linking runs through the portable router's backends.
    pub backends: crate::backends::PostgresBackends,
    held: Rc<RefCell<HashMap<[u8; 32], usize>>>,
}
/// A held stream slot; dropping it frees both caps.
pub(crate) struct Slot {
    _permit: OwnedSemaphorePermit,
    held: Rc<RefCell<HashMap<[u8; 32], usize>>>,
    key: [u8; 32],
}
impl Drop for Slot {
    fn drop(&mut self) {
        let mut held = self.held.borrow_mut();
        if let Some(n) = held.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                held.remove(&self.key);
            }
        }
    }
}
impl Streams {
    pub(crate) fn new(
        service: Service,
        config: DatabaseConfig,
        limits: &crate::config::TransportLimits,
        sessions: Option<Arc<crate::session::Sessions>>,
        backends: crate::backends::PostgresBackends,
    ) -> Self {
        Self {
            service,
            hub: LiveHub::new(config),
            global: Arc::new(Semaphore::new(limits.max_streams)),
            per_credential: limits.streams_per_credential,
            lifetime: limits.stream_lifetime,
            recheck: limits.stream_recheck,
            sessions,
            backends,
            held: Rc::default(),
        }
    }
    /// Admit or refuse at once; admitted streams run on this executor.
    pub(crate) fn admit(&self, call: StreamCall) {
        // Keyed by the presented secret's (or session cookie's) hash; a bad
        // one fails auth straight away and gives its slot back.
        let key = call
            .request
            .bearer()
            .or(call.cookie.as_deref())
            .map_or([0; 32], |secret| {
                *runbook_core::credentials::secret_hash(secret).as_bytes()
            });
        let id = call.request.request_id.clone();
        if self.held.borrow().get(&key).copied().unwrap_or(0) >= self.per_credential {
            let _ = call
                .reply
                .send(Err(Response::error(429, "stream.too_many", &id)));
            return;
        }
        let Ok(permit) = self.global.clone().try_acquire_owned() else {
            let _ = call
                .reply
                .send(Err(Response::error(503, "stream.capacity", &id)));
            return;
        };
        *self.held.borrow_mut().entry(key).or_default() += 1;
        let slot = Slot {
            _permit: permit,
            held: self.held.clone(),
            key,
        };
        tokio::task::spawn_local(serve(
            call,
            self.service.clone(),
            self.hub.clone(),
            (self.lifetime, self.recheck),
            self.sessions.clone(),
            self.backends.clone(),
            slot,
        ));
    }
}

/// AUTH2 (D32, D44): a session-authenticated stream asks the auth service
/// again (through the 30 s cache) and rebuilds its principal from the
/// directory; a signed-out or unlinked session ends the stream. Bearer
/// streams pass through unchanged.
async fn refresh_session(
    request: &mut Request,
    cookie: Option<&str>,
    sessions: Option<&crate::session::Sessions>,
    backends: &crate::backends::PostgresBackends,
) -> Result<(), (u16, &'static str)> {
    let Some(cookie) = cookie else {
        return Ok(());
    };
    let sessions = sessions.ok_or((401, "auth.required"))?;
    let mut session = match sessions.resolve(cookie).await {
        Ok(Some(session)) => session,
        Ok(None) => return Err((401, "auth.required")),
        Err(_) => return Err((503, "auth.unavailable")),
    };
    runbook_api::session::link(backends, &mut session).await?;
    request.credential = Some(runbook_api::Credential::Session(session));
    Ok(())
}

async fn serve(
    mut call: StreamCall,
    service: Service,
    hub: Rc<LiveHub>,
    (lifetime, recheck): (Duration, Duration),
    sessions: Option<Arc<crate::session::Sessions>>,
    backends: crate::backends::PostgresBackends,
    _slot: Slot,
) {
    let started = Instant::now();
    let id = call.request.request_id.clone();
    let fail = |status, code| Response::error(status, code, &id);
    if runbook_api::execution_access::is_execution(&call.request) {
        let _ = call.reply.send(Err(fail(403, "auth.forbidden")));
        return;
    }
    let cookie = call.cookie.take();
    if let Err((status, code)) = refresh_session(
        &mut call.request,
        cookie.as_deref(),
        sessions.as_deref(),
        &backends,
    )
    .await
    {
        let _ = call.reply.send(Err(fail(status, code)));
        return;
    }
    let (scope, execution) = match service.authorize_events(&call.request).await {
        Ok(v) => v,
        Err(e) => {
            let _ = call.reply.send(Err(fail(e.status, e.code)));
            return;
        }
    };
    let Some(mut live) = hub.subscribe().await else {
        let _ = call.reply.send(Err(fail(503, "stream.unavailable")));
        return;
    };
    let store = runbook_api::backends::Backends::execution(&backends, scope.clone());
    let mut run = match load(&store, &execution, call.after).await {
        Ok(v) => v,
        Err((s, c)) => {
            let _ = call.reply.send(Err(fail(s, c)));
            return;
        }
    };
    let (tx, rx) = mpsc::channel(16);
    if call.reply.send(Ok(rx)).is_err() {
        return;
    }
    let mut sequence = call.after;
    let mut checked = Instant::now();
    loop {
        for event in run
            .events
            .iter()
            .filter(|event| event.seq > sequence)
            .cloned()
            .collect::<Vec<_>>()
        {
            // D32: validate current membership and credential on every event.
            if refresh_session(
                &mut call.request,
                cookie.as_deref(),
                sessions.as_deref(),
                &backends,
            )
            .await
            .is_err()
            {
                return;
            }
            match service.authorize_events(&call.request).await {
                Ok((current, _)) if current == scope => checked = Instant::now(),
                _ => return,
            }
            let Some(frame) = runbook_api::sse::frame(&event) else {
                return;
            };
            let frame = frame.into_bytes();
            // A stalled reader cannot keep a stream authorized indefinitely.
            if !matches!(
                tokio::time::timeout(recheck, tx.send(Ok(frame))).await,
                Ok(Ok(()))
            ) {
                return;
            }
            sequence = event.seq;
            if runbook_api::sse::is_last(&event) {
                return;
            }
        }
        if matches!(
            run.projection.status,
            runbook_model::ExecutionStatus::Succeeded
                | runbook_model::ExecutionStatus::Failed
                | runbook_model::ExecutionStatus::Cancelled
        ) {
            return;
        }
        let Some(remaining) = lifetime.checked_sub(started.elapsed()) else {
            return;
        };
        let wait = recheck.saturating_sub(checked.elapsed()).min(remaining);
        let hint = tokio::select! {_ = tx.closed()=>return,hint=tokio::time::timeout(wait, live.recv())=>hint};
        if started.elapsed() >= lifetime {
            return;
        }
        let recheck_due = checked.elapsed() >= recheck;
        if recheck_due {
            if refresh_session(
                &mut call.request,
                cookie.as_deref(),
                sessions.as_deref(),
                &backends,
            )
            .await
            .is_err()
            {
                return;
            }
            match service.authorize_events(&call.request).await {
                Ok((current, _)) if current == scope => checked = Instant::now(),
                _ => return,
            }
        }
        match hint {
            Ok(Ok(h))
                if !recheck_due && (h.execution_id != execution || h.sequence <= sequence) =>
            {
                continue;
            }
            // The hub is gone: this executor is stopping.
            Ok(Err(broadcast::error::RecvError::Closed)) => return,
            // A hint, missed hints (lagged) or the timeout: read again.
            _ => {}
        }
        run = match load(&store, &execution, sequence).await {
            Ok(run) => run,
            Err((_, code)) => {
                let frame = runbook_api::sse::error_frame(code);
                let _ = tokio::time::timeout(recheck, tx.send(Ok(frame.into_bytes()))).await;
                return;
            }
        };
        if run.revision == sequence
            && tx
                .try_send(Ok(runbook_api::sse::KEEP_ALIVE.as_bytes().to_vec()))
                .is_err()
        {
            return;
        }
    }
}
async fn load(
    store: &PostgresExecutionStore,
    id: &runbook_model::ExecutionId,
    after: u64,
) -> Result<runbook_core::StoredExecution, (u16, &'static str)> {
    let run = match store.load(id).await {
        Ok(run) => run,
        Err(StoreError::NotFound) => {
            return Err(
                if store
                    .tombstone(id)
                    .await
                    .map_err(|_| (503, "storage.unavailable"))?
                    .is_some()
                {
                    (410, "execution.history_expired")
                } else {
                    (404, "resource.not_found")
                },
            );
        }
        Err(_) => return Err((503, "storage.unavailable")),
    };
    runbook_api::sse::check_history(&run.events, run.revision, after)?;
    Ok(run)
}
