//! C2 pilot administration (PRD §10): `user create|deactivate`,
//! `project create`, `project member add|remove`, `team create` and
//! `team member add|remove`, mapped onto the B4.2 directory routes.
//!
//! Directory routes are organization-scoped. `--org` selects one; without
//! it the CLI reads `GET /v1/orgs` and uses the only organization the
//! credential sees (the pilot has one). Project and team arguments take an
//! ID or a slug; a slug is resolved through the directory list. POSTs carry
//! an `Idempotency-Key`; the directory routes need no `If-Match` revision.
use crate::agent::Env;
use crate::api::ApiClient;
use crate::cliutil::{
    Exit, OK, api_failed, client, fail, idempotency_key, parse_id, print_json, text, usage,
};
use clap::{Args, Subcommand, ValueEnum};
use runbook_model::{OrganizationId, ProjectId, TeamId, UserId};
use serde_json::{Value, json};

#[derive(Subcommand, Debug)]
pub enum UserCommands {
    #[command(about = "Create a user in the organization")]
    Create(UserCreateArgs),
    #[command(about = "Deactivate a user")]
    Deactivate(UserDeactivateArgs),
    #[command(name = "list", about = "List users in the organization")]
    List(UserListArgs),
}

#[derive(Subcommand, Debug)]
pub enum ProjectCommands {
    #[command(about = "Create a project")]
    Create(SlugTitleArgs),
    #[command(subcommand, about = "Grant or revoke project roles")]
    Member(ProjectMemberCommands),
}

#[derive(Subcommand, Debug)]
pub enum ProjectMemberCommands {
    #[command(about = "Grant a user a project role")]
    Add(ProjectMemberAddArgs),
    #[command(about = "Revoke a user's project role")]
    Remove(MemberRemoveArgs),
    #[command(name = "list", about = "List project members")]
    List(ProjectMemberListArgs),
}

#[derive(Subcommand, Debug)]
pub enum TeamCommands {
    #[command(about = "Create a team")]
    Create(SlugTitleArgs),
    #[command(name = "list", about = "List teams in the organization")]
    List(TeamListArgs),
    #[command(subcommand, about = "Add or remove team members")]
    Member(TeamMemberCommands),
}

#[derive(Subcommand, Debug)]
pub enum TeamMemberCommands {
    #[command(about = "Add a user to a team")]
    Add(TeamMemberAddArgs),
    #[command(about = "Remove a user from a team")]
    Remove(MemberRemoveArgs),
}

#[derive(Args, Debug)]
pub struct UserListArgs {
    #[arg(long, help = "Organization ID (default: the only one visible)")]
    pub org: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct TeamListArgs {
    #[arg(long, help = "Organization ID (default: the only one visible)")]
    pub org: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ProjectMemberListArgs {
    #[arg(help = "Project ID or slug")]
    pub project: String,
    #[arg(long, help = "Organization ID (default: the only one visible)")]
    pub org: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct Common {
    #[arg(long, help = "Organization ID (default: the only one visible)")]
    pub org: Option<String>,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct UserCreateArgs {
    #[arg(long, help = "Display name")]
    pub name: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug)]
pub struct UserDeactivateArgs {
    #[arg(help = "User ID (usr_…)")]
    pub user: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug)]
pub struct SlugTitleArgs {
    #[arg(long, help = "Slug (lowercase kebab-case)")]
    pub slug: String,
    #[arg(long, help = "Title")]
    pub title: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RoleArg {
    Viewer,
    Operator,
    Editor,
    Admin,
}

impl RoleArg {
    fn as_str(self) -> &'static str {
        match self {
            RoleArg::Viewer => "viewer",
            RoleArg::Operator => "operator",
            RoleArg::Editor => "editor",
            RoleArg::Admin => "admin",
        }
    }
}

#[derive(Args, Debug)]
pub struct ProjectMemberAddArgs {
    #[arg(help = "Project ID or slug")]
    pub project: String,
    #[arg(help = "User ID (usr_…)")]
    pub user: String,
    #[arg(long, value_enum, default_value_t = RoleArg::Viewer, help = "Project role")]
    pub role: RoleArg,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug)]
pub struct TeamMemberAddArgs {
    #[arg(help = "Team ID or slug")]
    pub team: String,
    #[arg(help = "User ID (usr_…)")]
    pub user: String,
    #[command(flatten)]
    pub common: Common,
}

#[derive(Args, Debug)]
pub struct MemberRemoveArgs {
    #[arg(help = "Project or team ID or slug")]
    pub group: String,
    #[arg(help = "User ID (usr_…)")]
    pub user: String,
    #[arg(long, help = "Organization ID (default: the only one visible)")]
    pub org: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

pub fn handle_user(env: &Env, command: UserCommands) -> i32 {
    let run = || -> Exit<i32> {
        match command {
            UserCommands::Create(a) => {
                let client = client(env)?;
                let org = resolve_org(&client, a.common.org.as_deref())?;
                let key = idempotency_key(&a.common.idempotency_key)?;
                let user = client
                    .post_json(
                        &format!("/v1/orgs/{org}/users"),
                        &key,
                        &json!({"name": a.name}),
                    )
                    .map_err(api_failed)?;
                emit(a.common.json, &user, || {
                    format!("created user {} {}", text(&user, "id"), text(&user, "name"))
                })
            }
            UserCommands::Deactivate(a) => {
                let user_id = parse_id::<UserId>(&a.user, "user")?;
                let client = client(env)?;
                let org = resolve_org(&client, a.common.org.as_deref())?;
                let key = idempotency_key(&a.common.idempotency_key)?;
                let user = client
                    .patch_json(
                        &format!("/v1/orgs/{org}/users/{user_id}"),
                        &key,
                        &json!({"active": false}),
                    )
                    .map_err(api_failed)?;
                emit(a.common.json, &user, || format!("deactivated {user_id}"))
            }
            UserCommands::List(a) => {
                let client = client(env)?;
                let org = resolve_org(&client, a.org.as_deref())?;
                let page = client
                    .get_json(&format!("/v1/orgs/{org}/users"))
                    .map_err(api_failed)?;
                if a.json {
                    print_json(&page);
                    return Ok(OK);
                }
                for u in page["items"].as_array().into_iter().flatten() {
                    let active = if u["active"].as_bool() == Some(true) {
                        "active"
                    } else {
                        "inactive"
                    };
                    println!(
                        "{}  {}  {}  {}",
                        text(u, "id"),
                        text(u, "name"),
                        text(u, "role"),
                        active
                    );
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

pub fn handle_project(env: &Env, command: ProjectCommands) -> i32 {
    let run = || -> Exit<i32> {
        match command {
            ProjectCommands::Create(a) => {
                let client = client(env)?;
                let org = resolve_org(&client, a.common.org.as_deref())?;
                let key = idempotency_key(&a.common.idempotency_key)?;
                let project = client
                    .post_json(
                        &format!("/v1/orgs/{org}/projects"),
                        &key,
                        &json!({"slug": a.slug, "title": a.title}),
                    )
                    .map_err(api_failed)?;
                emit(a.common.json, &project, || {
                    format!(
                        "created project {} {}",
                        text(&project, "id"),
                        text(&project, "slug")
                    )
                })
            }
            ProjectCommands::Member(ProjectMemberCommands::Add(a)) => {
                let user_id = parse_id::<UserId>(&a.user, "user")?;
                let client = client(env)?;
                let project = resolve_project(&client, a.common.org.as_deref(), &a.project)?;
                let key = idempotency_key(&a.common.idempotency_key)?;
                let role = a.role.as_str();
                let member = client
                    .post_json(
                        &format!("/v1/projects/{project}/members"),
                        &key,
                        &json!({"user_id": user_id, "role": role}),
                    )
                    .map_err(api_failed)?;
                emit(a.common.json, &member, || {
                    format!("added {user_id} to {project} as {role}")
                })
            }
            ProjectCommands::Member(ProjectMemberCommands::Remove(a)) => {
                let user_id = parse_id::<UserId>(&a.user, "user")?;
                let client = client(env)?;
                let project = resolve_project(&client, a.org.as_deref(), &a.group)?;
                client
                    .delete(&format!("/v1/projects/{project}/members/{user_id}"))
                    .map_err(api_failed)?;
                emit(
                    a.json,
                    &json!({"removed": true, "project_id": project, "user_id": user_id}),
                    || format!("removed {user_id} from {project}"),
                )
            }
            ProjectCommands::Member(ProjectMemberCommands::List(a)) => {
                let client = client(env)?;
                let project = resolve_project(&client, a.org.as_deref(), &a.project)?;
                let page = client
                    .get_json(&format!("/v1/projects/{project}/members"))
                    .map_err(api_failed)?;
                if a.json {
                    print_json(&page);
                    return Ok(OK);
                }
                for m in page["items"].as_array().into_iter().flatten() {
                    println!("{}  {}", text(m, "user_id"), text(m, "role"));
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

pub fn handle_team(env: &Env, command: TeamCommands) -> i32 {
    let run = || -> Exit<i32> {
        match command {
            TeamCommands::Create(a) => {
                let client = client(env)?;
                let org = resolve_org(&client, a.common.org.as_deref())?;
                let key = idempotency_key(&a.common.idempotency_key)?;
                let team = client
                    .post_json(
                        &format!("/v1/orgs/{org}/teams"),
                        &key,
                        &json!({"slug": a.slug, "title": a.title}),
                    )
                    .map_err(api_failed)?;
                emit(a.common.json, &team, || {
                    format!("created team {} {}", text(&team, "id"), text(&team, "slug"))
                })
            }
            TeamCommands::List(a) => {
                let client = client(env)?;
                let org = resolve_org(&client, a.org.as_deref())?;
                let page = client
                    .get_json(&format!("/v1/orgs/{org}/teams"))
                    .map_err(api_failed)?;
                if a.json {
                    print_json(&page);
                    return Ok(OK);
                }
                for t in page["items"].as_array().into_iter().flatten() {
                    println!(
                        "{}  {}  {}",
                        text(t, "id"),
                        text(t, "slug"),
                        text(t, "title")
                    );
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
            TeamCommands::Member(TeamMemberCommands::Add(a)) => {
                let user_id = parse_id::<UserId>(&a.user, "user")?;
                let client = client(env)?;
                let org = resolve_org(&client, a.common.org.as_deref())?;
                let team = resolve_team(&client, &org, &a.team)?;
                let key = idempotency_key(&a.common.idempotency_key)?;
                let member = client
                    .post_json(
                        &format!("/v1/orgs/{org}/teams/{team}/members"),
                        &key,
                        &json!({"user_id": user_id}),
                    )
                    .map_err(api_failed)?;
                emit(a.common.json, &member, || {
                    format!("added {user_id} to {team}")
                })
            }
            TeamCommands::Member(TeamMemberCommands::Remove(a)) => {
                let user_id = parse_id::<UserId>(&a.user, "user")?;
                let client = client(env)?;
                let org = resolve_org(&client, a.org.as_deref())?;
                let team = resolve_team(&client, &org, &a.group)?;
                client
                    .delete(&format!("/v1/orgs/{org}/teams/{team}/members/{user_id}"))
                    .map_err(api_failed)?;
                emit(
                    a.json,
                    &json!({"removed": true, "team_id": team, "user_id": user_id}),
                    || format!("removed {user_id} from {team}"),
                )
            }
        }
    };
    run().unwrap_or_else(|code| code)
}

fn emit(json: bool, value: &Value, human: impl FnOnce() -> String) -> Exit<i32> {
    if json {
        print_json(value);
    } else {
        println!("{}", human());
    }
    Ok(OK)
}

/// `--org`, or the single organization the credential can see.
pub fn resolve_org(client: &ApiClient, given: Option<&str>) -> Exit<String> {
    if let Some(org) = given {
        return parse_id::<OrganizationId>(org, "organization");
    }
    let page = client.get_json("/v1/orgs").map_err(api_failed)?;
    let ids: Vec<&str> = page
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|o| o.get("id").and_then(Value::as_str))
        .collect();
    match ids.as_slice() {
        [one] => Ok((*one).to_string()),
        [] => Err(fail("no organization is visible to this credential")),
        _ => Err(usage(
            "this credential sees several organizations; pass --org <org_id>",
        )),
    }
}

/// Every item of a cursor-paginated directory list.
fn list_all(client: &ApiClient, path: &str) -> Exit<Vec<Value>> {
    let mut items = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let query: Vec<(&str, &str)> = cursor.iter().map(|c| ("cursor", c.as_str())).collect();
        let page = client.get_json_query(path, &query).map_err(api_failed)?;
        items.extend(
            page.get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        match page.get("next_cursor").and_then(Value::as_str) {
            Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
            _ => return Ok(items),
        }
    }
}

fn find_slug(items: &[Value], slug: &str, what: &str) -> Exit<String> {
    items
        .iter()
        .find(|v| v.get("slug").and_then(Value::as_str) == Some(slug))
        .and_then(|v| v.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| fail(format!("{what} '{slug}' not found")))
}

fn resolve_project(client: &ApiClient, org: Option<&str>, text: &str) -> Exit<String> {
    if let Ok(id) = text.parse::<ProjectId>() {
        return Ok(id.to_string());
    }
    crate::selector::validate_project_ref(text).map_err(usage)?;
    let org = resolve_org(client, org)?;
    let items = list_all(client, &format!("/v1/orgs/{org}/projects"))?;
    find_slug(&items, text, "project")
}

fn resolve_team(client: &ApiClient, org: &str, text: &str) -> Exit<String> {
    if let Ok(id) = text.parse::<TeamId>() {
        return Ok(id.to_string());
    }
    crate::selector::validate_project_ref(text)
        .map_err(|_| usage(format!("invalid team ID or slug: '{text}'")))?;
    let items = list_all(client, &format!("/v1/orgs/{org}/teams"))?;
    find_slug(&items, text, "team")
}
