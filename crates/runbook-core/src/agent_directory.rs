//! R0-3: the agent directory (runner design §12.3). The runner syncs each
//! agent's current version and ceiling; the core keeps one row per agent
//! so publish and claim check steps without calling the runner. An
//! agent's project must be allowed on its group, and its name is unique in
//! the project (first writer wins; a rename frees the old name). Names
//! resolve to IDs at publish; everything after refers to agents by ID.
use crate::runners::{RunnerError, Unsupported};
use runbook_model::*;
use std::future::Future;

/// The longest agent or version ID the runner may send.
pub const MAX_AGENT_ID_CHARS: usize = 128;

/// A runner-chosen identifier: 1 to 128 ASCII letters, digits, `_`, `-`,
/// `.` or `:`.
pub fn valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_AGENT_ID_CHARS
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

/// Check an entry's own shape before storing it.
pub fn validate_entry(agent_id: &str, entry: &AgentDirectoryEntry) -> Result<(), RunnerError> {
    if entry.agent_id != agent_id
        || !valid_agent_id(&entry.agent_id)
        || !valid_agent_id(&entry.version_id)
    {
        return Err(RunnerError::Invalid("request.invalid"));
    }
    let name = entry.name.trim();
    if name.is_empty() || name != entry.name || entry.name.chars().count() > 128 {
        return Err(RunnerError::Invalid("request.invalid"));
    }
    if entry.name == EXTERNAL_AGENT {
        return Err(RunnerError::Invalid("directory.name_reserved"));
    }
    let c = &entry.ceiling;
    if !c.max_cost_usd.is_finite()
        || c.max_cost_usd < 0.0
        || c.profile_models.iter().any(|(profile, model)| {
            !c.profiles.contains(profile)
                || model.trim().is_empty()
                || model.len() > 256
                || model.chars().any(char::is_control)
        })
    {
        return Err(RunnerError::Invalid("request.invalid"));
    }
    Ok(())
}

pub trait AgentDirectory {
    /// Store an agent's current version (`Forbidden("directory.
    /// project_not_allowed")` when its group does not allow its project,
    /// `Conflict("directory.name_taken")` when another agent holds the
    /// name in the project, `NotFound` for a group not the install's).
    fn upsert_agent(
        &self,
        install: &RunnerInstallId,
        entry: &AgentDirectoryEntry,
        now_ms: i64,
    ) -> impl Future<Output = Result<(), RunnerError>>;
    fn remove_agent(
        &self,
        install: &RunnerInstallId,
        agent_id: &str,
    ) -> impl Future<Output = Result<(), RunnerError>>;
    /// A project's agents (every status) whose group still allows the
    /// project.
    fn project_agents(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> impl Future<Output = Result<Vec<AgentDirectoryEntry>, RunnerError>>;
    /// An org's agent by ID with its install, while its group still allows
    /// its project (so a project removed from a group stops claims and
    /// grants at once).
    fn agent_by_id(
        &self,
        org: &OrganizationId,
        agent_id: &str,
    ) -> impl Future<Output = Result<Option<(RunnerInstallId, AgentDirectoryEntry)>, RunnerError>>;
    fn default_agent(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> impl Future<Output = Result<Option<String>, RunnerError>>;
    fn set_default_agent(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
        name: Option<&str>,
    ) -> impl Future<Output = Result<(), RunnerError>>;
}

impl AgentDirectory for Unsupported {
    async fn upsert_agent(
        &self,
        _: &RunnerInstallId,
        _: &AgentDirectoryEntry,
        _: i64,
    ) -> Result<(), RunnerError> {
        Err(RunnerError::Unsupported)
    }
    async fn remove_agent(&self, _: &RunnerInstallId, _: &str) -> Result<(), RunnerError> {
        Err(RunnerError::Unsupported)
    }
    async fn project_agents(
        &self,
        _: &OrganizationId,
        _: &ProjectId,
    ) -> Result<Vec<AgentDirectoryEntry>, RunnerError> {
        Ok(vec![])
    }
    async fn agent_by_id(
        &self,
        _: &OrganizationId,
        _: &str,
    ) -> Result<Option<(RunnerInstallId, AgentDirectoryEntry)>, RunnerError> {
        Ok(None)
    }
    async fn default_agent(
        &self,
        _: &OrganizationId,
        _: &ProjectId,
    ) -> Result<Option<String>, RunnerError> {
        Ok(None)
    }
    async fn set_default_agent(
        &self,
        _: &OrganizationId,
        _: &ProjectId,
        _: Option<&str>,
    ) -> Result<(), RunnerError> {
        Err(RunnerError::Unsupported)
    }
}
