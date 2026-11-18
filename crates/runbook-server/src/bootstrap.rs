//! OS entropy stays in the host. Only hashes cross into Postgres storage.
use crate::{Error, config::ServerConfig};
use runbook_core::{credentials::*, directory::*};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, UserId};
use runbook_store_postgres::{BootstrapFault, BootstrapSeed, DoctorReport, PostgresStore};
use std::{collections::BTreeSet, fmt, io::Write};

pub struct BootstrapOptions {
    pub organization: String,
    pub user_name: String,
    pub project_slug: String,
    /// AUTH5: the owner's email, bound as verified in the same directory
    /// commit, so signing in by email (D45) resolves to this owner.
    pub owner_email: Option<String>,
    /// Issue (and print) the owner's PAT and the project's service token.
    pub with_token: bool,
}
impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            organization: "Runbook".into(),
            user_name: "Administrator".into(),
            project_slug: "default".into(),
            owner_email: None,
            with_token: true,
        }
    }
}
/// Not Clone or Serialize: consuming the output is the only secret disclosure API.
pub struct BootstrapOutput {
    org_id: OrganizationId,
    user_id: UserId,
    project_id: ProjectId,
    owner_email: Option<String>,
    tokens: Option<(String, String)>,
}
impl fmt::Debug for BootstrapOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BootstrapOutput([REDACTED])")
    }
}
impl BootstrapOutput {
    pub fn write_once(self, mut writer: impl Write) -> Result<(), Error> {
        let mut out = serde_json::json!({"org_id":self.org_id,"user_id":self.user_id,"project_id":self.project_id});
        if let Some(email) = self.owner_email {
            out["owner_email"] = email.into();
        }
        if let Some((pat, service)) = self.tokens {
            out["pat"] = pat.into();
            out["service"] = service.into();
        }
        serde_json::to_writer(&mut writer, &out).map_err(|_| Error("bootstrap.output_failed"))?;
        writeln!(writer)
            .and_then(|_| writer.flush())
            .map_err(|_| Error("bootstrap.output_failed"))
    }
}
fn random<const N: usize>() -> Result<[u8; N], Error> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| Error("bootstrap.entropy_failed"))?;
    Ok(bytes)
}
/// ULID: database milliseconds plus 80 bits from OS entropy, Crockford base32.
pub(crate) fn id(prefix: &str, now: i64) -> Result<String, Error> {
    if !(0..=0xffff_ffff_ffff).contains(&now) {
        return Err(Error("bootstrap.clock_range"));
    }
    let mut value = (now as u128) << 80;
    for (i, b) in random::<10>()?.into_iter().enumerate() {
        value |= u128::from(b) << ((9 - i) * 8);
    }
    let mut bytes = [b'0'; 26];
    for b in bytes.iter_mut().rev() {
        *b = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[(value & 31) as usize];
        value >>= 5;
    }
    Ok(format!(
        "{prefix}{}",
        std::str::from_utf8(&bytes).map_err(|_| Error("bootstrap.id_encoding"))?
    ))
}
/// UPD1: apply every pending migration and report the schema reached,
/// without serving. This backs `runbook-server migrate` (a Kubernetes Job).
pub async fn migrate(database: &runbook_store_postgres::DatabaseConfig) -> Result<u32, Error> {
    let mut store = PostgresStore::connect(database)
        .await
        .map_err(|e| Error(e.0))?;
    store.migrate().await.map_err(|e| Error(e.0))
}

pub async fn run(
    config: &ServerConfig,
    options: &BootstrapOptions,
) -> Result<BootstrapOutput, Error> {
    run_with_fault(config, options, None).await
}
/// Explicit test hook; never selected from CLI arguments or environment.
pub async fn run_with_fault(
    config: &ServerConfig,
    options: &BootstrapOptions,
    fail: Option<BootstrapFault>,
) -> Result<BootstrapOutput, Error> {
    // AUTH5: refuse an owner who could never sign in, before any write.
    let owner_email = match &options.owner_email {
        Some(email) => {
            let key = email_key(email).map_err(|_| Error("bootstrap.invalid_owner_email"))?;
            if config
                .auth
                .as_ref()
                .is_some_and(|a| !a.domain_allowed(&key))
            {
                return Err(Error("bootstrap.owner_domain_not_allowed"));
            }
            Some(key)
        }
        None => None,
    };
    let store = PostgresStore::connect(&config.database)
        .await
        .map_err(|e| Error(e.0))?;
    let now: i64 = store
        .client()
        .query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
            &[],
        )
        .await
        .map_err(|_| Error("bootstrap.database_clock"))?
        .get(0);
    let org_id: OrganizationId = id("org_", now)?
        .parse()
        .map_err(|_| Error("bootstrap.id"))?;
    let user_id: UserId = id("usr_", now)?
        .parse()
        .map_err(|_| Error("bootstrap.id"))?;
    let project_id: ProjectId = id("prj_", now)?
        .parse()
        .map_err(|_| Error("bootstrap.id"))?;
    let pat = encode_secret(TokenKind::Personal, random()?);
    let service = encode_secret(TokenKind::Service, random()?);
    let scopes: BTreeSet<_> = [
        Scope::RunbooksRead,
        Scope::RunbooksWrite,
        Scope::RunbooksPublish,
        Scope::ExecutionsRead,
        Scope::ExecutionsStart,
        Scope::ExecutionsAct,
        Scope::ExecutionsAdmin,
        Scope::SkillsRead,
        Scope::SkillsWrite,
        Scope::TokensManage,
        Scope::WebhooksManage,
        Scope::ProjectAdmin,
    ]
    .into();
    let personal = CredentialRecord {
        id: id("tok_", now)?
            .parse()
            .map_err(|_| Error("bootstrap.id"))?,
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
                .ok_or(Error("bootstrap.expiry"))?,
        ),
        revoked_at: None,
        last_used_at: None,
    };
    let service_record = CredentialRecord {
        id: id("tok_", now)?
            .parse()
            .map_err(|_| Error("bootstrap.id"))?,
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
    store
        .bootstrap(
            BootstrapSeed {
                organization: OrganizationRecord {
                    id: org_id.clone(),
                    title: options.organization.clone(),
                },
                owner: UserRecord {
                    id: user_id.clone(),
                    name: options.user_name.clone(),
                    active: true,
                    role: OrgRole::Owner,
                },
                project: ProjectRecord {
                    id: project_id.clone(),
                    org_id: org_id.clone(),
                    slug: options.project_slug.clone(),
                    title: options.project_slug.clone(),
                },
                owner_email: owner_email.clone(),
                personal: options.with_token.then_some(personal),
                service: options.with_token.then_some(service_record),
            },
            fail,
        )
        .await
        .map_err(|e| Error(e.0))?;
    Ok(BootstrapOutput {
        org_id,
        user_id,
        project_id,
        owner_email,
        tokens: options.with_token.then_some((pat, service)),
    })
}
pub async fn doctor(config: &ServerConfig) -> Result<DoctorReport, Error> {
    PostgresStore::connect(&config.database)
        .await
        .map_err(|e| Error(e.0))?
        .doctor()
        .await
        .map_err(|e| Error(e.0))
}
