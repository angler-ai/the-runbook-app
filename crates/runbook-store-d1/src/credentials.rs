//! Hash-only credential storage on D1 (B4.1, C2), the D20 token-issue
//! replay, the RFC 8628 device codes (AUTH2, D47) and the execution-token
//! route (CF2-9). Each credential mutation is one statement; a device-code
//! poll or decision is a compare-and-set of the stored payload, retried
//! when another writer changed it first.
use crate::sql::{Columns, D1, D1Error, guard, prepare};
use runbook_core::{
    catalog::TenantScope,
    credentials::{
        CredentialError, CredentialRecord, CredentialStore, SecretHash, issuance::TokenMetadata,
    },
    device_codes::{
        DeviceCodeRecord, DeviceCodeState, DeviceCodeStore, DeviceDecision, DevicePoll,
        apply_decision, apply_poll,
    },
};
use runbook_model::{ExecutionId, OrganizationId, TokenId};

#[derive(Clone)]
pub struct D1Credentials<D: D1> {
    pub(crate) db: D,
}
impl<D: D1> D1Credentials<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }
}

pub(crate) fn err(error: D1Error) -> CredentialError {
    match error {
        D1Error::Unique => CredentialError::Duplicate,
        D1Error::Guard(code) if code == "duplicate" => CredentialError::Duplicate,
        D1Error::Range => CredentialError::Backend("d1.decode"),
        _ => CredentialError::Backend("d1.credentials"),
    }
}

/// The token issuance outcome (D20), as `runbook_api::backends::IssueOutcome`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueOutcome {
    Issued,
    AlreadyIssued,
    KeyReused,
}

/// The D20 replay operation name in `directory_request` (24 hours).
const ISSUE_OPERATION: &str = "POST:/v1/tokens";
const REPLAY_TTL_MS: i64 = 86_400_000;
/// Expired device codes are purged this long after expiry, on each insert.
const PURGE_AFTER_MS: i64 = 86_400_000;
/// Compare-and-set attempts for one device-code poll or decision.
const CAS_ATTEMPTS: usize = 5;

fn record_of(row: &crate::sql::Row) -> Result<CredentialRecord, CredentialError> {
    let mut record: CredentialRecord = row.json(0).map_err(err)?;
    record.revoked_at = row.optional_integer(1).map_err(err)?;
    record.last_used_at = row.optional_integer(2).map_err(err)?;
    Ok(record)
}
/// The management metadata rides in the payload under `management`;
/// `CredentialRecord` ignores the key, so authentication is unchanged.
fn metadata_of(payload: &str) -> Result<TokenMetadata, CredentialError> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|_| CredentialError::Backend("d1.decode"))?;
    match value.get("management") {
        Some(m) => {
            serde_json::from_value(m.clone()).map_err(|_| CredentialError::Backend("d1.decode"))
        }
        None => Ok(TokenMetadata::default()),
    }
}
pub(crate) fn insert_statement(
    record: &CredentialRecord,
    metadata: Option<&TokenMetadata>,
) -> Result<crate::Statement, CredentialError> {
    record.validate()?;
    let mut payload =
        serde_json::to_value(record).map_err(|_| CredentialError::Backend("d1.encode"))?;
    if let Some(metadata) = metadata {
        payload["management"] =
            serde_json::to_value(metadata).map_err(|_| CredentialError::Backend("d1.encode"))?;
    }
    Ok(prepare(
        "INSERT INTO credential (id, org_id, secret_hash, payload, revoked_at, last_used_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(record.id.as_str())
    .bind(record.org_id.as_str())
    .bind(&record.hash.as_bytes()[..])
    .bind(payload.to_string())
    .bind(record.revoked_at)
    .bind(record.last_used_at))
}

impl<D: D1> CredentialStore for D1Credentials<D> {
    async fn insert(&self, record: CredentialRecord) -> Result<(), CredentialError> {
        self.db
            .run(insert_statement(&record, None)?)
            .await
            .map(|_| ())
            .map_err(err)
    }
    async fn find_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<CredentialRecord>, CredentialError> {
        self.db
            .all(
                prepare(
                    "SELECT payload, revoked_at, last_used_at FROM credential WHERE secret_hash = ?",
                )
                .bind(&hash.as_bytes()[..]),
            )
            .await
            .map_err(err)?
            .first()
            .map(record_of)
            .transpose()
    }
    async fn find_by_id(&self, id: &TokenId) -> Result<Option<CredentialRecord>, CredentialError> {
        self.db
            .all(
                prepare("SELECT payload, revoked_at, last_used_at FROM credential WHERE id = ?")
                    .bind(id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(record_of)
            .transpose()
    }
    async fn revoke(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> Result<bool, CredentialError> {
        Ok(self
            .db
            .run(
                prepare(
                    "UPDATE credential SET revoked_at = ? WHERE org_id = ? AND id = ? \
                     AND revoked_at IS NULL",
                )
                .bind(now)
                .bind(org.as_str())
                .bind(id.as_str()),
            )
            .await
            .map_err(err)?
            == 1)
    }
    /// One conditional statement, so concurrent touches write at most once
    /// per window. The subtraction is done on the bound value (SQLite
    /// integers are 64-bit and `now` is within the exact range).
    async fn touch_last_used(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> Result<bool, CredentialError> {
        Ok(self
            .db
            .run(
                prepare(
                    "UPDATE credential SET last_used_at = ? WHERE org_id = ? AND id = ? \
                     AND (last_used_at IS NULL OR last_used_at <= ?)",
                )
                .bind(now)
                .bind(org.as_str())
                .bind(id.as_str())
                .bind(now.saturating_sub(300_000)),
            )
            .await
            .map_err(err)?
            == 1)
    }
}

impl<D: D1> D1Credentials<D> {
    /// Read-only D20 probe before validating a retried token create.
    pub async fn issue_replay(
        &self,
        org: &OrganizationId,
        principal: &str,
        key: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<IssueOutcome>, CredentialError> {
        let rows = self
            .db
            .all(
                prepare(
                    "SELECT payload FROM directory_request WHERE org_id = ? AND principal = ? \
                     AND operation = ? AND key = ? AND expires_at > ?",
                )
                .bind(org.as_str())
                .bind(principal)
                .bind(ISSUE_OPERATION)
                .bind(key)
                .bind(self.db.now_ms()),
            )
            .await
            .map_err(err)?;
        rows.first()
            .map(|row| {
                Ok(
                    if crate::replay::matches(row.text(0).map_err(err)?, payload) {
                        IssueOutcome::AlreadyIssued
                    } else {
                        IssueOutcome::KeyReused
                    },
                )
            })
            .transpose()
    }

    /// The credential and its replay row in one batch, guarded on the
    /// replay row's absence: a concurrent retry with the same key loses the
    /// guard and answers from the winner's row, so a key never issues two
    /// secrets.
    pub async fn issue(
        &self,
        record: &CredentialRecord,
        metadata: &TokenMetadata,
        principal: &str,
        key: &str,
        payload: &serde_json::Value,
        response: &serde_json::Value,
    ) -> Result<IssueOutcome, CredentialError> {
        let org = record.org_id.as_str();
        let now = self.db.now_ms();
        let encode = |v: &serde_json::Value| v.to_string();
        let statements = vec![
            prepare(
                "DELETE FROM directory_request WHERE org_id = ? AND operation = ? AND expires_at <= ?",
            )
            .bind(org)
            .bind(ISSUE_OPERATION)
            .bind(now),
            guard(
                "replayed",
                "NOT EXISTS (SELECT 1 FROM directory_request WHERE org_id = ? AND principal = ? \
                 AND operation = ? AND key = ?)",
            )
            .bind(org)
            .bind(principal)
            .bind(ISSUE_OPERATION)
            .bind(key),
            prepare(
                "INSERT INTO directory_request (org_id, principal, operation, key, payload, \
                 response, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(org)
            .bind(principal)
            .bind(ISSUE_OPERATION)
            .bind(key)
            .bind(crate::replay::stored(payload))
            .bind(encode(response))
            .bind(now.saturating_add(REPLAY_TTL_MS)),
            insert_statement(record, Some(metadata))?,
        ];
        match self.db.batch(statements).await {
            Ok(_) => Ok(IssueOutcome::Issued),
            Err(D1Error::Guard(code)) if code == "replayed" => Ok(self
                .issue_replay(&record.org_id, principal, key, payload)
                .await?
                .unwrap_or(IssueOutcome::KeyReused)),
            Err(error) => Err(err(error)),
        }
    }

    /// D47: a credential minted outside `/v1/tokens` (a device login), with
    /// its management metadata.
    pub async fn insert_issued(
        &self,
        record: &CredentialRecord,
        metadata: &TokenMetadata,
    ) -> Result<(), CredentialError> {
        self.db
            .run(insert_statement(record, Some(metadata))?)
            .await
            .map(|_| ())
            .map_err(err)
    }

    pub async fn metadata(
        &self,
        org: &OrganizationId,
        id: &TokenId,
    ) -> Result<TokenMetadata, CredentialError> {
        let rows = self
            .db
            .all(
                prepare("SELECT payload FROM credential WHERE org_id = ? AND id = ?")
                    .bind(org.as_str())
                    .bind(id.as_str()),
            )
            .await
            .map_err(err)?;
        rows.first().map_or(Ok(TokenMetadata::default()), |row| {
            metadata_of(row.text(0).map_err(err)?)
        })
    }
    /// The organization's live credentials with their metadata, by ID.
    pub async fn list_org(
        &self,
        org: &OrganizationId,
    ) -> Result<Vec<(CredentialRecord, TokenMetadata)>, CredentialError> {
        self.db
            .all(
                prepare(
                    "SELECT payload, revoked_at, last_used_at FROM credential \
                     WHERE org_id = ? AND revoked_at IS NULL ORDER BY id",
                )
                .bind(org.as_str()),
            )
            .await
            .map_err(err)?
            .iter()
            .map(|row| Ok((record_of(row)?, metadata_of(row.text(0).map_err(err)?)?)))
            .collect()
    }

    /// A device code's record by the CLI's code. Grants nothing.
    pub async fn device_code_by_device_hash(
        &self,
        device_code_hash: &SecretHash,
    ) -> Result<Option<DeviceCodeRecord>, CredentialError> {
        self.device_code("device_code_hash", device_code_hash)
            .await
            .map(|found| found.map(|(record, _)| record))
    }

    async fn device_code(
        &self,
        column: &str,
        hash: &SecretHash,
    ) -> Result<Option<(DeviceCodeRecord, String)>, CredentialError> {
        self.db
            .all(
                prepare(format!(
                    "SELECT payload FROM device_code WHERE {column} = ?"
                ))
                .bind(&hash.as_bytes()[..]),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| Ok((row.json(0).map_err(err)?, row.string(0).map_err(err)?)))
            .transpose()
    }

    /// Read a code by `column`, change it with `f`, and write it back only
    /// if its stored payload is still the one read.
    async fn compare_and_set<T>(
        &self,
        column: &str,
        hash: &SecretHash,
        f: impl Fn(Option<&mut DeviceCodeRecord>) -> T,
    ) -> Result<T, CredentialError> {
        for _ in 0..CAS_ATTEMPTS {
            let found = self.device_code(column, hash).await?;
            let Some((mut record, before)) = found else {
                return Ok(f(None));
            };
            let unchanged = record.clone();
            let out = f(Some(&mut record));
            if record == unchanged {
                return Ok(out);
            }
            let payload = serde_json::to_string(&record)
                .map_err(|_| CredentialError::Backend("d1.encode"))?;
            let changed = self
                .db
                .run(
                    prepare(format!(
                        "UPDATE device_code SET payload = ?, org_id = COALESCE(org_id, ?) \
                         WHERE {column} = ? AND payload = ?"
                    ))
                    .bind(payload)
                    .bind(org_of(&record).map(str::to_owned))
                    .bind(&hash.as_bytes()[..])
                    .bind(before),
                )
                .await
                .map_err(err)?;
            if changed == 1 {
                return Ok(out);
            }
        }
        Err(CredentialError::Backend("d1.retry"))
    }

    /// CF2-9: route an execution or refresh token's hash to its execution,
    /// written before the mint or refresh reaches the ExecutionDO. Writing
    /// the same route again is a no-op; a hash routed elsewhere is refused.
    pub async fn route_execution_token(
        &self,
        hash: &SecretHash,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<(), CredentialError> {
        let statements = vec![
            prepare(
                "INSERT INTO execution_token_route (hash, org_id, project_id, execution_id, \
                 created_at_ms) VALUES (?, ?, ?, ?, ?) ON CONFLICT (hash) DO NOTHING",
            )
            .bind(&hash.as_bytes()[..])
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str())
            .bind(execution.as_str())
            .bind(self.db.now_ms()),
            guard(
                "duplicate",
                "EXISTS (SELECT 1 FROM execution_token_route WHERE hash = ? AND org_id = ? \
                 AND project_id = ? AND execution_id = ?)",
            )
            .bind(&hash.as_bytes()[..])
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str())
            .bind(execution.as_str()),
        ];
        self.db.batch(statements).await.map(|_| ()).map_err(err)
    }

    /// The execution (and its tenant) a token hash was routed to. It routes
    /// only: the ExecutionDO rechecks the token itself.
    pub async fn execution_route(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<(TenantScope, ExecutionId)>, CredentialError> {
        self.db
            .all(
                prepare(
                    "SELECT org_id, project_id, execution_id FROM execution_token_route \
                     WHERE hash = ?",
                )
                .bind(&hash.as_bytes()[..]),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok((
                    TenantScope {
                        org_id: row.parse(0).map_err(err)?,
                        project_id: row.parse(1).map_err(err)?,
                    },
                    row.parse(2).map_err(err)?,
                ))
            })
            .transpose()
    }
}

fn org_of(record: &DeviceCodeRecord) -> Option<&str> {
    match &record.state {
        DeviceCodeState::Approved(grant) => Some(grant.org_id.as_str()),
        _ => None,
    }
}

impl<D: D1> DeviceCodeStore for D1Credentials<D> {
    async fn insert_device_code(&self, record: DeviceCodeRecord) -> Result<(), CredentialError> {
        let payload =
            serde_json::to_string(&record).map_err(|_| CredentialError::Backend("d1.encode"))?;
        self.db
            .batch(vec![
                prepare("DELETE FROM device_code WHERE expires_at < ?")
                    .bind(record.created_at_ms.saturating_sub(PURGE_AFTER_MS)),
                prepare(
                    "INSERT INTO device_code (device_code_hash, user_code_hash, org_id, \
                     expires_at, payload) VALUES (?, ?, ?, ?, ?)",
                )
                .bind(&record.device_code_hash.as_bytes()[..])
                .bind(&record.user_code_hash.as_bytes()[..])
                .bind(org_of(&record).map(str::to_owned))
                .bind(record.expires_at_ms)
                .bind(payload),
            ])
            .await
            .map(|_| ())
            .map_err(err)
    }
    async fn device_code_by_user_code(
        &self,
        user_code_hash: &SecretHash,
    ) -> Result<Option<DeviceCodeRecord>, CredentialError> {
        self.device_code("user_code_hash", user_code_hash)
            .await
            .map(|found| found.map(|(record, _)| record))
    }
    async fn poll_device_code(
        &self,
        device_code_hash: &SecretHash,
        token_id: &TokenId,
        now_ms: i64,
    ) -> Result<DevicePoll, CredentialError> {
        self.compare_and_set("device_code_hash", device_code_hash, |record| {
            record.map_or(DevicePoll::Expired, |r| apply_poll(r, token_id, now_ms))
        })
        .await
    }
    async fn decide_device_code(
        &self,
        org: &OrganizationId,
        user_code_hash: &SecretHash,
        decision: DeviceDecision,
        now_ms: i64,
    ) -> Result<bool, CredentialError> {
        self.compare_and_set("user_code_hash", user_code_hash, |record| {
            record.is_some_and(|r| apply_decision(r, org, decision.clone(), now_ms))
        })
        .await
    }
}

/// CF11: forget a deleted run's execution-token routes. The routes are the
/// control database's, so a run's deletion (on its organization's shard)
/// cannot drop them in its own batch; its purge does, before
/// `retention::purge_done`, and a repeat is a no-op.
pub async fn forget_execution_routes<D: D1>(
    control: &D,
    execution: &ExecutionId,
) -> Result<(), runbook_core::StoreError> {
    control
        .batch(vec![
            prepare("DELETE FROM execution_token_route WHERE execution_id = ?")
                .bind(execution.as_str()),
        ])
        .await
        .map(|_| ())
        .map_err(|_| runbook_core::StoreError::Backend("d1.token_route"))
}
