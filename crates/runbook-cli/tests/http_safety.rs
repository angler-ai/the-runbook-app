//! F73: the CLI never follows a redirect (a bearer token must not travel to
//! another host or path), and every agent-loop request goes to the expected
//! route with the expected credential: the execution access token, never
//! another one.
mod support;
use runbook_cli::credstore::{ExecutionCredential, StateDir};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use support::*;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const CLAIM: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3";
const ACCESS: &str = "trb_exe_access-only-for-the-api";

/// Answers every connection with a 307 to `target`.
fn redirector(target: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
        }
    });
    host
}

/// Records whether anything ever connects.
fn elsewhere() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/stolen",
        listener.local_addr().unwrap().port()
    );
    let seen = Arc::new(Mutex::new(vec![]));
    let record = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            record
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).into());
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
    (url, seen)
}

#[test]
fn redirects_are_never_followed_with_a_bearer_token() {
    let home = tempfile::tempdir().unwrap();
    let (target, seen) = elsewhere();
    let host = redirector(target);
    let token = [("RUNBOOK_TOKEN", PAT)];
    for args in [
        vec!["run", "get", EXE, "--json"],
        vec!["run", "list", "--project", "default", "--json"],
        vec!["run", "watch", EXE],
        vec!["list", "--project", "default", "--json"],
    ] {
        let o = cli(&host, home.path(), &args, &token);
        assert_ne!(o.status.code(), Some(0), "{args:?}: {}", out(&o));
        assert!(err(&o).contains("redirect"), "{args:?}: {}", err(&o));
    }
    thread::sleep(Duration::from_millis(200));
    assert!(
        seen.lock().unwrap().is_empty(),
        "a request followed the redirect"
    );
}

#[test]
fn agent_loop_requests_use_the_execution_access_token_on_the_expected_routes() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(
            200,
            json!({"execution_id": EXE, "ready_work": [{"step_run_id": SR, "path": "check", "title": "Check", "actor_kind": "agent"}], "next_action": "claim_ready_work"}),
        ),
        reply(200, json!({"id": CLAIM, "generation": 1})),
        reply(
            200,
            json!({"step_run_id": SR, "instructions": "Check.", "checklist": [], "parameters": {}, "output_schema": {}, "skill": null}),
        ),
        reply(200, json!({"status": "succeeded"})),
        reply(
            200,
            json!({"execution_id": EXE, "ready_work": [], "next_action": "wait"}),
        ),
    ]);
    let s = StateDir::at(home.path().join("runbook").join("credentials"));
    s.save_execution(&ExecutionCredential {
        host: host.clone(),
        execution_id: EXE.into(),
        family_id: "fam_1".into(),
        generation: 1,
        access_token: ACCESS.into(),
        refresh_token: "trb_exr_refresh".into(),
        expires_at_ms: i64::MAX,
        refresh_expires_at_ms: i64::MAX,
    })
    .unwrap();
    s.set_active(&host, EXE).unwrap();
    for args in [
        vec!["step", "next", EXE, "--json"],
        vec![
            "step",
            "claim",
            SR,
            "--session",
            "s1",
            "--agent-name",
            "a",
            "--json",
        ],
        vec!["step", "get", SR, "--json"],
        vec!["step", "report", SR, "--status", "succeeded", "--json"],
    ] {
        let o = cli(&host, home.path(), &args, &[]);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", err(&o));
    }
    let seen = server.join().unwrap();
    let expected = [
        format!("GET /v1/executions/{EXE}/next "),
        format!("POST /v1/step-runs/{SR}/claim "),
        format!("GET /v1/step-runs/{SR} "),
        format!("POST /v1/step-runs/{SR}/report "),
        format!("GET /v1/executions/{EXE}/next "),
    ];
    assert_eq!(seen.len(), expected.len(), "{seen:?}");
    for (request, line) in seen.iter().zip(expected) {
        assert!(
            request_line(request).starts_with(&line),
            "{} != {line}",
            request_line(request)
        );
        assert_eq!(
            header(request, "authorization"),
            Some(format!("Bearer {ACCESS}").as_str()),
            "{line}"
        );
    }
}
