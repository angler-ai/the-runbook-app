//! `runbook mcp serve` over stdio against a scripted fake API, and CLI/MCP
//! trace equivalence.
mod support;
use runbook_cli::credstore::{ExecutionCredential, StateDir};
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use support::*;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const CLAIM: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3";
const RB: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ4";
const RBV: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXYZ6";
const ACCESS: &str = "trb_exe_access-secret-one";
const REFRESH: &str = "trb_exr_refresh-secret-one";
const ACCESS2: &str = "trb_exe_access-secret-two";
const REFRESH2: &str = "trb_exr_refresh-secret-two";

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

/// Run one MCP session; returns the responses and the raw stdout.
fn mcp(host: &str, home: &Path, env: &[(&str, &str)], messages: &[Value]) -> (Vec<Value>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .args(["--host", host, "mcp", "serve"])
        .env_remove("RUNBOOK_TOKEN")
        .env_remove("RUNBOOK_HOST")
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for m in messages {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let raw = out(&o);
    let responses = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (responses, raw)
}

fn text(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn control(response: &Value) -> Value {
    serde_json::from_str(text(response).lines().next().unwrap()).unwrap()
}

fn brief(next: &str) -> Value {
    json!({"schema_revision": 1, "execution_id": EXE, "completed_steps": [], "decisions": [], "comments": [],
           "ready_work": [{"step_run_id": SR, "path": "check", "title": "Check", "actor_kind": "agent"}],
           "next_action": next})
}

#[test]
fn a_fresh_mcp_connection_round_trips_against_the_api() {
    let home = tmp();
    let (host, server) = serve(vec![reply(
        200,
        json!({"id": EXE, "status": "running", "phase": "active", "runbook_version_id": RBV,
               "runbook_version": 2, "inputs": {"service": "api"}, "fields": {},
               "resume_brief": brief("claim_ready_work")}),
    )]);
    let (responses, raw) = mcp(
        &host,
        home.path(),
        &[("RUNBOOK_TOKEN", PAT)],
        &[
            initialize(),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            call(2, "get_execution", json!({"execution_id": EXE})),
        ],
    );
    let seen = server.join().unwrap();
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "runbook");
    assert!(responses[1]["result"]["tools"].as_array().unwrap().len() >= 18);
    let c = control(&responses[2]);
    assert_eq!(c["execution_id"], EXE);
    assert_eq!(c["runbook_version"], 2);
    assert_eq!(c["next_action"], "claim_ready_work");
    assert_eq!(c["ready_work"][0]["step_run_id"], SR);
    assert_eq!(seen.len(), 1);
    assert!(request_line(&seen[0]).starts_with(&format!("GET /v1/executions/{EXE} ")));
    assert_eq!(
        header(&seen[0], "Authorization"),
        Some(format!("Bearer {PAT}").as_str())
    );
    assert!(!raw.contains(PAT));
}

#[test]
fn write_only_token_saves_a_draft_then_publish_returns_403_asking_a_human() {
    let home = tmp();
    let etag3 = format!("\"draft-{RB}-3\"");
    let etag4 = format!("\"draft-{RB}-4\"");
    let (host, server) = serve(vec![
        with_etag(
            200,
            json!({"runbook_id": RB, "source": "old", "revision": 3}),
            &etag3,
        ),
        with_etag(
            200,
            json!({"runbook_id": RB, "source": "new", "revision": 4, "diagnostics": []}),
            &etag4,
        ),
        with_etag(
            200,
            json!({"runbook_id": RB, "source": "new", "revision": 4}),
            &etag4,
        ),
        {
            let mut denied = problem(403, "auth.scope_missing");
            denied.body["next_action"] = json!("ask_human_to_publish");
            denied
        },
    ]);
    let (responses, _) = mcp(
        &host,
        home.path(),
        &[("RUNBOOK_TOKEN", PAT)],
        &[
            initialize(),
            call(
                1,
                "save_runbook_draft",
                json!({"runbook": RB, "source": "new", "expected_revision": 3}),
            ),
            call(2, "publish_runbook", json!({"runbook": RB, "revision": 4})),
        ],
    );
    let seen = server.join().unwrap();
    let saved = &responses[1];
    assert_eq!(saved["result"]["isError"], false, "{}", text(saved));
    assert_eq!(control(saved)["revision"], 4);
    assert!(request_line(&seen[1]).starts_with(&format!("PATCH /v1/runbooks/{RB}/draft")));
    assert_eq!(header(&seen[1], "If-Match"), Some(etag3.as_str()));
    assert_eq!(body_of(&seen[1]), json!({"source": "new"}));

    let published = &responses[2];
    assert_eq!(published["result"]["isError"], true);
    let c = control(published);
    assert_eq!(c["error"]["status"], 403);
    assert_eq!(c["next_action"], "ask_human_to_publish");
    assert!(c["hint"].as_str().unwrap().contains("Ask a human"));
    assert!(request_line(&seen[3]).starts_with(&format!("POST /v1/runbooks/{RB}/versions")));
    assert_eq!(header(&seen[3], "If-Match"), Some(etag4.as_str()));
    assert_eq!(seen.len(), 4);
}

#[test]
fn mcp_uses_and_refreshes_the_stored_pair_without_exposing_it() {
    let home = tmp();
    let store = StateDir::from_env(home.path().to_str(), None).unwrap();
    let (host, server) = serve(vec![
        reply(
            200,
            json!({"family_id": "fam", "generation": 2, "access_token": ACCESS2, "refresh_token": REFRESH2,
                   "expires_at_ms": i64::MAX, "refresh_expires_at_ms": i64::MAX}),
        ),
        reply(200, brief("claim_ready_work")),
    ]);
    store
        .save_execution(&ExecutionCredential {
            host: host.clone(),
            execution_id: EXE.into(),
            family_id: "fam".into(),
            generation: 1,
            access_token: ACCESS.into(),
            refresh_token: REFRESH.into(),
            expires_at_ms: 0,
            refresh_expires_at_ms: i64::MAX,
        })
        .unwrap();
    store.set_active(&host, EXE).unwrap();
    let (responses, raw) = mcp(
        &host,
        home.path(),
        &[],
        &[call(1, "get_next_steps", json!({"execution_id": EXE}))],
    );
    let seen = server.join().unwrap();
    assert_eq!(control(&responses[0])["next_action"], "claim_ready_work");
    assert!(request_line(&seen[0]).contains("/tokens/refresh"));
    assert_eq!(
        header(&seen[1], "Authorization"),
        Some(format!("Bearer {ACCESS2}").as_str())
    );
    for secret in [ACCESS, REFRESH, ACCESS2, REFRESH2] {
        assert!(!raw.contains(secret), "{secret} leaked");
    }
    assert_eq!(
        store.load_execution(EXE).unwrap().unwrap().refresh_token,
        REFRESH2
    );
}

/// What a request means: method, path and query, body, idempotency key,
/// precondition and credential.
fn trace(seen: &[String]) -> Vec<Value> {
    seen.iter()
        .map(|r| {
            json!({
                "line": request_line(r).rsplit_once(' ').map(|(l, _)| l).unwrap_or(""),
                "body": body_of(r),
                "idempotency_key": header(r, "Idempotency-Key"),
                "if_match": header(r, "If-Match"),
                "authorization": header(r, "Authorization"),
            })
        })
        .collect()
}

fn loop_script() -> Vec<Reply> {
    vec![
        reply(200, brief("claim_ready_work")),
        reply(200, json!({"id": CLAIM, "generation": 1})),
        reply(200, json!({"step_run_id": SR, "status": "succeeded"})),
        reply(
            200,
            json!({"schema_revision": 1, "execution_id": EXE, "completed_steps": [], "decisions": [],
                   "comments": [], "ready_work": [], "next_action": "exit"}),
        ),
    ]
}

#[test]
fn cli_and_mcp_send_the_same_requests_for_next_claim_and_report() {
    // CLI
    let home = tmp();
    let (host, server) = serve(loop_script());
    let env = [("RUNBOOK_TOKEN", PAT)];
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &env);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "claim",
            SR,
            "--session",
            "s1",
            "--agent-name",
            "claude",
            "--model",
            "opus",
            "--idempotency-key",
            "k1",
            "--json",
        ],
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--verdict",
            "fail",
            "--output",
            "rate=0.2",
            "--summary",
            "high",
            "--evidence",
            "https://e/1",
            "--idempotency-key",
            "k2",
            "--json",
        ],
        &env,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let cli_report: Value = serde_json::from_str(&out(&o)).unwrap();
    let cli_trace = trace(&server.join().unwrap());

    // MCP, in one fresh session
    let home = tmp();
    let (host, server) = serve(loop_script());
    let (responses, _) = mcp(
        &host,
        home.path(),
        &env,
        &[
            initialize(),
            call(1, "get_next_steps", json!({"execution_id": EXE})),
            call(
                2,
                "claim_step",
                json!({"step_run_id": SR, "session_id": "s1", "agent_name": "claude", "model": "opus", "idempotency_key": "k1"}),
            ),
            call(
                3,
                "report_step",
                json!({"step_run_id": SR, "claim": {"id": CLAIM, "generation": 1}, "status": "succeeded",
                       "verdict": "fail", "outputs": {"rate": 0.2}, "summary": "high",
                       "evidence": ["https://e/1"], "idempotency_key": "k2"}),
            ),
        ],
    );
    let mcp_trace = trace(&server.join().unwrap());

    assert_eq!(cli_trace.len(), 4, "{cli_trace:#?}");
    assert_eq!(cli_trace, mcp_trace);
    // The same next_action hint.
    assert_eq!(cli_report["next_action"], "exit");
    assert_eq!(control(&responses[3])["next_action"], "exit");
    assert_eq!(
        control(&responses[2])["claim"],
        json!({"id": CLAIM, "generation": 1})
    );
}

#[test]
fn search_and_fetch_link_to_the_host_over_stdio() {
    const ORG: &str = "org_01JABCDEFGHJKMNPQRSTVWXYZ7";
    let home = tmp();
    let (host, server) = serve(vec![
        reply(200, json!({"items": [{"id": ORG}], "next_cursor": null})),
        reply(
            200,
            json!({"runbooks": [{"project_id": "prj_1", "project_slug": "ops", "runbook_id": RB, "slug": "disk-full", "title": "Disk full"}],
                   "runs": [{"project_id": "prj_1", "project_slug": "ops", "id": EXE, "runbook_id": RB, "status": "running",
                             "title": "Disk full on db-2", "tags": [], "fields": {}, "created_by": "user:x", "created_at_ms": 1, "completed_at_ms": null}],
                   "steps": [], "memory": [], "skills": []}),
        ),
        reply(
            200,
            json!({"id": EXE, "runbook_id": RB, "runbook_version_id": RBV, "runbook_version": 2, "title": "Disk full on db-2",
                   "status": "running", "phase": "active", "inputs": {"host": "db-2"}, "fields": {}, "tags": [], "steps": []}),
        ),
    ]);
    let (responses, raw) = mcp(
        &host,
        home.path(),
        &[("RUNBOOK_TOKEN", PAT)],
        &[
            initialize(),
            call(1, "search", json!({"query": "disk full"})),
            call(2, "fetch", json!({"id": EXE})),
        ],
    );
    let seen = server.join().unwrap();
    assert!(request_line(&seen[0]).starts_with("GET /v1/orgs?"));
    assert!(request_line(&seen[1]).starts_with(&format!("GET /v1/orgs/{ORG}/search?q=disk")));
    assert!(request_line(&seen[2]).starts_with(&format!("GET /v1/executions/{EXE} ")));
    let found: Value = serde_json::from_str(&text(&responses[1])).unwrap();
    assert_eq!(found, responses[1]["result"]["structuredContent"]);
    assert_eq!(
        found["results"],
        json!([
            {"id": RB, "title": "Disk full (runbook)", "url": format!("{host}/p/ops/runbooks/{RB}")},
            {"id": EXE, "title": "Disk full on db-2 (run, running)", "url": format!("{host}/x/{EXE}")}
        ])
    );
    let doc: Value = serde_json::from_str(&text(&responses[2])).unwrap();
    assert_eq!(doc["url"], format!("{host}/x/{EXE}"));
    assert!(
        doc["text"]
            .as_str()
            .unwrap()
            .contains("-----BEGIN RUNBOOK DATA ")
    );
    assert!(!raw.contains(PAT));
}
