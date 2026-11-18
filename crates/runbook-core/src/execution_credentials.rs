//! Execution credential families (B7a/D30/D31/D36): one-hour access plus a
//! rotating refresh capped at start + 30 days. Secrets are generated from
//! host entropy and returned once, after commit; only hashes persist.
//! Every use rechecks the issuer credential and D16 sponsorship.
use crate::{
    ExecutionStore, StoreError,
    catalog::CatalogStore,
    credentials::{
        AuthError, CredentialError, CredentialPrincipal, CredentialStore, Scope, SecretHash,
        TokenKind, authorize, encode_secret, parse_secret, secret_hash,
    },
    session::{ExecutionSession, SessionStore},
    start::StartStore,
};
use runbook_engine::Event;
use runbook_model::{
    ActorRef, ExecutionId, ExecutionStatus, MintTokenRequest, OrganizationId, ProjectId,
    RefreshRequest, TokenId,
};
use serde::{Deserialize, Serialize};
use std::future::Future;

pub const ACCESS_TTL_MS: i64 = 3_600_000;
pub const REFRESH_CAP_MS: i64 = 30 * 24 * 3_600_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialFamily {
    pub family_id: String,
    pub execution_id: ExecutionId,
    pub org_id: OrganizationId,
    pub project_id: ProjectId,
    pub issuer_token_id: TokenId,
    pub issuer: CredentialPrincipal,
    pub session_id: String,
    pub idempotency_key: String,
    pub request_hash: [u8; 32],
    pub created_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCredentialRecord {
    pub token_id: TokenId,
    pub kind: TokenKind,
    pub hash: SecretHash,
    pub family_id: String,
    pub execution_id: ExecutionId,
    pub org_id: OrganizationId,
    pub project_id: ProjectId,
    pub generation: u64,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub consumed_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
}

/// Credential mutations riding an ExecutionCommit so audit events and
/// token rows commit atomically (including reuse revocation).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CredentialWrite {
    IssueFamily {
        family: Box<CredentialFamily>,
        access: Box<ExecutionCredentialRecord>,
        refresh: Box<ExecutionCredentialRecord>,
    },
    Rotate {
        consume_token_id: TokenId,
        consumed_at_ms: i64,
        access: Box<ExecutionCredentialRecord>,
        refresh: Box<ExecutionCredentialRecord>,
    },
    RevokeFamily {
        execution_id: ExecutionId,
        family_id: String,
        revoked_at_ms: i64,
    },
}
impl CredentialWrite {
    pub fn execution_id(&self) -> &ExecutionId {
        match self {
            Self::IssueFamily { family, .. } => &family.execution_id,
            Self::Rotate { access, .. } => &access.execution_id,
            Self::RevokeFamily { execution_id, .. } => execution_id,
        }
    }
}

/// Non-secret token identity returned with the one-time secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedToken {
    pub token_id: TokenId,
    pub secret: String,
    pub expires_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedPair {
    pub family_id: String,
    pub generation: u64,
    pub access: IssuedToken,
    pub refresh: IssuedToken,
}

/// Host entropy and identities for one issuance; core never generates IDs
/// or randomness itself.
pub struct MintEntropy {
    pub family_id: String,
    pub access_token_id: TokenId,
    pub refresh_token_id: TokenId,
    pub access_random: [u8; 32],
    pub refresh_random: [u8; 32],
}

#[derive(Debug)]
pub enum ExecutionCredentialError {
    Store(StoreError),
    Credentials(CredentialError),
    Auth(AuthError),
    /// Machine-readable rejection code (409-class at routes).
    Rejected(&'static str),
}
impl ExecutionCredentialError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(StoreError::NotFound) => "execution.not_found",
            Self::Store(StoreError::InvalidBatch(code)) => code,
            Self::Store(StoreError::Backend(_)) => "store.backend",
            Self::Credentials(CredentialError::Duplicate) => "credentials.duplicate",
            Self::Credentials(CredentialError::Invalid(code)) => code,
            Self::Credentials(CredentialError::Backend(_)) => "store.backend",
            Self::Auth(AuthError::InvalidCredential) => "credentials.invalid",
            Self::Auth(AuthError::Expired) => "credentials.expired",
            Self::Auth(AuthError::Revoked) => "credentials.revoked",
            Self::Auth(AuthError::Forbidden) => "credentials.forbidden",
            Self::Rejected(code) => code,
        }
    }
}
impl From<StoreError> for ExecutionCredentialError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<CredentialError> for ExecutionCredentialError {
    fn from(error: CredentialError) -> Self {
        Self::Credentials(error)
    }
}
impl From<AuthError> for ExecutionCredentialError {
    fn from(error: AuthError) -> Self {
        Self::Auth(error)
    }
}
impl std::fmt::Display for ExecutionCredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ExecutionCredentialError {}

/// Execution-scoped credential reads (writes ride ExecutionCommits).
pub trait ExecutionCredentialStore {
    fn credential_family(
        &self,
        family_id: &str,
    ) -> impl Future<Output = Result<Option<CredentialFamily>, StoreError>>;
    fn credential_by_hash(
        &self,
        hash: &SecretHash,
    ) -> impl Future<Output = Result<Option<ExecutionCredentialRecord>, StoreError>>;
    fn credential_by_id(
        &self,
        token_id: &TokenId,
    ) -> impl Future<Output = Result<Option<ExecutionCredentialRecord>, StoreError>>;
    /// The family minted under this key for one execution, if any.
    fn family_by_idempotency_key(
        &self,
        execution_id: &ExecutionId,
        key: &str,
    ) -> impl Future<Output = Result<Option<CredentialFamily>, StoreError>>;
}

fn pair_records(
    family: &CredentialFamily,
    entropy: &MintEntropy,
    generation: u64,
    now: i64,
    start_at_ms: i64,
) -> (
    IssuedPair,
    ExecutionCredentialRecord,
    ExecutionCredentialRecord,
) {
    let access_secret = encode_secret(TokenKind::Execution, entropy.access_random);
    let refresh_secret = encode_secret(TokenKind::Refresh, entropy.refresh_random);
    let access_expires = now + ACCESS_TTL_MS;
    let refresh_expires = (now + REFRESH_CAP_MS).min(start_at_ms + REFRESH_CAP_MS);
    let base = |kind, token_id: TokenId, secret: &str, expires: i64| ExecutionCredentialRecord {
        token_id,
        kind,
        hash: secret_hash(secret),
        family_id: family.family_id.clone(),
        execution_id: family.execution_id.clone(),
        org_id: family.org_id.clone(),
        project_id: family.project_id.clone(),
        generation,
        issued_at_ms: now,
        expires_at_ms: expires,
        consumed_at_ms: None,
        revoked_at_ms: None,
    };
    let access = base(
        TokenKind::Execution,
        entropy.access_token_id.clone(),
        &access_secret,
        access_expires,
    );
    let refresh = base(
        TokenKind::Refresh,
        entropy.refresh_token_id.clone(),
        &refresh_secret,
        refresh_expires,
    );
    (
        IssuedPair {
            family_id: family.family_id.clone(),
            generation,
            access: IssuedToken {
                token_id: access.token_id.clone(),
                secret: access_secret,
                expires_at_ms: access_expires,
            },
            refresh: IssuedToken {
                token_id: refresh.token_id.clone(),
                secret: refresh_secret,
                expires_at_ms: refresh_expires,
            },
        },
        access,
        refresh,
    )
}

/// Mint the first pair for a family. The issuer must currently hold
/// executions:act on the run's project (D30), and the run must have
/// activated (D31). The pair returns only after the session commits.
#[allow(clippy::too_many_arguments)]
pub async fn mint_execution_pair<S, C>(
    store: &S,
    credentials: &C,
    directory: &impl crate::directory::DirectoryStore,
    issuer: &crate::credentials::CredentialRecord,
    execution_id: &ExecutionId,
    request: &MintTokenRequest,
    idempotency_key: &str,
    request_hash: [u8; 32],
    entropy: MintEntropy,
) -> Result<IssuedPair, ExecutionCredentialError>
where
    S: StartStore + CatalogStore + ExecutionCredentialStore + SessionStore + ExecutionStore,
    C: CredentialStore,
{
    let _ = credentials;
    if idempotency_key.is_empty() {
        return Err(ExecutionCredentialError::Rejected(
            "request.idempotency_key_required",
        ));
    }
    if let Some(existing) = store
        .family_by_idempotency_key(execution_id, idempotency_key)
        .await?
    {
        return Err(if existing.request_hash == request_hash {
            ExecutionCredentialError::Rejected("credentials.already_issued")
        } else {
            ExecutionCredentialError::Rejected("request.idempotency_key_reused")
        });
    }
    let session = store.begin(execution_id).await?;
    let snapshot = session.load(execution_id).await?;
    if snapshot.revision == 0 {
        return Err(ExecutionCredentialError::Rejected("execution.not_started"));
    }
    if matches!(
        snapshot.projection.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    ) {
        return Err(ExecutionCredentialError::Rejected("execution.closed"));
    }
    let _record = store
        .start_record(execution_id)
        .await?
        .ok_or(StoreError::NotFound)?;
    let directory_snapshot = directory
        .snapshot(&issuer.org_id)
        .await
        .map_err(|_| StoreError::Backend("actor.directory_unavailable"))?;
    let project_id = store
        .execution_project(&issuer.org_id, execution_id)
        .await?
        .ok_or(ExecutionCredentialError::Auth(AuthError::Forbidden))?;
    authorize(
        issuer,
        &directory_snapshot,
        session.transaction_time_ms(),
        &project_id,
        Scope::ExecutionsAct,
    )?;
    let now = session.transaction_time_ms();
    let start_at = snapshot
        .events
        .first()
        .map(|event| event.at_ms)
        .ok_or(ExecutionCredentialError::Rejected("execution.not_started"))?;
    let family = CredentialFamily {
        family_id: entropy.family_id.clone(),
        execution_id: execution_id.clone(),
        org_id: issuer.org_id.clone(),
        project_id: project_id.clone(),
        issuer_token_id: issuer.id.clone(),
        issuer: issuer.principal.clone(),
        session_id: request.session_id.clone(),
        idempotency_key: idempotency_key.to_owned(),
        request_hash,
        created_at_ms: now,
        revoked_at_ms: None,
    };
    let (pair, access, refresh) = pair_records(&family, &entropy, 1, now, start_at);
    let summary = store
        .get_run(execution_id)
        .await?
        .ok_or(StoreError::NotFound)?;
    let assignments = current_assignments(store, execution_id).await?;
    let batch = audit_batch(
        &snapshot,
        summary,
        assignments,
        vec![Event::ExecutionCredentialsIssued {
            family_id: family.family_id.clone(),
            generation: 1,
            issuer: issuer_label(&family),
        }],
        now,
        vec![CredentialWrite::IssueFamily {
            family: Box::new(family),
            access: Box::new(access),
            refresh: Box::new(refresh),
        }],
    );
    match session.commit(batch).await? {
        crate::CommitResult::Committed(_) => Ok(pair),
        crate::CommitResult::Conflict => Err(ExecutionCredentialError::Rejected(
            "credentials.already_issued",
        )),
    }
}

fn issuer_label(family: &CredentialFamily) -> String {
    match &family.issuer {
        CredentialPrincipal::User { user_id, .. } => format!("user:{user_id}"),
        CredentialPrincipal::Service { .. } => format!("service:{}", family.issuer_token_id),
    }
}

async fn current_assignments(
    store: &impl CatalogStore,
    execution_id: &ExecutionId,
) -> Result<Vec<crate::catalog::Assignment>, StoreError> {
    Ok(store
        .list_assignments()
        .await?
        .into_iter()
        .filter(|assignment| &assignment.execution_id == execution_id)
        .collect())
}
/// Build a one-sequence audit commit on top of a session snapshot; timers,
/// projection (sequence only) and the derived summary are refreshed, in
/// line with the actor runtime's commit shape but without settling.
fn audit_batch(
    snapshot: &crate::StoredExecution,
    mut summary: crate::catalog::RunSummary,
    assignments: Vec<crate::catalog::Assignment>,
    events: Vec<Event>,
    at_ms: i64,
    credentials: Vec<CredentialWrite>,
) -> crate::ExecutionCommit {
    summary.sequence = snapshot.revision + events.len() as u64;
    let envelopes: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| runbook_engine::EventEnvelope {
            execution_id: snapshot.projection.execution_id.clone(),
            seq: snapshot.revision + 1 + index as u64,
            at_ms,
            actor: ActorRef::System,
            event,
        })
        .collect();
    let mut projection = snapshot.projection.clone();
    projection.sequence = snapshot.revision + envelopes.len() as u64;
    // Audit facts may land on a closed run (refresh reuse after close): its
    // derived update keeps the run's concurrency released and carries no
    // assignments, as every commit on a terminal run does (F18).
    let closed = matches!(
        projection.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    );
    crate::ExecutionCommit {
        execution_id: snapshot.projection.execution_id.clone(),
        expected_sequence: snapshot.revision,
        events: envelopes,
        projection: projection.clone(),
        timers: vec![],
        outbox: vec![],
        idempotency: vec![],
        claims: vec![],
        credentials,
        derived_outbox: vec![crate::catalog::DerivedUpdate {
            summary,
            assignments: if closed { vec![] } else { assignments },
            release_concurrency: closed,
            memory: vec![],
        }],
    }
}

/// Recheck the issuing credential on every use (D31): its token must still
/// authorize executions:act, which also requires an active sponsor (D16).
async fn recheck_issuer<C: CredentialStore>(
    credentials: &C,
    directory: &impl crate::directory::DirectoryStore,
    family: &CredentialFamily,
    now: i64,
) -> Result<crate::credentials::Principal, ExecutionCredentialError> {
    let issuer = credentials
        .find_by_id(&family.issuer_token_id)
        .await?
        .ok_or(AuthError::InvalidCredential)?;
    let snapshot = directory
        .snapshot(&family.org_id)
        .await
        .map_err(|_| StoreError::Backend("actor.directory_unavailable"))?;
    Ok(authorize(
        &issuer,
        &snapshot,
        now,
        &family.project_id,
        Scope::ExecutionsAct,
    )?)
}

/// MEM6 review fix 4: whether the credential that issued an execution
/// family holds `scope` on the family's project right now. A run's memory
/// writes need the issuer's `runbooks:write`: `executions:act` alone never
/// reaches live project memory.
pub async fn issuer_holds<S, C>(
    store: &S,
    credentials: &C,
    directory: &impl crate::directory::DirectoryStore,
    family_id: &str,
    now: i64,
    scope: Scope,
) -> Result<bool, StoreError>
where
    S: ExecutionCredentialStore,
    C: CredentialStore,
{
    let Some(family) = store.credential_family(family_id).await? else {
        return Ok(false);
    };
    let Some(issuer) = credentials
        .find_by_id(&family.issuer_token_id)
        .await
        .map_err(|_| StoreError::Backend("credentials.unavailable"))?
    else {
        return Ok(false);
    };
    let snapshot = directory
        .snapshot(&family.org_id)
        .await
        .map_err(|_| StoreError::Backend("actor.directory_unavailable"))?;
    Ok(authorize(&issuer, &snapshot, now, &family.project_id, scope).is_ok())
}

async fn revoke_family_now<S>(
    store: &S,
    family: &CredentialFamily,
    event: Event,
) -> Result<(), ExecutionCredentialError>
where
    S: CatalogStore + SessionStore + ExecutionStore,
{
    let session = store.begin(&family.execution_id).await?;
    let snapshot = session.load(&family.execution_id).await?;
    let now = session.transaction_time_ms();
    let summary = store
        .get_run(&family.execution_id)
        .await?
        .ok_or(StoreError::NotFound)?;
    let assignments = current_assignments(store, &family.execution_id).await?;
    let batch = audit_batch(
        &snapshot,
        summary,
        assignments,
        vec![event],
        now,
        vec![CredentialWrite::RevokeFamily {
            execution_id: family.execution_id.clone(),
            family_id: family.family_id.clone(),
            revoked_at_ms: now,
        }],
    );
    match session.commit(batch).await? {
        crate::CommitResult::Committed(_) => Ok(()),
        crate::CommitResult::Conflict => {
            Err(ExecutionCredentialError::Rejected("credentials.conflict"))
        }
    }
}

/// Rotate a refresh token. A consumed token presented again — by an
/// attacker, a legitimate lost-response retry, or the same idempotency
/// key — commits family revocation plus the reuse audit event BEFORE the
/// 409 is returned; the revocation is never rolled back with the error.
pub async fn refresh_execution_pair<S, C>(
    store: &S,
    credentials: &C,
    directory: &impl crate::directory::DirectoryStore,
    request: &RefreshRequest,
    entropy: MintEntropy,
) -> Result<IssuedPair, ExecutionCredentialError>
where
    S: StartStore + CatalogStore + ExecutionCredentialStore + SessionStore + ExecutionStore,
    C: CredentialStore,
{
    let (kind, _) = parse_secret(&request.refresh_token)?;
    if kind != TokenKind::Refresh {
        return Err(AuthError::InvalidCredential.into());
    }
    let record = store
        .credential_by_hash(&secret_hash(&request.refresh_token))
        .await?
        .ok_or(AuthError::InvalidCredential)?;
    let family = store
        .credential_family(&record.family_id)
        .await?
        .ok_or(AuthError::InvalidCredential)?;
    // Serialize on the execution session, then re-read the token row.
    let session = store.begin(&record.execution_id).await?;
    let snapshot = session.load(&record.execution_id).await?;
    let now = session.transaction_time_ms();
    let current = store
        .credential_by_id(&record.token_id)
        .await?
        .ok_or(AuthError::InvalidCredential)?;
    if current.consumed_at_ms.is_some() {
        drop(session);
        revoke_family_now(
            store,
            &family,
            Event::ExecutionTokenReuseDetected {
                family_id: family.family_id.clone(),
            },
        )
        .await?;
        return Err(ExecutionCredentialError::Rejected(
            "credentials.refresh_reused",
        ));
    }
    if current.revoked_at_ms.is_some() || family.revoked_at_ms.is_some() {
        return Err(AuthError::Revoked.into());
    }
    if now >= current.expires_at_ms {
        return Err(AuthError::Expired.into());
    }
    if matches!(
        snapshot.projection.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    ) {
        return Err(ExecutionCredentialError::Rejected("execution.closed"));
    }
    recheck_issuer(credentials, directory, &family, now).await?;
    let start_at = snapshot
        .events
        .first()
        .map(|event| event.at_ms)
        .ok_or(ExecutionCredentialError::Rejected("execution.not_started"))?;
    let generation = current.generation + 1;
    let (pair, access, refresh) = pair_records(&family, &entropy, generation, now, start_at);
    let summary = store
        .get_run(&record.execution_id)
        .await?
        .ok_or(StoreError::NotFound)?;
    let assignments = current_assignments(store, &record.execution_id).await?;
    let batch = audit_batch(
        &snapshot,
        summary,
        assignments,
        vec![Event::ExecutionCredentialsRotated {
            family_id: family.family_id.clone(),
            generation,
        }],
        now,
        vec![CredentialWrite::Rotate {
            consume_token_id: current.token_id.clone(),
            consumed_at_ms: now,
            access: Box::new(access),
            refresh: Box::new(refresh),
        }],
    );
    match session.commit(batch).await {
        Ok(crate::CommitResult::Committed(_)) => Ok(pair),
        // A racing rotation consumed the token first: reuse semantics.
        Ok(crate::CommitResult::Conflict)
        | Err(StoreError::InvalidBatch("credentials.refresh_reused")) => {
            revoke_family_now(
                store,
                &family,
                Event::ExecutionTokenReuseDetected {
                    family_id: family.family_id.clone(),
                },
            )
            .await?;
            Err(ExecutionCredentialError::Rejected(
                "credentials.refresh_reused",
            ))
        }
        Err(error) => Err(error.into()),
    }
}

/// The authorized execution identity behind a live access token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionPrincipal {
    pub execution_id: ExecutionId,
    pub family_id: String,
    pub generation: u64,
    pub issuer: crate::credentials::Principal,
}

pub async fn verify_execution_token<S, C>(
    store: &S,
    credentials: &C,
    directory: &impl crate::directory::DirectoryStore,
    secret: &str,
    now: i64,
) -> Result<ExecutionPrincipal, ExecutionCredentialError>
where
    S: ExecutionCredentialStore + ExecutionStore,
    C: CredentialStore,
{
    let (kind, _) = parse_secret(secret)?;
    if kind != TokenKind::Execution {
        return Err(AuthError::InvalidCredential.into());
    }
    let record = store
        .credential_by_hash(&secret_hash(secret))
        .await?
        .ok_or(AuthError::InvalidCredential)?;
    if record.revoked_at_ms.is_some() {
        return Err(AuthError::Revoked.into());
    }
    if now >= record.expires_at_ms {
        return Err(AuthError::Expired.into());
    }
    let family = store
        .credential_family(&record.family_id)
        .await?
        .ok_or(AuthError::InvalidCredential)?;
    if family.revoked_at_ms.is_some() {
        return Err(AuthError::Revoked.into());
    }
    // Families end when the run closes: the closing commit revokes every
    // family in the same transaction (both adapters), so the family check
    // above covers it without loading the run's events (F20).
    let principal = recheck_issuer(credentials, directory, &family, now).await?;
    Ok(ExecutionPrincipal {
        execution_id: record.execution_id.clone(),
        family_id: family.family_id,
        generation: record.generation,
        issuer: principal,
    })
}

/// Revoke a whole family with basics: the minting principal or a project
/// admin (the route authorizes; here the issuer is given already checked).
pub async fn revoke_execution_family<S>(
    store: &S,
    family: &CredentialFamily,
) -> Result<(), ExecutionCredentialError>
where
    S: CatalogStore + SessionStore + ExecutionStore,
{
    revoke_family_now(
        store,
        family,
        Event::ExecutionCredentialsRevoked {
            family_id: family.family_id.clone(),
        },
    )
    .await
}
