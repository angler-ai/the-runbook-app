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
    rc::Rc,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread::ThreadId,
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_postgres::Client;

/// Pooled connections per process when `RUNBOOK_DATABASE_POOL_SIZE` is unset.
pub const DEFAULT_POOL_SIZE: usize = 16;
/// How long a caller waits for a slot before `postgres.pool_timeout`.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// The process-wide part, shared by every clone of one config.
pub(crate) struct Shared {
    size: usize,
    /// One permit per open pooled connection, idle or in use.
    slots: Arc<Semaphore>,
    state: Mutex<State>,
    tls: Mutex<Option<native_tls::TlsConnector>>,
}
#[derive(Default)]
struct State {
    idle: Vec<Idle>,
    /// Callers queued for a slot; a released connection then closes
    /// instead of idling, so its slot goes to them.
    waiting: usize,
}
struct Idle {
    thread: ThreadId,
    store: PostgresStore,
    slot: OwnedSemaphorePermit,
}
impl Shared {
    pub(crate) fn new(size: usize) -> Arc<Self> {
        Arc::new(Self {
            size,
            slots: Arc::new(Semaphore::new(size)),
            state: Mutex::default(),
            tls: Mutex::default(),
        })
    }
    pub(crate) fn size(&self) -> usize {
        self.size
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
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
        let deadline = tokio::time::Instant::now() + ACQUIRE_TIMEOUT;
        let slot = loop {
            let evicted;
            {
                let mut state = shared.state();
                state.idle.retain(|idle| !idle.store.client().is_closed());
                if let Some(at) = state.idle.iter().rposition(|idle| idle.thread == thread) {
                    let idle = state.idle.swap_remove(at);
                    return Ok(Conn {
                        store: Some(idle.store),
                        slot: Some(idle.slot),
                        thread,
                        pool: self.clone(),
                        reusable: true,
                    });
                }
                if let Ok(slot) = shared.slots.clone().try_acquire_owned() {
                    break slot;
                }
                if state.idle.is_empty() {
                    state.waiting += 1;
                    evicted = None;
                } else {
                    // Another thread's idle connection: close it for its slot.
                    evicted = Some(state.idle.remove(0));
                }
            }
            if evicted.is_some() {
                drop(evicted);
                continue;
            }
            let acquired =
                tokio::time::timeout_at(deadline, shared.slots.clone().acquire_owned()).await;
            shared.state().waiting -= 1;
            match acquired {
                Ok(Ok(slot)) => break slot,
                Ok(Err(_)) | Err(_) => return Err(Error("postgres.pool_timeout")),
            }
        };
        let store =
            PostgresStore::connect_tls(&self.config, shared.tls(self.config.tls())?).await?;
        Ok(Conn {
            store: Some(store),
            slot: Some(slot),
            thread,
            pool: self.clone(),
            reusable: true,
        })
    }
}

pub(crate) struct Conn {
    store: Option<PostgresStore>,
    slot: Option<OwnedSemaphorePermit>,
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
        let (Some(store), Some(slot)) = (self.store.take(), self.slot.take()) else {
            return;
        };
        if !self.reusable || store.client().is_closed() {
            return;
        }
        let shared = self.pool.config.pool();
        let mut state = shared.state();
        if state.waiting == 0 {
            state.idle.push(Idle {
                thread: self.thread,
                store,
                slot,
            });
            return;
        }
        drop(state);
        // Close first, then hand the slot to a waiter.
        drop(store);
        drop(slot);
    }
}
