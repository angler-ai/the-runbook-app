//! `runbook sync push` / `runbook sync pull` (2026-11-17, PRD §12's
//! open-source path): a repository folder of runbook sources is the source
//! of truth for a project. `push` publishes the folder as one batch from CI
//! (`POST /v1/projects/{p}/sync`; any invalid file refuses the batch);
//! `pull` writes each runbook's latest published source to the folder, to
//! start a repository from runbooks written in the app.
//!
//! A folder is `<dir>/*.yaml` (and `*.yml`) for one project. With
//! `--each-project`, every subfolder of `<dir>` is a project named by its
//! slug: `runbooks/<project>/<runbook>.yaml`, each pushed with
//! `RUNBOOK_TOKEN_<PROJECT>` when set (a service token is per project),
//! else `RUNBOOK_TOKEN`.
use crate::agent::Env;
use crate::api::{ApiClient, Request};
use crate::cliutil::{Exit, OK, USAGE, api_failed_json, print_json, usage};
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum SyncCommands {
    #[command(
        about = "Publish a folder of runbook sources to its project as one batch (from CI); any invalid file refuses the batch"
    )]
    Push(SyncPushArgs),
    #[command(
        about = "Write each runbook's latest published source to a folder (to start a repository from the app)"
    )]
    Pull(SyncPullArgs),
}

#[derive(Args, Debug)]
pub struct SyncPushArgs {
    #[arg(help = "Folder of <runbook>.yaml files (or of project folders, with --each-project)")]
    pub dir: PathBuf,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (default: the context's project, else `default`)"
    )]
    pub project: Option<String>,
    #[arg(
        long,
        help = "Each subfolder of DIR is a project, named by its slug (--project is ignored)"
    )]
    pub each_project: bool,
    #[arg(long, help = "Validate and show what would change; change nothing")]
    pub dry_run: bool,
    #[arg(
        long,
        help = "Archive Git-managed runbooks that have no file in the folder"
    )]
    pub prune: bool,
    #[arg(
        long,
        help = "Commit recorded on new versions (default: $GITHUB_SHA, else `git rev-parse HEAD`)"
    )]
    pub commit: Option<String>,
    #[arg(
        long,
        help = "Branch recorded on new versions (default: $GITHUB_HEAD_REF or $GITHUB_REF_NAME, else the checked-out branch)"
    )]
    pub branch: Option<String>,
    #[arg(
        long = "idempotency-key",
        help = "Idempotency key (one per project is derived from it)"
    )]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output the result as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct SyncPullArgs {
    #[arg(help = "Folder to write <runbook>.yaml files into (created if missing)")]
    pub dir: PathBuf,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (default: the context's project, else `default`)"
    )]
    pub project: Option<String>,
    #[arg(
        long,
        help = "Every project you can read, each into DIR/<project-slug>/ (--project is ignored)"
    )]
    pub each_project: bool,
    #[arg(long, help = "Output what was written as JSON")]
    pub json: bool,
}

pub fn handle(env: &Env, command: SyncCommands) -> i32 {
    match command {
        SyncCommands::Push(a) => push(env, a).unwrap_or_else(|e| e),
        SyncCommands::Pull(a) => pull(env, a).unwrap_or_else(|e| e),
    }
}

fn project_ref(given: Option<&str>) -> Exit<String> {
    let p = given.unwrap_or("default");
    crate::selector::validate_project_ref(p).map_err(usage)?;
    Ok(p.to_owned())
}

/// The runbook sources directly in `dir`, sorted by name.
fn sources(dir: &Path) -> Exit<Vec<(String, String)>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| usage(format!("cannot read '{}': {e}", dir.display())))?;
    let mut files = vec![];
    for entry in entries {
        let path = entry
            .map_err(|e| usage(format!("cannot read '{}': {e}", dir.display())))?
            .path();
        let yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yaml" || e == "yml");
        if path.is_file() && yaml {
            let source = std::fs::read_to_string(&path)
                .map_err(|e| usage(format!("cannot read '{}': {e}", path.display())))?;
            files.push((path.to_string_lossy().into_owned(), source));
        }
    }
    files.sort();
    Ok(files)
}

/// The project folders under `dir` (`--each-project`), sorted.
fn project_dirs(dir: &Path) -> Exit<Vec<(String, PathBuf)>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| usage(format!("cannot read '{}': {e}", dir.display())))?;
    let mut out = vec![];
    for entry in entries {
        let path = entry
            .map_err(|e| usage(format!("cannot read '{}': {e}", dir.display())))?
            .path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        if name.starts_with('.') {
            continue;
        }
        crate::selector::validate_project_ref(&name)
            .map_err(|e| usage(format!("'{}' is not a project slug: {e}", path.display())))?;
        out.push((name, path));
    }
    out.sort();
    Ok(out)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !text.is_empty() && text != "HEAD").then_some(text)
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn commit_and_branch(a: &SyncPushArgs) -> (Option<String>, Option<String>) {
    let commit = a
        .commit
        .clone()
        .or_else(|| non_empty("GITHUB_SHA"))
        .or_else(|| git(&a.dir, &["rev-parse", "HEAD"]));
    let branch = a
        .branch
        .clone()
        .or_else(|| non_empty("GITHUB_HEAD_REF"))
        .or_else(|| non_empty("GITHUB_REF_NAME"))
        .or_else(|| git(&a.dir, &["rev-parse", "--abbrev-ref", "HEAD"]));
    (commit, branch)
}

fn push(env: &Env, a: SyncPushArgs) -> Exit<i32> {
    let projects: Vec<(String, PathBuf)> = if a.each_project {
        let dirs = project_dirs(&a.dir)?;
        if dirs.is_empty() {
            return Err(usage(format!(
                "'{}' has no project folders",
                a.dir.display()
            )));
        }
        dirs
    } else {
        vec![(project_ref(a.project.as_deref())?, a.dir.clone())]
    };
    let (commit, branch) = commit_and_branch(&a);
    let key = crate::cliutil::idempotency_key(&a.idempotency_key)?;
    let mut exit = OK;
    let mut results = vec![];
    for (project, dir) in &projects {
        let files = sources(dir)?;
        let mut body = json!({
            "files": files.iter().map(|(path, source)| json!({"path": path, "source": source})).collect::<Vec<_>>(),
            "dry_run": a.dry_run,
            "prune": a.prune,
        });
        if let Some(commit) = &commit {
            body["commit_sha"] = json!(commit);
        }
        if let Some(branch) = &branch {
            body["branch"] = json!(branch);
        }
        let client = project_client(env, project)?;
        let path = format!("/v1/projects/{project}/sync");
        let project_key = format!("{key}-{project}");
        let (status, reply) = client
            .send_raw(&Request {
                method: "POST",
                path: &path,
                body: Some(&body),
                idempotency_key: Some(&project_key),
                ..Default::default()
            })
            .map_err(|e| api_failed_json(e, a.json))?;
        let code = if (200..300).contains(&status) {
            if !a.json {
                print_result(project, &reply);
            }
            OK
        } else {
            report_problem(project, status, &reply, a.json)
        };
        if exit == OK {
            exit = code;
        }
        results.push(json!({"project": project, "status": status, "body": reply}));
    }
    if a.json {
        print_json(&json!({"projects": results}));
    }
    Ok(exit)
}

/// The environment variable holding one project's token for
/// `--each-project`: `RUNBOOK_TOKEN_` and the slug in upper case with `-`
/// as `_` (`data-pipelines` → `RUNBOOK_TOKEN_DATA_PIPELINES`). A service
/// token acts for one project, so a repository of several needs one each.
pub fn project_token_var(project: &str) -> String {
    format!(
        "RUNBOOK_TOKEN_{}",
        project.to_ascii_uppercase().replace('-', "_")
    )
}

fn project_client(env: &Env, project: &str) -> Exit<ApiClient> {
    match non_empty(&project_token_var(project)) {
        Some(token) => Ok(ApiClient::new(env.host.clone(), token)),
        None => crate::cliutil::client(env),
    }
}

fn print_result(project: &str, reply: &Value) {
    let runbooks = reply["runbooks"].as_array().cloned().unwrap_or_default();
    let dry = reply["dry_run"] == json!(true);
    println!(
        "{project}: {} runbook{}{}",
        runbooks.len(),
        if runbooks.len() == 1 { "" } else { "s" },
        if dry {
            " (dry run: nothing changed)"
        } else {
            ""
        }
    );
    for r in runbooks {
        let version = r["runbook_version"]
            .as_u64()
            .map(|v| format!(" v{v}"))
            .unwrap_or_default();
        println!(
            "  {:<9} {}{}  {}",
            r["action"].as_str().unwrap_or(""),
            r["slug"].as_str().unwrap_or(""),
            version,
            r["path"].as_str().unwrap_or("")
        );
    }
}

/// A refused sync: each file's errors (`sync.invalid`), or the problem.
/// Exit 2 for invalid runbooks and usage problems, 4 for limits, else 1.
fn report_problem(project: &str, status: u16, body: &Value, json: bool) -> i32 {
    let code = body["code"].as_str().unwrap_or("error");
    if !json {
        if let Some(files) = body["files"].as_array() {
            eprintln!("error: {project}: nothing was published; fix these files:");
            for file in files {
                for d in file["errors"].as_array().cloned().unwrap_or_default() {
                    let at = match (d["line"].as_u64(), d["column"].as_u64()) {
                        (Some(line), Some(column)) => format!(":{line}:{column}"),
                        (Some(line), None) => format!(":{line}"),
                        _ => String::new(),
                    };
                    eprintln!(
                        "  {}{at}: {} {} {}",
                        file["path"].as_str().unwrap_or(""),
                        d["code"].as_str().unwrap_or(""),
                        d["path"].as_str().unwrap_or(""),
                        d["message"].as_str().unwrap_or("")
                    );
                }
            }
        } else {
            let detail = body["detail"].as_str().unwrap_or(code);
            eprintln!("error: {project}: {code}: {detail}");
        }
    }
    match status {
        422 if code == "sync.invalid" || code.starts_with("spec.") => USAGE,
        400 | 413 | 422 => USAGE,
        429 => 4,
        _ => 1,
    }
}

fn pull(env: &Env, a: SyncPullArgs) -> Exit<i32> {
    let client = crate::cliutil::client(env)?;
    let projects: Vec<(String, PathBuf)> = if a.each_project {
        let me = client
            .get_json("/v1/me")
            .map_err(|e| api_failed_json(e, a.json))?;
        let org = me["org"]["id"]
            .as_str()
            .ok_or_else(|| usage("this credential has no organization"))?
            .to_owned();
        let listed = client
            .get_json(&format!("/v1/orgs/{org}/projects"))
            .map_err(|e| api_failed_json(e, a.json))?;
        let items = listed
            .as_array()
            .cloned()
            .or_else(|| listed["items"].as_array().cloned())
            .unwrap_or_default();
        items
            .iter()
            .filter_map(|p| p["slug"].as_str())
            .map(|slug| (slug.to_owned(), a.dir.join(slug)))
            .collect()
    } else {
        vec![(project_ref(a.project.as_deref())?, a.dir.clone())]
    };
    let mut written = vec![];
    for (project, dir) in &projects {
        let runbooks = match list_runbooks(&client, project) {
            Ok(runbooks) => runbooks,
            // Every project of the org is listed; one you can't read is
            // skipped, not an error.
            Err(crate::api::ApiError::Problem {
                status: 403 | 404, ..
            }) if a.each_project => {
                if !a.json {
                    eprintln!("skipped project {project}: not readable with this credential");
                }
                continue;
            }
            Err(e) => return Err(api_failed_json(e, a.json)),
        };
        for rb in runbooks {
            if rb["archived"] == json!(true) {
                continue;
            }
            let slug = rb["slug"].as_str().unwrap_or_default().to_owned();
            let id = rb["runbook_id"].as_str().unwrap_or_default().to_owned();
            let Some(version) = rb["latest_version"].as_u64() else {
                if !a.json {
                    eprintln!("skipped {project}/{slug}: never published");
                }
                continue;
            };
            let read = client
                .get_json(&format!("/v1/runbooks/{id}/versions/{version}"))
                .map_err(|e| api_failed_json(e, a.json))?;
            let source = read["source"].as_str().unwrap_or_default();
            std::fs::create_dir_all(dir)
                .map_err(|e| usage(format!("cannot create '{}': {e}", dir.display())))?;
            let path = dir.join(format!("{slug}.yaml"));
            std::fs::write(&path, source)
                .map_err(|e| usage(format!("cannot write '{}': {e}", path.display())))?;
            if !a.json {
                println!("wrote {} (v{version})", path.display());
            }
            written.push(json!({
                "project": project,
                "slug": slug,
                "runbook_version": version,
                "path": path.to_string_lossy(),
            }));
        }
    }
    if a.json {
        print_json(&json!({"written": written}));
    }
    Ok(OK)
}

fn list_runbooks(client: &ApiClient, project: &str) -> Result<Vec<Value>, crate::api::ApiError> {
    let mut out = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let mut query = vec![("limit".to_owned(), "100".to_owned())];
        if let Some(c) = &cursor {
            query.push(("cursor".to_owned(), c.clone()));
        }
        let page = client
            .send(&Request {
                method: "GET",
                path: &format!("/v1/projects/{project}/runbooks"),
                query: &query,
                ..Default::default()
            })?
            .body;
        out.extend(page["items"].as_array().cloned().unwrap_or_default());
        match page["next_cursor"].as_str() {
            Some(next) if !next.is_empty() => cursor = Some(next.to_owned()),
            _ => return Ok(out),
        }
    }
}
