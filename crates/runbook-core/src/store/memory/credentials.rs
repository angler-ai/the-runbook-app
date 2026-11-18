//! Memory execution-credential writes, sharing the commit transaction.
use super::Database;
use crate::{StoreError, catalog::TenantScope, execution_credentials::CredentialWrite};
use runbook_model::ExecutionId;

pub(super) fn precheck(
    db: &Database,
    scope: &TenantScope,
    write: &CredentialWrite,
) -> Result<(), StoreError> {
    match write {
        CredentialWrite::IssueFamily {
            family,
            access,
            refresh,
        } => {
            if let Some((_, existing)) = db.credential_families.values().find(|(owner, f)| {
                owner == scope
                    && f.execution_id == family.execution_id
                    && f.idempotency_key == family.idempotency_key
            }) {
                return Err(StoreError::InvalidBatch(
                    if existing.request_hash == family.request_hash {
                        "credentials.already_issued"
                    } else {
                        "request.idempotency_key_reused"
                    },
                ));
            }
            if db.credential_families.contains_key(&family.family_id)
                || [access, refresh].iter().any(|record| {
                    db.exec_credentials.contains_key(&record.token_id)
                        || db.exec_credentials.values().any(|r| r.hash == record.hash)
                })
            {
                return Err(StoreError::InvalidBatch("credentials.duplicate"));
            }
        }
        CredentialWrite::Rotate {
            consume_token_id,
            access,
            refresh,
            ..
        } => {
            let current = db
                .exec_credentials
                .get(consume_token_id)
                .ok_or(StoreError::NotFound)?;
            if current.consumed_at_ms.is_some() {
                return Err(StoreError::InvalidBatch("credentials.refresh_reused"));
            }
            if current.revoked_at_ms.is_some()
                || access.generation != current.generation + 1
                || db.exec_credentials.contains_key(&access.token_id)
                || db.exec_credentials.contains_key(&refresh.token_id)
            {
                return Err(StoreError::InvalidBatch("credentials.generation_conflict"));
            }
        }
        CredentialWrite::RevokeFamily { family_id, .. } => {
            if !db
                .credential_families
                .get(family_id)
                .is_some_and(|(owner, _)| owner == scope)
            {
                return Err(StoreError::NotFound);
            }
        }
    }
    Ok(())
}

pub(super) fn apply(db: &mut Database, write: CredentialWrite) {
    match write {
        CredentialWrite::IssueFamily {
            family,
            access,
            refresh,
        } => {
            // Scope is the family's own tenant (validated by commit scope).
            let scope = TenantScope {
                org_id: family.org_id.clone(),
                project_id: family.project_id.clone(),
            };
            db.exec_credentials.insert(access.token_id.clone(), *access);
            db.exec_credentials
                .insert(refresh.token_id.clone(), *refresh);
            db.credential_families
                .insert(family.family_id.clone(), (scope, *family));
        }
        CredentialWrite::Rotate {
            consume_token_id,
            consumed_at_ms,
            access,
            refresh,
        } => {
            if let Some(current) = db.exec_credentials.get_mut(&consume_token_id) {
                current.consumed_at_ms = Some(consumed_at_ms);
            }
            db.exec_credentials.insert(access.token_id.clone(), *access);
            db.exec_credentials
                .insert(refresh.token_id.clone(), *refresh);
        }
        CredentialWrite::RevokeFamily {
            execution_id,
            family_id,
            revoked_at_ms,
        } => revoke_family(db, &execution_id, &family_id, revoked_at_ms),
    }
}

pub(super) fn revoke_family(
    db: &mut Database,
    execution_id: &ExecutionId,
    family_id: &str,
    at_ms: i64,
) {
    if let Some((_, family)) = db.credential_families.get_mut(family_id) {
        family.revoked_at_ms.get_or_insert(at_ms);
    }
    for record in db.exec_credentials.values_mut() {
        if record.execution_id == *execution_id && record.family_id == family_id {
            record.revoked_at_ms.get_or_insert(at_ms);
        }
    }
}

pub(super) fn revoke_all(db: &mut Database, execution_id: &ExecutionId, at_ms: i64) {
    let families: Vec<String> = db
        .credential_families
        .values()
        .filter(|(_, family)| &family.execution_id == execution_id)
        .map(|(_, family)| family.family_id.clone())
        .collect();
    for family_id in families {
        revoke_family(db, execution_id, &family_id, at_ms);
    }
}
