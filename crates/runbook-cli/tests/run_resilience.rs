//! F63: `run get --wait` and `run start --wait` keep waiting through
//! transient failures (transport errors, 5xx, 429) with capped backoff,
//! and still stop at once on a real error. F64: `run watch` reconnects for
//! the whole stream (resuming with Last-Event-ID), not just once.
mod support;
use serde_json::json;
use std::io::Write;
use std::net::TcpListener;
use std::thread;
use support::*;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const TOKEN: [(&str, &str); 1] = [("RUNBOOK_TOKEN", PAT)];

#[test]
fn run_get_wait_survives_transient_failures() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(200, json!({"id": EXE, "status": "running"})),
        problem(503, "storage.unavailable"),
        problem(502, "gateway.bad"),
        problem(429, "rate_limited"),
        reply(200, json!({"id": EXE, "status": "succeeded"})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["run", "get", EXE, "--wait", "--interval-ms", "10"],
        &TOKEN,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 5, "{seen:?}");
}

#[test]
fn run_start_wait_survives_a_transient_failure() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(
            201,
            json!({"id": EXE, "status": "queued", "runbook_version_id": "rbv_01JABCDEFGHJKMNPQRSTVWXYZ6", "runbook_version": 1}),
        ),
        problem(503, "storage.unavailable"),
        reply(200, json!({"id": EXE, "status": "succeeded"})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &[
            "run",
            "start",
            "deploy",
            "--project",
            "default",
            "--wait",
            "--interval-ms",
            "10",
        ],
        &TOKEN,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // One start only: a transient failure while waiting never starts again.
    let seen = server.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|r| request_line(r).starts_with("POST"))
            .count(),
        1
    );
}

#[test]
fn run_get_wait_stops_at_once_on_a_real_error() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(200, json!({"id": EXE, "status": "running"})),
        problem(404, "execution.not_found"),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["run", "get", EXE, "--wait", "--interval-ms", "10"],
        &TOKEN,
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("execution.not_found"));
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn run_get_wait_gives_up_after_repeated_transient_failures() {
    let home = tempfile::tempdir().unwrap();
    let mut replies = vec![reply(200, json!({"id": EXE, "status": "running"}))];
    replies.extend((0..12).map(|_| problem(503, "storage.unavailable")));
    let (host, server) = serve(replies);
    let o = cli(
        &host,
        home.path(),
        &["run", "get", EXE, "--wait", "--interval-ms", "1"],
        &TOKEN,
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(seen.len() < 13, "it must give up: {}", seen.len());
}

/// Serves one event stream per connection, in order; returns the requests.
fn streams(frames: Vec<&'static str>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = thread::spawn(move || {
        let mut seen = vec![];
        let mut idx = 0;
        while idx < frames.len() {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0u8; 4096];
            let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
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
            seen.push(req);
            let body = frames[idx];
            idx += 1;
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        seen
    });
    (host, handle)
}

#[test]
fn run_watch_reconnects_for_the_whole_stream() {
    let home = tempfile::tempdir().unwrap();
    // More dropped connections than the consecutive-failure budget: every
    // connection delivered an event, so none of them counts as a failure.
    let mut frames: Vec<&'static str> = (1..=10)
        .map(|n| {
            &*Box::leak(
                format!("id: {n}\nevent: StepReady\ndata: {{\"seq\":{n}}}\n\n").into_boxed_str(),
            )
        })
        .collect();
    frames.push("id: 11\nevent: ExecutionCompleted\ndata: {\"seq\":11,\"event\":{\"type\":\"ExecutionCompleted\",\"status\":\"succeeded\"}}\n\n");
    let (host, server) = streams(frames);
    let o = cli(&host, home.path(), &["run", "watch", EXE], &TOKEN);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 11);
    // Each reconnect resumes after the last event it printed.
    for (n, request) in seen.iter().enumerate().skip(1) {
        assert_eq!(
            header(request, "last-event-id"),
            Some(n.to_string().as_str())
        );
    }
    let printed = out(&o);
    assert_eq!(printed.lines().count(), 11, "{printed}");
}
