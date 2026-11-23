//! DeviceCodeStore cases (RFC 8628, D47). Registered red at T0; AUTH2 makes
//! them pass on both adapters.
use crate::directory::{org, user};
use runbook_core::{
    credentials::{CredentialError, Scope, SecretHash, secret_hash},
    device_codes::{
        DEVICE_CODE_TTL_MS, DEVICE_POLL_INTERVAL_MS, DeviceCodeRecord, DeviceCodeState,
        DeviceCodeStore, DeviceDecision, DeviceGrant, DevicePoll,
    },
};
use runbook_model::TokenId;

fn tok(n: u8) -> TokenId {
    format!("tok_{n:026}").parse().unwrap()
}
fn hash(text: &str) -> SecretHash {
    secret_hash(text)
}

/// Pending until approved, `slow_down` when polled early, the grant handed
/// to exactly one poll, and a decision only from the approver's org.
pub async fn poll_consumes_an_approval_once<C: DeviceCodeStore>(
    c: &C,
) -> Result<(), CredentialError> {
    let now = 1_000_000;
    let (device, user_code) = (hash("device-code-1"), hash("WDJBMJHT"));
    c.insert_device_code(DeviceCodeRecord {
        device_code_hash: device,
        user_code_hash: user_code,
        client_name: "runbook CLI".into(),
        requester_ip: Some("198.51.100.4".into()),
        requested_scopes: [Scope::RunbooksRead, Scope::ExecutionsAct].into(),
        created_at_ms: now,
        expires_at_ms: now + DEVICE_CODE_TTL_MS,
        interval_ms: DEVICE_POLL_INTERVAL_MS,
        last_polled_at_ms: None,
        state: DeviceCodeState::Pending,
    })
    .await?;
    // The approval page reads what the CLI's request recorded.
    let shown = c
        .device_code_by_user_code(&user_code)
        .await?
        .expect("stored");
    assert_eq!(shown.requester_ip.as_deref(), Some("198.51.100.4"));
    assert_eq!(shown.client_name, "runbook CLI");
    let t = now + DEVICE_POLL_INTERVAL_MS;
    assert_eq!(
        c.poll_device_code(&device, &tok(1), t).await?,
        DevicePoll::Pending
    );
    assert!(matches!(
        c.poll_device_code(&device, &tok(1), t + 1).await?,
        DevicePoll::SlowDown { .. }
    ));
    let grant = DeviceGrant {
        org_id: org(1),
        user_id: user(1),
        scopes: [Scope::RunbooksRead].into(),
        projects: None,
        token_expires_at_ms: now + runbook_core::device_codes::DEVICE_TOKEN_TTL_MS,
        decided_at_ms: t + 2,
        fresh_mfa_at_issue: false,
    };
    assert!(
        !c.decide_device_code(
            &org(2),
            &user_code,
            DeviceDecision::Approve(grant.clone()),
            t + 2
        )
        .await?
    );
    assert!(
        c.decide_device_code(
            &org(1),
            &user_code,
            DeviceDecision::Approve(grant.clone()),
            t + 2
        )
        .await?
    );
    assert!(
        !c.decide_device_code(&org(1), &user_code, DeviceDecision::Deny, t + 3)
            .await?
    );
    let later = t + 60_000;
    assert_eq!(
        c.poll_device_code(&device, &tok(1), later).await?,
        DevicePoll::Approved(grant)
    );
    assert_eq!(
        c.poll_device_code(&device, &tok(2), later + 60_000).await?,
        DevicePoll::AlreadyConsumed
    );
    let stored = c.device_code_by_user_code(&user_code).await?.unwrap();
    assert!(
        matches!(stored.state, DeviceCodeState::Consumed { ref token_id, .. } if *token_id == tok(1))
    );
    assert_eq!(
        c.poll_device_code(&hash("unknown"), &tok(3), later).await?,
        DevicePoll::Expired
    );
    Ok(())
}

fn pending_record(n: u8, now: i64) -> DeviceCodeRecord {
    DeviceCodeRecord {
        device_code_hash: hash(&format!("device-{n}")),
        user_code_hash: hash(&format!("USER{n}")),
        client_name: "runbook CLI".into(),
        requester_ip: None,
        requested_scopes: [Scope::RunbooksRead].into(),
        created_at_ms: now,
        expires_at_ms: now + DEVICE_CODE_TTL_MS,
        interval_ms: DEVICE_POLL_INTERVAL_MS,
        last_polled_at_ms: None,
        state: DeviceCodeState::Pending,
    }
}

/// AUTH3 storage rules (RFC 8628): a denied code answers `Denied` to every
/// poll; an expired code (even if approved first) answers `Expired` and
/// takes no decision; a duplicate code hash is refused; each early poll
/// grows the interval by 5 s.
pub async fn denial_expiry_and_collisions<C: DeviceCodeStore>(
    c: &C,
) -> Result<(), CredentialError> {
    let now = 5_000_000;
    c.insert_device_code(pending_record(1, now)).await?;
    assert_eq!(
        c.insert_device_code(pending_record(1, now)).await,
        Err(CredentialError::Duplicate)
    );
    assert!(
        c.decide_device_code(&org(1), &hash("USER1"), DeviceDecision::Deny, now + 1)
            .await?
    );
    for at in [now + 10_000, now + DEVICE_CODE_TTL_MS + 1] {
        assert_eq!(
            c.poll_device_code(&hash("device-1"), &tok(1), at).await?,
            DevicePoll::Denied
        );
    }
    c.insert_device_code(pending_record(2, now)).await?;
    let mut interval = DEVICE_POLL_INTERVAL_MS;
    assert_eq!(
        c.poll_device_code(&hash("device-2"), &tok(1), now).await?,
        DevicePoll::Pending
    );
    for step in 1..=2 {
        interval += 5_000;
        assert_eq!(
            c.poll_device_code(&hash("device-2"), &tok(1), now + step)
                .await?,
            DevicePoll::SlowDown {
                interval_ms: interval
            }
        );
    }
    let expiry = now + DEVICE_CODE_TTL_MS;
    let grant = DeviceGrant {
        org_id: org(1),
        user_id: user(1),
        scopes: [Scope::RunbooksRead].into(),
        projects: None,
        token_expires_at_ms: expiry + 1,
        decided_at_ms: now,
        fresh_mfa_at_issue: false,
    };
    assert!(
        !c.decide_device_code(
            &org(1),
            &hash("USER2"),
            DeviceDecision::Approve(grant.clone()),
            expiry
        )
        .await?
    );
    assert!(
        c.decide_device_code(
            &org(1),
            &hash("USER2"),
            DeviceDecision::Approve(grant),
            expiry - 1
        )
        .await?
    );
    assert_eq!(
        c.poll_device_code(&hash("device-2"), &tok(2), expiry)
            .await?,
        DevicePoll::Expired
    );
    let stored = c.device_code_by_user_code(&hash("USER2")).await?.unwrap();
    assert!(matches!(stored.state, DeviceCodeState::Approved(_)));
    assert_eq!(c.device_code_by_user_code(&hash("NONE")).await?, None);
    Ok(())
}
