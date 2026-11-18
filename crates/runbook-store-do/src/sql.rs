//! The SQL surface the ExecutionDO store needs (CF2): `exec`/`query` over
//! SQLite rows, one synchronous transaction, the object's clock and its
//! alarm. Workers implements it over `ctx.storage` (`workers.rs`); native
//! tests implement it over `rusqlite`. Nothing here is Workers-specific, so
//! the store logic compiles and runs unchanged on both.
use runbook_core::StoreError;

/// One SQLite value. Booleans are integers (0/1) and there are no reals:
/// the store never writes one, and a DO integer outside JavaScript's safe
/// range would come back as a float.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Integer(i64),
    Text(String),
    Blob(Vec<u8>),
}
pub type Row = Vec<Value>;

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}
impl From<String> for Value {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}
impl From<i64> for Value {
    fn from(number: i64) -> Self {
        Self::Integer(number)
    }
}
impl From<Option<i64>> for Value {
    fn from(number: Option<i64>) -> Self {
        number.map_or(Self::Null, Self::Integer)
    }
}
impl From<bool> for Value {
    fn from(flag: bool) -> Self {
        Self::Integer(i64::from(flag))
    }
}
impl From<Vec<u8>> for Value {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Blob(bytes)
    }
}

/// The largest integer a DO can store exactly (JavaScript's safe range).
pub const MAX_EXACT_INTEGER: i64 = (1 << 53) - 1;

/// An unsigned count or generation as a SQL integer, refused when a DO
/// could not store it exactly.
pub fn uint(value: u64) -> Result<Value, StoreError> {
    i64::try_from(value)
        .ok()
        .filter(|v| *v <= MAX_EXACT_INTEGER)
        .map(Value::Integer)
        .ok_or(StoreError::Backend("do.integer_range"))
}
/// A signed time or count as a SQL integer, refused outside the exact range.
pub fn int(value: i64) -> Result<Value, StoreError> {
    if (-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&value) {
        Ok(Value::Integer(value))
    } else {
        Err(StoreError::Backend("do.integer_range"))
    }
}

/// The ExecutionDO's storage host.
///
/// - `exec` runs one statement with positional `?` bindings; `query` also
///   returns its rows, each a vector of column values in select order.
/// - `exec_script` runs several statements without bindings (migrations).
/// - `transaction` runs `body` atomically: every write it makes, and any
///   `set_alarm` inside it, commits together or rolls back on `Err` or a
///   trap. The body is synchronous, so nothing can interleave (on Workers it
///   is `ctx.storage.transactionSync`). Bodies never nest.
/// - `now_ms` is the store clock (the object's request time on Workers),
///   the only time the store uses (PRD §14 "One clock").
/// - `set_alarm` sets or clears the object's single alarm.
pub trait DoSql: Clone + 'static {
    fn exec(&self, sql: &str, params: &[Value]) -> Result<(), StoreError>;
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, StoreError>;
    fn exec_script(&self, sql: &str) -> Result<(), StoreError>;
    fn transaction<T: 'static>(
        &self,
        body: impl FnOnce() -> Result<T, StoreError> + 'static,
    ) -> Result<T, StoreError>;
    fn now_ms(&self) -> i64;
    fn set_alarm(&self, at_ms: Option<i64>) -> Result<(), StoreError>;
}

fn decode_error() -> StoreError {
    StoreError::Backend("do.decode")
}

/// Typed column reads. Every accessor refuses a missing column or a value
/// of another type instead of panicking (the commit path must not trap).
pub(crate) trait Columns {
    fn integer(&self, index: usize) -> Result<i64, StoreError>;
    fn optional_integer(&self, index: usize) -> Result<Option<i64>, StoreError>;
    fn unsigned(&self, index: usize) -> Result<u64, StoreError>;
    fn text(&self, index: usize) -> Result<&str, StoreError>;
    fn flag(&self, index: usize) -> Result<bool, StoreError>;
}
impl Columns for Row {
    fn integer(&self, index: usize) -> Result<i64, StoreError> {
        match self.get(index) {
            Some(Value::Integer(value)) => Ok(*value),
            _ => Err(decode_error()),
        }
    }
    fn optional_integer(&self, index: usize) -> Result<Option<i64>, StoreError> {
        match self.get(index) {
            Some(Value::Null) => Ok(None),
            Some(Value::Integer(value)) => Ok(Some(*value)),
            _ => Err(decode_error()),
        }
    }
    fn unsigned(&self, index: usize) -> Result<u64, StoreError> {
        u64::try_from(self.integer(index)?).map_err(|_| decode_error())
    }
    fn text(&self, index: usize) -> Result<&str, StoreError> {
        match self.get(index) {
            Some(Value::Text(value)) => Ok(value),
            _ => Err(decode_error()),
        }
    }
    fn flag(&self, index: usize) -> Result<bool, StoreError> {
        match self.integer(index)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(decode_error()),
        }
    }
}

pub(crate) fn encode<T: serde::Serialize>(value: &T) -> Result<Value, StoreError> {
    serde_json::to_string(value)
        .map(Value::Text)
        .map_err(|_| StoreError::Backend("do.encode"))
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(|_| decode_error())
}
