//! One bounded connection pool per [`DatabaseConfig`] (F32): every store
//! built from a clone of the same config shares it, so a process holds at
//! most `pool_size` pooled connections however many handles it builds.
//!
//! A connection that may hold an open transaction is discarded on drop
//! instead of reused, so the server rolls it back and its slot frees.
//! Each connection's I/O task runs on the runtime that opened it, so an
//! idle connection is reused only on the thread that opened it; another
//! thread that finds no free slot closes an idle one to take its slot.
use crate::{DatabaseConfig, Error, PostgresStore};
use std::{
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread::ThreadId,
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};
use tokio_postgres::Client;

/// Pooled connections per process when `RUNBOOK_DATABASE_POOL_SIZE` is unset.
pub const DEFAULT_POOL_SIZE: usize = 16;
/// Bound both queueing and starting a fully initialized connection.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// A connected socket can outlive the underlying database or proxy session.
const REUSE_TIMEOUT: Duration = Duration::from_millis(750);

/// Counts for one core pool. Contains no connection or tenant details.
#[derive(Debug)]
pub struct PoolStats {
    pub max: usize,
    pub in_use: usize,
    pub idle: usize,
    pub waiting: usize,
    pub opened: u64,
    pub failures: BTreeMap<&'static str, u64>,
}

/// The process-wide part, shared by every clone of one config.
pub(crate) struct Shared {
    size: usize,
    state: Mutex<State>,
    available: Notify,
    tls: Mutex<Option<native_tls::TlsConnector>>,
}
#[derive(Default)]
struct State {
    idle: Vec<Idle>,
    in_use: usize,
    waiting: usize,
    opened: u64,
    failures: BTreeMap<&'static str, u64>,
}
// A checkout may be cancelled by the HTTP deadline or a disconnect.
struct Waiter<'a>(&'a Shared);
impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        self.0.state().waiting -= 1;
    }
}

struct Idle {
    thread: ThreadId,
    store: PostgresStore,
}
impl Shared {
    pub(crate) fn new(size: usize) -> Arc<Self> {
        Arc::new(Self {
            size,
            state: Mutex::default(),
            available: Notify::new(),
            tls: Mutex::default(),
        })
    }
    pub(crate) fn size(&self) -> usize {
        self.size
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(crate) fn stats(&self) -> PoolStats {
        let state = self.state();
        PoolStats {
            max: self.size,
            in_use: state.in_use,
            idle: state.idle.len(),
            waiting: state.waiting,
            opened: state.opened,
            failures: state.failures.clone(),
        }
    }
    pub(crate) fn failure(&self, cause: &'static str) {
        *self.state().failures.entry(cause).or_default() += 1;
    }
    /// The connector for the pool's endpoint; every config sharing this
    /// pool has the same TLS policy (another endpoint starts a new pool).
    fn tls(&self, policy: &crate::TlsPolicy) -> Result<native_tls::TlsConnector, Error> {
        let mut slot = self.tls.lock().unwrap_or_else(PoisonError::into_inner);
        match &*slot {
            Some(tls) => Ok(tls.clone()),
            None => {
                let tls = policy.connector()?;
                *slot = Some(tls.clone());
                Ok(tls)
            }
        }
    }
}

/// A store's handle on its config's shared pool.
pub(crate) struct Pool {
    config: DatabaseConfig,
}
impl Pool {
    pub(crate) fn new(config: DatabaseConfig) -> Rc<Self> {
        Rc::new(Self { config })
    }
    pub(crate) async fn get(self: &Rc<Self>) -> Result<Conn, Error> {
        let shared = self.config.pool();
        let thread = std::thread::current().id();
        let deadline = Instant::now() + ACQUIRE_TIMEOUT;
        loop {
            // Register before observing the state so a release between
            // inspecting it and parking cannot be missed.
            let available = shared.available.notified();
            let checkout = {
                let mut state = shared.state();
                let before = state.idle.len();
                state.idle.retain(|idle| !idle.store.client().is_closed());
                let closed = before - state.idle.len();
                if closed > 0 {
                    *state.failures.entry("idle_closed").or_default() += closed as u64;
                    shared.available.notify_waiters();
                }
                let own = state.idle.iter().rposition(|idle| idle.thread == thread);
                if let Some(at) = own {
                    state.in_use += 1;
                    Some(Some(state.idle.swap_remove(at).store))
                } else if state.in_use < shared.size {
                    if state.in_use + state.idle.len() == shared.size {
                        // All unused slots belong to other runtimes. Retain
                        // their idle connections until this slot is needed.
                        state.idle.swap_remove(0);
                    }
                    state.in_use += 1;
                    Some(None)
                } else {
                    state.waiting += 1;
                    None
                }
            };
            let Some(store) = checkout else {
                let waiting = Waiter(shared);
                let result = tokio::time::timeout_at(deadline, available).await;
                drop(waiting);
                if result.is_err() {
                    shared.failure("queue_timeout");
                    return Err(Error("postgres.pool_timeout"));
                }
                continue;
            };
            // Own the slot before the first await, including connection
            // establishment, so cancelling the checkout always releases it.
            let mut conn = Conn {
                store,
                thread,
                pool: self.clone(),
                reusable: false,
            };
            if conn.store.is_some() {
                let check_deadline = deadline.min(Instant::now() + REUSE_TIMEOUT);
                match tokio::time::timeout_at(check_deadline, conn.client().check_connection())
                    .await
                {
                    Ok(Ok(())) => {
                        conn.reusable = true;
                        return Ok(conn);
                    }
                    Ok(Err(_)) => shared.failure("idle_check_error"),
                    Err(_) => shared.failure("idle_check_timeout"),
                }
                // Open a replacement using the same reservation. Drop the
                // unresponsive socket before making another connection.
                conn.store = None;
            }
            conn.store = Some(
                match tokio::time::timeout_at(
                    deadline,
                    PostgresStore::connect_tls(&self.config, shared.tls(self.config.tls())?),
                )
                .await
                {
                    Ok(result) => result?,
                    Err(_) => {
                        shared.failure("connect_timeout");
                        return Err(Error("postgres.pool_timeout"));
                    }
                },
            );
            shared.state().opened += 1;
            conn.reusable = true;
            return Ok(conn);
        }
    }
}

pub(crate) struct Conn {
    store: Option<PostgresStore>,
    /// The thread whose runtime drives this connection's I/O.
    thread: ThreadId,
    pool: Rc<Pool>,
    reusable: bool,
}
impl Conn {
    pub(crate) fn client(&self) -> &Client {
        self.store
            .as_ref()
            .expect("connection present until drop")
            .client()
    }
    pub(crate) async fn ping(&mut self) -> Result<(), Error> {
        self.reusable = false;
        self.client()
            .query_one("SELECT 1", &[])
            .await
            .map_err(|error| {
                self.pool.config.pool().failure("ping_error");
                Error::from(error)
            })?;
        self.reusable = true;
        Ok(())
    }
    pub(crate) async fn begin(&mut self, statement: &str) -> Result<(), tokio_postgres::Error> {
        self.reusable = false;
        self.client().batch_execute(statement).await
    }
    pub(crate) async fn commit(&mut self) -> Result<(), tokio_postgres::Error> {
        self.client().batch_execute("COMMIT").await?;
        self.reusable = true;
        Ok(())
    }
    pub(crate) async fn rollback(&mut self) {
        if self.client().batch_execute("ROLLBACK").await.is_ok() {
            self.reusable = true;
        }
    }
}
impl Drop for Conn {
    fn drop(&mut self) {
        let shared = self.pool.config.pool();
        let mut state = shared.state();
        state.in_use -= 1;
        if let Some(store) = self.store.take()
            && self.reusable
            && !store.client().is_closed()
        {
            state.idle.push(Idle {
                thread: self.thread,
                store,
            });
        }
        drop(state);
        shared.available.notify_waiters();
    }
}
