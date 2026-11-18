use clap::{Args, Parser, Subcommand};
use runbook_cli::admin;
use runbook_cli::agent::{self, Env, RunTokenArgs, StepCommands, WhoamiArgs};
use runbook_cli::api::{ApiClient, ApiError};
use runbook_cli::apps;
use runbook_cli::archive;
use runbook_cli::auth;
use runbook_cli::config::{Config, ConfigError, config_path, resolve_host};
use runbook_cli::credstore::StateDir;
use runbook_cli::memory;
use runbook_cli::parse_key_value_pairs;
use runbook_cli::reports::{self, ReportFormat};
use runbook_cli::selector::{
    parse_runbook_ref, parse_version_selector, validate_project_ref, validate_signal_name,
};
use runbook_cli::sse::extract_sse_frame;
use runbook_cli::tokens;
use runbook_cli::validate::validate_source;
use runbook_cli::webhooks;
use serde_json::{Map, Value};
use std::env;
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process;

#[derive(Parser, Debug)]
#[command(
    name = "runbook",
    about = "The Runbook CLI",
    version,
    subcommand_required = true,
    arg_required_else_help = true,
    propagate_version = true
)]
pub struct Cli {
    #[arg(long, global = true, help = "Runbook server host URL")]
    pub host: Option<String>,

    #[arg(long, global = true, help = "Configured context name to use")]
    pub context: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(subcommand, about = "Manage CLI contexts")]
    Context(ContextCommands),
    #[command(subcommand, about = "Manage and validate runbook specs")]
    Spec(SpecCommands),
    #[command(subcommand, about = "Manage and inspect executions")]
    Run(RunCommands),
    #[command(subcommand, about = "Agent loop: next, claim, heartbeat, log, report")]
    Step(StepCommands),
    #[command(about = "Show which credentials and host the CLI uses")]
    Whoami(WhoamiArgs),
    #[command(subcommand, about = "Current user tasks and inbox")]
    Me(runbook_cli::me::MeCommands),
    #[command(subcommand, about = "Pilot administration: users")]
    User(admin::UserCommands),
    #[command(subcommand, about = "Pilot administration: projects and project roles")]
    Project(admin::ProjectCommands),
    #[command(subcommand, about = "Pilot administration: teams and team members")]
    Team(admin::TeamCommands),
    #[command(about = "Store a personal access token for the current host")]
    Login(auth::LoginArgs),
    #[command(about = "Forget the stored token for the current host")]
    Logout(auth::LogoutArgs),
    #[command(subcommand, about = "Create, list and revoke personal access tokens")]
    Token(tokens::TokenCommands),
    #[command(
        subcommand,
        about = "List and disconnect the OAuth apps (MCP clients) you connected"
    )]
    Apps(apps::AppsCommands),
    #[command(subcommand, about = "Manage webhook subscriptions and deliveries")]
    Webhook(webhooks::WebhookCommands),
    #[command(subcommand, about = "Search, save and manage runbook memory")]
    Memory(memory::MemoryCommands),
    #[command(subcommand, about = "List and download runbook execution archives")]
    Archive(archive::ArchiveCommands),
    #[command(flatten)]
    Authoring(runbook_cli::authoring::AuthoringCommands),
    #[command(subcommand, about = "Model Context Protocol server")]
    Mcp(runbook_cli::mcp::McpCommands),
    #[command(subcommand, about = "Manage and install agent skills")]
    Skill(runbook_cli::skill::SkillCommands),
    #[command(
        subcommand,
        about = "Sync a repository folder of runbooks with a project (push from CI, pull to start)"
    )]
    Sync(runbook_cli::sync::SyncCommands),
    #[command(about = "Show this CLI's release and the server's release and schema")]
    Version(runbook_cli::version::VersionArgs),
}

impl Commands {
    pub fn talks_to_server(&self) -> bool {
        match self {
            Commands::Context(_) | Commands::Spec(SpecCommands::Validate(_)) => false,
            Commands::Skill(cmd) => cmd.talks_to_server(),
            _ => true,
        }
    }

    pub fn is_json_or_jsonl(&self) -> bool {
        match self {
            Commands::Context(_) => false,
            Commands::Spec(SpecCommands::Validate(args)) => args.json,
            Commands::Run(run_cmd) => match run_cmd {
                RunCommands::Start(args) => args.json,
                RunCommands::Get(args) => args.json,
                RunCommands::List(args) => args.json || args.jsonl,
                RunCommands::Watch(args) => args.json || args.jsonl,
                RunCommands::Signal(args) => args.json,
                RunCommands::Report(args) => args.json,
                RunCommands::Cancel(args) => args.json,
                RunCommands::Pause(args) => args.json,
                RunCommands::Resume(args) => args.json,
                RunCommands::Update(args) => args.json,
                RunCommands::Rerun(args) => args.json,
                RunCommands::Fields(args) => match &args.command {
                    RunFieldsCommands::Set(a) => a.json,
                },
                RunCommands::Token(args) => args.json,
            },
            Commands::Step(step_cmd) => match step_cmd {
                StepCommands::Next(args) => args.json,
                StepCommands::Get(args) => args.json,
                StepCommands::SkillFile(args) => args.json,
                StepCommands::Claim(args) => args.post.json,
                StepCommands::Heartbeat(args) => args.post.json,
                StepCommands::Log(args) => args.post.json,
                StepCommands::Report(args) => args.post.json,
                StepCommands::Evidence(args) => args.post.json,
                StepCommands::Checklist(args) => args.post.json,
                StepCommands::Decide(args) => args.post.json,
                StepCommands::Release(args) => args.post.json,
                StepCommands::RequestHuman(args) => args.post.json,
                StepCommands::Exec(args) => args.post.json,
            },
            Commands::Whoami(args) => args.json,
            Commands::Me(me_cmd) => match me_cmd {
                runbook_cli::me::MeCommands::Tasks(args) => args.json,
            },
            Commands::User(user_cmd) => match user_cmd {
                admin::UserCommands::Create(args) => args.common.json,
                admin::UserCommands::Deactivate(args) => args.common.json,
                admin::UserCommands::List(args) => args.json,
            },
            Commands::Project(proj_cmd) => match proj_cmd {
                admin::ProjectCommands::Create(args) => args.common.json,
                admin::ProjectCommands::Member(member_cmd) => match member_cmd {
                    admin::ProjectMemberCommands::Add(args) => args.common.json,
                    admin::ProjectMemberCommands::Remove(args) => args.json,
                    admin::ProjectMemberCommands::List(args) => args.json,
                },
            },
            Commands::Team(team_cmd) => match team_cmd {
                admin::TeamCommands::Create(args) => args.common.json,
                admin::TeamCommands::List(args) => args.json,
                admin::TeamCommands::Member(member_cmd) => match member_cmd {
                    admin::TeamMemberCommands::Add(args) => args.common.json,
                    admin::TeamMemberCommands::Remove(args) => args.json,
                },
            },
            Commands::Login(args) => args.json,
            Commands::Logout(args) => args.json,
            Commands::Token(tok_cmd) => match tok_cmd {
                tokens::TokenCommands::Create(args) => args.json,
                tokens::TokenCommands::List(args) => args.json,
                tokens::TokenCommands::Revoke(args) => args.json,
            },
            Commands::Apps(apps_cmd) => match apps_cmd {
                apps::AppsCommands::List(args) => args.json,
                apps::AppsCommands::Revoke(args) => args.json,
            },
            Commands::Webhook(whk_cmd) => match whk_cmd {
                webhooks::WebhookCommands::Create(args) => args.json,
                webhooks::WebhookCommands::List(args) => args.json,
                webhooks::WebhookCommands::Update(args) => args.json,
                webhooks::WebhookCommands::Delete(args) => args.json,
                webhooks::WebhookCommands::Test(args) => args.json,
                webhooks::WebhookCommands::Deliveries(args) => args.json,
                webhooks::WebhookCommands::Redeliver(args) => args.json,
                webhooks::WebhookCommands::RotateSecret(args) => args.json,
            },
            Commands::Memory(mem_cmd) => match mem_cmd {
                memory::MemoryCommands::Search(args) => args.json,
                memory::MemoryCommands::Save(args) => args.json,
                memory::MemoryCommands::Edit(args) => args.json,
                memory::MemoryCommands::History(args) => args.json,
                memory::MemoryCommands::Revert(args) => args.json,
                memory::MemoryCommands::Lock(args) => args.json,
                memory::MemoryCommands::Unlock(args) => args.json,
                memory::MemoryCommands::List(args) => args.json,
                memory::MemoryCommands::Accept(args) => args.json,
                memory::MemoryCommands::Reject(args) => args.json,
                memory::MemoryCommands::Archive(args) => args.json,
            },
            Commands::Archive(arch_cmd) => match arch_cmd {
                archive::ArchiveCommands::List(args) => args.json,
                archive::ArchiveCommands::Download(_) => false,
            },
            Commands::Authoring(auth_cmd) => match auth_cmd {
                runbook_cli::authoring::AuthoringCommands::List(args) => args.json,
                runbook_cli::authoring::AuthoringCommands::Get(args) => args.json,
                runbook_cli::authoring::AuthoringCommands::Pull(args) => args.json,
                runbook_cli::authoring::AuthoringCommands::Push(args) => args.json,
                runbook_cli::authoring::AuthoringCommands::Publish(args) => args.json,
                runbook_cli::authoring::AuthoringCommands::Diff(args) => args.json,
            },
            Commands::Mcp(_) => false,
            Commands::Skill(cmd) => cmd.is_json(),
            Commands::Sync(runbook_cli::sync::SyncCommands::Push(args)) => args.json,
            Commands::Sync(runbook_cli::sync::SyncCommands::Pull(args)) => args.json,
            Commands::Version(args) => args.json,
        }
    }
}

fn should_skip_version_check(command: &Commands) -> bool {
    if !command.talks_to_server() || matches!(command, Commands::Version(_)) {
        return true;
    }
    if command.is_json_or_jsonl() {
        return true;
    }
    if !std::io::stderr().is_terminal() && std::env::var_os("CI").is_some() {
        return true;
    }
    false
}

fn has_refused_file(command: &Commands, store: Option<&StateDir>) -> bool {
    let Some(store) = store else { return false };
    match command {
        Commands::Authoring(runbook_cli::authoring::AuthoringCommands::Push(args)) => {
            store.refuse_inside(&args.file).is_err()
        }
        Commands::Step(StepCommands::Report(args)) => {
            if let Some(path) = &args.log_file
                && store.refuse_inside(path).is_err()
            {
                return true;
            }
            args.evidence.iter().any(|e| {
                let p = std::path::Path::new(e);
                p.exists() && store.refuse_inside(p).is_err()
            })
        }
        Commands::Step(StepCommands::Evidence(args)) => store.refuse_inside(&args.file).is_err(),
        Commands::Authoring(runbook_cli::authoring::AuthoringCommands::Diff(args)) => store
            .refuse_inside(std::path::Path::new(&args.first))
            .is_err(),
        _ => false,
    }
}

#[derive(Subcommand, Debug)]
pub enum ContextCommands {
    #[command(about = "Add or update a context")]
    Add(ContextAddArgs),
    #[command(about = "Switch current context")]
    Use(ContextUseArgs),
    #[command(name = "list", about = "List all contexts")]
    List(ContextListArgs),
    #[command(about = "Remove a context and forget its login")]
    Remove(ContextUseArgs),
}

#[derive(Args, Debug)]
pub struct ContextListArgs {
    #[arg(long, help = "Output as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ContextAddArgs {
    #[arg(help = "Context name")]
    pub name: String,

    #[arg(long, help = "Runbook server host URL")]
    pub host: String,

    #[arg(
        long = "default-project",
        value_name = "PROJECT",
        help = "Project (ID or slug) commands use in this context when they name none"
    )]
    pub default_project: Option<String>,
}

#[derive(Args, Debug)]
pub struct ContextUseArgs {
    #[arg(help = "Context name")]
    pub name: String,
}

#[derive(Subcommand, Debug)]
pub enum SpecCommands {
    #[command(about = "Validate a runbook spec offline")]
    Validate(SpecValidateArgs),
}

#[derive(Args, Debug)]
pub struct SpecValidateArgs {
    #[arg(help = "Path to the runbook spec file")]
    pub file: PathBuf,

    #[arg(long, help = "Output validation report as JSON")]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum RunCommands {
    #[command(about = "Start an execution")]
    Start(RunStartArgs),
    #[command(about = "Get an execution's details")]
    Get(RunGetArgs),
    #[command(name = "list", about = "List executions")]
    List(RunListArgs),
    #[command(name = "watch", about = "Watch execution events in real time")]
    Watch(RunWatchArgs),
    #[command(name = "signal", about = "Send a signal to an execution")]
    Signal(RunSignalArgs),
    #[command(name = "report", about = "View an execution report")]
    Report(RunReportArgs),
    #[command(name = "cancel", about = "Cancel an execution")]
    Cancel(RunControlArgs),
    #[command(name = "pause", about = "Pause an execution")]
    Pause(RunControlArgs),
    #[command(name = "resume", about = "Resume an execution")]
    Resume(RunControlArgs),
    #[command(name = "update", about = "Update execution metadata")]
    Update(RunUpdateArgs),
    #[command(name = "rerun", about = "Rerun an execution")]
    Rerun(RunRerunArgs),
    #[command(name = "fields", about = "Manage execution fields")]
    Fields(RunFieldsArgs),
    #[command(name = "token", about = "Mint an execution access/refresh pair")]
    Token(RunTokenArgs),
}

#[derive(Args, Debug)]
pub struct RunStartArgs {
    #[arg(help = "Runbook reference (<runbook>[@<n|rbv_id>])")]
    pub runbook: String,

    #[arg(long, env = "RUNBOOK_PROJECT", help = "Project ID or slug")]
    pub project: String,

    #[arg(long = "input", help = "Input parameter (key=val)")]
    pub inputs: Vec<String>,

    #[arg(long = "field", help = "Field parameter (key=val)")]
    pub fields: Vec<String>,

    #[arg(long = "tag", help = "Execution tag")]
    pub tags: Vec<String>,

    #[arg(long, help = "Execution title")]
    pub title: Option<String>,

    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,

    #[arg(
        long,
        help = "Poll until the execution is terminal; exit 3 if it failed or was cancelled"
    )]
    pub wait: bool,

    #[arg(
        long,
        help = "Stream the execution's events until it ends (implies --wait)"
    )]
    pub follow: bool,

    #[arg(long = "interval-ms", default_value_t = 2000, hide = true)]
    pub interval_ms: u64,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RunGetArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(
        long,
        help = "Poll until the execution is terminal; exit 3 if it failed"
    )]
    pub wait: bool,

    #[arg(
        long = "fail-on",
        value_delimiter = ',',
        help = "Exit 1 if final status matches (e.g. failed, cancelled)"
    )]
    pub fail_on: Vec<String>,

    #[arg(long = "interval-ms", default_value_t = 2000, hide = true)]
    pub interval_ms: u64,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RunListArgs {
    #[arg(long, env = "RUNBOOK_PROJECT", help = "Project ID or slug")]
    pub project: String,

    #[arg(long, value_delimiter = ',', help = "Filter by status (comma list)")]
    pub status: Vec<String>,

    #[arg(long = "tag", help = "Filter by tag")]
    pub tags: Vec<String>,

    #[arg(long, help = "Filter by runbook (<slug|rb_id>)")]
    pub runbook: Option<String>,

    #[arg(long = "field", help = "Filter by field (key=val)")]
    pub fields: Vec<String>,

    #[arg(
        long,
        help = "Page limit (1-100)",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    pub limit: Option<u32>,

    #[arg(long, help = "Pagination cursor")]
    pub cursor: Option<String>,

    #[arg(long, help = "Page through all executions")]
    pub all: bool,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,

    #[arg(long, help = "Stream JSON objects one per line")]
    pub jsonl: bool,
}

#[derive(Args, Debug)]
pub struct RunWatchArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(long, help = "Resume after sequence number")]
    pub after: Option<u64>,

    #[arg(long, help = "Output raw event data as JSON")]
    pub json: bool,

    #[arg(long, help = "Stream JSON objects one per line")]
    pub jsonl: bool,
}

#[derive(Args, Debug)]
pub struct RunSignalArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(help = "Signal name")]
    pub name: String,

    #[arg(long = "payload", help = "Payload parameter (key=val)")]
    pub payload: Vec<String>,

    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RunReportArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(
        long,
        value_enum,
        help = "Export format: md (Markdown text), json, junit (JUnit XML) or pdf"
    )]
    pub format: Option<ReportFormat>,

    #[arg(
        long,
        value_name = "FILE",
        requires = "format",
        help = "Write the report to FILE instead of stdout (a PDF needs this unless stdout is redirected)"
    )]
    pub out: Option<std::path::PathBuf>,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RunControlArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RunUpdateArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(long, help = "Execution title")]
    pub title: Option<String>,

    #[arg(long = "tag", help = "Execution tag")]
    pub tags: Vec<String>,

    #[arg(long = "field", help = "Field parameter (key=val)")]
    pub fields: Vec<String>,

    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
#[command(disable_version_flag = true)]
pub struct RunRerunArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(long, help = "Runbook version (<n|rbv_id>)")]
    pub version: Option<String>,

    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RunFieldsArgs {
    #[command(subcommand)]
    pub command: RunFieldsCommands,
}

#[derive(Subcommand, Debug)]
pub enum RunFieldsCommands {
    #[command(name = "set", about = "Set execution fields")]
    Set(RunFieldsSetArgs),
}

#[derive(Args, Debug)]
pub struct RunFieldsSetArgs {
    #[arg(help = "Execution ID")]
    pub execution_id: String,

    #[arg(required = true, help = "Field parameter (key=val)")]
    pub fields: Vec<String>,

    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,

    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

fn generate_random_hex_32() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return Err("cannot generate idempotency key; pass --idempotency-key".to_string());
    }
    let mut hex = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{b:02x}");
    }
    Ok(hex)
}

fn handle_spec_validate(
    args: SpecValidateArgs,
    store: Option<&runbook_cli::credstore::StateDir>,
) -> Result<i32, String> {
    if let Some(store) = store {
        store.refuse_inside(&args.file)?;
    }
    let content = fs::read_to_string(&args.file)
        .map_err(|e| format!("failed to read file '{}': {e}", args.file.display()))?;

    let report = validate_source(&content);

    if args.json {
        let json_str = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("failed to serialize report to JSON: {e}"))?;
        println!("{json_str}");
    } else if report.ok {
        let budget = report.worst_case_step_runs.unwrap_or(0);
        println!("ok (worst case {budget} step runs)");
    } else {
        for d in &report.diagnostics {
            println!("{}: {}: {}", d.path, d.code, d.message);
        }
    }

    if report.ok { Ok(0) } else { Ok(2) }
}

fn handle_run_start(client: &ApiClient, args: RunStartArgs) -> Result<i32, String> {
    validate_project_ref(&args.project)?;
    let (runbook_name, version) = parse_runbook_ref(&args.runbook)?;
    let inputs = parse_key_value_pairs(&args.inputs)?;
    let fields = parse_key_value_pairs(&args.fields)?;

    let mut body_map = Map::new();
    body_map.insert("runbook".to_string(), Value::String(runbook_name));
    if let Some(ver) = version {
        body_map.insert("version".to_string(), ver);
    }
    body_map.insert("inputs".to_string(), Value::Object(inputs));
    body_map.insert("fields".to_string(), Value::Object(fields));
    if let Some(title) = args.title {
        body_map.insert("title".to_string(), Value::String(title));
    }
    body_map.insert(
        "tags".to_string(),
        Value::Array(args.tags.into_iter().map(Value::String).collect()),
    );

    let idempotency_key = match args.idempotency_key {
        Some(k) => k,
        None => {
            let key = generate_random_hex_32()?;
            eprintln!("Idempotency-Key: {key}");
            key
        }
    };

    let path = format!("/v1/projects/{}/executions", args.project);
    let resp = match client.post_json(&path, &idempotency_key, &Value::Object(body_map)) {
        Ok(val) => val,
        Err(e) => return Ok(runbook_cli::cliutil::api_failed_json(e, args.json)),
    };

    let follow_up = args.wait || args.follow;
    if args.json && !follow_up {
        let json_str = serde_json::to_string_pretty(&resp)
            .map_err(|e| format!("failed to format JSON response: {e}"))?;
        println!("{json_str}");
    } else {
        let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let status = resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        // With --json the follow-up output is the only stdout document.
        if args.json {
            eprintln!("started {id} ({status})");
        } else {
            println!("started {id} ({status})");
        }
    }

    if follow_up {
        let execution_id = resp
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        return if args.follow {
            handle_run_watch(
                client,
                RunWatchArgs {
                    execution_id,
                    after: None,
                    json: args.json,
                    jsonl: false,
                },
            )
        } else {
            handle_run_get(
                client,
                RunGetArgs {
                    execution_id,
                    wait: true,
                    interval_ms: args.interval_ms,
                    fail_on: Vec::new(),
                    json: args.json,
                },
            )
        };
    }

    Ok(0)
}

fn handle_run_get(client: &ApiClient, args: RunGetArgs) -> Result<i32, String> {
    let exe_id = args
        .execution_id
        .parse::<runbook_model::ExecutionId>()
        .map_err(|_| format!("invalid execution ID: '{}'", args.execution_id))?;
    let path = format!("/v1/executions/{exe_id}");
    let wait = args.wait || !args.fail_on.is_empty();
    let mut failures = 0;
    let mut resp = loop {
        let resp = match client.get_json(&path) {
            Ok(val) => {
                failures = 0;
                val
            }
            // While waiting, a transient failure is not the run's outcome:
            // retry with capped backoff (F63). A real error stops at once.
            Err(e) if wait && transient(&e) && failures < MAX_TRANSIENT_RETRIES => {
                failures += 1;
                let delay = backoff(args.interval_ms, failures);
                eprintln!("warning: {e}; retrying in {} ms", delay.as_millis());
                std::thread::sleep(delay);
                continue;
            }
            Err(e) => return Ok(runbook_cli::cliutil::api_failed_json(e, args.json)),
        };
        if !wait || terminal_status(&resp).is_some() {
            break resp;
        }
        std::thread::sleep(std::time::Duration::from_millis(args.interval_ms));
    };

    if args.json {
        // The same next_action hint as get_execution (resume brief).
        if resp.get("next_action").is_none()
            && let Some(next) = resp
                .get("resume_brief")
                .and_then(|b| b.get("next_action"))
                .cloned()
        {
            resp["next_action"] = next;
        }
        let json_str = serde_json::to_string_pretty(&resp)
            .map_err(|e| format!("failed to format JSON response: {e}"))?;
        println!("{json_str}");
    } else {
        let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let title = resp
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");
        let status = resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        println!("{id} {title} ({status})");
        for line in runbook_cli::cliutil::call_links(&resp) {
            println!("  {line}");
        }
    }

    Ok(if wait {
        if let Some(status) = terminal_status(&resp)
            && !args.fail_on.is_empty()
            && args.fail_on.iter().any(|s| s == status)
        {
            return Ok(1);
        }
        execution_exit_code(terminal_status(&resp))
    } else {
        0
    })
}

/// Consecutive transient failures `run get --wait`, `run start --wait` and
/// `run watch` ride out before giving up.
const MAX_TRANSIENT_RETRIES: u32 = 8;
const MAX_BACKOFF_MS: u64 = 30_000;

/// Transport errors, 5xx and 429 may pass; anything else is the answer.
fn transient(e: &ApiError) -> bool {
    match e {
        ApiError::Transport(_) => true,
        ApiError::Problem { status, .. } => *status == 429 || *status >= 500,
    }
}

/// `base` doubled per consecutive failure, capped at 30 s.
fn backoff(base_ms: u64, failures: u32) -> std::time::Duration {
    let ms = base_ms
        .max(1)
        .saturating_mul(1u64 << failures.min(16))
        .min(MAX_BACKOFF_MS);
    std::time::Duration::from_millis(ms)
}

/// Terminal execution status of an execution view or brief, if any.
fn terminal_status(view: &Value) -> Option<&str> {
    view.get("status")
        .and_then(Value::as_str)
        .filter(|s| matches!(*s, "succeeded" | "failed" | "cancelled"))
}

/// Exit 3 when the execution failed or was cancelled. A succeeded run exits
/// 0 whatever its result (a successful rollback is still success).
fn execution_exit_code(status: Option<&str>) -> i32 {
    match status {
        Some("failed" | "cancelled") => 3,
        _ => 0,
    }
}

fn handle_run_list(client: &ApiClient, args: RunListArgs) -> Result<i32, String> {
    validate_project_ref(&args.project)?;

    let mut query_params: Vec<(&str, String)> = Vec::new();

    let status_str = args.status.join(",");
    if !status_str.is_empty() {
        query_params.push(("status", status_str));
    }

    let tags_str = args.tags.join(",");
    if !tags_str.is_empty() {
        query_params.push(("tags", tags_str));
    }

    if let Some(rb) = args.runbook {
        query_params.push(("runbook", rb));
    }

    let mut field_params = Vec::new();
    for field in &args.fields {
        let (k, v) = field
            .split_once('=')
            .ok_or_else(|| format!("invalid key=value pair: '{field}'"))?;
        let parsed_val = match serde_json::from_str::<Value>(v) {
            Ok(json_val) => json_val,
            Err(_) => Value::String(v.to_string()),
        };
        field_params.push((format!("field.{k}"), parsed_val.to_string()));
    }

    let lim_str;
    if let Some(lim) = args.limit {
        lim_str = lim.to_string();
        query_params.push(("limit", lim_str));
    }

    let path = format!("/v1/projects/{}/executions", args.project);
    let mut current_cursor = args.cursor.clone();
    let mut all_items = Vec::new();

    loop {
        let mut all_params: Vec<(&str, &str)> = Vec::new();
        for (k, v) in &query_params {
            all_params.push((k, v.as_str()));
        }
        for (k, v) in &field_params {
            all_params.push((k.as_str(), v.as_str()));
        }
        if let Some(cur) = &current_cursor {
            all_params.push(("cursor", cur.as_str()));
        }

        let resp = match client.get_json_query(&path, &all_params) {
            Ok(val) => val,
            Err(e) => {
                return Ok(runbook_cli::cliutil::api_failed_json(
                    e,
                    args.json || args.jsonl,
                ));
            }
        };

        if let Some(items) = resp.get("items").and_then(|v| v.as_array()) {
            if args.jsonl {
                for item in items {
                    println!(
                        "{}",
                        serde_json::to_string(item).map_err(|e| e.to_string())?
                    );
                }
            } else if args.all && args.json {
                all_items.extend(items.clone());
            } else if !args.json {
                for item in items {
                    let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
                    let status = item
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let title = item
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Untitled");
                    println!("{id}  {status}  {title}");
                }
            }
        }

        if !args.all {
            if args.json {
                let json_str = serde_json::to_string_pretty(&resp)
                    .map_err(|e| format!("failed to format JSON response: {e}"))?;
                println!("{json_str}");
            } else if !args.jsonl
                && let Some(next_cursor) = resp
                    .get("next_cursor")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            {
                println!("next: {next_cursor}");
            }
            break;
        }

        match resp.get("next_cursor").and_then(|v| v.as_str()) {
            Some(next) if !next.is_empty() => {
                current_cursor = Some(next.to_string());
            }
            _ => {
                if args.all && args.json && !args.jsonl {
                    let combined = serde_json::json!({
                        "items": all_items,
                        "next_cursor": serde_json::Value::Null,
                    });
                    let json_str = serde_json::to_string_pretty(&combined)
                        .map_err(|e| format!("failed to format JSON response: {e}"))?;
                    println!("{json_str}");
                }
                break;
            }
        }
    }

    Ok(0)
}

fn handle_run_watch(client: &ApiClient, args: RunWatchArgs) -> Result<i32, String> {
    let exe_id = args
        .execution_id
        .parse::<runbook_model::ExecutionId>()
        .map_err(|_| format!("invalid execution ID: '{}'", args.execution_id))?;

    let path = format!("/v1/executions/{exe_id}/events");
    let mut last_seen_id: Option<String> = args.after.map(|n| n.to_string());
    // Consecutive reconnects that delivered nothing; any event resets it, so
    // a long stream survives any number of dropped connections (F64).
    let mut failures = 0;

    loop {
        let mut stream = match client.get_sse_stream(&path, last_seen_id.as_deref()) {
            Ok(s) => s,
            Err(e) if transient(&e) && failures < MAX_TRANSIENT_RETRIES => {
                failures += 1;
                std::thread::sleep(backoff(250, failures));
                continue;
            }
            Err(e @ ApiError::Problem { .. }) => {
                eprintln!("{}", e.code().unwrap_or("error"));
                return Ok(e.exit_code());
            }
            Err(ApiError::Transport(err)) => {
                eprintln!("error: {err}");
                return Ok(1);
            }
        };

        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let mut completed = false;
        let mut final_status: Option<String> = None;
        let mut error_exit = false;

        loop {
            let n = match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            buf.extend_from_slice(&chunk[..n]);

            while let Some(frame) = extract_sse_frame(&mut buf) {
                failures = 0;
                if let Some(id) = &frame.id {
                    last_seen_id = Some(id.clone());
                }

                if frame.event.as_deref() == Some("error") {
                    let code = match serde_json::from_str::<Value>(&frame.data) {
                        Ok(val) => val
                            .get("code")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or(frame.data),
                        Err(_) => frame.data,
                    };
                    eprintln!("{code}");
                    error_exit = true;
                    break;
                }

                if args.json || args.jsonl {
                    if let Ok(val) = serde_json::from_str::<Value>(&frame.data) {
                        println!("{}", serde_json::to_string(&val).unwrap());
                    } else {
                        println!("{}", frame.data);
                    }
                } else {
                    let seq = frame
                        .id
                        .as_deref()
                        .or(last_seen_id.as_deref())
                        .unwrap_or("");
                    let ev = frame.event.as_deref().unwrap_or("message");
                    println!("{seq} {ev}");
                }

                if frame.event.as_deref() == Some("ExecutionCompleted") {
                    final_status = serde_json::from_str::<Value>(&frame.data)
                        .ok()
                        .and_then(|v| v["event"]["status"].as_str().map(str::to_owned));
                    completed = true;
                    break;
                }
            }

            if completed || error_exit {
                break;
            }
        }

        if completed {
            return Ok(execution_exit_code(final_status.as_deref()));
        }
        if error_exit {
            return Ok(1);
        }

        if failures < MAX_TRANSIENT_RETRIES {
            failures += 1;
            // The first reconnect after a delivering connection is immediate.
            if failures > 1 {
                std::thread::sleep(backoff(250, failures - 1));
            }
            continue;
        }
        eprintln!("error: connection dropped");
        return Ok(1);
    }
}

fn handle_run_signal(client: &ApiClient, args: RunSignalArgs) -> Result<i32, String> {
    let exe_id = args
        .execution_id
        .parse::<runbook_model::ExecutionId>()
        .map_err(|_| format!("invalid execution ID: '{}'", args.execution_id))?;
    validate_signal_name(&args.name)?;

    let payload_map = parse_key_value_pairs(&args.payload)?;
    let mut body_map = Map::new();
    body_map.insert("payload".to_string(), Value::Object(payload_map));

    let idempotency_key = match args.idempotency_key {
        Some(k) => k,
        None => {
            let key = generate_random_hex_32()?;
            eprintln!("Idempotency-Key: {key}");
            key
        }
    };

    let path = format!("/v1/executions/{exe_id}/signals/{}", args.name);
    let resp = match client.post_json(&path, &idempotency_key, &Value::Object(body_map)) {
        Ok(val) => val,
        Err(e) => return Ok(runbook_cli::cliutil::api_failed_json(e, args.json)),
    };

    if args.json {
        let json_str = serde_json::to_string_pretty(&resp)
            .map_err(|e| format!("failed to format JSON response: {e}"))?;
        println!("{json_str}");
    } else {
        println!("signal {} sent to {}", args.name, exe_id);
    }

    Ok(0)
}

fn handle_run_report(client: &ApiClient, args: RunReportArgs) -> Result<i32, String> {
    let exe_id = args
        .execution_id
        .parse::<runbook_model::ExecutionId>()
        .map_err(|_| format!("invalid execution ID: '{}'", args.execution_id))?;

    if let Some(format) = args.format {
        return Ok(reports::print_report(
            client,
            &exe_id.to_string(),
            format,
            args.out.as_deref(),
        ));
    }

    let path = format!("/v1/executions/{exe_id}/report");
    let resp = match client.get_json(&path) {
        Ok(val) => val,
        Err(e) => return Ok(runbook_cli::cliutil::api_failed_json(e, args.json)),
    };

    if args.json {
        let json_str = serde_json::to_string_pretty(&resp)
            .map_err(|e| format!("failed to format JSON response: {e}"))?;
        println!("{json_str}");
    } else {
        let header = resp.get("header");
        let title = header
            .and_then(|h| h.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");
        let status = header
            .and_then(|h| h.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        println!("{title} ({status})");

        if let Some(sections) = resp.get("sections").and_then(|v| v.as_array()) {
            for section in sections {
                let path = section.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let status = section
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let title = section
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Untitled");
                let verdict = section
                    .get("verdict")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());

                if let Some(v) = verdict {
                    println!("  {path}  {status}  {title}  [{v}]");
                } else {
                    println!("  {path}  {status}  {title}");
                }
            }
        }
    }

    Ok(0)
}

fn handle_run_control(
    client: &ApiClient,
    args: RunControlArgs,
    action: &'static str,
) -> Result<i32, String> {
    let exe_id = args
        .execution_id
        .parse::<runbook_model::ExecutionId>()
        .map_err(|_| format!("invalid execution ID: '{}'", args.execution_id))?;

    let idempotency_key = match args.idempotency_key {
        Some(k) => k,
        None => {
            let key = generate_random_hex_32()?;
            eprintln!("Idempotency-Key: {key}");
            key
        }
    };

    let mut body = Map::new();
    body.insert("action".to_string(), Value::String(action.to_string()));

    let path = format!("/v1/executions/{exe_id}");
    let resp = match client.patch_json(&path, &idempotency_key, &Value::Object(body)) {
        Ok(val) => val,
        Err(e) => return Ok(runbook_cli::cliutil::api_failed_json(e, args.json)),
    };

    if args.json {
        let json_str = serde_json::to_string_pretty(&resp)
            .map_err(|e| format!("failed to format JSON response: {e}"))?;
        println!("{json_str}");
    } else {
        let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("");
        println!("{id} {status}");
    }

    Ok(0)
}

fn handle_run_update(client: &ApiClient, args: RunUpdateArgs) -> Result<i32, String> {
    let exe_id = args
        .execution_id
        .parse::<runbook_model::ExecutionId>()
        .map_err(|_| format!("invalid execution ID: '{}'", args.execution_id))?;

    if args.title.is_none() && args.tags.is_empty() && args.fields.is_empty() {
        return Err("nothing to update".to_string());
    }

    let mut body = Map::new();
    if let Some(title) = args.title {
        body.insert("title".to_string(), Value::String(title));
    }
    if !args.tags.is_empty() {
        body.insert(
            "tags".to_string(),
            Value::Array(args.tags.into_iter().map(Value::String).collect()),
        );
    }
    if !args.fields.is_empty() {
        let field_map = parse_key_value_pairs(&args.fields)?;
        body.insert("fields".to_string(), Value::Object(field_map));
    }

    let idempotency_key = match args.idempotency_key {
        Some(k) => k,
        None => {
            let key = generate_random_hex_32()?;
            eprintln!("Idempotency-Key: {key}");
            key
        }
    };

    let path = format!("/v1/executions/{exe_id}");
    let resp = match client.patch_json(&path, &idempotency_key, &Value::Object(body)) {
        Ok(val) => val,
        Err(e) => return Ok(runbook_cli::cliutil::api_failed_json(e, args.json)),
    };

    if args.json {
        let json_str = serde_json::to_string_pretty(&resp)
            .map_err(|e| format!("failed to format JSON response: {e}"))?;
        println!("{json_str}");
    } else {
        let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("");
        println!("{id} {status}");
    }

    Ok(0)
}

fn handle_run_rerun(client: &ApiClient, args: RunRerunArgs) -> Result<i32, String> {
    let exe_id = args
        .execution_id
        .parse::<runbook_model::ExecutionId>()
        .map_err(|_| format!("invalid execution ID: '{}'", args.execution_id))?;

    let mut body = Map::new();
    if let Some(ver_str) = &args.version {
        let ver_val = parse_version_selector(ver_str)?;
        body.insert("version".to_string(), ver_val);
    }

    let idempotency_key = match args.idempotency_key {
        Some(k) => k,
        None => {
            let key = generate_random_hex_32()?;
            eprintln!("Idempotency-Key: {key}");
            key
        }
    };

    let path = format!("/v1/executions/{exe_id}/rerun");
    let resp = match client.post_json(&path, &idempotency_key, &Value::Object(body)) {
        Ok(val) => val,
        Err(e) => return Ok(runbook_cli::cliutil::api_failed_json(e, args.json)),
    };

    if args.json {
        let json_str = serde_json::to_string_pretty(&resp)
            .map_err(|e| format!("failed to format JSON response: {e}"))?;
        println!("{json_str}");
    } else {
        let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let status = resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        println!("started {id} ({status}) rerun of {exe_id}");
    }

    Ok(0)
}

/// The current (or `--context`) context's default project, as
/// RUNBOOK_PROJECT for the `--project` arguments, unless the project is set
/// already or another server is named (`--host`, RUNBOOK_HOST).
fn context_project_env() {
    if env::var_os("RUNBOOK_PROJECT").is_some() || env::var_os("RUNBOOK_HOST").is_some() {
        return;
    }
    let args: Vec<String> = env::args().collect();
    if args
        .iter()
        .any(|a| a == "--host" || a.starts_with("--host="))
    {
        return;
    }
    let named = args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--context=").map(str::to_owned).or_else(|| {
            (a == "--context")
                .then(|| args.get(i + 1).cloned())
                .flatten()
        })
    });
    let xdg = env::var("XDG_CONFIG_HOME").ok();
    let home = env::var("HOME").ok();
    let Some(config) =
        config_path(xdg.as_deref(), home.as_deref()).and_then(|p| Config::load(&p).ok())
    else {
        return;
    };
    let Some(name) = named.or(config.current_context.clone()) else {
        return;
    };
    if let Some(project) = config.contexts.get(&name).and_then(|c| c.project.clone()) {
        // SAFETY: called first in `run`, before any thread starts.
        unsafe { env::set_var("RUNBOOK_PROJECT", project) };
    }
}

fn run() -> i32 {
    context_project_env();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if err.kind() == clap::error::ErrorKind::DisplayHelp
                || err.kind() == clap::error::ErrorKind::DisplayVersion
            {
                print!("{err}");
                return 0;
            }
            eprintln!("{err}");
            return 2;
        }
    };

    let xdg = env::var("XDG_CONFIG_HOME").ok();
    let home = env::var("HOME").ok();
    let env_host = env::var("RUNBOOK_HOST").ok();
    let env_token = env::var("RUNBOOK_TOKEN").ok();
    let host_flag = cli.host.clone();
    let context_flag = cli.context.clone();
    let cli_env = CliEnv {
        host_flag: host_flag.clone(),
        context_flag: context_flag.clone(),
        xdg: xdg.clone(),
        home: home.clone(),
        env_host: env_host.clone(),
        env_token: env_token.clone(),
    };

    if !should_skip_version_check(&cli.command) {
        let effective_host_flag = match &cli.command {
            Commands::Login(args) => args.host.as_deref().or(host_flag.as_deref()),
            _ => host_flag.as_deref(),
        };
        if let Some(env) = agent_env(
            effective_host_flag,
            context_flag.as_deref(),
            xdg.clone(),
            home.clone(),
            env_host.clone(),
            env_token.clone(),
        )
        .ok()
        .filter(|env| !has_refused_file(&cli.command, env.store.as_ref()))
        {
            runbook_cli::cliutil::check_minimum_cli_version(&env.host, env.store.as_ref());
        }
    }

    match cli.command {
        Commands::Spec(SpecCommands::Validate(args)) => match handle_spec_validate(
            args,
            runbook_cli::credstore::StateDir::from_env(xdg.as_deref(), home.as_deref()).as_ref(),
        ) {
            Ok(code) => code,
            Err(msg) => {
                eprintln!("error: {msg}");
                2
            }
        },
        Commands::Context(context_cmd) => {
            let config_file = match config_path(xdg.as_deref(), home.as_deref()) {
                Some(p) => p,
                None => {
                    eprintln!("error: neither $XDG_CONFIG_HOME nor $HOME is set");
                    return 1;
                }
            };

            let mut config = match Config::load(&config_file) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };

            let store = StateDir::from_env(xdg.as_deref(), home.as_deref());
            let forget = |name: &str| -> Result<(), String> {
                match &store {
                    Some(store) => runbook_cli::logins::remove_context(store, name).map(|_| ()),
                    None => Ok(()),
                }
            };
            let result: Result<(), String> = (|| match context_cmd {
                ContextCommands::Add(args) => {
                    let before = config.contexts.get(&args.name).map(|c| c.normalized_host());
                    config
                        .add_context(&args.name, &args.host)
                        .map_err(|e| e.to_string())?;
                    config
                        .set_project(&args.name, args.default_project.as_deref())
                        .map_err(|e| e.to_string())?;
                    config.save(&config_file).map_err(|e| e.to_string())?;
                    // A context moved to another host loses its login: it
                    // was for the old host.
                    if before.is_some_and(|h| h != config.contexts[&args.name].normalized_host()) {
                        forget(&args.name)?;
                    }
                    println!("Context '{}' added.", args.name);
                    Ok(())
                }
                ContextCommands::Use(args) => {
                    config.use_context(&args.name).map_err(|e| e.to_string())?;
                    config.save(&config_file).map_err(|e| e.to_string())?;
                    println!("Switched to context '{}'.", args.name);
                    Ok(())
                }
                ContextCommands::Remove(args) => {
                    if config.contexts.remove(&args.name).is_none() {
                        return Err(ConfigError::UnknownContext(args.name.clone()).to_string());
                    }
                    if config.current_context.as_deref() == Some(args.name.as_str()) {
                        config.current_context = None;
                    }
                    config.save(&config_file).map_err(|e| e.to_string())?;
                    forget(&args.name)?;
                    println!("Context '{}' removed.", args.name);
                    Ok(())
                }
                ContextCommands::List(args) => {
                    let mut rows = vec![];
                    for (name, current) in config.list() {
                        let host = config.contexts[&name].normalized_host();
                        let key = runbook_cli::logins::LoginKey {
                            host: host.clone(),
                            context: Some(name.clone()),
                        };
                        let logged_in = store
                            .as_ref()
                            .is_some_and(|s| runbook_cli::logins::is_logged_in(s, &key));
                        let mut row = serde_json::json!({
                            "name": name, "host": host, "current": current, "logged_in": logged_in,
                        });
                        if let Some(project) = &config.contexts[&name].project {
                            row["project"] = Value::from(project.as_str());
                        }
                        rows.push(row);
                    }
                    if args.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&Value::Array(rows)).unwrap_or_default()
                        );
                    } else {
                        for row in rows {
                            println!(
                                "{} {}  {}{}{}",
                                if row["current"] == true { "*" } else { " " },
                                row["name"].as_str().unwrap_or(""),
                                row["host"].as_str().unwrap_or(""),
                                row["project"]
                                    .as_str()
                                    .map(|p| format!("  project {p}"))
                                    .unwrap_or_default(),
                                if row["logged_in"] == true {
                                    "  (logged in)"
                                } else {
                                    ""
                                }
                            );
                        }
                    }
                    Ok(())
                }
            })();

            if let Err(e) = result {
                eprintln!("error: {e}");
                return 1;
            }
            0
        }
        Commands::Step(step_cmd) => match agent_env(
            host_flag.as_deref(),
            context_flag.as_deref(),
            xdg,
            home,
            env_host,
            env_token,
        ) {
            Ok(env) => agent::handle_step(&env, step_cmd),
            Err(code) => code,
        },
        Commands::Whoami(args) => match agent_env(
            host_flag.as_deref(),
            context_flag.as_deref(),
            xdg,
            home,
            env_host,
            env_token,
        ) {
            Ok(env) => agent::handle_whoami(&auth::with_login(env), args),
            Err(code) => code,
        },
        Commands::Me(cmd) => with_env(&cli_env, |env| runbook_cli::me::handle_me(env, cmd)),
        Commands::User(cmd) => with_env(&cli_env, |env| admin::handle_user(env, cmd)),
        Commands::Project(cmd) => with_env(&cli_env, |env| admin::handle_project(env, cmd)),
        Commands::Team(cmd) => with_env(&cli_env, |env| admin::handle_team(env, cmd)),
        Commands::Token(cmd) => with_env(&cli_env, |env| tokens::handle_token(env, cmd)),
        Commands::Apps(cmd) => with_env(&cli_env, |env| apps::handle_apps(env, cmd)),
        Commands::Webhook(cmd) => with_env(&cli_env, |env| webhooks::handle_webhook(env, cmd)),
        // Memory resolves its own credentials: a save prefers the stored
        // execution pair over the login (`memory.rs`).
        Commands::Memory(cmd) => match agent_env(
            host_flag.as_deref(),
            context_flag.as_deref(),
            xdg.clone(),
            home.clone(),
            env_host.clone(),
            env_token.clone(),
        ) {
            Ok(env) => memory::handle_memory(&env, cmd),
            Err(code) => code,
        },
        Commands::Archive(cmd) => with_env(&cli_env, |env| archive::handle_archive(env, cmd)),
        Commands::Login(args) => {
            let mut host_override = args.host.clone().or(host_flag.clone());
            let mut context_override = context_flag.clone();
            // A first login (no server named, no context yet) asks for the
            // server, and remembers it as the current context so later
            // commands need no --host (there is no built-in server).
            // `--host` and RUNBOOK_HOST are one-off choices and save nothing.
            if !args.migrate_to_keychain
                && context_flag.is_none()
                && env_host.is_none()
                && host_flag.is_none()
            {
                match first_login_context(args.host.as_deref(), xdg.as_deref(), home.as_deref()) {
                    Ok(Some((name, host))) => {
                        host_override = Some(host);
                        context_override = Some(name);
                    }
                    Ok(None) => {}
                    Err(code) => return code,
                }
            }
            // Moving stored logins into the keychain touches every host's
            // login and calls no server: any host will do when none is set.
            if args.migrate_to_keychain
                && host_override.is_none()
                && env_host.is_none()
                && context_override.is_none()
                && config_path(xdg.as_deref(), home.as_deref())
                    .and_then(|p| Config::load(&p).ok())
                    .is_none_or(|c| c.current_context.is_none())
            {
                host_override = Some("http://localhost".into());
            }
            match agent_env(
                host_override.as_deref(),
                context_override.as_deref(),
                xdg,
                home,
                env_host,
                env_token,
            ) {
                Ok(env) => auth::handle_login(&env, args),
                Err(code) => code,
            }
        }
        Commands::Logout(args) => match agent_env(
            host_flag.as_deref(),
            context_flag.as_deref(),
            xdg,
            home,
            env_host,
            env_token,
        ) {
            Ok(env) => auth::handle_logout(&env, args),
            Err(code) => code,
        },
        Commands::Authoring(command) => match agent_env(
            host_flag.as_deref(),
            context_flag.as_deref(),
            xdg,
            home,
            env_host,
            env_token,
        ) {
            Ok(env) => runbook_cli::authoring::handle(auth::with_login(env), command),
            Err(code) => code,
        },
        Commands::Skill(cmd) => with_env(&cli_env, |env| runbook_cli::skill::handle(env, cmd)),
        Commands::Sync(cmd) => with_env(&cli_env, |env| runbook_cli::sync::handle(env, cmd)),
        Commands::Version(args) => match agent_env(
            host_flag.as_deref(),
            context_flag.as_deref(),
            xdg,
            home,
            env_host,
            None,
        ) {
            Ok(env) => runbook_cli::version::handle(&env.host, &args),
            Err(code) => code,
        },
        Commands::Mcp(command) => match agent_env(
            host_flag.as_deref(),
            context_flag.as_deref(),
            xdg,
            home,
            env_host,
            env_token,
        ) {
            Ok(env) => runbook_cli::mcp::handle(env, command),
            Err(code) => code,
        },
        Commands::Run(RunCommands::Token(args)) => {
            match agent_env(
                host_flag.as_deref(),
                context_flag.as_deref(),
                xdg,
                home,
                env_host,
                env_token,
            ) {
                Ok(env) => match auth::with_login(env) {
                    env if env.token.is_some() => agent::handle_run_token(&env, args),
                    _ => {
                        eprintln!("error: set RUNBOOK_TOKEN or run `runbook login`");
                        2
                    }
                },
                Err(code) => code,
            }
        }
        Commands::Run(run_cmd) => {
            let env = match agent_env(
                host_flag.as_deref(),
                context_flag.as_deref(),
                xdg,
                home,
                env_host,
                env_token,
            ) {
                Ok(env) => auth::with_login(env),
                Err(code) => return code,
            };
            let Some(token) = env.token.clone() else {
                eprintln!("error: set RUNBOOK_TOKEN or run `runbook login`");
                return 2;
            };
            let client = ApiClient::new(env.host, token);

            match run_cmd {
                RunCommands::Start(args) => match handle_run_start(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Get(args) => match handle_run_get(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::List(args) => match handle_run_list(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Watch(args) => match handle_run_watch(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Signal(args) => match handle_run_signal(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Report(args) => match handle_run_report(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Cancel(args) => match handle_run_control(&client, args, "cancel") {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Pause(args) => match handle_run_control(&client, args, "pause") {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Resume(args) => match handle_run_control(&client, args, "resume") {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Update(args) => match handle_run_update(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Rerun(args) => match handle_run_rerun(&client, args) {
                    Ok(code) => code,
                    Err(msg) => {
                        eprintln!("error: {msg}");
                        2
                    }
                },
                RunCommands::Token(_) => unreachable!("handled above"),
                RunCommands::Fields(args) => match args.command {
                    RunFieldsCommands::Set(set_args) => {
                        let update_args = RunUpdateArgs {
                            execution_id: set_args.execution_id,
                            title: None,
                            tags: Vec::new(),
                            fields: set_args.fields,
                            idempotency_key: set_args.idempotency_key,
                            json: set_args.json,
                        };
                        match handle_run_update(&client, update_args) {
                            Ok(code) => code,
                            Err(msg) => {
                                eprintln!("error: {msg}");
                                2
                            }
                        }
                    }
                },
            }
        }
    }
}

/// Global flags and environment, for commands dispatched to modules.
struct CliEnv {
    host_flag: Option<String>,
    context_flag: Option<String>,
    xdg: Option<String>,
    home: Option<String>,
    env_host: Option<String>,
    env_token: Option<String>,
}

/// Resolve the host and credentials (`RUNBOOK_TOKEN`, then the stored
/// login) for a user-level command.
fn with_env(c: &CliEnv, f: impl FnOnce(&Env) -> i32) -> i32 {
    match agent_env(
        c.host_flag.as_deref(),
        c.context_flag.as_deref(),
        c.xdg.clone(),
        c.home.clone(),
        c.env_host.clone(),
        c.env_token.clone(),
    ) {
        Ok(env) => f(&auth::with_login(env)),
        Err(code) => code,
    }
}

/// On a machine with no current context: the server to log in to (the
/// named one, else asked for on a terminal), saved as a new current context
/// named after its host. `None` when a current context already decides.
fn first_login_context(
    named: Option<&str>,
    xdg: Option<&str>,
    home: Option<&str>,
) -> Result<Option<(String, String)>, i32> {
    let Some(path) = config_path(xdg, home) else {
        return Ok(None);
    };
    let mut config = Config::load(&path).map_err(|e| {
        eprintln!("error: {e}");
        2
    })?;
    if config.current_context.is_some() {
        return Ok(None);
    }
    let host = match named {
        Some(host) => host.to_owned(),
        None => {
            use std::io::{BufRead, IsTerminal, Write};
            if !std::io::stdin().is_terminal() {
                eprintln!("error: {}", ConfigError::NoHost);
                return Err(1);
            }
            eprint!("Server URL (for example https://runbook.example.com): ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|_| 1)?;
            line.trim().to_owned()
        }
    };
    let host = runbook_cli::config::normalize_and_validate_host(&host).map_err(|e| {
        eprintln!("error: {e}");
        2
    })?;
    let mut name = runbook_cli::config::context_name_for(&host);
    if config
        .contexts
        .get(&name)
        .is_some_and(|c| c.normalized_host() != host)
    {
        name = "default".into();
    }
    config.add_context(&name, &host).map_err(|e| {
        eprintln!("error: {e}");
        2
    })?;
    config.use_context(&name).map_err(|e| {
        eprintln!("error: {e}");
        2
    })?;
    config.save(&path).map_err(|e| {
        eprintln!("error: {e}");
        2
    })?;
    eprintln!("Saved {host} as context '{name}' (now current).");
    Ok(Some((name, host)))
}

/// Host, optional `RUNBOOK_TOKEN` and the credential store for agent commands.
fn agent_env(
    host_flag: Option<&str>,
    context_flag: Option<&str>,
    xdg: Option<String>,
    home: Option<String>,
    env_host: Option<String>,
    env_token: Option<String>,
) -> Result<Env, i32> {
    let config = match config_path(xdg.as_deref(), home.as_deref()) {
        Some(p) => Config::load(&p).map_err(|e| {
            eprintln!("error: {e}");
            2
        })?,
        None => Config::default(),
    };
    let resolved =
        resolve_host(host_flag, env_host.as_deref(), context_flag, &config).map_err(|e| {
            eprintln!("error: {e}");
            1
        })?;
    // An explicit --context must exist, and must agree with an explicit
    // --host or RUNBOOK_HOST (CLI3 review): otherwise the identity the user
    // named would silently not apply.
    if let Some(name) = context_flag {
        let Some(named) = config.contexts.get(name) else {
            eprintln!("error: {}", ConfigError::UnknownContext(name.to_owned()));
            return Err(1);
        };
        let context_host = named.normalized_host();
        if context_host != resolved.host {
            let source = if host_flag.is_some() {
                "--host"
            } else {
                "RUNBOOK_HOST"
            };
            eprintln!(
                "error: context '{name}' is for {context_host}, but {source} names {}; drop one of them",
                resolved.host
            );
            return Err(1);
        }
    }
    // CLI3: the named (else current) context's identity applies whenever
    // the resolved host is that context's host (both normalized: re-check
    // N10).
    let context = context_flag
        .map(str::to_owned)
        .or_else(|| config.current_context.clone())
        .filter(|name| {
            config
                .contexts
                .get(name)
                .is_some_and(|c| c.normalized_host() == resolved.host)
        });
    Ok(Env {
        host: resolved.host,
        context,
        token: env_token.filter(|t| !t.trim().is_empty()),
        store: StateDir::from_env(xdg.as_deref(), home.as_deref()),
    })
}

#[cfg(test)]
mod mcp_parity_tests;

fn main() {
    let exit_code = run();
    process::exit(exit_code);
}
