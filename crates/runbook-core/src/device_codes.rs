//! RFC 8628 device authorization for `runbook login` (PRD §10; D47). T0
//! freezes the records and trait; AUTH2 adds the `device_code` table in
//! `0027_auth_links_invitations_device` and implements them, AUTH3 builds the
//! routes and rate limits on top.
//!
//! The grant ends in an ordinary personal access token (`trb_pat_`): the
//! approver picks scopes within their own, and the poll that observes the
//! approval consumes it exactly once so the host can mint the token. Codes are
//! stored as SHA-256 hashes only, like every other credential.
use crate::credentials::{CredentialError, Scope, SecretHash};
use runbook_model::{OrganizationId, ProjectId, TokenId, UserId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, future::Future};

/// Device and user codes live 10 minutes.
pub const DEVICE_CODE_TTL_MS: i64 = 10 * 60 * 1000;
/// RFC 8628 default polling interval; `slow_down` adds 5 seconds.
pub const DEVICE_POLL_INTERVAL_MS: i64 = 5_000;
/// D47: default lifetime of the token a device login issues.
pub const DEVICE_TOKEN_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// What the approver granted. Every field is within the approver's own
/// credential and role; the host checks that before `decide_device_code`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceGrant {
    pub org_id: OrganizationId,
    pub user_id: UserId,
    pub scopes: BTreeSet<Scope>,
    /// `None`: every project the user may access.
    pub projects: Option<BTreeSet<ProjectId>>,
    pub token_expires_at_ms: i64,
    pub decided_at_ms: i64,
    #[serde(default)]
    pub fresh_mfa_at_issue: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceCodeState {
    Pending,
    Approved(DeviceGrant),
    Denied {
        decided_at_ms: i64,
    },
    /// The grant was handed to one poll, which minted `token_id`.
    Consumed {
        token_id: TokenId,
        at_ms: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCodeRecord {
    /// Hash of the high-entropy `device_code` the CLI polls with.
    pub device_code_hash: SecretHash,
    /// Hash of the normalized short `user_code` a person types.
    pub user_code_hash: SecretHash,
    /// Shown on the approval page ("runbook CLI on host …"); not trusted.
    pub client_name: String,
    /// Sign-in gate fix 3: the address that asked for the code (the TCP
    /// peer, or the client a trusted proxy named), shown on the approval
    /// page. Absent in records written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_ip: Option<String>,
    /// What the CLI asked for; the approver may narrow it.
    pub requested_scopes: BTreeSet<Scope>,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub interval_ms: i64,
    pub last_polled_at_ms: Option<i64>,
    pub state: DeviceCodeState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceDecision {
    Approve(DeviceGrant),
    Deny,
}

/// RFC 8628 §3.5 outcomes of one poll.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevicePoll {
    /// `authorization_pending`.
    Pending,
    /// `slow_down`: polled sooner than the interval; the interval grew 5 s.
    SlowDown { interval_ms: i64 },
    /// `expired_token`, also for an unknown code.
    Expired,
    /// `access_denied`.
    Denied,
    /// The single successful poll: mint the token named in the call.
    Approved(DeviceGrant),
    /// A later poll after consumption: `expired_token` on the wire.
    AlreadyConsumed,
}

/// Device-code storage. The CLI's requests carry no credential and no
/// organization, so inserts and polls are global by code hash, like
/// `CredentialStore::find_by_hash`; decisions are scoped by the approver's
/// organization. Every call is atomic; futures need not be Send.
pub trait DeviceCodeStore {
    /// `Duplicate` on a device- or user-code hash collision.
    fn insert_device_code(
        &self,
        record: DeviceCodeRecord,
    ) -> impl Future<Output = Result<(), CredentialError>>;
    /// Global key read for the approval page. Grants nothing.
    fn device_code_by_user_code(
        &self,
        user_code_hash: &SecretHash,
    ) -> impl Future<Output = Result<Option<DeviceCodeRecord>, CredentialError>>;
    /// Record the poll at `now_ms` and answer it. An approved code moves to
    /// `Consumed { token_id }` in the same step and returns its grant once.
    fn poll_device_code(
        &self,
        device_code_hash: &SecretHash,
        token_id: &TokenId,
        now_ms: i64,
    ) -> impl Future<Output = Result<DevicePoll, CredentialError>>;
    /// Approve or deny a pending, unexpired code. The grant's org must equal
    /// `org`. True only for the first decision.
    fn decide_device_code(
        &self,
        org: &OrganizationId,
        user_code_hash: &SecretHash,
        decision: DeviceDecision,
        now_ms: i64,
    ) -> impl Future<Output = Result<bool, CredentialError>>;
}

/// RFC 8628 §3.5 `slow_down` adds this much to the interval.
pub const SLOW_DOWN_STEP_MS: i64 = 5_000;
/// The interval never grows past a minute.
pub const MAX_POLL_INTERVAL_MS: i64 = 60_000;

/// One poll against a stored record, shared by every adapter: the adapter
/// loads the record under its lock, calls this, and writes the record back
/// in the same step. A pending code polled sooner than its interval answers
/// `SlowDown` and grows the interval; every poll records its time. An
/// approved code becomes `Consumed { token_id }` and hands out its grant
/// once; a later poll is `AlreadyConsumed`.
pub fn apply_poll(record: &mut DeviceCodeRecord, token_id: &TokenId, now_ms: i64) -> DevicePoll {
    let previous = record.last_polled_at_ms.replace(now_ms);
    match &record.state {
        DeviceCodeState::Consumed { .. } => DevicePoll::AlreadyConsumed,
        DeviceCodeState::Denied { .. } => DevicePoll::Denied,
        _ if now_ms >= record.expires_at_ms => DevicePoll::Expired,
        DeviceCodeState::Approved(grant) => {
            let grant = grant.clone();
            record.state = DeviceCodeState::Consumed {
                token_id: token_id.clone(),
                at_ms: now_ms,
            };
            DevicePoll::Approved(grant)
        }
        DeviceCodeState::Pending => {
            if previous.is_some_and(|t| {
                i128::from(now_ms) - i128::from(t) < i128::from(record.interval_ms)
            }) {
                record.interval_ms = record
                    .interval_ms
                    .saturating_add(SLOW_DOWN_STEP_MS)
                    .min(MAX_POLL_INTERVAL_MS.max(record.interval_ms));
                DevicePoll::SlowDown {
                    interval_ms: record.interval_ms,
                }
            } else {
                DevicePoll::Pending
            }
        }
    }
}

/// Approve or deny a pending, unexpired code. An approval's grant must name
/// `org`. True only for the first decision.
pub fn apply_decision(
    record: &mut DeviceCodeRecord,
    org: &OrganizationId,
    decision: DeviceDecision,
    now_ms: i64,
) -> bool {
    if record.state != DeviceCodeState::Pending || now_ms >= record.expires_at_ms {
        return false;
    }
    record.state = match decision {
        DeviceDecision::Approve(grant) => {
            if &grant.org_id != org {
                return false;
            }
            DeviceCodeState::Approved(grant)
        }
        DeviceDecision::Deny => DeviceCodeState::Denied {
            decided_at_ms: now_ms,
        },
    };
    true
}
