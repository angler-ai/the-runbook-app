//! Native local bootstrap: migration lock, directory and credentials in one transaction.
use crate::{Error, PostgresStore, credentials, directory, migrate};
use runbook_core::{CommitResult, credentials::*, directory::*};
use runbook_model::ProjectRole;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapFault {
    AfterDirectory,
    AfterPersonalToken,
    BeforeCommit,
}
/// No raw secrets cross the adapter boundary. The owner needs a way in:
/// a verified email (signing in by email, AUTH5), the tokens, or both. The
/// two tokens come together or not at all.
pub struct BootstrapSeed {
    pub organization: OrganizationRecord,
    pub owner: UserRecord,
    pub project: ProjectRecord,
    /// Normalized (`directory::email_key`); bound as the owner's verified email.
    pub owner_email: Option<String>,
    pub personal: Option<CredentialRecord>,
    pub service: Option<CredentialRecord>,
}
#[derive(Debug)]
pub struct DoctorReport {
    /// The database's schema version.
    pub migration_version: u32,
    /// The schema this code migrates to (UPD1); lower than
    /// `migration_version` only when a newer schema is allowed.
    pub code_version: u32,
}
impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.migration_version > self.code_version {
            write!(
                f,
                "database: ok\nschema: ahead (database {}, code {}; RUNBOOK_ALLOW_NEWER_SCHEMA)\nblobs: ok",
                self.migration_version, self.code_version
            )
        } else {
            write!(
                f,
                "database: ok\nschema: current ({})\nblobs: ok",
                self.migration_version
            )
        }
    }
}
fn fault(selected: Option<BootstrapFault>, here: BootstrapFault) -> Result<(), Error> {
    if selected == Some(here) {
        Err(Error("bootstrap.injected_failure"))
    } else {
        Ok(())
    }
}
impl PostgresStore {
    /// Consumes this connection. Cancellation or any unfinished transaction drops
    /// the connection, so no transaction can escape into an unrelated operation.
    pub async fn bootstrap(
        self,
        seed: BootstrapSeed,
        fail: Option<BootstrapFault>,
    ) -> Result<(), Error> {
        self.client().batch_execute("BEGIN").await?;
        let result = self.bootstrap_inner(seed, fail).await;
        match result {
            Ok(()) => {
                self.client().batch_execute("COMMIT").await?;
                Ok(())
            }
            Err(e) => {
                let _ = self.client().batch_execute("ROLLBACK").await;
                Err(e)
            }
        }
    }
    async fn bootstrap_inner(
        &self,
        seed: BootstrapSeed,
        fail: Option<BootstrapFault>,
    ) -> Result<(), Error> {
        let c = self.client();
        // A fresh instance only: never on a newer release's schema.
        migrate::apply(c, false).await?;
        // Migration advisory lock serializes concurrent bootstrap attempts.
        let occupied: bool = c.query_one("SELECT EXISTS (SELECT 1 FROM runbook.organization) OR EXISTS (SELECT 1 FROM runbook.instance_bootstrap)", &[]).await?.get(0);
        if occupied {
            return Err(Error("bootstrap.already_initialized"));
        }
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
                    && matches!(&service.principal, CredentialPrincipal::Service {project_id, ..} if project_id == &seed.project.id)
            }
            (None, None) => seed.owner_email.is_some(),
            _ => false,
        };
        if !seed.owner.active
            || seed.owner.role != OrgRole::Owner
            || seed.project.org_id != seed.organization.id
            || !tokens_ok
        {
            return Err(Error("bootstrap.invalid_seed"));
        }
        c.execute("INSERT INTO runbook.instance_bootstrap(singleton, org_id, user_id, project_id) VALUES (true,$1,$2,$3)", &[&seed.organization.id.as_str(),&seed.owner.id.as_str(),&seed.project.id.as_str()]).await?;
        let commit = DirectoryCommit {
            org_id: seed.organization.id.clone(),
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: seed.organization.title,
                },
                DirectoryMutation::CreateUser(seed.owner.clone()),
                DirectoryMutation::CreateProject(seed.project.clone()),
                DirectoryMutation::GrantProjectRole {
                    project_id: seed.project.id,
                    user_id: seed.owner.id.clone(),
                    role: ProjectRole::Admin,
                },
            ]
            .into_iter()
            .chain(
                seed.owner_email
                    .map(|email| DirectoryMutation::BindVerifiedEmail {
                        user_id: seed.owner.id,
                        email,
                    }),
            )
            .collect(),
        };
        if directory::apply(c, commit)
            .await
            .map_err(|_| Error("bootstrap.directory_failed"))?
            != CommitResult::Committed(1)
        {
            return Err(Error("bootstrap.directory_conflict"));
        }
        fault(fail, BootstrapFault::AfterDirectory)?;
        if let Some(personal) = &seed.personal {
            credentials::insert_on(c, personal)
                .await
                .map_err(|_| Error("bootstrap.credential_failed"))?;
        }
        fault(fail, BootstrapFault::AfterPersonalToken)?;
        if let Some(service) = &seed.service {
            credentials::insert_on(c, service)
                .await
                .map_err(|_| Error("bootstrap.credential_failed"))?;
        }
        fault(fail, BootstrapFault::BeforeCommit)
    }
    /// Read-only health check; validates history and access to both blob tables.
    pub async fn doctor(self) -> Result<DoctorReport, Error> {
        self.client()
            .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await?;
        let migration_version = migrate::check(self.client(), self.allow_newer_schema).await?;
        for table in ["runbook.blob", "runbook.blob_chunk"] {
            let allowed: bool = self.client().query_one("SELECT has_table_privilege($1,'SELECT') AND has_table_privilege($1,'INSERT') AND has_table_privilege($1,'UPDATE') AND has_table_privilege($1,'DELETE')", &[&table]).await?.get(0);
            if !allowed {
                return Err(Error("doctor.blob_access_denied"));
            }
        }
        self.client()
            .query("SELECT bytes FROM runbook.blob_chunk LIMIT 0", &[])
            .await?;
        self.client()
            .query("SELECT key FROM runbook.blob LIMIT 0", &[])
            .await?;
        self.client().batch_execute("COMMIT").await?;
        Ok(DoctorReport {
            migration_version,
            code_version: migrate::SCHEMA_VERSION,
        })
    }
}
