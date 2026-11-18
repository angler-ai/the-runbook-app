//! `DoSql` over an in-memory SQLite database (rusqlite, bundled), standing
//! in for one Durable Object's storage in native tests. Like the DO, the
//! alarm is transactional: one set inside a transaction lands only on
//! commit. The clock is a settable test clock.
use crate::host::Host;
use runbook_core::StoreError;
use runbook_store_do::{DoSql, Row, Value};
use rusqlite::{Connection, types::ValueRef};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

struct Inner {
    connection: Connection,
    clock: Cell<i64>,
    alarm: Cell<Option<i64>>,
    staged_alarm: Cell<Option<Option<i64>>>,
    in_transaction: Cell<bool>,
    /// Every alarm value committed, in order (for alarm-mapping tests).
    alarm_history: RefCell<Vec<Option<i64>>>,
}

#[derive(Clone)]
pub struct NativeSql(Rc<Inner>);

impl NativeSql {
    pub fn new(now_ms: i64) -> Self {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        // CF12b: Durable Object SQLite holds at most 2,000,000 bytes in a
        // string, a BLOB or a row (as D1); so does this stand-in.
        connection
            .set_limit(
                rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
                i32::try_from(runbook_store_d1::sql::D1_MAX_LENGTH).expect("fits"),
            )
            .expect("limit");
        // Durable Object SQLite enforces foreign keys; so does this stand-in.
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("pragma");
        Self(Rc::new(Inner {
            connection,
            clock: Cell::new(now_ms),
            alarm: Cell::new(None),
            staged_alarm: Cell::new(None),
            in_transaction: Cell::new(false),
            alarm_history: RefCell::default(),
        }))
    }
    pub fn set_time(&self, now_ms: i64) {
        self.0.clock.set(now_ms);
    }
}
/// A fresh database at time 0 (a WebhookDO's log, which takes its clock
/// from its caller).
impl Default for NativeSql {
    fn default() -> Self {
        Self::new(0)
    }
}
impl NativeSql {
    pub fn advance(&self, by_ms: i64) {
        self.0.clock.set(self.0.clock.get() + by_ms);
    }
    pub fn alarm(&self) -> Option<i64> {
        self.0.alarm.get()
    }
    pub fn alarm_history(&self) -> Vec<Option<i64>> {
        self.0.alarm_history.borrow().clone()
    }
}

fn backend(code: &'static str) -> impl Fn(rusqlite::Error) -> StoreError {
    move |_| StoreError::Backend(code)
}
fn params(values: &[Value]) -> Vec<rusqlite::types::Value> {
    values
        .iter()
        .map(|value| match value {
            Value::Null => rusqlite::types::Value::Null,
            Value::Integer(number) => rusqlite::types::Value::Integer(*number),
            Value::Text(text) => rusqlite::types::Value::Text(text.clone()),
            Value::Blob(bytes) => rusqlite::types::Value::Blob(bytes.clone()),
        })
        .collect()
}

/// Rolls back an open transaction on drop (an `Err` or a panic in the body).
struct Rollback<'a>(&'a Inner, bool);
impl Drop for Rollback<'_> {
    fn drop(&mut self) {
        if !self.1 {
            let _ = self.0.connection.execute_batch("ROLLBACK");
            self.0.staged_alarm.set(None);
            self.0.in_transaction.set(false);
        }
    }
}

impl DoSql for NativeSql {
    fn exec(&self, sql: &str, values: &[Value]) -> Result<(), StoreError> {
        self.0
            .connection
            .execute(sql, rusqlite::params_from_iter(params(values)))
            .map(|_| ())
            .map_err(backend("do.write"))
    }
    fn query(&self, sql: &str, values: &[Value]) -> Result<Vec<Row>, StoreError> {
        let mut statement = self.0.connection.prepare(sql).map_err(backend("do.read"))?;
        let width = statement.column_count();
        let mut rows = statement
            .query(rusqlite::params_from_iter(params(values)))
            .map_err(backend("do.read"))?;
        let mut out = vec![];
        while let Some(row) = rows.next().map_err(backend("do.read"))? {
            let mut values = Vec::with_capacity(width);
            for index in 0..width {
                values.push(match row.get_ref(index).map_err(backend("do.read"))? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(number) => Value::Integer(number),
                    ValueRef::Real(_) => return Err(StoreError::Backend("do.real_column")),
                    ValueRef::Text(bytes) => Value::Text(
                        String::from_utf8(bytes.to_vec())
                            .map_err(|_| StoreError::Backend("do.decode"))?,
                    ),
                    ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
                });
            }
            out.push(values);
        }
        Ok(out)
    }
    fn exec_script(&self, sql: &str) -> Result<(), StoreError> {
        self.0
            .connection
            .execute_batch(sql)
            .map_err(backend("do.write"))
    }
    fn transaction<T: 'static>(
        &self,
        body: impl FnOnce() -> Result<T, StoreError> + 'static,
    ) -> Result<T, StoreError> {
        assert!(
            !self.0.in_transaction.get(),
            "DoSql transactions never nest"
        );
        self.0
            .connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(backend("do.begin"))?;
        self.0.in_transaction.set(true);
        let mut guard = Rollback(&self.0, false);
        let value = body()?;
        self.0
            .connection
            .execute_batch("COMMIT")
            .map_err(backend("do.commit"))?;
        guard.1 = true;
        self.0.in_transaction.set(false);
        if let Some(alarm) = self.0.staged_alarm.take() {
            self.0.alarm.set(alarm);
            self.0.alarm_history.borrow_mut().push(alarm);
        }
        Ok(value)
    }
    fn now_ms(&self) -> i64 {
        self.0.clock.get()
    }
    fn set_alarm(&self, at_ms: Option<i64>) -> Result<(), StoreError> {
        if self.0.in_transaction.get() {
            self.0.staged_alarm.set(Some(at_ms));
        } else {
            self.0.alarm.set(at_ms);
            self.0.alarm_history.borrow_mut().push(at_ms);
        }
        Ok(())
    }
}

/// The native test host: one in-memory database per fresh object.
pub struct Native;
pub const START_MS: i64 = 10_000;
impl Host for Native {
    type Sql = NativeSql;
    fn fresh(&self) -> Result<NativeSql, StoreError> {
        Ok(NativeSql::new(START_MS))
    }
    fn start_ms(&self) -> i64 {
        START_MS
    }
    fn set_time(&self, sql: &NativeSql, now_ms: i64) {
        sql.set_time(now_ms);
    }
    async fn alarm(&self, sql: &NativeSql) -> Option<i64> {
        sql.alarm()
    }
    type Bucket = runbook_store_r2::MemoryBucket;
    async fn buckets(&self) -> Result<(Self::Bucket, Self::Bucket), StoreError> {
        Ok(Default::default())
    }
    type D1 = runbook_store_d1::native::NativeD1;
    async fn d1(&self) -> Result<Self::D1, StoreError> {
        let db = runbook_store_d1::native::NativeD1::new(START_MS);
        crate::host::fresh_d1(&db).await?;
        Ok(db)
    }
    type Webhooks = runbook_worker::webhooks::LocalWebhooks<NativeSql>;
    fn webhooks(&self) -> Self::Webhooks {
        runbook_worker::webhooks::LocalWebhooks::default()
    }
}

/// Drive a store future to completion: every DO store future is
/// synchronous underneath, so one poll must finish it.
pub fn run<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("a DO store future yielded"),
    }
}
