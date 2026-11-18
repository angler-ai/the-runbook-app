//! [`DoSql`] over a SQLite-backed Durable Object's `ctx.storage` (wasm32
//! only). Statements go through `storage.sql.exec`; a transaction is
//! `storage.transactionSync(callback)`, called by name because workers-rs
//! 0.8.7 does not bind it (D55, CF1). The alarm is set without awaiting:
//! on SQLite storage the write is synchronous and transactional, so it
//! rolls back with the rows.
use crate::{
    Row, Value,
    sql::{DoSql, MAX_EXACT_INTEGER},
};
use runbook_core::StoreError;
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use worker::{SqlStorage, SqlStorageValue, Storage};

/// One Durable Object's storage. Cheap to clone; clones share the object.
#[derive(Clone)]
pub struct WorkersSql {
    storage: Rc<Storage>,
    sql: SqlStorage,
}

impl WorkersSql {
    pub fn new(storage: Storage) -> Self {
        let sql = storage.sql();
        Self {
            storage: Rc::new(storage),
            sql,
        }
    }
    pub fn storage(&self) -> &Storage {
        &self.storage
    }
}

fn binding(value: &Value) -> SqlStorageValue {
    match value {
        Value::Null => SqlStorageValue::Null,
        Value::Integer(number) => SqlStorageValue::Integer(*number),
        Value::Text(text) => SqlStorageValue::String(text.clone()),
        Value::Blob(bytes) => SqlStorageValue::Blob(bytes.clone()),
    }
}
fn column(value: SqlStorageValue) -> Result<Value, StoreError> {
    Ok(match value {
        SqlStorageValue::Null => Value::Null,
        SqlStorageValue::Boolean(flag) => Value::Integer(i64::from(flag)),
        SqlStorageValue::Integer(number) => Value::Integer(number),
        // A float the store did not write: a value outside the exact range.
        SqlStorageValue::Float(_) => return Err(StoreError::Backend("do.integer_range")),
        SqlStorageValue::String(text) => Value::Text(text),
        SqlStorageValue::Blob(bytes) => Value::Blob(bytes),
    })
}
fn bindings(values: &[Value]) -> Result<Vec<SqlStorageValue>, StoreError> {
    values
        .iter()
        .map(|value| match value {
            Value::Integer(n) if !(-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(n) => {
                Err(StoreError::Backend("do.integer_range"))
            }
            other => Ok(binding(other)),
        })
        .collect()
}

impl DoSql for WorkersSql {
    fn exec(&self, sql: &str, params: &[Value]) -> Result<(), StoreError> {
        let cursor = self
            .sql
            .exec(sql, bindings(params)?)
            .map_err(|_| StoreError::Backend("do.write"))?;
        // Drain the cursor so the statement runs to completion.
        for row in cursor.raw() {
            row.map_err(|_| StoreError::Backend("do.write"))?;
        }
        Ok(())
    }
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, StoreError> {
        let cursor = self
            .sql
            .exec(sql, bindings(params)?)
            .map_err(|_| StoreError::Backend("do.read"))?;
        cursor
            .raw()
            .map(|row| {
                row.map_err(|_| StoreError::Backend("do.read"))?
                    .into_iter()
                    .map(column)
                    .collect()
            })
            .collect()
    }
    fn exec_script(&self, sql: &str) -> Result<(), StoreError> {
        self.sql
            .exec(sql, None)
            .map(|_| ())
            .map_err(|_| StoreError::Backend("do.write"))
    }
    /// A Rust `Err` is thrown as a JS exception so SQLite rolls back every
    /// write the body made; so does any SQL error or trap escaping it.
    fn transaction<T: 'static>(
        &self,
        body: impl FnOnce() -> Result<T, StoreError> + 'static,
    ) -> Result<T, StoreError> {
        let outcome: Rc<RefCell<Option<Result<T, StoreError>>>> = Rc::default();
        let slot = outcome.clone();
        let callback = Closure::once_into_js(move || -> Result<(), JsValue> {
            let result = body();
            let thrown = result
                .as_ref()
                .err()
                .map(|error| JsValue::from(js_sys::Error::new(&format!("{error:?}"))));
            *slot.borrow_mut() = Some(result);
            thrown.map_or(Ok(()), Err)
        });
        let raw: &JsValue = self.storage.as_raw().as_ref();
        let function: js_sys::Function = js_sys::Reflect::get(raw, &"transactionSync".into())
            .ok()
            .and_then(|f| f.dyn_into().ok())
            .ok_or(StoreError::Backend("do.no_transaction_sync"))?;
        let called = function.call1(raw, &callback);
        let taken = outcome.borrow_mut().take();
        match (called, taken) {
            (Ok(_), Some(Ok(value))) => Ok(value),
            (_, Some(Err(error))) => Err(error),
            // A SQL error or trap escaped the body: the transaction rolled back.
            _ => Err(StoreError::Backend("do.transaction_aborted")),
        }
    }
    fn now_ms(&self) -> i64 {
        // Date.now() is an integral millisecond count (the request time).
        #[allow(clippy::cast_possible_truncation)]
        let now = js_sys::Date::now() as i64;
        now
    }
    fn set_alarm(&self, at_ms: Option<i64>) -> Result<(), StoreError> {
        let raw = self.storage.as_raw();
        let promise = match at_ms {
            // Precision: epoch milliseconds are far inside f64's exact range.
            #[allow(clippy::cast_precision_loss)]
            Some(at_ms) => raw.set_alarm(
                js_sys::Date::new(&JsValue::from_f64(at_ms as f64)),
                js_sys::Object::new(),
            ),
            None => raw.delete_alarm(js_sys::Object::new()),
        }
        .map_err(|_| StoreError::Backend("do.alarm"))?;
        drop(promise);
        Ok(())
    }
}
