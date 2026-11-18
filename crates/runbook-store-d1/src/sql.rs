//! The D1 surface the adapters need (CF3): prepared statements with
//! positional `?` bindings, `all`, `run` and an atomic `batch`, plus the
//! host clock. Workers implements it over `worker::D1Database`
//! (`workers.rs`); native tests over rusqlite (`native.rs`). Nothing here
//! is Workers-specific.
//!
//! D1 has no interactive transactions: a `batch` is the only atomic unit.
//! So every write that depends on what it read is "read, decide, then one
//! guarded batch": the batch starts with [`guard`] statements that abort
//! it (rolling back every statement) when a condition read earlier no
//! longer holds, such as a revision compare-and-set.
use std::{borrow::Cow, future::Future};

/// One SQLite value. Booleans are integers (0/1). There are no reals: the
/// stores never write one, and D1 returns integers as JavaScript numbers,
/// so values outside ±2^53 are refused rather than rounded.
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
impl From<&String> for Value {
    fn from(text: &String) -> Self {
        Self::Text(text.clone())
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
impl From<i32> for Value {
    fn from(number: i32) -> Self {
        Self::Integer(i64::from(number))
    }
}
impl From<u32> for Value {
    fn from(number: u32) -> Self {
        Self::Integer(i64::from(number))
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
impl From<&[u8]> for Value {
    fn from(bytes: &[u8]) -> Self {
        Self::Blob(bytes.to_vec())
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

/// The largest integer D1 round-trips exactly (JavaScript's safe range).
pub const MAX_EXACT_INTEGER: i64 = (1 << 53) - 1;

/// A failed D1 call, classified from SQLite's message (both backends
/// produce the same text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum D1Error {
    /// A [`guard`] aborted the batch; the code it was given.
    Guard(String),
    /// A UNIQUE or PRIMARY KEY constraint (a racing duplicate).
    Unique,
    /// A FOREIGN KEY constraint.
    Foreign,
    /// An integer outside the exact range, or a column of an unexpected type.
    Range,
    /// Anything else (a SQL error, a binding or transport failure).
    Other(String),
}
impl D1Error {
    /// Classify an error message from SQLite or D1.
    pub fn classify(message: &str) -> Self {
        if let Some(at) = message.find(GUARD_PREFIX) {
            let rest = &message[at + GUARD_PREFIX.len()..];
            let code = rest
                .split(|c: char| c == ':' || c.is_whitespace())
                .next()
                .unwrap_or_default();
            return Self::Guard(code.to_owned());
        }
        if message.contains("UNIQUE constraint failed") {
            return Self::Unique;
        }
        if message.contains("FOREIGN KEY constraint failed") {
            return Self::Foreign;
        }
        Self::Other(message.chars().take(300).collect())
    }
    /// The guard code, if a guard aborted the batch.
    pub fn guard(&self) -> Option<&str> {
        match self {
            Self::Guard(code) => Some(code),
            _ => None,
        }
    }
}

const GUARD_PREFIX: &str = "rb.guard:";

/// D1's largest string, BLOB or table row, in bytes, on every plan
/// (CF12). The native stand-in enforces it (`SQLITE_LIMIT_LENGTH`).
pub const D1_MAX_LENGTH: usize = 2_000_000;

/// One statement with its bindings. Pure data: a backend prepares it when
/// it runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub sql: Cow<'static, str>,
    pub params: Vec<Value>,
}
/// Start a statement (`D1Database.prepare`).
pub fn prepare(sql: impl Into<Cow<'static, str>>) -> Statement {
    Statement {
        sql: sql.into(),
        params: vec![],
    }
}
impl Statement {
    /// Bind the next positional parameter.
    #[must_use]
    pub fn bind(mut self, value: impl Into<Value>) -> Self {
        self.params.push(value.into());
        self
    }
    /// Bind several, in order.
    #[must_use]
    pub fn binds<V: Into<Value>>(mut self, values: impl IntoIterator<Item = V>) -> Self {
        self.params.extend(values.into_iter().map(Into::into));
        self
    }
}

/// A statement that aborts its batch with `code` unless `condition` (an
/// SQL boolean over its own `?` bindings) holds when the batch runs. It
/// inserts into `d1_guard`, whose trigger raises `rb.guard:<code>`
/// (migration `1001_d1_guard.sql`).
pub fn guard(code: &'static str, condition: &str) -> Statement {
    prepare(format!(
        "INSERT INTO d1_guard (code) SELECT ? WHERE NOT ({condition})"
    ))
    .bind(code)
}

/// What one statement of a batch returned.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub rows: Vec<Row>,
    /// Rows written (`meta.changes`). On D1 this also counts rows changed
    /// by triggers and foreign-key actions (native SQLite does not), so
    /// compare it only on tables with neither, or read the rows back with
    /// `RETURNING` instead.
    pub changes: u64,
}

/// The D1 database a store runs on. Futures need not be `Send`.
///
/// - `all` runs one statement and returns its rows (column order);
/// - `run` runs one statement and returns the rows it changed (see
///   [`Outcome::changes`] for triggers and cascades);
/// - `batch` runs statements in order as one transaction: all of them
///   commit, or none (an error, or a [`guard`], rolls everything back);
/// - `now_ms` is the host clock (Workers' `Date.now()`; a test clock
///   natively). D1 itself has no clock the stores may call.
pub trait D1: Clone + 'static {
    fn all(&self, statement: Statement) -> impl Future<Output = Result<Vec<Row>, D1Error>>;
    fn run(&self, statement: Statement) -> impl Future<Output = Result<u64, D1Error>>;
    fn batch(
        &self,
        statements: Vec<Statement>,
    ) -> impl Future<Output = Result<Vec<Outcome>, D1Error>>;
    fn now_ms(&self) -> i64;
}

/// Typed column reads that refuse a missing column or another type.
pub trait Columns {
    fn value(&self, index: usize) -> Result<&Value, D1Error>;
    fn integer(&self, index: usize) -> Result<i64, D1Error> {
        match self.value(index)? {
            Value::Integer(value) => Ok(*value),
            _ => Err(D1Error::Range),
        }
    }
    fn optional_integer(&self, index: usize) -> Result<Option<i64>, D1Error> {
        match self.value(index)? {
            Value::Null => Ok(None),
            Value::Integer(value) => Ok(Some(*value)),
            _ => Err(D1Error::Range),
        }
    }
    fn unsigned(&self, index: usize) -> Result<u64, D1Error> {
        u64::try_from(self.integer(index)?).map_err(|_| D1Error::Range)
    }
    fn text(&self, index: usize) -> Result<&str, D1Error> {
        match self.value(index)? {
            Value::Text(value) => Ok(value),
            _ => Err(D1Error::Range),
        }
    }
    fn string(&self, index: usize) -> Result<String, D1Error> {
        self.text(index).map(str::to_owned)
    }
    fn optional_text(&self, index: usize) -> Result<Option<String>, D1Error> {
        match self.value(index)? {
            Value::Null => Ok(None),
            Value::Text(value) => Ok(Some(value.clone())),
            _ => Err(D1Error::Range),
        }
    }
    fn blob(&self, index: usize) -> Result<&[u8], D1Error> {
        match self.value(index)? {
            Value::Blob(value) => Ok(value),
            _ => Err(D1Error::Range),
        }
    }
    fn optional_blob(&self, index: usize) -> Result<Option<Vec<u8>>, D1Error> {
        match self.value(index)? {
            Value::Null => Ok(None),
            Value::Blob(value) => Ok(Some(value.clone())),
            _ => Err(D1Error::Range),
        }
    }
    fn flag(&self, index: usize) -> Result<bool, D1Error> {
        match self.integer(index)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(D1Error::Range),
        }
    }
    /// A typed ID (or any `FromStr`) stored as text.
    fn parse<T: std::str::FromStr>(&self, index: usize) -> Result<T, D1Error> {
        self.text(index)?.parse().map_err(|_| D1Error::Range)
    }
    fn optional_parse<T: std::str::FromStr>(&self, index: usize) -> Result<Option<T>, D1Error> {
        self.optional_text(index)?
            .map(|text| text.parse().map_err(|_| D1Error::Range))
            .transpose()
    }
    /// A JSON column decoded with serde.
    fn json<T: serde::de::DeserializeOwned>(&self, index: usize) -> Result<T, D1Error> {
        serde_json::from_str(self.text(index)?).map_err(|_| D1Error::Range)
    }
    fn optional_json<T: serde::de::DeserializeOwned>(
        &self,
        index: usize,
    ) -> Result<Option<T>, D1Error> {
        self.optional_text(index)?
            .map(|text| serde_json::from_str(&text).map_err(|_| D1Error::Range))
            .transpose()
    }
    /// A 32-byte digest stored as a BLOB.
    fn digest(&self, index: usize) -> Result<[u8; 32], D1Error> {
        self.blob(index)?.try_into().map_err(|_| D1Error::Range)
    }
}
impl Columns for Row {
    fn value(&self, index: usize) -> Result<&Value, D1Error> {
        self.get(index).ok_or(D1Error::Range)
    }
}

/// A serde value as a JSON TEXT binding.
pub fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Value, D1Error> {
    serde_json::to_string(value)
        .map(Value::Text)
        .map_err(|_| D1Error::Other("d1.encode".into()))
}
/// An unsigned count, sequence or revision as an exact integer.
pub fn uint(value: u64) -> Result<Value, D1Error> {
    i64::try_from(value)
        .ok()
        .filter(|v| *v <= MAX_EXACT_INTEGER)
        .map(Value::Integer)
        .ok_or(D1Error::Range)
}
/// A signed time or count as an exact integer.
pub fn int(value: i64) -> Result<Value, D1Error> {
    if (-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&value) {
        Ok(Value::Integer(value))
    } else {
        Err(D1Error::Range)
    }
}
/// Refuse any binding outside the exact range before it reaches D1.
pub fn check_params(params: &[Value]) -> Result<(), D1Error> {
    for value in params {
        if let Value::Integer(n) = value
            && !(-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(n)
        {
            return Err(D1Error::Range);
        }
    }
    Ok(())
}

/// `?, ?, ?` for `n` bindings (an `IN (...)` list).
pub fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_are_classified_from_either_backend() {
        assert_eq!(
            D1Error::classify(
                "D1_ERROR: rb.guard:conflict: SQLITE_CONSTRAINT (extended: SQLITE_CONSTRAINT_TRIGGER)"
            ),
            D1Error::Guard("conflict".into())
        );
        assert_eq!(
            D1Error::classify("rb.guard:directory.revision"),
            D1Error::Guard("directory.revision".into())
        );
        assert_eq!(
            D1Error::classify("D1_ERROR: UNIQUE constraint failed: project.org_id, project.slug"),
            D1Error::Unique
        );
        assert_eq!(
            D1Error::classify("FOREIGN KEY constraint failed"),
            D1Error::Foreign
        );
    }
}
