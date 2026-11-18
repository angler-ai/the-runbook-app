//! Memory CLI commands (PRD §10, §11; plan §2.1 MEM4).
//!
//! - `runbook memory search <runbook> [--step <id>] [--tag <t>] [query]`
//!   (active entries only)
//! - `runbook memory save --title <t> --body-file <path> [--step <id>] [--tag <t>]... [--propose]`
//!   Live at once (MEM6), or a proposal for review with `--propose`. An
//!   agent saves with its stored execution credentials (the step it claims,
//!   or the active pair from `runbook run token`), so the entry names its
//!   run; only without a stored pair does it fall back to the login.
//! - `runbook memory edit <mem> --revision <n> [--title] [--body-file] [--step] [--tag]...`
//!   (MEM6: agents and people; the same credentials as `save`)
//! - `runbook memory history <mem>`, `runbook memory revert <mem> --to <n>`
//!   and `runbook memory lock|unlock <mem>` (people: who wrote each
//!   revision, a restore of one, and a lock against agent edits)
//! - `runbook memory list --proposed` (with what a reviewer needs)
//! - `runbook memory accept|reject <mem> --revision <n>` (people review:
//!   the revision binds `If-Match` to what the reviewer saw),
//!   `runbook memory reject --all-proposed <runbook>` and
//!   `runbook memory archive <mem>` (people only)
//!
//! Entry text is untrusted: everything printed in human output has its
//! control characters escaped.
use crate::agent::{Env, NoSession, resolve_session, session_for};
use crate::api::{ApiError, Request};
use crate::cliutil::{Exit, OK, fail, idempotency_key, parse_id, print_json, text, usage};
use clap::{Args, Subcommand};
use runbook_model::{MemoryId, RunbookId};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Read};

#[derive(Subcommand, Debug)]
pub enum MemoryCommands {
    #[command(about = "Search a runbook's active memory")]
    Search(SearchArgs),
    #[command(
        about = "Save a durable fact for future runs: live at once, or a proposal with --propose"
    )]
    Save(SaveArgs),
    #[command(about = "Edit a memory entry at the revision you read (agents and people)")]
    Edit(EditArgs),
    #[command(about = "List a memory entry's revisions and who wrote each (people only)")]
    History(HistoryArgs),
    #[command(
        about = "Restore a prior revision's content as a new revision; unarchives (people only)"
    )]
    Revert(RevertArgs),
    #[command(about = "Lock a memory entry so agents cannot edit it (people only)")]
    Lock(ActionArgs),
    #[command(about = "Unlock a memory entry (people only)")]
    Unlock(ActionArgs),
    #[command(about = "List memory entries (use --proposed for proposals)")]
    List(ListArgs),
    #[command(about = "Accept a proposed memory entry (people only)")]
    Accept(ReviewArgs),
    #[command(
        about = "Reject a proposed memory entry, or every proposal of a runbook (people only)"
    )]
    Reject(RejectArgs),
    #[command(about = "Archive a memory entry (people only)")]
    Archive(ActionArgs),
}

#[derive(Args, Debug)]
pub struct ReviewArgs {
    #[arg(help = "Memory ID (mem_…)", required = true)]
    pub mem: String,
    #[arg(
        long,
        required = true,
        help = "The revision you reviewed (from `memory list --proposed`); refused if it changed"
    )]
    pub revision: u32,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RejectArgs {
    #[arg(help = "Memory ID (mem_…)", required_unless_present = "all_proposed")]
    pub mem: Option<String>,
    #[arg(long, help = "The revision you reviewed; refused if it changed")]
    pub revision: Option<u32>,
    #[arg(
        long = "all-proposed",
        value_name = "RUNBOOK",
        conflicts_with = "mem",
        help = "Reject every pending proposal of this runbook (ID or slug)"
    )]
    pub all_proposed: Option<String>,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (required when RUNBOOK is a slug)"
    )]
    pub project: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    #[arg(help = "Runbook ID or slug")]
    pub runbook: String,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (required when <runbook> is a slug)"
    )]
    pub project: Option<String>,
    #[arg(help = "Words that must all appear in the title or body")]
    pub query: Option<String>,
    #[arg(long = "step", help = "Step ID or step path")]
    pub step: Option<String>,
    #[arg(long = "tag", help = "Tag (repeat for multiple tags)")]
    pub tags: Vec<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
    #[arg(
        long,
        help = "Page limit (1-100)",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    pub limit: Option<u32>,
    #[arg(long, help = "Pagination cursor")]
    pub cursor: Option<String>,
}

#[derive(Args, Debug)]
pub struct SaveArgs {
    #[arg(help = "Runbook ID or slug")]
    pub runbook: Option<String>,
    #[arg(long = "runbook", help = "Runbook ID or slug")]
    pub runbook_flag: Option<String>,
    #[arg(long, env = "RUNBOOK_PROJECT", help = "Project ID or slug")]
    pub project: Option<String>,
    #[arg(long, required = true, help = "Short title")]
    pub title: String,
    #[arg(
        long = "body-file",
        required = true,
        help = "Path to markdown body file, or '-' for stdin"
    )]
    pub body_file: String,
    #[arg(long = "step", help = "Step ID or step path")]
    pub step: Option<String>,
    #[arg(long = "tag", help = "Tag (repeat for multiple tags)")]
    pub tags: Vec<String>,
    #[arg(
        long = "step-run",
        help = "The step run you claim: save with that run's stored execution credentials"
    )]
    pub step_run: Option<String>,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(
        long,
        help = "Save a proposal a person reviews instead of a live entry"
    )]
    pub propose: bool,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct EditArgs {
    #[arg(help = "Memory ID (mem_…)", required = true)]
    pub mem: String,
    #[arg(
        long,
        required = true,
        help = "The revision you read; refused if it changed (412 memory.conflict)"
    )]
    pub revision: u32,
    #[arg(long, help = "New title (default: keep)")]
    pub title: Option<String>,
    #[arg(
        long = "body-file",
        help = "Path to the new markdown body, or '-' for stdin (default: keep)"
    )]
    pub body_file: Option<String>,
    #[arg(long = "step", help = "New step ID or step path (default: keep)")]
    pub step: Option<String>,
    #[arg(long = "tag", help = "Tag (repeat; replaces every tag; default: keep)")]
    pub tags: Vec<String>,
    #[arg(
        long = "step-run",
        help = "The step run you claim: edit with that run's stored execution credentials"
    )]
    pub step_run: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct HistoryArgs {
    #[arg(help = "Memory ID (mem_…)", required = true)]
    pub mem: String,
    #[arg(
        long,
        help = "Page limit (1-100)",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    pub limit: Option<u32>,
    #[arg(long, help = "Pagination cursor (`next:` of the last page)")]
    pub cursor: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RevertArgs {
    #[arg(help = "Memory ID (mem_…)", required = true)]
    pub mem: String,
    #[arg(long, required = true, help = "The revision whose content to restore")]
    pub to: u32,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    #[arg(help = "Runbook ID or slug")]
    pub runbook: Option<String>,
    #[arg(long = "runbook", help = "Runbook ID or slug")]
    pub runbook_flag: Option<String>,
    #[arg(long, env = "RUNBOOK_PROJECT", help = "Project ID or slug")]
    pub project: Option<String>,
    #[arg(long, help = "List proposed entries")]
    pub proposed: bool,
    #[arg(long, help = "Filter by status")]
    pub status: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
    #[arg(
        long,
        help = "Page limit (1-100)",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    pub limit: Option<u32>,
    #[arg(long, help = "Pagination cursor")]
    pub cursor: Option<String>,
}

#[derive(Args, Debug)]
pub struct ActionArgs {
    #[arg(help = "Memory ID (mem_…)", required = true)]
    pub mem: String,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

/// Report an API failure; 4 for not found (404) or rate/quota limit, otherwise 1.
pub fn memory_api_failed(e: ApiError, json: bool) -> i32 {
    crate::cliutil::api_failed_404(e, json)
}

/// Entry text for a terminal: control characters (newlines, ESC, bidi
/// overrides and the rest) escaped, so stored text never drives the
/// terminal or forges an output line.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                c.escape_unicode().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn print_entry(item: &Value) {
    let id = clean(text(item, "id"));
    let status = clean(text(item, "status"));
    let title = clean(text(item, "title"));
    println!("{id}  {status}  {title}");
}

/// The source of an entry, for a reviewer.
fn source(item: &Value) -> String {
    let s = &item["source"];
    let who = match text(s, "kind") {
        "person" => format!("person {}", text(s, "user_id")),
        "step_run" => format!(
            "run {} step {}",
            text(s, "execution_id"),
            text(s, "step_run_id")
        ),
        "agent" => format!(
            "agent token {} (sponsor {})",
            text(s, "token_id"),
            text(s, "sponsor_user_id")
        ),
        "service" => format!("service token {}", text(s, "token_id")),
        other => other.to_owned(),
    };
    clean(&who)
}

/// What a reviewer needs before accepting: revision, source, step, tags and
/// the start of the body.
fn print_review(item: &Value) {
    print_entry(item);
    let revision = item["revision"].as_u64().unwrap_or_default();
    println!("    revision: {revision}  source: {}", source(item));
    if let Some(last) = item["last_writer"].as_object() {
        let kind = last.get("kind").and_then(Value::as_str).unwrap_or_default();
        let actor = last
            .get("actor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        println!("    last written by: {} {}", clean(kind), clean(actor));
    }
    if let Some(step) = item["step_id"].as_str() {
        println!("    step: {}", clean(step));
    }
    let tags: Vec<_> = item["tags"]
        .as_array()
        .map(|t| t.iter().filter_map(Value::as_str).map(clean).collect())
        .unwrap_or_default();
    if !tags.is_empty() {
        println!("    tags: {}", tags.join(", "));
    }
    let body = text(item, "body_markdown");
    let mut excerpt: String = body.chars().take(240).collect();
    if excerpt.len() < body.len() {
        excerpt.push('…');
    }
    println!("    body: {}", clean(&excerpt));
}

fn revision_tag(revision: u32) -> String {
    format!("\"r{revision}\"")
}

pub fn handle_memory(env: &Env, command: MemoryCommands) -> i32 {
    // Saving picks its own credentials; everything else is a person's (or
    // an explicit token's) and may use the login.
    let login = || crate::auth::with_login(clone_env(env));
    let result = match command {
        MemoryCommands::Save(a) => save(env, &login(), a),
        MemoryCommands::Edit(a) => edit(env, &login(), a),
        MemoryCommands::History(a) => history(&login(), a),
        MemoryCommands::Revert(a) => revert(&login(), a),
        MemoryCommands::Lock(a) => patch_action(&login(), &a.mem, "lock", a.json),
        MemoryCommands::Unlock(a) => patch_action(&login(), &a.mem, "unlock", a.json),
        MemoryCommands::Search(a) => search(&login(), a),
        MemoryCommands::List(a) => list(&login(), a),
        MemoryCommands::Accept(a) => review(&login(), &a.mem, a.revision, "accept", a.json),
        MemoryCommands::Reject(a) => reject(&login(), a),
        MemoryCommands::Archive(a) => patch_action(&login(), &a.mem, "archive", a.json),
    };
    match result {
        Ok(code) => code,
        Err(code) => code,
    }
}

fn clone_env(env: &Env) -> Env {
    env.clone()
}

fn resolve_runbook_id(env: &Env, raw: &str, project: Option<&str>, json: bool) -> Exit<RunbookId> {
    crate::cliutil::resolve_runbook_id(env, raw, project, "memory", json)
}

fn search(env: &Env, a: SearchArgs) -> Exit<i32> {
    let rb_id = resolve_runbook_id(env, &a.runbook, a.project.as_deref(), a.json)?;
    // Search reads active memory; proposals are listed with `memory list --proposed`.
    let mut query: Vec<(String, String)> = vec![("status".into(), "active".into())];
    if let Some(q) = &a.query {
        query.push(("q".into(), q.clone()));
    }
    if let Some(step) = &a.step {
        query.push(("step_id".into(), step.clone()));
    }
    if !a.tags.is_empty() {
        query.push(("tags".into(), a.tags.join(",")));
    }
    if let Some(limit) = a.limit {
        query.push(("limit".into(), limit.to_string()));
    }
    if let Some(cursor) = &a.cursor {
        query.push(("cursor".into(), cursor.clone()));
    }

    let path = format!("/v1/runbooks/{rb_id}/memory");
    let reply = crate::cliutil::client(env)?
        .send(&Request {
            method: "GET",
            path: &path,
            query: &query,
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, a.json))?;

    if a.json {
        print_json(&reply.body);
    } else {
        if let Some(items) = reply.body["items"].as_array() {
            for item in items {
                print_entry(item);
            }
        }
        if let Some(next) = reply.body["next_cursor"].as_str().filter(|c| !c.is_empty()) {
            println!("next: {}", clean(next));
        }
    }
    Ok(OK)
}

/// The credentials a save or an edit uses: `RUNBOOK_TOKEN` when set; else
/// the stored execution pair of the claimed step (`--step-run`) or the
/// active pair (`runbook run token`), so an agent's write names its run;
/// only with no stored pair, the login (a person's own token).
fn save_session(env: &Env, login: &Env, step_run: Option<&str>) -> Exit<crate::session::Session> {
    if let Some(sr) = step_run {
        parse_id::<runbook_model::StepRunId>(sr, "step run")?;
        let execution = env
            .store
            .as_ref()
            .and_then(|s| s.load_step(sr).ok().flatten())
            .filter(|s| s.host == env.host)
            .and_then(|s| s.execution_id);
        if execution.is_none() && env.token.is_none() {
            return Err(usage(format!(
                "no stored execution for {sr}; claim it with `runbook step claim {sr}` first"
            )));
        }
        return session_for(env, execution.as_deref());
    }
    match resolve_session(env, None) {
        Ok(session) => Ok(session),
        Err(NoSession::Unreadable(msg)) => Err(fail(msg)),
        Err(NoSession::Missing(_)) => match &login.token {
            Some(token) => Ok(crate::session::Session::new_static(
                login.host.clone(),
                token.clone(),
            )),
            None => Err(usage(
                "not logged in: run `runbook login`, set RUNBOOK_TOKEN, or install an execution pair with `runbook run token <exe>`",
            )),
        },
    }
}

/// A markdown body from a file, or stdin for `-`.
fn read_body(path: &str) -> Exit<String> {
    if path == "-" {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| fail(format!("cannot read stdin: {e}")))?;
        Ok(buf)
    } else {
        fs::read_to_string(path).map_err(|e| fail(format!("cannot read body file '{path}': {e}")))
    }
}

fn save(env: &Env, login: &Env, a: SaveArgs) -> Exit<i32> {
    let body_markdown = read_body(&a.body_file)?;

    let mut session = save_session(env, login, a.step_run.as_deref())?;
    let path = if let Some(rb) = a.runbook_flag.as_ref().or(a.runbook.as_ref()) {
        let rb_id = match rb.parse::<RunbookId>() {
            Ok(id) => id,
            // A slug resolves with the login, as other commands do.
            Err(_) => resolve_runbook_id(login, rb, a.project.as_deref(), a.json)?,
        };
        format!("/v1/runbooks/{rb_id}/memory")
    } else if let Some(p) = a.project.as_ref() {
        crate::selector::validate_project_ref(p).map_err(usage)?;
        format!("/v1/projects/{p}/memory")
    } else {
        return Err(usage(
            "pass <runbook> or --runbook (or --project) to save memory",
        ));
    };

    let key = idempotency_key(&a.idempotency_key)?;
    let mut body_map = serde_json::Map::new();
    body_map.insert("title".into(), Value::String(a.title));
    body_map.insert("body_markdown".into(), Value::String(body_markdown));
    body_map.insert("tags".into(), json!(a.tags));
    if let Some(step) = a.step {
        body_map.insert("step_id".into(), Value::String(step));
    }
    if a.propose {
        body_map.insert("propose".into(), Value::Bool(true));
    }
    let body = Value::Object(body_map);

    let reply = session
        .send(&Request {
            method: "POST",
            path: &path,
            body: Some(&body),
            idempotency_key: Some(&key),
            ..Default::default()
        })
        .map_err(|e| crate::agent::failed(e, a.json))?;

    if a.json {
        print_json(&reply.body);
    } else {
        print_entry(&reply.body);
        if reply.body["status"] == "proposed" {
            eprintln!("saved as a proposal: a person reviews it before future runs see it");
        } else {
            eprintln!("saved: future runs see it");
        }
    }
    Ok(OK)
}

/// The editable content of an entry view, as a save request carries it.
fn content_of(entry: &Value) -> serde_json::Map<String, Value> {
    let mut content = serde_json::Map::new();
    for key in ["title", "body_markdown", "tags", "step_id"] {
        content.insert(key.into(), entry[key].clone());
    }
    if !entry["expires_at"].is_null() {
        content.insert("expires_at".into(), entry["expires_at"].clone());
    }
    content
}

/// MEM6: edit an entry (agents and people). The entry is read first so the
/// flags only change what they name; the edit is fenced by `--revision`, so
/// an entry changed since that revision is refused.
fn edit(env: &Env, login: &Env, a: EditArgs) -> Exit<i32> {
    let id = parse_id::<MemoryId>(&a.mem, "memory")?;
    let body = a.body_file.as_deref().map(read_body).transpose()?;
    let mut session = save_session(env, login, a.step_run.as_deref())?;
    let path = format!("/v1/memory/{id}");
    let current = session
        .send(&Request {
            method: "GET",
            path: &path,
            ..Default::default()
        })
        .map_err(|e| crate::agent::failed(e, a.json))?;
    let mut entry = content_of(&current.body);
    if let Some(title) = a.title {
        entry.insert("title".into(), Value::String(title));
    }
    if let Some(body) = body {
        entry.insert("body_markdown".into(), Value::String(body));
    }
    if let Some(step) = a.step {
        entry.insert("step_id".into(), Value::String(step));
    }
    if !a.tags.is_empty() {
        entry.insert("tags".into(), json!(a.tags));
    }
    let updated = session
        .send(&Request {
            method: "PATCH",
            path: &path,
            body: Some(&json!({"action": "edit", "entry": entry})),
            if_match: Some(&revision_tag(a.revision)),
            ..Default::default()
        })
        .map_err(|e| crate::agent::failed(e, a.json))?;
    if a.json {
        print_json(&updated.body);
    } else {
        print_entry(&updated.body);
    }
    Ok(OK)
}

/// MEM6: an entry's kept revisions and who wrote each (people only), a
/// page at a time; says how many were pruned from the middle.
fn history(env: &Env, a: HistoryArgs) -> Exit<i32> {
    let id = parse_id::<MemoryId>(&a.mem, "memory")?;
    let mut query: Vec<(String, String)> = vec![];
    if let Some(limit) = a.limit {
        query.push(("limit".into(), limit.to_string()));
    }
    if let Some(cursor) = &a.cursor {
        query.push(("cursor".into(), cursor.clone()));
    }
    let reply = crate::cliutil::client(env)?
        .send(&Request {
            method: "GET",
            path: &format!("/v1/memory/{id}/revisions"),
            query: &query,
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, a.json))?;
    if a.json {
        print_json(&reply.body);
        return Ok(OK);
    }
    for item in reply.body["items"].as_array().cloned().unwrap_or_default() {
        println!(
            "r{}  {}  {}  {}  {}  {}",
            item["revision"].as_u64().unwrap_or_default(),
            clean(text(&item, "action")),
            clean(text(&item, "actor_kind")),
            clean(text(&item, "actor")),
            clean(text(&item, "at")),
            clean(text(&item["entry"], "title")),
        );
    }
    if let Some(next) = reply.body["next_cursor"].as_str().filter(|c| !c.is_empty()) {
        println!("next: {}", clean(next));
    }
    let pruned = reply.body["pruned"].as_u64().unwrap_or_default();
    if pruned > 0 {
        eprintln!("{pruned} revisions pruned: the first and the newest 50 are kept");
    }
    Ok(OK)
}

/// MEM6 review fix 6: restore a prior revision's content as a new
/// revision (people only; it unarchives), fenced by the current revision.
fn revert(env: &Env, a: RevertArgs) -> Exit<i32> {
    let id = parse_id::<MemoryId>(&a.mem, "memory")?;
    let path = format!("/v1/memory/{id}");
    let client = crate::cliutil::client(env)?;
    let current = client
        .send(&Request {
            method: "GET",
            path: &path,
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, a.json))?;
    let if_match = current
        .etag
        .filter(|t| t.starts_with("\"r"))
        .ok_or_else(|| fail("could not determine the entry's current revision"))?;
    let updated = client
        .send(&Request {
            method: "PATCH",
            path: &path,
            body: Some(&json!({"action": "restore", "revision": a.to})),
            if_match: Some(&if_match),
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, a.json))?;
    if a.json {
        print_json(&updated.body);
    } else {
        eprintln!("restored {id} to the content of revision {}", a.to);
        print_entry(&updated.body);
    }
    Ok(OK)
}

fn list(env: &Env, a: ListArgs) -> Exit<i32> {
    let path = if let Some(rb) = a.runbook_flag.as_ref().or(a.runbook.as_ref()) {
        let rb_id = resolve_runbook_id(env, rb, a.project.as_deref(), a.json)?;
        format!("/v1/runbooks/{rb_id}/memory")
    } else if let Some(p) = a.project.as_ref() {
        crate::selector::validate_project_ref(p).map_err(usage)?;
        format!("/v1/projects/{p}/memory")
    } else {
        return Err(usage(
            "pass <runbook> or --runbook (or --project) to list memory",
        ));
    };

    let mut query: Vec<(String, String)> = Vec::new();
    if a.proposed {
        query.push(("status".into(), "proposed".into()));
    } else if let Some(status) = a.status {
        query.push(("status".into(), status));
    }
    if let Some(limit) = a.limit {
        query.push(("limit".into(), limit.to_string()));
    }
    if let Some(cursor) = a.cursor {
        query.push(("cursor".into(), cursor));
    }

    let reply = crate::cliutil::client(env)?
        .send(&Request {
            method: "GET",
            path: &path,
            query: &query,
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, a.json))?;

    if a.json {
        print_json(&reply.body);
    } else {
        if let Some(items) = reply.body["items"].as_array() {
            for item in items {
                if a.proposed || item["status"] == "proposed" {
                    print_review(item);
                } else {
                    print_entry(item);
                }
            }
        }
        if let Some(next) = reply.body["next_cursor"].as_str().filter(|c| !c.is_empty()) {
            println!("next: {}", clean(next));
        }
        if a.proposed {
            eprintln!(
                "review each, then `runbook memory accept <mem> --revision <n>` or `runbook memory reject <mem> --revision <n>`"
            );
        }
    }
    Ok(OK)
}

/// Accept or reject one proposal at the revision the reviewer saw. That
/// revision is shown first, so the person sees what the action applies to;
/// an entry changed since is refused (`412 memory.conflict`).
fn review(env: &Env, mem_raw: &str, revision: u32, action: &str, json: bool) -> Exit<i32> {
    let id = parse_id::<MemoryId>(mem_raw, "memory")?;
    let path = format!("/v1/memory/{id}");
    let client = crate::cliutil::client(env)?;
    let shown = client
        .send(&Request {
            method: "GET",
            path: &path,
            query: &[("revision".to_owned(), revision.to_string())],
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, json))?;
    if !json {
        eprintln!("{action}: revision {revision} of {id}");
        print_review(&shown.body);
    }
    let updated = client
        .send(&Request {
            method: "PATCH",
            path: &path,
            body: Some(&json!({ "action": action })),
            if_match: Some(&revision_tag(revision)),
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, json))?;
    if json {
        print_json(&updated.body);
    } else {
        print_entry(&updated.body);
    }
    Ok(OK)
}

/// Reject one proposal (`--revision` binds it), or every pending proposal
/// of a runbook, each at the revision just listed: a proposal that changes
/// meanwhile is refused and reported, never rejected unseen.
fn reject(env: &Env, a: RejectArgs) -> Exit<i32> {
    let Some(rb) = &a.all_proposed else {
        let mem = a.mem.as_deref().unwrap_or_default();
        let revision = a
            .revision
            .ok_or_else(|| usage("pass --revision <n> (the revision you reviewed)"))?;
        return review(env, mem, revision, "reject", a.json);
    };
    let rb_id = resolve_runbook_id(env, rb, a.project.as_deref(), a.json)?;
    let client = crate::cliutil::client(env)?;
    let path = format!("/v1/runbooks/{rb_id}/memory");
    let (mut rejected, mut refused, mut cursor) = (vec![], vec![], None::<String>);
    loop {
        let mut query = vec![
            ("status".to_owned(), "proposed".to_owned()),
            ("limit".to_owned(), "100".to_owned()),
        ];
        if let Some(c) = &cursor {
            query.push(("cursor".into(), c.clone()));
        }
        let page = client
            .send(&Request {
                method: "GET",
                path: &path,
                query: &query,
                ..Default::default()
            })
            .map_err(|e| memory_api_failed(e, a.json))?;
        for item in page.body["items"].as_array().cloned().unwrap_or_default() {
            // A runbook's listing includes its project's proposals; this
            // rejects the runbook's own only.
            if item["scope"]["runbook_id"] != rb_id.as_str() {
                continue;
            }
            let id = parse_id::<MemoryId>(text(&item, "id"), "memory")?;
            let revision =
                u32::try_from(item["revision"].as_u64().unwrap_or_default()).unwrap_or_default();
            let result = client.send(&Request {
                method: "PATCH",
                path: &format!("/v1/memory/{id}"),
                body: Some(&json!({"action": "reject"})),
                if_match: Some(&revision_tag(revision)),
                ..Default::default()
            });
            match result {
                Ok(_) => rejected.push(id.to_string()),
                Err(e) => refused.push(json!({"id": id.to_string(), "error": e.to_string()})),
            }
        }
        cursor = page.body["next_cursor"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    if a.json {
        print_json(&json!({"rejected": rejected, "refused": refused}));
    } else {
        println!("rejected {} proposal(s) of {rb_id}", rejected.len());
        for r in &refused {
            println!(
                "refused {}: {}",
                clean(text(r, "id")),
                clean(text(r, "error"))
            );
        }
    }
    Ok(if refused.is_empty() { OK } else { 1 })
}

fn patch_action(env: &Env, mem_raw: &str, action: &str, json: bool) -> Exit<i32> {
    let id = parse_id::<MemoryId>(mem_raw, "memory")?;
    let path = format!("/v1/memory/{id}");
    let client = crate::cliutil::client(env)?;

    let current = client
        .send(&Request {
            method: "GET",
            path: &path,
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, json))?;

    let if_match = if let Some(tag) = current.etag.filter(|t| t.starts_with("\"r")) {
        tag
    } else if let Some(rev) = current.body.get("revision").and_then(Value::as_u64) {
        format!("\"r{rev}\"")
    } else {
        return Err(fail("could not determine memory entry revision"));
    };

    let body = json!({ "action": action });
    let updated = client
        .send(&Request {
            method: "PATCH",
            path: &path,
            body: Some(&body),
            if_match: Some(&if_match),
            ..Default::default()
        })
        .map_err(|e| memory_api_failed(e, json))?;

    if json {
        print_json(&updated.body);
    } else {
        print_entry(&updated.body);
    }
    Ok(OK)
}
