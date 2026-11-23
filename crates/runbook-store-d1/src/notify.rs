//! NOTIFY1 on D1: preferences and the email outbox. Claims are one
//! UPDATE ... RETURNING statement, so two workers cannot take the same
//! row; completions compare the generation and an unexpired lease.
use crate::sql::{Columns, D1, Statement, prepare};
use runbook_core::{
    StoreError,
    notify::{
        EmailCompletion, EmailLease, EmailPreference, EmailRecord, EmailState, NotifyStore,
        ProjectSlackSettings,
    },
};
use runbook_model::UserId;

#[derive(Clone)]
pub struct D1Notify<D: D1> {
    db: D,
}
impl<D: D1> D1Notify<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }
}

fn err(error: crate::sql::D1Error) -> StoreError {
    crate::store_error(error)
}

fn slack_settings(row: &crate::sql::Row) -> Result<ProjectSlackSettings, StoreError> {
    Ok(ProjectSlackSettings {
        target: row.string(0).map_err(err)?,
        notify_run_started: row.integer(1).map_err(err)? != 0,
    })
}

fn preference_name(preference: EmailPreference) -> &'static str {
    match preference {
        EmailPreference::All => "all",
        EmailPreference::ApprovalsOnly => "approvals_only",
        EmailPreference::DailySummary => "daily_summary",
        EmailPreference::None => "none",
    }
}
fn parse_preference(name: &str) -> Result<EmailPreference, StoreError> {
    match name {
        "all" => Ok(EmailPreference::All),
        "approvals_only" => Ok(EmailPreference::ApprovalsOnly),
        "daily_summary" => Ok(EmailPreference::DailySummary),
        "none" => Ok(EmailPreference::None),
        _ => Err(StoreError::Backend("d1.invalid_email_preference")),
    }
}
fn state_name(state: EmailState) -> &'static str {
    match state {
        EmailState::Pending => "pending",
        EmailState::Digest => "digest",
        EmailState::Sent => "sent",
        EmailState::Failed => "failed",
    }
}
fn parse_state(name: &str) -> Result<EmailState, StoreError> {
    match name {
        "pending" => Ok(EmailState::Pending),
        "digest" => Ok(EmailState::Digest),
        "sent" => Ok(EmailState::Sent),
        "failed" => Ok(EmailState::Failed),
        _ => Err(StoreError::Backend("d1.invalid_email_state")),
    }
}

const COLUMNS: &str = "id, org_id, user_id, email, kind, execution_id, event_id, subject, \
     text_body, html_body, state, attempts, next_attempt_at_ms, created_at_ms, sent_at_ms, failure";

fn record_of(row: &crate::sql::Row) -> Result<EmailRecord, StoreError> {
    Ok(EmailRecord {
        id: row.string(0).map_err(err)?,
        org_id: row.parse(1).map_err(err)?,
        user_id: row.optional_parse(2).map_err(err)?,
        email: row.string(3).map_err(err)?,
        kind: row.string(4).map_err(err)?,
        execution_id: row.optional_parse(5).map_err(err)?,
        event_id: row.string(6).map_err(err)?,
        subject: row.string(7).map_err(err)?,
        text: row.string(8).map_err(err)?,
        html: row.string(9).map_err(err)?,
        state: parse_state(row.text(10).map_err(err)?)?,
        attempts: u32::try_from(row.unsigned(11).map_err(err)?)
            .map_err(|_| StoreError::Backend("d1.invalid_email_attempts"))?,
        next_attempt_at_ms: row.integer(12).map_err(err)?,
        created_at_ms: row.integer(13).map_err(err)?,
        sent_at_ms: row.optional_integer(14).map_err(err)?,
        failure: row.optional_text(15).map_err(err)?,
    })
}

impl<D: D1> D1Notify<D> {
    async fn claim_state(
        &self,
        state: EmailState,
        limit: usize,
        lease_ms: u64,
        before_ms: Option<i64>,
    ) -> Result<Vec<EmailLease>, StoreError> {
        if lease_ms == 0 {
            return Err(StoreError::InvalidBatch("notify.invalid_lease"));
        }
        let now = self.db.now_ms();
        let deadline = now
            .checked_add(
                i64::try_from(lease_ms)
                    .map_err(|_| StoreError::InvalidBatch("notify.lease_overflow"))?,
            )
            .ok_or(StoreError::InvalidBatch("notify.lease_overflow"))?;
        let due = if before_ms.is_some() {
            "AND created_at_ms < ?"
        } else {
            "AND next_attempt_at_ms <= ?"
        };
        let sql = format!(
            "UPDATE email_delivery SET generation = generation + 1, lease_until_ms = ? \
             WHERE id IN (SELECT id FROM email_delivery WHERE state = ? {due} \
             AND (lease_until_ms IS NULL OR lease_until_ms <= ?) \
             ORDER BY created_at_ms, id LIMIT ?) \
             RETURNING {COLUMNS}, generation",
        );
        let statement = prepare(sql)
            .bind(deadline)
            .bind(state_name(state))
            .bind(before_ms.unwrap_or(now))
            .bind(now)
            .bind(
                i64::try_from(limit)
                    .map_err(|_| StoreError::InvalidBatch("notify.invalid_limit"))?,
            );
        let rows = self.db.all(statement).await.map_err(err)?;
        rows.iter()
            .map(|row| {
                Ok(EmailLease {
                    record: record_of(row)?,
                    generation: row.unsigned(16).map_err(err)?,
                    claimed_at_ms: now,
                })
            })
            .collect()
    }
}

impl<D: D1> NotifyStore for D1Notify<D> {
    async fn notify_time_ms(&self) -> Result<i64, StoreError> {
        Ok(self.db.now_ms())
    }
    async fn email_preference(
        &self,
        user_id: &UserId,
    ) -> Result<Option<(EmailPreference, Option<String>)>, StoreError> {
        self.db
            .all(
                prepare("SELECT preference, token_salt FROM email_preference WHERE user_id = ?")
                    .bind(user_id.as_str()),
            )
            .await
            .map_err(err)?
            .first()
            .map(|row| {
                Ok((
                    parse_preference(row.text(0).map_err(err)?)?,
                    row.optional_text(1).map_err(err)?,
                ))
            })
            .transpose()
    }
    async fn set_email_preference(
        &self,
        user_id: &UserId,
        preference: EmailPreference,
    ) -> Result<(), StoreError> {
        self.db
            .run(
                prepare(
                    "INSERT INTO email_preference (user_id, preference) VALUES (?, ?) \
                     ON CONFLICT(user_id) DO UPDATE SET preference = excluded.preference",
                )
                .bind(user_id.as_str())
                .bind(preference_name(preference)),
            )
            .await
            .map_err(err)?;
        Ok(())
    }
    async fn ensure_unsubscribe_salt(
        &self,
        user_id: &UserId,
        candidate: String,
    ) -> Result<String, StoreError> {
        let rows = self
            .db
            .all(
                prepare(
                    "INSERT INTO email_preference (user_id, preference, token_salt) VALUES (?, 'all', ?) \
                     ON CONFLICT(user_id) DO UPDATE SET token_salt = COALESCE(email_preference.token_salt, excluded.token_salt) \
                     RETURNING token_salt",
                )
                .bind(user_id.as_str())
                .bind(candidate),
            )
            .await
            .map_err(err)?;
        rows.first()
            .and_then(|row| row.optional_text(0).map_err(err).transpose())
            .transpose()?
            .ok_or(StoreError::Backend("d1.notify_salt"))
    }
    async fn record_emails(&self, records: Vec<EmailRecord>) -> Result<usize, StoreError> {
        let mut inserted = 0usize;
        for record in &records {
            inserted += self
                .db
                .run(
                    prepare(
                        "INSERT OR IGNORE INTO email_delivery (id, org_id, user_id, email, kind, execution_id, event_id, subject, text_body, html_body, state, attempts, next_attempt_at_ms, created_at_ms, sent_at_ms, failure) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(record.id.clone())
                    .bind(record.org_id.as_str())
                    .bind(record.user_id.as_ref().map(|id| id.as_str()))
                    .bind(record.email.clone())
                    .bind(record.kind.clone())
                    .bind(record.execution_id.as_ref().map(|id| id.as_str()))
                    .bind(record.event_id.clone())
                    .bind(record.subject.clone())
                    .bind(record.text.clone())
                    .bind(record.html.clone())
                    .bind(state_name(record.state))
                    .bind(record.attempts)
                    .bind(record.next_attempt_at_ms)
                    .bind(record.created_at_ms)
                    .bind(record.sent_at_ms)
                    .bind(record.failure.clone()),
                )
                .await
                .map_err(err)? as usize;
        }
        Ok(inserted)
    }
    async fn claim_emails(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<EmailLease>, StoreError> {
        self.claim_state(EmailState::Pending, limit, lease_ms, None)
            .await
    }
    async fn complete_email(
        &self,
        id: &str,
        generation: u64,
        completion: EmailCompletion,
    ) -> Result<(), StoreError> {
        let now = self.db.now_ms();
        let (state, sent_at, next) = if completion.sent {
            (EmailState::Sent, Some(now), None)
        } else if let Some(next) = completion.next_attempt_at_ms {
            (EmailState::Pending, None, Some(next))
        } else {
            (EmailState::Failed, None, None)
        };
        let changed = self
            .db
            .run(
                prepare(
                    "UPDATE email_delivery SET state = ?, attempts = attempts + 1, \
                     next_attempt_at_ms = COALESCE(?, next_attempt_at_ms), sent_at_ms = ?, failure = ?, lease_until_ms = NULL \
                     WHERE id = ? AND generation = ? AND lease_until_ms > ?",
                )
                .bind(state_name(state))
                .bind(next)
                .bind(sent_at)
                .bind(completion.failure.clone())
                .bind(id)
                .bind(generation as i64)
                .bind(now),
            )
            .await
            .map_err(err)?;
        if changed == 0 {
            return Err(StoreError::InvalidBatch("notify.stale_lease"));
        }
        Ok(())
    }
    async fn claim_digest(
        &self,
        before_ms: i64,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<EmailLease>, StoreError> {
        self.claim_state(EmailState::Digest, limit, lease_ms, Some(before_ms))
            .await
    }
    async fn slack_settings(
        &self,
        scope: &runbook_core::catalog::TenantScope,
    ) -> Result<Option<ProjectSlackSettings>, StoreError> {
        let rows = self
            .db
            .all(
                prepare("SELECT url, COALESCE(notify_run_started, 0) FROM project_slack WHERE org_id = ? AND project_id = ?")
                    .bind(scope.org_id.as_str())
                    .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?;
        rows.first().map(slack_settings).transpose()
    }
    async fn save_slack_settings(
        &self,
        scope: &runbook_core::catalog::TenantScope,
        target: Option<String>,
        notify_run_started: Option<bool>,
    ) -> Result<Option<ProjectSlackSettings>, StoreError> {
        use crate::sql::Value;
        let flag = notify_run_started.map_or(Value::Null, |flag| Value::Integer(i64::from(flag)));
        let org = scope.org_id.as_str();
        let project = scope.project_id.as_str();
        let query =
            if let Some(target) = target {
                prepare(
                    "INSERT INTO project_slack (org_id, project_id, url, notify_run_started)
                 VALUES (?1, ?2, ?3, COALESCE(?4, 0))
                 ON CONFLICT(org_id, project_id) DO UPDATE SET url = excluded.url,
                     notify_run_started = COALESCE(?4, project_slack.notify_run_started, 0)
                 RETURNING url, notify_run_started",
                )
                .bind(org)
                .bind(project)
                .bind(target)
                .bind(flag)
            } else {
                prepare(
                "UPDATE project_slack SET notify_run_started = COALESCE(?3, notify_run_started, 0)
                 WHERE org_id = ?1 AND project_id = ?2 RETURNING url, notify_run_started",
            ).bind(org).bind(project).bind(flag)
            };
        let rows = self.db.all(query).await.map_err(err)?;
        rows.first().map(slack_settings).transpose()
    }
    async fn remove_slack_settings(
        &self,
        scope: &runbook_core::catalog::TenantScope,
    ) -> Result<(), StoreError> {
        self.db
            .run(
                prepare("DELETE FROM project_slack WHERE org_id = ? AND project_id = ?")
                    .bind(scope.org_id.as_str())
                    .bind(scope.project_id.as_str()),
            )
            .await
            .map_err(err)?;
        Ok(())
    }
    async fn mark_digested(&self, ids: Vec<(String, u64)>, at_ms: i64) -> Result<(), StoreError> {
        let statements: Vec<Statement> = ids
            .iter()
            .map(|(id, generation)| {
                prepare(
                    "UPDATE email_delivery SET state = 'sent', sent_at_ms = ?, lease_until_ms = NULL \
                     WHERE id = ? AND generation = ? RETURNING id",
                )
                .bind(at_ms)
                .bind(id.clone())
                .bind(*generation as i64)
            })
            .collect();
        let outcomes = self.db.batch(statements).await.map_err(err)?;
        if outcomes.iter().any(|outcome| outcome.rows.is_empty()) {
            return Err(StoreError::InvalidBatch("notify.stale_lease"));
        }
        Ok(())
    }
}
