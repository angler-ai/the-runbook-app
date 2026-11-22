//! The runner-synced directory and project defaults on D1.
use crate::{
    runners::{D1Runners, parse, store},
    sql::{Columns, D1, D1Error, Row, guard, prepare},
};
use runbook_core::{agent_directory::AgentDirectory, runners::RunnerError};
use runbook_model::*;

const COLUMNS: &str = "agent_id, name, project_id, group_id, status, version_id, ceiling";

// Removing a project's group access immediately fences directory reads.
const GROUP_ALLOWS: &str = "EXISTS (SELECT 1 FROM runner_group_projects gp \
    WHERE gp.group_id = runner_agents.group_id AND gp.project_id = runner_agents.project_id)";

fn decode(row: &Row) -> Result<AgentDirectoryEntry, D1Error> {
    let status = match row.text(4)? {
        "active" => AgentStatus::Active,
        "paused" => AgentStatus::Paused,
        "archived" => AgentStatus::Archived,
        _ => return Err(D1Error::Range),
    };
    Ok(AgentDirectoryEntry {
        agent_id: row.text(0)?.into(),
        name: row.text(1)?.into(),
        project_id: parse(row.text(2)?)?,
        group_id: parse(row.text(3)?)?,
        status,
        version_id: row.text(5)?.into(),
        ceiling: row.json(6)?,
    })
}

impl<D: D1> AgentDirectory for D1Runners<D> {
    async fn upsert_agent(
        &self,
        install: &RunnerInstallId,
        entry: &AgentDirectoryEntry,
        now_ms: i64,
    ) -> Result<(), RunnerError> {
        let ceiling = serde_json::to_string(&entry.ceiling)
            .map_err(|_| RunnerError::Invalid("request.invalid"))?;
        let status = match entry.status {
            AgentStatus::Active => "active",
            AgentStatus::Paused => "paused",
            AgentStatus::Archived => "archived",
        };
        let group = entry.group_id.as_str();
        let install = install.as_str();
        let project = entry.project_id.as_str();
        self.db.batch(vec![
            guard("directory.group_unknown", "EXISTS (SELECT 1 FROM runner_groups WHERE id = ? AND install_id = ?)")
                .bind(group).bind(install),
            guard("directory.project_not_allowed", "EXISTS (SELECT 1 FROM runner_group_projects WHERE group_id = ? AND project_id = ?)")
                .bind(group).bind(project),
            prepare("INSERT INTO runner_agents (install_id, agent_id, org_id, project_id, group_id, name, status, version_id, ceiling, updated_at_ms)
                     SELECT install_id, ?, org_id, ?, id, ?, ?, ?, ?, ? FROM runner_groups WHERE id = ?
                     ON CONFLICT (install_id, agent_id) DO UPDATE SET project_id = excluded.project_id,
                     group_id = excluded.group_id, name = excluded.name, status = excluded.status,
                     version_id = excluded.version_id, ceiling = excluded.ceiling, updated_at_ms = excluded.updated_at_ms")
                .bind(&entry.agent_id).bind(project).bind(&entry.name).bind(status)
                .bind(&entry.version_id).bind(ceiling).bind(now_ms).bind(group),
        ]).await.map(|_| ()).map_err(|error| match error {
            D1Error::Unique => RunnerError::Conflict("directory.name_taken"),
            _ => match error.guard() {
                Some("directory.group_unknown") => RunnerError::Forbidden("directory.group_unknown"),
                Some("directory.project_not_allowed") => RunnerError::Forbidden("directory.project_not_allowed"),
                _ => store(error),
            },
        })
    }

    async fn remove_agent(
        &self,
        install: &RunnerInstallId,
        agent_id: &str,
    ) -> Result<(), RunnerError> {
        self.db
            .run(
                prepare("DELETE FROM runner_agents WHERE install_id = ? AND agent_id = ?")
                    .bind(install.as_str())
                    .bind(agent_id),
            )
            .await
            .map(|_| ())
            .map_err(store)
    }

    async fn project_agents(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> Result<Vec<AgentDirectoryEntry>, RunnerError> {
        self.db
            .all(
                prepare(format!(
                    "SELECT {COLUMNS} FROM runner_agents WHERE org_id = ? AND project_id = ?
                             AND {GROUP_ALLOWS} ORDER BY name"
                ))
                .bind(org.as_str())
                .bind(project.as_str()),
            )
            .await
            .map_err(store)?
            .iter()
            .map(decode)
            .collect::<Result<_, _>>()
            .map_err(store)
    }

    async fn agent_by_id(
        &self,
        org: &OrganizationId,
        agent_id: &str,
    ) -> Result<Option<(RunnerInstallId, AgentDirectoryEntry)>, RunnerError> {
        let rows = self
            .db
            .all(
                prepare(format!(
                    "SELECT {COLUMNS}, install_id FROM runner_agents WHERE org_id = ?
                             AND agent_id = ? AND {GROUP_ALLOWS}"
                ))
                .bind(org.as_str())
                .bind(agent_id),
            )
            .await
            .map_err(store)?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => {
                let install = parse(row.text(7).map_err(store)?).map_err(store)?;
                Ok(Some((install, decode(row).map_err(store)?)))
            }
            _ => Err(store(D1Error::Range)),
        }
    }

    async fn default_agent(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> Result<Option<String>, RunnerError> {
        Ok(self.db.all(
            prepare("SELECT default_agent FROM project_runner_settings WHERE org_id = ? AND project_id = ?")
                .bind(org.as_str()).bind(project.as_str()),
        ).await.map_err(store)?.first()
            .map(|row| row.optional_text(0)).transpose().map_err(store)?.flatten())
    }

    async fn set_default_agent(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
        name: Option<&str>,
    ) -> Result<(), RunnerError> {
        self.db.run(
            prepare("INSERT INTO project_runner_settings (org_id, project_id, default_agent)
                     VALUES (?, ?, ?) ON CONFLICT (org_id, project_id) DO UPDATE SET default_agent = excluded.default_agent")
                .bind(org.as_str()).bind(project.as_str()).bind(name),
        ).await.map(|_| ()).map_err(store)
    }
}
