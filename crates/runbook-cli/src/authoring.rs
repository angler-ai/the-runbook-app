//! Top-level authoring commands: `list`, `get`, `pull`, `push`, `publish`
//! (PRD §10). They share `runbook_mcp::ops` with the MCP authoring tools, so
//! both interfaces send the same requests.
//!
//! A publishing `push` never changes anything it cannot finish: before the
//! first write it confirms `runbooks:publish` with `ops::probe_publish`, and
//! without the scope it exits 1 with `403 auth.scope_missing`, suggesting
//! `--draft`.
use crate::agent::Env;
use crate::credstore::StateDir;
use crate::host::CliBackend;
use crate::session::random_hex;
use clap::{Args, Subcommand};
use runbook_mcp::backend::{Backend, BackendError};
use runbook_mcp::ops::blocking as ops;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum AuthoringCommands {
    #[command(about = "List a project's runbooks")]
    List(ListArgs),
    #[command(
        about = "Show a runbook, its latest published version, input schema and step outline"
    )]
    Get(GetArgs),
    #[command(about = "Write a runbook's draft source to a file or stdout")]
    Pull(PullArgs),
    #[command(about = "Create or update a runbook from a file and publish it (--draft saves only)")]
    Push(PushArgs),
    #[command(about = "Publish a runbook's current draft")]
    Publish(PublishArgs),
    #[command(
        about = "Compare a local spec file with the published runbook, or two published versions"
    )]
    Diff(DiffArgs),
}

#[derive(Args, Debug)]
pub struct DiffArgs {
    #[arg(
        value_name = "FILE|RUNBOOK@VERSION",
        help = "A local spec file, or a published version <runbook>[@<n|rbv_id|latest>]"
    )]
    pub first: String,
    #[arg(
        value_name = "RUNBOOK@VERSION",
        help = "The published version to compare with (default: the runbook's latest)"
    )]
    pub second: Option<String>,
    #[arg(
        long,
        help = "Project ID or slug (to resolve slugs and a file's runbook)"
    )]
    pub project: Option<String>,
    #[arg(long, help = "Also show a unified diff of the two sources")]
    pub text: bool,
    #[arg(long, help = "Output as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    #[arg(long, env = "RUNBOOK_PROJECT", help = "Project ID or slug")]
    pub project: String,
    #[arg(
        long,
        help = "Page limit (1-100)",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    pub limit: Option<u32>,
    #[arg(long, help = "Pagination cursor")]
    pub cursor: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    #[arg(help = "Runbook (rb_ ID, or slug with --project)")]
    pub runbook: String,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (to resolve a slug)"
    )]
    pub project: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct PullArgs {
    #[arg(help = "Runbook (rb_ ID, or slug with --project)")]
    pub runbook: String,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (to resolve a slug)"
    )]
    pub project: Option<String>,
    #[arg(short = 'o', long = "output", help = "Write the source to this file")]
    pub output: Option<PathBuf>,
    #[arg(long, help = "Output source and revision as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct PushArgs {
    #[arg(help = "Runbook spec file (its metadata.id is the slug)")]
    pub file: PathBuf,
    #[arg(long, env = "RUNBOOK_PROJECT", help = "Project ID or slug")]
    pub project: String,
    #[arg(long, help = "Save the draft without publishing")]
    pub draft: bool,
    #[arg(long, help = "Expected current draft revision (fail if it moved)")]
    pub revision: Option<u64>,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct PublishArgs {
    #[arg(help = "Runbook (rb_ ID, or slug with --project)")]
    pub runbook: String,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (to resolve a slug)"
    )]
    pub project: Option<String>,
    #[arg(long, help = "Draft revision to publish (fail if the draft moved)")]
    pub revision: Option<u64>,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

pub fn handle(env: Env, command: AuthoringCommands) -> i32 {
    let store = env.store.clone();
    let mut backend = CliBackend::new(env);
    let b: &mut dyn Backend = &mut backend;
    let (json, publishing, result) = match command {
        AuthoringCommands::List(a) => (a.json, false, list(b, &a)),
        AuthoringCommands::Get(a) => (a.json, false, get(b, &a)),
        AuthoringCommands::Pull(a) => (a.json, false, pull(b, &a)),
        AuthoringCommands::Push(a) => (a.json, !a.draft, push(b, &a, store.as_ref())),
        AuthoringCommands::Publish(a) => (a.json, true, publish(b, &a)),
        AuthoringCommands::Diff(a) => (a.json, false, diff(b, &a, store.as_ref())),
    };
    result.unwrap_or_else(|e| failed(&e, json, publishing))
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
    );
}

/// Report an error and return the stable exit code: 2 for usage problems
/// and invalid specs, 4 for limits, otherwise 1.
fn failed(e: &BackendError, json: bool, publishing: bool) -> i32 {
    eprintln!("error: {e}");
    let code = e.code().unwrap_or("");
    let exit = match e {
        BackendError::Usage(_) => 2,
        BackendError::Problem { status: 429, .. } => 4,
        _ if code.starts_with("limit.") || code.starts_with("quota.") => 4,
        BackendError::Problem { status: 422, .. } if code.starts_with("spec.") => 2,
        _ => 1,
    };
    let scope_missing = publishing && e.status() == Some(403);
    if scope_missing {
        eprintln!(
            "nothing was changed. Without runbooks:publish, save with `runbook push <file> --draft` \
             and ask a person with runbooks:publish to publish the draft revision."
        );
    }
    if json {
        let mut out = json!({"error": {"code": e.code(), "message": e.to_string()}});
        if let Some(status) = e.status() {
            out["error"]["status"] = json!(status);
        }
        if scope_missing {
            out["next_action"] = json!("ask_human_to_publish");
        }
        print_json(&out);
    }
    exit
}

/// The given idempotency key, or a new one (printed so a retry can reuse
/// it). A push derives one key per write from it.
fn base_key(given: &Option<String>) -> Result<String, BackendError> {
    match given {
        Some(k) => Ok(k.clone()),
        None => {
            let key = random_hex().map_err(BackendError::Usage)?;
            eprintln!("Idempotency-Key: {key}");
            Ok(key)
        }
    }
}

fn list(b: &mut dyn Backend, a: &ListArgs) -> Result<i32, BackendError> {
    let page = ops::list_runbooks(b, &a.project, a.limit, a.cursor.as_deref())?;
    if a.json {
        print_json(&page);
        return Ok(0);
    }
    for item in page["items"].as_array().into_iter().flatten() {
        println!(
            "{}  {}  {}{}",
            item["runbook_id"].as_str().unwrap_or(""),
            item["slug"].as_str().unwrap_or(""),
            item["origin"].as_str().unwrap_or(""),
            if item["archived"].as_bool() == Some(true) {
                "  archived"
            } else {
                ""
            }
        );
    }
    if let Some(next) = page["next_cursor"].as_str() {
        println!("next cursor: {next}");
    }
    Ok(0)
}

fn get(b: &mut dyn Backend, a: &GetArgs) -> Result<i32, BackendError> {
    let rb = ops::resolve_runbook(b, &a.runbook, a.project.as_deref())?;
    let view = ops::get_runbook(b, &rb)?;
    if a.json {
        print_json(&view);
        return Ok(0);
    }
    println!(
        "{}  {}  {}",
        view["runbook_id"].as_str().unwrap_or(&rb),
        view["slug"].as_str().unwrap_or(""),
        view["origin"].as_str().unwrap_or("")
    );
    match view["latest_version"]["runbook_version"].as_u64() {
        Some(n) => println!(
            "latest version {n} ({})",
            view["latest_version"]["runbook_version_id"]
                .as_str()
                .unwrap_or("")
        ),
        None => println!("not published"),
    }
    // CLI3: the inputs to start it with, and its steps (titles only).
    if let Some(props) = view["input_schema"]["properties"].as_object() {
        let required: Vec<&str> = view["input_schema"]["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        println!("inputs:");
        for (name, schema) in props {
            println!(
                "  {name}{}  {}",
                if required.contains(&name.as_str()) {
                    " (required)"
                } else {
                    ""
                },
                schema["type"].as_str().unwrap_or("")
            );
        }
    }
    for (key, label) in [("steps", "steps:"), ("finally", "finally:")] {
        let steps = view["outline"][key].as_array().cloned().unwrap_or_default();
        if !steps.is_empty() {
            println!("{label}");
            print_outline(&steps, 1);
        }
    }
    Ok(0)
}

fn print_outline(steps: &[Value], depth: usize) {
    for step in steps {
        println!(
            "{}{}  {} ({})",
            "  ".repeat(depth),
            step["id"].as_str().unwrap_or(""),
            step["title"].as_str().unwrap_or(""),
            step["kind"].as_str().unwrap_or("")
        );
        if let Some(children) = step["steps"].as_array() {
            print_outline(children, depth + 1);
        }
    }
}

fn pull(b: &mut dyn Backend, a: &PullArgs) -> Result<i32, BackendError> {
    let rb = ops::resolve_runbook(b, &a.runbook, a.project.as_deref())?;
    let draft = ops::get_draft(b, &rb)?;
    if let Some(path) = &a.output {
        std::fs::write(path, &draft.source)
            .map_err(|e| BackendError::Usage(format!("cannot write '{}': {e}", path.display())))?;
    }
    if a.json {
        let mut out = json!({"runbook_id": draft.runbook_id, "revision": draft.revision});
        if a.output.is_none() {
            out["source"] = json!(draft.source);
        }
        print_json(&out);
    } else {
        if a.output.is_none() {
            print!("{}", draft.source);
        }
        eprintln!("draft revision {} of {}", draft.revision, draft.runbook_id);
    }
    Ok(0)
}

/// `metadata.id` of a spec file: the runbook's slug.
pub fn metadata_id(source: &str) -> Option<String> {
    let docs = yaml_rust2::YamlLoader::load_from_str(source).ok()?;
    (docs.len() == 1)
        .then(|| docs[0]["metadata"]["id"].as_str().map(str::to_owned))
        .flatten()
}

fn print_diagnostics(view: &Value) -> usize {
    let diagnostics = view["diagnostics"].as_array().cloned().unwrap_or_default();
    for d in &diagnostics {
        eprintln!(
            "{}: {}: {}",
            d["path"].as_str().unwrap_or(""),
            d["code"].as_str().unwrap_or(""),
            d["message"].as_str().unwrap_or("")
        );
    }
    diagnostics.len()
}

fn push(b: &mut dyn Backend, a: &PushArgs, store: Option<&StateDir>) -> Result<i32, BackendError> {
    if let Some(store) = store {
        store.refuse_inside(&a.file).map_err(BackendError::Usage)?;
    }
    let source = std::fs::read_to_string(&a.file)
        .map_err(|e| BackendError::Usage(format!("cannot read '{}': {e}", a.file.display())))?;
    let slug = metadata_id(&source).ok_or_else(|| {
        BackendError::Usage(format!(
            "'{}' has no metadata.id; it names the runbook",
            a.file.display()
        ))
    })?;
    ops::check_segment("project", &a.project)?;
    let existing = ops::find_by_slug(b, &a.project, &slug)?
        .and_then(|item| item["runbook_id"].as_str().map(str::to_owned));
    if existing.is_none() && a.revision.is_some() {
        return Err(BackendError::Usage(format!(
            "--revision was given but '{slug}' does not exist in project '{}'",
            a.project
        )));
    }
    if !a.draft {
        // Confirm runbooks:publish before the first write.
        let target = match &existing {
            Some(rb) => rb.clone(),
            None => any_runbook(b, &a.project)?.ok_or_else(|| {
                let file = shell_arg(&a.file.to_string_lossy());
                let project = shell_arg(&a.project);
                let slug = shell_arg(&slug);
                BackendError::Usage(format!(
                    "cannot confirm runbooks:publish before project {project} has a runbook. \
                     Run `runbook push {file} --project {project} --draft`, then `runbook publish {slug} --project {project}`"
                ))
            })?,
        };
        ops::probe_publish(b, &target)?;
    }
    let key = base_key(&a.idempotency_key)?;
    let (rb, created, view) = match &existing {
        Some(rb) => {
            let draft = ops::get_draft(b, rb)?;
            ops::expect_revision(&draft, a.revision)?;
            if draft.source == source {
                let view = json!({"runbook_id": rb, "revision": draft.revision, "diagnostics": [], "unchanged": true});
                (rb.clone(), false, view)
            } else {
                let view = ops::save_over(b, &draft, &source, &format!("{key}-draft"))?;
                (rb.clone(), false, ops::without_source(view))
            }
        }
        None => {
            let view = ops::create_runbook(b, &a.project, &source, &format!("{key}-draft"))?;
            let rb = view["runbook_id"].as_str().unwrap_or_default().to_owned();
            (rb, true, ops::without_source(view))
        }
    };
    let revision = view["revision"].as_u64();
    let problems = print_diagnostics(&view);
    let mut out = json!({
        "runbook_id": rb,
        "slug": slug,
        "created": created,
        "draft": view,
        "published": null,
    });
    let mut exit = 0;
    if !a.json {
        let verb = if created {
            "created"
        } else if out["draft"]["unchanged"] == json!(true) {
            "unchanged"
        } else {
            "saved"
        };
        println!(
            "{verb} {slug} ({rb}) draft revision {}",
            revision.unwrap_or_default()
        );
    }
    if problems > 0 {
        eprintln!("draft has {problems} problem(s); not published");
        exit = 2;
    } else if !a.draft {
        let (_, published) = ops::publish(b, &rb, revision, &format!("{key}-publish"))?;
        if !a.json {
            println!(
                "published {slug} version {} ({})",
                published["runbook_version"],
                published["runbook_version_id"].as_str().unwrap_or("")
            );
        }
        out["published"] = published;
    }
    if problems == 0
        && let Some(warning) = step_cap_warning(b, &source)
    {
        eprintln!("warning: {warning}");
        out["step_cap_warning"] = json!(warning);
    }
    if a.json {
        print_json(&out);
    }
    Ok(exit)
}

/// The step cap warning (2026-11-17): a runbook whose worst case is over
/// this server's `limits.step_runs` still publishes, but a run that reaches
/// the cap stops. `None` when there is no cap, the budget is unknown, or it
/// fits. `/v1/meta` failing is not an error here.
fn step_cap_warning(b: &mut dyn Backend, source: &str) -> Option<String> {
    let budget = crate::validate::validate_source(source).worst_case_step_runs?;
    let meta = b
        .call(runbook_mcp::backend::HttpCall::get("/v1/meta"))
        .ok()?
        .body;
    let cap = meta["limits"]["step_runs"].as_u64()?;
    (budget > cap).then(|| {
        format!(
            "worst case {budget} step runs is over this server's limit of {cap}: a run that reaches it stops"
        )
    })
}

fn shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Any runbook of the project, to confirm publish permission in it.
fn any_runbook(b: &mut dyn Backend, project: &str) -> Result<Option<String>, BackendError> {
    let page = ops::list_runbooks(b, project, Some(1), None)?;
    Ok(page["items"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["runbook_id"].as_str().map(str::to_owned)))
}

fn publish(b: &mut dyn Backend, a: &PublishArgs) -> Result<i32, BackendError> {
    let rb = ops::resolve_runbook(b, &a.runbook, a.project.as_deref())?;
    let key = base_key(&a.idempotency_key)?;
    let (revision, mut published) = ops::publish(b, &rb, a.revision, &key)?;
    let warning = ops::get_draft(b, &rb)
        .ok()
        .and_then(|draft| step_cap_warning(b, &draft.source));
    if let Some(warning) = &warning {
        eprintln!("warning: {warning}");
        published["step_cap_warning"] = json!(warning);
    }
    if a.json {
        print_json(&published);
    } else {
        println!(
            "published draft revision {revision} of {rb} as version {} ({})",
            published["runbook_version"],
            published["runbook_version_id"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}

/// Whether a `diff` argument names a local file rather than a runbook.
/// An existing path is a file (a directory is a usage error); a missing one
/// that looks like a path is a usage error rather than a runbook name.
fn diff_file(arg: &str) -> Result<Option<PathBuf>, BackendError> {
    let path = PathBuf::from(arg);
    if path.is_dir() {
        return Err(BackendError::Usage(format!(
            "'{arg}' is a directory; diff takes a runbook file or a runbook name"
        )));
    }
    if path.exists() {
        return Ok(Some(path));
    }
    let looks_like_path = arg.contains(['/', '\\'])
        || [".yaml", ".yml", ".json"]
            .iter()
            .any(|ext| arg.ends_with(ext));
    if looks_like_path {
        return Err(BackendError::Usage(format!("'{arg}' does not exist")));
    }
    Ok(None)
}

/// `<runbook>[@<n|rbv_id|latest>]` as (runbook, selector).
fn diff_ref(arg: &str) -> Result<(String, String), BackendError> {
    let (name, selector) = match arg.split_once('@') {
        Some((name, "latest")) => (name, "latest".to_owned()),
        Some((name, raw)) => {
            let parsed =
                crate::selector::parse_version_selector(raw).map_err(BackendError::Usage)?;
            (
                name,
                match parsed {
                    Value::Number(n) => n.to_string(),
                    other => other.as_str().unwrap_or_default().to_owned(),
                },
            )
        }
        None => (arg, "latest".to_owned()),
    };
    ops::check_segment("runbook", name)?;
    Ok((name.to_owned(), selector))
}

/// The canonical spec of a source.
fn canonical(source: &str, what: &str) -> Result<Value, BackendError> {
    runbook_spec::parse(source)
        .map(|spec| spec.source_json().clone())
        .map_err(|diagnostics| {
            for d in &diagnostics {
                eprintln!("{what}:{}:{}: {} {}", d.line, d.column, d.code, d.path);
            }
            BackendError::Usage(format!("{what} is not a valid runbook spec"))
        })
}

struct Side {
    label: String,
    source: String,
    info: Value,
}

fn published(
    b: &mut dyn Backend,
    (name, selector): &(String, String),
    project: Option<&str>,
) -> Result<Side, BackendError> {
    let rb = ops::resolve_runbook(b, name, project)?;
    let view = ops::get_version(b, &rb, selector)?;
    let at = match view["runbook_version"].as_u64() {
        Some(n) => n.to_string(),
        None => view["runbook_version_id"]
            .as_str()
            .unwrap_or(selector)
            .to_owned(),
    };
    Ok(Side {
        label: format!("{name}@{at}"),
        source: view["source"].as_str().unwrap_or_default().to_owned(),
        info: json!({
            "runbook_id": view["runbook_id"],
            "runbook_version_id": view["runbook_version_id"],
            "runbook_version": view["runbook_version"],
        }),
    })
}

/// CLI3 `runbook diff`: a local file against the published runbook it
/// names (`metadata.id`; latest unless a version is given), or one
/// published version against another (default: the latest). Every argument
/// is checked before the first request.
fn diff(b: &mut dyn Backend, a: &DiffArgs, store: Option<&StateDir>) -> Result<i32, BackendError> {
    let project = a.project.as_deref();
    let (old_ref, new_side) = match diff_file(&a.first)? {
        Some(path) => {
            if let Some(store) = store {
                store.refuse_inside(&path).map_err(BackendError::Usage)?;
            }
            let source = std::fs::read_to_string(&path).map_err(|e| {
                BackendError::Usage(format!("cannot read '{}': {e}", path.display()))
            })?;
            let label = path.display().to_string();
            canonical(&source, &label)?;
            let old_ref = match &a.second {
                Some(r) => diff_ref(r)?,
                None => {
                    let slug = metadata_id(&source).ok_or_else(|| {
                        BackendError::Usage(format!(
                            "'{label}' has no metadata.id; it names the runbook"
                        ))
                    })?;
                    (slug, "latest".to_owned())
                }
            };
            let side = Side {
                info: json!({"file": label}),
                label,
                source,
            };
            (old_ref, Some(side))
        }
        None => (diff_ref(&a.first)?, None),
    };
    let new_ref = match (&new_side, &a.second) {
        (Some(_), _) => None,
        (None, Some(r)) => Some(diff_ref(r)?),
        (None, None) => Some((old_ref.0.clone(), "latest".to_owned())),
    };
    let slug_needs_project = |r: &(String, String)| !ops::is_runbook_id(&r.0) && project.is_none();
    if slug_needs_project(&old_ref) || new_ref.as_ref().is_some_and(slug_needs_project) {
        return Err(BackendError::Usage(
            "a runbook slug needs --project (or use its rb_ ID)".into(),
        ));
    }
    let old = published(b, &old_ref, project)?;
    let new = match (new_side, new_ref) {
        (Some(side), _) => side,
        (None, Some(r)) => published(b, &r, project)?,
        (None, None) => unreachable!("one side is a file or a reference"),
    };
    let mut out = crate::diff::structured(
        &canonical(&old.source, &old.label)?,
        &canonical(&new.source, &new.label)?,
    );
    let text = a
        .text
        .then(|| crate::diff::unified(&old.source, &new.source, &old.label, &new.label));
    if a.json {
        let side = |s: &Side| {
            let mut v = s.info.clone();
            v["label"] = json!(s.label);
            v
        };
        out["old"] = side(&old);
        out["new"] = side(&new);
        if let Some(text) = text {
            out["text"] = json!(text);
        }
        print_json(&out);
    } else {
        print!("{}", crate::diff::render(&out, &old.label, &new.label));
        if let Some(text) = text {
            println!();
            print!("{text}");
        }
    }
    Ok(0)
}
