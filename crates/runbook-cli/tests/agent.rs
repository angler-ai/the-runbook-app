//! C2 agent loop, auth and credential commands against a scripted fake server.
use runbook_cli::credstore::{ExecutionCredential, StateDir};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const SR2: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ2";
const CLAIM: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3";
const ACCESS: &str = "trb_exe_access-secret-one";
const REFRESH: &str = "trb_exr_refresh-secret-one";
const ACCESS2: &str = "trb_exe_access-secret-two";
const REFRESH2: &str = "trb_exr_refresh-secret-two";

/// One scripted reply per connection, in order. Stops after the script or
/// after five idle seconds, and returns every request it saw.
fn serve(replies: Vec<(u16, Value)>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = thread::spawn(move || {
        let mut seen = vec![];
        let mut idx = 0;
        while idx < replies.len() {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                    Err(_) => return seen,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let req = read_request(&mut stream);
            if req.starts_with("GET /.well-known/runbook ") {
                let text = r#"{"minimum_cli_version":"0.1.0"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                continue;
            }
            let (status, body) = &replies[idx];
            idx += 1;
            seen.push(req);
            let text = if body.is_null() && *status == 200 {
                "null".to_string()
            } else {
                body.to_string()
            };
            let kind = if *status >= 400 {
                "application/problem+json"
            } else {
                "application/json"
            };
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
        seen
    });
    (host, handle)
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = vec![];
    let mut chunk = [0u8; 4096];
    let mut need = None;
    loop {
        if let Some(n) = need
            && buf.len() >= n
        {
            break;
        }
        let n = stream.read(&mut chunk).unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if need.is_none()
            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse().unwrap());
            need = Some(pos + 4 + len);
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

fn body_of(request: &str) -> Value {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or(Value::Null)
}

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

fn cli(
    host: &str,
    home: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&str>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .arg("--host")
        .arg(host)
        .args(args)
        .env_remove("RUNBOOK_TOKEN")
        .env_remove("RUNBOOK_HOST")
        .env_remove("CI")
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn store(home: &Path) -> StateDir {
    StateDir::at(home.join("runbook").join("credentials"))
}
fn install(home: &Path, host: &str, expires_at_ms: i64) {
    let s = store(home);
    s.save_execution(&ExecutionCredential {
        host: host.into(),
        execution_id: EXE.into(),
        family_id: "fam_1".into(),
        generation: 1,
        access_token: ACCESS.into(),
        refresh_token: REFRESH.into(),
        expires_at_ms,
        refresh_expires_at_ms: now_ms() + 86_400_000,
    })
    .unwrap();
    s.set_active(host, EXE).unwrap();
}
fn pair(access: &str, refresh: &str) -> Value {
    json!({"family_id":"fam_1","generation":2,"access_token":access,"refresh_token":refresh,
        "expires_at_ms":now_ms()+3_600_000,"refresh_expires_at_ms":now_ms()+86_400_000})
}
fn brief(next: &str) -> Value {
    json!({"schema_revision":1,"execution_id":EXE,"completed_steps":[],"decisions":[],"comments":[],
        "ready_work":[{"step_run_id":SR,"path":"work","title":"Work","actor_kind":"agent"}],"next_action":next})
}
fn assert_no_secrets(o: &Output) {
    for secret in [ACCESS, REFRESH, ACCESS2, REFRESH2] {
        assert!(!out(o).contains(secret), "stdout leaked a secret");
        assert!(!err(o).contains(secret), "stderr leaked a secret");
    }
}

#[test]
fn whoami_reports_a_personal_token_and_its_organizations() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![(
        200,
        json!({"items":[{"id":"org_01JABCDEFGHJKMNPQRSTVWXYZ0","title":"Acme"}],"next_cursor":null}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &["whoami", "--json"],
        &[("RUNBOOK_TOKEN", "trb_pat_secret")],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["credential"], "personal");
    assert_eq!(v["authenticated"], true);
    assert_eq!(v["organizations"][0]["title"], "Acme");
    assert!(!out(&o).contains("trb_pat_secret"));
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with("GET /v1/orgs "));
}

#[test]
fn whoami_with_a_valid_but_unscoped_token_is_still_authenticated() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = serve(vec![(403, json!({"code":"auth.scope_missing"}))]);
    let o = cli(
        &host,
        home.path(),
        &["whoami", "--json"],
        &[("RUNBOOK_TOKEN", "trb_svc_secret")],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["credential"], "service");
    assert_eq!(v["organizations"], Value::Null);
}

#[test]
fn whoami_fails_when_rejected_or_unconfigured() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = serve(vec![(401, json!({"code":"auth.invalid_credential"}))]);
    let o = cli(
        &host,
        home.path(),
        &["whoami"],
        &[("RUNBOOK_TOKEN", "trb_pat_bad")],
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("auth.invalid_credential"));
    let o = cli(&host, home.path(), &["whoami"], &[], None);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("runbook run token"));
}

#[test]
fn whoami_uses_stored_execution_credentials() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![(200, brief("claim_ready_work"))]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(&host, home.path(), &["whoami", "--json"], &[], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["credential"], "execution");
    assert_eq!(v["execution_id"], EXE);
    assert_no_secrets(&o);
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with(&format!("GET /v1/executions/{EXE} ")));
    assert_eq!(
        header(&seen[0], "authorization"),
        Some(format!("Bearer {ACCESS}").as_str())
    );
}

#[test]
fn step_next_prints_the_brief_and_records_ready_steps() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (200, brief("claim_ready_work")),
        (200, brief("claim_ready_work")),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &["step", "next", EXE, "--json"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["next_action"], "claim_ready_work");
    assert_eq!(v["ready_work"][0]["step_run_id"], SR);
    assert_eq!(
        store(home.path())
            .load_step(SR)
            .unwrap()
            .unwrap()
            .execution_id
            .as_deref(),
        Some(EXE)
    );
    let o = cli(&host, home.path(), &["step", "next", EXE], &[], None);
    assert!(out(&o).contains(SR) && out(&o).contains("claim_ready_work"));
    assert_no_secrets(&o);
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with(&format!("GET /v1/executions/{EXE}/next ")));
}

#[test]
fn step_get_and_skill_file_read_one_step() {
    let home = tempfile::tempdir().unwrap();
    let detail = json!({"step_run_id":SR,"instructions":"Check health","checklist":[],"parameters":{},"output_schema":{},"skill":null,
        "template":{"instructions":"Check health","checklist":[],"values":{}}});
    let (host, server) = serve(vec![
        (200, detail),
        (
            200,
            json!({"path":"references/notes.md","content":"aGVsbG8gbm90ZXMK"}),
        ),
    ]);
    let token = [("RUNBOOK_TOKEN", "trb_pat_secret")];
    let o = cli(
        &host,
        home.path(),
        &["step", "get", SR, "--json"],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["instructions"], "Check health");
    assert_eq!(v["next_action"], "claim_ready_work");
    let o = cli(
        &host,
        home.path(),
        &["step", "skill-file", SR, "references/notes.md"],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "hello notes\n");
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with(&format!("GET /v1/step-runs/{SR} ")));
    assert!(seen[1].starts_with(&format!(
        "GET /v1/step-runs/{SR}/skill-files?path=references%2Fnotes.md "
    )));
    let o = cli(&host, home.path(), &["step", "get", "../x"], &token, None);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn claim_reference_is_stored_and_reused_by_later_commands() {
    let home = tempfile::tempdir().unwrap();
    let claim = json!({"id":CLAIM,"generation":3});
    let (host, server) = serve(vec![
        (200, claim.clone()),
        (200, json!({"lease_deadline_at_ms":1})),
        (200, Value::Null),
        (200, json!({"lease_deadline_at_ms":2})),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
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
            "runner",
            "--model",
            "sol",
            "--json",
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("Idempotency-Key: "));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["claim"], claim);
    let saved = store(home.path()).load_step(SR).unwrap().unwrap();
    assert_eq!(saved.claim, Some(claim.clone()));
    assert_eq!(saved.execution_id.as_deref(), Some(EXE));
    assert_eq!(
        cli(&host, home.path(), &["step", "heartbeat", SR], &[], None)
            .status
            .code(),
        Some(0)
    );
    let o = cli(
        &host,
        home.path(),
        &["step", "log", SR, "--idempotency-key", "log-1"],
        &[],
        Some("first line\r\nsecond line\npartial"),
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &["step", "heartbeat", SR, "--claim", &format!("{CLAIM}:9")],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with(&format!("POST /v1/step-runs/{SR}/claim ")));
    assert_eq!(
        body_of(&seen[0]),
        json!({"session_id":"s1","actor":{"kind":"agent","agent_name":"runner","model":"sol"}})
    );
    assert!(header(&seen[0], "idempotency-key").is_some_and(|k| !k.is_empty()));
    assert_eq!(body_of(&seen[1]), json!({"claim":claim}));
    assert_eq!(
        body_of(&seen[2]),
        json!({"claim":claim,"logs":["first line","second line","partial"]})
    );
    assert_eq!(header(&seen[2], "idempotency-key"), Some("log-1"));
    assert_eq!(
        body_of(&seen[3]),
        json!({"claim":{"id":CLAIM,"generation":9}})
    );
    // A malformed override is a usage error and sends nothing.
    let o = cli(
        &host,
        home.path(),
        &["step", "heartbeat", SR, "--claim", "nope"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
}

/// LANG7: `--take-over` sends `take_over: true` (a person claiming an
/// agent step); it cannot be combined with an agent declaration.
#[test]
fn take_over_claims_as_a_person() {
    let home = tempfile::tempdir().unwrap();
    let claim = json!({"id":CLAIM,"generation":1});
    let (host, server) = serve(vec![(200, claim.clone())]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "claim",
            SR,
            "--session",
            "web",
            "--take-over",
            "--json",
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with(&format!("POST /v1/step-runs/{SR}/claim ")));
    assert_eq!(
        body_of(&seen[0]),
        json!({"session_id":"web","take_over":true})
    );
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "claim",
            SR,
            "--session",
            "s1",
            "--take-over",
            "--agent-name",
            "runner",
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
}

#[test]
fn step_report_sends_typed_outputs_and_returns_the_next_action() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("out.log");
    std::fs::write(&log, "ran checks\nall green\n").unwrap();
    let (host, server) = serve(vec![(200, Value::Null), (200, brief("exit"))]);
    install(home.path(), &host, now_ms() + 3_600_000);
    store(home.path())
        .save_step(
            SR,
            &runbook_cli::credstore::StepState {
                host: host.clone(),
                execution_id: Some(EXE.into()),
                claim: Some(json!({"id":CLAIM,"generation":1})),
            },
        )
        .unwrap();
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
            "pass",
            "--output",
            "count=3",
            "--output",
            "service=api",
            "--summary",
            "Healthy",
            "--evidence",
            "https://ci.example/run/1",
            "--log-file",
            log.to_str().unwrap(),
            "--json",
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["next_action"], "exit");
    let seen = server.join().unwrap();
    // CF12b review round 2: a log file that fits a report's logs goes in
    // the report itself, one atomic request.
    assert!(seen[0].starts_with(&format!("POST /v1/step-runs/{SR}/report ")));
    assert_eq!(
        body_of(&seen[0]),
        json!({"step_run_id":SR,"claim":{"id":CLAIM,"generation":1},"status":"succeeded",
        "verdict":"pass","outputs":{"count":3,"service":"api"},"summary":"Healthy",
        "evidence":["https://ci.example/run/1"],"logs":["ran checks","all green"]})
    );
    assert!(seen[1].starts_with(&format!("GET /v1/executions/{EXE}/next ")));
    // The finished claim is forgotten.
    assert_eq!(
        store(home.path())
            .load_step(SR)
            .unwrap()
            .and_then(|s| s.claim),
        None
    );
    let o = cli(
        &host,
        home.path(),
        &["step", "report", SR, "--status", "succeeded"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2), "no stored claim is a usage error");
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "maybe",
            "--claim",
            &format!("{CLAIM}:1"),
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn request_human_reports_blocked_with_reason_and_question() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![(200, Value::Null), (200, brief("wait"))]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "request-human",
            SR2,
            "--claim",
            &format!("{CLAIM}:2"),
            "--reason",
            "No prod access",
            "--question",
            "Can someone rotate the key?",
            "--json",
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        serde_json::from_str::<Value>(&out(&o)).unwrap()["next_action"],
        "wait"
    );
    let seen = server.join().unwrap();
    let body = body_of(&seen[0]);
    assert_eq!(body["status"], "blocked");
    assert_eq!(
        body["summary"],
        "Reason: No prod access\nQuestion: Can someone rotate the key?"
    );
    assert_eq!(body["claim"], json!({"id":CLAIM,"generation":2}));
}

#[test]
fn checklist_sends_every_item_in_order() {
    let home = tempfile::tempdir().unwrap();
    let detail = json!({"step_run_id":SR,"instructions":"","checklist":["Drain node","Verify health"],"parameters":{},"output_schema":{},"skill":null});
    let (host, server) = serve(vec![
        (200, detail.clone()),
        (200, Value::Null),
        (200, detail),
    ]);
    let token = [("RUNBOOK_TOKEN", "trb_pat_secret")];
    let claim = format!("{CLAIM}:1");
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "checklist",
            SR,
            "--claim",
            &claim,
            "--tick",
            "Verify health",
        ],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "checklist",
            SR,
            "--claim",
            &claim,
            "--tick",
            "Unknown",
        ],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 3, "an unknown item sends no update");
    assert_eq!(
        body_of(&seen[1]),
        json!({"claim":{"id":CLAIM,"generation":1},"checklist":[
        {"title":"Drain node","completed":false},{"title":"Verify health","completed":true}]})
    );
}

#[test]
fn decide_and_release_send_their_bodies() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (
            200,
            json!({"by":"agent","agent_name":"runner","model":null,"session_id":"s1","option":"keep","comment":null}),
        ),
        (200, Value::Null),
        (200, Value::Null),
    ]);
    let token = [("RUNBOOK_TOKEN", "trb_pat_secret")];
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "decide",
            SR,
            "--option",
            "keep",
            "--comment",
            "fine",
            "--session",
            "s1",
            "--agent-name",
            "runner",
        ],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &["step", "release", SR, "--claim", &format!("{CLAIM}:4")],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &["step", "release", SR, "--reason", "stuck agent"],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with(&format!("POST /v1/step-runs/{SR}/decisions ")));
    assert_eq!(
        body_of(&seen[0]),
        json!({"option":"keep","comment":"fine","session_id":"s1","actor":{"kind":"agent","agent_name":"runner"}})
    );
    assert_eq!(
        body_of(&seen[1]),
        json!({"claim":{"id":CLAIM,"generation":4}})
    );
    assert_eq!(body_of(&seen[2]), json!({"reason":"stuck agent"}));
    for request in &seen {
        assert!(header(request, "idempotency-key").is_some_and(|k| !k.is_empty()));
    }
}

#[cfg(unix)]
#[test]
fn run_token_stores_the_pair_privately_and_never_prints_it() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (201, pair(ACCESS, REFRESH)),
        (201, pair(ACCESS2, REFRESH2)),
    ]);
    let token = [("RUNBOOK_TOKEN", "trb_svc_launcher")];
    let o = cli(
        &host,
        home.path(),
        &["run", "token", EXE, "--session", "launch-1"],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_no_secrets(&o);
    let saved = store(home.path()).load_execution(EXE).unwrap().unwrap();
    assert_eq!(saved.access_token, ACCESS);
    assert_eq!(saved.refresh_token, REFRESH);
    assert!(
        !format!("{saved:?}").contains(ACCESS),
        "Debug must redact secrets"
    );
    let root = home.path().join("runbook").join("credentials");
    let file = std::fs::read_dir(root.join("executions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(root.join("executions"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    // --json hands the pair to the caller once and stores nothing.
    let other = tempfile::tempdir().unwrap();
    let o = cli(
        &host,
        other.path(),
        &["run", "token", EXE, "--json"],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        serde_json::from_str::<Value>(&out(&o)).unwrap()["access_token"],
        ACCESS2
    );
    assert!(store(other.path()).load_execution(EXE).unwrap().is_none());
    let seen = server.join().unwrap();
    assert!(seen[0].starts_with(&format!("POST /v1/executions/{EXE}/tokens ")));
    assert_eq!(body_of(&seen[0]), json!({"session_id":"launch-1"}));
    assert!(
        body_of(&seen[1])["session_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
}

#[test]
fn expired_access_token_refreshes_once_and_rotates_both_secrets() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (401, json!({"code":"credentials.expired"})),
        (201, pair(ACCESS2, REFRESH2)),
        (200, brief("claim_ready_work")),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &["step", "next", EXE, "--json"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_no_secrets(&o);
    let seen = server.join().unwrap();
    assert_eq!(
        header(&seen[0], "authorization"),
        Some(format!("Bearer {ACCESS}").as_str())
    );
    assert!(seen[1].starts_with(&format!("POST /v1/executions/{EXE}/tokens/refresh ")));
    assert_eq!(body_of(&seen[1]), json!({"refresh_token":REFRESH}));
    assert_eq!(
        header(&seen[1], "authorization"),
        None,
        "refresh authenticates its body only"
    );
    assert!(header(&seen[1], "idempotency-key").is_some());
    assert_eq!(
        header(&seen[2], "authorization"),
        Some(format!("Bearer {ACCESS2}").as_str())
    );
    let saved = store(home.path()).load_execution(EXE).unwrap().unwrap();
    assert_eq!(
        (saved.access_token.as_str(), saved.refresh_token.as_str()),
        (ACCESS2, REFRESH2)
    );
    // The advisory lock is released (its file may remain).
    assert!(
        store(home.path())
            .lock(EXE, Duration::from_millis(100))
            .is_ok()
    );
}

#[test]
fn near_expiry_access_token_refreshes_before_the_request() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![(201, pair(ACCESS2, REFRESH2)), (200, brief("wait"))]);
    install(home.path(), &host, now_ms() + 5_000);
    let o = cli(&host, home.path(), &["step", "next", EXE], &[], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(seen[0].contains("/tokens/refresh"));
    assert_eq!(
        header(&seen[1], "authorization"),
        Some(format!("Bearer {ACCESS2}").as_str())
    );
}

#[test]
fn refresh_failure_stops_with_launcher_guidance_and_never_loops() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (401, json!({"code":"credentials.expired"})),
        (409, json!({"code":"credentials.refresh_reused"})),
        (200, brief("wait")),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &["step", "next", EXE, "--json"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("credentials.refresh_reused"));
    assert!(err(&o).contains("trusted launcher"));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["next_action"], "refresh_credentials");
    assert_no_secrets(&o);
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2, "one refresh attempt, no retry loop");
    assert!(
        store(home.path()).load_execution(EXE).unwrap().is_none(),
        "a dead family is forgotten"
    );
}

#[test]
fn concurrent_refresh_waits_for_the_lock_and_reuses_the_rotated_pair() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (401, json!({"code":"credentials.expired"})),
        (200, brief("wait")),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let s = store(home.path());
    let guard = s.lock(EXE, Duration::from_secs(1)).unwrap();
    let rotated = host.clone();
    let holder = thread::spawn(move || {
        thread::sleep(Duration::from_millis(700));
        let mut cred = s.load_execution(EXE).unwrap().unwrap();
        cred.access_token = ACCESS2.into();
        cred.refresh_token = REFRESH2.into();
        cred.host = rotated;
        s.save_execution(&cred).unwrap();
        drop(guard);
    });
    let o = cli(&host, home.path(), &["step", "next", EXE], &[], None);
    holder.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2, "no second refresh of a rotated family");
    assert_eq!(
        header(&seen[1], "authorization"),
        Some(format!("Bearer {ACCESS2}").as_str())
    );
}

#[test]
fn limit_errors_exit_4_and_other_problems_exit_1() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = serve(vec![
        (429, json!({"code":"rate.limited"})),
        (403, json!({"code":"limit.executions_monthly"})),
        (409, json!({"code":"step.not_claimed"})),
    ]);
    let token = [("RUNBOOK_TOKEN", "trb_pat_secret")];
    let start = [
        "run",
        "start",
        "deploy",
        "--project",
        "ops",
        "--idempotency-key",
        "k",
    ];
    assert_eq!(
        cli(&host, home.path(), &start, &token, None).status.code(),
        Some(4)
    );
    assert_eq!(
        cli(&host, home.path(), &start, &token, None).status.code(),
        Some(4)
    );
    let o = cli(
        &host,
        home.path(),
        &["step", "heartbeat", SR, "--claim", &format!("{CLAIM}:1")],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("step.not_claimed"));
}

fn sse(frames: &'static str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = thread::spawn(move || {
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            let req = read_request(&mut stream);
            if req.starts_with("GET /.well-known/runbook ") {
                let text = r#"{"minimum_cli_version":"0.1.0"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                continue;
            }
            stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
            stream.write_all(frames.as_bytes()).unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Both);
            break;
        }
    });
    (host, handle)
}

#[test]
fn run_watch_exit_code_follows_the_execution_status() {
    let home = tempfile::tempdir().unwrap();
    let token = [("RUNBOOK_TOKEN", "trb_pat_secret")];
    let (host, h) = sse(
        "id: 9\nevent: ExecutionCompleted\ndata: {\"seq\":9,\"event\":{\"type\":\"ExecutionCompleted\",\"status\":\"failed\",\"outputs\":{}}}\n\n",
    );
    assert_eq!(
        cli(&host, home.path(), &["run", "watch", EXE], &token, None)
            .status
            .code(),
        Some(3)
    );
    h.join().unwrap();
    let (host, h) = sse(
        "id: 9\nevent: ExecutionCompleted\ndata: {\"seq\":9,\"event\":{\"type\":\"ExecutionCompleted\",\"status\":\"succeeded\",\"outputs\":{\"result\":\"rolled_back\"}}}\n\n",
    );
    assert_eq!(
        cli(&host, home.path(), &["run", "watch", EXE], &token, None)
            .status
            .code(),
        Some(0)
    );
    h.join().unwrap();
}

#[test]
fn run_get_wait_polls_until_terminal() {
    let home = tempfile::tempdir().unwrap();
    let token = [("RUNBOOK_TOKEN", "trb_pat_secret")];
    let (host, server) = serve(vec![
        (200, json!({"id":EXE,"status":"running"})),
        (200, json!({"id":EXE,"status":"failed"})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["run", "get", EXE, "--wait", "--interval-ms", "10"],
        &token,
        None,
    );
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert_eq!(server.join().unwrap().len(), 2);
    let (host, _server) = serve(vec![(
        200,
        json!({"id":EXE,"status":"succeeded","result":"rolled_back"}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &["run", "get", EXE, "--wait", "--interval-ms", "10"],
        &token,
        None,
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "a successful rollback result is success"
    );
    // Without --wait a failed run is just a successful read.
    let (host, _server) = serve(vec![(200, json!({"id":EXE,"status":"failed"}))]);
    assert_eq!(
        cli(&host, home.path(), &["run", "get", EXE], &token, None)
            .status
            .code(),
        Some(0)
    );
}

const ACCESS3: &str = "trb_exe_access-secret-three";
const REFRESH3: &str = "trb_exr_refresh-secret-three";

/// M2: another process rotated the pair, but its new access token is itself
/// near expiry. The refresh must spend the stored (newest) refresh token,
/// never the one this process already used, or the family is revoked.
#[test]
fn rotated_but_expiring_pair_refreshes_with_the_stored_refresh_token() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (401, json!({"code":"credentials.expired"})),
        (201, pair(ACCESS3, REFRESH3)),
        (200, brief("wait")),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let s = store(home.path());
    let guard = s.lock(EXE, Duration::from_secs(1)).unwrap();
    let holder = thread::spawn(move || {
        thread::sleep(Duration::from_millis(700));
        let mut cred = s.load_execution(EXE).unwrap().unwrap();
        cred.access_token = ACCESS2.into();
        cred.refresh_token = REFRESH2.into();
        cred.expires_at_ms = now_ms() + 5_000;
        s.save_execution(&cred).unwrap();
        drop(guard);
    });
    let o = cli(&host, home.path(), &["step", "next", EXE], &[], None);
    holder.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(body_of(&seen[1]), json!({"refresh_token":REFRESH2}));
    assert_eq!(
        header(&seen[2], "authorization"),
        Some(format!("Bearer {ACCESS3}").as_str())
    );
    let saved = store(home.path()).load_execution(EXE).unwrap().unwrap();
    assert_eq!(saved.refresh_token, REFRESH3);
}

/// M2: a refresh response without an expiry must not make the next command
/// refresh pre-emptively.
#[test]
fn missing_expiry_never_forces_another_refresh() {
    let home = tempfile::tempdir().unwrap();
    let mut rotated = pair(ACCESS2, REFRESH2);
    rotated.as_object_mut().unwrap().remove("expires_at_ms");
    let (host, server) = serve(vec![
        (401, json!({"code":"credentials.expired"})),
        (201, rotated),
        (200, brief("wait")),
        (200, brief("wait")),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    assert_eq!(
        cli(&host, home.path(), &["step", "next", EXE], &[], None)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        cli(&host, home.path(), &["step", "next", EXE], &[], None)
            .status
            .code(),
        Some(0)
    );
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 4);
    assert!(!seen[3].contains("/tokens/refresh"));
    assert_eq!(
        header(&seen[3], "authorization"),
        Some(format!("Bearer {ACCESS2}").as_str())
    );
}

/// M3: a lock that looks old is still held; it is never deleted or stolen.
#[test]
fn an_old_lock_file_held_by_another_process_is_never_stolen() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (401, json!({"code":"credentials.expired"})),
        (200, brief("wait")),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let s = store(home.path());
    let guard = s.lock(EXE, Duration::from_secs(1)).unwrap();
    let lock_path = home
        .path()
        .join("runbook/credentials/executions")
        .join(format!("{EXE}.lock"));
    std::fs::File::options()
        .write(true)
        .open(&lock_path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    let holder = thread::spawn(move || {
        thread::sleep(Duration::from_millis(700));
        let mut cred = s.load_execution(EXE).unwrap().unwrap();
        cred.access_token = ACCESS2.into();
        cred.refresh_token = REFRESH2.into();
        s.save_execution(&cred).unwrap();
        drop(guard);
    });
    let o = cli(&host, home.path(), &["step", "next", EXE], &[], None);
    holder.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "the waiter must not refresh while the holder works"
    );
    assert_eq!(
        header(&seen[1], "authorization"),
        Some(format!("Bearer {ACCESS2}").as_str())
    );
}

/// M3: a hung refresh times out instead of outliving the lock; a transport
/// failure keeps the pair.
#[test]
fn a_hung_refresh_times_out_and_keeps_the_pair() {
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let hang = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        thread::sleep(Duration::from_secs(4));
    });
    install(home.path(), &host, now_ms() + 5_000);
    let started = Instant::now();
    let o = cli(
        &host,
        home.path(),
        &["step", "next", EXE],
        &[("RUNBOOK_HTTP_TIMEOUT_MS", "300")],
        None,
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "no read timeout"
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_no_secrets(&o);
    assert!(store(home.path()).load_execution(EXE).unwrap().is_some());
    hang.join().unwrap();
}

/// M4: only terminal credential codes forget the pair; transient problems
/// keep it and exit with the mapped code.
#[test]
fn transient_refresh_problems_keep_the_pair() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        (401, json!({"code":"credentials.expired"})),
        (503, json!({"code":"storage.unavailable"})),
        (401, json!({"code":"credentials.expired"})),
        (429, json!({"code":"rate.limited"})),
    ]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &["step", "next", EXE, "--json"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("storage.unavailable"));
    assert!(!err(&o).contains("trusted launcher"));
    assert_ne!(
        serde_json::from_str::<Value>(&out(&o)).unwrap()["next_action"],
        "refresh_credentials"
    );
    assert!(store(home.path()).load_execution(EXE).unwrap().is_some());
    let o = cli(&host, home.path(), &["step", "next", EXE], &[], None);
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let saved = store(home.path()).load_execution(EXE).unwrap().unwrap();
    assert_eq!(saved.refresh_token, REFRESH);
    assert_no_secrets(&o);
    assert_eq!(server.join().unwrap().len(), 4);
}

/// Every `/logs` request's key and entries, in order.
fn log_batches(seen: &[String]) -> Vec<(String, Vec<String>)> {
    seen.iter()
        .filter(|r| r.starts_with(&format!("POST /v1/step-runs/{SR}/logs ")))
        .map(|r| {
            let entries = body_of(r)["logs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap().to_owned())
                .collect();
            (header(r, "idempotency-key").unwrap().to_owned(), entries)
        })
        .collect()
}

/// CF12b review round 2 (finding 2): `step log` sends each line whole,
/// however long: the server redacts, then splits it into 64 KiB entries. A
/// client split before redaction stored the tail of a secret on its own
/// (the review's reproduction below). Input past one request's batch goes
/// as several requests, keyed `K-logs-1`, `K-logs-2`… (a log that fits one
/// request keeps the key K).
#[test]
fn step_log_sends_long_lines_whole_and_batches_large_input() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![(200, Value::Null), (200, Value::Null)]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let long = format!(
        "{} password=hunter2SUPERSECRETVALUE done",
        "x".repeat(65_518)
    );
    let short = "b".repeat(1_000);
    let input = format!("{long}\n{}", format!("{short}\n").repeat(300));
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "log",
            SR,
            "--claim",
            &format!("{CLAIM}:2"),
            "--idempotency-key",
            "big",
        ],
        &[],
        Some(&input),
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let batches = log_batches(&server.join().unwrap());
    let keys: Vec<&str> = batches.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["big-logs-1", "big-logs-2"]);
    let entries: Vec<String> = batches.into_iter().flat_map(|(_, e)| e).collect();
    assert_eq!(entries[0], long);
    assert_eq!(entries[1..], vec![short; 300][..]);
}

/// CF12b review round 2 (nit): a key too long for `-logs-N` (the server
/// takes 256 bytes) is hashed for the sub-keys; a line larger than one
/// request is cut after whitespace, and stderr says so.
#[test]
fn step_log_sub_keys_fit_and_an_oversized_line_is_cut_with_a_warning() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![(200, Value::Null), (200, Value::Null)]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let key = "k".repeat(250);
    let huge = format!("{} {}", "y".repeat(1_500_000), "z".repeat(1_000_000));
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "log",
            SR,
            "--claim",
            &format!("{CLAIM}:2"),
            "--idempotency-key",
            &key,
        ],
        &[],
        Some(&huge),
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("larger than one request"), "{}", err(&o));
    let batches = log_batches(&server.join().unwrap());
    assert_eq!(batches.len(), 2);
    for (n, (k, _)) in batches.iter().enumerate() {
        assert!(k.len() <= 256, "{}", k.len());
        assert!(k.ends_with(&format!("-logs-{}", n + 1)), "{k}");
    }
    let entries: Vec<String> = batches.into_iter().flat_map(|(_, e)| e).collect();
    assert_eq!(entries[0], format!("{} ", "y".repeat(1_500_000)));
    assert_eq!(entries.concat(), huge);
}

/// CF12b review round 2 (finding 3): a log file that fits a report's logs
/// (1 MiB as JSON) goes in the report itself, one atomic request: a
/// refused report appends nothing.
#[test]
fn step_report_carries_a_log_file_that_fits_in_the_report() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("small.log");
    std::fs::write(&log, "built\nok\n").unwrap();
    let (host, server) = serve(vec![(200, Value::Null), (200, brief("exit"))]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--claim",
            &format!("{CLAIM}:1"),
            "--status",
            "succeeded",
            "--log-file",
            log.to_str().unwrap(),
            "--idempotency-key",
            "rep",
            "--json",
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(log_batches(&seen).is_empty());
    let report = &seen[0];
    assert!(report.starts_with(&format!("POST /v1/step-runs/{SR}/report ")));
    assert_eq!(header(report, "idempotency-key"), Some("rep"));
    assert_eq!(body_of(report)["logs"], json!(["built", "ok"]));
}

/// CF12b review round 2 (finding 3): without `--idempotency-key`, a log file
/// sent through the logs route is keyed by the step run, the claim and the
/// file's SHA-256, so running the same command again after a refused report
/// replays those requests instead of appending the lines twice. The
/// report's own key stays random, so the corrected report goes through.
#[test]
fn step_report_log_keys_are_derived_so_a_rerun_replays_them() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("big.log");
    std::fs::write(&log, format!("{}\n", "c".repeat(1_000)).repeat(1_100)).unwrap();
    let other = dir.path().join("other.log");
    std::fs::write(&other, format!("{}\n", "e".repeat(1_000)).repeat(1_100)).unwrap();
    // 1.1 MB: past one report, five batches.
    let run = |file: &std::path::Path, claim: u64, refuse: bool| {
        let home = tempfile::tempdir().unwrap();
        // CF12c: the step read (its evidence policy), then five batches.
        let mut replies = vec![(200, Value::Null); 6];
        if refuse {
            replies.push((422, json!({"code": "step.invalid_outputs"})));
        } else {
            replies.push((200, Value::Null));
            replies.push((200, brief("exit")));
        }
        let (host, server) = serve(replies);
        install(home.path(), &host, now_ms() + 3_600_000);
        let o = cli(
            &host,
            home.path(),
            &[
                "step",
                "report",
                SR,
                "--claim",
                &format!("{CLAIM}:{claim}"),
                "--status",
                "succeeded",
                "--log-file",
                file.to_str().unwrap(),
                "--json",
            ],
            &[],
            None,
        );
        assert_eq!(o.status.code() == Some(0), !refuse, "{}", err(&o));
        let seen = server.join().unwrap();
        let keys: Vec<String> = log_batches(&seen).into_iter().map(|(k, _)| k).collect();
        let report = seen
            .iter()
            .find(|r| r.starts_with(&format!("POST /v1/step-runs/{SR}/report ")))
            .unwrap();
        assert_eq!(body_of(report)["logs"], json!([]));
        (keys, header(report, "idempotency-key").unwrap().to_owned())
    };
    let (first, first_report) = run(&log, 1, true);
    let (again, again_report) = run(&log, 1, false);
    assert_eq!(first.len(), 5);
    assert_eq!(first, again, "the rerun replays the same log requests");
    assert!(
        first[0].starts_with("logs-") && first[0].ends_with("-1"),
        "{}",
        first[0]
    );
    assert_ne!(first_report, again_report, "each report has its own key");
    assert!(!first.contains(&first_report));
    // Another file or another claim is another set of keys.
    assert_ne!(run(&other, 1, false).0, first);
    assert_ne!(run(&log, 2, false).0, first);
}

/// `step report SR --claim CLAIM:1 --status succeeded --log-file FILE
/// --json` and `extra` against `replies`: the command's output and the
/// requests the server saw.
fn report_log_file(
    file: &std::path::Path,
    extra: &[&str],
    replies: Vec<(u16, Value)>,
) -> (Output, Vec<String>) {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(replies);
    install(home.path(), &host, now_ms() + 3_600_000);
    let claim = format!("{CLAIM}:1");
    let mut args = vec![
        "step",
        "report",
        SR,
        "--claim",
        &claim,
        "--status",
        "succeeded",
        "--log-file",
        file.to_str().unwrap(),
        "--json",
    ];
    args.extend_from_slice(extra);
    let o = cli(&host, home.path(), &args, &[], None);
    (o, server.join().unwrap())
}

/// CF12c (CF12b's final review, item 3): on a step that does not accept
/// `log` evidence, a log file is refused whatever its size. A file that
/// fits rides in the report, which the server refuses
/// (`step.evidence_not_accepted`) before anything is stored. A larger one
/// would go through the logs route, which takes lines on any step, and the
/// report would then be accepted: so the CLI reads the step's evidence
/// policy first and refuses it with the same code, sending nothing.
/// Before, the large file's lines were appended and the report accepted.
#[test]
fn step_report_refuses_a_log_file_the_step_does_not_accept_whatever_its_size() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.log");
    std::fs::write(&big, format!("{}\n", "c".repeat(1_000)).repeat(1_100)).unwrap();
    let small = dir.path().join("small.log");
    std::fs::write(&small, "built\n").unwrap();
    let policy = |accepted: Value| json!({"step_run_id": SR, "evidence": {"required": [], "accepted": accepted}});

    let (o, seen) = report_log_file(&big, &[], vec![(200, policy(json!(["link"])))]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("step.evidence_not_accepted"),
        "{}",
        err(&o)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&o.stdout).unwrap()["error"]["code"],
        "step.evidence_not_accepted"
    );
    assert_eq!(seen.len(), 1, "only the step read");
    assert!(
        seen[0].starts_with(&format!("GET /v1/step-runs/{SR} ")),
        "{}",
        seen[0]
    );

    let (o, seen) = report_log_file(
        &small,
        &[],
        vec![(422, json!({"code": "step.evidence_not_accepted"}))],
    );
    assert_ne!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        err(&o).contains("step.evidence_not_accepted"),
        "{}",
        err(&o)
    );
    assert!(log_batches(&seen).is_empty());
    assert_eq!(seen.len(), 1, "the report alone");

    // A step that accepts `log`, or a server whose step read has no policy
    // (older), still takes the large file through the logs route.
    for read in [policy(json!(["log", "link"])), json!({"step_run_id": SR})] {
        let mut replies = vec![(200, read)];
        replies.extend(vec![(200, Value::Null); 6]);
        replies.push((200, brief("exit")));
        let (o, seen) = report_log_file(&big, &[], replies);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(log_batches(&seen).len(), 5);
    }
}

/// CF12c review round 1 (F2): `step report --log-file` run again after a
/// lost response, with the same key and an execution token. The first run
/// committed the report, which ended the claim, so the step read (its
/// evidence policy) is now refused: 403 for an execution token, 404 where
/// the step is no longer visible. That read only decides whether to refuse
/// up front; it falls through to the keyed requests, which replay what was
/// committed (or the server's fence refuses them). Before, the rerun failed
/// on the read and the committed report was never replayed.
#[test]
fn step_report_log_file_rerun_after_a_lost_response_replays_when_the_step_read_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.log");
    std::fs::write(&big, format!("{}\n", "c".repeat(1_000)).repeat(1_100)).unwrap();
    for status in [403, 404] {
        let mut replies = vec![(
            status,
            json!({"code": if status == 403 { "auth.forbidden" } else { "resource.not_found" }}),
        )];
        replies.extend(vec![(200, Value::Null); 5]);
        replies.push((200, brief("exit")));
        let (o, seen) = report_log_file(&big, &["--idempotency-key", "rep"], replies);
        assert_eq!(o.status.code(), Some(0), "{status}: {}", err(&o));
        assert!(seen[0].starts_with(&format!("GET /v1/step-runs/{SR} ")));
        let keys: Vec<String> = log_batches(&seen).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            [
                "rep-logs-1",
                "rep-logs-2",
                "rep-logs-3",
                "rep-logs-4",
                "rep-logs-5"
            ]
        );
        let report = seen.last().unwrap();
        assert!(report.starts_with(&format!("POST /v1/step-runs/{SR}/report ")));
        assert_eq!(header(report, "idempotency-key"), Some("rep"));
    }
    // Any other failure of the read still stops before anything is sent.
    let (o, seen) = report_log_file(
        &big,
        &["--idempotency-key", "rep"],
        vec![(500, json!({"code": "internal"}))],
    );
    assert_ne!(o.status.code(), Some(0), "{}", err(&o));
    assert!(log_batches(&seen).is_empty());
}

/// CF12c (CF12b's final review, item 4): a log file under 1 MiB whose
/// redacted form passes what a report's logs may hold once redacted
/// (`claims::MAX_REPORT_LOG_JSON_BYTES`, 1 MiB + 128 KiB) goes through the
/// logs route, under the derived keys, as a larger file does. Before, it
/// rode in the report, which the server refused (`request.value_too_large`)
/// though the logs route would have taken it.
#[test]
fn step_report_sends_a_log_file_that_redaction_grows_past_a_report_through_the_logs_route() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("pairs.log");
    // 1,000 lines of 125 `token=x` pairs: 1,000,000 bytes, about 2.9 MB
    // once each value is `[REDACTED:token]`.
    let line = "token=x ".repeat(125);
    std::fs::write(&log, format!("{}\n", line.trim_end()).repeat(1_000)).unwrap();
    let raw: Vec<String> = vec![line.trim_end().to_owned(); 1_000];
    assert!(serde_json::to_vec(&raw).unwrap().len() <= 1_048_576);
    let mut replies = vec![(200, Value::Null); 5];
    replies.push((200, Value::Null));
    replies.push((200, brief("exit")));
    let (o, seen) = report_log_file(&log, &[], replies.clone());
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let keys: Vec<String> = log_batches(&seen).into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys.len(), 4, "{keys:?}");
    assert!(
        keys[0].starts_with("logs-") && keys[0].ends_with("-1"),
        "{}",
        keys[0]
    );
    let report = seen
        .iter()
        .find(|r| r.starts_with(&format!("POST /v1/step-runs/{SR}/report ")))
        .unwrap();
    assert_eq!(body_of(report)["logs"], json!([]));
    // The same file again: the same keys (a rerun replays them).
    let (_, again) = report_log_file(&log, &[], replies);
    let again: Vec<String> = log_batches(&again).into_iter().map(|(k, _)| k).collect();
    assert_eq!(again, keys);
}

/// CF12b review round 1 (finding 1): `step report --log-file` sends a file
/// past the report's 1 MiB through the logs route before the report (keys
/// `K-logs-N`), so it is not refused and the step's 5 MiB stays reachable;
/// the report itself then carries no logs. (Round 2: a file that fits goes
/// in the report.)
#[test]
fn step_report_sends_a_large_log_file_through_the_logs_route() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("big.log");
    // 1,600 lines of 1,000 bytes (1.6 MB), then one of 100,000.
    let line = "c".repeat(1_000);
    let text = format!(
        "{}{}\n",
        format!("{line}\n").repeat(1_600),
        "d".repeat(100_000)
    );
    std::fs::write(&log, &text).unwrap();
    // 261 lines of 1,000 bytes (1,003 as JSON) fill a 256 KiB batch: 7
    // batches, after the step read (CF12c: its evidence policy).
    let mut replies = vec![(200, Value::Null); 8];
    replies.push((200, Value::Null));
    replies.push((200, brief("exit")));
    let (host, server) = serve(replies);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--claim",
            &format!("{CLAIM}:1"),
            "--status",
            "succeeded",
            "--log-file",
            log.to_str().unwrap(),
            "--idempotency-key",
            "rep",
            "--json",
        ],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    let batches = log_batches(&seen);
    let keys: Vec<String> = batches.iter().map(|(k, _)| k.clone()).collect();
    let expected: Vec<String> = (1..=7).map(|n| format!("rep-logs-{n}")).collect();
    assert_eq!(keys, expected);
    let entries: Vec<String> = batches.into_iter().flat_map(|(_, e)| e).collect();
    assert_eq!(entries.concat(), text.replace('\n', ""));
    assert_eq!(entries.len(), 1_601, "the long line whole (round 2)");
    assert!(seen[0].starts_with(&format!("GET /v1/step-runs/{SR} ")));
    let report = &seen[8];
    assert!(report.starts_with(&format!("POST /v1/step-runs/{SR}/report ")));
    assert_eq!(header(report, "idempotency-key"), Some("rep"));
    assert_eq!(body_of(report)["logs"], json!([]));
}
