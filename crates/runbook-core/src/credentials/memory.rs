//! Single-threaded adapter used by the shared conformance cases.
use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
#[derive(Clone, Default)]
pub struct MemoryCredentials {
    fail_next_lookup: Rc<Cell<bool>>,
    records: Rc<RefCell<BTreeMap<TokenId, CredentialRecord>>>,
    device_codes: Rc<RefCell<Vec<crate::device_codes::DeviceCodeRecord>>>,
}

impl MemoryCredentials {
    /// Simulate a backend outage on exactly one hash lookup.
    pub fn fail_next_lookup(&self) {
        self.fail_next_lookup.set(true);
    }
}

use crate::device_codes::{
    DeviceCodeRecord, DeviceCodeStore, DeviceDecision, DevicePoll, apply_decision, apply_poll,
};
impl DeviceCodeStore for MemoryCredentials {
    async fn insert_device_code(&self, record: DeviceCodeRecord) -> Result<(), CredentialError> {
        let mut rows = self.device_codes.borrow_mut();
        if rows.iter().any(|r| {
            r.device_code_hash == record.device_code_hash
                || r.user_code_hash == record.user_code_hash
        }) {
            return Err(CredentialError::Duplicate);
        }
        rows.push(record);
        Ok(())
    }
    async fn device_code_by_user_code(
        &self,
        user_code_hash: &SecretHash,
    ) -> Result<Option<DeviceCodeRecord>, CredentialError> {
        Ok(self
            .device_codes
            .borrow()
            .iter()
            .find(|r| &r.user_code_hash == user_code_hash)
            .cloned())
    }
    async fn poll_device_code(
        &self,
        device_code_hash: &SecretHash,
        token_id: &TokenId,
        now_ms: i64,
    ) -> Result<DevicePoll, CredentialError> {
        let mut rows = self.device_codes.borrow_mut();
        Ok(
            match rows
                .iter_mut()
                .find(|r| &r.device_code_hash == device_code_hash)
            {
                Some(record) => apply_poll(record, token_id, now_ms),
                None => DevicePoll::Expired,
            },
        )
    }
    async fn decide_device_code(
        &self,
        org: &OrganizationId,
        user_code_hash: &SecretHash,
        decision: DeviceDecision,
        now_ms: i64,
    ) -> Result<bool, CredentialError> {
        let mut rows = self.device_codes.borrow_mut();
        Ok(rows
            .iter_mut()
            .find(|r| &r.user_code_hash == user_code_hash)
            .is_some_and(|record| apply_decision(record, org, decision, now_ms)))
    }
}
impl CredentialStore for MemoryCredentials {
    async fn insert(&self, record: CredentialRecord) -> Result<(), CredentialError> {
        record.validate()?;
        let mut rows = self.records.borrow_mut();
        if rows.contains_key(&record.id) || rows.values().any(|r| r.hash == record.hash) {
            return Err(CredentialError::Duplicate);
        }
        rows.insert(record.id.clone(), record);
        Ok(())
    }
    async fn find_by_hash(
        &self,
        hash: &SecretHash,
    ) -> Result<Option<CredentialRecord>, CredentialError> {
        if self.fail_next_lookup.replace(false) {
            return Err(CredentialError::Backend(
                "injected credential lookup failure",
            ));
        }
        Ok(self
            .records
            .borrow()
            .values()
            .find(|r| &r.hash == hash)
            .cloned())
    }
    async fn find_by_id(&self, id: &TokenId) -> Result<Option<CredentialRecord>, CredentialError> {
        Ok(self.records.borrow().get(id).cloned())
    }
    async fn revoke(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> Result<bool, CredentialError> {
        let mut rows = self.records.borrow_mut();
        let Some(r) = rows
            .get_mut(id)
            .filter(|r| &r.org_id == org && r.revoked_at.is_none())
        else {
            return Ok(false);
        };
        r.revoked_at = Some(now);
        Ok(true)
    }
    async fn touch_last_used(
        &self,
        org: &OrganizationId,
        id: &TokenId,
        now: i64,
    ) -> Result<bool, CredentialError> {
        let mut rows = self.records.borrow_mut();
        let Some(r) = rows
            .get_mut(id)
            .filter(|r| &r.org_id == org && should_touch_last_used(r.last_used_at, now))
        else {
            return Ok(false);
        };
        r.last_used_at = Some(now);
        Ok(true)
    }
}
