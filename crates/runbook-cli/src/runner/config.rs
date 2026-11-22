use super::{file_json, proxy, segment};
use crate::api::{ApiClient, Request};
use crate::cliutil::{Exit, api_failed, parse_id, usage};
use clap::Subcommand;
use runbook_model::{ProjectId, RunnerGroupId, RunnerGroupProjectsRequest};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub(super) enum CatalogCommand {
    /// List catalog entries.
    List,
    /// Show one catalog entry.
    Get { id: String },
    /// Create an entry from public JSON configuration.
    Create {
        #[arg(long)]
        file: PathBuf,
    },
    /// Patch an entry from public JSON configuration.
    Update {
        id: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Delete a catalog entry.
    Delete { id: String },
}

impl CatalogCommand {
    pub(super) fn run(self, api: &ApiClient, install: &str, resource: &str) -> Exit<Value> {
        let (method, path, body) = match self {
            Self::List => ("GET", resource.into(), None),
            Self::Get { id } => ("GET", format!("{resource}/{}", segment(&id)?), None),
            Self::Create { file } => ("POST", resource.into(), Some(file_json(&file)?)),
            Self::Update { id, file } => (
                "PATCH",
                format!("{resource}/{}", segment(&id)?),
                Some(file_json(&file)?),
            ),
            Self::Delete { id } => ("DELETE", format!("{resource}/{}", segment(&id)?), None),
        };
        if let Some(body) = &body {
            runbook_mcp::ops::check_runner_metadata(resource, body)
                .map_err(|error| usage(error.to_string()))?;
        }
        proxy(api, install, method, &path, body.as_ref())
    }
}

#[derive(Subcommand, Debug)]
pub(super) enum McpServerCommand {
    #[command(flatten)]
    Catalog(CatalogCommand),
    /// Discover tools on the configured upstream MCP server.
    Discover { id: String },
    /// Pin a previously discovered tool snapshot.
    Pin { id: String, snapshot_id: String },
    /// Replace tool labels from JSON containing a tool_labels map.
    LabelTools {
        id: String,
        #[arg(long)]
        file: PathBuf,
    },
}

impl McpServerCommand {
    pub(super) fn run(self, api: &ApiClient, install: &str) -> Exit<Value> {
        let (method, id, suffix, body) = match self {
            Self::Catalog(command) => return command.run(api, install, "mcp-servers"),
            Self::Discover { id } => ("POST", id, "/discover", json!({})),
            Self::Pin { id, snapshot_id } => (
                "POST",
                id,
                "/pin",
                json!({"snapshot_id": segment(&snapshot_id)?}),
            ),
            Self::LabelTools { id, file } => {
                let change = file_json(&file)?;
                let labels = &change["tool_labels"];
                runbook_mcp::ops::check_runner_tool_labels(labels)
                    .map_err(|error| usage(error.to_string()))?;
                ("PATCH", id, "", json!({"tool_labels": labels}))
            }
        };
        proxy(
            api,
            install,
            method,
            &format!("mcp-servers/{}{suffix}", segment(&id)?),
            Some(&body),
        )
    }
}

#[derive(Subcommand, Debug)]
pub(super) enum PolicyCommand {
    /// Show runner policy and budgets.
    Get,
    /// Update policy, budgets and per-project limits from JSON.
    Update {
        #[arg(long)]
        file: PathBuf,
    },
}

impl PolicyCommand {
    pub(super) fn run(self, api: &ApiClient, install: &str) -> Exit<Value> {
        match self {
            Self::Get => proxy(api, install, "GET", "policy", None),
            Self::Update { file } => proxy(api, install, "PUT", "policy", Some(&file_json(&file)?)),
        }
    }
}

#[derive(Subcommand, Debug)]
pub(super) enum GroupCommand {
    /// List runner groups.
    List,
    /// Show a runner group.
    Get { group: String },
    /// Modify group configuration, limits and budgets from JSON.
    Update {
        group: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Replace allowed projects from JSON containing project_ids.
    Projects {
        group: String,
        #[arg(long)]
        file: PathBuf,
    },
}

impl GroupCommand {
    pub(super) fn run(
        self,
        api: &ApiClient,
        org: Option<&str>,
        install: Option<&str>,
    ) -> Exit<Value> {
        if let Self::Projects { group, file } = self {
            let group = parse_id::<RunnerGroupId>(&group, "runner group")?;
            let projects: RunnerGroupProjectsRequest = serde_json::from_value(file_json(&file)?)
                .map_err(|_| usage("expected project_ids with an array of project IDs"))?;
            let org = crate::admin::resolve_org(api, org)?;
            if let Some(install) = install {
                let status = super::install(api, Some(&org), None)?;
                if status != install {
                    return Err(usage("--install does not match the organization's runner"));
                }
            }
            let body = json!(projects);
            return api
                .send(&Request {
                    method: "PUT",
                    path: &format!("/v1/orgs/{org}/runner/groups/{group}/projects"),
                    query: &[],
                    body: Some(&body),
                    idempotency_key: None,
                    if_match: None,
                })
                .map(|reply| reply.body)
                .map_err(api_failed);
        }
        let install = super::install(api, org, install)?;
        match self {
            Self::List => proxy(api, &install, "GET", "groups", None),
            Self::Get { group } => proxy(
                api,
                &install,
                "GET",
                &format!("groups/{}", segment(&group)?),
                None,
            ),
            Self::Update { group, file } => {
                let body = file_json(&file)?;
                runbook_mcp::ops::check_runner_metadata("groups", &body)
                    .map_err(|error| usage(error.to_string()))?;
                proxy(
                    api,
                    &install,
                    "PATCH",
                    &format!("groups/{}", segment(&group)?),
                    Some(&body),
                )
            }
            Self::Projects { .. } => unreachable!(),
        }
    }
}

pub(super) fn agent_profiles(
    api: &ApiClient,
    install: &str,
    project: &str,
    agent: &str,
    file: PathBuf,
) -> Exit<Value> {
    let project = parse_id::<ProjectId>(project, "project")?;
    let agent = segment(agent)?;
    let change = file_json(&file)?;
    let profiles: Vec<String> = serde_json::from_value(change["profiles"].clone())
        .map_err(|_| usage("expected profiles with an array of profile IDs"))?;
    for id in &profiles {
        segment(id)?;
    }
    proxy(
        api,
        install,
        "PATCH",
        &format!("projects/{project}/agents/{agent}"),
        Some(&json!({"profiles": profiles})),
    )
}
