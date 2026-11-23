//! NOTIFY1 on PostgreSQL: preferences and the email outbox. Claims lock
//! due rows with FOR UPDATE SKIP LOCKED and bump the generation that
//! fences completion, on the database clock, like webhook deliveries.
use crate::{
    DatabaseConfig,
    execution::{READ_COMMITTED, finish},
    pool::Pool,
    rows::{connection, db, int, uint},
};
use runbook_core::{
    StoreError,
    notify::{
        EmailCompletion, EmailLease, EmailPreference, EmailRecord, EmailState, NotifyStore,
        ProjectSlackSettings,
    },
};
use runbook_model::UserId;
use std::rc::Rc;
use tokio_postgres::{Client, Row};

fn slack_settings(row: Row) -> ProjectSlackSettings {
    ProjectSlackSettings {
        target: row.get(0),
        notify_run_started: row.get(1),
    }
}

const NOW_MS: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint";
const COLUMNS: &str = "id, org_id, user_id, email, kind, execution_id, event_id, subject, \
     text_body, html_body, state, attempts, next_attempt_at_ms, created_at_ms, sent_at_ms, failure";

#[derive(Clone)]
pub struct PostgresNotify {
    pool: Rc<Pool>,
}
impl PostgresNotify {
    pub fn new(config: DatabaseConfig) -> Self {
        Self {
            pool: Pool::new(config),
        }
    }
    async fn transaction<T>(
        &self,
        body: impl AsyncFnOnce(&Client) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut conn = self.pool.get().await.map_err(connection)?;
        conn.begin(READ_COMMITTED).await.map_err(db)?;
        let result = body(conn.client()).await;
        finish(&mut conn, result).await
    }
}

fn parse_id<T: std::str::FromStr>(text: &str) -> Result<T, StoreError> {
    text.parse()
        .map_err(|_| StoreError::Backend("postgres.invalid_row"))
}

async fn now_ms(c: &Client) -> Result<i64, StoreError> {
    c.query_one(NOW_MS, &[]).await.map(|r| r.get(0)).map_err(db)
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
        _ => Err(StoreError::Backend("postgres.invalid_email_preference")),
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
        _ => Err(StoreError::Backend("postgres.invalid_email_state")),
    }
}

fn record(row: &Row) -> Result<EmailRecord, StoreError> {
    Ok(EmailRecord {
        id: row.get(0),
        org_id: parse_id(row.get(1))?,
        user_id: row.get::<_, Option<&str>>(2).map(parse_id).transpose()?,
        email: row.get(3),
        kind: row.get(4),
        execution_id: row.get::<_, Option<&str>>(5).map(parse_id).transpose()?,
        event_id: row.get(6),
        subject: row.get(7),
        text: row.get(8),
        html: row.get(9),
        state: parse_state(row.get(10))?,
        attempts: u32::try_from(row.get::<_, i32>(11))
            .map_err(|_| StoreError::Backend("postgres.negative_counter"))?,
        next_attempt_at_ms: row.get(12),
        created_at_ms: row.get(13),
        sent_at_ms: row.get(14),
        failure: row.get(15),
    })
}

async fn claim(
    c: &Client,
    state: EmailState,
    limit: usize,
    lease_ms: u64,
    before_ms: Option<i64>,
) -> Result<Vec<EmailLease>, StoreError> {
    if lease_ms == 0 {
        return Err(StoreError::InvalidBatch("notify.invalid_lease"));
    }
    let now = now_ms(c).await?;
    let deadline = now
        .checked_add(
            i64::try_from(lease_ms)
                .map_err(|_| StoreError::InvalidBatch("notify.lease_overflow"))?,
        )
        .ok_or(StoreError::InvalidBatch("notify.lease_overflow"))?;
    let due = if before_ms.is_some() {
        "AND created_at_ms < $4"
    } else {
        "AND next_attempt_at_ms <= $3"
    };
    let sql = format!(
        "SELECT {COLUMNS}, generation FROM runbook.email_delivery \
         WHERE state = $1 {due} AND (lease_until_ms IS NULL OR lease_until_ms <= $3) \
         ORDER BY created_at_ms, id LIMIT $2 FOR UPDATE SKIP LOCKED"
    );
    let rows = if let Some(before) = before_ms {
        c.query(
            &sql,
            &[&state_name(state), &int(limit as u64)?, &now, &before],
        )
        .await
        .map_err(db)?
    } else {
        c.query(&sql, &[&state_name(state), &int(limit as u64)?, &now])
            .await
            .map_err(db)?
    };
    let mut leases = vec![];
    for row in &rows {
        let record = record(row)?;
        let generation = uint(row.get(16))? + 1;
        c.execute(
            "UPDATE runbook.email_delivery SET generation = $2, lease_until_ms = $3 WHERE id = $1",
            &[&record.id, &int(generation)?, &deadline],
        )
        .await
        .map_err(db)?;
        leases.push(EmailLease {
            record,
            generation,
            claimed_at_ms: now,
        });
    }
    Ok(leases)
}

impl NotifyStore for PostgresNotify {
    async fn notify_time_ms(&self) -> Result<i64, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        now_ms(conn.client()).await
    }
    async fn email_preference(
        &self,
        user_id: &UserId,
    ) -> Result<Option<(EmailPreference, Option<String>)>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .query_opt(
                "SELECT preference, token_salt FROM runbook.email_preference WHERE user_id = $1",
                &[&user_id.as_str()],
            )
            .await
            .map_err(db)?
            .map(|row| Ok((parse_preference(row.get(0))?, row.get(1))))
            .transpose()
    }
    async fn set_email_preference(
        &self,
        user_id: &UserId,
        preference: EmailPreference,
    ) -> Result<(), StoreError> {
        self.transaction(async |c| {
            c.execute(
                "INSERT INTO runbook.email_preference (user_id, preference) VALUES ($1, $2) \
                 ON CONFLICT (user_id) DO UPDATE SET preference = EXCLUDED.preference",
                &[&user_id.as_str(), &preference_name(preference)],
            )
            .await
            .map_err(db)?;
            Ok(())
        })
        .await
    }
    async fn ensure_unsubscribe_salt(
        &self,
        user_id: &UserId,
        candidate: String,
    ) -> Result<String, StoreError> {
        self.transaction(async |c| {
            let row = c
                .query_one(
                    "INSERT INTO runbook.email_preference (user_id, preference, token_salt) \
                     VALUES ($1, 'all', $2) \
                     ON CONFLICT (user_id) DO UPDATE SET token_salt = COALESCE(runbook.email_preference.token_salt, EXCLUDED.token_salt) \
                     RETURNING token_salt",
                    &[&user_id.as_str(), &candidate],
                )
                .await
                .map_err(db)?;
            Ok(row.get(0))
        })
        .await
    }
    async fn record_emails(&self, records: Vec<EmailRecord>) -> Result<usize, StoreError> {
        self.transaction(async |c| {
            let mut inserted = 0;
            for record in &records {
                inserted += c
                    .execute(
                        "INSERT INTO runbook.email_delivery (id, org_id, user_id, email, kind, execution_id, event_id, subject, text_body, html_body, state, attempts, next_attempt_at_ms, created_at_ms, sent_at_ms, failure) \
                         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) \
                         ON CONFLICT (event_id, user_id) DO NOTHING",
                        &[
                            &record.id,
                            &record.org_id.as_str(),
                            &record.user_id.as_ref().map(|id| id.as_str()),
                            &record.email,
                            &record.kind,
                            &record.execution_id.as_ref().map(|id| id.as_str()),
                            &record.event_id,
                            &record.subject,
                            &record.text,
                            &record.html,
                            &state_name(record.state),
                            &i32::try_from(record.attempts)
                                .map_err(|_| StoreError::Backend("postgres.invalid_count"))?,
                            &record.next_attempt_at_ms,
                            &record.created_at_ms,
                            &record.sent_at_ms,
                            &record.failure,
                        ],
                    )
                    .await
                    .map_err(db)? as usize;
            }
            Ok(inserted)
        })
        .await
    }
    async fn claim_emails(
        &self,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<EmailLease>, StoreError> {
        self.transaction(async |c| claim(c, EmailState::Pending, limit, lease_ms, None).await)
            .await
    }
    async fn complete_email(
        &self,
        id: &str,
        generation: u64,
        completion: EmailCompletion,
    ) -> Result<(), StoreError> {
        self.transaction(async |c| {
            let now = now_ms(c).await?;
            let (state, sent_at, next) = if completion.sent {
                (EmailState::Sent, Some(now), None)
            } else if let Some(next) = completion.next_attempt_at_ms {
                (EmailState::Pending, None, Some(next))
            } else {
                (EmailState::Failed, None, None)
            };
            let changed = c
                .execute(
                    "UPDATE runbook.email_delivery SET state = $2, attempts = attempts + 1, \
                     next_attempt_at_ms = COALESCE($3, next_attempt_at_ms), sent_at_ms = $4, failure = $5, lease_until_ms = NULL \
                     WHERE id = $1 AND generation = $6 AND lease_until_ms > $7",
                    &[
                        &id,
                        &state_name(state),
                        &next,
                        &sent_at,
                        &completion.failure,
                        &int(generation)?,
                        &now,
                    ],
                )
                .await
                .map_err(db)?;
            if changed == 0 {
                return Err(StoreError::InvalidBatch("notify.stale_lease"));
            }
            Ok(())
        })
        .await
    }
    async fn claim_digest(
        &self,
        before_ms: i64,
        limit: usize,
        lease_ms: u64,
    ) -> Result<Vec<EmailLease>, StoreError> {
        self.transaction(async |c| {
            claim(c, EmailState::Digest, limit, lease_ms, Some(before_ms)).await
        })
        .await
    }
    async fn slack_settings(
        &self,
        scope: &runbook_core::catalog::TenantScope,
    ) -> Result<Option<ProjectSlackSettings>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let row = conn
            .client()
            .query_opt(
                "SELECT url, COALESCE(notify_run_started, false) FROM runbook.project_slack WHERE org_id = $1 AND project_id = $2",
                &[&scope.org_id.as_str(), &scope.project_id.as_str()],
            )
            .await
            .map_err(db)?;
        Ok(row.map(slack_settings))
    }
    async fn save_slack_settings(
        &self,
        scope: &runbook_core::catalog::TenantScope,
        target: Option<String>,
        notify_run_started: Option<bool>,
    ) -> Result<Option<ProjectSlackSettings>, StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        let c = conn.client();
        let org = scope.org_id.as_str();
        let project = scope.project_id.as_str();
        let row = if let Some(target) = &target {
            c.query_opt(
                "INSERT INTO runbook.project_slack (org_id, project_id, url, notify_run_started)
                 VALUES ($1, $2, $3, COALESCE($4, false))
                 ON CONFLICT (org_id, project_id) DO UPDATE SET url = EXCLUDED.url,
                     notify_run_started = COALESCE($4, project_slack.notify_run_started, false)
                 RETURNING url, notify_run_started",
                &[&org, &project, target, &notify_run_started],
            ).await
        } else {
            c.query_opt(
                "UPDATE runbook.project_slack SET notify_run_started = COALESCE($3, notify_run_started, false)
                 WHERE org_id = $1 AND project_id = $2 RETURNING url, notify_run_started",
                &[&org, &project, &notify_run_started],
            ).await
        }.map_err(db)?;
        Ok(row.map(slack_settings))
    }
    async fn remove_slack_settings(
        &self,
        scope: &runbook_core::catalog::TenantScope,
    ) -> Result<(), StoreError> {
        let conn = self.pool.get().await.map_err(connection)?;
        conn.client()
            .execute(
                "DELETE FROM runbook.project_slack WHERE org_id = $1 AND project_id = $2",
                &[&scope.org_id.as_str(), &scope.project_id.as_str()],
            )
            .await
            .map_err(db)?;
        Ok(())
    }
    async fn mark_digested(&self, ids: Vec<(String, u64)>, at_ms: i64) -> Result<(), StoreError> {
        self.transaction(async |c| {
            for (id, generation) in &ids {
                let changed = c
                    .execute(
                        "UPDATE runbook.email_delivery SET state = 'sent', sent_at_ms = $2, lease_until_ms = NULL \
                         WHERE id = $1 AND generation = $3",
                        &[id, &at_ms, &int(*generation)?],
                    )
                    .await
                    .map_err(db)?;
                if changed == 0 {
                    return Err(StoreError::InvalidBatch("notify.stale_lease"));
                }
            }
            Ok(())
        })
        .await
    }
}
