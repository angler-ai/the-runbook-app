//! CLI3 `runbook step exec <sr> -- <cmd>`: a local command does the work of
//! a claimed step. Whole output lines become step logs (D38), the claim is
//! heartbeated while it runs, and the step is reported by exit code.
//! These run a real child process against a scripted fake API.
#![cfg(unix)]
mod fake_api;
mod support;
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use support::*;

const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const CLAIM: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3";
const TOKEN: &str = "trb_pat_exec-test";

fn claim_ref() -> String {
    format!("{CLAIM}:2")
}

fn args<'a>(claim: &'a str, extra: &[&'a str], cmd: &[&'a str]) -> Vec<&'a str> {
    let mut a = vec![
        "step",
        "exec",
        SR,
        "--claim",
        claim,
        "--flush-interval-ms",
        "60000",
        "--heartbeat-interval-ms",
        "60000",
        "--json",
    ];
    a.extend_from_slice(extra);
    a.push("--");
    a.extend_from_slice(cmd);
    a
}

fn exec(host: &str, home: &Path, extra: &[&str], cmd: &[&str]) -> Output {
    let claim = claim_ref();
    cli(
        host,
        home,
        &args(&claim, extra, cmd),
        &[("RUNBOOK_TOKEN", TOKEN)],
    )
}

/// The step read `step exec` makes before it starts anything (review M5).
fn step_read() -> Reply {
    reply(200, step_read_body(json!({})))
}

fn step_read_body(outputs: Value) -> Value {
    let mut schema =
        json!({"type": "object", "properties": outputs, "additionalProperties": false});
    let required: Vec<&String> = outputs.as_object().unwrap().keys().collect();
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    json!({"step_run_id": SR, "instructions": "", "checklist": [], "parameters": {},
           "output_schema": schema, "template": {}})
}

fn accepted() -> Reply {
    reply(200, json!({"step_run_id": SR, "accepted": true}))
}

fn path_of(request: &str) -> String {
    request_line(request)
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string()
}

/// Every log line sent, across all `/logs` requests, in order.
fn logged(seen: &[String]) -> Vec<String> {
    seen.iter()
        .filter(|r| path_of(r).ends_with("/logs"))
        .flat_map(|r| {
            body_of(r)["logs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn report(seen: &[String]) -> Value {
    let reports: Vec<&String> = seen
        .iter()
        .filter(|r| path_of(r).ends_with("/report"))
        .collect();
    assert_eq!(reports.len(), 1, "{seen:#?}");
    body_of(reports[0])
}

#[test]
fn a_zero_exit_reports_success_with_the_output_as_logs() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    let o = exec(
        &host,
        home.path(),
        &[],
        &["sh", "-c", "echo one; echo two >&2; echo three"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["status"], "succeeded", "{v}");
    assert_eq!(v["exit_code"], 0, "{v}");
    let seen = server.join().unwrap();
    assert_eq!(
        request_line(&seen[0]),
        format!("GET /v1/step-runs/{SR} HTTP/1.1")
    );
    assert_eq!(path_of(&seen[1]), format!("/v1/step-runs/{SR}/logs"));
    let mut lines = logged(&seen);
    lines.sort();
    assert_eq!(lines, vec!["one", "three", "two"]);
    let body = report(&seen);
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["claim"], json!({"id": CLAIM, "generation": 2}));
    assert_eq!(body["logs"], json!([]));
    assert!(
        body["summary"].as_str().unwrap().contains("code 0"),
        "{body}"
    );
    // Every POST carries its own Idempotency-Key.
    let keys: Vec<&str> = seen[1..]
        .iter()
        .map(|r| header(r, "idempotency-key").unwrap())
        .collect();
    assert_ne!(keys[0], keys[1]);
}

#[test]
fn a_nonzero_exit_reports_failure_with_the_code_and_exits_with_it() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted()]);
    let o = exec(&host, home.path(), &[], &["sh", "-c", "exit 3"]);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    let seen = server.join().unwrap();
    // No output: no log request, just the step read and the report.
    assert_eq!(seen.len(), 2, "{seen:#?}");
    let body = report(&seen);
    assert_eq!(body["status"], "failed");
    let summary = body["summary"].as_str().unwrap();
    assert!(summary.contains("code 3"), "{summary}");
    // The summary names the program, never its arguments.
    assert!(!summary.contains("exit 3"), "{summary}");
}

#[test]
fn long_partial_and_binary_lines_are_whole_sanitized_entries() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    // A 40,000-byte line, binary bytes with an ANSI colour, a CRLF line, a
    // progress line redrawn with CR, and a final line with no newline.
    let script = "head -c 40000 /dev/zero | tr '\\000' x; echo; \
                  printf '\\000\\377\\033[31mred\\033[0m\\n'; \
                  printf 'crlf\\r\\n'; printf '10%%\\r50%%\\r100%%\\n'; printf 'partial'";
    let o = exec(&host, home.path(), &[], &["sh", "-c", script]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let lines = logged(&server.join().unwrap());
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(
        lines[0].starts_with(&"x".repeat(16 * 1024)),
        "{}",
        &lines[0][..40]
    );
    assert!(lines[0].len() < 17 * 1024, "{}", lines[0].len());
    assert!(
        lines[0].ends_with("bytes truncated]"),
        "{}",
        &lines[0][16 * 1024..]
    );
    assert_eq!(lines[1], "\u{fffd}\u{fffd}red");
    assert_eq!(lines[2], "crlf");
    assert_eq!(lines[3], "100%");
    assert_eq!(lines[4], "partial");
}

#[test]
fn output_is_sent_in_whole_line_batches_while_the_command_runs() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted(), accepted()]);
    let claim = claim_ref();
    let mut a = args(&claim, &[], &["sh", "-c", "echo early; sleep 1; echo late"]);
    // Flush every 200 ms instead of at the end.
    let at = a.iter().position(|x| *x == "--flush-interval-ms").unwrap();
    a[at + 1] = "200";
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    let batches: Vec<Value> = seen
        .iter()
        .filter(|r| path_of(r).ends_with("/logs"))
        .map(|r| body_of(r)["logs"].clone())
        .collect();
    assert_eq!(batches, vec![json!(["early"]), json!(["late"])]);
}

#[test]
fn it_heartbeats_while_the_command_runs() {
    let home = tempfile::tempdir().unwrap();
    // Every request is accepted, however many heartbeats fit.
    let (host, server) = fake_api::serve_repeating(
        fake_api::Reply::json(200, json!({"accepted": true})),
        fake_api::Reply::json(200, json!({"accepted": true})),
    );
    let claim = claim_ref();
    let mut a = args(&claim, &[], &["sleep", "1"]);
    let at = a
        .iter()
        .position(|x| *x == "--heartbeat-interval-ms")
        .unwrap();
    a[at + 1] = "300";
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    let beats: Vec<&String> = seen
        .iter()
        .filter(|r| path_of(r).ends_with("/heartbeat"))
        .collect();
    assert!(beats.len() >= 2, "{seen:#?}");
    assert_eq!(
        body_of(beats[0])["claim"],
        json!({"id": CLAIM, "generation": 2})
    );
    assert_eq!(report(&seen)["status"], "succeeded");
}

#[test]
fn a_lost_claim_stops_the_command_and_reports_nothing() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("command.pid");
    let (host, server) = serve(vec![step_read(), problem(409, "claim.stale_generation")]);
    let claim = claim_ref();
    // The command would run a minute; the bounds are loose so a loaded
    // machine does not flake it.
    let script = format!("echo $$ > {}; exec sleep 60", pid_file.display());
    let mut a = args(&claim, &[], &["sh", "-c", &script]);
    let at = a
        .iter()
        .position(|x| *x == "--heartbeat-interval-ms")
        .unwrap();
    a[at + 1] = "200";
    let started = Instant::now();
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["next_action"], "reconcile", "{v}");
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2, "{seen:#?}");
    assert!(path_of(&seen[1]).ends_with("/heartbeat"));
    // The whole command (the shell and its sleep) was stopped; the
    // orphaned process is reaped by init once the CLI has exited.
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "the command kept running after the claim was lost"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn sigint_is_forwarded_to_the_command_then_the_step_is_reported() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    let claim = claim_ref();
    let script = "trap 'echo got-int; exit 7' INT; echo ready; while :; do sleep 0.05; done";
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .arg("--host")
        .arg(&host)
        .args(args(&claim, &[], &["sh", "-c", script]))
        .env_remove("RUNBOOK_HOST")
        .env("RUNBOOK_TOKEN", TOKEN)
        .env("XDG_CONFIG_HOME", home.path())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let pid = child.id() as i32;
    // The command's output is echoed on stderr under --json; wait for it.
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if line == "ready" {
                let _ = tx.send(());
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    unsafe { libc::kill(pid, libc::SIGINT) };
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(130), "{}", out(&o));
    let seen = server.join().unwrap();
    assert_eq!(logged(&seen), vec!["ready", "got-int"]);
    let body = report(&seen);
    assert_eq!(body["status"], "failed");
    let summary = body["summary"].as_str().unwrap();
    assert!(
        summary.contains("SIGINT") && summary.contains("code 7"),
        "{summary}"
    );
}

#[test]
fn a_command_that_cannot_start_is_reported_as_failed() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted()]);
    let o = exec(
        &host,
        home.path(),
        &[],
        &["/nonexistent/runbook-test-command"],
    );
    assert_eq!(o.status.code(), Some(127), "{}", err(&o));
    let body = report(&server.join().unwrap());
    assert_eq!(body["status"], "failed");
    assert!(
        body["summary"]
            .as_str()
            .unwrap()
            .contains("could not start"),
        "{body}"
    );
}

#[test]
fn without_a_claim_nothing_runs() {
    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("ran");
    let (host, server) = serve(vec![]);
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "exec",
            SR,
            "--json",
            "--",
            "touch",
            marker.to_str().unwrap(),
        ],
        &[("RUNBOOK_TOKEN", TOKEN)],
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(!marker.exists());
    assert!(server.join().unwrap().is_empty());
}

/// `args` with the hidden interval flags set.
fn with_flag<'a>(mut a: Vec<&'a str>, flag: &'a str, value: &'a str) -> Vec<&'a str> {
    match a.iter().position(|x| *x == flag) {
        Some(at) => a[at + 1] = value,
        None => {
            let at = a.iter().position(|x| *x == "--").unwrap();
            a.insert(at, value);
            a.insert(at, flag);
        }
    }
    a
}

/// Start the CLI; the receiver fires when the command echoes `ready`.
fn spawn_cli(
    host: &str,
    home: &Path,
    args: &[&str],
) -> (std::process::Child, std::sync::mpsc::Receiver<()>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .arg("--host")
        .arg(host)
        .args(args)
        .env_remove("RUNBOOK_HOST")
        .env("RUNBOOK_TOKEN", TOKEN)
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr).lines() {
            if line.unwrap_or_default() == "ready" {
                let _ = tx.send(());
            }
        }
    });
    (child, rx)
}

/// Wait at most `limit` for the CLI; kill it and fail when it overruns.
fn wait_within(child: &mut std::process::Child, limit: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
            panic!("the CLI did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn count(seen: &[String], suffix: &str) -> usize {
    seen.iter().filter(|r| path_of(r).ends_with(suffix)).count()
}

/// Review M1: heartbeats that keep failing (here a 5xx) for longer than
/// the lease stop the command as for a lost claim: the server may already
/// have given the step to someone else.
///
/// The command would run a minute; the CLI ending well before then, and
/// the command's process being gone afterwards, prove the lapsed lease
/// ended it. The bounds are loose so a loaded machine does not flake it.
#[test]
fn a_lease_that_lapses_without_a_heartbeat_stops_the_command() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("command.pid");
    let (host, server) = serve_by(|req| {
        if path_of(req).ends_with("/heartbeat") {
            answer(503, json!({"code": "server.unavailable"}))
        } else {
            answer(200, step_read_body(json!({})))
        }
    });
    let claim = claim_ref();
    let script = format!("echo $$ > {}; exec sleep 60", pid_file.display());
    let a = args(&claim, &[], &["sh", "-c", &script]);
    let a = with_flag(a, "--heartbeat-interval-ms", "100");
    let a = with_flag(a, "--lease-ms", "700");
    let started = Instant::now();
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert!(err(&o).contains("lease"), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["next_action"], "reconcile", "{v}");
    let seen = server.join().unwrap();
    assert_eq!(count(&seen, "/report"), 0, "{seen:#?}");
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    // The orphaned command is reaped by init once the CLI has exited.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "the command kept running");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Review M1: a heartbeat refused with 401/403 twice in a row is a lost
/// claim, without waiting for the lease.
#[test]
fn a_persistently_refused_heartbeat_is_a_lost_claim() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve_by(|req| {
        if path_of(req).ends_with("/heartbeat") {
            answer(403, json!({"code": "auth.forbidden"}))
        } else {
            answer(200, step_read_body(json!({})))
        }
    });
    let claim = claim_ref();
    // The command would run a minute: stopping well before then proves the
    // refused heartbeats ended it (not the command, nor the 10-minute
    // lease). The bound is loose so a loaded machine does not flake it.
    let a = args(&claim, &[], &["sleep", "60"]);
    let a = with_flag(a, "--heartbeat-interval-ms", "100");
    let started = Instant::now();
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    let seen = server.join().unwrap();
    assert_eq!(count(&seen, "/heartbeat"), 2, "{seen:#?}");
    assert_eq!(count(&seen, "/report"), 0, "{seen:#?}");
}

/// Review M2: SIGHUP (a closed terminal) is forwarded like SIGINT.
#[test]
fn sighup_is_forwarded_to_the_command() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    let claim = claim_ref();
    let script = "trap 'echo got-hup; exit 7' HUP; echo ready; while :; do sleep 0.05; done";
    let (mut child, ready) = spawn_cli(
        &host,
        home.path(),
        &args(&claim, &[], &["sh", "-c", script]),
    );
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    unsafe { libc::kill(child.id() as i32, libc::SIGHUP) };
    let status = wait_within(&mut child, Duration::from_secs(10));
    assert_eq!(status.code(), Some(129));
    let body = report(&server.join().unwrap());
    assert_eq!(body["status"], "failed");
    assert!(
        body["summary"].as_str().unwrap().contains("SIGHUP"),
        "{body}"
    );
}

/// Review M2: a second signal kills the command's whole group, for a
/// command that ignores the first.
#[test]
fn a_second_signal_kills_a_command_that_ignores_the_first() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    let claim = claim_ref();
    let script = "trap '' INT TERM; echo ready; while :; do sleep 0.05; done";
    let (mut child, ready) = spawn_cli(
        &host,
        home.path(),
        &args(&claim, &[], &["sh", "-c", script]),
    );
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    // Re-check N2: only a signal 2 s or more after the forwarded one kills.
    std::thread::sleep(Duration::from_millis(2300));
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let status = wait_within(&mut child, Duration::from_secs(10));
    assert_eq!(status.code(), Some(130));
    let body = report(&server.join().unwrap());
    assert_eq!(body["status"], "failed");
    let summary = body["summary"].as_str().unwrap();
    assert!(
        summary.contains("forwarded SIGINT") && summary.contains("SIGKILL"),
        "{body}"
    );
}

/// Re-check N2: two signals at once (a closed terminal sends SIGHUP
/// twice; `npm run` forwards its own Ctrl-C) forward the first and do not
/// kill the command.
#[test]
fn two_signals_at_once_forward_the_first_and_do_not_kill() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    let claim = claim_ref();
    let script = "trap 'echo got-hup; exit 7' HUP; trap 'echo got-term; exit 7' TERM; \
                  echo ready; while :; do sleep 0.05; done";
    let (mut child, ready) = spawn_cli(
        &host,
        home.path(),
        &args(&claim, &[], &["sh", "-c", script]),
    );
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGHUP);
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = wait_within(&mut child, Duration::from_secs(10));
    let body = report(&server.join().unwrap());
    let summary = body["summary"].as_str().unwrap();
    // The kernel picks which of two pending signals is caught first; the
    // summary and the exit code name the one forwarded.
    let forwarded = match status.code() {
        Some(129) => "SIGHUP",
        Some(143) => "SIGTERM",
        other => panic!("exit {other:?}: {body}"),
    };
    assert!(
        summary.contains(&format!("forwarded {forwarded})")) && summary.contains("code 7"),
        "{body}"
    );
    assert!(!summary.contains("SIGKILL"), "{body}");
}

/// Re-check N2: a second signal within 2 s of the forwarded one is not
/// escalated, so a command still cleaning up is not killed.
#[test]
fn a_second_signal_within_the_grace_period_does_not_kill() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    let claim = claim_ref();
    let script = "trap 'sleep 1; echo cleaned-up; exit 7' INT; echo ready; \
                  while :; do sleep 0.05; done";
    let (mut child, ready) = spawn_cli(
        &host,
        home.path(),
        &args(&claim, &[], &["sh", "-c", script]),
    );
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    std::thread::sleep(Duration::from_millis(300));
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let status = wait_within(&mut child, Duration::from_secs(10));
    assert_eq!(status.code(), Some(130));
    let seen = server.join().unwrap();
    assert!(
        logged(&seen).contains(&"cleaned-up".to_string()),
        "{seen:#?}"
    );
    let summary = report(&seen)["summary"].as_str().unwrap().to_string();
    assert!(
        summary.contains("code 7") && !summary.contains("SIGKILL"),
        "{summary}"
    );
}

/// Re-check 3 (finding 4): a different signal within the grace period is
/// forwarded too, so a command that ignores SIGHUP still gets SIGTERM
/// before any kill.
#[test]
fn a_different_second_signal_within_the_grace_period_is_forwarded() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted(), accepted()]);
    let claim = claim_ref();
    let script = "trap '' HUP; trap 'echo got-term; exit 7' TERM; echo ready; \
                  while :; do sleep 0.05; done";
    let (mut child, ready) = spawn_cli(
        &host,
        home.path(),
        &args(&claim, &[], &["sh", "-c", script]),
    );
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    unsafe { libc::kill(child.id() as i32, libc::SIGHUP) };
    std::thread::sleep(Duration::from_millis(300));
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = wait_within(&mut child, Duration::from_secs(10));
    assert_eq!(status.code(), Some(129));
    let seen = server.join().unwrap();
    assert!(logged(&seen).contains(&"got-term".to_string()), "{seen:#?}");
    let summary = report(&seen)["summary"].as_str().unwrap().to_string();
    assert!(
        summary.contains("code 7") && !summary.contains("SIGKILL"),
        "{summary}"
    );
}

/// Review M3: a full batch that fails transiently is retried after a
/// backoff (and the server's Retry-After), not on every 20 ms tick.
#[test]
fn a_failing_full_batch_is_retried_with_backoff() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve_by(|req| {
        let path = path_of(req);
        if path.ends_with("/logs") {
            Answer {
                status: 503,
                headers: vec![("Retry-After".into(), "1".into())],
                body: json!({"code": "server.unavailable"}),
            }
        } else if path.ends_with("/report") {
            answer(200, json!({"accepted": true}))
        } else {
            answer(200, step_read_body(json!({})))
        }
    });
    let claim = claim_ref();
    let script = "i=0; while [ $i -lt 1200 ]; do echo line$i; i=$((i+1)); done; sleep 2";
    let a = with_flag(
        args(&claim, &[], &["sh", "-c", script]),
        "--flush-interval-ms",
        "50",
    );
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    let logs = count(&seen, "/logs");
    // About one try a second while it runs, then the final attempts.
    assert!((2..=8).contains(&logs), "{logs} log requests");
    assert_eq!(count(&seen, "/report"), 1);
}

/// Review M4: a command that closes its pipes and keeps running does not
/// make the CLI spin.
#[test]
fn a_command_that_closes_its_pipes_does_not_make_the_cli_spin() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![step_read(), accepted()]);
    let claim = claim_ref();
    let (child, _) = spawn_cli(
        &host,
        home.path(),
        &args(&claim, &[], &["sh", "-c", "exec >&- 2>&-; sleep 1.5"]),
    );
    let mut status = 0;
    // SAFETY: a zeroed rusage is valid; wait4 fills it for this child.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let pid = unsafe { libc::wait4(child.id() as i32, &mut status, 0, &mut usage) };
    assert_eq!(pid, child.id() as i32);
    let cpu = Duration::from_secs((usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64)
        + Duration::from_micros((usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64);
    assert!(
        cpu < Duration::from_millis(500),
        "the CLI used {cpu:?} of CPU"
    );
    assert_eq!(report(&server.join().unwrap())["status"], "succeeded");
}

/// Review M5: a step with required outputs cannot be reported by `exec`;
/// it is refused before anything runs, and the claim is kept.
#[test]
fn a_step_with_required_outputs_is_refused_before_the_command_runs() {
    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("ran");
    let (host, server) = serve(vec![reply(
        200,
        step_read_body(json!({"version": {"type": "string"}})),
    )]);
    let o = exec(
        &host,
        home.path(),
        &[],
        &["touch", marker.to_str().unwrap()],
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("version"), "{}", err(&o));
    assert!(!marker.exists());
    assert_eq!(server.join().unwrap().len(), 1);
}

/// Review M5, re-check N7: a step that needs a verdict (a `check` task;
/// the step read does not say so) is rejected at the report. The claim is
/// kept, not released (a release would let someone run the work again):
/// the agent finishes with `runbook step report` and the verdict.
#[test]
fn a_report_that_needs_a_verdict_keeps_the_claim() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        step_read(),
        problem(422, "step.verdict_required"),
        accepted(),
    ]);
    let o = exec(&host, home.path(), &[], &["true"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("claim is kept"), "{}", err(&o));
    assert!(
        err(&o).contains(&format!(
            "runbook step report {SR} --status succeeded --verdict"
        )),
        "{}",
        err(&o)
    );
    let seen = server.join().unwrap();
    assert_eq!(count(&seen, "/release"), 0, "{seen:#?}");
    assert_eq!(seen.len(), 2, "{seen:#?}");
    // The stored claim is kept for that report.
    let state = home
        .path()
        .join("runbook")
        .join("credentials")
        .join("steps")
        .join(format!("{SR}.json"));
    if let Ok(bytes) = std::fs::read(&state) {
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!v["claim"].is_null(), "{v}");
    }
}

/// Review (low): once the command has exited, the final log flush gives
/// up without a last pointless sleep, and Ctrl-C cuts its retries short.
#[test]
fn ctrl_c_after_the_command_exited_ends_the_final_retries() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve_by(|req| {
        let path = path_of(req);
        if path.ends_with("/logs") {
            answer(503, json!({"code": "server.unavailable"}))
        } else if path.ends_with("/report") {
            answer(200, json!({"accepted": true}))
        } else {
            answer(200, step_read_body(json!({})))
        }
    });
    let claim = claim_ref();
    let (mut child, ready) = spawn_cli(
        &host,
        home.path(),
        &args(&claim, &[], &["sh", "-c", "echo ready"]),
    );
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let signalled = Instant::now();
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let status = wait_within(&mut child, Duration::from_secs(10));
    assert!(
        signalled.elapsed() < Duration::from_millis(1500),
        "{:?}",
        signalled.elapsed()
    );
    assert_eq!(status.code(), Some(0));
    let seen = server.join().unwrap();
    assert_eq!(count(&seen, "/report"), 1, "{seen:#?}");
}

#[test]
fn the_final_flush_does_not_sleep_after_its_last_try() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve_by(|req| {
        let path = path_of(req);
        if path.ends_with("/logs") {
            answer(503, json!({"code": "server.unavailable"}))
        } else if path.ends_with("/report") {
            answer(200, json!({"accepted": true}))
        } else {
            answer(200, step_read_body(json!({})))
        }
    });
    let started = Instant::now();
    let o = exec(&host, home.path(), &[], &["echo", "hi"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // Three tries with 1 s and 2 s between them; no 4 s sleep after.
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "{:?}",
        started.elapsed()
    );
    let seen = server.join().unwrap();
    assert_eq!(count(&seen, "/logs"), 3, "{seen:#?}");
}

/// Review (low): `--idempotency-key` names one attempt; reusing it for a
/// new run is refused by the server, and the CLI says why.
#[test]
fn a_reused_idempotency_key_is_explained() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        step_read(),
        problem(409, "request.idempotency_key_reused"),
    ]);
    let o = exec(
        &host,
        home.path(),
        &["--idempotency-key", "attempt-1"],
        &["true"],
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("needs a new key"), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(
        header(&seen[1], "idempotency-key"),
        Some("attempt-1-report")
    );
}

/// A heartbeat reply: the lease deadline (epoch ms), or null for a claim
/// without one (a person's).
fn beat_then_unavailable(first: Value) -> impl Fn(&str) -> Answer + Send + 'static {
    let beats = std::sync::atomic::AtomicUsize::new(0);
    move |req: &str| {
        let path = path_of(req);
        if path.ends_with("/heartbeat") {
            if beats.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                answer(200, first.clone())
            } else {
                answer(503, json!({"code": "server.unavailable"}))
            }
        } else if path.ends_with("/report") {
            answer(200, json!({"accepted": true}))
        } else {
            answer(200, step_read_body(json!({})))
        }
    }
}

/// Re-check N1: a person's claim has no lease (the heartbeat answers
/// null), so an outage longer than the agent lease does not stop the
/// command.
#[test]
fn a_claim_without_a_lease_survives_an_outage() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve_by(beat_then_unavailable(Value::Null));
    let claim = claim_ref();
    let a = args(&claim, &[], &["sleep", "2"]);
    let a = with_flag(a, "--heartbeat-interval-ms", "100");
    let a = with_flag(a, "--lease-ms", "700");
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(count(&seen, "/heartbeat") > 3, "{seen:#?}");
    assert_eq!(report(&seen)["status"], "succeeded");
}

/// Re-check N1: a claim with a lease (the heartbeat answers its deadline)
/// stops the command once no heartbeat succeeded for that lease.
///
/// The command would run a minute; the CLI ending well before then, and
/// the command's process being gone afterwards, prove the lapsed lease
/// ended it. The bounds are loose so a loaded machine does not flake it.
#[test]
fn a_claim_with_a_lease_stops_after_it_lapses() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("command.pid");
    let (host, server) = serve_by(beat_then_unavailable(json!(1_900_000_000_000_i64)));
    let claim = claim_ref();
    let script = format!("echo $$ > {}; exec sleep 60", pid_file.display());
    let a = args(&claim, &[], &["sh", "-c", &script]);
    let a = with_flag(a, "--heartbeat-interval-ms", "100");
    let a = with_flag(a, "--lease-ms", "700");
    let started = Instant::now();
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("lease"), "{}", err(&o));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    let seen = server.join().unwrap();
    assert_eq!(count(&seen, "/report"), 0, "{seen:#?}");
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    // The orphaned command is reaped by init once the CLI has exited.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(Instant::now() < deadline, "the command kept running");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Re-check N3: a 403 that is not a Runbook problem with a known auth
/// code (a proxy or WAF in front of the API) is not a refused heartbeat;
/// it is left to the lease timer.
#[test]
fn a_proxy_403_on_heartbeats_is_not_a_lost_claim() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve_by(|req| {
        let path = path_of(req);
        if path.ends_with("/heartbeat") {
            answer(403, json!({"message": "Request blocked"}))
        } else if path.ends_with("/report") {
            answer(200, json!({"accepted": true}))
        } else {
            answer(200, step_read_body(json!({})))
        }
    });
    let claim = claim_ref();
    let a = args(&claim, &[], &["sleep", "1"]);
    let a = with_flag(a, "--heartbeat-interval-ms", "100");
    let o = cli(&host, home.path(), &a, &[("RUNBOOK_TOKEN", TOKEN)]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(count(&seen, "/heartbeat") > 2, "{seen:#?}");
    assert_eq!(report(&seen)["status"], "succeeded");
}
