//! [`D1`] over an in-memory SQLite database (rusqlite, bundled), standing in
//! for a D1 database in native tests. A batch is one `BEGIN IMMEDIATE …
//! COMMIT`, rolled back on the first error, as D1 does. Foreign keys are
//! enforced, as on D1, and so is D1's 2,000,000-byte limit on a string,
//! BLOB or row (CF12). The clock is a settable test clock.
use crate::sql::{D1, D1_MAX_LENGTH, D1Error, Outcome, Row, Statement, Value, check_params};
use rusqlite::{Connection, types::ValueRef};
use std::{cell::Cell, rc::Rc};

struct Inner {
    connection: Connection,
    clock: Cell<i64>,
    /// Statements run so far (tests assert read shapes).
    statements: Cell<u64>,
    /// Calls made so far (`all`, `run` and `batch` each count one, as D1
    /// counts a batch as one query; UI-1c).
    queries: Cell<u64>,
    /// Rows, and bytes of text and BLOB values, returned so far (what a
    /// Worker would hold; UI-1b's list budget checks they stay flat).
    returned: Cell<(u64, u64)>,
}

/// One database; clones share it.
#[derive(Clone)]
pub struct NativeD1(Rc<Inner>);

impl NativeD1 {
    /// A fresh, empty database (no schema: run [`crate::migrate::migrate`]).
    pub fn new(now_ms: i64) -> Self {
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("pragma");
        // CF12 review round 1: D1's 2,000,000-byte string, BLOB and row
        // limit is SQLite's length limit, which also bounds each record a
        // write builds (a row).
        connection
            .set_limit(
                rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
                i32::try_from(D1_MAX_LENGTH).expect("fits"),
            )
            .expect("limit");
        Self(Rc::new(Inner {
            connection,
            clock: Cell::new(now_ms),
            statements: Cell::new(0),
            queries: Cell::new(0),
            returned: Cell::new((0, 0)),
        }))
    }
    pub fn set_time(&self, now_ms: i64) {
        self.0.clock.set(now_ms);
    }
    pub fn advance(&self, by_ms: i64) {
        self.0.clock.set(self.0.clock.get() + by_ms);
    }
    pub fn statements_run(&self) -> u64 {
        self.0.statements.get()
    }
    /// D1 queries made so far: each `all`, `run` and `batch` call is one,
    /// however many statements a batch holds (D1's per-invocation count).
    pub fn queries(&self) -> u64 {
        self.0.queries.get()
    }
    /// Rows, and bytes of text and BLOB values, returned so far.
    pub fn returned(&self) -> (u64, u64) {
        self.0.returned.get()
    }
    /// Run raw SQL outside any batch (test setup and inventory only).
    pub fn execute_script(&self, sql: &str) -> Result<(), D1Error> {
        self.0
            .connection
            .execute_batch(sql)
            .map_err(|e| D1Error::classify(&e.to_string()))
    }

    fn one(&self, statement: &Statement, batched: bool) -> Result<Outcome, D1Error> {
        check_params(&statement.params)?;
        self.0.statements.set(self.0.statements.get() + 1);
        let error = |e: rusqlite::Error| D1Error::classify(&e.to_string());
        let mut prepared = self.0.connection.prepare(&statement.sql).map_err(error)?;
        let width = prepared.column_count();
        if batched {
            // A batch returns row objects keyed by column name (Workers), so
            // two columns with one name would collapse into one there.
            let names: Vec<&str> = prepared.column_names().collect();
            let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
            if unique.len() != names.len() {
                return Err(D1Error::Other(format!(
                    "d1.duplicate_column_name in a batched statement: {names:?}"
                )));
            }
        }
        let readonly = prepared.readonly();
        let params: Vec<rusqlite::types::Value> = statement
            .params
            .iter()
            .map(|value| match value {
                Value::Null => rusqlite::types::Value::Null,
                Value::Integer(n) => rusqlite::types::Value::Integer(*n),
                Value::Text(text) => rusqlite::types::Value::Text(text.clone()),
                Value::Blob(bytes) => rusqlite::types::Value::Blob(bytes.clone()),
            })
            .collect();
        let mut rows = prepared
            .query(rusqlite::params_from_iter(params))
            .map_err(error)?;
        let mut out: Vec<Row> = vec![];
        while let Some(row) = rows.next().map_err(error)? {
            let mut values = Vec::with_capacity(width);
            for index in 0..width {
                values.push(match row.get_ref(index).map_err(error)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => Value::Integer(n),
                    ValueRef::Real(_) => return Err(D1Error::Range),
                    ValueRef::Text(bytes) => {
                        Value::Text(String::from_utf8(bytes.to_vec()).map_err(|_| D1Error::Range)?)
                    }
                    ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
                });
            }
            let bytes: usize = values
                .iter()
                .map(|value| match value {
                    Value::Text(text) => text.len(),
                    Value::Blob(bytes) => bytes.len(),
                    _ => 0,
                })
                .sum();
            let (count, total) = self.0.returned.get();
            self.0
                .returned
                .set((count + 1, total + u64::try_from(bytes).unwrap_or(u64::MAX)));
            out.push(values);
        }
        drop(rows);
        let changes = if readonly {
            0
        } else {
            self.0.connection.changes()
        };
        Ok(Outcome { rows: out, changes })
    }
}

impl D1 for NativeD1 {
    async fn all(&self, statement: Statement) -> Result<Vec<Row>, D1Error> {
        self.0.queries.set(self.0.queries.get() + 1);
        self.one(&statement, false).map(|outcome| outcome.rows)
    }
    async fn run(&self, statement: Statement) -> Result<u64, D1Error> {
        self.0.queries.set(self.0.queries.get() + 1);
        self.one(&statement, false).map(|outcome| outcome.changes)
    }
    async fn batch(&self, statements: Vec<Statement>) -> Result<Vec<Outcome>, D1Error> {
        self.0.queries.set(self.0.queries.get() + 1);
        let connection = &self.0.connection;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| D1Error::classify(&e.to_string()))?;
        let mut outcomes = Vec::with_capacity(statements.len());
        for statement in &statements {
            match self.one(statement, true) {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    let _ = connection.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
        }
        connection
            .execute_batch("COMMIT")
            .map_err(|e| D1Error::classify(&e.to_string()))?;
        Ok(outcomes)
    }
    fn now_ms(&self) -> i64 {
        self.0.clock.get()
    }
}

/// Drive a D1 store future to completion: every native D1 future is
/// synchronous underneath, so one poll must finish it.
pub fn run<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("a native D1 future yielded"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::prepare;

    /// CF12 review round 1: D1 refuses any string, BLOB or row over
    /// 2,000,000 bytes on every plan; the native stand-in does too, so a
    /// store test catches a row production would refuse.
    #[test]
    fn values_and_rows_are_held_to_d1s_limit() {
        let db = NativeD1::new(0);
        db.execute_script("CREATE TABLE t (a TEXT, b BLOB)")
            .unwrap();
        let insert = |a: usize, b: usize| {
            run(db.run(
                prepare("INSERT INTO t (a, b) VALUES (?, ?)")
                    .bind("x".repeat(a))
                    .bind(vec![0u8; b]),
            ))
        };
        assert!(insert(1_999_000, 0).is_ok(), "under the limit");
        assert!(insert(0, 1_999_000).is_ok(), "a BLOB under the limit");
        assert!(insert(D1_MAX_LENGTH + 1, 0).is_err(), "one string over");
        assert!(insert(0, D1_MAX_LENGTH + 1).is_err(), "one BLOB over");
        assert!(insert(1_000_000, 1_000_000).is_err(), "a row over");
        let rows = run(db.all(prepare("SELECT count(*) FROM t"))).unwrap();
        assert_eq!(rows[0][0], Value::Integer(2));
    }
}
