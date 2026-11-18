//! `runbook me tasks` against `/v1/me/tasks` (PRD §10).
use crate::agent::Env;
use crate::api::ApiError;
use crate::cliutil::{Exit, OK, api_failed, client, print_json, text};
use clap::{Args, Subcommand};
use serde_json::Value;

#[derive(Subcommand, Debug)]
pub enum MeCommands {
    #[command(about = "List open tasks and approvals for the current user")]
    Tasks(MeTasksArgs),
}

#[derive(Args, Debug)]
pub struct MeTasksArgs {
    #[arg(long, help = "Filter by item kind: approval, human_task")]
    pub kind: Option<String>,
    #[arg(long, help = "Page limit (1-100)")]
    pub limit: Option<usize>,
    #[arg(long, help = "Pagination cursor")]
    pub cursor: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

pub fn handle_me(env: &Env, command: MeCommands) -> i32 {
    let run = || -> Exit<i32> {
        match command {
            MeCommands::Tasks(a) => {
                let client = client(env)?;
                let mut query = Vec::new();
                if let Some(kind) = &a.kind {
                    query.push(("kind", kind.as_str()));
                }
                let limit_str;
                if let Some(limit) = a.limit {
                    limit_str = limit.to_string();
                    query.push(("limit", limit_str.as_str()));
                }
                if let Some(cursor) = &a.cursor {
                    query.push(("cursor", cursor.as_str()));
                }
                let page = client
                    .get_json_query("/v1/me/tasks", &query)
                    .map_err(|e| match &e {
                        ApiError::Problem { status: 404, .. } => 4,
                        _ => api_failed(e),
                    })?;
                if a.json {
                    print_json(&page);
                    return Ok(OK);
                }
                for t in page["items"].as_array().into_iter().flatten() {
                    let id = if !text(t, "step_run_id").is_empty() {
                        text(t, "step_run_id")
                    } else {
                        text(t, "id")
                    };
                    let kind = text(t, "kind");
                    let path = text(t, "path");
                    let title = text(t, "title");
                    if !path.is_empty() {
                        println!("{id}  {kind}  {path}  {title}");
                    } else {
                        println!("{id}  {kind}  {title}");
                    }
                }
                if let Some(next) = page
                    .get("next_cursor")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    println!("next: {next}");
                }
                Ok(OK)
            }
        }
    };
    run().unwrap_or_else(|code| code)
}
