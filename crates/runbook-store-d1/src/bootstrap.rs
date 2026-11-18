//! First-run bootstrap on D1 (as `runbook-store-postgres`'s): the instance
//! marker, the organization with its owner and first project, and the
//! owner's personal and service tokens, in one guarded batch that refuses
//! an instance that already has an organization.
use crate::{
    D1Directory,
    directory::Planned,
    sql::{D1, D1Error, guard, prepare},
};
use runbook_core::{
    credentials::{ActorKind, CredentialPrincipal, CredentialRecord, TokenKind},
    directory::{
        DirectoryCommit, DirectoryMutation, OrgRole, OrganizationRecord, ProjectRecord, UserRecord,
    },
};
use runbook_model::ProjectRole;

pub struct BootstrapSeed {
    pub organization: OrganizationRecord,
    pub owner: UserRecord,
    pub project: ProjectRecord,
    pub owner_email: Option<String>,
    pub personal: Option<CredentialRecord>,
    pub service: Option<CredentialRecord>,
}

fn seed_valid(seed: &BootstrapSeed) -> bool {
    let tokens_ok = match (&seed.personal, &seed.service) {
        (Some(personal), Some(service)) => {
            personal.org_id == seed.organization.id
                && service.org_id == seed.organization.id
                && personal.kind == TokenKind::Personal
                && service.kind == TokenKind::Service
                && personal.principal
                    == (CredentialPrincipal::User {
                        user_id: seed.owner.id.clone(),
                        actor: ActorKind::Human,
                    })
                && matches!(&service.principal, CredentialPrincipal::Service { project_id, .. }
                    if project_id == &seed.project.id)
        }
        (None, None) => seed.owner_email.is_some(),
        _ => false,
    };
    seed.owner.active
        && seed.owner.role == OrgRole::Owner
        && seed.project.org_id == seed.organization.id
        && tokens_ok
}

/// `bootstrap.already_initialized`, `bootstrap.invalid_seed` or a backend code.
pub async fn bootstrap<D: D1>(db: &D, seed: BootstrapSeed) -> Result<(), &'static str> {
    if !seed_valid(&seed) {
        return Err("bootstrap.invalid_seed");
    }
    crate::migrate::migrate(db)
        .await
        .map_err(|_| "bootstrap.migrate_failed")?;
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: seed.organization.title.clone(),
        },
        DirectoryMutation::CreateUser(seed.owner.clone()),
        DirectoryMutation::CreateProject(seed.project.clone()),
        DirectoryMutation::GrantProjectRole {
            project_id: seed.project.id.clone(),
            user_id: seed.owner.id.clone(),
            role: ProjectRole::Admin,
        },
    ];
    if let Some(email) = &seed.owner_email {
        mutations.push(DirectoryMutation::BindVerifiedEmail {
            user_id: seed.owner.id.clone(),
            email: email.clone(),
        });
    }
    let directory = D1Directory::new(db.clone());
    let commit = DirectoryCommit {
        org_id: seed.organization.id.clone(),
        expected_revision: 0,
        mutations,
    };
    let Planned::Write { statements, .. } = directory
        .plan(&commit)
        .await
        .map_err(|_| "bootstrap.directory_failed")?
    else {
        return Err("bootstrap.already_initialized");
    };
    let mut batch = vec![
        guard(
            "bootstrap.already_initialized",
            "NOT EXISTS (SELECT 1 FROM organization) AND NOT EXISTS (SELECT 1 FROM instance_bootstrap)",
        ),
        prepare(
            "INSERT INTO instance_bootstrap (singleton, org_id, user_id, project_id, completed_at) \
             VALUES (1, ?, ?, ?, ?)",
        )
        .bind(seed.organization.id.as_str())
        .bind(seed.owner.id.as_str())
        .bind(seed.project.id.as_str())
        .bind(db.now_ms()),
    ];
    batch.extend(statements);
    for record in [&seed.personal, &seed.service].into_iter().flatten() {
        batch.push(
            crate::credentials::insert_statement(record, None)
                .map_err(|_| "bootstrap.invalid_seed")?,
        );
    }
    match db.batch(batch).await {
        Ok(_) => Ok(()),
        Err(D1Error::Guard(code))
            if code == "bootstrap.already_initialized" || code == "conflict" =>
        {
            Err("bootstrap.already_initialized")
        }
        Err(_) => Err("bootstrap.write_failed"),
    }
}
