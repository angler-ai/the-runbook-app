//! Runner registry on D1. Guarded batches keep token consumption and
//! group ownership checks atomic with the corresponding writes.
use crate::sql::{Columns, D1, D1Error, Row, guard, prepare};
use runbook_core::{
    credentials::SecretHash,
    runners::{
        BOOTSTRAP_TTL_MS, InstallRecord, NewInstall, NewRunner, RunnerError, RunnerPrincipal,
        RunnerPurpose, RunnerStore, install_status, no_install,
    },
};
use runbook_model::*;

#[derive(Clone)]
pub struct D1Runners<D: D1> {
    pub(crate) db: D,
}

impl<D: D1> D1Runners<D> {
    pub fn new(db: D) -> Self {
        Self { db }
    }
}

pub(crate) fn store(error: D1Error) -> RunnerError {
    RunnerError::Store(crate::store_error(error))
}

pub(crate) fn parse<T: std::str::FromStr>(text: &str) -> Result<T, D1Error> {
    text.parse().map_err(|_| D1Error::Range)
}

fn install_record(row: &Row) -> Result<InstallRecord, D1Error> {
    Ok(InstallRecord {
        id: parse(row.text(0)?)?,
        org_id: parse(row.text(1)?)?,
        url: row.text(2)?.into(),
        version: row.text(3)?.into(),
        contract_version: u32::try_from(row.integer(4)?).unwrap_or(0),
        created_at_ms: row.integer(5)?,
        last_heartbeat_at_ms: row.optional_integer(6)?,
        vault_identity_key: row.optional_text(7)?,
    })
}

impl<D: D1> RunnerStore for D1Runners<D> {
    async fn create_bootstrap(
        &self,
        org: &OrganizationId,
        url: &str,
        token: &SecretHash,
        now_ms: i64,
    ) -> Result<(), RunnerError> {
        self.db.batch(vec![
            prepare("DELETE FROM runner_bootstrap_tokens WHERE org_id = ? AND used_at_ms IS NULL")
                .bind(org.as_str()),
            prepare("INSERT INTO runner_bootstrap_tokens (token_hash, org_id, url, created_at_ms, expires_at_ms) VALUES (?, ?, ?, ?, ?)")
                .bind(token.as_bytes().as_slice()).bind(org.as_str()).bind(url).bind(now_ms).bind(now_ms + BOOTSTRAP_TTL_MS),
        ]).await.map(|_| ()).map_err(store)
    }

    async fn register(
        &self,
        bootstrap: &SecretHash,
        install: NewInstall,
        now_ms: i64,
    ) -> Result<InstallRecord, RunnerError> {
        let hash = bootstrap.as_bytes().as_slice();
        let read = prepare("SELECT org_id, url FROM runner_bootstrap_tokens WHERE token_hash = ? AND used_at_ms IS NULL AND expires_at_ms > ?")
            .bind(hash).bind(now_ms);
        let rows = self.db.all(read).await.map_err(store)?;
        let row = rows.first().ok_or(RunnerError::InvalidToken)?;
        let org = row.text(0).map_err(store)?;
        let url = row.text(1).map_err(store)?;
        let mut statements = vec![
            guard("runner.invalid_token", "EXISTS (SELECT 1 FROM runner_bootstrap_tokens WHERE token_hash = ? AND used_at_ms IS NULL AND expires_at_ms > ? AND org_id = ? AND url = ?)")
                .bind(hash).bind(now_ms).bind(org).bind(url),
            guard("runner.already_registered", "NOT EXISTS (SELECT 1 FROM runner_installs WHERE org_id = ?)")
                .bind(org),
            guard("runner.url_mismatch", "? = ?").bind(url).bind(&install.url),
            prepare("INSERT INTO runner_installs (id, org_id, url, version, contract_version, directory_token_hash, release_token_hash, join_token_hash, created_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(install.id.as_str()).bind(org).bind(&install.url).bind(&install.version).bind(install.contract_version.min(i32::MAX as u32))
                .bind(install.directory.as_bytes().as_slice()).bind(install.release.as_bytes().as_slice()).bind(install.join.as_bytes().as_slice()).bind(now_ms),
            prepare("UPDATE runner_bootstrap_tokens SET used_at_ms = ? WHERE token_hash = ?").bind(now_ms).bind(hash),
        ];
        if let Some(key) = &install.vault_identity_key {
            statements.push(prepare("INSERT INTO runner_vault_identity_history (install_id, public_key, changed_at_ms) VALUES (?, ?, ?)")
                .bind(install.id.as_str()).bind(key).bind(now_ms));
            statements.push(prepare("INSERT INTO runner_vault_identity_audit
                (sequence, install_id, org_id, action, actor, public_key, occurred_at_ms)
                SELECT sequence, install_id, ?, 'runner.vault_identity_enrolled', 'installer bootstrap',
                  public_key, changed_at_ms FROM runner_vault_identity_history
                WHERE install_id = ? ORDER BY sequence DESC LIMIT 1")
                .bind(org).bind(install.id.as_str()));
        }
        self.db
            .batch(statements)
            .await
            .map_err(|error| match error.guard() {
                Some("runner.invalid_token") => RunnerError::InvalidToken,
                Some("runner.already_registered") => RunnerError::AlreadyRegistered,
                Some("runner.url_mismatch") => RunnerError::UrlMismatch,
                _ => store(error),
            })?;
        Ok(InstallRecord {
            id: install.id,
            org_id: parse(org).map_err(store)?,
            url: install.url,
            version: install.version,
            contract_version: install.contract_version,
            created_at_ms: now_ms,
            last_heartbeat_at_ms: None,
            vault_identity_key: install.vault_identity_key,
        })
    }

    async fn join(
        &self,
        install: &RunnerInstallId,
        join: &SecretHash,
        runner: NewRunner,
        now_ms: i64,
    ) -> Result<RunnerId, RunnerError> {
        let group = runner.group_id.as_str();
        let install = install.as_str();
        let mut outcomes = self.db.batch(vec![
            guard("runner.invalid_token", "EXISTS (SELECT 1 FROM runner_installs WHERE id = ? AND join_token_hash = ?)")
                .bind(install).bind(join.as_bytes().as_slice()),
            guard("runner.group_taken", "NOT EXISTS (SELECT 1 FROM runner_groups WHERE id = ? AND install_id <> ?)")
                .bind(group).bind(install),
            prepare("INSERT INTO runner_groups (id, install_id, org_id, name) SELECT ?, id, org_id, ? FROM runner_installs WHERE id = ? ON CONFLICT (id) DO UPDATE SET name = excluded.name")
                .bind(group).bind(&runner.group_name).bind(install),
            prepare("INSERT INTO runner_runners (id, install_id, group_id, org_id, name, version, claim_token_hash, created_at_ms) SELECT ?, id, ?, org_id, ?, ?, ?, ? FROM runner_installs WHERE id = ? ON CONFLICT (group_id, name) DO UPDATE SET claim_token_hash = excluded.claim_token_hash, version = excluded.version, decision_v1 = 0 RETURNING id")
                .bind(runner.id.as_str()).bind(group).bind(&runner.name).bind(&runner.version).bind(runner.claim.as_bytes().as_slice()).bind(now_ms).bind(install),
        ]).await.map_err(|error| match error.guard() {
            Some("runner.invalid_token") => RunnerError::InvalidToken,
            Some("runner.group_taken") => RunnerError::Conflict("runner.group_taken"),
            _ => store(error),
        })?;
        let row = outcomes
            .pop()
            .and_then(|result| result.rows.into_iter().next())
            .ok_or_else(|| store(D1Error::Range))?;
        parse(row.text(0).map_err(store)?).map_err(store)
    }

    async fn authenticate(
        &self,
        token: &SecretHash,
    ) -> Result<Option<RunnerPrincipal>, RunnerError> {
        let rows = self.db.all(prepare(
            "SELECT org_id, id, NULL, NULL, CASE WHEN directory_token_hash = ?1 THEN 'directory' WHEN release_token_hash = ?1 THEN 'release' ELSE 'join' END AS purpose
             FROM runner_installs WHERE directory_token_hash = ?1 OR release_token_hash = ?1 OR join_token_hash = ?1
             UNION ALL SELECT org_id, install_id, id, group_id, 'claim' FROM runner_runners WHERE claim_token_hash = ?1"
        ).bind(token.as_bytes().as_slice())).await.map_err(store)?;
        let principal = match rows.as_slice() {
            [] => return Ok(None),
            [row] => {
                let purpose = match row.text(4).map_err(store)? {
                    "directory" => RunnerPurpose::Directory,
                    "release" => RunnerPurpose::Release,
                    "join" => RunnerPurpose::Join,
                    _ => RunnerPurpose::Claim,
                };
                RunnerPrincipal {
                    org_id: parse(row.text(0).map_err(store)?).map_err(store)?,
                    install_id: parse(row.text(1).map_err(store)?).map_err(store)?,
                    runner_id: row
                        .optional_text(2)
                        .and_then(|id| id.as_deref().map(parse).transpose())
                        .map_err(store)?,
                    group_id: row
                        .optional_text(3)
                        .and_then(|id| id.as_deref().map(parse).transpose())
                        .map_err(store)?,
                    purpose,
                }
            }
            _ => return Err(store(D1Error::Range)),
        };
        Ok(Some(principal))
    }

    async fn heartbeat(
        &self,
        install: &RunnerInstallId,
        heartbeat: &RunnerHeartbeatRequest,
        now_ms: i64,
    ) -> Result<(), RunnerError> {
        let install = install.as_str();
        let mut statements = vec![
            prepare("UPDATE runner_installs SET last_heartbeat_at_ms = ? WHERE id = ?")
                .bind(now_ms)
                .bind(install),
        ];
        statements.extend(heartbeat.groups.iter().map(|group| {
            prepare("UPDATE runner_groups SET name = ? WHERE id = ? AND install_id = ?")
                .bind(&group.name)
                .bind(group.id.as_str())
                .bind(install)
        }));
        statements.extend(heartbeat.runners.iter().map(|runner| {
            prepare("UPDATE runner_runners SET status = ?, running = ?, capacity = ?, last_heartbeat_at_ms = ?, decision_v1 = ? WHERE id = ? AND install_id = ?")
                .bind(&runner.status).bind(runner.running.min(i32::MAX as u32)).bind(runner.capacity.min(i32::MAX as u32)).bind(now_ms).bind(u32::from(runner.decision_contract == Some(RUNNER_DECISION_CONTRACT))).bind(runner.id.as_str()).bind(install)
        }));
        self.db.batch(statements).await.map(|_| ()).map_err(store)
    }

    async fn status(
        &self,
        org: &OrganizationId,
        now_ms: i64,
    ) -> Result<RunnerStatusView, RunnerError> {
        let Some(install) = self.install_of(org).await? else {
            return Ok(no_install());
        };
        let results = self.db.batch(vec![
            prepare("SELECT g.id, g.name, (SELECT json_group_array(project_id ORDER BY project_id) FROM runner_group_projects WHERE group_id = g.id) AS projects FROM runner_groups g WHERE g.install_id = ? ORDER BY g.id").bind(install.id.as_str()),
            prepare("SELECT id, name, group_id, version, status, running, capacity, last_heartbeat_at_ms FROM runner_runners WHERE install_id = ? ORDER BY id").bind(install.id.as_str()),
            prepare("SELECT count(*) FROM runner_ready_steps r
                     JOIN runner_agents a ON a.org_id = r.org_id AND a.agent_id = r.agent_id
                     JOIN runner_group_projects gp ON gp.group_id = a.group_id AND gp.project_id = r.project_id
                     WHERE a.install_id = ? AND r.org_id = ? AND a.project_id = r.project_id AND r.decision IS NULL")
                .bind(install.id.as_str()).bind(org.as_str()),
        ]).await.map_err(store)?;
        let groups = results[0]
            .rows
            .iter()
            .map(|row| {
                Ok(RunnerGroupView {
                    id: parse(row.text(0)?)?,
                    name: row.text(1)?.into(),
                    projects: row.json(2)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(store)?;
        let runners = results[1]
            .rows
            .iter()
            .map(|row| {
                Ok(RunnerView {
                    id: parse(row.text(0)?)?,
                    name: row.text(1)?.into(),
                    group_id: parse(row.text(2)?)?,
                    version: row.text(3)?.into(),
                    status: row.text(4)?.into(),
                    running: u32::try_from(row.integer(5)?).unwrap_or(0),
                    capacity: u32::try_from(row.integer(6)?).unwrap_or(0),
                    last_heartbeat_at: row.optional_integer(7)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(store)?;
        Ok(RunnerStatusView {
            install_id: Some(install.id),
            status: install_status(install.last_heartbeat_at_ms, now_ms),
            version: Some(install.version),
            contract_version: Some(install.contract_version),
            vault_identity_key: install.vault_identity_key,
            groups,
            runners,
            queued_steps: Some(results[2].rows[0].integer(0).map_err(store)? as u64),
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
        self.db.batch(vec![
            guard("not_found", "EXISTS (SELECT 1 FROM runner_installs WHERE org_id = ?)").bind(org.as_str()),
            prepare("INSERT INTO runner_vault_identity_history (install_id, public_key, changed_by, changed_at_ms) SELECT id, ?, ?, ? FROM runner_installs WHERE org_id = ?")
                .bind(key).bind(owner.as_str()).bind(now_ms).bind(org.as_str()),
            prepare("INSERT INTO runner_vault_identity_audit
                (sequence, install_id, org_id, action, actor, public_key, occurred_at_ms)
                SELECT h.sequence, h.install_id, i.org_id, 'runner.vault_identity_changed', h.changed_by,
                  h.public_key, h.changed_at_ms FROM runner_vault_identity_history h
                JOIN runner_installs i ON i.id = h.install_id WHERE i.org_id = ? ORDER BY h.sequence DESC LIMIT 1")
                .bind(org.as_str()),
        ]).await.map(|_| ()).map_err(|error| match error.guard() {
            Some("not_found") => RunnerError::NotFound,
            _ => store(error),
        })
    }

    async fn set_group_projects(
        &self,
        org: &OrganizationId,
        group: &RunnerGroupId,
        projects: &[ProjectId],
    ) -> Result<(), RunnerError> {
        let mut statements = vec![
            guard(
                "not_found",
                "EXISTS (SELECT 1 FROM runner_groups WHERE id = ? AND org_id = ?)",
            )
            .bind(group.as_str())
            .bind(org.as_str()),
            prepare("DELETE FROM runner_group_projects WHERE group_id = ?").bind(group.as_str()),
        ];
        statements.extend(projects.iter().map(|project| {
            prepare("INSERT INTO runner_group_projects (group_id, project_id) VALUES (?, ?) ON CONFLICT DO NOTHING")
                .bind(group.as_str()).bind(project.as_str())
        }));
        self.db
            .batch(statements)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.guard() == Some("not_found") {
                    RunnerError::NotFound
                } else {
                    store(error)
                }
            })
    }

    async fn install_of(&self, org: &OrganizationId) -> Result<Option<InstallRecord>, RunnerError> {
        self.db.all(prepare("SELECT id, org_id, url, version, contract_version, created_at_ms, last_heartbeat_at_ms, (SELECT public_key FROM runner_vault_identity_history h WHERE h.install_id = i.id ORDER BY sequence DESC LIMIT 1) FROM runner_installs i WHERE org_id = ?").bind(org.as_str()))
            .await.map_err(store)?.first().map(install_record).transpose().map_err(store)
    }

    async fn runner_group(
        &self,
        install: &RunnerInstallId,
        runner: &RunnerId,
    ) -> Result<Option<RunnerGroupId>, RunnerError> {
        self.db
            .all(
                prepare("SELECT group_id FROM runner_runners WHERE id = ? AND install_id = ?")
                    .bind(runner.as_str())
                    .bind(install.as_str()),
            )
            .await
            .map_err(store)?
            .first()
            .map(|row| parse(row.text(0)?))
            .transpose()
            .map_err(store)
    }

    async fn runner_supports_decisions(
        &self,
        install: &RunnerInstallId,
        runner: &RunnerId,
        since: i64,
    ) -> Result<bool, RunnerError> {
        Ok(!self
            .db
            .all(
                prepare(
                    "SELECT 1 FROM runner_runners WHERE install_id = ? AND id = ?
             AND decision_v1 = 1 AND last_heartbeat_at_ms > ?",
                )
                .bind(install.as_str())
                .bind(runner.as_str())
                .bind(since),
            )
            .await
            .map_err(store)?
            .is_empty())
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
        let limit = i64::try_from(limit).map_err(|_| store(D1Error::Range))?;
        self.db.all(prepare(
            "SELECT r.step_run_id, r.execution_id, r.project_id, r.agent_id, r.ready_at_ms, r.max_cost_micro_usd, r.decision
             FROM runner_ready_steps r JOIN runner_agents a ON a.org_id = r.org_id AND a.agent_id = r.agent_id
             WHERE a.install_id = ?1 AND a.group_id = ?2 AND a.project_id = r.project_id
             AND (?3 IS NULL OR r.project_id = ?3)
             AND (r.decision IS NULL OR (r.ready_at_ms <= unixepoch('subsec') * 1000
                 AND EXISTS (SELECT 1 FROM runner_runners worker WHERE worker.install_id = ?1
                     AND worker.group_id = ?2 AND worker.decision_v1 = 1 AND worker.last_heartbeat_at_ms > ?7)))
             AND (?4 IS NULL OR (r.ready_at_ms, r.step_run_id) > (?4, ?5))
             AND EXISTS (SELECT 1 FROM runner_group_projects gp WHERE gp.group_id = a.group_id AND gp.project_id = r.project_id)
             ORDER BY r.ready_at_ms, r.step_run_id LIMIT ?6"
        ).bind(install.as_str()).bind(group.as_str()).bind(project.map(ProjectId::as_str))
            .bind(after.map(|(at, _)| at)).bind(after.map(|(_, id)| id.as_str())).bind(limit).bind(decision_since))
            .await.map_err(store)?.iter().map(|row| {
                let decision = row.optional_text(6)?.as_deref().map(serde_json::from_str).transpose()
                    .map_err(|_| D1Error::Range)?;
                Ok(ReadyStepView {
                    kind: if decision.is_some() { RunnerWorkKind::Decision } else { RunnerWorkKind::Step },
                    decision,
                    step_run_id: parse(row.text(0)?)?, execution_id: parse(row.text(1)?)?, project_id: parse(row.text(2)?)?,
                    agent_id: row.text(3)?.into(), ready_at: row.integer(4)?, max_cost_usd: row.optional_integer(5)?.map(|micros| micros as f64 / 1.0e6),
                })
            }).collect::<Result<Vec<_>, _>>().map_err(store)
    }
}
