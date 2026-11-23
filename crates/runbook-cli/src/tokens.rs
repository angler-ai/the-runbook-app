//! C2 `runbook token create|list|revoke` against `/v1/tokens` (PRD §9).
//!
//! Issuance is always for the authenticated user. `token create --user`
//! only forwards `user_id`; the server rejects anyone but the caller
//! (`403 auth.forbidden`), so the CLI never impersonates. The secret
//! is printed once, on stdout of `token create` only, and never logged.
use crate::agent::Env;
use crate::cliutil::{
    Exit, OK, api_failed, client, idempotency_key, parse_id, print_json, text, usage,
};
use crate::session::now_ms;
use clap::{Args, Subcommand};
use runbook_model::{TokenId, UserId};
use serde_json::{Map, Value, json};

/// The PRD §9 scope names, and the runner's (R0-2), which a token holds
/// only when chosen explicitly.
pub const SCOPES: &[&str] = &[
    "runbooks:read",
    "runbooks:write",
    "runbooks:publish",
    "executions:read",
    "executions:start",
    "executions:act",
    "executions:admin",
    "skills:read",
    "skills:write",
    "tokens:manage",
    "webhooks:manage",
    "project:admin",
    "runner:read",
    "runner:admin",
];

/// PATs last at most one year.
const MAX_LIFETIME_MS: i64 = 365 * 86_400_000;

#[derive(Subcommand, Debug)]
pub enum TokenCommands {
    #[command(about = "Create a personal access token (the secret is shown once)")]
    Create(TokenCreateArgs),
    #[command(about = "List your tokens")]
    List(TokenListArgs),
    #[command(about = "Revoke a token")]
    Revoke(TokenRevokeArgs),
}

#[derive(Args, Debug)]
pub struct TokenCreateArgs {
    #[arg(long, help = "Token name")]
    pub name: Option<String>,
    #[arg(long = "scope", help = "Narrow to a scope (repeat for each)")]
    pub scopes: Vec<String>,
    #[arg(long = "project", help = "Narrow to a project ID or slug (repeat)")]
    pub projects: Vec<String>,
    #[arg(
        long = "expires-in",
        alias = "expires",
        help = "Lifetime: <n>d or <n>h, at most 365d"
    )]
    pub expires: Option<String>,
    #[arg(long, help = "Token kind: personal or service (default personal)")]
    pub kind: Option<String>,
    #[arg(
        long,
        help = "Role for service token: viewer, operator, editor, admin (default operator)"
    )]
    pub role: Option<String>,
    #[arg(
        long,
        help = "Agent-kind token (D26): agent provenance with you as sponsor; never casts human approvals"
    )]
    pub agent: bool,
    #[arg(
        long,
        help = "Must be you: the server never issues tokens for another user (usr_…)"
    )]
    pub user: Option<String>,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output the response (with the secret) as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct TokenListArgs {
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct TokenRevokeArgs {
    #[arg(help = "Token ID (tok_…)")]
    pub token: String,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

/// `<n>d` or `<n>h` as milliseconds, within one year.
pub fn parse_lifetime(text: &str) -> Result<i64, String> {
    let bad = || format!("invalid --expires '{text}'; use <n>d or <n>h (at most 365d)");
    let (last, _) = text.char_indices().last().ok_or_else(bad)?;
    let (digits, unit) = text.split_at(last);
    let n: i64 = digits.parse().map_err(|_| bad())?;
    let ms = match unit {
        "d" => n.checked_mul(86_400_000),
        "h" => n.checked_mul(3_600_000),
        _ => None,
    }
    .ok_or_else(bad)?;
    if ms <= 0 || ms > MAX_LIFETIME_MS {
        return Err(bad());
    }
    Ok(ms)
}

pub fn handle_token(env: &Env, command: TokenCommands) -> i32 {
    let result = match command {
        TokenCommands::Create(a) => create(env, a),
        TokenCommands::List(a) => list(env, a),
        TokenCommands::Revoke(a) => revoke(env, a),
    };
    result.unwrap_or_else(|code| code)
}

fn create(env: &Env, a: TokenCreateArgs) -> Exit<i32> {
    let mut body = Map::new();
    if let Some(kind) = &a.kind {
        match kind.as_str() {
            "service" => {
                body.insert("kind".into(), json!("service"));
                body.insert("actor_kind".into(), json!("service"));
                if a.projects.len() != 1 {
                    return Err(usage("service token requires exactly one --project"));
                }
                if a.scopes.is_empty() {
                    return Err(usage("service token requires at least one --scope"));
                }
                let role = match a.role.as_deref() {
                    Some("viewer") => "viewer",
                    Some("operator") => "operator",
                    Some("editor") => "editor",
                    Some("admin") => "admin",
                    Some(bad) => {
                        return Err(usage(format!(
                            "invalid role '{bad}'; expected viewer, operator, editor, or admin"
                        )));
                    }
                    None => "operator",
                };
                body.insert("role".into(), json!(role));
            }
            "personal" => {
                body.insert("kind".into(), json!("personal"));
            }
            bad => {
                return Err(usage(format!(
                    "invalid --kind '{bad}'; expected personal or service"
                )));
            }
        }
    }
    if let Some(name) = &a.name {
        body.insert("name".into(), json!(name));
    }
    if !a.scopes.is_empty() {
        if let Some(bad) = a.scopes.iter().find(|s| !SCOPES.contains(&s.as_str())) {
            return Err(usage(format!(
                "unknown scope '{bad}'; expected one of {}",
                SCOPES.join(", ")
            )));
        }
        body.insert("scopes".into(), json!(a.scopes));
    }
    if !a.projects.is_empty() {
        for p in &a.projects {
            crate::selector::validate_project_ref(p).map_err(usage)?;
        }
        body.insert("projects".into(), json!(a.projects));
    }
    if let Some(expires) = &a.expires {
        let lifetime = parse_lifetime(expires).map_err(usage)?;
        body.insert("expires_at_ms".into(), json!(now_ms() + lifetime));
    }
    if a.agent {
        body.insert("actor_kind".into(), json!("agent"));
    }
    if let Some(user) = &a.user {
        body.insert("user_id".into(), json!(parse_id::<UserId>(user, "user")?));
    }
    let client = client(env)?;
    let key = idempotency_key(&a.idempotency_key)?;
    let created = client
        .post_json("/v1/tokens", &key, &Value::Object(body))
        .map_err(api_failed)?;
    if a.json {
        print_json(&created);
        return Ok(OK);
    }
    let view = created.get("token").unwrap_or(&created);
    let Some(secret) = created.get("secret").and_then(Value::as_str) else {
        eprintln!("error: the response carried no secret");
        return Ok(1);
    };
    println!("created token {} {}", text(view, "id"), text(view, "name"));
    println!("{secret}");
    eprintln!("Store this secret now; it is shown only once.");
    Ok(OK)
}

fn list(env: &Env, a: TokenListArgs) -> Exit<i32> {
    let page = client(env)?.get_json("/v1/tokens").map_err(api_failed)?;
    if a.json {
        print_json(&page);
        return Ok(OK);
    }
    for t in page["items"].as_array().into_iter().flatten() {
        let scopes: Vec<&str> = t["scopes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let scopes = if scopes.is_empty() {
            "(all of your permissions)".to_string()
        } else {
            scopes.join(",")
        };
        println!("{}  {}  {scopes}", text(t, "id"), text(t, "name"));
    }
    Ok(OK)
}

fn revoke(env: &Env, a: TokenRevokeArgs) -> Exit<i32> {
    let id = parse_id::<TokenId>(&a.token, "token")?;
    client(env)?
        .delete(&format!("/v1/tokens/{id}"))
        .map_err(api_failed)?;
    if a.json {
        print_json(&json!({"revoked": true, "id": id}));
    } else {
        println!("revoked {id}");
    }
    Ok(OK)
}
