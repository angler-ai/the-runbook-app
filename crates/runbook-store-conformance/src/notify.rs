//! NOTIFY1 store cases, adapter-independent.
use crate::NotifyFactory;
use runbook_core::{
    StoreError,
    notify::{
        EmailCompletion, EmailPreference, EmailRecord, EmailState, NotifyStore, unsubscribe_token,
        unsubscribe_token_matches,
    },
};
use runbook_model::{OrganizationId, UserId};

fn org() -> OrganizationId {
    "org_00000000000000000000000001".parse().unwrap()
}
fn user() -> UserId {
    "usr_00000000000000000000000001".parse().unwrap()
}
fn email(id: &str, at: i64) -> EmailRecord {
    EmailRecord {
        id: id.to_owned(),
        org_id: org(),
        user_id: Some(user()),
        email: "ada@example.test".into(),
        kind: "approval.requested".into(),
        execution_id: None,
        event_id: format!("evt_{id}"),
        subject: "Approval needed".into(),
        text: "text".into(),
        html: "<p>text</p>".into(),
        state: EmailState::Pending,
        attempts: 0,
        next_attempt_at_ms: at,
        created_at_ms: at,
        sent_at_ms: None,
        failure: None,
    }
}

pub async fn preferences_salt_and_token<F: NotifyFactory>(f: &F) -> Result<(), StoreError> {
    let s = f.create_notify().await?;
    assert_eq!(s.email_preference(&user()).await?, None);
    s.set_email_preference(&user(), EmailPreference::ApprovalsOnly)
        .await?;
    let salt = s
        .ensure_unsubscribe_salt(&user(), "salt-one".into())
        .await?;
    assert_eq!(salt, "salt-one");
    // A later candidate never rotates the salt (old links keep working).
    assert_eq!(
        s.ensure_unsubscribe_salt(&user(), "salt-two".into())
            .await?,
        "salt-one"
    );
    assert_eq!(
        s.email_preference(&user()).await?,
        Some((EmailPreference::ApprovalsOnly, Some("salt-one".into())))
    );
    let token = unsubscribe_token(&salt, &user());
    assert!(unsubscribe_token_matches(&salt, &user(), &token));
    assert!(!unsubscribe_token_matches(
        &salt,
        &user(),
        "usr_00000000000000000000000001.deadbeef"
    ));
    Ok(())
}

pub async fn outbox_records_once_and_claims_are_fenced<F: NotifyFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create_notify().await?;
    let now = s.notify_time_ms().await?;
    assert_eq!(
        s.record_emails(vec![email("eml_1", now - 100), email("eml_2", now - 50)])
            .await?,
        2
    );
    // The same fan-out retried writes nothing new.
    assert_eq!(s.record_emails(vec![email("eml_1", now - 100)]).await?, 0);

    let leases = s.claim_emails(1, 60_000).await?;
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].record.id, "eml_1");
    // A second claim skips the leased row and takes the other one.
    let more = s.claim_emails(10, 60_000).await?;
    assert_eq!(more.len(), 1);
    assert_eq!(more[0].record.id, "eml_2");

    // A stale generation cannot complete.
    assert_eq!(
        s.complete_email(
            "eml_1",
            leases[0].generation + 1,
            EmailCompletion {
                sent: true,
                next_attempt_at_ms: None,
                failure: None
            },
        )
        .await,
        Err(StoreError::InvalidBatch("notify.stale_lease"))
    );
    s.complete_email(
        "eml_1",
        leases[0].generation,
        EmailCompletion {
            sent: true,
            next_attempt_at_ms: None,
            failure: None,
        },
    )
    .await?;
    // A failed attempt waits for its retry time. Times are relative to
    // the store clock, which only the memory adapter can move, so the
    // due-again claim is proven on a third row completed due at once.
    s.complete_email(
        "eml_2",
        more[0].generation,
        EmailCompletion {
            sent: false,
            next_attempt_at_ms: Some(now + 3_600_000),
            failure: Some("smtp_550".into()),
        },
    )
    .await?;
    assert_eq!(
        s.claim_emails(10, 60_000)
            .await?
            .iter()
            .map(|lease| lease.record.id.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new()
    );
    s.record_emails(vec![email("eml_3", now - 10)]).await?;
    let third = s.claim_emails(10, 60_000).await?;
    assert_eq!(third.len(), 1);
    s.complete_email(
        "eml_3",
        third[0].generation,
        EmailCompletion {
            sent: false,
            next_attempt_at_ms: Some(now - 1),
            failure: Some("smtp_451".into()),
        },
    )
    .await?;
    let retry = s.claim_emails(10, 60_000).await?;
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].record.id, "eml_3");
    assert_eq!(retry[0].record.attempts, 1);
    assert_eq!(retry[0].record.failure.as_deref(), Some("smtp_451"));
    Ok(())
}

pub async fn digest_rows_are_claimed_by_age_and_marked_sent<F: NotifyFactory>(
    f: &F,
) -> Result<(), StoreError> {
    let s = f.create_notify().await?;
    let now = s.notify_time_ms().await?;
    let mut old = email("eml_old", now - 9_000);
    old.state = EmailState::Digest;
    let mut new = email("eml_new", now - 1_000);
    new.state = EmailState::Digest;
    s.record_emails(vec![old, new]).await?;
    let claimed = s.claim_digest(now - 5_000, 10, 60_000).await?;
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].record.id, "eml_old");
    s.mark_digested(vec![("eml_old".into(), claimed[0].generation)], 10_000)
        .await?;
    assert!(s.claim_digest(now - 5_000, 10, 60_000).await?.is_empty());
    Ok(())
}
