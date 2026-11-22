//! R0-3: the agent directory on Postgres (`runbook.runner_agents`) and
//! each project's default agent (`runbook.project_runner_settings`).
use crate::{PostgresRunners, rows::db};
use runbook_core::{StoreError, agent_directory::AgentDirectory, runners::RunnerError};
use runbook_model::*;

fn store(e: tokio_postgres::Error) -> RunnerError {
    RunnerError::Store(db(e))
}
fn decode_error() -> RunnerError {
    RunnerError::Store(StoreError::Backend("postgres.decode_failed"))
}
fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Active => "active",
        AgentStatus::Paused => "paused",
        AgentStatus::Archived => "archived",
    }
}
fn status_of(text: &str) -> Result<AgentStatus, RunnerError> {
    match text {
        "active" => Ok(AgentStatus::Active),
        "paused" => Ok(AgentStatus::Paused),
        "archived" => Ok(AgentStatus::Archived),
        _ => Err(decode_error()),
    }
}
const COLUMNS: &str = "agent_id, name, project_id, group_id, status, version_id, ceiling";
/// An agent counts only while its group still allows its project: removing
/// a project from a group stops publish resolution, claims and grants at
/// once, without waiting for the runner's next sync (review M3).
const GROUP_ALLOWS: &str = "EXISTS (SELECT 1 FROM runbook.runner_group_projects gp \
     WHERE gp.group_id = runner_agents.group_id AND gp.project_id = runner_agents.project_id)";
fn entry(row: &tokio_postgres::Row) -> Result<AgentDirectoryEntry, RunnerError> {
    Ok(AgentDirectoryEntry {
        agent_id: row.get(0),
        name: row.get(1),
        project_id: row.get::<_, &str>(2).parse().map_err(|_| decode_error())?,
        group_id: row.get::<_, &str>(3).parse().map_err(|_| decode_error())?,
        status: status_of(row.get(4))?,
        version_id: row.get(5),
        ceiling: serde_json::from_str(row.get(6)).map_err(|_| decode_error())?,
    })
}

impl AgentDirectory for PostgresRunners {
    async fn upsert_agent(
        &self,
        install: &RunnerInstallId,
        entry: &AgentDirectoryEntry,
        now_ms: i64,
    ) -> Result<(), RunnerError> {
        let ceiling = serde_json::to_string(&entry.ceiling)
            .map_err(|_| RunnerError::Invalid("request.invalid"))?;
        let mut conn = self.conn().await?;
        conn.begin(crate::execution::READ_COMMITTED)
            .await
            .map_err(store)?;
        let result = async {
            let c = conn.client();
            let Some(group) = c
                .query_opt(
                    "SELECT org_id FROM runbook.runner_groups WHERE id = $1 AND install_id = $2",
                    &[&entry.group_id.as_str(), &install.as_str()],
                )
                .await
                .map_err(store)?
            else {
                return Err(RunnerError::Forbidden("directory.group_unknown"));
            };
            let org: String = group.get(0);
            let allowed = c
                .query_opt(
                    "SELECT 1 FROM runbook.runner_group_projects WHERE group_id = $1 AND project_id = $2",
                    &[&entry.group_id.as_str(), &entry.project_id.as_str()],
                )
                .await
                .map_err(store)?;
            if allowed.is_none() {
                return Err(RunnerError::Forbidden("directory.project_not_allowed"));
            }
            // The name lock: two agents racing for one name serialize here.
            c.execute(
                "SELECT pg_advisory_xact_lock(hashtext('runner_agent_name:' || $1 || ':' || $2))",
                &[&entry.project_id.as_str(), &entry.name],
            )
            .await
            .map_err(store)?;
            let holder = c
                .query_opt(
                    "SELECT install_id, agent_id FROM runbook.runner_agents WHERE project_id = $1 AND name = $2",
                    &[&entry.project_id.as_str(), &entry.name],
                )
                .await
                .map_err(store)?;
            if let Some(holder) = holder
                && (holder.get::<_, &str>(0) != install.as_str()
                    || holder.get::<_, &str>(1) != entry.agent_id)
            {
                return Err(RunnerError::Conflict("directory.name_taken"));
            }
            c.execute(
                "INSERT INTO runbook.runner_agents (install_id, agent_id, org_id, project_id, group_id, \
                 name, status, version_id, ceiling, updated_at_ms) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (install_id, agent_id) DO UPDATE SET project_id = EXCLUDED.project_id, \
                 group_id = EXCLUDED.group_id, name = EXCLUDED.name, status = EXCLUDED.status, \
                 version_id = EXCLUDED.version_id, ceiling = EXCLUDED.ceiling, \
                 updated_at_ms = EXCLUDED.updated_at_ms",
                &[
                    &install.as_str(),
                    &entry.agent_id,
                    &org,
                    &entry.project_id.as_str(),
                    &entry.group_id.as_str(),
                    &entry.name,
                    &status_label(entry.status),
                    &entry.version_id,
                    &ceiling,
                    &now_ms,
                ],
            )
            .await
            .map_err(|e| {
                if crate::rows::is_unique_violation(&e) {
                    RunnerError::Conflict("directory.name_taken")
                } else {
                    store(e)
                }
            })?;
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

    async fn remove_agent(
        &self,
        install: &RunnerInstallId,
        agent_id: &str,
    ) -> Result<(), RunnerError> {
        let conn = self.conn().await?;
        conn.client()
            .execute(
                "DELETE FROM runbook.runner_agents WHERE install_id = $1 AND agent_id = $2",
                &[&install.as_str(), &agent_id],
            )
            .await
            .map_err(store)?;
        Ok(())
    }

    async fn project_agents(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> Result<Vec<AgentDirectoryEntry>, RunnerError> {
        let conn = self.conn().await?;
        conn.client()
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM runbook.runner_agents WHERE org_id = $1 AND project_id = $2 \
                     AND {GROUP_ALLOWS} ORDER BY name"
                ),
                &[&org.as_str(), &project.as_str()],
            )
            .await
            .map_err(store)?
            .iter()
            .map(entry)
            .collect()
    }

    async fn agent_by_id(
        &self,
        org: &OrganizationId,
        agent_id: &str,
    ) -> Result<Option<(RunnerInstallId, AgentDirectoryEntry)>, RunnerError> {
        let conn = self.conn().await?;
        let row = conn
            .client()
            .query_opt(
                &format!(
                    "SELECT {COLUMNS}, install_id FROM runbook.runner_agents WHERE org_id = $1 AND agent_id = $2 \
                     AND {GROUP_ALLOWS}"
                ),
                &[&org.as_str(), &agent_id],
            )
            .await
            .map_err(store)?;
        row.map(|row| {
            Ok((
                row.get::<_, &str>(7).parse().map_err(|_| decode_error())?,
                entry(&row)?,
            ))
        })
        .transpose()
    }

    async fn default_agent(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> Result<Option<String>, RunnerError> {
        let conn = self.conn().await?;
        Ok(conn
            .client()
            .query_opt(
                "SELECT default_agent FROM runbook.project_runner_settings WHERE org_id = $1 AND project_id = $2",
                &[&org.as_str(), &project.as_str()],
            )
            .await
            .map_err(store)?
            .and_then(|row| row.get(0)))
    }

    async fn set_default_agent(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
        name: Option<&str>,
    ) -> Result<(), RunnerError> {
        let conn = self.conn().await?;
        conn.client()
            .execute(
                "INSERT INTO runbook.project_runner_settings (org_id, project_id, default_agent) \
                 VALUES ($1, $2, $3) ON CONFLICT (org_id, project_id) DO UPDATE SET default_agent = EXCLUDED.default_agent",
                &[&org.as_str(), &project.as_str(), &name],
            )
            .await
            .map_err(store)?;
        Ok(())
    }
}

/// The project's directory and default agent for a compile context.
pub(crate) async fn compile_agents(
    c: &tokio_postgres::Client,
    org: &OrganizationId,
    project: &ProjectId,
) -> Result<runbook_core::authoring::RunnerAgents, StoreError> {
    let failed = |_| StoreError::Backend("postgres.decode_failed");
    let agents = c
        .query(
            &format!(
                "SELECT {COLUMNS} FROM runbook.runner_agents WHERE org_id = $1 AND project_id = $2 \
                 AND {GROUP_ALLOWS} ORDER BY name"
            ),
            &[&org.as_str(), &project.as_str()],
        )
        .await
        .map_err(db)?
        .iter()
        .map(entry)
        .collect::<Result<Vec<_>, _>>()
        .map_err(failed)?;
    let default_agent = c
        .query_opt(
            "SELECT default_agent FROM runbook.project_runner_settings WHERE org_id = $1 AND project_id = $2",
            &[&org.as_str(), &project.as_str()],
        )
        .await
        .map_err(db)?
        .and_then(|row| row.get(0));
    Ok(runbook_core::authoring::RunnerAgents {
        agents,
        default_agent,
    })
}
