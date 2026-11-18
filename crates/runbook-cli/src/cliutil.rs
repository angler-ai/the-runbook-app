//! Shared plumbing for the C2 administration, token and webhook commands:
//! credential resolution, stable exit codes and output.
use crate::agent::Env;
use crate::api::{ApiClient, ApiError};
use crate::session::random_hex;
use serde_json::{Value, json};

/// Stable exit codes (PRD §10).
pub const OK: i32 = 0;
pub const ERROR: i32 = 1;
pub const USAGE: i32 = 2;

pub type Exit<T> = Result<T, i32>;

pub fn usage(msg: impl std::fmt::Display) -> i32 {
    eprintln!("error: {msg}");
    USAGE
}

pub fn fail(msg: impl std::fmt::Display) -> i32 {
    eprintln!("error: {msg}");
    ERROR
}

/// Report an API failure; 4 for a reached limit, otherwise 1.
pub fn api_failed(e: ApiError) -> i32 {
    match &e {
        ApiError::Problem { .. } => eprintln!("{e}"),
        ApiError::Transport(_) => eprintln!("error: {e}"),
    }
    e.exit_code()
}

/// The `--json` document for a failed call (F67): the problem and the
/// next action an agent should take, so stdout is never empty on failure.
pub fn error_json(e: &ApiError) -> Value {
    let next = match e {
        ApiError::Transport(_) => json!("retry"),
        ApiError::Problem { code, .. }
            if code.starts_with("claim.") || code == "step.not_claimed" =>
        {
            json!("reconcile")
        }
        ApiError::Problem { .. } if e.exit_code() == 4 => json!("wait"),
        ApiError::Problem { status, .. } if *status >= 500 => json!("retry"),
        ApiError::Problem { .. } => Value::Null,
    };
    let mut error = json!({"message": e.to_string()});
    if let ApiError::Problem { status, code, .. } = e {
        error["code"] = json!(code);
        error["status"] = json!(status);
    }
    json!({"error": error, "next_action": next})
}

/// [`api_failed`], plus the error document on stdout under `--json`.
pub fn api_failed_json(e: ApiError, json: bool) -> i32 {
    if json {
        print_json(&error_json(&e));
    }
    api_failed(e)
}

/// Report an API failure; 4 for not found (404) or rate/quota limit, otherwise 1.
pub fn api_failed_404(e: ApiError, json: bool) -> i32 {
    let exit_code = match &e {
        ApiError::Problem { status: 404, .. } => 4,
        ApiError::Problem { code, .. } if code.ends_with(".not_found") => 4,
        _ => e.exit_code(),
    };
    if json {
        print_json(&error_json(&e));
    }
    match &e {
        ApiError::Problem { .. } => eprintln!("{e}"),
        ApiError::Transport(_) => eprintln!("error: {e}"),
    }
    exit_code
}

/// Resolves a runbook argument (which may be an `rb_` ID or a slug) to an `rb_` ID.
/// If `raw` is a slug, `project` is required and is searched by paging `/v1/projects/{p}/runbooks`.
pub fn resolve_runbook_id(
    env: &Env,
    raw: &str,
    project: Option<&str>,
    command_name: &str,
    json: bool,
) -> Exit<runbook_model::RunbookId> {
    if let Ok(id) = raw.parse::<runbook_model::RunbookId>() {
        return Ok(id);
    }
    let (slug, version) = crate::selector::parse_runbook_ref(raw).map_err(usage)?;
    if version.is_some() {
        return Err(usage(format!(
            "version selectors (@...) are not supported for {command_name} commands"
        )));
    }
    let p = project.ok_or_else(|| {
        usage(format!(
            "'{raw}' is a slug; pass --project to look it up, or use the rb_ ID"
        ))
    })?;
    crate::selector::validate_project_ref(p).map_err(usage)?;

    let client = client(env)?;
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        let mut query = vec![("limit".to_string(), "100".to_string())];
        if let Some(c) = &cursor {
            query.push(("cursor".to_string(), c.clone()));
        }
        let reply = client
            .send(&crate::api::Request {
                method: "GET",
                path: &format!("/v1/projects/{p}/runbooks"),
                query: &query,
                ..Default::default()
            })
            .map_err(|e| api_failed_404(e, json))?;

        if let Some(items) = reply.body["items"].as_array() {
            for item in items {
                if item["slug"].as_str() == Some(&slug)
                    && let Some(id_str) =
                        item["runbook_id"].as_str().or_else(|| item["id"].as_str())
                    && let Ok(id) = id_str.parse::<runbook_model::RunbookId>()
                {
                    return Ok(id);
                }
            }
        }
        match reply.body["next_cursor"].as_str() {
            Some(next) if !next.is_empty() => cursor = Some(next.to_owned()),
            _ => break,
        }
    }
    let msg = format!("runbook '{slug}' not found in project '{p}'");
    if json {
        print_json(&error_json(&ApiError::Problem {
            status: 404,
            code: "runbook.not_found".into(),
            message: msg.clone(),
            retry_after: None,
        }));
    }
    eprintln!("error: {msg}");
    Err(4)
}

pub fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
    );
}

/// The given `--idempotency-key`, or a fresh one printed to stderr so a
/// retry can reuse it.
pub fn idempotency_key(given: &Option<String>) -> Exit<String> {
    match given {
        Some(k) => Ok(k.clone()),
        None => {
            let key = random_hex()
                .map_err(|_| usage("cannot generate idempotency key; pass --idempotency-key"))?;
            eprintln!("Idempotency-Key: {key}");
            Ok(key)
        }
    }
}

/// Parse a typed public ID (`usr_…`, `whk_…`), or fail as a usage error.
pub fn parse_id<T: std::str::FromStr + std::fmt::Display>(text: &str, what: &str) -> Exit<String> {
    text.parse::<T>()
        .map(|id| id.to_string())
        .map_err(|_| usage(format!("invalid {what} ID: '{text}'")))
}

/// A bearer client for user-level commands. `RUNBOOK_TOKEN` wins; a
/// missing credential is a usage error.
pub fn client(env: &Env) -> Exit<ApiClient> {
    match &env.token {
        Some(token) => Ok(ApiClient::new(env.host.clone(), token.clone())),
        None => Err(usage(
            "not logged in: run `runbook login --token-stdin` or set RUNBOOK_TOKEN",
        )),
    }
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn is_version_newer(min_ver: &str, cur_ver: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<u64>().ok())
            .collect()
    };
    let min = parse(min_ver);
    let cur = parse(cur_ver);
    min > cur
}

pub fn check_minimum_cli_version(host: &str, store: Option<&crate::credstore::StateDir>) {
    let norm_host = host.trim_end_matches('/');
    const TTL_MS: i64 = 24 * 60 * 60 * 1000;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let maybe_warn = |min_ver: &str| {
        let cur_ver = env!("CARGO_PKG_VERSION");
        if is_version_newer(min_ver, cur_ver)
            && !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!(
                "warning: server requires runbook CLI version >= {min_ver} (current: {cur_ver})"
            );
        }
    };

    let cached = store
        .and_then(|s| s.load_version_cache().ok())
        .unwrap_or_default();
    let entry = cached.get(norm_host);

    let needs_fetch = match entry {
        Some(e) => {
            let age = now_ms - e.checked_at_ms;
            e.checked_at_ms > now_ms || age >= TTL_MS
        }
        None => true,
    };

    if !needs_fetch {
        if let Some(min_ver) = entry.and_then(|e| e.minimum_cli_version.as_deref()) {
            maybe_warn(min_ver);
        }
        return;
    }

    let url = format!("{norm_host}/.well-known/runbook");
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(std::time::Duration::from_millis(500))
        .timeout_read(std::time::Duration::from_millis(500))
        .build();

    let resp = agent.get(&url).set("Accept", "application/json").call();
    let val: Option<Value> = match resp {
        Ok(r) if r.status() == 200 => r.into_json().ok(),
        _ => None,
    };

    let min_ver = val
        .as_ref()
        .and_then(|v| v.get("minimum_cli_version"))
        .and_then(Value::as_str)
        .map(String::from);

    if let Some(ref v) = min_ver {
        maybe_warn(v);
    }

    if let Some(s) = store {
        let mut cache = s.load_version_cache().unwrap_or_default();
        cache.insert(
            norm_host.to_string(),
            crate::credstore::CachedVersion {
                minimum_cli_version: min_ver,
                checked_at_ms: now_ms,
            },
        );
        let _ = s.save_version_cache(&cache);
    }
}

/// LANG4: the parent/child lines of an execution view, for `run get`: the
/// `call` step that started this run, and each `call` step's child run.
pub fn call_links(view: &serde_json::Value) -> Vec<String> {
    let mut lines = vec![];
    if let Some(parent) = view.get("parent").filter(|p| p.is_object()) {
        lines.push(format!(
            "called by {} (step {})",
            parent["execution_id"].as_str().unwrap_or("unknown"),
            parent["step"].as_str().unwrap_or("unknown"),
        ));
    }
    for step in view["steps"].as_array().into_iter().flatten() {
        let Some(call) = step.get("call").filter(|c| c.is_object()) else {
            continue;
        };
        let child = call["child_execution_id"].as_str().unwrap_or("unknown");
        let state = match (call["error"].as_str(), call["started"].as_bool()) {
            (Some(code), _) => format!(" ({code})"),
            (None, Some(false)) => " (starting)".into(),
            _ => String::new(),
        };
        lines.push(format!(
            "step {} runs {child}{state}",
            step["path"].as_str().unwrap_or("unknown")
        ));
    }
    lines
}
