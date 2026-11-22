//! R0-2: the runner registry on Postgres (`runbook.runner_*`).
use crate::{
    DatabaseConfig,
    rows::{connection, db},
};
use runbook_core::{
    credentials::SecretHash,
    runners::{
        BOOTSTRAP_TTL_MS, InstallRecord, NewInstall, NewRunner, RunnerError, RunnerPrincipal,
        RunnerPurpose, RunnerStore, install_status, no_install,
    },
};
use runbook_model::*;
use tokio_postgres::Client;

#[derive(Clone)]
pub struct PostgresRunners {
    config: DatabaseConfig,
}
impl PostgresRunners {
    pub fn new(config: DatabaseConfig) -> Self {
        Self { config }
    }
    pub(crate) async fn conn(&self) -> Result<crate::pool::Conn, RunnerError> {
        crate::pool::Pool::new(self.config.clone())
            .get()
            .await
            .map_err(|e| RunnerError::Store(connection(e)))
    }
}

fn store(e: tokio_postgres::Error) -> RunnerError {
    RunnerError::Store(db(e))
}
fn parse<T: std::str::FromStr>(text: &str) -> Result<T, RunnerError> {
    text.parse().map_err(|_| {
        RunnerError::Store(runbook_core::StoreError::Backend("postgres.decode_failed"))
    })
}
fn u32_of(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}
fn i32_of(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

async fn install_row(
    c: &Client,
    org: &OrganizationId,
) -> Result<Option<InstallRecord>, RunnerError> {
    let row = c
        .query_opt(
            "SELECT id, org_id, url, version, contract_version, created_at_ms, last_heartbeat_at_ms \
             , (SELECT public_key FROM runbook.runner_vault_identity_history h
                 WHERE h.install_id = i.id ORDER BY sequence DESC LIMIT 1)
             FROM runbook.runner_installs i WHERE org_id = $1",
            &[&org.as_str()],
        )
        .await
        .map_err(store)?;
    row.map(|row| {
        Ok(InstallRecord {
            id: parse(row.get::<_, &str>(0))?,
            org_id: parse(row.get::<_, &str>(1))?,
            url: row.get(2),
            version: row.get(3),
            contract_version: u32_of(row.get(4)),
            created_at_ms: row.get(5),
            last_heartbeat_at_ms: row.get(6),
            vault_identity_key: row.get(7),
        })
    })
    .transpose()
}

impl RunnerStore for PostgresRunners {
    async fn create_bootstrap(
        &self,
        org: &OrganizationId,
        url: &str,
        token: &SecretHash,
        now_ms: i64,
    ) -> Result<(), RunnerError> {
        let mut conn = self.conn().await?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(store)?;
        let result = async {
            conn.client()
                .execute(
                    "DELETE FROM runbook.runner_bootstrap_tokens WHERE org_id = $1 AND used_at_ms IS NULL",
                    &[&org.as_str()],
                )
                .await?;
            conn.client()
                .execute(
                    "INSERT INTO runbook.runner_bootstrap_tokens \
                     (token_hash, org_id, url, created_at_ms, expires_at_ms) VALUES ($1, $2, $3, $4, $5)",
                    &[
                        &token.as_bytes().as_slice(),
                        &org.as_str(),
                        &url,
                        &now_ms,
                        &(now_ms + BOOTSTRAP_TTL_MS),
                    ],
                )
                .await
        }
        .await;
        match result {
            Ok(_) => conn.commit().await.map_err(store),
            Err(e) => {
                conn.rollback().await;
                Err(store(e))
            }
        }
    }

    async fn register(
        &self,
        bootstrap: &SecretHash,
        install: NewInstall,
        now_ms: i64,
    ) -> Result<InstallRecord, RunnerError> {
        let mut conn = self.conn().await?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(store)?;
        let result = async {
            let c = conn.client();
            let Some(row) = c
                .query_opt(
                    "SELECT org_id, url FROM runbook.runner_bootstrap_tokens \
                     WHERE token_hash = $1 AND used_at_ms IS NULL AND expires_at_ms > $2 FOR UPDATE",
                    &[&bootstrap.as_bytes().as_slice(), &now_ms],
                )
                .await
                .map_err(store)?
            else {
                return Err(RunnerError::InvalidToken);
            };
            let org: OrganizationId = parse(row.get::<_, &str>(0))?;
            let url: String = row.get(1);
            // One install per org: serialize registrations of the org.
            c.execute(
                "SELECT pg_advisory_xact_lock(hashtext('runner_install:' || $1))",
                &[&org.as_str()],
            )
            .await
            .map_err(store)?;
            if install_row(c, &org).await?.is_some() {
                return Err(RunnerError::AlreadyRegistered);
            }
            if url != install.url {
                return Err(RunnerError::UrlMismatch);
            }
            c.execute(
                "INSERT INTO runbook.runner_installs (id, org_id, url, version, contract_version, \
                 directory_token_hash, release_token_hash, join_token_hash, created_at_ms) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[
                    &install.id.as_str(),
                    &org.as_str(),
                    &install.url,
                    &install.version,
                    &i32_of(install.contract_version),
                    &install.directory.as_bytes().as_slice(),
                    &install.release.as_bytes().as_slice(),
                    &install.join.as_bytes().as_slice(),
                    &now_ms,
                ],
            )
            .await
            .map_err(store)?;
            c.execute(
                "UPDATE runbook.runner_bootstrap_tokens SET used_at_ms = $2 WHERE token_hash = $1",
                &[&bootstrap.as_bytes().as_slice(), &now_ms],
            )
            .await
            .map_err(store)?;
            if let Some(key) = &install.vault_identity_key {
                c.execute(
                    "WITH recorded AS (
                         INSERT INTO runbook.runner_vault_identity_history (install_id, public_key, changed_at_ms)
                         VALUES ($1,$2,$3) RETURNING sequence, install_id, public_key, changed_at_ms
                     )
                     INSERT INTO runbook.runner_vault_identity_audit
                       (sequence, install_id, org_id, action, actor, public_key, occurred_at_ms)
                     SELECT sequence, install_id, $4, 'runner.vault_identity_enrolled', 'installer bootstrap',
                       public_key, changed_at_ms FROM recorded",
                    &[&install.id.as_str(), key, &now_ms, &org.as_str()],
                ).await.map_err(store)?;
            }
            Ok(InstallRecord {
                id: install.id,
                org_id: org,
                url: install.url,
                version: install.version,
                contract_version: install.contract_version,
                created_at_ms: now_ms,
                last_heartbeat_at_ms: None,
                vault_identity_key: install.vault_identity_key,
            })
        }
        .await;
        match result {
            Ok(record) => {
                conn.commit().await.map_err(store)?;
                Ok(record)
            }
            Err(e) => {
                conn.rollback().await;
                Err(e)
            }
        }
    }

    async fn join(
        &self,
        install: &RunnerInstallId,
        join: &SecretHash,
        runner: NewRunner,
        now_ms: i64,
    ) -> Result<RunnerId, RunnerError> {
        let mut conn = self.conn().await?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(store)?;
        let result = async {
            let c = conn.client();
            let Some(row) = c
                .query_opt(
                    "SELECT org_id FROM runbook.runner_installs WHERE id = $1 AND join_token_hash = $2 FOR UPDATE",
                    &[&install.as_str(), &join.as_bytes().as_slice()],
                )
                .await
                .map_err(store)?
            else {
                return Err(RunnerError::InvalidToken);
            };
            let org: String = row.get(0);
            let group = c
                .query_opt(
                    "SELECT install_id FROM runbook.runner_groups WHERE id = $1",
                    &[&runner.group_id.as_str()],
                )
                .await
                .map_err(store)?;
            match group {
                Some(row) if row.get::<_, &str>(0) != install.as_str() => {
                    return Err(RunnerError::Conflict("runner.group_taken"));
                }
                Some(_) => {
                    c.execute(
                        "UPDATE runbook.runner_groups SET name = $2 WHERE id = $1",
                        &[&runner.group_id.as_str(), &runner.group_name],
                    )
                    .await
                    .map_err(store)?;
                }
                None => {
                    c.execute(
                        "INSERT INTO runbook.runner_groups (id, install_id, org_id, name) VALUES ($1, $2, $3, $4)",
                        &[&runner.group_id.as_str(), &install.as_str(), &org, &runner.group_name],
                    )
                    .await
                    .map_err(store)?;
                }
            }
            let row = c
                .query_one(
                    "INSERT INTO runbook.runner_runners (id, install_id, group_id, org_id, name, version, \
                     claim_token_hash, created_at_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                     ON CONFLICT (group_id, name) DO UPDATE SET claim_token_hash = EXCLUDED.claim_token_hash, \
                     version = EXCLUDED.version, decision_v1 = false RETURNING id",
                    &[
                        &runner.id.as_str(),
                        &install.as_str(),
                        &runner.group_id.as_str(),
                        &org,
                        &runner.name,
                        &runner.version,
                        &runner.claim.as_bytes().as_slice(),
                        &now_ms,
                    ],
                )
                .await
                .map_err(store)?;
            parse::<RunnerId>(row.get::<_, &str>(0))
        }
        .await;
        match result {
            Ok(id) => {
                conn.commit().await.map_err(store)?;
                Ok(id)
            }
            Err(e) => {
                conn.rollback().await;
                Err(e)
            }
        }
    }

    async fn authenticate(
        &self,
        token: &SecretHash,
    ) -> Result<Option<RunnerPrincipal>, RunnerError> {
        let conn = self.conn().await?;
        let hash = token.as_bytes().as_slice();
        let row = conn
            .client()
            .query_opt(
                "SELECT org_id, id, NULL::text, NULL::text, CASE \
                   WHEN directory_token_hash = $1 THEN 'directory' \
                   WHEN release_token_hash = $1 THEN 'release' ELSE 'join' END \
                 FROM runbook.runner_installs \
                 WHERE directory_token_hash = $1 OR release_token_hash = $1 OR join_token_hash = $1 \
                 UNION ALL \
                 SELECT org_id, install_id, id, group_id, 'claim' FROM runbook.runner_runners \
                 WHERE claim_token_hash = $1",
                &[&hash],
            )
            .await
            .map_err(store)?;
        let Some(row) = row else { return Ok(None) };
        let purpose = match row.get::<_, &str>(4) {
            "directory" => RunnerPurpose::Directory,
            "release" => RunnerPurpose::Release,
            "join" => RunnerPurpose::Join,
            _ => RunnerPurpose::Claim,
        };
        Ok(Some(RunnerPrincipal {
            org_id: parse(row.get::<_, &str>(0))?,
            install_id: parse(row.get::<_, &str>(1))?,
            runner_id: row.get::<_, Option<&str>>(2).map(parse).transpose()?,
            group_id: row.get::<_, Option<&str>>(3).map(parse).transpose()?,
            purpose,
        }))
    }

    async fn heartbeat(
        &self,
        install: &RunnerInstallId,
        heartbeat: &RunnerHeartbeatRequest,
        now_ms: i64,
    ) -> Result<(), RunnerError> {
        let mut conn = self.conn().await?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(store)?;
        let result = async {
            let c = conn.client();
            c.execute(
                "UPDATE runbook.runner_installs SET last_heartbeat_at_ms = $2 WHERE id = $1",
                &[&install.as_str(), &now_ms],
            )
            .await?;
            for group in &heartbeat.groups {
                c.execute(
                    "UPDATE runbook.runner_groups SET name = $3 WHERE id = $1 AND install_id = $2",
                    &[&group.id.as_str(), &install.as_str(), &group.name],
                )
                .await?;
            }
            for runner in &heartbeat.runners {
                c.execute(
                    "UPDATE runbook.runner_runners SET status = $3, running = $4, capacity = $5, \
                     last_heartbeat_at_ms = $6, decision_v1 = $7 WHERE id = $1 AND install_id = $2",
                    &[
                        &runner.id.as_str(),
                        &install.as_str(),
                        &runner.status,
                        &i32_of(runner.running),
                        &i32_of(runner.capacity),
                        &now_ms,
                        &(runner.decision_contract == Some(RUNNER_DECISION_CONTRACT)),
                    ],
                )
                .await?;
            }
            Ok::<_, tokio_postgres::Error>(())
        }
        .await;
        match result {
            Ok(()) => conn.commit().await.map_err(store),
            Err(e) => {
                conn.rollback().await;
                Err(store(e))
            }
        }
    }

    async fn status(
        &self,
        org: &OrganizationId,
        now_ms: i64,
    ) -> Result<RunnerStatusView, RunnerError> {
        let conn = self.conn().await?;
        let c = conn.client();
        let Some(install) = install_row(c, org).await? else {
            return Ok(no_install());
        };
        let mut groups = Vec::new();
        for row in c
            .query(
                "SELECT g.id, g.name, coalesce(array_agg(p.project_id ORDER BY p.project_id) \
                 FILTER (WHERE p.project_id IS NOT NULL), '{}') \
                 FROM runbook.runner_groups g LEFT JOIN runbook.runner_group_projects p ON p.group_id = g.id \
                 WHERE g.install_id = $1 GROUP BY g.id, g.name ORDER BY g.id",
                &[&install.id.as_str()],
            )
            .await
            .map_err(store)?
        {
            let projects: Vec<String> = row.get(2);
            groups.push(RunnerGroupView {
                id: parse(row.get::<_, &str>(0))?,
                name: row.get(1),
                projects: projects.iter().map(|p| parse(p)).collect::<Result<_, _>>()?,
            });
        }
        let mut runners = Vec::new();
        for row in c
            .query(
                "SELECT id, name, group_id, version, status, running, capacity, last_heartbeat_at_ms \
                 FROM runbook.runner_runners WHERE install_id = $1 ORDER BY id",
                &[&install.id.as_str()],
            )
            .await
            .map_err(store)?
        {
            runners.push(RunnerView {
                id: parse(row.get::<_, &str>(0))?,
                name: row.get(1),
                group_id: parse(row.get::<_, &str>(2))?,
                version: row.get(3),
                status: row.get(4),
                running: u32_of(row.get(5)),
                capacity: u32_of(row.get(6)),
                last_heartbeat_at: row.get(7),
            });
        }
        Ok(RunnerStatusView {
            install_id: Some(install.id),
            status: install_status(install.last_heartbeat_at_ms, now_ms),
            version: Some(install.version),
            contract_version: Some(install.contract_version),
            vault_identity_key: install.vault_identity_key,
            groups,
            runners,
            last_heartbeat_at: install.last_heartbeat_at_ms,
        })
    }

    async fn set_vault_identity(
        &self,
        org: &OrganizationId,
        owner: &UserId,
        key: &str,
        now_ms: i64,
    ) -> Result<(), RunnerError> {
        let conn = self.conn().await?;
        let inserted = conn.client().execute(
            "WITH recorded AS (
                 INSERT INTO runbook.runner_vault_identity_history (install_id, public_key, changed_by, changed_at_ms)
                 SELECT id, $2, $3, $4 FROM runbook.runner_installs WHERE org_id = $1
                 RETURNING sequence, install_id, public_key, changed_by, changed_at_ms
             )
             INSERT INTO runbook.runner_vault_identity_audit
               (sequence, install_id, org_id, action, actor, public_key, occurred_at_ms)
             SELECT sequence, install_id, $1, 'runner.vault_identity_changed', changed_by, public_key, changed_at_ms
             FROM recorded",
            &[&org.as_str(), &key, &owner.as_str(), &now_ms],
        ).await.map_err(store)?;
        if inserted != 1 {
            return Err(RunnerError::NotFound);
        }
        Ok(())
    }

    async fn set_group_projects(
        &self,
        org: &OrganizationId,
        group: &RunnerGroupId,
        projects: &[ProjectId],
    ) -> Result<(), RunnerError> {
        let mut conn = self.conn().await?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(store)?;
        let result = async {
            let c = conn.client();
            let found = c
                .query_opt(
                    "SELECT 1 FROM runbook.runner_groups WHERE id = $1 AND org_id = $2 FOR UPDATE",
                    &[&group.as_str(), &org.as_str()],
                )
                .await
                .map_err(store)?;
            if found.is_none() {
                return Err(RunnerError::NotFound);
            }
            c.execute(
                "DELETE FROM runbook.runner_group_projects WHERE group_id = $1",
                &[&group.as_str()],
            )
            .await
            .map_err(store)?;
            for project in projects {
                c.execute(
                    "INSERT INTO runbook.runner_group_projects (group_id, project_id) VALUES ($1, $2) \
                     ON CONFLICT DO NOTHING",
                    &[&group.as_str(), &project.as_str()],
                )
                .await
                .map_err(store)?;
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => conn.commit().await.map_err(store),
            Err(e) => {
                conn.rollback().await;
                Err(e)
            }
        }
    }

    async fn install_of(&self, org: &OrganizationId) -> Result<Option<InstallRecord>, RunnerError> {
        let conn = self.conn().await?;
        install_row(conn.client(), org).await
    }

    async fn runner_group(
        &self,
        install: &RunnerInstallId,
        runner: &RunnerId,
    ) -> Result<Option<RunnerGroupId>, RunnerError> {
        let conn = self.conn().await?;
        conn.client()
            .query_opt(
                "SELECT group_id FROM runbook.runner_runners WHERE id = $1 AND install_id = $2",
                &[&runner.as_str(), &install.as_str()],
            )
            .await
            .map_err(store)?
            .map(|row| parse(row.get::<_, &str>(0)))
            .transpose()
    }

    async fn runner_supports_decisions(
        &self,
        install: &RunnerInstallId,
        runner: &RunnerId,
        since: i64,
    ) -> Result<bool, RunnerError> {
        let conn = self.conn().await?;
        Ok(conn
            .client()
            .query_opt(
                "SELECT 1 FROM runbook.runner_runners WHERE install_id = $1 AND id = $2
             AND decision_v1 AND last_heartbeat_at_ms > $3",
                &[&install.as_str(), &runner.as_str(), &since],
            )
            .await
            .map_err(store)?
            .is_some())
    }

    async fn ready_steps(
        &self,
        install: &RunnerInstallId,
        group: &RunnerGroupId,
        project: Option<&ProjectId>,
        after: Option<(i64, &StepRunId)>,
        limit: u64,
        decision_since: Option<i64>,
    ) -> Result<Vec<ReadyStepView>, RunnerError> {
        self.ready_steps_query(install, group, project, after, limit, decision_since)
            .await
    }
}
