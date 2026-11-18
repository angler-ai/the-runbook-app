//! The agent loop exactly as `skills/therunbook/SKILL.md` teaches it, driven
//! through the real `runbook` binary (CLI and `runbook mcp serve`) against
//! the real server on an owned PostgreSQL database, with nothing but a
//! stored execution credential pair (D32):
//!
//! - F60: the loop is "claim, then get". D32 stays: an execution token
//!   reading a ready step it has not claimed is refused with 403, so the
//!   documented order is the only one that works.
//! - F61 / Gate D: the final report ends the run and the server revokes the
//!   pair; the agent still gets `next_action: exit` (exit 0, no error),
//!   from the report and from a later `step next` / `get_next_steps`.
//! - F69: the publish permission probe relies on the server answering
//!   `428 request.if_match_required` only after authorizing.
//!
//! Ignored by default: needs DATABASE_URL and creates/drops its own
//! `rb_test_` database.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;

use runbook_server::config::ServerConfig;
use runbook_server::drain::{DrainSettings, DrainWorker};
use runbook_store_postgres::PostgresStore;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::panic::AssertUnwindSafe;
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::time::{Duration, Instant};

const SPEC: &str = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: agent-loop, title: Agent loop}
spec:
  inputs:
    service: {type: string, required: true}
  steps:
    - id: check
      title: Check the service
      actor: agent
      instructions: Call /healthz on ${{ inputs.service }} and record the status code.
      outputs:
        status_code: {type: integer, required: true}
    - id: notify
      title: Post the result
      actor: agent
      instructions: Post the result to the channel.
"#;

/// The server (HTTP plus drain) on an owned database, and the bootstrap
/// owner's tokens. The HTTP thread is left running; the owned database is
/// dropped with FORCE at the end.
struct Stack {
    host: String,
    owner: String,
    service: String,
    _drain: DrainWorker,
}

async fn stack(db: &database::TestDatabase) -> Stack {
    let mut store = PostgresStore::connect(db.config()).await.unwrap();
    store.migrate().await.unwrap();
    drop(store);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let host = format!("http://127.0.0.1:{port}");
    let config =
        ServerConfig::new(db.config().clone(), &host, &format!("127.0.0.1:{port}")).unwrap();
    let issued = runbook_server::bootstrap::run(&config, &Default::default())
        .await
        .unwrap();
    let mut bytes = vec![];
    issued.write_once(&mut bytes).unwrap();
    let issued: Value = serde_json::from_slice(&bytes).unwrap();
    let drain = DrainWorker::start(
        config.clone(),
        DrainSettings {
            batch: 64,
            lease_ms: 30_000,
            idle: Duration::from_millis(20),
        },
    )
    .unwrap();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(runbook_server::http::serve(config))
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while ureq::get(&format!("{host}/v1/meta")).call().is_err() {
        assert!(Instant::now() < deadline, "server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Stack {
        host,
        owner: issued["pat"].as_str().unwrap().into(),
        service: issued["service"].as_str().unwrap().into(),
        _drain: drain,
    }
}

/// Run `body` against a fresh stack, then always drop the owned database.
fn with_stack(body: impl FnOnce(&Stack)) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut db = runtime
        .block_on(database::TestDatabase::create_from_env())
        .unwrap();
    let stack = runtime.block_on(stack(&db));
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| body(&stack)));
    runtime.block_on(db.finish(result));
}

/// One person's or agent's CLI: its own config directory.
struct Cli {
    host: String,
    home: tempfile::TempDir,
}

impl Cli {
    fn new(host: &str) -> Self {
        Self {
            host: host.into(),
            home: tempfile::tempdir().unwrap(),
        }
    }
    fn command(&self, token: Option<&str>) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_runbook"));
        c.args(["--host", &self.host])
            .env_remove("RUNBOOK_TOKEN")
            .env_remove("RUNBOOK_HOST")
            .env("XDG_CONFIG_HOME", self.home.path())
            .env("HOME", self.home.path());
        if let Some(token) = token {
            c.env("RUNBOOK_TOKEN", token);
        }
        c
    }
    fn run(&self, token: Option<&str>, args: &[&str]) -> Output {
        self.command(token)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
    /// A `--json` command that must succeed; its stdout as JSON.
    fn json(&self, token: Option<&str>, args: &[&str]) -> Value {
        let o = self.run(token, args);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", show(&o));
        serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("{args:?}: {}", show(&o)))
    }
}

fn show(o: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Publish the runbook and start a run as the owner; wait until it leaves
/// `queued`, then mint the agent's pair with the launcher's service token
/// into `agent`'s credential store. Returns the execution ID.
fn start_run(stack: &Stack, agent: &Cli) -> String {
    start_named(
        stack,
        agent,
        "agent-loop",
        SPEC,
        &["--input", "service=api"],
    )
}

/// [`start_run`] for the runbook `id` with `source`, started with `args`.
fn start_named(stack: &Stack, agent: &Cli, id: &str, source: &str, args: &[&str]) -> String {
    let owner = Cli::new(&stack.host);
    let file = owner.home.path().join(format!("{id}.yaml"));
    std::fs::write(&file, source).unwrap();
    let file = file.to_str().unwrap();
    owner.json(
        Some(&stack.owner),
        &["push", file, "--draft", "--project", "default", "--json"],
    );
    let published = owner.run(Some(&stack.owner), &["publish", id, "--project", "default"]);
    assert_eq!(published.status.code(), Some(0), "{}", show(&published));
    let mut start = vec!["run", "start", id, "--project", "default"];
    start.extend_from_slice(args);
    start.push("--json");
    let started = owner.json(Some(&stack.owner), &start);
    let exe = started["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let run = owner.json(Some(&stack.owner), &["run", "get", &exe, "--json"]);
        if run["status"] != "queued" {
            break;
        }
        assert!(Instant::now() < deadline, "{exe} stayed queued");
        std::thread::sleep(Duration::from_millis(100));
    }
    let minted = agent.run(
        Some(&stack.service),
        &["run", "token", &exe, "--session", "agent-1"],
    );
    assert_eq!(minted.status.code(), Some(0), "{}", show(&minted));
    exe
}

fn outputs_for(path: &str) -> &'static [&'static str] {
    match path {
        "check" => &["--output", "status_code=200"],
        _ => &[],
    }
}

#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn the_documented_cli_loop_runs_to_a_clean_exit_with_only_an_execution_pair() {
    with_stack(|stack| {
        let agent = Cli::new(&stack.host);
        let exe = start_run(stack, &agent);
        let who = agent.json(None, &["whoami", "--json"]);
        assert_eq!(who["credential"], "execution");

        // D32 stays: a ready step the pair has not claimed cannot be read.
        let brief = agent.json(None, &["step", "next", &exe, "--json"]);
        assert_eq!(brief["next_action"], "claim_ready_work", "{brief}");
        let first = brief["ready_work"][0]["step_run_id"].as_str().unwrap();
        let early = agent.run(None, &["step", "get", first, "--json"]);
        assert_ne!(early.status.code(), Some(0), "{}", show(&early));
        assert!(
            String::from_utf8_lossy(&early.stderr).contains("auth.forbidden"),
            "{}",
            show(&early)
        );

        // The loop as SKILL.md teaches it.
        let mut reported = vec![];
        let mut next = brief["next_action"].clone();
        let mut brief = brief;
        while next == "claim_ready_work" {
            let item = &brief["ready_work"][0];
            let sr = item["step_run_id"].as_str().unwrap().to_owned();
            let path = item["path"].as_str().unwrap().to_owned();
            agent.json(
                None,
                &[
                    "step",
                    "claim",
                    &sr,
                    "--session",
                    "agent-1",
                    "--agent-name",
                    "e2e-agent",
                    "--model",
                    "manual",
                    "--json",
                ],
            );
            let step = agent.json(None, &["step", "get", &sr, "--json"]);
            assert!(
                step["instructions"].as_str().is_some_and(|i| !i.is_empty()),
                "{step}"
            );
            // A claimed step is work in progress, not more ready work.
            assert_ne!(step["next_action"], "claim_ready_work", "{step}");
            let mut args = vec!["step", "report", &sr, "--status", "succeeded", "--json"];
            args.extend_from_slice(outputs_for(&path));
            let report = agent.json(None, &args);
            reported.push(path);
            next = report["next_action"].clone();
            if next == "claim_ready_work" {
                brief = agent.json(None, &["step", "next", &exe, "--json"]);
                next = brief["next_action"].clone();
            }
        }
        assert_eq!(reported, ["check", "notify"]);
        // The final report ended the run and the server revoked the pair:
        // that is a clean exit, not an error.
        assert_eq!(next, "exit");
        let after = agent.run(None, &["step", "next", &exe, "--json"]);
        assert_eq!(after.status.code(), Some(0), "{}", show(&after));
        let after: Value = serde_json::from_slice(&after.stdout).unwrap();
        assert_eq!(after["next_action"], "exit", "{after}");

        let owner = Cli::new(&stack.host);
        let run = owner.json(Some(&stack.owner), &["run", "get", &exe, "--json"]);
        assert_eq!(run["status"], "succeeded", "{run}");
    });
}

const LOG_SPEC: &str = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: log-evidence, title: Log evidence}
spec:
  steps:
    - id: build
      title: Build
      actor: agent
      evidence: {required: [log]}
    - id: secret
      title: Log a secret
      actor: agent
      evidence: {required: [log]}
    - id: large
      title: Attach a large log
      actor: agent
      evidence: {required: [log]}
      outputs:
        ok: {type: boolean, required: true}
"#;

/// Every line appended to `sr` through the logs route, read as the owner.
fn read_log(stack: &Stack, sr: &str) -> Vec<String> {
    let mut lines = vec![];
    let mut cursor = "0".to_owned();
    loop {
        let page: Value = ureq::get(&format!(
            "{}/v1/step-runs/{sr}/logs?cursor={cursor}&limit=1000",
            stack.host
        ))
        .set("Authorization", &format!("Bearer {}", stack.owner))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
        for line in page["lines"].as_array().unwrap() {
            lines.push(line["line"].as_str().unwrap().to_owned());
        }
        if page["has_more"] != true {
            return lines;
        }
        cursor = page["next_cursor"].as_str().unwrap().to_owned();
    }
}

/// The next ready step, claimed by `agent`.
fn claim_next(agent: &Cli, exe: &str, path: &str) -> String {
    let brief = agent.json(None, &["step", "next", exe, "--json"]);
    let item = &brief["ready_work"][0];
    assert_eq!(item["path"], path, "{brief}");
    let sr = item["step_run_id"].as_str().unwrap().to_owned();
    agent.json(
        None,
        &[
            "step",
            "claim",
            &sr,
            "--session",
            "agent-1",
            "--agent-name",
            "e2e-agent",
            "--model",
            "manual",
            "--json",
        ],
    );
    sr
}

/// CF12b review round 2, end to end through the real binary and server:
///
/// - finding 1: required `log` evidence is met by lines appended through
///   the logs route, by `step exec` and by `step report --log-file` with a
///   file past the report's 1 MiB;
/// - finding 2: the review's reproduction (a secret after 65,518 bytes)
///   sent with `step log`: no stored entry holds any part of the secret;
/// - finding 3: a report refused once, then corrected and run again with
///   the same log file, appends its lines once.
#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn appended_logs_meet_log_evidence_and_stay_redacted_and_single() {
    with_stack(|stack| {
        let agent = Cli::new(&stack.host);
        let exe = start_named(stack, &agent, "log-evidence", LOG_SPEC, &[]);

        let build = claim_next(&agent, &exe, "build");
        let ran = agent.run(
            None,
            &[
                "step",
                "exec",
                &build,
                "--json",
                "--",
                "sh",
                "-c",
                "echo compiled; echo linked",
            ],
        );
        assert_eq!(ran.status.code(), Some(0), "{}", show(&ran));
        assert_eq!(read_log(stack, &build), ["compiled", "linked"]);

        let secret = claim_next(&agent, &exe, "secret");
        let line = format!(
            "{} password=hunter2SUPERSECRETVALUE done",
            "x".repeat(65_518)
        );
        let mut log = agent
            .command(None)
            .args(["step", "log", &secret])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        log.stdin
            .take()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
        let logged = log.wait_with_output().unwrap();
        assert_eq!(logged.status.code(), Some(0), "{}", show(&logged));
        let reported = agent.run(None, &["step", "report", &secret, "--status", "succeeded"]);
        assert_eq!(reported.status.code(), Some(0), "{}", show(&reported));
        let stored = read_log(stack, &secret);
        assert!(stored.iter().all(|e| e.len() <= 65_536));
        for part in ["hunter2", "SUPERSECRET", "UPERSECRETVALUE", "SECRETVALUE"] {
            assert!(
                stored.iter().all(|e| !e.contains(part)),
                "{part} stored: {:?}",
                stored
                    .iter()
                    .map(|e| &e[e.len().saturating_sub(60)..])
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            stored
                .concat()
                .contains("password=[REDACTED:password] done")
        );

        let large = claim_next(&agent, &exe, "large");
        let file = agent.home.path().join("large.log");
        let lines: Vec<String> = (0..12_000)
            .map(|n| format!("{n:05} {}", "l".repeat(94)))
            .collect();
        std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        let file = file.to_str().unwrap();
        let refused = agent.run(
            None,
            &[
                "step",
                "report",
                &large,
                "--status",
                "succeeded",
                "--log-file",
                file,
            ],
        );
        assert_ne!(refused.status.code(), Some(0), "{}", show(&refused));
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("step.invalid_outputs"),
            "{}",
            show(&refused)
        );
        let fixed = agent.run(
            None,
            &[
                "step",
                "report",
                &large,
                "--status",
                "succeeded",
                "--log-file",
                file,
                "--output",
                "ok=true",
            ],
        );
        assert_eq!(fixed.status.code(), Some(0), "{}", show(&fixed));
        assert_eq!(read_log(stack, &large), lines, "each line once, in order");

        let owner = Cli::new(&stack.host);
        let run = owner.json(Some(&stack.owner), &["run", "get", &exe, "--json"]);
        assert_eq!(run["status"], "succeeded", "{run}");
    });
}

const RETRY_SPEC: &str = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: report-retry, title: Report retry}
spec:
  steps:
    - id: big
      title: Attach a large log
      actor: agent
      evidence: {required: [log]}
    - id: last
      title: Last
      actor: agent
"#;

/// CF12c review round 1 (F2), end to end with only an execution pair:
/// `step report --log-file` past one report, with a key, then the same
/// command again as after a lost response. The first report ended the
/// claim, so the step read the CLI makes first (the evidence policy) is
/// refused for the execution token. The rerun falls through to the keyed
/// requests, which replay: exit 0, and each line stored once. Before, the
/// rerun failed on the read.
#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn a_log_file_report_rerun_after_a_lost_response_replays() {
    with_stack(|stack| {
        let agent = Cli::new(&stack.host);
        let exe = start_named(stack, &agent, "report-retry", RETRY_SPEC, &[]);
        let brief = agent.json(None, &["step", "next", &exe, "--json"]);
        let big = brief["ready_work"][0]["step_run_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let claimed = agent.json(
            None,
            &[
                "step",
                "claim",
                &big,
                "--session",
                "agent-1",
                "--agent-name",
                "e2e-agent",
                "--model",
                "manual",
                "--json",
            ],
        );
        // Named on the command line: a response lost in flight leaves the
        // stored claim in place, but a successful report clears it.
        let claim = format!(
            "{}:{}",
            claimed["claim"]["id"].as_str().unwrap(),
            claimed["claim"]["generation"]
        );
        let file = agent.home.path().join("big.log");
        let lines: Vec<String> = (0..12_000)
            .map(|n| format!("{n:05} {}", "l".repeat(94)))
            .collect();
        std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        let file = file.to_str().unwrap();
        let report = [
            "step",
            "report",
            &big,
            "--claim",
            &claim,
            "--status",
            "succeeded",
            "--log-file",
            file,
            "--idempotency-key",
            "retry-1",
            "--json",
        ];
        let first = agent.json(None, &report);
        // The claim has ended: the execution token may not read the step.
        let read = agent.run(None, &["step", "get", &big, "--json"]);
        assert_ne!(read.status.code(), Some(0), "{}", show(&read));
        let again = agent.json(None, &report);
        assert_eq!(again["step_run_id"], first["step_run_id"], "{again}");
        assert_eq!(read_log(stack, &big), lines, "each line once, in order");
        let next = claim_next(&agent, &exe, "last");
        let done = agent.run(None, &["step", "report", &next, "--status", "succeeded"]);
        assert_eq!(done.status.code(), Some(0), "{}", show(&done));
    });
}

const CUT_SPEC: &str = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: cut-secrets, title: Cut secrets}
spec:
  steps:
    - {id: json, title: JSON, actor: agent}
    - {id: spaced, title: Spaced, actor: agent}
    - {id: quoted, title: Quoted, actor: agent}
    - {id: bearer, title: Bearer, actor: agent}
"#;

/// CF12b's final review, item 1, end to end through the real binary and
/// server: a line over one request (2 MiB as JSON) holding a secret whose
/// key and value a whitespace cut separates, sent with `step log` and with
/// MCP `append_log`. For each pattern the cut falls between the key and
/// its value. No stored entry, alone or joined with the next, holds any
/// five bytes of the secret. Before CF12c, the value was stored in clear.
#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn a_cut_line_never_stores_a_secret_apart_from_its_key() {
    use runbook_mcp::logs::{LINE_JSON_BYTES, json_len};
    let cases = [
        (
            "json",
            r#""password": "hunter2SUPERSECRETVALUE""#,
            "hunter2SUPERSECRETVALUE",
        ),
        (
            "spaced",
            "password = hunter2SUPERSECRETVALUE",
            "hunter2SUPERSECRETVALUE",
        ),
        (
            "quoted",
            r#"password="correct horse battery staple SUPERSECRETVALUE""#,
            "correct horse battery staple SUPERSECRETVALUE",
        ),
        (
            "bearer",
            "Authorization: Bearer abcdefSUPERSECRETVALUE",
            "abcdefSUPERSECRETVALUE",
        ),
    ];
    // The cut's content budget ends three bytes after the text's last
    // whitespace, so a cut after whitespace falls right there.
    let straddling = |text: &str| {
        let after_space = text.rfind(' ').unwrap() + 1;
        let lead = json_len(&text[..after_space + 3]) - 2;
        format!(
            "{}{text} {}",
            "x".repeat(LINE_JSON_BYTES - 2 - lead),
            "y".repeat(1_000)
        )
    };
    let leaks = |stored: &[String], secret: &str| {
        let pairs: Vec<String> = stored.windows(2).map(|w| w.concat()).collect();
        secret
            .as_bytes()
            .windows(5)
            .map(|w| std::str::from_utf8(w).unwrap().to_owned())
            .filter(|w| !w.contains(' '))
            .find(|w| stored.iter().chain(&pairs).any(|e| e.contains(w.as_str())))
    };
    with_stack(|stack| {
        let agent = Cli::new(&stack.host);
        let exe = start_named(stack, &agent, "cut-secrets", CUT_SPEC, &[]);
        let mut mcp = Mcp::start(&agent);
        for (path, text, secret) in cases {
            let line = straddling(text);
            let brief = agent.json(None, &["step", "next", &exe, "--json"]);
            let item = &brief["ready_work"][0];
            assert_eq!(item["path"], path, "{brief}");
            let sr = item["step_run_id"].as_str().unwrap().to_owned();
            let claimed = agent.json(
                None,
                &[
                    "step",
                    "claim",
                    &sr,
                    "--session",
                    "agent-1",
                    "--agent-name",
                    "e2e-agent",
                    "--model",
                    "manual",
                    "--json",
                ],
            );
            let claim = json!({
                "id": claimed["claim"]["id"],
                "generation": claimed["claim"]["generation"],
            });

            let mut log = agent
                .command(None)
                .args(["step", "log", &sr])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            log.stdin
                .take()
                .unwrap()
                .write_all(format!("{line}\nafter-cli\n").as_bytes())
                .unwrap();
            let logged = log.wait_with_output().unwrap();
            assert_eq!(logged.status.code(), Some(0), "{}", show(&logged));
            assert!(
                String::from_utf8_lossy(&logged.stderr).contains("larger than one request"),
                "{}",
                show(&logged)
            );

            let (error, appended, out) = mcp.tool(
                "append_log",
                json!({
                    "step_run_id": sr,
                    "claim": claim,
                    "text": format!("{line}\nafter-mcp"),
                    "idempotency_key": format!("cut-{path}"),
                }),
            );
            assert!(!error, "{}", &out[..out.len().min(400)]);
            assert_eq!(appended["cut_lines"], 1, "{}", &out[..out.len().min(400)]);

            let stored = read_log(stack, &sr);
            assert!(stored.iter().all(|e| e.len() <= 65_536));
            assert_eq!(leaks(&stored, secret), None, "{path}");
            let joined = stored.concat();
            assert_eq!(joined.matches("[REDACTED:").count(), 2, "{path}");
            // Redacted by the client, then again by the server: unchanged.
            assert!(!joined.contains("]]"), "{path}");
            assert!(joined.contains("after-cli") && joined.ends_with("after-mcp"));
            let reported = agent.run(None, &["step", "report", &sr, "--status", "succeeded"]);
            assert_eq!(reported.status.code(), Some(0), "{}", show(&reported));
        }
    });
}

/// One `runbook mcp serve` session, one request and one response at a time.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(cli: &Cli) -> Self {
        let mut child = cli
            .command(None)
            .args(["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        mcp.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "1"}}),
        );
        mcp
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let message =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        writeln!(self.stdin, "{message}").unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    /// A tool call: (isError, the control line, the full text).
    fn tool(&mut self, name: &str, arguments: Value) -> (bool, Value, String) {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &response["result"];
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        let control = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        (result["isError"] == json!(true), control, text)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn the_documented_mcp_loop_runs_to_a_clean_exit_with_only_an_execution_pair() {
    with_stack(|stack| {
        let agent = Cli::new(&stack.host);
        let exe = start_run(stack, &agent);
        let mut mcp = Mcp::start(&agent);

        let (error, brief, text) = mcp.tool("get_next_steps", json!({"execution_id": exe}));
        assert!(!error, "{text}");
        assert_eq!(brief["next_action"], "claim_ready_work", "{text}");
        // D32 stays: get_step before claim_step is refused.
        let first = brief["ready_work"][0]["step_run_id"].as_str().unwrap();
        let (error, _, text) = mcp.tool("get_step", json!({"step_run_id": first}));
        assert!(error && text.contains("auth.forbidden"), "{text}");

        let mut reported = vec![];
        let mut brief = brief;
        let mut next = brief["next_action"].clone();
        while next == "claim_ready_work" {
            let item = brief["ready_work"][0].clone();
            let sr = item["step_run_id"].as_str().unwrap();
            let path = item["path"].as_str().unwrap().to_owned();
            let (error, claimed, text) = mcp.tool(
                "claim_step",
                json!({"step_run_id": sr, "session_id": "agent-1", "agent_name": "e2e-agent", "model": "manual"}),
            );
            assert!(!error, "{text}");
            let claim = claimed["claim"].clone();
            let (error, step, text) = mcp.tool("get_step", json!({"step_run_id": sr}));
            assert!(!error, "{text}");
            assert!(
                step["instructions"].as_str().is_some_and(|i| !i.is_empty())
                    || text.contains("instructions"),
                "{text}"
            );
            assert_ne!(step["next_action"], "claim_ready_work", "{text}");
            let outputs = if path == "check" {
                json!({"status_code": 200})
            } else {
                json!({})
            };
            let (error, report, text) = mcp.tool(
                "report_step",
                json!({"step_run_id": sr, "claim": claim, "status": "succeeded", "outputs": outputs}),
            );
            assert!(!error, "{text}");
            reported.push(path);
            next = report["next_action"].clone();
            if next == "claim_ready_work" {
                let (error, b, text) = mcp.tool("get_next_steps", json!({"execution_id": exe}));
                assert!(!error, "{text}");
                next = b["next_action"].clone();
                brief = b;
            }
        }
        assert_eq!(reported, ["check", "notify"]);
        assert_eq!(next, "exit");
        let (error, after, text) = mcp.tool("get_next_steps", json!({"execution_id": exe}));
        assert!(!error, "{text}");
        assert_eq!(after["next_action"], "exit", "{text}");
    });
}

fn raw_publish(host: &str, token: &str, runbook_id: &str) -> (u16, Value) {
    let response = ureq::post(&format!("{host}/v1/runbooks/{runbook_id}/versions"))
        .set("authorization", &format!("Bearer {token}"))
        .set("idempotency-key", "e2e-publish-probe")
        .send_json(json!({}));
    let (status, body) = match response {
        Ok(r) => (r.status(), r.into_string().unwrap()),
        Err(ureq::Error::Status(status, r)) => (status, r.into_string().unwrap()),
        Err(e) => panic!("{e}"),
    };
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn publish_answers_if_match_required_only_after_authorizing() {
    with_stack(|stack| {
        let owner = Cli::new(&stack.host);
        let file = owner.home.path().join("agent-loop.yaml");
        std::fs::write(&file, SPEC).unwrap();
        let file = file.to_str().unwrap();
        let saved = owner.json(
            Some(&stack.owner),
            &["push", file, "--draft", "--project", "default", "--json"],
        );
        let rb = saved["runbook_id"].as_str().unwrap().to_owned();
        // A token without runbooks:publish is refused before the
        // precondition is looked at.
        let narrow = owner.json(
            Some(&stack.owner),
            &[
                "token",
                "create",
                "--name",
                "no-publish",
                "--scope",
                "runbooks:read",
                "--scope",
                "runbooks:write",
                "--project",
                "default",
                "--json",
            ],
        );
        let narrow = narrow["secret"].as_str().expect("token secret").to_owned();
        let (status, body) = raw_publish(&stack.host, &narrow, &rb);
        assert_eq!(status, 403, "{body}");
        // An authorized caller without If-Match gets 428 and nothing is
        // published: that is what the CLI's publish probe relies on.
        let (status, body) = raw_publish(&stack.host, &stack.owner, &rb);
        assert_eq!(
            (status, body["code"].as_str()),
            (428, Some("request.if_match_required")),
            "{body}"
        );
        let view = owner.json(Some(&stack.owner), &["get", &rb, "--json"]);
        assert!(view["latest_version"].is_null(), "{view}");
    });
}

#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn skill_push_list_pull_install_e2e() {
    with_stack(|stack| {
        let owner = Cli::new(&stack.host);
        let skill_dir = owner.home.path().join("e2e-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        let skill_content =
            "---\nname: e2e-skill\ndescription: End-to-end skill test\n---\n# E2E Skill\n";
        std::fs::write(skill_dir.join("SKILL.md"), skill_content).unwrap();

        // 1. Push
        let pushed = owner.json(
            Some(&stack.owner),
            &[
                "skill",
                "push",
                skill_dir.to_str().unwrap(),
                "--project",
                "default",
                "--json",
            ],
        );
        let hash = pushed["hash"].as_str().expect("skill hash").to_owned();
        assert_eq!(hash.len(), 64, "{pushed}");

        // 2. List
        let list = owner.json(
            Some(&stack.owner),
            &["skill", "list", "--project", "default", "--json"],
        );
        let items = list["items"].as_array().expect("items array");
        let found = items
            .iter()
            .find(|i| i["name"] == "e2e-skill")
            .expect("skill in list");
        assert_eq!(found["hash"], hash);

        // 3. Pull
        let pull_dir = owner.home.path().join("pulled-skill");
        let pull_out = owner.run(
            Some(&stack.owner),
            &[
                "skill",
                "pull",
                "e2e-skill",
                "--project",
                "default",
                "--out",
                pull_dir.to_str().unwrap(),
            ],
        );
        assert_eq!(
            pull_out.status.code(),
            Some(0),
            "pull failed: {}",
            show(&pull_out)
        );
        let pulled_content = std::fs::read_to_string(pull_dir.join("SKILL.md")).unwrap();
        assert_eq!(pulled_content, skill_content);

        // 4. Install project skill
        let install_out = owner.run(
            Some(&stack.owner),
            &[
                "skill",
                "install",
                "--agent",
                "claude",
                "--skill",
                "e2e-skill",
                "--project",
                "default",
            ],
        );
        assert_eq!(
            install_out.status.code(),
            Some(0),
            "install failed: {}",
            show(&install_out)
        );
        let installed_file = owner.home.path().join(".claude/skills/e2e-skill/SKILL.md");
        assert!(installed_file.exists());
        assert_eq!(
            std::fs::read_to_string(&installed_file).unwrap(),
            skill_content
        );

        // 5. Install therunbook skill offline
        let install_trb = owner.run(
            None,
            &["skill", "install", "--agent", "claude", "--therunbook"],
        );
        assert_eq!(
            install_trb.status.code(),
            Some(0),
            "install therunbook failed: {}",
            show(&install_trb)
        );
        let trb_file = owner.home.path().join(".claude/skills/therunbook/SKILL.md");
        assert!(trb_file.exists());
    });
}
