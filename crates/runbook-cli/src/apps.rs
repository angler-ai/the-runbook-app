//! MCP2 `runbook apps list|revoke`: the OAuth apps (remote MCP clients) you
//! connected through the consent page, from `/v1/me/apps`. Revoking ends the
//! grant at once: its refresh and access tokens stop working.
use crate::agent::Env;
use crate::cliutil::{Exit, OK, api_failed, client, print_json, text, usage};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

#[derive(Subcommand, Debug)]
pub enum AppsCommands {
    #[command(about = "List the OAuth apps (MCP clients) you connected")]
    List(AppsListArgs),
    #[command(about = "Disconnect an OAuth app: its tokens stop working now")]
    Revoke(AppsRevokeArgs),
}

#[derive(Args, Debug)]
pub struct AppsListArgs {
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct AppsRevokeArgs {
    #[arg(help = "Client ID (from `runbook apps list`)")]
    pub client: String,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

pub fn handle_apps(env: &Env, command: AppsCommands) -> i32 {
    let result = match command {
        AppsCommands::List(a) => list(env, a),
        AppsCommands::Revoke(a) => revoke(env, a),
    };
    result.unwrap_or_else(|code| code)
}

/// A client ID as the auth service issues them: letters, digits, `-`, `_`.
fn client_id(id: &str) -> Exit<&str> {
    if !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        Ok(id)
    } else {
        Err(usage(format!("invalid client ID '{id}'")))
    }
}

fn list(env: &Env, a: AppsListArgs) -> Exit<i32> {
    let page = client(env)?.get_json("/v1/me/apps").map_err(api_failed)?;
    if a.json {
        print_json(&page);
        return Ok(OK);
    }
    let items = page["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("No connected apps.");
    }
    let join = |v: &Value| {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(",")
    };
    for app in &items {
        // The name is the app's own, unverified claim.
        let name = app["client_name"].as_str().unwrap_or("(no name)");
        println!(
            "{}  {:?} (unverified)  returns to {}  {}",
            text(app, "client_id"),
            name,
            join(&app["redirect_hosts"]),
            join(&app["scopes"]),
        );
    }
    Ok(OK)
}

fn revoke(env: &Env, a: AppsRevokeArgs) -> Exit<i32> {
    let id = client_id(&a.client)?;
    client(env)?
        .delete(&format!("/v1/me/apps/{id}"))
        .map_err(api_failed)?;
    if a.json {
        print_json(&json!({"revoked": true, "client_id": id}));
    } else {
        println!("disconnected {id}");
    }
    Ok(OK)
}
