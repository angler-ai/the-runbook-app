//! Archive CLI commands (PRD §14; plan §2.5 ARC4).
//!
//! - `runbook archive list <runbook> [--project <p>] [--from YYYY-MM-DD] [--to YYYY-MM-DD] [--json]`
//! - `runbook archive download <runbook> [--project <p>] --date YYYY-MM-DD [--include-evidence] [--out FILE]`
//! - `runbook archive download <runbook> [--project <p>] --from YYYY-MM-DD --to YYYY-MM-DD [--include-evidence] [--out FILE]`

use crate::agent::Env;
use crate::api::{ApiError, ArchiveDownloadError, Request};
use crate::cliutil::{Exit, OK, api_failed_404, client, fail, resolve_runbook_id, text, usage};
use clap::{Args, Subcommand};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum ArchiveCommands {
    #[command(about = "List a runbook's daily archive files")]
    List(ArchiveListArgs),
    #[command(about = "Download daily archive files or range bundle")]
    Download(ArchiveDownloadArgs),
}

#[derive(Args, Debug)]
pub struct ArchiveListArgs {
    #[arg(help = "Runbook ID or slug")]
    pub runbook: String,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (required when <runbook> is a slug)"
    )]
    pub project: Option<String>,
    #[arg(long, help = "First UTC day, inclusive (YYYY-MM-DD)")]
    pub from: Option<String>,
    #[arg(long, help = "Last UTC day, inclusive (YYYY-MM-DD)")]
    pub to: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ArchiveDownloadArgs {
    #[arg(help = "Runbook ID or slug")]
    pub runbook: String,
    #[arg(
        long,
        env = "RUNBOOK_PROJECT",
        help = "Project ID or slug (required when <runbook> is a slug)"
    )]
    pub project: Option<String>,
    #[arg(long, help = "Single UTC day to download (YYYY-MM-DD)")]
    pub date: Option<String>,
    #[arg(
        long,
        help = "First UTC day for bundle download, inclusive (YYYY-MM-DD)"
    )]
    pub from: Option<String>,
    #[arg(
        long,
        help = "Last UTC day for bundle download, inclusive (YYYY-MM-DD)"
    )]
    pub to: Option<String>,
    #[arg(long, help = "Include held evidence objects in archive bundle")]
    pub include_evidence: bool,
    #[arg(
        long,
        help = "Output file path (default: sensible filename based on runbook and date range)"
    )]
    pub out: Option<PathBuf>,
}

pub fn handle_archive(env: &Env, command: ArchiveCommands) -> i32 {
    let result = match command {
        ArchiveCommands::List(a) => list(env, a),
        ArchiveCommands::Download(a) => download(env, a),
    };
    match result {
        Ok(code) => code,
        Err(code) => code,
    }
}

fn list(env: &Env, a: ArchiveListArgs) -> Exit<i32> {
    let rb_id = resolve_runbook_id(env, &a.runbook, a.project.as_deref(), "archive", a.json)?;
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(f) = &a.from {
        query.push(("from".into(), f.clone()));
    }
    if let Some(t) = &a.to {
        query.push(("to".into(), t.clone()));
    }
    let path = format!("/v1/runbooks/{rb_id}/archives");
    let reply = client(env)?
        .send(&Request {
            method: "GET",
            path: &path,
            query: &query,
            ..Default::default()
        })
        .map_err(|e| api_failed_404(e, a.json))?;

    if a.json {
        crate::cliutil::print_json(&reply.body);
        return Ok(OK);
    }

    if let Some(items) = reply.body["items"].as_array() {
        for item in items {
            let date = text(item, "date");
            let runs = item["run_count"].as_u64().unwrap_or(0);
            let size = item["size_bytes"].as_u64().unwrap_or(0);
            let sha256 = text(item, "sha256");
            let stale = item["stale"].as_bool().unwrap_or(false);
            if stale {
                println!("{date}  {runs} runs  {size}B  {sha256}  stale");
            } else {
                println!("{date}  {runs} runs  {size}B  {sha256}");
            }
        }
    }
    Ok(OK)
}

fn download(env: &Env, a: ArchiveDownloadArgs) -> Exit<i32> {
    let rb_id = resolve_runbook_id(env, &a.runbook, a.project.as_deref(), "archive", false)?;

    let (path, query, default_filename) = match (&a.date, &a.from, &a.to) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err(usage("--date cannot be combined with --from / --to"));
        }
        (Some(date), None, None) => {
            let mut query = Vec::new();
            if a.include_evidence {
                query.push(("include_evidence", "true"));
            }
            let default_name = if a.include_evidence {
                format!("{rb_id}-{date}-bundle.zip")
            } else {
                format!("{rb_id}-{date}.zip")
            };
            (
                format!("/v1/runbooks/{rb_id}/archives/{date}"),
                query,
                default_name,
            )
        }
        (None, Some(from), Some(to)) => {
            let mut query = vec![
                ("format", "zip"),
                ("from", from.as_str()),
                ("to", to.as_str()),
            ];
            if a.include_evidence {
                query.push(("include_evidence", "true"));
            }
            let default_name = format!("{rb_id}-{from}-{to}.zip");
            (
                format!("/v1/runbooks/{rb_id}/archives"),
                query,
                default_name,
            )
        }
        (None, Some(_), None) => {
            return Err(usage("--from requires --to"));
        }
        (None, None, Some(_)) => {
            return Err(usage("--to requires --from"));
        }
        (None, None, None) => {
            return Err(usage(
                "pass either --date <YYYY-MM-DD> or both --from and --to",
            ));
        }
    };

    let client = client(env)?;
    let download = match client.download_archive(&path, &query) {
        Ok(d) => d,
        Err(ArchiveDownloadError::NotFound(e)) => {
            eprintln!("{e}");
            return Err(4);
        }
        Err(ArchiveDownloadError::RewritePending { retry_after }) => {
            if let Some(secs) = retry_after {
                eprintln!(
                    "error: archive rewrite pending: retry after {secs}s (the daily job will rewrite it)"
                );
            } else {
                eprintln!("error: archive rewrite pending; retry later");
            }
            return Err(1);
        }
        Err(ArchiveDownloadError::Api(e)) => {
            let code = match &e {
                ApiError::Problem { status: 404, .. } => 4,
                _ => e.exit_code(),
            };
            eprintln!("{e}");
            return Err(code);
        }
    };

    let explicit_out = a.out.is_some();
    let target = if let Some(out) = a.out {
        out
    } else {
        let chosen_name = download
            .filename
            .as_deref()
            .filter(|name| is_safe_server_name(name))
            .unwrap_or(&default_filename);
        PathBuf::from(chosen_name)
    };

    if target == Path::new("-") {
        if std::io::stdout().is_terminal() {
            return Err(usage(
                "refusing to write binary archive to a terminal: pass a file path or redirect stdout",
            ));
        }
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(&download.bytes)
            .and_then(|()| stdout.flush())
            .map_err(|e| fail(format!("could not write archive to stdout: {e}")))?;
        return Ok(OK);
    }

    if !explicit_out {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(file) => drop(file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(fail("file exists; pass --out to choose a path"));
            }
            Err(e) => {
                return Err(fail(format!("could not create {}: {e}", target.display())));
            }
        }
    }

    let parent = target.parent().unwrap_or(Path::new(""));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("archive.zip");
    let hex = crate::session::random_hex().map_err(fail)?;
    let temp_name = format!(".{file_name}.{}.{hex}.tmp", std::process::id());
    let temp_path = parent.join(temp_name);

    let write_res = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(&download.bytes)?;
        file.flush()?;
        drop(file);
        std::fs::rename(&temp_path, &target)?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&temp_path);
        if !explicit_out {
            let _ = std::fs::remove_file(&target);
        }
        return Err(fail(format!(
            "could not write archive {}: {e}",
            target.display()
        )));
    }

    eprintln!("downloaded {}", target.display());
    Ok(OK)
}

fn is_safe_server_name(name: &str) -> bool {
    if !name.ends_with(".zip") {
        return false;
    }
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 201 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    for &b in &bytes[1..] {
        if !b.is_ascii_alphanumeric() && b != b'.' && b != b'_' && b != b'-' {
            return false;
        }
    }
    true
}
