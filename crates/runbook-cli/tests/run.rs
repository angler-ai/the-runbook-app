use runbook_cli::api::{ApiClient, ApiError};
use runbook_cli::selector::{parse_runbook_ref, validate_project_ref};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn spawn_fake_server(
    response_status: u16,
    response_body: serde_json::Value,
) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = format!("http://127.0.0.1:{port}");

    let handle = thread::spawn(move || {
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];

            // Read headers first
            let mut content_length = 0;
            let mut header_end = None;
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    let header_str = String::from_utf8_lossy(&buf[..pos]);
                    for line in header_str.lines() {
                        if let Some(val) = line
                            .strip_prefix("Content-Length:")
                            .or_else(|| line.strip_prefix("content-length:"))
                        {
                            content_length = val.trim().parse::<usize>().unwrap_or(0);
                        }
                    }
                    break;
                }
            }

            if let Some(h_end) = header_end {
                while buf.len() < h_end + content_length {
                    let n = stream.read(&mut chunk).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }

            let req_str = String::from_utf8_lossy(&buf).to_string();
            if req_str.starts_with("GET /.well-known/runbook ") {
                let text = r#"{"minimum_cli_version":"0.1.0"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                continue;
            }

            let status_text = match response_status {
                200 => "200 OK",
                201 => "201 Created",
                400 => "400 Bad Request",
                401 => "401 Unauthorized",
                404 => "404 Not Found",
                409 => "409 Conflict",
                422 => "422 Unprocessable Entity",
                _ => "500 Internal Server Error",
            };
            let body_str = serde_json::to_string(&response_body).unwrap();
            let response = format!(
                "HTTP/1.1 {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body_str.len(),
                body_str
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            return req_str;
        }
    });

    (host, handle)
}

#[test]
fn run_start_sends_selector_inputs_and_idempotency_key() {
    let response_body = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "status": "running"
    });
    let (host, handle) = spawn_fake_server(201, response_body);

    let client = ApiClient::new(host, "my-secret-token-12345".to_string());
    let (runbook_name, version) = parse_runbook_ref("deploy@3").unwrap();

    let body = json!({
        "runbook": runbook_name,
        "version": version,
        "inputs": {
            "count": 3,
            "ok": true,
            "env": "prod"
        },
        "fields": {},
        "tags": ["deploy", "v3"]
    });

    let resp = client
        .post_json("/v1/projects/my-proj/executions", "idem-key-abc-123", &body)
        .unwrap();

    assert_eq!(resp["id"], "exe_01JABCDEFGHJKMNPQRSTVWXYZ0");
    assert_eq!(resp["status"], "running");

    let req_str = handle.join().unwrap();
    // Method and path
    assert!(req_str.starts_with("POST /v1/projects/my-proj/executions HTTP/1.1"));
    // Headers: Authorization, Accept, Idempotency-Key
    assert!(
        req_str.contains("authorization: Bearer my-secret-token-12345")
            || req_str.contains("Authorization: Bearer my-secret-token-12345")
    );
    assert!(
        req_str.contains("accept: application/json")
            || req_str.contains("Accept: application/json")
    );
    assert!(
        req_str.contains("idempotency-key: idem-key-abc-123")
            || req_str.contains("Idempotency-Key: idem-key-abc-123")
    );

    // Token appears ONLY in the Authorization header
    let without_auth = req_str
        .replace("Authorization: Bearer my-secret-token-12345", "")
        .replace("authorization: Bearer my-secret-token-12345", "");
    assert!(!without_auth.contains("my-secret-token-12345"));

    // Body contains version 3 as a number and typed inputs
    assert!(req_str.contains("\"version\":3"));
    assert!(req_str.contains("\"count\":3"));
    assert!(req_str.contains("\"ok\":true"));
}

#[test]
fn run_start_reports_problem_responses() {
    let canned_problem = json!({
        "code": "execution.inputs_invalid",
        "detail": "Input 'service' is required"
    });
    let (host, handle) = spawn_fake_server(422, canned_problem);

    let client = ApiClient::new(host, "dummy-token".to_string());
    let err = client
        .post_json("/v1/projects/p/executions", "key", &json!({}))
        .unwrap_err();

    let req_str = handle.join().unwrap();
    assert!(req_str.contains("POST /v1/projects/p/executions"));

    match err {
        ApiError::Problem {
            status,
            code,
            message,
            ..
        } => {
            assert_eq!(status, 422);
            assert_eq!(code, "execution.inputs_invalid");
            assert_eq!(message, "Input 'service' is required");
        }
        _ => panic!("expected ApiError::Problem, got {err:?}"),
    }
}

#[test]
fn selector_parsing_follows_prd() {
    // 1. deploy -> ("deploy", None)
    let (name, ver) = parse_runbook_ref("deploy").unwrap();
    assert_eq!(name, "deploy");
    assert_eq!(ver, None);

    // 2. deploy@3 -> ("deploy", Some(Value::Number(3)))
    let (name, ver) = parse_runbook_ref("deploy@3").unwrap();
    assert_eq!(name, "deploy");
    assert_eq!(ver, Some(json!(3)));

    // 3. deploy@rbv_<26 chars> -> ("deploy", Some(Value::String("rbv_...")))
    let valid_rbv = "rbv_01JABCDEFGHJKMNPQRSTVWXYZ0";
    let (name, ver) = parse_runbook_ref(&format!("deploy@{valid_rbv}")).unwrap();
    assert_eq!(name, "deploy");
    assert_eq!(ver, Some(json!(valid_rbv)));

    // 4. Rejections:
    // @0
    assert!(parse_runbook_ref("deploy@0").is_err());
    // @-1
    assert!(parse_runbook_ref("deploy@-1").is_err());
    // @
    assert!(parse_runbook_ref("deploy@").is_err());
    // @abc
    assert!(parse_runbook_ref("deploy@abc").is_err());
    // Empty runbook name
    assert!(parse_runbook_ref("").is_err());
    assert!(parse_runbook_ref("@3").is_err());
    assert!(parse_runbook_ref("@rbv_01JABCDEFGHJKMNPQRSTVWXYZ0").is_err());
}

#[test]
fn run_get_reads_execution() {
    let canned_execution = json!({
        "id": "exe_12345",
        "title": "My Execution",
        "status": "completed"
    });
    let (host, handle) = spawn_fake_server(200, canned_execution);

    let client = ApiClient::new(host, "get-token".to_string());
    let resp = client.get_json("/v1/executions/exe_12345").unwrap();

    let req_str = handle.join().unwrap();
    assert!(req_str.starts_with("GET /v1/executions/exe_12345 HTTP/1.1"));
    assert!(
        req_str.contains("authorization: Bearer get-token")
            || req_str.contains("Authorization: Bearer get-token")
    );

    assert_eq!(resp["id"], "exe_12345");
    assert_eq!(resp["title"], "My Execution");
    assert_eq!(resp["status"], "completed");
}

#[test]
fn run_get_rejects_invalid_execution_id() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args(["run", "get", "../traversal"])
        .env("RUNBOOK_TOKEN", "dummy")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid execution ID"));

    let output2 = std::process::Command::new(bin)
        .args(["run", "get", "not-an-exe-id"])
        .env("RUNBOOK_TOKEN", "dummy")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();
    assert_eq!(output2.status.code(), Some(2));
}

#[test]
fn run_start_rejects_invalid_project_id_or_slug() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "run",
            "start",
            "deploy",
            "--project",
            "../x",
            "--idempotency-key",
            "k",
        ])
        .env("RUNBOOK_TOKEN", "dummy")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid project identifier or slug"));

    let output2 = std::process::Command::new(bin)
        .args([
            "run",
            "start",
            "deploy",
            "--project",
            "a?b",
            "--idempotency-key",
            "k",
        ])
        .env("RUNBOOK_TOKEN", "dummy")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();
    assert_eq!(output2.status.code(), Some(2));
}

#[test]
fn run_commands_fail_on_malformed_config() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_dir = temp_dir.path().join("runbook");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), b"this is not valid [[ toml").unwrap();

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args(["run", "get", "exe_01JABCDEFGHJKMNPQRSTVWXYZ0"])
        .env("RUNBOOK_TOKEN", "dummy")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .env("XDG_CONFIG_HOME", temp_dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to parse config"));
}

#[test]
fn project_ref_validation() {
    assert!(validate_project_ref("prj_01JABCDEFGHJKMNPQRSTVWXYZ0").is_ok());
    assert!(validate_project_ref("my-project").is_ok());
    assert!(validate_project_ref("proj123").is_ok());
    assert!(validate_project_ref("a").is_ok());

    assert!(validate_project_ref("").is_err());
    assert!(validate_project_ref("../x").is_err());
    assert!(validate_project_ref("a?b").is_err());
    assert!(validate_project_ref("my_project").is_err());
    assert!(validate_project_ref("-leading").is_err());
    assert!(validate_project_ref("trailing-").is_err());
    assert!(validate_project_ref("double--dash").is_err());
    assert!(validate_project_ref("UPPERCASE").is_err());
}

fn spawn_fake_sse_server<F>(handler: F) -> (String, thread::JoinHandle<()>)
where
    F: FnOnce(TcpListener) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = format!("http://127.0.0.1:{port}");
    let handle = thread::spawn(move || {
        handler(listener);
    });
    (host, handle)
}

fn read_request_str(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

fn accept_sse_stream(listener: &TcpListener) -> (std::net::TcpStream, String) {
    loop {
        let (mut stream, _) = listener.accept().unwrap();
        let req = read_request_str(&mut stream);
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
        return (stream, req);
    }
}

#[test]
fn run_list_query_building() {
    let response_body = json!({
        "items": [],
        "next_cursor": null
    });
    let (host, handle) = spawn_fake_server(200, response_body);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "list",
            "--project",
            "test-proj",
            "--status",
            "running,completed",
            "--tag",
            "prod",
            "--tag",
            "deploy",
            "--runbook",
            "my-rb",
            "--field",
            "count=5",
            "--field",
            "env=production",
            "--limit",
            "50",
            "--cursor",
            "cur_123",
        ])
        .env("RUNBOOK_TOKEN", "token-xyz-list")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));

    let req_str = handle.join().unwrap();
    assert!(req_str.starts_with("GET /v1/projects/test-proj/executions?"));
    assert!(req_str.contains("status=running") && req_str.contains("completed"));
    assert!(req_str.contains("tags=prod") && req_str.contains("deploy"));
    assert!(req_str.contains("runbook=my-rb"));
    assert!(req_str.contains("field.count=5"));
    assert!(req_str.contains("field.env=") && (req_str.contains("production")));
    assert!(req_str.contains("limit=50"));
    assert!(req_str.contains("cursor=cur_123"));
    assert!(
        req_str.contains("Authorization: Bearer token-xyz-list")
            || req_str.contains("authorization: Bearer token-xyz-list")
    );
    assert!(
        req_str.contains("Accept: application/json")
            || req_str.contains("accept: application/json")
    );
}

#[test]
fn run_list_pagination_and_output() {
    let response_body = json!({
        "items": [
            {
                "id": "exe_01",
                "runbook_id": "rb_01",
                "status": "running",
                "title": "Build app",
                "tags": [],
                "fields": {},
                "created_by": "alice",
                "created_at_ms": 1000,
                "completed_at_ms": null
            },
            {
                "id": "exe_02",
                "runbook_id": "rb_02",
                "status": "completed",
                "title": "Deploy prod",
                "tags": ["deploy"],
                "fields": {},
                "created_by": "bob",
                "created_at_ms": 2000,
                "completed_at_ms": 3000
            }
        ],
        "next_cursor": "cur_next_123"
    });
    let (host, handle) = spawn_fake_server(200, response_body);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args(["--host", &host, "run", "list", "--project", "test-proj"])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "exe_01  running  Build app");
    assert_eq!(lines[1], "exe_02  completed  Deploy prod");
    assert_eq!(lines[2], "next: cur_next_123");

    handle.join().unwrap();

    // Now test with --json
    let response_body_json = json!({
        "items": [],
        "next_cursor": "cur_abc"
    });
    let (host2, handle2) = spawn_fake_server(200, response_body_json);
    let output_json = std::process::Command::new(bin)
        .args([
            "--host",
            &host2,
            "run",
            "list",
            "--project",
            "test-proj",
            "--json",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output_json.status.code(), Some(0));
    let stdout_json = String::from_utf8_lossy(&output_json.stdout);
    assert!(stdout_json.contains("\"next_cursor\": \"cur_abc\""));
    handle2.join().unwrap();
}

#[test]
fn run_watch_sse_chunk_boundaries_and_keep_alive() {
    let (host, handle) = spawn_fake_sse_server(|listener| {
        let (mut stream, req) = accept_sse_stream(&listener);
        assert!(req.starts_with("GET /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0/events"));
        assert!(
            req.contains("Accept: text/event-stream") || req.contains("accept: text/event-stream")
        );

        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        stream.flush().unwrap();

        // Chunk 1: keep-alive and partial event
        stream
            .write_all(b": keep-alive\n\n: 15s ping\n\nid: 1\nev")
            .unwrap();
        stream.flush().unwrap();
        thread::sleep(std::time::Duration::from_millis(15));

        // Chunk 2: rest of event 1
        stream
            .write_all(b"ent: StepStarted\ndata: {\"step\":\"build\"}\n\n")
            .unwrap();
        stream.flush().unwrap();
        thread::sleep(std::time::Duration::from_millis(15));

        // Chunk 3: keep-alive and event 2 (ExecutionCompleted)
        stream
            .write_all(b": keep-alive\n\nid: 2\nevent: ExecutionCompleted\ndata: {\"status\":\"succeeded\"}\n\n")
            .unwrap();
        stream.flush().unwrap();
        let _ = stream.shutdown(std::net::Shutdown::Both);
    });

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "watch",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["1 StepStarted", "2 ExecutionCompleted"]);

    handle.join().unwrap();
}

#[test]
fn run_watch_error_frame_exit() {
    let (host, handle) = spawn_fake_sse_server(|listener| {
        let (mut stream, _) = accept_sse_stream(&listener);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        stream
            .write_all(b"id: 5\nevent: error\ndata: {\"code\":\"execution.aborted\"}\n\n")
            .unwrap();
        stream.flush().unwrap();
        let _ = stream.shutdown(std::net::Shutdown::Both);
    });

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "watch",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("execution.aborted"));

    handle.join().unwrap();
}

#[test]
fn run_watch_problem_response_exit() {
    let canned_problem = json!({
        "code": "execution.not_found",
        "detail": "Execution not found"
    });
    let (host, handle) = spawn_fake_server(404, canned_problem);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "watch",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("execution.not_found"));

    handle.join().unwrap();
}

#[test]
fn run_watch_reconnects_once_with_last_seen_id() {
    let (host, handle) = spawn_fake_sse_server(|listener| {
        // Connection 1:
        let (mut stream1, req1) = accept_sse_stream(&listener);
        assert!(!req1.contains("Last-Event-ID:"));

        stream1
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        stream1
            .write_all(b"id: 42\nevent: StepStarted\ndata: {}\n\n")
            .unwrap();
        stream1.flush().unwrap();
        drop(stream1);

        // Connection 2:
        let (mut stream2, req2) = accept_sse_stream(&listener);
        assert!(req2.contains("Last-Event-ID: 42") || req2.contains("last-event-id: 42"));

        stream2
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        stream2
            .write_all(b"id: 43\nevent: ExecutionCompleted\ndata: {\"status\":\"ok\"}\n\n")
            .unwrap();
        stream2.flush().unwrap();
        drop(stream2);
    });

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "watch",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42 StepStarted"));
    assert!(stdout.contains("43 ExecutionCompleted"));

    handle.join().unwrap();
}

#[test]
fn run_watch_json_mode_and_after_flag() {
    let (host, handle) = spawn_fake_sse_server(|listener| {
        let (mut stream, req) = accept_sse_stream(&listener);
        assert!(req.contains("Last-Event-ID: 99") || req.contains("last-event-id: 99"));

        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        stream
            .write_all(b"id: 100\nevent: ExecutionCompleted\ndata: {\"done\":true}\n\n")
            .unwrap();
        stream.flush().unwrap();
        drop(stream);
    });

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "watch",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--after",
            "99",
            "--json",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "{\"done\":true}");

    handle.join().unwrap();
}

#[test]
fn run_watch_rejects_non_numeric_after() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "run",
            "watch",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--after",
            "abc",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn run_signal_sends_payload_and_idempotency_key() {
    let canned_resp = json!({"ok": true});
    let (host, handle) = spawn_fake_server(200, canned_resp);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "signal",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "approve-step",
            "--payload",
            "reason=looks good",
            "--payload",
            "score=42",
            "--idempotency-key",
            "sig-key-123",
        ])
        .env("RUNBOOK_TOKEN", "signal-token")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("signal approve-step sent to exe_01JABCDEFGHJKMNPQRSTVWXYZ0"));

    let req_str = handle.join().unwrap();
    assert!(req_str.starts_with(
        "POST /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0/signals/approve-step HTTP/1.1"
    ));
    assert!(
        req_str.contains("Authorization: Bearer signal-token")
            || req_str.contains("authorization: Bearer signal-token")
    );
    assert!(
        req_str.contains("Accept: application/json")
            || req_str.contains("accept: application/json")
    );
    assert!(
        req_str.contains("Idempotency-Key: sig-key-123")
            || req_str.contains("idempotency-key: sig-key-123")
    );
    assert!(
        req_str.contains("\"payload\":{\"reason\":\"looks good\",\"score\":42}")
            || (req_str.contains("\"reason\":\"looks good\"") && req_str.contains("\"score\":42"))
    );
}

#[test]
fn run_signal_rejects_invalid_signal_name_or_execution_id() {
    let bin = env!("CARGO_BIN_EXE_runbook");

    // Invalid signal names (must match ^[a-z][a-z0-9_-]{0,63}$)
    for bad_name in [
        "InvalidName",
        "1start",
        "-start",
        "_start",
        "has space",
        "a?b",
    ] {
        let output = std::process::Command::new(bin)
            .args(["run", "signal", "exe_01JABCDEFGHJKMNPQRSTVWXYZ0", bad_name])
            .env("RUNBOOK_TOKEN", "token")
            .env("RUNBOOK_HOST", "https://runbook.example.test")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "expected exit 2 for signal name '{bad_name}'"
        );
    }

    // Invalid execution ID
    let output = std::process::Command::new(bin)
        .args(["run", "signal", "not-an-exe", "valid_name"])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn run_signal_reports_problem_response() {
    let canned_problem = json!({
        "code": "signal.invalid",
        "detail": "Signal cannot be received"
    });
    let (host, handle) = spawn_fake_server(422, canned_problem);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "signal",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "my_signal",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("signal.invalid: Signal cannot be received"));
    handle.join().unwrap();
}

#[test]
fn run_report_default_formatting_with_and_without_verdict() {
    let canned_doc = json!({
        "header": {
            "title": "Production Deployment",
            "status": "running"
        },
        "sections": [
            {
                "path": "prep",
                "status": "completed",
                "title": "Preparation",
                "verdict": "passed"
            },
            {
                "path": "deploy",
                "status": "running",
                "title": "Deploy to cluster"
            }
        ]
    });
    let (host, handle) = spawn_fake_server(200, canned_doc);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "report",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "Production Deployment (running)");
    assert_eq!(lines[1], "  prep  completed  Preparation  [passed]");
    assert_eq!(lines[2], "  deploy  running  Deploy to cluster");

    let req_str = handle.join().unwrap();
    assert!(
        req_str.starts_with("GET /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0/report HTTP/1.1")
    );
}

#[test]
fn run_report_json_mode() {
    let canned_doc = json!({
        "header": {
            "title": "My Run",
            "status": "completed"
        },
        "sections": []
    });
    let (host, handle) = spawn_fake_server(200, canned_doc);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "report",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--json",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"title\": \"My Run\""));
    assert!(stdout.contains("\"status\": \"completed\""));
    handle.join().unwrap();
}

#[test]
fn run_report_404_problem_response() {
    let canned_problem = json!({
        "code": "execution.not_found",
        "detail": "Execution not found"
    });
    let (host, handle) = spawn_fake_server(404, canned_problem);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "report",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("execution.not_found: Execution not found"));
    handle.join().unwrap();
}

#[test]
fn run_control_actions_send_body_and_idempotency_key() {
    let bin = env!("CARGO_BIN_EXE_runbook");

    for (action, status_val) in [
        ("cancel", "cancelled"),
        ("pause", "paused"),
        ("resume", "running"),
    ] {
        let canned = json!({
            "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "status": status_val,
        });
        let (host, handle) = spawn_fake_server(200, canned);

        let output = std::process::Command::new(bin)
            .args([
                "--host",
                &host,
                "run",
                action,
                "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
                "--idempotency-key",
                &format!("key-{action}"),
            ])
            .env("RUNBOOK_TOKEN", "token")
            .env("RUNBOOK_HOST", "https://runbook.example.test")
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.trim(),
            format!("exe_01JABCDEFGHJKMNPQRSTVWXYZ0 {status_val}")
        );

        let req_str = handle.join().unwrap();
        assert!(
            req_str.starts_with("PATCH /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0 HTTP/1.1")
        );
        assert!(
            req_str.contains(&format!("Idempotency-Key: key-{action}"))
                || req_str.contains(&format!("idempotency-key: key-{action}"))
        );
        assert!(req_str.contains(&format!("\"action\":\"{action}\"")));
    }
}

#[test]
fn run_update_sends_only_given_keys() {
    let bin = env!("CARGO_BIN_EXE_runbook");

    // 1. Update title and tags only
    let canned1 = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "status": "running",
    });
    let (host1, handle1) = spawn_fake_server(200, canned1);

    let output1 = std::process::Command::new(bin)
        .args([
            "--host",
            &host1,
            "run",
            "update",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--title",
            "New Title",
            "--tag",
            "prod",
            "--tag",
            "v2",
            "--idempotency-key",
            "upd-key-1",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output1.status.code(), Some(0));
    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    assert_eq!(stdout1.trim(), "exe_01JABCDEFGHJKMNPQRSTVWXYZ0 running");

    let req_str1 = handle1.join().unwrap();
    assert!(req_str1.starts_with("PATCH /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0 HTTP/1.1"));
    assert!(
        req_str1.contains("Idempotency-Key: upd-key-1")
            || req_str1.contains("idempotency-key: upd-key-1")
    );
    assert!(req_str1.contains("\"title\":\"New Title\""));
    assert!(req_str1.contains("\"tags\":[\"prod\",\"v2\"]"));
    assert!(!req_str1.contains("\"fields\""));

    // 2. Update fields only
    let canned2 = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "status": "running",
    });
    let (host2, handle2) = spawn_fake_server(200, canned2);

    let output2 = std::process::Command::new(bin)
        .args([
            "--host",
            &host2,
            "run",
            "update",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--field",
            "env=prod",
            "--field",
            "count=3",
            "--idempotency-key",
            "upd-key-2",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output2.status.code(), Some(0));
    let req_str2 = handle2.join().unwrap();
    assert!(req_str2.starts_with("PATCH /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0 HTTP/1.1"));
    assert!(!req_str2.contains("\"title\""));
    assert!(!req_str2.contains("\"tags\""));
    assert!(
        req_str2.contains("\"fields\":{\"count\":3,\"env\":\"prod\"}")
            || (req_str2.contains("\"count\":3") && req_str2.contains("\"env\":\"prod\""))
    );
}

#[test]
fn run_update_with_no_options_exits_2_without_request() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args(["run", "update", "exe_01JABCDEFGHJKMNPQRSTVWXYZ0"])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error: nothing to update"));
}

#[test]
fn run_control_problem_response_exits_1() {
    let canned_problem = json!({
        "code": "execution.closed",
        "detail": "Execution is already closed"
    });
    let (host, handle) = spawn_fake_server(409, canned_problem);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "cancel",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("execution.closed: Execution is already closed"));
    handle.join().unwrap();
}

#[test]
fn run_control_and_update_reject_invalid_execution_id() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    for cmd in ["cancel", "pause", "resume"] {
        let output = std::process::Command::new(bin)
            .args(["run", cmd, "not_an_exe_id"])
            .env("RUNBOOK_TOKEN", "token")
            .env("RUNBOOK_HOST", "https://runbook.example.test")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid execution ID: 'not_an_exe_id'"));
    }

    let output_upd = std::process::Command::new(bin)
        .args(["run", "update", "not_an_exe_id", "--title", "Hello"])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();
    assert_eq!(output_upd.status.code(), Some(2));
    let stderr_upd = String::from_utf8_lossy(&output_upd.stderr);
    assert!(stderr_upd.contains("invalid execution ID: 'not_an_exe_id'"));
}

#[test]
fn run_control_and_update_json_output() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let canned = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "status": "paused",
        "title": "My Execution"
    });
    let (host, handle) = spawn_fake_server(200, canned.clone());

    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "pause",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--idempotency-key",
            "pause-key",
            "--json",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["status"], "paused");
    handle.join().unwrap();
}

#[test]
fn run_control_auto_generates_idempotency_key() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let canned = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "status": "cancelled",
    });
    let (host, handle) = spawn_fake_server(200, canned);

    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "cancel",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Idempotency-Key: "));

    let req_str = handle.join().unwrap();
    assert!(req_str.contains("Idempotency-Key: ") || req_str.contains("idempotency-key: "));
}

#[test]
fn run_rerun_sends_empty_body_when_no_version() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let canned = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ1",
        "status": "running"
    });
    let (host, handle) = spawn_fake_server(201, canned);

    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "rerun",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--idempotency-key",
            "rerun-key-1",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "started exe_01JABCDEFGHJKMNPQRSTVWXYZ1 (running) rerun of exe_01JABCDEFGHJKMNPQRSTVWXYZ0"
    );

    let req_str = handle.join().unwrap();
    assert!(
        req_str.starts_with("POST /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0/rerun HTTP/1.1")
    );
    assert!(
        req_str.contains("Idempotency-Key: rerun-key-1")
            || req_str.contains("idempotency-key: rerun-key-1")
    );
    assert!(req_str.ends_with("\r\n\r\n{}"));
}

#[test]
fn run_rerun_with_numeric_version_sends_number() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let canned = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ1",
        "status": "running"
    });
    let (host, handle) = spawn_fake_server(201, canned);

    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "rerun",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--version",
            "3",
            "--idempotency-key",
            "rerun-key-2",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let req_str = handle.join().unwrap();
    assert!(req_str.contains("\"version\":3"));
}

#[test]
fn run_rerun_with_rbv_version_sends_string() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let canned = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ1",
        "status": "running"
    });
    let (host, handle) = spawn_fake_server(201, canned);

    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "rerun",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--version",
            "rbv_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--idempotency-key",
            "rerun-key-3",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let req_str = handle.join().unwrap();
    assert!(req_str.contains("\"version\":\"rbv_01JABCDEFGHJKMNPQRSTVWXYZ0\""));
}

#[test]
fn run_rerun_with_invalid_version_exits_2() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "run",
            "rerun",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--version",
            "abc",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid version"));
}

#[test]
fn run_fields_set_sends_only_fields() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let canned = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "status": "running"
    });
    let (host, handle) = spawn_fake_server(200, canned);

    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "fields",
            "set",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "foo=bar",
            "count=42",
            "--idempotency-key",
            "fields-key-1",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "exe_01JABCDEFGHJKMNPQRSTVWXYZ0 running");

    let req_str = handle.join().unwrap();
    assert!(req_str.starts_with("PATCH /v1/executions/exe_01JABCDEFGHJKMNPQRSTVWXYZ0 HTTP/1.1"));
    assert!(!req_str.contains("\"title\""));
    assert!(!req_str.contains("\"tags\""));
    assert!(
        req_str.contains("\"fields\":{\"count\":42,\"foo\":\"bar\"}")
            || (req_str.contains("\"count\":42") && req_str.contains("\"foo\":\"bar\""))
    );
}

#[test]
fn run_rerun_and_fields_set_problem_responses_exit_1() {
    let canned_problem = json!({
        "code": "invalid_state",
        "detail": "Cannot rerun closed execution"
    });
    let (host, handle) = spawn_fake_server(422, canned_problem);

    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "rerun",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid_state: Cannot rerun closed execution"));
    handle.join().unwrap();
}

#[test]
fn run_rerun_json_mode() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let canned = json!({
        "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ1",
        "status": "running",
        "rerun_of": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0"
    });
    let (host, handle) = spawn_fake_server(201, canned);

    let output = std::process::Command::new(bin)
        .args([
            "--host",
            &host,
            "run",
            "rerun",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "--idempotency-key",
            "rerun-key-json",
            "--json",
        ])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["id"], "exe_01JABCDEFGHJKMNPQRSTVWXYZ1");
    handle.join().unwrap();
}

#[test]
fn run_rerun_invalid_exe_id_exits_2() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args(["run", "rerun", "invalid_exe"])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid execution ID: 'invalid_exe'"));
}

#[test]
fn run_fields_set_no_fields_exits_2() {
    let bin = env!("CARGO_BIN_EXE_runbook");
    let output = std::process::Command::new(bin)
        .args(["run", "fields", "set", "exe_01JABCDEFGHJKMNPQRSTVWXYZ0"])
        .env("RUNBOOK_TOKEN", "token")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
}

/// LANG4: `run get` names the parent call step and each call step's child.
#[test]
fn run_get_shows_the_call_links() {
    let (host, handle) = spawn_fake_server(
        200,
        json!({
            "id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "title": "Deploy",
            "status": "running",
            "parent": {"execution_id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ1", "step_run_id": "sr_01JABCDEFGHJKMNPQRSTVWXYZ0", "step": "release", "depth": 1},
            "steps": [
                {"path": "db", "call": {"child_execution_id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ2", "started": true}},
                {"path": "cache", "call": {"child_execution_id": "exe_01JABCDEFGHJKMNPQRSTVWXYZ3", "started": false, "error": "limit.executions_monthly"}},
                {"path": "verify"}
            ]
        }),
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_runbook"))
        .args([
            "--host",
            &host,
            "run",
            "get",
            "exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        ])
        .env("RUNBOOK_TOKEN", "dummy")
        .env("RUNBOOK_HOST", "https://runbook.example.test")
        .output()
        .unwrap();
    handle.join().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        "exe_01JABCDEFGHJKMNPQRSTVWXYZ0 Deploy (running)\n\
         \x20 called by exe_01JABCDEFGHJKMNPQRSTVWXYZ1 (step release)\n\
         \x20 step db runs exe_01JABCDEFGHJKMNPQRSTVWXYZ2\n\
         \x20 step cache runs exe_01JABCDEFGHJKMNPQRSTVWXYZ3 (limit.executions_monthly)\n"
    );
}
