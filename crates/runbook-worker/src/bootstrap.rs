//! First-run bootstrap on Cloudflare (the native `runbook-server bootstrap`,
//! CLI-less): one organization with its owner and first project, and the
//! owner's personal token and a project service token, minted with the
//! host's entropy and clock and stored by `runbook_store_d1::bootstrap`.
//! The api Worker serves it only to an operator holding the
//! `BOOTSTRAP_SECRET` binding, and only on an empty instance.
use crate::cloudflare::CfHost;
use runbook_api::backends::encode_id;
use runbook_core::{
    credentials::{
        ActorKind, CredentialPrincipal, CredentialRecord, Scope, TokenKind, encode_secret,
        secret_hash,
    },
    directory::{OrgRole, OrganizationRecord, ProjectRecord, UserRecord, email_key},
};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, UserId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct BootstrapRequest {
    pub organization: String,
    pub user_name: String,
    pub project_slug: String,
    #[serde(default)]
    pub owner_email: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BootstrapOutput {
    pub org_id: OrganizationId,
    pub user_id: UserId,
    pub project_id: ProjectId,
    /// Shown once; only their hashes are stored.
    pub personal_token: String,
    pub service_token: String,
}

fn id<H: CfHost, T: std::str::FromStr>(
    host: &H,
    prefix: &str,
    now: i64,
) -> Result<T, &'static str> {
    let mut random = [0u8; 10];
    host.fill_random(&mut random)
        .map_err(|_| "bootstrap.entropy_failed")?;
    encode_id(prefix, now, random)
        .ok_or("bootstrap.clock_range")?
        .parse()
        .map_err(|_| "bootstrap.id")
}

pub async fn run<H: CfHost>(
    host: &H,
    request: BootstrapRequest,
) -> Result<BootstrapOutput, &'static str> {
    let owner_email = request
        .owner_email
        .as_deref()
        .map(email_key)
        .transpose()
        .map_err(|_| "bootstrap.invalid_owner_email")?;
    let now = host.now_ms();
    let org_id: OrganizationId = id(host, "org_", now)?;
    let user_id: UserId = id(host, "usr_", now)?;
    let project_id: ProjectId = id(host, "prj_", now)?;
    let mut pat = [0u8; 32];
    let mut service = [0u8; 32];
    host.fill_random(&mut pat)
        .map_err(|_| "bootstrap.entropy_failed")?;
    host.fill_random(&mut service)
        .map_err(|_| "bootstrap.entropy_failed")?;
    let pat = encode_secret(TokenKind::Personal, pat);
    let service = encode_secret(TokenKind::Service, service);
    let scopes: std::collections::BTreeSet<_> = Scope::ALL.into_iter().collect();
    let personal = CredentialRecord {
        id: id(host, "tok_", now)?,
        kind: TokenKind::Personal,
        hash: secret_hash(&pat),
        principal: CredentialPrincipal::User {
            user_id: user_id.clone(),
            actor: ActorKind::Human,
        },
        org_id: org_id.clone(),
        scopes: scopes.clone(),
        projects: None,
        expires_at: Some(
            now.checked_add(365 * 24 * 60 * 60 * 1000)
                .ok_or("bootstrap.expiry")?,
        ),
        revoked_at: None,
        last_used_at: None,
    };
    let service_record = CredentialRecord {
        id: id(host, "tok_", now)?,
        kind: TokenKind::Service,
        hash: secret_hash(&service),
        principal: CredentialPrincipal::Service {
            project_id: project_id.clone(),
            role: ProjectRole::Admin,
        },
        org_id: org_id.clone(),
        scopes,
        projects: Some([project_id.clone()].into()),
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
    };
    runbook_store_d1::bootstrap::bootstrap(
        // The directory and the tokens: the control database (CF11).
        &host.shards().control(),
        runbook_store_d1::bootstrap::BootstrapSeed {
            organization: OrganizationRecord {
                id: org_id.clone(),
                title: request.organization,
            },
            owner: UserRecord {
                id: user_id.clone(),
                name: request.user_name,
                active: true,
                role: OrgRole::Owner,
            },
            project: ProjectRecord {
                id: project_id.clone(),
                org_id: org_id.clone(),
                slug: request.project_slug.clone(),
                title: request.project_slug,
            },
            owner_email,
            personal: Some(personal),
            service: Some(service_record),
        },
    )
    .await?;
    Ok(BootstrapOutput {
        org_id,
        user_id,
        project_id,
        personal_token: pat,
        service_token: service,
    })
}
