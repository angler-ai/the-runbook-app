//! AUTH4 (D47): tests for `runbook login` via RFC 8628 OAuth device grant.
mod fake_api;
use fake_api::*;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const PAT_SECRET: &str = "trb_pat_device-secret-123456789";

fn logins_file(home: &Path) -> PathBuf {
    home.join("runbook").join("credentials").join("logins.json")
}

fn device_code_reply(host: &str, interval: u32) -> Reply {
    Reply::json(
        200,
        json!({
            "device_code": "dev_code_xyz123",
            "user_code": "WDJB-MJHT",
            "verification_uri": format!("{host}/device"),
            "verification_uri_complete": format!("{host}/device?user_code=WDJB-MJHT"),
            "expires_in": 300,
            "interval": interval,
        }),
    )
}

fn pending_reply() -> Reply {
    Reply::json(
        400,
        json!({
            "code": "device.authorization_pending",
            "error": "authorization_pending",
            "message": "authorization pending",
        }),
    )
}

fn slow_down_reply() -> Reply {
    Reply::json(
        400,
        json!({
            "code": "device.slow_down",
            "error": "slow_down",
            "interval": 6,
            "message": "slow down",
        }),
    )
}

fn expired_reply() -> Reply {
    Reply::json(
        400,
        json!({
            "code": "device.expired_token",
            "error": "expired_token",
            "message": "expired token",
        }),
    )
}

fn denied_reply() -> Reply {
    Reply::json(
        400,
        json!({
            "code": "device.access_denied",
            "error": "access_denied",
            "message": "access denied",
        }),
    )
}

fn token_reply() -> Reply {
    Reply::json(
        200,
        json!({
            "access_token": PAT_SECRET,
            "token_type": "bearer",
            "expires_in": 7776000,
            "scope": "runbooks:read",
            "token_id": TOK,
            "user_id": USR,
            "expires_at": 1700000000,
        }),
    )
}

fn cli_no_host(home: &Path, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .args(args)
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
    let mut input = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

#[test]
fn device_login_success_after_two_pending_polls_and_json_output() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        device_code_reply("http://fake", 1),
        pending_reply(),
        pending_reply(),
        token_reply(),
    ]);

    let o = cli(
        &host,
        home.path(),
        &["login", "--json", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "10")],
        None,
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    // Stderr prints verification URI and user code
    let stderr = err(&o);
    assert!(stderr.contains("http://fake/device"), "{stderr}");
    assert!(stderr.contains("WDJB-MJHT"), "{stderr}");

    // The PAT is never printed to stdout or stderr
    assert!(!stderr.contains(PAT_SECRET), "stderr contained PAT");
    let stdout = out(&o);
    assert!(!stdout.contains(PAT_SECRET), "stdout contained PAT");

    // --json prints {host, user_id, expires_at} to stdout
    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(body["host"], host);
    assert_eq!(body["user_id"], USR);
    assert_eq!(body["expires_at"], 1700000000);

    // PAT is saved in logins.json
    let file = logins_file(home.path());
    assert!(file.exists());
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains(PAT_SECRET));

    // Server saw 4 calls: code + 3 token polls
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 4);
    assert_eq!(line(&seen[0]), "POST /v1/device/code");
    assert_eq!(line(&seen[1]), "POST /v1/device/token");
    assert_eq!(line(&seen[2]), "POST /v1/device/token");
    assert_eq!(line(&seen[3]), "POST /v1/device/token");
}

#[test]
fn device_login_slow_down_raises_the_interval() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve_timed(vec![
        device_code_reply("http://fake", 1),
        slow_down_reply(),
        token_reply(),
    ]);

    // Scale 1 second to 20 ms. Initial interval is 1s (20 ms).
    // On slow_down, interval is raised by 5s to 6s (6 * 20 = 120 ms).
    let o = cli(
        &host,
        home.path(),
        &["login", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "20")],
        None,
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);

    // Timing between poll 1 (slow_down) and poll 2 (token)
    let t_slow_down = requests[1].0;
    let t_next_poll = requests[2].0;
    let elapsed = t_next_poll.duration_since(t_slow_down);

    // 6s * 20ms = 120ms. Verify it waited at least 100ms and less than 1 second.
    assert!(
        elapsed >= Duration::from_millis(90),
        "expected >= 90ms for raised interval, got {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(1000),
        "no real sleeps over 1s, got {elapsed:?}"
    );
}

#[test]
fn device_login_expired_token_exits_1_with_clear_message() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![device_code_reply("http://fake", 1), expired_reply()]);

    let o = cli(
        &host,
        home.path(),
        &["login", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "10")],
        None,
    );

    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.to_lowercase().contains("expired"),
        "expected message mentioning expired, got: {stderr}"
    );
    assert!(!logins_file(home.path()).exists());
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn device_login_access_denied_exits_1_with_clear_message() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![device_code_reply("http://fake", 1), denied_reply()]);

    let o = cli(
        &host,
        home.path(),
        &["login", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "10")],
        None,
    );

    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.to_lowercase().contains("denied"),
        "expected message mentioning denied, got: {stderr}"
    );
    assert!(!logins_file(home.path()).exists());
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn device_login_pat_stored_and_never_printed_non_json() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![device_code_reply("http://fake", 1), token_reply()]);

    let o = cli(
        &host,
        home.path(),
        &["login", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "10")],
        None,
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stdout = out(&o);
    let stderr = err(&o);

    assert!(stdout.contains("logged in"), "{stdout}");
    assert!(!stdout.contains(PAT_SECRET));
    assert!(!stderr.contains(PAT_SECRET));

    let file = logins_file(home.path());
    assert!(file.exists());
    assert!(std::fs::read_to_string(&file).unwrap().contains(PAT_SECRET));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn device_login_positional_host_and_me_resolution() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        device_code_reply("http://fake", 1),
        // DeviceTokenView without user_id or expires_at
        Reply::json(
            200,
            json!({
                "access_token": PAT_SECRET,
                "token_type": "bearer",
                "expires_in": 7776000,
                "scope": "runbooks:read",
                "token_id": TOK,
            }),
        ),
        // Resolves user_id and expires_at_ms via GET /v1/me
        Reply::json(
            200,
            json!({
                "user": {"id": USR, "name": "Alice"},
                "org": {"id": ORG, "title": "Runbook"},
                "teams": [],
                "credential": {
                    "kind": "personal",
                    "actor_kind": "human",
                    "scopes": ["runbooks:read"],
                    "projects": null,
                    "expires_at_ms": 1700000000,
                }
            }),
        ),
    ]);

    // Pass host as positional argument: runbook login <host> --json --no-browser
    let o = cli_no_host(
        home.path(),
        &["login", &host, "--json", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "10")],
        None,
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stdout = out(&o);
    assert!(!stdout.contains(PAT_SECRET));

    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(body["host"], host);
    assert_eq!(body["user_id"], USR);
    assert_eq!(body["expires_at"], 1700000000);

    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(line(&seen[0]), "POST /v1/device/code");
    assert_eq!(line(&seen[1]), "POST /v1/device/token");
    assert_eq!(line(&seen[2]), "GET /v1/me");
    assert_eq!(
        header(&seen[2], "authorization"),
        Some(format!("Bearer {PAT_SECRET}").as_str())
    );
    // A first login saves the server as the current context, so later
    // commands such as `runbook whoami` need no --host.
    assert!(
        err(&o).contains("as context 'default' (now current)"),
        "{}",
        err(&o)
    );
    let config = std::fs::read_to_string(home.path().join("runbook").join("config.toml")).unwrap();
    assert!(config.contains("current_context = \"default\""), "{config}");
    assert!(config.contains(&host), "{config}");
}

#[test]
fn token_stdin_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs()]);

    let o = cli(
        &host,
        home.path(),
        &["login", "--token-stdin", "--json"],
        &[],
        Some(&format!("{PAT_SECRET}\n")),
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stdout = out(&o);
    assert!(!stdout.contains(PAT_SECRET));

    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(body["host"], host);
    assert_eq!(body["logged_in"], true);
    assert_eq!(body["credential"], "personal");

    let file = logins_file(home.path());
    assert!(std::fs::read_to_string(&file).unwrap().contains(PAT_SECRET));

    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(line(&seen[0]), "GET /v1/orgs");
}

#[test]
fn device_login_local_deadline_stops_polling_even_if_server_returns_pending_forever() {
    let home = tempfile::tempdir().unwrap();
    let code_reply = Reply::json(
        200,
        json!({
            "device_code": "dev_code_xyz123",
            "user_code": "WDJB-MJHT",
            "verification_uri": "http://fake/device",
            "verification_uri_complete": "http://fake/device?user_code=WDJB-MJHT",
            "expires_in": 1,
            "interval": 1,
        }),
    );
    let (host, server) = serve_repeating(code_reply, pending_reply());

    let o = cli(
        &host,
        home.path(),
        &["login", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "20")],
        None,
    );

    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let stderr = err(&o);
    assert!(
        stderr.to_lowercase().contains("expired"),
        "expected expired message on local deadline, got: {stderr}"
    );
    assert!(!logins_file(home.path()).exists());

    let seen = server.join().unwrap();
    assert!(
        seen.len() >= 2,
        "expected code and at least 1 poll, saw {}",
        seen.len()
    );
    assert_eq!(line(&seen[0]), "POST /v1/device/code");
    assert_eq!(line(&seen[1]), "POST /v1/device/token");
}

/// Sign-in gate fix 3: `runbook login` asks for what an operator or agent
/// CLI needs, not every scope; `--admin` adds the admin scopes and
/// `--scope` (repeatable) asks for exactly the named ones.
#[test]
fn device_login_asks_for_narrow_scopes_unless_told_otherwise() {
    let ask = |extra: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let (host, server) = serve(vec![device_code_reply("http://fake", 1), token_reply()]);
        let mut args = vec!["login", "--no-browser"];
        args.extend_from_slice(extra);
        let o = cli(
            &host,
            home.path(),
            &args,
            &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "10")],
            None,
        );
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        let seen = server.join().unwrap();
        let body = body_of(&seen[0]);
        assert!(
            body["client_name"]
                .as_str()
                .unwrap()
                .starts_with("runbook CLI"),
            "{body}"
        );
        let mut scopes: Vec<String> = body["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect();
        scopes.sort();
        scopes
    };
    let default = ask(&[]);
    assert_eq!(
        default,
        [
            "executions:act",
            "executions:read",
            "executions:start",
            "runbooks:publish",
            "runbooks:read",
            "runbooks:write",
            "skills:read",
            "skills:write"
        ]
    );
    let admin = ask(&["--admin"]);
    for scope in [
        "tokens:manage",
        "project:admin",
        "webhooks:manage",
        "executions:admin",
    ] {
        assert!(!default.iter().any(|s| s == scope), "{scope} by default");
        assert!(admin.iter().any(|s| s == scope), "{scope} with --admin");
    }
    assert_eq!(admin.len(), 12);
    assert_eq!(
        ask(&["--scope", "runbooks:read", "--scope", "executions:read"]),
        ["executions:read", "runbooks:read"]
    );
    // The runner scopes are asked for by name; they are not in --admin.
    assert_eq!(
        ask(&["--scope", "runner:read", "--scope", "runner:admin"]),
        ["runner:admin", "runner:read"]
    );
    // An unknown scope name is refused before any request.
    let home = tempfile::tempdir().unwrap();
    let o = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["login", "--no-browser", "--scope", "everything"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    // The list of valid names includes the runner scopes.
    assert!(err(&o).contains("runner:read, runner:admin"), "{}", err(&o));
}

#[test]
fn device_login_says_the_browser_may_ask_for_a_second_factor_and_waits() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        device_code_reply("http://fake", 1),
        pending_reply(),
        token_reply(),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["login", "--no-browser"],
        &[("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS", "10")],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stderr = err(&o);
    let note = stderr.find("second factor").expect(&stderr);
    // Before the wait, after the code to type.
    assert!(stderr.find("WDJB-MJHT").unwrap() < note, "{stderr}");
    assert!(stderr.contains("passkey or authenticator code"), "{stderr}");
    assert!(stderr.contains("this command waits"), "{stderr}");
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn a_command_without_a_server_says_so_once() {
    let home = tempfile::tempdir().unwrap();
    let o = cli_no_host(home.path(), &["whoami"], &[], None);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_eq!(err(&o).matches("no server set").count(), 1, "{}", err(&o));
}
