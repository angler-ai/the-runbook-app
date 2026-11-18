//! SEARCH2: one access-filtered search across every project the caller
//! can read. Each hit names its project so choosing it can switch there.
use crate::{MemoryId, ProjectId, PublicRoute, RunSearchEntry, RunbookId, StepRunId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const UNIVERSAL_SEARCH_ROUTES: &[PublicRoute] = &[PublicRoute {
    method: "GET",
    path: "/v1/orgs/{org}/search",
    request: None,
    response: Some("UniversalSearchView"),
    status: 200,
}];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UniversalRunbookHit {
    pub project_id: ProjectId,
    pub project_slug: String,
    pub runbook_id: RunbookId,
    pub slug: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UniversalRunHit {
    pub project_id: ProjectId,
    pub project_slug: String,
    #[serde(flatten)]
    pub run: RunSearchEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UniversalStepHit {
    pub project_id: ProjectId,
    pub project_slug: String,
    pub execution_id: crate::ExecutionId,
    pub step_run_id: StepRunId,
    pub path: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UniversalMemoryHit {
    pub project_id: ProjectId,
    pub project_slug: String,
    pub memory_id: MemoryId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UniversalSkillHit {
    pub project_id: ProjectId,
    pub project_slug: String,
    pub name: String,
    pub hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UniversalSearchView {
    #[serde(default)]
    pub runbooks: Vec<UniversalRunbookHit>,
    #[serde(default)]
    pub runs: Vec<UniversalRunHit>,
    /// Open steps (assignments) matching the text — the steps a person
    /// can act on now, not a historical step index.
    #[serde(default)]
    pub steps: Vec<UniversalStepHit>,
    #[serde(default)]
    pub memory: Vec<UniversalMemoryHit>,
    #[serde(default)]
    pub skills: Vec<UniversalSkillHit>,
}
