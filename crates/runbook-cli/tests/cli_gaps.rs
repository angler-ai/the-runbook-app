//! Tests for CLI2 gaps: new commands (token create service, user list,
//! team list, project member list, me tasks), behavior fixes (spec validate
//! exit 2, --help and --version exit 0 to stdout, --jsonl streaming,
//! run get --wait --fail-on, minimum_cli_version warning).

mod fake_api;
use fake_api::*;
use serde_json::{Value, json};
use std::fs;
use tempfile::tempdir;

#[test]
fn token_create_service_kind() {
    let home = tempdir().unwrap();
    let (host, server) = serve(vec![Reply::json(
        201,
        json!({
            "id": TOK,
            "secret": "trb_svc_test-secret",
            "name": "my-service",
            "actor_kind": "service",
            "projects": [PRJ],
            "scopes": ["runbooks:read"]
        }),
    )]);

    let o = cli(
        &host,
        home.path(),
        &[
            "token",
            "create",
            "--kind",
            "service",
            "--project",
            PRJ,
            "--scope",
            "runbooks:read",
            "--name",
            "my-service",
        ],
        &token(),
        None,
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stdout = out(&o);
    assert!(stdout.contains("trb_svc_test-secret"), "{stdout}");
    assert!(stdout.contains(TOK), "{stdout}");

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "POST /v1/tokens");
    let body = body_of(&seen[0]);
    assert_eq!(body["actor_kind"], "service");
    assert_eq!(body["projects"], json!([PRJ]));
    assert_eq!(body["scopes"], json!(["runbooks:read"]));
    assert_eq!(body["name"], "my-service");
}

#[test]
fn token_create_service_json_and_validation() {
    let home = tempdir().unwrap();
    let (host, server) = serve(vec![Reply::json(
        201,
        json!({
            "id": TOK,
            "secret": "trb_svc_test-secret",
            "name": "my-service",
            "actor_kind": "service",
            "project_ids": [PRJ],
            "scopes": ["runbooks:read"]
        }),
    )]);

    let o = cli(
        &host,
        home.path(),
        &[
            "token",
            "create",
            "--kind",
            "service",
            "--project",
            PRJ,
            "--scope",
            "runbooks:read",
            "--json",
        ],
        &token(),
        None,
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let val: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(val["id"], TOK);
    assert_eq!(val["secret"], "trb_svc_test-secret");
    assert_eq!(val["actor_kind"], "service");
    drop(server);

    // Validation: missing --project
    let o_no_prj = cli(
        &host,
        home.path(),
        &[
            "token",
            "create",
            "--kind",
            "service",
            "--scope",
            "runbooks:read",
        ],
        &token(),
        None,
    );
    assert_eq!(o_no_prj.status.code(), Some(2));
    assert!(err(&o_no_prj).contains("service token requires exactly one --project"));

    // Validation: missing --scope
    let o_no_scope = cli(
        &host,
        home.path(),
        &["token", "create", "--kind", "service", "--project", PRJ],
        &token(),
        None,
    );
    assert_eq!(o_no_scope.status.code(), Some(2));
    assert!(err(&o_no_scope).contains("service token requires at least one --scope"));
}

#[test]
fn user_list_human_and_json() {
    let home = tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": USR, "name": "Alice", "role": "member", "active": true}
                ],
                "next_cursor": null
            }),
        ),
        orgs(),
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": USR, "name": "Alice", "role": "member", "active": true}
                ],
                "next_cursor": null
            }),
        ),
    ]);

    // Human output
    let o_human = cli(&host, home.path(), &["user", "list"], &token(), None);
    assert_eq!(o_human.status.code(), Some(0), "{}", err(&o_human));
    let stdout = out(&o_human);
    assert!(stdout.contains(USR), "{stdout}");
    assert!(stdout.contains("Alice"), "{stdout}");
    assert!(stdout.contains("member"), "{stdout}");
    assert!(stdout.contains("active"), "{stdout}");

    // JSON output
    let o_json = cli(
        &host,
        home.path(),
        &["user", "list", "--json"],
        &token(),
        None,
    );
    assert_eq!(o_json.status.code(), Some(0), "{}", err(&o_json));
    let val: Value = serde_json::from_str(&out(&o_json)).unwrap();
    assert_eq!(val["items"][0]["id"], USR);
    assert_eq!(val["items"][0]["name"], "Alice");

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("GET /v1/orgs/{ORG}/users"));
    assert_eq!(line(&seen[3]), format!("GET /v1/orgs/{ORG}/users"));
}

#[test]
fn team_list_human_and_json() {
    let home = tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": TEAM, "slug": "sre", "title": "Site Reliability"}
                ],
                "next_cursor": null
            }),
        ),
        orgs(),
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": TEAM, "slug": "sre", "title": "Site Reliability"}
                ],
                "next_cursor": null
            }),
        ),
    ]);

    // Human output
    let o_human = cli(&host, home.path(), &["team", "list"], &token(), None);
    assert_eq!(o_human.status.code(), Some(0), "{}", err(&o_human));
    let stdout = out(&o_human);
    assert!(stdout.contains(TEAM), "{stdout}");
    assert!(stdout.contains("sre"), "{stdout}");
    assert!(stdout.contains("Site Reliability"), "{stdout}");

    // JSON output
    let o_json = cli(
        &host,
        home.path(),
        &["team", "list", "--json"],
        &token(),
        None,
    );
    assert_eq!(o_json.status.code(), Some(0), "{}", err(&o_json));
    let val: Value = serde_json::from_str(&out(&o_json)).unwrap();
    assert_eq!(val["items"][0]["slug"], "sre");

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("GET /v1/orgs/{ORG}/teams"));
    assert_eq!(line(&seen[3]), format!("GET /v1/orgs/{ORG}/teams"));
}

#[test]
fn project_list_reads_every_page_human_and_json() {
    let home = tempdir().unwrap();
    let second = "prj_01J00000000000000000000002";
    let pages = || {
        vec![
            orgs(),
            Reply::json(
                200,
                json!({"items": [{"id": PRJ, "slug": "ops", "title": "Operations", "archived": false}], "next_cursor": "c1"}),
            ),
            Reply::json(
                200,
                json!({"items": [{"id": second, "slug": "old", "title": "Old", "archived": true}], "next_cursor": null}),
            ),
        ]
    };
    let (host, server) = serve(pages().into_iter().chain(pages()).collect());

    let o_human = cli(&host, home.path(), &["project", "list"], &token(), None);
    assert_eq!(o_human.status.code(), Some(0), "{}", err(&o_human));
    let stdout = out(&o_human);
    assert!(
        stdout.contains(&format!("{PRJ}  ops  Operations\n")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{second}  old  Old  (archived)")),
        "{stdout}"
    );

    let o_json = cli(
        &host,
        home.path(),
        &["project", "list", "--json"],
        &token(),
        None,
    );
    assert_eq!(o_json.status.code(), Some(0), "{}", err(&o_json));
    let val: Value = serde_json::from_str(&out(&o_json)).unwrap();
    assert_eq!(val["items"].as_array().unwrap().len(), 2);
    assert_eq!(val["items"][1]["slug"], "old");

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("GET /v1/orgs/{ORG}/projects"));
    assert_eq!(
        line(&seen[2]),
        format!("GET /v1/orgs/{ORG}/projects?cursor=c1")
    );
}

#[test]
fn project_member_list_human_and_json() {
    let home = tempdir().unwrap();
    let (host, server) = serve(vec![
        Reply::json(
            200,
            json!({
                "items": [
                    {"user_id": USR, "role": "editor"}
                ],
                "next_cursor": null
            }),
        ),
        Reply::json(
            200,
            json!({
                "items": [
                    {"user_id": USR, "role": "editor"}
                ],
                "next_cursor": null
            }),
        ),
    ]);

    // Human output
    let o_human = cli(
        &host,
        home.path(),
        &["project", "member", "list", PRJ],
        &token(),
        None,
    );
    assert_eq!(o_human.status.code(), Some(0), "{}", err(&o_human));
    let stdout = out(&o_human);
    assert!(stdout.contains(USR), "{stdout}");
    assert!(stdout.contains("editor"), "{stdout}");

    // JSON output
    let o_json = cli(
        &host,
        home.path(),
        &["project", "member", "list", PRJ, "--json"],
        &token(),
        None,
    );
    assert_eq!(o_json.status.code(), Some(0), "{}", err(&o_json));
    let val: Value = serde_json::from_str(&out(&o_json)).unwrap();
    assert_eq!(val["items"][0]["user_id"], USR);
    assert_eq!(val["items"][0]["role"], "editor");

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("GET /v1/projects/{PRJ}/members"));
    assert_eq!(line(&seen[1]), format!("GET /v1/projects/{PRJ}/members"));
}

#[test]
fn me_tasks_human_and_json() {
    let home = tempdir().unwrap();
    let (host, server) = serve(vec![
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": "tsk_01JABCDEFGHJKMNPQRSTVWXYZ9", "kind": "approval", "title": "Deploy Approval", "status": "pending"}
                ],
                "next_cursor": null
            }),
        ),
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": "tsk_01JABCDEFGHJKMNPQRSTVWXYZ9", "kind": "approval", "title": "Deploy Approval", "status": "pending"}
                ],
                "next_cursor": null
            }),
        ),
    ]);

    // Human output
    let o_human = cli(&host, home.path(), &["me", "tasks"], &token(), None);
    assert_eq!(o_human.status.code(), Some(0), "{}", err(&o_human));
    let stdout = out(&o_human);
    assert!(
        stdout.contains("tsk_01JABCDEFGHJKMNPQRSTVWXYZ9"),
        "{stdout}"
    );
    assert!(stdout.contains("approval"), "{stdout}");
    assert!(stdout.contains("Deploy Approval"), "{stdout}");

    // JSON output with query params
    let o_json = cli(
        &host,
        home.path(),
        &[
            "me", "tasks", "--kind", "approval", "--limit", "10", "--json",
        ],
        &token(),
        None,
    );
    assert_eq!(o_json.status.code(), Some(0), "{}", err(&o_json));
    let val: Value = serde_json::from_str(&out(&o_json)).unwrap();
    assert_eq!(val["items"][0]["kind"], "approval");

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "GET /v1/me/tasks");
    assert!(line(&seen[1]).starts_with("GET /v1/me/tasks?"));
    assert!(line(&seen[1]).contains("kind=approval"));
    assert!(line(&seen[1]).contains("limit=10"));
}

#[test]
fn spec_validate_exit_2_on_invalid_spec() {
    let home = tempdir().unwrap();
    let invalid_spec = home.path().join("invalid.yaml");
    fs::write(&invalid_spec, "invalid: [yaml: broken").unwrap();

    let o_inv = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["spec", "validate", invalid_spec.to_str().unwrap()],
        &[],
        None,
    );
    assert_eq!(o_inv.status.code(), Some(2), "{}", err(&o_inv));

    // Valid spec exits 0
    let valid_spec = home.path().join("valid.yaml");
    fs::write(
        &valid_spec,
        r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: valid-rb
  title: Valid
spec:
  steps:
    - id: step1
      title: Step 1
"#,
    )
    .unwrap();

    let o_val = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["spec", "validate", valid_spec.to_str().unwrap()],
        &[],
        None,
    );
    assert_eq!(o_val.status.code(), Some(0), "{}", err(&o_val));
}

#[test]
fn help_and_version_flags_exit_0_to_stdout() {
    let home = tempdir().unwrap();

    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["run", "--help"],
        vec!["run", "--version"],
        vec!["user", "list", "--help"],
        vec!["user", "list", "--version"],
    ] {
        let o = cli("http://127.0.0.1:9", home.path(), &args, &[], None);
        assert_eq!(o.status.code(), Some(0), "args: {args:?}, err: {}", err(&o));
        let stdout = out(&o);
        assert!(!stdout.is_empty(), "stdout was empty for {args:?}");
        assert!(
            err(&o).is_empty(),
            "stderr was not empty for {args:?}: {}",
            err(&o)
        );
    }
}

#[test]
fn jsonl_streaming_run_list_all() {
    let home = tempdir().unwrap();
    let (host, server) = serve(vec![
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": "exe_1", "status": "running", "title": "Run 1"}
                ],
                "next_cursor": "cur_2"
            }),
        ),
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": "exe_2", "status": "completed", "title": "Run 2"}
                ],
                "next_cursor": null
            }),
        ),
    ]);

    let o = cli(
        &host,
        home.path(),
        &["run", "list", "--project", PRJ, "--all", "--jsonl"],
        &token(),
        None,
    );

    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stdout = out(&o);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "stdout: {stdout}");

    let item1: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(item1["id"], "exe_1");
    let item2: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(item2["id"], "exe_2");

    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2);
}

#[test]
fn run_get_wait_fail_on() {
    let home = tempdir().unwrap();

    // Case 1: Run fails and --fail-on failed is set -> exit code 1
    let (host1, server1) = serve(vec![
        Reply::json(
            200,
            json!({
                "id": EXE,
                "status": "running",
                "title": "My Run"
            }),
        ),
        Reply::json(
            200,
            json!({
                "id": EXE,
                "status": "failed",
                "title": "My Run"
            }),
        ),
    ]);

    let o_fail = cli(
        &host1,
        home.path(),
        &[
            "run",
            "get",
            EXE,
            "--wait",
            "--fail-on",
            "failed",
            "--interval-ms",
            "10",
        ],
        &token(),
        None,
    );
    assert_eq!(o_fail.status.code(), Some(1), "{}", err(&o_fail));
    drop(server1);

    // Case 2: Run succeeds and --fail-on failed is set -> exit code 0
    let (host2, server2) = serve(vec![Reply::json(
        200,
        json!({
            "id": EXE,
            "status": "succeeded",
            "title": "My Run"
        }),
    )]);

    let o_ok = cli(
        &host2,
        home.path(),
        &[
            "run",
            "get",
            EXE,
            "--wait",
            "--fail-on",
            "failed",
            "--interval-ms",
            "10",
        ],
        &token(),
        None,
    );
    assert_eq!(o_ok.status.code(), Some(0), "{}", err(&o_ok));
    drop(server2);
}

#[test]
fn minimum_cli_version_warning_shown_and_suppressed() {
    let home = tempdir().unwrap();

    // Server announces newer CLI version: warning on stderr
    let (host1, server1) = serve(vec![
        Reply::json(200, json!({"minimum_cli_version": "99.0.0"})),
        orgs(),
        Reply::json(200, json!({"items": [], "next_cursor": null})),
    ]);

    let o_warn = cli(&host1, home.path(), &["user", "list"], &token(), None);
    assert_eq!(o_warn.status.code(), Some(0), "{}", err(&o_warn));
    let stderr = err(&o_warn);
    assert!(
        stderr.contains("warning: server requires runbook CLI version >= 99.0.0"),
        "{stderr}"
    );
    drop(server1);

    // Under --json, discovery check is skipped entirely: no warning on stderr
    let (host2, server2) = serve(vec![
        orgs(),
        Reply::json(200, json!({"items": [], "next_cursor": null})),
    ]);

    let o_json = cli(
        &host2,
        home.path(),
        &["user", "list", "--json"],
        &token(),
        None,
    );
    assert_eq!(o_json.status.code(), Some(0), "{}", err(&o_json));
    assert!(!err(&o_json).contains("warning: server requires runbook CLI version"));
    drop(server2);
}

fn serve_counting(
    min_ver: &str,
) -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::mpsc::Sender<()>,
) {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = format!("http://127.0.0.1:{port}");
    let well_known_count = std::sync::Arc::new(AtomicUsize::new(0));
    let count_clone = well_known_count.clone();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let min_ver = min_ver.to_string();

    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            let mut stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
            };
            stream.set_nonblocking(false).unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            if req.starts_with("GET /.well-known/runbook") {
                count_clone.fetch_add(1, Ordering::SeqCst);
                let body = format!(r#"{{"minimum_cli_version":"{min_ver}"}}"#);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Both);
            } else if req.starts_with("GET /v1/orgs") {
                let body = r#"{"items":[{"id":"org_01H1234567890ABCDEFGHJKMNP","title":"Test"}],"next_cursor":null}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Both);
            } else {
                let body = r#"{"items":[],"next_cursor":null}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    });

    (host, well_known_count, stop_tx)
}

#[test]
fn version_check_cached_within_ttl() {
    use std::sync::atomic::Ordering;

    let home = tempdir().unwrap();
    let (host, count, stop) = serve_counting("99.0.0");

    let o1 = cli(&host, home.path(), &["user", "list"], &token(), None);
    assert_eq!(o1.status.code(), Some(0), "{}", err(&o1));
    assert!(err(&o1).contains("warning: server requires runbook CLI version >= 99.0.0"));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let o2 = cli(&host, home.path(), &["user", "list"], &token(), None);
    assert_eq!(o2.status.code(), Some(0), "{}", err(&o2));
    assert!(err(&o2).contains("warning: server requires runbook CLI version >= 99.0.0"));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let _ = stop.send(());
}

#[test]
fn version_check_expired_cache_refetches() {
    use std::sync::atomic::Ordering;

    let home = tempdir().unwrap();
    let (host, count, stop) = serve_counting("99.0.0");

    let o1 = cli(&host, home.path(), &["user", "list"], &token(), None);
    assert_eq!(o1.status.code(), Some(0), "{}", err(&o1));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let cache_path = home
        .path()
        .join("runbook")
        .join("credentials")
        .join("version_cache.json");
    assert!(cache_path.exists());
    let mut cache: std::collections::BTreeMap<String, runbook_cli::credstore::CachedVersion> =
        serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    for v in cache.values_mut() {
        v.checked_at_ms = 0;
    }
    fs::write(&cache_path, serde_json::to_string(&cache).unwrap()).unwrap();

    let o2 = cli(&host, home.path(), &["user", "list"], &token(), None);
    assert_eq!(o2.status.code(), Some(0), "{}", err(&o2));
    assert_eq!(count.load(Ordering::SeqCst), 2);

    let _ = stop.send(());
}

#[test]
fn version_check_unreachable_host_cached_without_delay() {
    let home = tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let host = format!("http://127.0.0.1:{port}");

    let o1 = cli(&host, home.path(), &["user", "list"], &token(), None);
    assert_ne!(o1.status.code(), Some(0));

    let cache_path = home
        .path()
        .join("runbook")
        .join("credentials")
        .join("version_cache.json");
    assert!(cache_path.exists());
    let cache: std::collections::BTreeMap<String, runbook_cli::credstore::CachedVersion> =
        serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let entry = cache.get(&host).expect("cache should contain host");
    assert_eq!(entry.minimum_cli_version, None);

    let start = std::time::Instant::now();
    let o2 = cli(&host, home.path(), &["user", "list"], &token(), None);
    let duration = start.elapsed();
    assert_ne!(o2.status.code(), Some(0));
    assert!(
        duration < std::time::Duration::from_millis(400),
        "cached unreachable host call took too long: {:?}",
        duration
    );
}

#[test]
fn version_check_skipped_for_spec_validate() {
    use std::sync::atomic::Ordering;

    let home = tempdir().unwrap();
    let (host, count, stop) = serve_counting("99.0.0");

    let spec_path = home.path().join("spec.yaml");
    fs::write(
        &spec_path,
        r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: test-rb
  title: Test
spec:
  steps:
    - id: step1
      title: Step 1
"#,
    )
    .unwrap();

    let o = cli(
        &host,
        home.path(),
        &["spec", "validate", spec_path.to_str().unwrap()],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(count.load(Ordering::SeqCst), 0);

    let _ = stop.send(());
}

#[test]
fn version_check_skipped_in_ci() {
    use std::sync::atomic::Ordering;

    let home = tempdir().unwrap();
    let (host, count, stop) = serve_counting("99.0.0");

    let o = cli(
        &host,
        home.path(),
        &["user", "list"],
        &[("CI", "true"), ("RUNBOOK_TOKEN", PAT)],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(count.load(Ordering::SeqCst), 0);

    let _ = stop.send(());
}
