//! D20 replay records are immutable and run-owned. A new timestamped record
//! replaces an expired logical key without changing the generic store schema.
use crate::{IdempotencyRecord, StoreError};
use runbook_model::ExecutionId;
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Replay {
    pub key: String,
    pub principal: String,
    pub request_hash: [u8; 32],
}
impl Replay {
    pub fn prefix(&self, operation: &str) -> Result<String, StoreError> {
        if self.key.trim().is_empty() || self.key.len() > 256 || self.principal.is_empty() {
            return Err(StoreError::InvalidBatch("request.invalid_replay"));
        }
        let framed = serde_json::to_vec(&(operation, &self.principal, &self.key))
            .map_err(|_| StoreError::Backend("replay.encode"))?;
        Ok(format!(
            "actor:{}:",
            crate::authoring::hex(&Sha256::digest(framed))
        ))
    }
    pub fn lookup<'a, T: DeserializeOwned>(
        &self,
        operation: &str,
        rows: impl Iterator<Item = &'a IdempotencyRecord>,
        now: i64,
    ) -> Result<Option<T>, StoreError> {
        let prefix = self.prefix(operation)?;
        let mut latest = None;
        for row in rows.filter(|r| r.key.starts_with(&prefix)) {
            let at = row
                .response
                .get("at_ms")
                .and_then(serde_json::Value::as_i64)
                .ok_or(StoreError::Backend("replay.corrupt"))?;
            if latest.is_none_or(|(previous, _)| at > previous) {
                latest = Some((at, row));
            }
        }
        let Some((at, row)) = latest else {
            return Ok(None);
        };
        if i128::from(now) - i128::from(at) >= 86_400_000 {
            return Ok(None);
        }
        if row.request_hash != self.request_hash || row.response["principal"] != self.principal {
            return Err(StoreError::InvalidBatch("request.idempotency_key_reused"));
        }
        serde_json::from_value(
            row.response
                .get("value")
                .cloned()
                .ok_or(StoreError::Backend("replay.corrupt"))?,
        )
        .map(Some)
        .map_err(|_| StoreError::Backend("replay.corrupt"))
    }
    pub fn record(
        &self,
        operation: &str,
        id: &ExecutionId,
        now: i64,
        value: impl Serialize,
    ) -> Result<IdempotencyRecord, StoreError> {
        let value =
            serde_json::to_value(value).map_err(|_| StoreError::Backend("replay.encode"))?;
        Ok(IdempotencyRecord {
            execution_id: id.clone(),
            key: format!("{}{now}", self.prefix(operation)?),
            request_hash: self.request_hash,
            response: serde_json::json!({"at_ms":now,"principal":self.principal,"value":value}),
        })
    }
}

/// Classify and validate the reserved actor replay envelope at the storage boundary.
pub fn replay_record_parts(record: &IdempotencyRecord) -> Result<Option<(&str, i64)>, StoreError> {
    if !record.key.starts_with("actor:") {
        return Ok(None);
    }
    let (prefix, suffix) = record
        .key
        .rsplit_once(':')
        .ok_or(StoreError::InvalidBatch("replay.invalid_record"))?;
    let at = suffix
        .parse::<i64>()
        .map_err(|_| StoreError::InvalidBatch("replay.invalid_record"))?;
    if prefix.len() != 70
        || !prefix[6..].bytes().all(|b| b.is_ascii_hexdigit())
        || record.response["at_ms"].as_i64() != Some(at)
    {
        return Err(StoreError::InvalidBatch("replay.invalid_record"));
    }
    Ok(Some((&record.key[..prefix.len() + 1], at)))
}
