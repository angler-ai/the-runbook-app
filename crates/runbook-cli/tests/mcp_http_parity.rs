//! MCP1: the same fresh-connection trace over stdio (`runbook mcp serve`)
//! and over the remote endpoint (`POST /mcp`) gives the same results, against
//! the real server on an owned PostgreSQL database.
//!
//! The trace: `initialize`, `notifications/initialized`, `tools/list`,
//! `start_execution`, `get_next_steps`, `claim_step`, `get_step`,
//! `report_step`, `resources/list`, `resources/read`, `prompts/list` and
//! `prompts/get`. Each transport starts its own run, so IDs, data-block
//! nonces, idempotency keys and timestamps are normalized before comparing.
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
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const SPEC: &str = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: parity, title: Parity}
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

struct Stack {
    host: String,
    owner: String,
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
        _drain: drain,
    }
}

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

/// One MCP connection: send a message, get its response (none for a
/// notification).
trait Transport {
    fn send(&mut self, message: &Value) -> Option<Value>;
}

/// `runbook mcp serve` with the token in RUNBOOK_TOKEN and a fresh home.
struct StdioServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _home: tempfile::TempDir,
}

impl StdioServer {
    fn start(host: &str, token: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_runbook"))
            .args(["--host", host, "mcp", "serve"])
            .env_remove("RUNBOOK_HOST")
            .env("RUNBOOK_TOKEN", token)
            .env("XDG_CONFIG_HOME", home.path())
            .env("HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            _home: home,
        }
    }
}

impl Transport for StdioServer {
    fn send(&mut self, message: &Value) -> Option<Value> {
        writeln!(self.stdin, "{message}").unwrap();
        message.get("id")?;
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        Some(serde_json::from_str(&line).unwrap())
    }
}

impl Drop for StdioServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `POST /mcp` with a bearer; no state between requests.
struct Http {
    url: String,
    token: String,
    version: Option<String>,
}

impl Transport for Http {
    fn send(&mut self, message: &Value) -> Option<Value> {
        let mut request = ureq::post(&self.url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json, text/event-stream")
            .set("Content-Type", "application/json");
        if let Some(v) = &self.version {
            request = request.set("MCP-Protocol-Version", v);
        }
        let response = request
            .send_string(&message.to_string())
            .unwrap_or_else(|e| panic!("{message}: {e}"));
        assert!(
            response.header("mcp-session-id").is_none(),
            "the server must not issue a session"
        );
        if response.status() == 202 {
            assert!(message.get("id").is_none(), "{message}");
            return None;
        }
        assert_eq!(response.status(), 200, "{message}");
        assert_eq!(
            response.header("content-type"),
            Some("application/json"),
            "{message}"
        );
        let body: Value = response.into_json().unwrap();
        if message["method"] == "initialize" {
            self.version = body["result"]["protocolVersion"]
                .as_str()
                .map(str::to_owned);
        }
        Some(body)
    }
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}
fn tool(id: u64, name: &str, arguments: Value) -> Value {
    request(
        id,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}
fn control(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("not a tool result: {response}"));
    serde_json::from_str(text.lines().next().unwrap()).unwrap()
}

/// The trace on one fresh connection. `start_key` differs per transport so
/// each starts its own run. Returns every response, in order.
fn trace(t: &mut dyn Transport, start_key: &str) -> Vec<Value> {
    let mut out = vec![];
    let mut push = |r: Option<Value>| {
        let r = r.expect("a request gets a response");
        assert!(r.get("error").is_none(), "{r}");
        out.push(r.clone());
        r
    };
    push(t.send(&request(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "parity", "version": "1"}}),
    )));
    assert!(
        t.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .is_none()
    );
    push(t.send(&request(2, "tools/list", json!({}))));
    let started = push(t.send(&tool(
        3,
        "start_execution",
        json!({"project": "default", "runbook": "parity", "inputs": {"service": "api"}, "idempotency_key": start_key}),
    )));
    let exe = control(&started)["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{started}"))
        .to_owned();
    // Not part of the trace: wait until the drain has made the first step
    // ready.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let brief = t
            .send(&tool(99, "get_next_steps", json!({"execution_id": exe})))
            .unwrap();
        if control(&brief)["next_action"] == "claim_ready_work" {
            break;
        }
        assert!(Instant::now() < deadline, "{exe}: no ready work: {brief}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let brief = push(t.send(&tool(4, "get_next_steps", json!({"execution_id": exe}))));
    assert_eq!(
        control(&brief)["next_action"],
        "claim_ready_work",
        "{brief}"
    );
    let sr = control(&brief)["ready_work"][0]["step_run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let claimed = push(t.send(&tool(
        5,
        "claim_step",
        json!({"step_run_id": sr, "session_id": "parity-1", "agent_name": "parity-agent", "model": "manual"}),
    )));
    let claim = control(&claimed)["claim"].clone();
    push(t.send(&tool(6, "get_step", json!({"step_run_id": sr}))));
    let report = push(t.send(&tool(
        7,
        "report_step",
        json!({"step_run_id": sr, "claim": claim, "status": "succeeded", "outputs": {"status_code": 200}, "summary": "healthz answered 200"}),
    )));
    assert_eq!(
        control(&report)["next_action"],
        "claim_ready_work",
        "{report}"
    );
    let resources = push(t.send(&request(8, "resources/list", json!({}))));
    let uri = resources["result"]["resources"][0]["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("{resources}"))
        .to_owned();
    push(t.send(&request(9, "resources/read", json!({"uri": uri}))));
    push(t.send(&request(10, "prompts/list", json!({}))));
    push(t.send(&request(
        11,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": "parity", "project": "default", "inputs": "{\"service\":\"api\"}"}}),
    )));
    out
}

/// IDs (`exe_…`, `sr_…`, `clm_…`), 32-hex nonces and keys, and timestamps
/// become placeholders; everything else must match exactly.
fn normalize(responses: &[Value]) -> Vec<String> {
    let id = regex::Regex::new(r"\b([a-z]{2,4})_[0-7][0-9A-HJKMNP-TV-Z]{25}\b").unwrap();
    let hex = regex::Regex::new(r"\b[0-9a-f]{32}\b").unwrap();
    let iso = regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z").unwrap();
    let ms = regex::Regex::new(r"\b1\d{12}\b").unwrap();
    responses
        .iter()
        .map(|r| {
            let text = serde_json::to_string_pretty(r).unwrap();
            let text = id.replace_all(&text, "${1}_<id>");
            let text = hex.replace_all(&text, "<hex>");
            let text = iso.replace_all(&text, "<time>");
            ms.replace_all(&text, "<ms>").into_owned()
        })
        .collect()
}

#[test]
#[ignore = "requires DATABASE_URL; owned databases only"]
fn stdio_and_remote_mcp_give_the_same_trace() {
    with_stack(|stack| {
        // Publish the runbook once, as the owner.
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("parity.yaml");
        std::fs::write(&file, SPEC).unwrap();
        let cli = |args: &[&str]| {
            let o = Command::new(env!("CARGO_BIN_EXE_runbook"))
                .args(["--host", &stack.host])
                .args(args)
                .env_remove("RUNBOOK_HOST")
                .env("RUNBOOK_TOKEN", &stack.owner)
                .env("XDG_CONFIG_HOME", home.path())
                .env("HOME", home.path())
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert_eq!(
                o.status.code(),
                Some(0),
                "{args:?}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            o.stdout
        };
        cli(&[
            "push",
            file.to_str().unwrap(),
            "--draft",
            "--project",
            "default",
            "--json",
        ]);
        cli(&["publish", "parity", "--project", "default"]);
        // The agent: a PAT of actor kind agent, used on both transports.
        let created: Value = serde_json::from_slice(&cli(&[
            "token", "create", "--name", "parity", "--agent", "--json",
        ]))
        .unwrap();
        let agent = created["secret"].as_str().unwrap().to_owned();

        let mut stdio = StdioServer::start(&stack.host, &agent);
        let over_stdio = trace(&mut stdio, "parity-stdio");
        let mut http = Http {
            url: format!("{}/mcp", stack.host),
            token: agent.clone(),
            version: None,
        };
        let over_http = trace(&mut http, "parity-http");

        // Two distinct runs, so the comparison is not trivially one replay.
        assert_ne!(
            control(&over_stdio[2])["execution_id"],
            control(&over_http[2])["execution_id"]
        );
        // The trace carries real content on both sides.
        for responses in [&over_stdio, &over_http] {
            assert_eq!(responses.len(), 11);
            assert_eq!(responses[7]["result"]["resources"][0]["name"], "parity");
            assert!(
                responses[8]["result"]["contents"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("metadata: {id: parity")
            );
            let prompt = responses[10]["result"]["messages"][0]["content"]["text"]
                .as_str()
                .unwrap();
            assert!(prompt.contains("\"runbook\":\"parity\""), "{prompt}");
        }
        let (a, b) = (normalize(&over_stdio), normalize(&over_http));
        assert_eq!(a.len(), b.len());
        for (i, (a, b)) in a.iter().zip(&b).enumerate() {
            assert_eq!(a, b, "response {i} differs between stdio and HTTP");
        }
    });
}
