//! Shared fake API for the C2 administration, token, webhook and run
//! ergonomics tests: one scripted reply per connection, in order.
#![allow(dead_code)]
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const ORG: &str = "org_01JABCDEFGHJKMNPQRSTVWXYZ0";
pub const USR: &str = "usr_01JABCDEFGHJKMNPQRSTVWXYZ1";
pub const PRJ: &str = "prj_01JABCDEFGHJKMNPQRSTVWXYZ2";
pub const TEAM: &str = "team_01JABCDEFGHJKMNPQRSTVWXYZ3";
pub const TOK: &str = "tok_01JABCDEFGHJKMNPQRSTVWXYZ4";
pub const WHK: &str = "whk_01JABCDEFGHJKMNPQRSTVWXYZ5";
pub const WHD: &str = "whd_01JABCDEFGHJKMNPQRSTVWXYZ6";
pub const RB: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ7";
pub const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ8";
pub const PAT: &str = "trb_pat_caller-secret";

pub struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            content_type: if status >= 400 {
                "application/problem+json"
            } else {
                "application/json"
            },
            headers: Vec::new(),
            body: body.to_string().into_bytes(),
        }
    }
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: "",
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
    pub fn text(status: u16, content_type: &'static str, body: &str) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }
    pub fn raw(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body,
        }
    }
    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

/// Serves `first` on the first request, and `repeating` on all subsequent requests,
/// stopping after two seconds of idle time.
pub fn serve_repeating(
    first: Reply,
    repeating: Reply,
) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = thread::spawn(move || {
        let mut seen = vec![];
        let mut is_first = true;
        loop {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    Err(_) => return seen,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let req = read_request(&mut stream);
            if req.starts_with("GET /.well-known/runbook ")
                && !std::str::from_utf8(&first.body)
                    .is_ok_and(|s| s.contains("minimum_cli_version"))
                && !std::str::from_utf8(&repeating.body)
                    .is_ok_and(|s| s.contains("minimum_cli_version"))
            {
                let body = r#"{"minimum_cli_version":"0.1.0"}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                continue;
            }
            seen.push(req);
            let reply = if is_first {
                is_first = false;
                &first
            } else {
                &repeating
            };
            let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
            if !reply.content_type.is_empty() {
                head.push_str(&format!("Content-Type: {}\r\n", reply.content_type));
            }
            for (k, v) in &reply.headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            if reply.content_type != "text/event-stream" {
                head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&reply.body).unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    (host, handle)
}

/// Serves the replies in order; stops after the script or after five idle
/// seconds and returns every request it saw.
pub fn serve(replies: Vec<Reply>) -> (String, thread::JoinHandle<Vec<String>>) {
    let (host, handle) = serve_timed(replies);
    let untimed = thread::spawn(move || {
        handle
            .join()
            .unwrap()
            .into_iter()
            .map(|(_, req)| req)
            .collect()
    });
    (host, untimed)
}

/// Serves the replies in order, recording the Instant each connection was accepted.
pub fn serve_timed(replies: Vec<Reply>) -> (String, thread::JoinHandle<Vec<(Instant, String)>>) {
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
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    Err(_) => return seen,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let req = read_request(&mut stream);
            if req.starts_with("GET /.well-known/runbook ")
                && !std::str::from_utf8(&replies[idx].body)
                    .is_ok_and(|s| s.contains("minimum_cli_version"))
            {
                let body = r#"{"minimum_cli_version":"0.1.0"}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                continue;
            }
            let now = Instant::now();
            seen.push((now, req));
            let reply = &replies[idx];
            idx += 1;
            let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status);
            if !reply.content_type.is_empty() {
                head.push_str(&format!("Content-Type: {}\r\n", reply.content_type));
            }
            for (k, v) in &reply.headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            if reply.content_type != "text/event-stream" {
                head.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&reply.body).unwrap();
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

/// `METHOD /path?query` of a recorded request.
pub fn line(request: &str) -> String {
    let first = request.lines().next().unwrap_or("");
    first.trim_end_matches(" HTTP/1.1").to_string()
}

pub fn body_of(request: &str) -> Value {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or(Value::Null)
}

pub fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Runs the binary against `host` with a private config home.
pub fn cli(
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

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
pub fn token() -> [(&'static str, &'static str); 1] {
    [("RUNBOOK_TOKEN", PAT)]
}
pub fn orgs() -> Reply {
    Reply::json(
        200,
        serde_json::json!({"items":[{"id":ORG,"title":"Runbook"}],"next_cursor":null}),
    )
}
