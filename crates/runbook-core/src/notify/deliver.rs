//! Sending recorded email (NOTIFY1): claim due rows, hand each to the
//! host transport with no store transaction open, and complete the
//! attempt with the webhook backoff. Digest rows are assembled into one
//! summary per person per UTC day, itself a normal pending delivery.
use super::store::{EmailCompletion, EmailRecord, EmailState, NotifyStore};
use super::{EmailFacts, EmailMessage, unsubscribe_token};
use crate::{StoreError, directory::DirectoryStore, webhooks::backoff_ms};
use runbook_model::UserId;
use std::collections::BTreeMap;
use std::future::Future;

/// One transport attempt. Err carries a short code for the row's
/// `failure` (SMTP reply codes, never message content).
pub trait MailTransport {
    fn send(&self, message: &EmailMessage) -> impl Future<Output = Result<(), String>>;
}

/// Attempts after which a delivery is parked Failed (webhook parity has
/// its own window; mail gets a day of the shared backoff table).
pub const MAX_ATTEMPTS: u32 = 8;

pub async fn deliver_emails<N: NotifyStore, T: MailTransport>(
    store: &N,
    transport: &T,
    limit: usize,
) -> Result<usize, StoreError> {
    let leases = store.claim_emails(limit, 120_000).await?;
    let mut sent = 0;
    for lease in leases {
        let record = lease.record;
        let message = EmailMessage {
            to: record.email.clone(),
            subject: record.subject.clone(),
            text: record.text.clone(),
            html: record.html.clone(),
        };
        let completion = match transport.send(&message).await {
            Ok(()) => {
                sent += 1;
                EmailCompletion {
                    sent: true,
                    next_attempt_at_ms: None,
                    failure: None,
                }
            }
            Err(code) => {
                let attempts = record.attempts + 1;
                EmailCompletion {
                    sent: false,
                    next_attempt_at_ms: (attempts < MAX_ATTEMPTS)
                        .then_some(lease.claimed_at_ms.saturating_add(backoff_ms(attempts))),
                    failure: Some(code.chars().take(64).collect()),
                }
            }
        };
        store
            .complete_email(&record.id, lease.generation, completion)
            .await?;
    }
    Ok(sent)
}

/// Days since the epoch as a `YYYY-MM-DD` civil date (Howard Hinnant's
/// algorithm; no clock dependency in core).
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}")
}

/// The UTC day containing `now_ms`, as (day start, label).
pub fn utc_day(now_ms: i64) -> (i64, String) {
    let days = now_ms.div_euclid(86_400_000);
    (days * 86_400_000, civil_date(days))
}

/// Fold every digest row from before today (UTC) into one summary mail
/// per person. The summary is an ordinary pending delivery keyed
/// `digest:{user}:{date}`, so a crashed assembly records nothing twice;
/// source rows are marked sent only after their summary exists.
pub async fn assemble_daily_digest<N: NotifyStore, D: DirectoryStore>(
    notify: &N,
    directory: &D,
    origin: &str,
) -> Result<usize, StoreError> {
    let now = notify.notify_time_ms().await?;
    let (day_start, label) = utc_day(now);
    let leases = notify.claim_digest(day_start, 500, 120_000).await?;
    if leases.is_empty() {
        return Ok(0);
    }
    let mut by_person: BTreeMap<UserId, Vec<_>> = BTreeMap::new();
    for lease in leases {
        // Digest rows always name a person; address-only rows
        // (invitations) are never recorded as digest items.
        if let Some(user_id) = lease.record.user_id.clone() {
            by_person.entry(user_id).or_default().push(lease);
        }
    }
    let mut made = 0;
    for (user_id, leases) in by_person {
        let first = &leases[0].record;
        let snapshot = directory
            .snapshot(&first.org_id)
            .await
            .map_err(|_| StoreError::Backend("notify.directory_unavailable"))?;
        let user = snapshot.users.get(&user_id);
        let (_, salt) = notify.email_preference(&user_id).await?.unwrap_or_default();
        let salt = salt.unwrap_or_default();
        let facts = EmailFacts {
            recipient_name: user
                .map(|u| u.name.clone())
                .unwrap_or_else(|| "there".into()),
            place: snapshot.organization.title.clone(),
            run_title: String::new(),
            step_title: None,
            link: format!("{}/tasks", origin.trim_end_matches('/')),
            unsubscribe_url: format!(
                "{}/unsubscribe?token={}",
                origin.trim_end_matches('/'),
                unsubscribe_token(&salt, &user_id)
            ),
        };
        let (subject, text, html) = digest_message(&facts, &leases, &label);
        let summary = EmailRecord {
            id: crate::webhooks::derived_id("eml_", now, &[&format!("digest:{user_id}:{label}")]),
            org_id: first.org_id.clone(),
            user_id: Some(user_id.clone()),
            email: first.email.clone(),
            kind: "daily_summary".into(),
            execution_id: None,
            event_id: format!("digest:{user_id}:{label}"),
            subject,
            text,
            html,
            state: EmailState::Pending,
            attempts: 0,
            next_attempt_at_ms: now,
            created_at_ms: now,
            sent_at_ms: None,
            failure: None,
        };
        notify.record_emails(vec![summary]).await?;
        notify
            .mark_digested(
                leases
                    .iter()
                    .map(|lease| (lease.record.id.clone(), lease.generation))
                    .collect(),
                now,
            )
            .await?;
        made += 1;
    }
    Ok(made)
}

fn digest_message(
    facts: &EmailFacts,
    leases: &[super::store::EmailLease],
    label: &str,
) -> (String, String, String) {
    let place = if facts.place.is_empty() {
        String::new()
    } else {
        format!("[{}] ", facts.place)
    };
    let subject = format!("{place}Your daily summary for {label}");
    let lines: Vec<String> = leases
        .iter()
        .map(|lease| lease.record.subject.clone())
        .collect();
    let text_lines = lines
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!(
        "Hi {name},\n\nHere is what needed you:\n\n{lines}\n\nOpen your tasks: {link}\n\nUnsubscribe: {unsub}",
        name = facts.recipient_name,
        lines = text_lines,
        link = facts.link,
        unsub = facts.unsubscribe_url,
    );
    let items = lines
        .iter()
        .map(|line| format!("<li>{}</li>", super::escape_html_pub(line)))
        .collect::<String>();
    let html = format!(
        "<div style=\"font-family:system-ui,sans-serif;max-width:560px;margin:auto;padding:24px\">\
<p>Hi {name},</p><p>Here is what needed you:</p><ul>{items}</ul>\
<p><a href=\"{link}\">Open your tasks</a></p>\
<p style=\"color:#555;font-size:13px\">You are getting this daily summary because run activity is assigned to you. <a href=\"{unsub}\">Unsubscribe</a></p></div>",
        name = super::escape_html_pub(&facts.recipient_name),
        link = super::escape_html_pub(&facts.link),
        unsub = super::escape_html_pub(&facts.unsubscribe_url),
    );
    (subject, text, html)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::memory::MemoryNotify;
    use std::{cell::RefCell, future::ready};

    struct Flaky(RefCell<u32>);
    impl MailTransport for Flaky {
        fn send(&self, _message: &EmailMessage) -> impl Future<Output = Result<(), String>> {
            *self.0.borrow_mut() += 1;
            let fail = *self.0.borrow() == 1;
            ready(if fail { Err("smtp_451".into()) } else { Ok(()) })
        }
    }

    fn record(id: &str) -> EmailRecord {
        EmailRecord {
            id: id.into(),
            org_id: "org_00000000000000000000000001".parse().unwrap(),
            user_id: Some("usr_00000000000000000000000001".parse().unwrap()),
            email: "ada@example.test".into(),
            kind: "approval.requested".into(),
            execution_id: None,
            event_id: format!("evt_{id}"),
            subject: "Approval needed".into(),
            text: "text".into(),
            html: "<p>text</p>".into(),
            state: EmailState::Pending,
            attempts: 0,
            next_attempt_at_ms: 0,
            created_at_ms: 0,
            sent_at_ms: None,
            failure: None,
        }
    }

    #[test]
    fn utc_day_labels_are_civil_dates() {
        assert_eq!(utc_day(0).1, "1970-01-01");
        assert_eq!(utc_day(1_700_000_000_000).1, "2023-11-14");
    }

    #[test]
    fn a_failed_send_retries_then_sends() {
        let run = |f: &dyn Fn()| f();
        let _ = run;
        let notify = MemoryNotify::new(10_000);
        let transport = Flaky(RefCell::new(0));
        let block = |mut fut: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<usize, StoreError>>>,
        >| {
            let waker = std::task::Waker::noop();
            match fut
                .as_mut()
                .poll(&mut std::task::Context::from_waker(waker))
            {
                std::task::Poll::Ready(v) => v.unwrap(),
                std::task::Poll::Pending => panic!("memory deliver must be synchronous"),
            }
        };
        let setup = {
            let notify = &notify;
            async move { notify.record_emails(vec![record("eml_1")]).await.unwrap() }
        };
        let waker = std::task::Waker::noop();
        let mut setup = Box::pin(setup);
        assert!(matches!(
            setup
                .as_mut()
                .poll(&mut std::task::Context::from_waker(waker)),
            std::task::Poll::Ready(_)
        ));
        assert_eq!(block(Box::pin(deliver_emails(&notify, &transport, 10))), 0);
        // The retry is backoff-scheduled, not immediately due.
        assert_eq!(block(Box::pin(deliver_emails(&notify, &transport, 10))), 0);
        notify.set_time(10_000 + backoff_ms(1) + 1);
        assert_eq!(block(Box::pin(deliver_emails(&notify, &transport, 10))), 1);
    }
}
