//! D2 pilot install kit (`examples/pilot/`, `scripts/check.sh`,
//! `docs/install.md`):
//!
//! - the recording receiver verifies `Runbook-Signature` (tampered, stale
//!   and wrong-secret deliveries are refused), deduplicates by event ID,
//!   logs event types and IDs and never a secret, and can stand in for the
//!   GitHub API so the GitHub Actions relay runs locally;
//! - `pilot-json.py` keeps secrets in 0600 files and prints only IDs;
//! - the shell scripts pass `bash -n`;
//! - every `runbook-server` command in the install guide parses;
//! - the interim first-token seed (`seed-first-token.sql`) issues a working
//!   credential (Postgres, owned database, ignored by default).
#[path = "support/examples.rs"]
mod examples;
use examples::{Program, deliver, example, python, raw, raw_post};
use runbook_core::webhooks::{encode_webhook_secret, signature_header};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ1";

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// A fresh private scratch directory under the system temp dir.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "runbook-d2-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn event(id: &str, kind: &str) -> String {
    json!({
        "id": id,
        "type": kind,
        "version": 1,
        "created_at": "2026-11-11T12:00:00.000Z",
        "org": "org_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "project": "prj_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "execution": {"id": EXE, "title": "Verify api v2.0.0", "url": "https://runbooks.example/executions/x"},
        "sequence": 4,
        "data": {
            "reason": "started",
            "ready_steps": [{"step_run_id": "sr_01JABCDEFGHJKMNPQRSTVWXYZ0", "path": "smoke.api_health", "title": "Health"}],
            "params": {"repo": "acme/api", "workflow": "runbook-resume.yml"}
        }
    })
    .to_string()
}

fn signed(url: &str, secret: &str, id: &str, kind: &str, body: &str, ts: i64) -> (u16, String) {
    let signature = signature_header(&[secret], ts, body.as_bytes());
    deliver(url, id, kind, &ts.to_string(), &signature, body)
}

fn log_records(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn receiver(env: &[(&str, String)]) -> Program {
    let mut all = vec![
        ("RECEIVER_HOST", "127.0.0.1".to_owned()),
        ("RECEIVER_PORT", "0".to_owned()),
    ];
    all.extend(env.iter().cloned());
    Program::start("../pilot/recording-receiver.py", &all)
}

#[test]
fn recording_receiver_verifies_deduplicates_and_logs_without_secrets() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let dir = scratch("receiver");
    let log = dir.join("events.jsonl");
    let secret = encode_webhook_secret([9; 32]);
    let other = encode_webhook_secret([8; 32]);
    let program = receiver(&[
        ("RUNBOOK_WEBHOOK_SECRET", secret.clone()),
        ("RECEIVER_LOG", log.display().to_string()),
    ]);
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXY01";
    let body = event(id, "agent.work_available");
    let (status, reply) = signed(
        &program.url,
        &secret,
        id,
        "agent.work_available",
        &body,
        now_s(),
    );
    assert_eq!(status, 200, "{reply}");
    // A redelivery of the same event is acknowledged, not recorded twice.
    let (status, reply) = signed(
        &program.url,
        &secret,
        id,
        "agent.work_available",
        &body,
        now_s(),
    );
    assert_eq!(
        (status, reply.contains("duplicate")),
        (200, true),
        "{reply}"
    );
    // Tampered body, stale timestamp, wrong secret and no signature.
    let tampered = body.replace("api_health", "api_health_x");
    let ts = now_s();
    let signature = signature_header(&[&secret], ts, body.as_bytes());
    let (status, _) = deliver(
        &program.url,
        id,
        "x",
        &ts.to_string(),
        &signature,
        &tampered,
    );
    assert_eq!(status, 401);
    let stale = "evt_01JABCDEFGHJKMNPQRSTVWXY02";
    let (status, _) = signed(
        &program.url,
        &secret,
        stale,
        "x",
        &event(stale, "x"),
        now_s() - 600,
    );
    assert_eq!(status, 401);
    let wrong = "evt_01JABCDEFGHJKMNPQRSTVWXY03";
    let (status, _) = signed(
        &program.url,
        &other,
        wrong,
        "x",
        &event(wrong, "x"),
        now_s(),
    );
    assert_eq!(status, 401);
    let (status, _) = deliver(&program.url, wrong, "x", &now_s().to_string(), "", "{}");
    assert_eq!(status, 401);
    // Malformed length is refused without stalling.
    let malformed = raw_post("-1", now_s().to_string().as_bytes(), b"v1=00", "");
    assert_eq!(raw(&program.url, &malformed), Some(400));
    // During a rotation the header carries one entry per secret.
    let rotated = "evt_01JABCDEFGHJKMNPQRSTVWXY04";
    let body2 = event(rotated, "approval.requested");
    let ts = now_s();
    let header = signature_header(&[&other, &secret], ts, body2.as_bytes());
    let (status, _) = deliver(
        &program.url,
        rotated,
        "approval.requested",
        &ts.to_string(),
        &header,
        &body2,
    );
    assert_eq!(status, 200);
    let records = log_records(&log);
    let events: Vec<(&str, &str)> = records
        .iter()
        .filter(|r| r["kind"] == "webhook")
        .map(|r| (r["type"].as_str().unwrap(), r["id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        events,
        vec![
            ("agent.work_available", id),
            ("approval.requested", rotated)
        ]
    );
    assert_eq!(records[0]["execution"], EXE);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains(&secret) && !text.contains("whsec_") && !text.contains("v1="));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the event log is private");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recording_receiver_stands_in_for_github_behind_the_relay() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let dir = scratch("relay");
    let log = dir.join("events.jsonl");
    let secret_file = dir.join("relay.whsec");
    let secret = encode_webhook_secret([4; 32]);
    // The receiver reads the secret file on each request: it can start
    // before the subscription (and its secret) exists.
    let program = receiver(&[
        (
            "RUNBOOK_WEBHOOK_SECRET_FILE",
            secret_file.display().to_string(),
        ),
        ("RECEIVER_LOG", log.display().to_string()),
        ("RECEIVER_MOCK_GITHUB", "1".to_owned()),
        ("RECEIVER_GITHUB_TOKEN", "pilot-dry-run".to_owned()),
    ]);
    std::fs::write(&secret_file, format!("{secret}\n")).unwrap();
    let relay = Program::start(
        "github-actions-relay/relay.py",
        &[
            ("RUNBOOK_WEBHOOK_SECRET", secret.clone()),
            ("GITHUB_TOKEN", "pilot-dry-run".to_owned()),
            ("GITHUB_API_URL", program.url.clone()),
            ("GITHUB_REPOSITORY", "acme/api".to_owned()),
            ("RELAY_ALLOWED_REPOS", "acme/api".to_owned()),
            ("RELAY_ALLOWED_WORKFLOWS", "runbook-resume.yml".to_owned()),
            ("RELAY_ALLOWED_REFS", "main".to_owned()),
            ("RELAY_HOST", "127.0.0.1".to_owned()),
            ("RELAY_PORT", "0".to_owned()),
        ],
    );
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXY05";
    let (status, reply) = signed(
        &relay.url,
        &secret,
        id,
        "agent.work_available",
        &event(id, "agent.work_available"),
        now_s(),
    );
    assert_eq!(status, 200, "{reply}");
    let records = log_records(&log);
    let dispatch: Vec<&Value> = records.iter().filter(|r| r["kind"] == "github").collect();
    assert_eq!(dispatch.len(), 1, "{records:?}");
    assert_eq!(dispatch[0]["repo"], "acme/api");
    assert_eq!(dispatch[0]["workflow"], "runbook-resume.yml");
    assert_eq!(dispatch[0]["ref"], "main");
    assert_eq!(dispatch[0]["execution_id"], EXE);
    assert_eq!(dispatch[0]["event_id"], id);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains("pilot-dry-run") && !text.contains(&secret));
    // The stand-in demands the configured bearer token.
    let forged = b"POST /repos/acme/api/actions/workflows/runbook-resume.yml/dispatches HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer pilot-dry-ruN\r\nContent-Length: 2\r\n\r\n{}";
    assert_eq!(raw(&program.url, forged), Some(401));
    assert_eq!(log_records(&log).len(), records.len());
    // Without the mock flag the GitHub path is not served.
    let plain = receiver(&[("RUNBOOK_WEBHOOK_SECRET", secret.clone())]);
    let dispatch = b"POST /repos/acme/api/actions/workflows/runbook-resume.yml/dispatches HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer x\r\nContent-Length: 2\r\n\r\n{}";
    assert_eq!(raw(&plain.url, dispatch), Some(401));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_github_stand_in_needs_a_token_and_binds_loopback_by_default() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    // Enabling the mock without a token is a configuration error: the
    // program exits instead of serving.
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(example("../pilot/recording-receiver.py"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUNBOOK_WEBHOOK_SECRET", encode_webhook_secret([1; 32]))
        .env("RECEIVER_MOCK_GITHUB", "1")
        .env("RECEIVER_PORT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the receiver served the GitHub stand-in without a token");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(!status.success());
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(stderr.contains("RECEIVER_GITHUB_TOKEN"), "{stderr}");
    // Without RECEIVER_HOST it listens on loopback only.
    let program = Program::start(
        "../pilot/recording-receiver.py",
        &[
            ("RUNBOOK_WEBHOOK_SECRET", encode_webhook_secret([1; 32])),
            ("RECEIVER_MOCK_GITHUB", "1".to_owned()),
            ("RECEIVER_GITHUB_TOKEN", "t".to_owned()),
            ("RECEIVER_PORT", "0".to_owned()),
        ],
    );
    assert!(
        program.url.starts_with("http://127.0.0.1:"),
        "{}",
        program.url
    );
}

#[test]
fn recording_receiver_answers_400_to_signed_events_of_the_wrong_shape() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let secret = encode_webhook_secret([3; 32]);
    let program = receiver(&[("RUNBOOK_WEBHOOK_SECRET", secret.clone())]);
    for (n, (field, value)) in [
        ("execution", json!("exe_01JABCDEFGHJKMNPQRSTVWXYZ1")),
        ("execution", json!([1])),
        ("data", json!([])),
        ("data", json!("x")),
        ("data", json!({"ready_steps": ["sr_x"]})),
        ("data", json!({"recipients": [7]})),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("evt_01JABCDEFGHJKMNPQRSTVWXY{:02}", 20 + n);
        let mut body: Value = serde_json::from_str(&event(&id, "approval.requested")).unwrap();
        body[field] = value;
        let body = body.to_string();
        let (status, reply) = signed(
            &program.url,
            &secret,
            &id,
            "approval.requested",
            &body,
            now_s(),
        );
        assert_eq!(status, 400, "{field}: {reply}");
    }
    // A well-formed event still records after the refusals.
    let id = "evt_01JABCDEFGHJKMNPQRSTVWXY30";
    let (status, _) = signed(
        &program.url,
        &secret,
        id,
        "x",
        &event(id, "approval.requested"),
        now_s(),
    );
    assert_eq!(status, 200);
}

/// A directory of stub programs (bash) put first on PATH.
fn stubs(dir: &Path, files: &[(&str, &str)]) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, body) in files {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/usr/bin/env bash\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
#[test]
fn seed_script_never_puts_the_database_url_or_password_on_a_command_line() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let dir = scratch("seed");
    let record = dir.join("psql.txt");
    let path = stubs(
        &dir,
        &[(
            "psql",
            &format!(
                "{{ printf 'ARG %s\\n' \"$@\"; env | grep '^PG' | sed 's/^/ENV /'; }} > '{}'\n\
                 cat > /dev/null\n\
                 for a in \"$@\"; do case $a in token_id=*) echo \"${{a#token_id=}}\";; esac; done\n",
                record.display()
            ),
        )],
    );
    let pilot = dir.join("pilot");
    let run = |name: &str, extra: &[(&str, &str)]| {
        let mut command = Command::new("bash");
        command
            .arg(repo("examples/pilot/seed-first-token.sh"))
            .args([
                name,
                "org_01JABCDEFGHJKMNPQRSTVWXYZ0",
                "usr_01JABCDEFGHJKMNPQRSTVWXYZ1",
                "prj_01JABCDEFGHJKMNPQRSTVWXYZ2",
            ])
            .env_clear()
            .env("PATH", &path)
            .env("PILOT_DIR", &pilot)
            .stdin(Stdio::null());
        for (k, v) in extra {
            command.env(k, v);
        }
        command.output().unwrap()
    };
    let url = "postgres://pilot%40ops:s3cr%3At-PW@db.example.internal:6543/runbook?sslmode=require";
    let out = run("alice", &[("DATABASE_URL", url)]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = std::fs::read_to_string(&record).unwrap();
    let args: Vec<&str> = seen
        .lines()
        .filter_map(|l| l.strip_prefix("ARG "))
        .collect();
    assert!(!args.is_empty());
    for arg in &args {
        assert!(
            !arg.contains("s3cr") && !arg.contains("postgres://") && !arg.contains("db.example"),
            "psql argument leaks the connection: {arg}"
        );
    }
    for expected in [
        "ENV PGPASSWORD=s3cr:t-PW",
        "ENV PGUSER=pilot@ops",
        "ENV PGHOST=db.example.internal",
        "ENV PGPORT=6543",
        "ENV PGDATABASE=runbook",
        "ENV PGSSLMODE=require",
    ] {
        assert!(seen.lines().any(|l| l == expected), "missing {expected}");
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stored = std::fs::read_to_string(pilot.join("alice.seed")).unwrap();
    assert!(stored.starts_with("trb_pat_") && !stdout.contains(stored.trim()));
    // A PILOT_PSQL that carries a URL or password is refused before psql runs.
    std::fs::remove_file(&record).unwrap();
    for psql in [
        "psql postgres://u:pw@h/db",
        "psql host=h password=pw dbname=db",
    ] {
        let out = run("bob", &[("PILOT_PSQL", psql)]);
        assert!(!out.status.success(), "{psql}");
        assert!(!record.exists(), "psql ran for {psql}");
        assert!(!pilot.join("bob.seed").exists());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

fn pilot_json(args: &[&str], stdin: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(example("../pilot/pilot-json.py"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn pilot_json_keeps_secrets_in_private_files_and_prints_only_ids() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let dir = scratch("json");
    let created = json!({"token": {"id": "tok_01JABCDEFGHJKMNPQRSTVWXYZ0", "name": "alice"}, "secret": "trb_pat_SECRETSECRET"}).to_string();
    let dir_text = dir.display().to_string();
    let out = pilot_json(&["keep", &dir_text, "secret=alice.pat"], &created);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("tok_01JABCDEFGHJKMNPQRSTVWXYZ0"),
        "{stdout}"
    );
    assert!(!stdout.contains("SECRET") && !String::from_utf8_lossy(&out.stderr).contains("SECRET"));
    let stored = dir.join("alice.pat");
    assert_eq!(
        std::fs::read_to_string(&stored).unwrap(),
        "trb_pat_SECRETSECRET\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&stored).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // Never overwrites a stored secret; never writes outside the directory.
    let again = pilot_json(&["keep", &dir_text, "secret=alice.pat"], &created);
    assert!(!again.status.success());
    assert_eq!(
        std::fs::read_to_string(&stored).unwrap(),
        "trb_pat_SECRETSECRET\n"
    );
    let escape = pilot_json(&["keep", &dir_text, "secret=../alice.pat"], &created);
    assert!(!escape.status.success());
    // A missing field stores nothing.
    let missing = pilot_json(&["keep", &dir_text, "nope=x.pat"], &created);
    assert!(!missing.status.success() && !dir.join("x.pat").exists());
    // `get` prints one non-secret field.
    let got = pilot_json(&["get", "token.id"], &created);
    assert_eq!(
        String::from_utf8_lossy(&got.stdout).trim(),
        "tok_01JABCDEFGHJKMNPQRSTVWXYZ0"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pilot_shell_scripts_pass_bash_syntax_check() {
    let Ok(status) = Command::new("bash")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
    else {
        eprintln!("skipped: bash is not installed");
        return;
    };
    assert!(status.success());
    for script in [
        "examples/pilot/setup.sh",
        "examples/pilot/run-flagship.sh",
        "examples/pilot/seed-first-token.sh",
        "scripts/check.sh",
    ] {
        let path = repo(script);
        assert!(path.exists(), "{script} exists");
        let out = Command::new("bash").arg("-n").arg(&path).output().unwrap();
        assert!(
            out.status.success(),
            "bash -n {script}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("set -euo pipefail"), "{script} is strict");
    }
}

/// Shell words of a command line (quotes removed), up to a pipe,
/// redirection or end of line. Variable references are kept as text.
fn words(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => quote = Some(c),
            (None, '|' | '>' | '<' | ';' | '&') => break,
            (None, c) if c.is_whitespace() => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            (None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Lines of ```sh blocks with backslash continuations joined.
pub fn shell_lines(markdown: &str) -> Vec<String> {
    let mut lines = vec![];
    let mut in_block = false;
    let mut pending = String::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_block = !in_block && matches!(trimmed, "```sh" | "```bash" | "```shell");
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(head) = line.strip_suffix('\\') {
            pending.push_str(head);
            pending.push(' ');
            continue;
        }
        pending.push_str(line);
        lines.push(std::mem::take(&mut pending));
    }
    lines
}

#[test]
fn install_guide_server_commands_parse() {
    let guide = std::fs::read_to_string(repo("docs/install.md")).expect("docs/install.md");
    let mut seen = 0;
    for line in shell_lines(&guide) {
        let all = words(&line);
        // At command position: first word, or the command after
        // `docker compose … exec [-T] server`.
        let Some(start) = all.iter().enumerate().position(|(i, w)| {
            (w == "runbook-server" || w.ends_with("/runbook-server"))
                && (i == 0 || all[i - 1] == "server")
        }) else {
            continue;
        };
        let args: Vec<String> = all[start + 1..]
            .iter()
            .map(|w| {
                if w.starts_with('$') {
                    // A placeholder for an ID the guide captured earlier.
                    if w.contains("ORG") {
                        "org_01JABCDEFGHJKMNPQRSTVWXYZ0".into()
                    } else {
                        "usr_01JABCDEFGHJKMNPQRSTVWXYZ0".into()
                    }
                } else {
                    w.clone()
                }
            })
            .collect();
        runbook_server::parse_command(args.clone())
            .unwrap_or_else(|e| panic!("install.md: `{line}` does not parse ({e}): {args:?}"));
        seen += 1;
    }
    assert!(seen >= 3, "the guide runs bootstrap, serve and doctor");
}

/// psql's `:'name'` interpolation, for running the seed file without psql.
fn interpolate(sql: &str, vars: &[(&str, &str)]) -> String {
    let mut out = sql.to_owned();
    for (name, value) in vars {
        out = out.replace(
            &format!(":'{name}'"),
            &format!("'{}'", value.replace('\'', "''")),
        );
    }
    assert!(!out.contains(":'"), "unbound psql variable in {out}");
    out
}

#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn seed_first_token_sql_issues_a_credential_that_mints_a_working_pat() {
    use axum::{body::Body, http::Request};
    use futures_util::FutureExt;
    use runbook_core::credentials::{TokenKind, encode_secret, secret_hash};
    use runbook_server::{
        config::ServerConfig,
        http::{Worker, router},
    };
    use tower::ServiceExt;
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        let config =
            ServerConfig::new(db.config().clone(), "https://runbooks.example", "127.0.0.1:0").unwrap();
        let issued = runbook_server::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = vec![];
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let owner = issued["pat"].as_str().unwrap().to_owned();
        let org = issued["org_id"].as_str().unwrap().to_owned();
        let project = issued["project_id"].as_str().unwrap().to_owned();
        let worker = Worker::start(config.clone()).unwrap();
        let app = router(worker.client());
        let call = |token: String, method: &'static str, path: String, body: Value, key: &'static str| {
            let app = app.clone();
            async move {
                let request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"))
                    .header("idempotency-key", key);
                let request = if body.is_null() {
                    request.body(Body::empty())
                } else {
                    request
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                }
                .unwrap();
                let response = app.oneshot(request).await.unwrap();
                let status = response.status().as_u16();
                let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
                (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
            }
        };
        let (status, alice) = call(owner.clone(), "POST", format!("/v1/orgs/{org}/users"), json!({"name": "Alice"}), "alice").await;
        assert_eq!(status, 201, "{alice}");
        let alice = alice["id"].as_str().unwrap().to_owned();
        let (status, member) = call(owner.clone(), "POST", format!("/v1/projects/{project}/members"), json!({"user_id": alice, "role": "operator"}), "member").await;
        assert!(status < 300, "{status} {member}");
        let seed = encode_secret(TokenKind::Personal, [0x5a; 32]);
        let hash: String = secret_hash(&seed).as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let token_id = "tok_01JABCDEFGHJKMNPQRSTVWXYZ9";
        let sql = std::fs::read_to_string(repo("examples/pilot/seed-first-token.sql")).unwrap();
        let store = runbook_store_postgres::PostgresStore::connect(db.config()).await.unwrap();
        let run = |user: &str, token: &str| {
            interpolate(&sql, &[("token_id", token), ("org_id", &org), ("user_id", user), ("project_id", &project), ("hash_hex", &hash)])
        };
        // An unknown user seeds nothing.
        let rows = store.client().query(&run("usr_01JABCDEFGHJKMNPQRSTVWXYZ8", "tok_01JABCDEFGHJKMNPQRSTVWXYZ8"), &[]).await.unwrap();
        assert!(rows.is_empty());
        let rows = store.client().query(&run(&alice, token_id), &[]).await.unwrap();
        let ids: Vec<String> = rows.iter().map(|r| r.get(0)).collect();
        assert_eq!(ids, vec![token_id.to_owned()]);
        // The seed authenticates as Alice and mints her working PAT.
        let (status, minted) = call(
            seed.clone(),
            "POST",
            "/v1/tokens".into(),
            json!({"name": "alice-cli", "scopes": ["executions:read", "executions:start", "executions:act"], "projects": ["default"]}),
            "mint",
        )
        .await;
        assert_eq!(status, 201, "{minted}");
        assert_eq!(minted["token"]["user_id"], alice.as_str());
        assert_eq!(minted["token"]["projects"], json!([project]));
        // It cannot exceed its own scopes.
        let (status, _) = call(seed.clone(), "POST", "/v1/tokens".into(), json!({"scopes": ["runbooks:publish"]}), "wider").await;
        assert_eq!(status, 403);
        let (status, listed) = call(seed, "GET", "/v1/tokens".into(), Value::Null, "list").await;
        assert_eq!(status, 200, "{listed}");
        let names: Vec<&str> = listed["items"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap_or("")).collect();
        assert!(names.contains(&"first token (operator seed)") && names.contains(&"alice-cli"), "{names:?}");
        drop(worker);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

/// Stub `runbook` and `curl` for driving setup.sh without a server. The
/// users list has two pages; "Alice" (`usr_…Z8`) is only on the second.
const STUB_RUNBOOK: &str = r#"st="$STUB_STATE"
echo "runbook $*" >> "$st/calls.log"
case "$1 $2" in
  "context add"|"context use") echo ok;;
  "login --token-stdin") cat > /dev/null; echo ok;;
  "whoami --json") echo '{"organizations":[{"id":"org_01JABCDEFGHJKMNPQRSTVWXYZ0"}]}';;
  "project create") echo '{"id":"prj_01JABCDEFGHJKMNPQRSTVWXYZ1","slug":"pilot"}';;
  "user create") case $4 in Alice) i=1;; Bob) i=2;; Carol) i=3;; esac; echo "{\"id\":\"usr_01JABCDEFGHJKMNPQRSTVWXYZ$i\"}";;
  "project member"|"team member"|"push "*) echo ok;;
  "team create") echo "{\"id\":\"team_01JABCDEFGHJKMNPQRSTVWXYZ$((RANDOM % 10))\"}";;
  "get post-deploy") exit 1;;
  "publish post-deploy") echo '{"runbook_version":1}';;
  "webhook list") if [ -s "$st/hooks-$PILOT_DIR_NAME" ]; then echo "{\"items\":[$(paste -sd, "$st/hooks-$PILOT_DIR_NAME")]}"; else echo '{"items":[]}'; fi;;
  "webhook create") name=""; while [ $# -gt 0 ]; do [ "$1" = --name ] && name=$2; shift; done
    id="whk_01JABCDEFGHJKMNPQRSTVWX$RANDOM"
    echo "{\"id\":\"$id\",\"name\":\"$name\"}" >> "$st/hooks-$PILOT_DIR_NAME"
    echo "{\"webhook\":{\"id\":\"$id\"},\"secret\":\"whsec_x\"}";;
  *) echo "unhandled: $*" >&2; exit 9;;
esac
"#;
const STUB_CURL: &str = r#"st="$STUB_STATE"
method=GET; url=""; prev=""
for a in "$@"; do case $a in http*) url=$a;; esac; [ "$prev" = -X ] && method=$a; prev=$a; done
echo "curl $method $url" >> "$st/calls.log"
case "$method $url" in
  "GET "*/users) echo '{"items":[{"id":"usr_01JABCDEFGHJKMNPQRSTVWXYZ9","name":"Zed"}],"next_cursor":"cDI"}';;
  "GET "*/users?cursor=cDI) echo '{"items":[{"id":"usr_01JABCDEFGHJKMNPQRSTVWXYZ8","name":"Alice"}],"next_cursor":null}';;
  "POST "*/v1/tokens) echo '{"token":{"id":"tok_01JABCDEFGHJKMNPQRSTVWXYZ6"},"secret":"trb_svc_x"}';;
  "POST "*/skills) echo '{"hash":"abc"}';;
  *) echo '{"items":[]}';;
esac
"#;

#[cfg(unix)]
#[test]
fn setup_never_adopts_an_existing_user_it_did_not_create() {
    if !python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let dir = scratch("setup");
    let path = stubs(&dir, &[("runbook", STUB_RUNBOOK), ("curl", STUB_CURL)]);
    let state = dir.join("stub");
    std::fs::create_dir_all(&state).unwrap();
    let run = |pilot: &Path, extra: &[(&str, &str)]| {
        let mut command = Command::new("bash");
        command
            .arg(repo("examples/pilot/setup.sh"))
            .current_dir(repo(""))
            .env_clear()
            .env("PATH", &path)
            .env("HOME", &dir)
            .env("STUB_STATE", &state)
            .env("RUNBOOK_HOST", "http://127.0.0.1:8080")
            .env("PILOT_DIR", pilot)
            .env("PILOT_DIR_NAME", pilot.file_name().unwrap())
            .env("RUNBOOK_BOOTSTRAP_TOKEN", "trb_pat_OWNERSECRET")
            .stdin(Stdio::null());
        for (k, v) in extra {
            command.env(k, v);
        }
        command.output().unwrap()
    };
    // A real "Alice" on the second page of the directory is not reused.
    let pilot = dir.join("refuse");
    let out = run(&pilot, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("Alice") && stderr.contains("PILOT_REUSE_USERS"),
        "{stderr}"
    );
    let calls = std::fs::read_to_string(state.join("calls.log")).unwrap();
    assert!(
        calls.contains("/users?cursor=cDI"),
        "the second page was read: {calls}"
    );
    assert!(!calls.contains("user create"));
    assert!(
        !std::fs::read_to_string(pilot.join("state.env"))
            .unwrap()
            .contains("ALICE_ID")
    );
    // With the explicit opt-in it is reused (and Bob, Carol are created).
    let pilot = dir.join("reuse");
    let out = run(&pilot, &[("PILOT_REUSE_USERS", "1")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recorded = std::fs::read_to_string(pilot.join("state.env")).unwrap();
    assert!(
        recorded.contains("ALICE_ID=usr_01JABCDEFGHJKMNPQRSTVWXYZ8"),
        "{recorded}"
    );
    assert!(
        recorded.contains("BOB_ID=usr_01JABCDEFGHJKMNPQRSTVWXYZ2"),
        "{recorded}"
    );
    // A user recorded by an earlier run is reused without asking.
    let again = run(&pilot, &[]);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let stdout = String::from_utf8_lossy(&again.stdout);
    assert!(
        stdout.contains("ALICE_ID=usr_01JABCDEFGHJKMNPQRSTVWXYZ8 (existing)"),
        "{stdout}"
    );
    assert!(!stdout.contains("OWNERSECRET") && !stdout.contains("trb_svc_x"));
    std::fs::remove_dir_all(dir).unwrap();
}
