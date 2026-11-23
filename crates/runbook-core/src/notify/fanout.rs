//! Fan-out of one committed notification intent to email rows: expand
//! its recipients through the directory, keep people with a verified
//! address and an allowing preference, render, and record one row per
//! person. Idempotent like webhook fan-out (record_emails dedupes on
//! the event and person), so it may run before the webhook fan-out of
//! the same lease and safely retry with it.
use super::store::{EmailRecord, EmailState, NotifyStore};
use super::{EmailFacts, EmailPlan, plan, render, unsubscribe_token};
use crate::{
    StoreError,
    catalog::TenantScope,
    directory::{DirectoryStore, OrgRole},
    webhooks::derived_id,
};
use runbook_engine::NotificationIntent;
use runbook_model::{RecipientRef, UserId};
use std::collections::BTreeSet;

/// What one fan-out recorded (for host logs and tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmailFanOut {
    pub recorded: usize,
    pub digest: usize,
}

fn step_title(intent: &NotificationIntent) -> Option<String> {
    if let Some(title) = intent
        .detail
        .as_ref()
        .and_then(|detail| detail["step_title"].as_str())
    {
        return Some(title.into());
    }
    let step_run_id = intent.step_run_ids.first()?;
    intent
        .ready_steps
        .iter()
        .find(|work| &work.step_run_id == step_run_id)
        .map(|work| work.title.clone())
}

fn recipients(
    snapshot: &crate::directory::DirectorySnapshot,
    intent: &NotificationIntent,
    failed_run: bool,
) -> BTreeSet<UserId> {
    let mut users = BTreeSet::new();
    for recipient in &intent.recipients {
        match recipient {
            RecipientRef::User(id) => {
                users.insert(id.clone());
            }
            RecipientRef::Team(reference) => {
                let team = snapshot
                    .teams
                    .values()
                    .find(|team| team.id.as_str() == reference || team.slug == *reference);
                if let Some(team) = team {
                    for (team_id, user_id) in &snapshot.team_members {
                        if team_id == &team.id {
                            users.insert(user_id.clone());
                        }
                    }
                }
            }
        }
    }
    if users.is_empty() && failed_run {
        for user in snapshot.users.values() {
            if user.role == OrgRole::Owner {
                users.insert(user.id.clone());
            }
        }
    }
    users
}

/// The link a person follows from an email or a Slack message: the run's
/// page in the web app. A run's own `url` is its API address
/// (`{origin}/v1/executions/{exe}`), which a browser cannot show, so any
/// link into the API on this origin becomes the run page `/x/{exe}`.
/// Links to other hosts are kept as they are.
pub(crate) fn link_absolute(origin: &str, intent: &NotificationIntent) -> String {
    let origin = origin.trim_end_matches('/');
    let link = intent.link.as_str();
    if (link.starts_with("http://") || link.starts_with("https://"))
        && link
            .strip_prefix(origin)
            .is_none_or(|rest| !rest.starts_with('/'))
    {
        return link.to_owned();
    }
    let path = link.strip_prefix(origin).unwrap_or(link);
    let path = if path.is_empty() || api_path(path) {
        format!("/x/{}", intent.execution_id)
    } else if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    format!("{origin}{path}")
}

/// Whether a path is the API's (`/v1` or below), which no person should be
/// sent to.
fn api_path(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path == "/v1" || path.starts_with("/v1/")
}

/// Record the emails for one intent. `origin` is the app's public
/// origin; `new_salt` is host entropy for a person's first unsubscribe
/// token. Returns what was newly recorded.
pub async fn fan_out_email<N: NotifyStore, D: DirectoryStore>(
    notify: &N,
    directory: &D,
    scope: &TenantScope,
    intent: &NotificationIntent,
    run_title: &str,
    origin: &str,
    new_salt: impl Fn() -> String,
) -> Result<EmailFanOut, StoreError> {
    if !super::emailable(&intent.kind) {
        return Ok(EmailFanOut::default());
    }
    let failed_run = intent.kind == "execution.completed"
        && intent
            .detail
            .as_ref()
            .and_then(|d| d.get("status"))
            .and_then(|s| s.as_str())
            == Some("failed");
    if intent.kind == "execution.completed" && !failed_run {
        return Ok(EmailFanOut::default());
    }
    let snapshot = directory
        .snapshot(&scope.org_id)
        .await
        .map_err(|_| StoreError::Backend("notify.directory_unavailable"))?;
    let emails = directory
        .verified_emails(&scope.org_id)
        .await
        .map_err(|_| StoreError::Backend("notify.directory_unavailable"))?;
    let project_title = snapshot
        .projects
        .get(&scope.project_id)
        .map(|p| p.title.clone())
        .unwrap_or_default();
    let place = if project_title.is_empty() {
        snapshot.organization.title.clone()
    } else {
        format!("{} / {project_title}", snapshot.organization.title)
    };
    let now = notify.notify_time_ms().await?;
    let mut records = vec![];
    let mut digest = 0;
    for user_id in recipients(&snapshot, intent, failed_run) {
        let Some(user) = snapshot.users.get(&user_id).filter(|u| u.active) else {
            continue;
        };
        let Some(address) = emails.get(&user_id) else {
            continue;
        };
        let (preference, _) = notify.email_preference(&user_id).await?.unwrap_or_default();
        let state_value = match plan(preference, &intent.kind) {
            EmailPlan::SendNow => EmailState::Pending,
            EmailPlan::Digest => EmailState::Digest,
            EmailPlan::Drop => continue,
        };
        let salt = notify.ensure_unsubscribe_salt(&user_id, new_salt()).await?;
        let facts = EmailFacts {
            recipient_name: user.name.clone(),
            place: place.clone(),
            run_title: run_title.to_owned(),
            step_title: step_title(intent),
            link: link_absolute(origin, intent),
            unsubscribe_url: format!(
                "{}/unsubscribe?token={}",
                origin.trim_end_matches('/'),
                unsubscribe_token(&salt, &user_id)
            ),
        };
        let Some(message) = render(intent, address, &facts) else {
            continue;
        };
        if state_value == EmailState::Digest {
            digest += 1;
        }
        records.push(EmailRecord {
            id: derived_id("eml_", intent.at_ms, &[&intent.event_id, user_id.as_str()]),
            org_id: scope.org_id.clone(),
            user_id: Some(user_id.clone()),
            email: address.clone(),
            kind: intent.kind.clone(),
            execution_id: Some(intent.execution_id.clone()),
            event_id: intent.event_id.clone(),
            subject: message.subject,
            text: message.text,
            html: message.html,
            state: state_value,
            attempts: 0,
            next_attempt_at_ms: now,
            created_at_ms: now,
            sent_at_ms: None,
            failure: None,
        });
    }
    let recorded = notify.record_emails(records).await?;
    Ok(EmailFanOut { recorded, digest })
}
