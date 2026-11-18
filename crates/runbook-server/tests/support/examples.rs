//! Helpers for the `examples/webhooks/` recipe tests: a recording mock
//! HTTP server on loopback (standing in for the GitHub API or a chat
//! webhook) and a launcher for the example Python 3 programs, which use
//! only the standard library. Tests skip when `python3` is unavailable.
#![allow(dead_code)]
use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}
impl Recorded {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap()
    }
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
            .unwrap_or("")
    }
}

/// Records every request and answers with scripted statuses (`default`
/// once the script is empty).
#[derive(Clone)]
pub struct MockHttp {
    pub port: u16,
    seen: Arc<Mutex<Vec<Recorded>>>,
    statuses: Arc<Mutex<VecDeque<u16>>>,
}
impl MockHttp {
    pub fn start(default: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mock = Self {
            port: listener.local_addr().unwrap().port(),
            seen: Arc::default(),
            statuses: Arc::default(),
        };
        let shared = mock.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let head_end = loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                    if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(i);
                    }
                };
                let Some(head_end) = head_end else { continue };
                let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
                let mut request_line = head.lines().next().unwrap_or("").split(' ');
                let method = request_line.next().unwrap_or("").to_owned();
                let path = request_line.next().unwrap_or("").to_owned();
                let headers: BTreeMap<String, String> = head
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let length: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = buffer[head_end + 4..].to_vec();
                while body.len() < length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                    }
                }
                shared.seen.lock().unwrap().push(Recorded {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let status = shared
                    .statuses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(default);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} Mock\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                );
            }
        });
        mock
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    pub fn seen(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }
    pub fn script(&self, status: u16) {
        self.statuses.lock().unwrap().push_back(status);
    }
}

pub fn example(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/webhooks")
        .join(path)
}

/// Whether a usable `python3` is on PATH.
pub fn python() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A running example program; killed on drop.
pub struct Program {
    child: Child,
    pub url: String,
}
impl Program {
    /// Start `python3 -I <script>` with exactly these variables (plus PATH)
    /// and wait for its `listening on http://host:port` line.
    pub fn start(script: &str, env: &[(&str, String)]) -> Self {
        let mut command = Command::new("python3");
        command
            .arg("-I")
            .arg(example(script))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn python3");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            if let Some(Ok(line)) = lines.next() {
                let _ = tx.send(line);
            }
            // Keep draining so the program never blocks on a full pipe.
            for _ in lines {}
        });
        let line = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("example program did not report its address");
        let url = line
            .strip_prefix("listening on ")
            .unwrap_or_else(|| panic!("unexpected first line: {line}"))
            .trim()
            .to_owned();
        Self { child, url }
    }
}
impl Drop for Program {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// POST a webhook delivery the way the server does: the given body,
/// event headers and a signature header. Returns (status, body).
pub fn deliver(
    url: &str,
    event_id: &str,
    event_type: &str,
    timestamp: &str,
    signature: &str,
    body: &str,
) -> (u16, String) {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let result = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set("User-Agent", "TheRunbook-Webhooks/1")
        .set("Runbook-Event-Id", event_id)
        .set("Runbook-Event-Type", event_type)
        .set("Runbook-Delivery-Id", "whd_01JABCDEFGHJKMNPQRSTVWXYZ0")
        .set("Runbook-Timestamp", timestamp)
        .set("Runbook-Signature", signature)
        .send_string(body);
    match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => {
            let status = response.status();
            (status, response.into_string().unwrap_or_default())
        }
        Err(error) => panic!("delivery failed: {error}"),
    }
}

/// Send raw bytes as one HTTP request and return the status code, or
/// `None` if the program did not answer within three seconds (a stall)
/// or closed the connection without a response.
pub fn raw(url: &str, request: &[u8]) -> Option<u16> {
    let address = url.trim_start_matches("http://").trim_end_matches('/');
    let mut stream = std::net::TcpStream::connect(address).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    stream.write_all(request).ok()?;
    let mut response = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&chunk[..n]);
                if response.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let text = String::from_utf8_lossy(&response);
    text.split(' ').nth(1)?.parse().ok()
}

/// A signed-looking POST with raw header bytes, for malformed-input tests.
pub fn raw_post(content_length: &str, timestamp: &[u8], signature: &[u8], body: &str) -> Vec<u8> {
    let mut request = format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\nRunbook-Timestamp: "
    )
    .into_bytes();
    request.extend_from_slice(timestamp);
    request.extend_from_slice(b"\r\nRunbook-Signature: ");
    request.extend_from_slice(signature);
    request.extend_from_slice(b"\r\n\r\n");
    request.extend_from_slice(body.as_bytes());
    request
}
