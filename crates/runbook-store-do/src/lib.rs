//! CF2: the ExecutionDO store (plan 2026-11-12 §2.6, D56).
//!
//! One Durable Object per execution holds that execution's events and
//! projection, step runs, claims and leases, execution credentials,
//! idempotency and actor-replay rows, the notification and derived-row
//! outboxes, and its timers as the object's single alarm. This crate is
//! that store as plain Rust over the small [`DoSql`] trait, so the logic is
//! target-independent: Workers runs it over `ctx.storage` ([`workers`],
//! wasm32 only) and native tests over SQLite.
//!
//! Implemented `runbook-core` traits: [`ExecutionStore`],
//! [`SessionStore`]/[`ExecutionSession`], [`ClaimStore`],
//! [`ExecutionCredentialStore`] and [`WorkQueue`] (this execution's timers,
//! notifications, activation and claim deadlines). Execution-local start
//! ([`StartInit`]) and the outbox drain ([`OutboxStream`]) are inherent
//! methods; D1 composes them into the cross-execution `StartStore` and the
//! outbox applier (CF3).
//!
//! Out of scope, by design (D56): the catalog, runs list, admission pools,
//! start replays, directory, step-run lookup by ID and anything else that
//! spans executions. Those live in D1 (CF3) behind their existing traits.
//!
//! [`ExecutionStore`]: runbook_core::ExecutionStore
//! [`SessionStore`]: runbook_core::session::SessionStore
//! [`ExecutionSession`]: runbook_core::session::ExecutionSession
//! [`ClaimStore`]: runbook_core::claims::ClaimStore
//! [`ExecutionCredentialStore`]: runbook_core::execution_credentials::ExecutionCredentialStore
//! [`WorkQueue`]: runbook_core::work_queue::WorkQueue
mod claims;
mod commit;
mod credentials;
mod export;
mod gate;
pub mod logs;
pub mod migrate;
pub mod org;
mod outbox;
pub mod portable;
mod queue;
mod rows;
pub mod sql;
mod start;
pub mod webhook;
#[cfg(target_arch = "wasm32")]
pub mod workers;

pub use commit::DoSession;
pub use export::{Fence, FenceRef};
pub use outbox::{
    NotificationCursor, OUTBOX_FIRST_RETRY_MS, OUTBOX_MAX_RETRY_MS, OUTBOX_MIN_RETRY_MS,
    OutboxStream,
};
pub use queue::MAX_ALARM_MS;
pub use queue::next_alarm_at;
pub use sql::{DoSql, Row, Value};
pub use start::StartInit;

use runbook_core::{StoreError, WriteStage, catalog::TenantScope};
use runbook_model::ExecutionId;
use sql::Columns;
use std::{cell::Cell, rc::Rc};

/// Test seams for the atomicity gates (CF1, conformance). A host enables
/// them only for local tests (the Worker's `CF1_FAULT_INJECTION` var).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Fail with an injected error right after this write stage.
    After(WriteStage),
    /// A real SQLite PRIMARY KEY failure at the outbox stage.
    SqlConstraint,
    /// Fail after the alarm was set inside the transaction.
    AfterAlarm,
    /// A Rust panic (a wasm trap on Workers) mid-commit.
    Panic,
}

/// One Durable Object instance's storage and the state that lives as long
/// as the instance: the name it is bound to and whether its schema is
/// current. A host builds one per object instance (in `DurableObject::new`)
/// and opens a [`DoStore`] per request from it.
#[derive(Clone)]
pub struct DoObject<B: DoSql> {
    sql: B,
    state: Rc<ObjectState>,
}
#[derive(Default)]
pub(crate) struct ObjectState {
    name: std::cell::RefCell<Option<ExecutionId>>,
    /// The schema was checked (and brought up to date) by this instance.
    migrated: Cell<bool>,
    /// One open session at a time (see [`gate`]).
    pub(crate) gate: gate::Gate,
}

impl<B: DoSql> DoObject<B> {
    pub fn new(sql: B) -> Self {
        Self {
            sql,
            state: Rc::default(),
        }
    }
    /// Check and apply migrations once per instance: a DO instance is the
    /// only writer of its storage, so nothing changes the schema under it.
    fn ensure_schema(&self) -> Result<(), StoreError> {
        if !self.state.migrated.get() {
            migrate::migrate(&self.sql)?;
            self.state.migrated.set(true);
        }
        Ok(())
    }
    /// A tenant-bound store on this object, which is named `name` (the
    /// execution ID the host addressed it by, `idFromName`).
    ///
    /// The first call binds the instance to `name`; a later call with
    /// another name, or an object whose head is another execution, is
    /// refused (`do.object_mismatch`): a misrouted request never reads or
    /// writes another run's rows.
    pub fn store(&self, name: &ExecutionId, scope: TenantScope) -> Result<DoStore<B>, StoreError> {
        let mismatch = StoreError::Backend("do.object_mismatch");
        if self
            .state
            .name
            .borrow()
            .as_ref()
            .is_some_and(|bound| bound != name)
        {
            return Err(mismatch);
        }
        self.ensure_schema()?;
        let store = DoStore {
            sql: self.sql.clone(),
            name: name.clone(),
            scope,
            object: self.state.clone(),
            fault: Rc::default(),
        };
        if store.head()?.is_some_and(|head| &head.execution_id != name) {
            return Err(mismatch);
        }
        *self.state.name.borrow_mut() = Some(name.clone());
        Ok(store)
    }
    /// The execution this object holds, from its head (an alarm has no
    /// request to name it). `None` before the first write.
    pub fn stored_name(&self) -> Result<Option<ExecutionId>, StoreError> {
        self.ensure_schema()?;
        let rows = self
            .sql
            .query("SELECT execution_id FROM meta WHERE id = 1", &[])?;
        rows.first()
            .map(|row| {
                row.text(0)?
                    .parse()
                    .map_err(|_| StoreError::Backend("do.invalid_stored_id"))
            })
            .transpose()
    }
    /// The run this object holds and the tenant that owns it, from its own
    /// head (CF3: the alarm needs no D1 read to know its tenant).
    pub fn stored_owner(&self) -> Result<Option<(ExecutionId, TenantScope)>, StoreError> {
        self.ensure_schema()?;
        let rows = self.sql.query(
            "SELECT execution_id, org_id, project_id FROM meta WHERE id = 1",
            &[],
        )?;
        let bad = |_| StoreError::Backend("do.invalid_stored_id");
        rows.first()
            .map(|row| {
                Ok((
                    row.text(0)?.parse().map_err(bad)?,
                    TenantScope {
                        org_id: row.text(1)?.parse().map_err(bad)?,
                        project_id: row.text(2)?.parse().map_err(bad)?,
                    },
                ))
            })
            .transpose()
    }
    /// Test seam: empty every table and the alarm. The schema stays.
    #[doc(hidden)]
    pub fn reset(&self) -> Result<(), StoreError> {
        self.ensure_schema()?;
        reset(&self.sql)
    }
}

/// A tenant-bound handle on one ExecutionDO's storage. Clones share the
/// backend; `scope` comes only from a verified credential (§18), and every
/// read and write is refused unless it matches the object's owner.
#[derive(Clone)]
pub struct DoStore<B: DoSql> {
    sql: B,
    name: ExecutionId,
    scope: TenantScope,
    pub(crate) object: Rc<ObjectState>,
    fault: Rc<Cell<Option<Fault>>>,
}

/// The head row: who owns this object and how far it has committed.
pub(crate) struct Head {
    pub(crate) scope: TenantScope,
    pub(crate) execution_id: ExecutionId,
    pub(crate) revision: u64,
    /// The session fence: bumped by every write a session reads (0002).
    pub(crate) writes: u64,
}

impl<B: DoSql> DoStore<B> {
    /// Open a store on a fresh object instance named `name` (tests, and
    /// hosts that keep no per-instance state; see [`DoObject`]).
    pub fn open(sql: B, name: &ExecutionId, scope: TenantScope) -> Result<Self, StoreError> {
        DoObject::new(sql).store(name, scope)
    }
    /// The execution this object is named for.
    pub fn name(&self) -> &ExecutionId {
        &self.name
    }
    /// Refuse a write for any execution but this object's own.
    pub(crate) fn named(&self, id: &ExecutionId) -> Result<(), StoreError> {
        if id == &self.name {
            Ok(())
        } else {
            Err(StoreError::InvalidBatch("do.object_mismatch"))
        }
    }
    /// The same storage seen by another tenant (every read still filters).
    pub fn scoped(&self, scope: TenantScope) -> Self {
        Self {
            scope,
            ..self.clone()
        }
    }
    pub fn scope(&self) -> &TenantScope {
        &self.scope
    }
    pub fn backend(&self) -> &B {
        &self.sql
    }
    /// The store clock (the object's request time on Workers).
    pub fn now_ms(&self) -> i64 {
        self.sql.now_ms()
    }
    #[doc(hidden)]
    pub fn inject(&self, fault: Fault) {
        self.fault.set(Some(fault));
    }
    #[doc(hidden)]
    pub fn fail_after(&self, stage: WriteStage) {
        self.inject(Fault::After(stage));
    }
    pub(crate) fn checkpoint(&self, stage: WriteStage) -> Result<(), StoreError> {
        if self.fault.get() == Some(Fault::After(stage)) {
            self.fault.set(None);
            return Err(StoreError::Backend("injected"));
        }
        Ok(())
    }
    pub(crate) fn take_fault(&self, fault: Fault) -> bool {
        let hit = self.fault.get() == Some(fault);
        if hit {
            self.fault.set(None);
        }
        hit
    }

    pub(crate) fn head(&self) -> Result<Option<Head>, StoreError> {
        let rows = self.sql.query(
            "SELECT org_id, project_id, execution_id, revision, writes FROM meta WHERE id = 1",
            &[],
        )?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let parse = |index: usize| -> Result<&str, StoreError> { row.text(index) };
        let bad = |_| StoreError::Backend("do.invalid_stored_id");
        Ok(Some(Head {
            scope: TenantScope {
                org_id: parse(0)?.parse().map_err(bad)?,
                project_id: parse(1)?.parse().map_err(bad)?,
            },
            execution_id: parse(2)?.parse().map_err(bad)?,
            revision: row.unsigned(3)?,
            writes: row.unsigned(4)?,
        }))
    }
    /// The head, only when this handle's tenant owns it and it is `id`.
    pub(crate) fn owned_head(&self, id: &ExecutionId) -> Result<Head, StoreError> {
        match self.head()? {
            Some(head) if head.scope == self.scope && &head.execution_id == id => Ok(head),
            _ => Err(StoreError::NotFound),
        }
    }
    /// The execution this object holds, when this handle's tenant owns it.
    pub fn current_execution(&self) -> Result<Option<ExecutionId>, StoreError> {
        Ok(self.own_head()?.map(|head| head.execution_id))
    }
    /// The head, only when this handle's tenant owns it.
    pub(crate) fn own_head(&self) -> Result<Option<Head>, StoreError> {
        Ok(self.head()?.filter(|head| head.scope == self.scope))
    }
    /// The (org_id, project_id, execution_id) every run-owned row leads with.
    pub(crate) fn tenant(&self, id: &ExecutionId) -> [Value; 3] {
        [
            self.scope.org_id.as_str().into(),
            self.scope.project_id.as_str().into(),
            id.as_str().into(),
        ]
    }
    /// Move the session fence: a session begun before this write conflicts.
    pub(crate) fn bump_writes(&self) -> Result<(), StoreError> {
        self.sql
            .exec("UPDATE meta SET writes = writes + 1 WHERE id = 1", &[])
    }
    /// Tenant columns followed by `rest`, for an INSERT that leads with them.
    pub(crate) fn owned(&self, id: &ExecutionId, rest: Vec<Value>) -> Vec<Value> {
        let mut params = self.tenant(id).to_vec();
        params.extend(rest);
        params
    }

    /// Run `body` in one storage transaction on a clone of this handle,
    /// then re-arm the alarm from what the transaction left behind.
    pub(crate) fn write<T: 'static>(
        &self,
        body: impl FnOnce(&Self) -> Result<T, StoreError> + 'static,
    ) -> Result<T, StoreError> {
        let store = self.clone();
        self.sql.transaction(move || {
            let value = body(&store)?;
            queue::rearm(&store)?;
            Ok(value)
        })
    }

    /// Test seam: empty every table (and the alarm) so one object can host
    /// a fresh conformance store. The schema and its history stay.
    #[doc(hidden)]
    pub fn reset(&self) -> Result<(), StoreError> {
        reset(&self.sql)
    }
    /// Test inventory: rows per table, unfiltered.
    #[doc(hidden)]
    pub fn table_counts(&self) -> Result<Vec<(&'static str, u64)>, StoreError> {
        TABLES
            .iter()
            .map(|table| {
                let rows = self
                    .sql
                    .query(&format!("SELECT COUNT(*) FROM {table}"), &[])?;
                let count = rows.first().ok_or(StoreError::Backend("do.decode"))?;
                Ok((*table, count.unsigned(0)?))
            })
            .collect()
    }
}

/// Empty every table (and the alarm) in one transaction.
fn reset<B: DoSql>(sql: &B) -> Result<(), StoreError> {
    let backend = sql.clone();
    sql.transaction(move || {
        for table in TABLES {
            backend.exec(&format!("DELETE FROM {table}"), &[])?;
        }
        backend.set_alarm(None)
    })
}

/// Every table the migrations create, children before parents (for DELETE order).
pub const TABLES: &[&str] = &[
    "meta",
    "event",
    "step_run",
    "timer",
    "timer_history",
    "outbox",
    "derived_outbox",
    "derived_assignment",
    "outbox_retry",
    "idempotency",
    "actor_replay",
    "claim",
    "claim_log",
    "log_chunk",
    "start_record",
    "work_lease",
    "execution_credential",
    "credential_family",
    "alarm_log",
    "import_hold",
];
