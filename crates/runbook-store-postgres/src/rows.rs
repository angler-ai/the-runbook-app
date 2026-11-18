//! Exact JSON payload encoding and redacted error mapping.
use runbook_core::StoreError;
use serde::{Serialize, de::DeserializeOwned};
use tokio_postgres::error::SqlState;

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|_| StoreError::Backend("postgres.encode_failed"))
}
pub(crate) fn decode<T: DeserializeOwned>(text: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(|_| StoreError::Backend("postgres.decode_failed"))
}
/// Serde name of a unit enum variant, for indexed scalar columns.
pub(crate) fn label<T: Serialize>(value: &T) -> Result<String, StoreError> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        _ => Err(StoreError::Backend("postgres.encode_failed")),
    }
}
pub(crate) fn int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidBatch("store.sequence_overflow"))
}
pub(crate) fn uint(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Backend("postgres.negative_counter"))
}
pub(crate) fn is_unique_violation(error: &tokio_postgres::Error) -> bool {
    error.code() == Some(&SqlState::UNIQUE_VIOLATION)
}
/// Driver errors never carry connection details or SQL text outward.
pub(crate) fn db(error: tokio_postgres::Error) -> StoreError {
    match error.code() {
        Some(code) if *code == SqlState::UNIQUE_VIOLATION => {
            StoreError::Backend("postgres.unique_violation")
        }
        Some(code) if *code == SqlState::LOCK_NOT_AVAILABLE => {
            StoreError::Backend("postgres.lock_timeout")
        }
        Some(code)
            if *code == SqlState::T_R_DEADLOCK_DETECTED
                || *code == SqlState::T_R_SERIALIZATION_FAILURE =>
        {
            StoreError::Backend("postgres.retry_transaction")
        }
        _ => StoreError::Backend("postgres.database_error"),
    }
}
pub(crate) fn connection(error: crate::Error) -> StoreError {
    match error.0 {
        "postgres.pool_timeout" => StoreError::Backend("postgres.pool_timeout"),
        _ => StoreError::Backend("postgres.connection_failed"),
    }
}
