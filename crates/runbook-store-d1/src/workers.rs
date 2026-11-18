//! [`D1`] over a Workers D1 binding (wasm32 only).
//!
//! - `all` uses `raw()`: each row is an array in column order;
//! - `batch` returns row objects, read back in key order (column order), so
//!   a batched SELECT must name each column once (`NativeD1` refuses a
//!   duplicate name, so native tests catch it);
//! - integers travel as JavaScript numbers: bindings outside ±2^53 are
//!   refused before the call, and a non-integral number in a result is
//!   `D1Error::Range`;
//! - BLOBs are bound as `ArrayBuffer`s and read from arrays of bytes;
//! - the clock is `Date.now()`.
use crate::sql::{D1, D1Error, MAX_EXACT_INTEGER, Outcome, Row, Statement, Value, check_params};
use js_sys::{Array, ArrayBuffer, Object, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use worker::{D1Database, D1PreparedStatement};

/// One D1 database binding. Cheap to clone.
#[derive(Clone)]
pub struct WorkersD1 {
    db: std::rc::Rc<D1Database>,
}
impl WorkersD1 {
    pub fn new(db: D1Database) -> Self {
        Self {
            db: std::rc::Rc::new(db),
        }
    }
}

fn binding(value: &Value) -> JsValue {
    match value {
        Value::Null => JsValue::null(),
        // Within the exact range (checked first), so the float is exact.
        #[allow(clippy::cast_precision_loss)]
        Value::Integer(n) => JsValue::from_f64(*n as f64),
        Value::Text(text) => JsValue::from_str(text),
        Value::Blob(bytes) => Uint8Array::from(bytes.as_slice()).buffer().into(),
    }
}

fn column(value: &JsValue) -> Result<Value, D1Error> {
    if value.is_null() || value.is_undefined() {
        return Ok(Value::Null);
    }
    if let Some(number) = value.as_f64() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let integer = number as i64;
        #[allow(clippy::cast_precision_loss)]
        if integer as f64 == number && integer.abs() <= MAX_EXACT_INTEGER {
            return Ok(Value::Integer(integer));
        }
        return Err(D1Error::Range);
    }
    if let Some(flag) = value.as_bool() {
        return Ok(Value::Integer(i64::from(flag)));
    }
    if let Some(text) = value.as_string() {
        return Ok(Value::Text(text));
    }
    if let Some(buffer) = value.dyn_ref::<ArrayBuffer>() {
        return Ok(Value::Blob(Uint8Array::new(buffer).to_vec()));
    }
    if let Some(view) = value.dyn_ref::<Uint8Array>() {
        return Ok(Value::Blob(view.to_vec()));
    }
    if let Some(array) = value.dyn_ref::<Array>() {
        let mut bytes = Vec::with_capacity(array.length() as usize);
        for item in array.iter() {
            let byte = item
                .as_f64()
                .filter(|b| (0.0..=255.0).contains(b) && b.fract() == 0.0)
                .ok_or(D1Error::Range)?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            bytes.push(byte as u8);
        }
        return Ok(Value::Blob(bytes));
    }
    Err(D1Error::Range)
}

/// The text of a thrown error: its message and its cause (D1 puts the
/// SQLite message in either, depending on the path).
fn error_text(error: &worker::Error) -> String {
    let raw: Option<&JsValue> = match error {
        worker::Error::D1(e) => Some(e.as_ref()),
        worker::Error::JsError(_) => None,
        worker::Error::Internal(value) => Some(value),
        _ => None,
    };
    let mut text = error.to_string();
    if let Some(value) = raw
        && let Some(e) = value.dyn_ref::<js_sys::Error>()
    {
        text.push(' ');
        text.push_str(&String::from(e.message()));
        if let Some(cause) = e.cause().dyn_ref::<js_sys::Error>() {
            text.push(' ');
            text.push_str(&String::from(cause.message()));
        }
    }
    text
}
fn failed(error: &worker::Error) -> D1Error {
    D1Error::classify(&error_text(error))
}

impl WorkersD1 {
    fn prepared(&self, statement: &Statement) -> Result<D1PreparedStatement, D1Error> {
        check_params(&statement.params)?;
        let values: Vec<JsValue> = statement.params.iter().map(binding).collect();
        self.db
            .prepare(statement.sql.as_ref())
            .bind(&values)
            .map_err(|e| failed(&e))
    }
}

fn row_of_array(value: &JsValue) -> Result<Row, D1Error> {
    let array = value.dyn_ref::<Array>().ok_or(D1Error::Range)?;
    array.iter().map(|v| column(&v)).collect()
}
fn row_of_object(value: &JsValue) -> Result<Row, D1Error> {
    let object = value.dyn_ref::<Object>().ok_or(D1Error::Range)?;
    Object::values(object).iter().map(|v| column(&v)).collect()
}

impl D1 for WorkersD1 {
    async fn all(&self, statement: Statement) -> Result<Vec<Row>, D1Error> {
        let prepared = self.prepared(&statement)?;
        prepared
            .raw_js_value()
            .await
            .map_err(|e| failed(&e))?
            .iter()
            .map(row_of_array)
            .collect()
    }
    async fn run(&self, statement: Statement) -> Result<u64, D1Error> {
        let prepared = self.prepared(&statement)?;
        let result = prepared.run().await.map_err(|e| failed(&e))?;
        Ok(result
            .meta()
            .ok()
            .flatten()
            .and_then(|m| m.changes)
            .map_or(0, |c| c as u64))
    }
    async fn batch(&self, statements: Vec<Statement>) -> Result<Vec<Outcome>, D1Error> {
        let prepared = statements
            .iter()
            .map(|s| self.prepared(s))
            .collect::<Result<Vec<_>, _>>()?;
        let results = self.db.batch(prepared).await.map_err(|e| failed(&e))?;
        let mut out = Vec::with_capacity(results.len());
        for result in results {
            let changes = result
                .meta()
                .ok()
                .flatten()
                .and_then(|m| m.changes)
                .map_or(0, |c| c as u64);
            let rows = result
                .results::<PreservedRow>()
                .map_err(|e| failed(&e))?
                .iter()
                .map(|row| row_of_object(&row.0))
                .collect::<Result<_, _>>()?;
            out.push(Outcome { rows, changes });
        }
        Ok(out)
    }
    fn now_ms(&self) -> i64 {
        #[allow(clippy::cast_possible_truncation)]
        let now = js_sys::Date::now() as i64;
        now
    }
}

/// A row object as deserialized by serde-wasm-bindgen, kept as a `JsValue`.
struct PreservedRow(JsValue);
impl<'de> serde::Deserialize<'de> for PreservedRow {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde_wasm_bindgen::preserve::deserialize(deserializer).map(PreservedRow)
    }
}
