//! A scripted fake API server (one reply per connection, in order) that
//! records every request, plus helpers to run the `runbook` binary.
#![allow(dead_code)]
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const PAT: &str = "trb_pat_test-secret";

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub etag: Option<String>,
}

pub fn reply(status: u16, body: Value) -> Reply {
    Reply {
        status,
        body,
        etag: None,
    }
}

pub fn with_etag(status: u16, body: Value, etag: &str) -> Reply {
    Reply {
        status,
        body,
        etag: Some(etag.into()),
    }
}

pub fn problem(status: u16, code: &str) -> Reply {
    reply(status, serde_json::json!({"code": code, "title": code}))
}

/// Serves the script, then stops after five idle seconds; returns every
/// request it saw.
pub fn serve(replies: Vec<Reply>) -> (String, thread::JoinHandle<Vec<String>>) {
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
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                    Err(_) => return seen,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let req = read_request(&mut stream);
            if req.starts_with("GET /.well-known/runbook ")
                && replies[idx].body.get("minimum_cli_version").is_none()
            {
                let text = r#"{"minimum_cli_version":"0.1.0"}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                continue;
            }
            let r = &replies[idx];
            idx += 1;
            seen.push(req);
            let text = r.body.to_string();
            let kind = if r.status >= 400 {
                "application/problem+json"
            } else {
                "application/json"
            };
            let etag = r
                .etag
                .as_ref()
                .map(|e| format!("ETag: {e}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 {} X\r\nContent-Type: {kind}\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                r.status,
                text.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
        // Report any request that arrives after the script ran out.
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if let Ok((mut s, _)) = listener.accept() {
                s.set_nonblocking(false).unwrap();
                seen.push(read_request(&mut s));
                let text = r#"{"code":"test.script_exhausted"}"#;
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 500 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    )
                    .as_bytes(),
                );
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        seen
    });
    (host, handle)
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf = vec![];
    let mut chunk = [0u8; 4096];
    let mut need = None;
    loop {
        if let Some(n) = need
            && buf.len() >= n
        {
            break;
        }
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(_) => break,
        };
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

pub fn request_line(request: &str) -> &str {
    request.lines().next().unwrap_or("")
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

pub fn cli(host: &str, home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .arg("--host")
        .arg(host)
        .args(args)
        .env_remove("RUNBOOK_TOKEN")
        .env_remove("RUNBOOK_HOST")
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    command.output().unwrap()
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// One answer from [`serve_by`].
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

pub fn answer(status: u16, body: Value) -> Answer {
    Answer {
        status,
        headers: vec![],
        body,
    }
}

/// Answers every request with `f(request)` until four idle seconds pass;
/// returns every request it saw (the version probe excepted).
pub fn serve_by(
    f: impl Fn(&str) -> Answer + Send + 'static,
) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let handle = thread::spawn(move || {
        let mut seen = vec![];
        loop {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    Err(_) => return seen,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let req = read_request(&mut stream);
            let a = if req.starts_with("GET /.well-known/runbook ") {
                answer(200, serde_json::json!({"minimum_cli_version": "0.1.0"}))
            } else {
                let a = f(&req);
                seen.push(req);
                a
            };
            let text = a.body.to_string();
            let mut head = format!(
                "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                a.status,
                if a.status >= 400 {
                    "application/problem+json"
                } else {
                    "application/json"
                },
                text.len()
            );
            for (k, v) in &a.headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(text.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    (host, handle)
}
