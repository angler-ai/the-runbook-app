//! Synchronous in-memory notify store under a settable clock, mirroring
//! the SQL adapters' atomicity and lease fencing.
use super::store::*;
use crate::{StoreError, notify::EmailPreference};
use runbook_model::UserId;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

#[derive(Clone)]
struct LeasedEmail {
    record: EmailRecord,
    generation: u64,
    lease_deadline_at_ms: Option<i64>,
}

#[derive(Clone, Default)]
struct Db {
    preferences: BTreeMap<UserId, (EmailPreference, Option<String>)>,
    emails: BTreeMap<String, LeasedEmail>,
    slack: BTreeMap<(String, String), String>,
}

#[derive(Clone, Default)]
pub struct MemoryNotify {
    db: Rc<RefCell<Db>>,
    clock: Rc<Cell<i64>>,
}

impl MemoryNotify {
    pub fn new(now_ms: i64) -> Self {
        let store = Self::default();
        store.clock.set(now_ms);
        store
    }
    pub fn set_time(&self, now_ms: i64) {
        self.clock.set(now_ms);
    }
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut Db) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut committed = self.db.borrow_mut();
        let mut staged = committed.clone();
        let result = f(&mut staged)?;
        *committed = staged;
        Ok(result)
    }
}

fn claimable(entry: &LeasedEmail, now: i64) -> bool {
    entry
        .lease_deadline_at_ms
        .is_none_or(|deadline| deadline <= now)
}

impl NotifyStore for MemoryNotify {
    async fn notify_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.clock.get())
    }
    async fn email_preference(
        &self,
        user_id: &UserId,
    ) -> Result<Option<(EmailPreference, Option<String>)>, StoreError> {
        Ok(self.db.borrow().preferences.get(user_id).cloned())
    }
    async fn set_email_preference(
        &self,
        user_id: &UserId,
        preference: EmailPreference,
    ) -> Result<(), StoreError> {
        self.transaction(|db| {
            let salt = db
                .preferences
                .get(user_id)
                .and_then(|(_, salt)| salt.clone());
            db.preferences.insert(user_id.clone(), (preference, salt));
            Ok(())
        })
    }
    async fn ensure_unsubscribe_salt(
        &self,
        user_id: &UserId,
        candidate: String,
    ) -> Result<String, StoreError> {
        self.transaction(|db| {
            let entry = db
                .preferences
                .entry(user_id.clone())
                .or_insert((EmailPreference::default(), None));
            if entry.1.is_none() {
                entry.1 = Some(candidate);
            }
            Ok(entry.1.clone().unwrap_or_default())
        })
    }
    async fn record_emails(&self, records: Vec<EmailRecord>) -> Result<usize, StoreError> {
        self.transaction(|db| {
            let mut inserted = 0;
            for record in records {
                let duplicate = db.emails.values().any(|entry| {
                    entry.record.event_id == record.event_id
                        && entry.record.user_id == record.user_id
                });
                if duplicate || db.emails.contains_key(&record.id) {
                    continue;
                }
                db.emails.insert(
                    record.id.clone(),
                    LeasedEmail {
                        record,
                        generation: 0,
                        lease_deadline_at_ms: None,
                    },
                );
                inserted += 1;
            }
            Ok(inserted)
        })
    }
    async fn claim_emails(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<EmailLease>, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let mut keys = db
                .emails
                .iter()
                .filter(|(_, entry)| {
                    entry.record.state == EmailState::Pending
                        && entry.record.next_attempt_at_ms <= now
                        && claimable(entry, now)
                })
                .map(|(id, entry)| (entry.record.created_at_ms, id.clone()))
                .collect::<Vec<_>>();
            keys.sort();
            let mut keys: Vec<String> = keys.into_iter().map(|(_, id)| id).collect();
            keys.truncate(limit);
            let mut leases = vec![];
            for key in keys {
                let entry = db.emails.get_mut(&key).expect("collected above");
                entry.generation += 1;
                entry.lease_deadline_at_ms = Some(now.saturating_add(lease_ms as i64));
                leases.push(EmailLease {
                    record: entry.record.clone(),
                    generation: entry.generation,
                    claimed_at_ms: now,
                });
            }
            Ok(leases)
        })
    }
    async fn complete_email(
        &self,
        id: &str,
        generation: u64,
        completion: EmailCompletion,
    ) -> Result<(), StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let entry = db
                .emails
                .get_mut(id)
                .filter(|entry| {
                    entry.generation == generation
                        && entry.lease_deadline_at_ms.is_some_and(|d| d > now)
                })
                .ok_or(StoreError::InvalidBatch("notify.stale_lease"))?;
            entry.lease_deadline_at_ms = None;
            entry.record.attempts += 1;
            if completion.sent {
                entry.record.state = EmailState::Sent;
                entry.record.sent_at_ms = Some(now);
                entry.record.failure = None;
            } else if let Some(next) = completion.next_attempt_at_ms {
                entry.record.state = EmailState::Pending;
                entry.record.next_attempt_at_ms = next;
                entry.record.failure = completion.failure;
            } else {
                entry.record.state = EmailState::Failed;
                entry.record.failure = completion.failure;
            }
            Ok(())
        })
    }
    async fn claim_digest(
        &self,
        before_ms: i64,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<EmailLease>, StoreError> {
        let now = self.clock.get();
        self.transaction(|db| {
            let mut keys = db
                .emails
                .iter()
                .filter(|(_, entry)| {
                    entry.record.state == EmailState::Digest
                        && entry.record.created_at_ms < before_ms
                        && claimable(entry, now)
                })
                .map(|(id, entry)| (entry.record.created_at_ms, id.clone()))
                .collect::<Vec<_>>();
            keys.sort();
            let mut keys: Vec<String> = keys.into_iter().map(|(_, id)| id).collect();
            keys.truncate(limit);
            let mut leases = vec![];
            for key in keys {
                let entry = db.emails.get_mut(&key).expect("collected above");
                entry.generation += 1;
                entry.lease_deadline_at_ms = Some(now.saturating_add(lease_ms as i64));
                leases.push(EmailLease {
                    record: entry.record.clone(),
                    generation: entry.generation,
                    claimed_at_ms: now,
                });
            }
            Ok(leases)
        })
    }
    async fn slack_url(
        &self,
        scope: &crate::catalog::TenantScope,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .db
            .borrow()
            .slack
            .get(&(scope.org_id.to_string(), scope.project_id.to_string()))
            .cloned())
    }
    async fn set_slack_url(
        &self,
        scope: &crate::catalog::TenantScope,
        url: Option<String>,
    ) -> Result<(), StoreError> {
        self.transaction(|db| {
            let key = (scope.org_id.to_string(), scope.project_id.to_string());
            match url {
                Some(url) => {
                    db.slack.insert(key, url);
                }
                None => {
                    db.slack.remove(&key);
                }
            }
            Ok(())
        })
    }
    async fn mark_digested(&self, ids: Vec<(String, u64)>, at_ms: i64) -> Result<(), StoreError> {
        self.transaction(|db| {
            for (id, generation) in &ids {
                let entry = db
                    .emails
                    .get_mut(id)
                    .filter(|entry| entry.generation == *generation)
                    .ok_or(StoreError::InvalidBatch("notify.stale_lease"))?;
                entry.lease_deadline_at_ms = None;
                entry.record.state = EmailState::Sent;
                entry.record.sent_at_ms = Some(at_ms);
            }
            Ok(())
        })
    }
}
