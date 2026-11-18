//! D3 portable archive rows for memory webhooks: subscriptions only, never
//! their secrets; the delivery log and replay rows stay behind.
use super::*;
use crate::portable::{PortableError, WebhookRow};

pub(crate) struct StagedWebhooks(Db);

fn has_rows(db: &Db, scope: &TenantScope) -> bool {
    db.subscriptions.values().any(|s| &s.scope == scope)
        || db.events.keys().any(|(owner, _)| owner == scope)
        || db.deliveries.values().any(|d| &d.scope == scope)
        || db.replays.keys().any(|(owner, _)| owner == scope)
}

impl MemoryWebhooks {
    pub(crate) fn portable_rows(&self, scope: &TenantScope) -> Vec<WebhookRow> {
        self.db
            .borrow()
            .subscriptions
            .values()
            .filter(|s| &s.scope == scope)
            .map(WebhookRow::from_subscription)
            .collect()
    }
    pub(crate) fn portable_stage(
        &self,
        scope: &TenantScope,
        rows: &[WebhookRow],
    ) -> Result<StagedWebhooks, PortableError> {
        let mut db = self.db.borrow().clone();
        if has_rows(&db, scope) {
            return Err(PortableError::TargetNotEmpty);
        }
        for row in rows {
            if db.subscriptions.contains_key(&row.id) {
                return Err(PortableError::Conflict("import.id_conflict"));
            }
            db.subscriptions
                .insert(row.id.clone(), row.to_subscription(scope));
        }
        Ok(StagedWebhooks(db))
    }
    pub(crate) fn portable_install(&self, staged: StagedWebhooks) {
        *self.db.borrow_mut() = staged.0;
    }
}
