//! Execution credentials (B7a/D30/D31): families and token rows live with
//! the execution and change only inside its commits, so validity, rotation,
//! reuse revocation and closure share one transaction (PRD §14 Rules).
//! Only SHA-256 hashes are stored; secrets never reach this object.
use crate::{
    DoSql, DoStore,
    sql::{Columns, decode, encode},
};
use runbook_core::{
    StoreError,
    credentials::SecretHash,
    execution_credentials::{
        CredentialFamily, CredentialWrite, ExecutionCredentialRecord, ExecutionCredentialStore,
    },
};
use runbook_model::{ExecutionId, TokenId};

fn invalid(code: &'static str) -> StoreError {
    StoreError::InvalidBatch(code)
}

impl<B: DoSql> DoStore<B> {
    fn family_row(&self, family_id: &str) -> Result<Option<CredentialFamily>, StoreError> {
        self.sql
            .query(
                "SELECT body FROM credential_family WHERE family_id = ?",
                &[family_id.into()],
            )?
            .first()
            .map(|row| decode(row.text(0)?))
            .transpose()
    }
    fn token_row(&self, token_id: &str) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        self.sql
            .query(
                "SELECT body FROM execution_credential WHERE token_id = ?",
                &[token_id.into()],
            )?
            .first()
            .map(|row| decode(row.text(0)?))
            .transpose()
    }
    fn hash_taken(&self, hash: &SecretHash) -> Result<bool, StoreError> {
        Ok(!self
            .sql
            .query(
                "SELECT 1 FROM execution_credential WHERE hash = ?",
                &[hash.as_bytes().to_vec().into()],
            )?
            .is_empty())
    }
    fn put_token(
        &self,
        id: &ExecutionId,
        record: &ExecutionCredentialRecord,
    ) -> Result<(), StoreError> {
        self.sql.exec(
            "INSERT INTO execution_credential (org_id, project_id, execution_id, token_id, hash, \
             family_id, body) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (token_id) DO UPDATE SET body = excluded.body",
            &self.owned(
                id,
                vec![
                    record.token_id.as_str().into(),
                    record.hash.as_bytes().to_vec().into(),
                    record.family_id.as_str().into(),
                    encode(record)?,
                ],
            ),
        )
    }

    /// Refuse a credential write before anything is written (as native).
    pub(crate) fn precheck_credential(
        &self,
        id: &ExecutionId,
        write: &CredentialWrite,
    ) -> Result<(), StoreError> {
        if write.execution_id() != id {
            return Err(invalid("credentials.execution_mismatch"));
        }
        match write {
            CredentialWrite::IssueFamily {
                family,
                access,
                refresh,
            } => {
                let tenant_ok =
                    |org: &runbook_model::OrganizationId, project: &runbook_model::ProjectId| {
                        org == &self.scope.org_id && project == &self.scope.project_id
                    };
                if !tenant_ok(&family.org_id, &family.project_id)
                    || [access, refresh]
                        .iter()
                        .any(|r| !tenant_ok(&r.org_id, &r.project_id) || &r.execution_id != id)
                {
                    return Err(invalid("credentials.scope_mismatch"));
                }
                let existing = self.sql.query(
                    "SELECT body FROM credential_family WHERE idempotency_key = ?",
                    &[family.idempotency_key.as_str().into()],
                )?;
                if let Some(row) = existing.first() {
                    let existing: CredentialFamily = decode(row.text(0)?)?;
                    return Err(invalid(if existing.request_hash == family.request_hash {
                        "credentials.already_issued"
                    } else {
                        "request.idempotency_key_reused"
                    }));
                }
                if self.family_row(&family.family_id)?.is_some()
                    || access.hash == refresh.hash
                    || access.token_id == refresh.token_id
                {
                    return Err(invalid("credentials.duplicate"));
                }
                for record in [access, refresh] {
                    if self.token_row(record.token_id.as_str())?.is_some()
                        || self.hash_taken(&record.hash)?
                    {
                        return Err(invalid("credentials.duplicate"));
                    }
                }
            }
            CredentialWrite::Rotate {
                consume_token_id,
                access,
                refresh,
                ..
            } => {
                let current = self
                    .token_row(consume_token_id.as_str())?
                    .ok_or(StoreError::NotFound)?;
                if current.consumed_at_ms.is_some() {
                    return Err(invalid("credentials.refresh_reused"));
                }
                if current.revoked_at_ms.is_some()
                    || access.generation != current.generation + 1
                    || self.token_row(access.token_id.as_str())?.is_some()
                    || self.token_row(refresh.token_id.as_str())?.is_some()
                {
                    return Err(invalid("credentials.generation_conflict"));
                }
                if [access, refresh].iter().any(|r| {
                    r.family_id != current.family_id
                        || &r.execution_id != id
                        || r.org_id != self.scope.org_id
                        || r.project_id != self.scope.project_id
                }) {
                    return Err(invalid("credentials.scope_mismatch"));
                }
                if access.hash == refresh.hash
                    || self.hash_taken(&access.hash)?
                    || self.hash_taken(&refresh.hash)?
                {
                    return Err(invalid("credentials.duplicate"));
                }
            }
            CredentialWrite::RevokeFamily { family_id, .. } => {
                if self
                    .family_row(family_id)?
                    .is_none_or(|family| &family.execution_id != id)
                {
                    return Err(StoreError::NotFound);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn apply_credential(
        &self,
        id: &ExecutionId,
        write: &CredentialWrite,
    ) -> Result<(), StoreError> {
        match write {
            CredentialWrite::IssueFamily {
                family,
                access,
                refresh,
            } => {
                self.sql.exec(
                    "INSERT INTO credential_family (org_id, project_id, execution_id, family_id, \
                     idempotency_key, body) VALUES (?, ?, ?, ?, ?, ?)",
                    &self.owned(
                        id,
                        vec![
                            family.family_id.as_str().into(),
                            family.idempotency_key.as_str().into(),
                            encode(family.as_ref())?,
                        ],
                    ),
                )?;
                self.put_token(id, access)?;
                self.put_token(id, refresh)
            }
            CredentialWrite::Rotate {
                consume_token_id,
                consumed_at_ms,
                access,
                refresh,
            } => {
                let mut current = self
                    .token_row(consume_token_id.as_str())?
                    .ok_or(StoreError::NotFound)?;
                current.consumed_at_ms = Some(*consumed_at_ms);
                self.put_token(id, &current)?;
                self.put_token(id, access)?;
                self.put_token(id, refresh)
            }
            CredentialWrite::RevokeFamily {
                family_id,
                revoked_at_ms,
                ..
            } => self.revoke_family(id, family_id, *revoked_at_ms),
        }
    }

    fn revoke_family(
        &self,
        id: &ExecutionId,
        family_id: &str,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        if let Some(mut family) = self.family_row(family_id)?
            && family.revoked_at_ms.is_none()
        {
            family.revoked_at_ms = Some(at_ms);
            self.sql.exec(
                "UPDATE credential_family SET body = ? WHERE family_id = ?",
                &[encode(&family)?, family_id.into()],
            )?;
        }
        let tokens = self.sql.query(
            "SELECT body FROM execution_credential WHERE family_id = ?",
            &[family_id.into()],
        )?;
        for row in tokens {
            let mut record: ExecutionCredentialRecord = decode(row.text(0)?)?;
            if record.revoked_at_ms.is_none() {
                record.revoked_at_ms = Some(at_ms);
                self.put_token(id, &record)?;
            }
        }
        Ok(())
    }

    /// Closure revokes every family of this execution (B7a).
    pub(crate) fn revoke_all_credentials(
        &self,
        id: &ExecutionId,
        at_ms: i64,
    ) -> Result<(), StoreError> {
        let families = self
            .sql
            .query("SELECT family_id FROM credential_family", &[])?;
        for row in families {
            self.revoke_family(id, row.text(0)?, at_ms)?;
        }
        Ok(())
    }

    fn visible<T>(&self, value: Option<T>) -> Result<Option<T>, StoreError> {
        Ok(if self.own_head()?.is_some() {
            value
        } else {
            None
        })
    }
}

impl<B: DoSql> ExecutionCredentialStore for DoStore<B> {
    async fn credential_family(
        &self,
        family_id: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        let family = self.family_row(family_id)?;
        self.visible(family)
    }
    async fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        let record = self
            .sql
            .query(
                "SELECT body FROM execution_credential WHERE hash = ?",
                &[hash.as_bytes().to_vec().into()],
            )?
            .first()
            .map(|row| decode(row.text(0)?))
            .transpose()?;
        self.visible(record)
    }
    async fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> Result<Option<ExecutionCredentialRecord>, StoreError> {
        let record = self.token_row(token_id.as_str())?;
        self.visible(record)
    }
    async fn family_by_idempotency_key(
        &self,
        execution_id: &ExecutionId,
        key: &str,
    ) -> Result<Option<CredentialFamily>, StoreError> {
        let family = self
            .sql
            .query(
                "SELECT body FROM credential_family WHERE idempotency_key = ? AND execution_id = ?",
                &[key.into(), execution_id.as_str().into()],
            )?
            .first()
            .map(|row| decode(row.text(0)?))
            .transpose()?;
        self.visible(family)
    }
}
