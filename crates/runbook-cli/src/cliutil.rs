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

/// Budget blocks recorded by the runner scheduler, for human-readable run details.
pub fn runner_budget_lines(view: &serde_json::Value) -> Vec<String> {
    use crate::memory::clean;
    view["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| {
            let runner = &step["runner"];
            let reason = runner["waiting_reason"].as_str()?;
            let scope = reason.strip_prefix("limit:")?;
            let path = clean(step["path"].as_str().unwrap_or("unknown"));
            let detail = runner["waiting_detail"]
                .as_str()
                .map(clean)
                .unwrap_or_else(|| format!("{} limit reached", clean(scope)));
            Some(format!("step {path}: {detail}"))
        })
        .collect()
}

/// R1-D2: what runner agents did on the run's approvals, for `run get`: an
/// agent's decision, the agent reviewing now (people can still decide),
/// its abstentions, and when an agent may decide. Agent-written text is
/// escaped for the terminal. `now_ms` dates "may decide in …".
pub fn approval_agent_lines(view: &serde_json::Value, now_ms: i64) -> Vec<String> {
    use crate::memory::clean;
    let mut lines = vec![];
    for step in view["steps"].as_array().into_iter().flatten() {
        if step["kind"].as_str() != Some("approval") {
            continue;
        }
        let path = clean(step["path"].as_str().unwrap_or("unknown"));
        let actor = &step["actor"];
        if actor["kind"].as_str() == Some("agent") && step["status"].as_str() == Some("succeeded") {
            let comment = step["outputs"]["comment"].as_str().unwrap_or("");
            lines.push(format!(
                "approval {path}: decided by agent {}{}: {}{}",
                clean(actor["name"].as_str().unwrap_or("unknown")),
                actor["model"]
                    .as_str()
                    .map(|m| format!(" ({})", clean(m)))
                    .unwrap_or_default(),
                clean(step["outputs"]["decision"].as_str().unwrap_or("")),
                if comment.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", clean(comment))
                },
            ));
        }
        let Some(runner) = step.get("runner").filter(|r| r.is_object()) else {
            continue;
        };
        let name = clean(runner["agent_name"].as_str().unwrap_or("unknown"));
        let notes = runner["agent_notes"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        for note in notes {
            lines.push(format!(
                "approval {path}: agent {}{} abstained: {}",
                clean(note["agent_name"].as_str().unwrap_or(&name)),
                note["model"]
                    .as_str()
                    .map(|m| format!(" ({})", clean(m)))
                    .unwrap_or_default(),
                clean(note["reason"].as_str().unwrap_or("")),
            ));
        }
        let open = matches!(step["status"].as_str(), Some("waiting" | "ready"));
        if !open {
            continue;
        }
        if let Some(lease) = runner.get("decision_lease").filter(|l| l.is_object()) {
            lines.push(format!(
                "approval {path}: agent {} is reviewing; people can still decide",
                clean(lease["agent_name"].as_str().unwrap_or(&name)),
            ));
        } else if notes.is_empty()
            && let Some(ready) = runner["decision_ready_at_ms"].as_i64()
        {
            lines.push(if ready > now_ms {
                format!(
                    "approval {path}: agent {name} may decide in {} if no one has",
                    short_duration(ready - now_ms)
                )
            } else {
                format!("approval {path}: agent {name} may decide now")
            });
        }
    }
    lines
}

/// `45s`, `15m`, `2h 5m`: a short, rounded-down duration.
fn short_duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ if seconds % 3600 < 60 => format!("{}h", seconds / 3600),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
mod approval_agent_tests {
    use super::approval_agent_lines;
    use serde_json::json;

    fn approval(status: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut step = json!({"path": "release_gate", "kind": "approval", "status": status});
        for (key, value) in extra.as_object().unwrap() {
            step[key] = value.clone();
        }
        json!({"steps": [step, {"path": "deploy", "kind": "task", "status": "ready",
            "runner": {"agent_id": "agt_1", "agent_name": "deployer"}}]})
    }

    #[test]
    fn says_when_an_agent_may_decide() {
        let runner = json!({"runner": {"agent_id": "agt_1", "agent_name": "release-reviewer",
            "decision_ready_at_ms": 1_000 + 15 * 60_000}});
        assert_eq!(
            approval_agent_lines(&approval("waiting", runner.clone()), 1_000),
            ["approval release_gate: agent release-reviewer may decide in 15m if no one has"]
        );
        assert_eq!(
            approval_agent_lines(&approval("waiting", runner), 100_000_000),
            ["approval release_gate: agent release-reviewer may decide now"]
        );
    }

    #[test]
    fn shows_the_agent_reviewing_and_its_abstentions() {
        let lease = json!({"runner": {"agent_id": "agt_1", "agent_name": "release-reviewer",
            "decision_ready_at_ms": 0,
            "decision_lease": {"lease_id": "clm_1", "agent_name": "release-reviewer"}}});
        assert_eq!(
            approval_agent_lines(&approval("waiting", lease), 1),
            ["approval release_gate: agent release-reviewer is reviewing; people can still decide"]
        );
        let abstained = json!({"runner": {"agent_id": "agt_1", "agent_name": "release-reviewer",
            "decision_ready_at_ms": 0,
            "agent_notes": [{"lease_id": "clm_1", "principal": "runner:rin_1",
                "agent_name": "release-reviewer", "model": "claude-sonnet",
                "reason": "No canary data\n\u{1b}[31mforged", "at_ms": 1}]}});
        assert_eq!(
            approval_agent_lines(&approval("waiting", abstained), 1),
            [
                "approval release_gate: agent release-reviewer (claude-sonnet) abstained: No canary data\\u{a}\\u{1b}[31mforged"
            ]
        );
    }

    #[test]
    fn shows_the_agent_decision() {
        let decided = json!({
            "actor": {"kind": "agent", "name": "release-reviewer", "model": "claude-sonnet",
                "principal_id": "runner:rin_1", "session_id": "clm_1"},
            "outputs": {"decision": "ship", "comment": "Canary healthy [call 1]",
                "decided_by": "runner:rin_1"},
            "runner": {"agent_id": "agt_1", "agent_name": "release-reviewer",
                "decision_ready_at_ms": 0}});
        assert_eq!(
            approval_agent_lines(&approval("succeeded", decided), 1),
            [
                "approval release_gate: decided by agent release-reviewer (claude-sonnet): ship (Canary healthy [call 1])"
            ]
        );
    }
}
