//! A stand-in for the TypeScript auth service (AUTH1), built into each test
//! binary that uses it: the test binary re-runs itself with
//! `RUNBOOK_AUTH_STUB=1` and serves the contract in `auth/README.md` on
//! `RUNBOOK_AUTH_PORT`, printing the same ready line once it listens.
//!
//! - `POST /internal/session` with `x-runbook-internal-secret` and the
//!   session cookie as the `Cookie` header: the session in `STUB_SESSIONS`
//!   (a JSON object keyed by the cookie's value) or 401.
//! - `POST /api/auth/sign-out`: removes the cookie's session.
//! - `GET /api/auth/register-check?email=`: asks
//!   `RUNBOOK_AUTH_CAN_REGISTER_URL` like Better Auth's `user.create.before`
//!   hook.
//! - `/api/auth/redirect`: a 302 the proxy must not follow.
//! - any other `/api/auth/*`: echoes what arrived, with two `Set-Cookie`.
//!
//! Every answer carries `x-runbook-auth-proof` for the request's nonce
//! unless `STUB_NO_PROOF` is set. Every request is appended to `STUB_LOG`
//! (JSON lines).
#![allow(dead_code)]
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

pub const ENV: &str = "RUNBOOK_AUTH_STUB";

/// The command that runs the stub: this test binary, one test, exact.
pub fn command(entry: &str) -> Vec<String> {
    vec![
        std::env::current_exe().unwrap().display().to_string(),
        "--exact".into(),
        entry.into(),
        "--nocapture".into(),
        "--test-threads".into(),
        "1".into(),
    ]
}

/// The files one stub instance reads and writes.
pub struct Files {
    pub sessions: PathBuf,
    pub log: PathBuf,
}
impl Files {
    pub fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("auth-stub-{}-{tag}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = Self {
            sessions: dir.join("sessions.json"),
            log: dir.join("log.jsonl"),
        };
        std::fs::write(&files.sessions, "{}").unwrap();
        std::fs::write(&files.log, "").unwrap();
        files
    }
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (ENV.into(), "1".into()),
            ("STUB_SESSIONS".into(), self.sessions.display().to_string()),
            ("STUB_LOG".into(), self.log.display().to_string()),
        ]
    }
    pub fn put_session(&self, token: &str, auth_user_id: &str, email: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        self.update(|s| {
            s.insert(
                token.into(),
                json!({"auth_user_id":auth_user_id,"email":email,"email_verified":true,
                       "session_id":format!("sess_{token}"),"expires_at":now + 3_600_000}),
            );
        });
    }
    pub fn remove_session(&self, token: &str) {
        self.update(|s| {
            s.remove(token);
        });
    }
    pub fn update(&self, f: impl FnOnce(&mut BTreeMap<String, Value>)) {
        let mut sessions: BTreeMap<String, Value> =
            serde_json::from_str(&std::fs::read_to_string(&self.sessions).unwrap()).unwrap();
        f(&mut sessions);
        let tmp = self.sessions.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string(&sessions).unwrap()).unwrap();
        std::fs::rename(tmp, &self.sessions).unwrap();
    }
    pub fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

thread_local! {
    /// The proof header for the request this thread is answering.
    static PROOF: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The stub's main loop; never returns. Binds `RUNBOOK_AUTH_PORT` (0, as
/// the server passes it, picks a free port) and reports the port it got.
pub fn run() -> ! {
    let port: u16 = std::env::var("RUNBOOK_AUTH_PORT").unwrap().parse().unwrap();
    let mut listener = None;
    for _ in 0..100 {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let listener = listener.expect("bind");
    let port = listener.local_addr().unwrap().port();
    // On a line of its own: the test harness has already written
    // `test auth_stub_child ... ` without a newline.
    println!(
        "\n{}",
        json!({"event": "ready", "port": port, "methods": ["email"]})
    );
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || {
            let _ = handle(stream);
        });
    }
    std::process::exit(0)
}

struct Incoming {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Incoming {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn read(stream: &mut TcpStream) -> Option<Incoming> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Incoming {
        method,
        target,
        headers,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, headers: &[(&str, String)], body: &str) {
    let mut out = format!(
        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(proof) = PROOF.with(|p| p.borrow().clone()) {
        out.push_str(&format!("x-runbook-auth-proof: {proof}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    let _ = stream.write_all(out.as_bytes());
}

fn sessions() -> BTreeMap<String, Value> {
    let path = std::env::var("STUB_SESSIONS").unwrap();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_default()).unwrap_or_default()
}

fn cookie_value(cookie: &str) -> Option<String> {
    cookie
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| k.ends_with("runbook.session_token"))
        .map(|(_, v)| v.to_owned())
}

fn handle(mut stream: TcpStream) -> Option<()> {
    let request = read(&mut stream)?;
    let (path, query) = request
        .target
        .split_once('?')
        .unwrap_or((&request.target, ""));
    let (path, query) = (path.to_owned(), query.to_owned());
    let headers: BTreeMap<_, _> = request.headers.iter().cloned().collect();
    let body_text = String::from_utf8_lossy(&request.body).to_string();
    {
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(std::env::var("STUB_LOG").unwrap())
            .ok()?;
        let line = json!({"method":request.method,"path":path,"query":query,"headers":headers,"body":body_text});
        let _ = writeln!(log, "{line}");
    }
    let secret = std::env::var("RUNBOOK_AUTH_INTERNAL_SECRET").unwrap();
    // Gate fix 2: prove the secret on every answer (unless told not to).
    let proof = request
        .header("x-runbook-auth-nonce")
        .filter(|_| std::env::var_os("STUB_NO_PROOF").is_none())
        .map(|nonce| runbook_server::auth_child::proof_for(&secret, nonce));
    PROOF.with(|p| *p.borrow_mut() = proof);
    let json_type = ("content-type", "application/json".to_owned());
    match (request.method.as_str(), path.as_str()) {
        ("POST", "/internal/session") => {
            if request.header("x-runbook-internal-secret") != Some(secret.as_str()) {
                respond(&mut stream, 401, &[], "");
                return Some(());
            }
            let token = cookie_value(request.header("cookie")?)?;
            match sessions().get(&token) {
                Some(session) => respond(&mut stream, 200, &[json_type], &session.to_string()),
                None => respond(&mut stream, 401, &[], ""),
            }
        }
        ("POST", "/api/auth/sign-out") => {
            if let Some(token) = request.header("cookie").and_then(cookie_value) {
                let mut all = sessions();
                all.remove(&token);
                let path = std::env::var("STUB_SESSIONS").unwrap();
                let tmp = format!("{path}.signout");
                std::fs::write(&tmp, serde_json::to_string(&all).unwrap()).ok()?;
                std::fs::rename(tmp, path).ok()?;
            }
            respond(
                &mut stream,
                200,
                &[
                    json_type,
                    ("set-cookie", "runbook.session_token=; Max-Age=0; Path=/".into()),
                ],
                r#"{"success":true}"#,
            );
        }
        ("GET", "/api/auth/register-check") => {
            let email = query.strip_prefix("email=").unwrap_or_default();
            let url = std::env::var("RUNBOOK_AUTH_CAN_REGISTER_URL").unwrap();
            let (addr, path) = url.trim_start_matches("http://").split_once('/')?;
            let (addr, path) = (addr.to_owned(), format!("/{path}"));
            let payload = json!({"email": email}).to_string();
            let mut rust = TcpStream::connect(&addr).ok()?;
            write!(
                rust,
                "POST {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\nx-runbook-internal-secret: {secret}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            )
            .ok()?;
            let mut answer = String::new();
            rust.read_to_string(&mut answer).ok()?;
            let status: u16 = answer.split(' ').nth(1)?.parse().ok()?;
            let body = answer.split_once("\r\n\r\n")?.1.to_owned();
            respond(&mut stream, 200, &[json_type], &json!({"status":status,"body":body}).to_string());
        }
        (_, "/api/auth/redirect") => respond(
            &mut stream,
            302,
            &[("location", "https://accounts.example.test/o/oauth2".into())],
            "",
        ),
        (_, p) if p.starts_with("/api/auth") => respond(
            &mut stream,
            200,
            &[
                json_type,
                ("set-cookie", "a=1; Path=/; HttpOnly".into()),
                ("set-cookie", "b=2; Path=/; HttpOnly".into()),
            ],
            &json!({"method":request.method,"path":path,"query":query,"headers":headers,"body":body_text}).to_string(),
        ),
        _ => respond(&mut stream, 404, &[], ""),
    }
    Some(())
}
