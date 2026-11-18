//! HOST1 end to end: two `runbook-server serve` replicas (the real binary)
//! on one Postgres database, behind a local round-robin proxy, in
//! single-port mode with JSON logs and a small pool. They migrate an empty
//! database at the same time, then a run with a timer completes and its
//! webhook arrives once; replicas frozen past a timer's due time (Cloud
//! Run's CPU throttling between requests) fire it late but exactly once;
//! SIGTERM stops a replica within its timeout while the other finishes the
//! work.
//!
//! Needs `DATABASE_URL` (an owned `rb_test_` database is created and
//! dropped).
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
#[allow(dead_code)]
mod database;
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    panic::AssertUnwindSafe,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const FILES_HOST: &str = "files.example.test";

fn database_url(name: &str) -> String {
    let base = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let (base, query) = base.split_once('?').unwrap_or((&base, ""));
    let (server, _) = base
        .rsplit_once('/')
        .expect("DATABASE_URL names a database");
    if query.is_empty() {
        format!("{server}/{name}")
    } else {
        format!("{server}/{name}?{query}")
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn server_command(env: &[(&str, String)], args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook-server"));
    command.env_clear().args(args);
    for key in ["PATH", "HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for (k, v) in env {
        command.env(k, v);
    }
    command
}

fn signal(pid: u32, name: &str) {
    let status = Command::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{name}");
}

/// One replica: its own process group, stdout captured line by line.
struct Replica {
    child: Child,
    port: u16,
    lines: Arc<Mutex<Vec<String>>>,
}
impl Replica {
    fn spawn(env: &[(&str, String)], port: u16) -> Self {
        use std::os::unix::process::CommandExt;
        let mut env = env.to_vec();
        env.push(("RUNBOOK_LISTEN_ADDR", format!("127.0.0.1:{port}")));
        let mut child = server_command(&env, &["serve"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let stdout = child.stdout.take().unwrap();
        let sink = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                println!("replica {port}: {line}");
                sink.lock().unwrap().push(line);
            }
        });
        Self { child, port, lines }
    }
    fn pid(&self) -> u32 {
        self.child.id()
    }
    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok((200, _)) = raw_get(self.port, "127.0.0.1", "/health/ready") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "replica {} never ready",
                self.port
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// SIGTERM; the exit status and how long the exit took.
    fn terminate(&mut self) -> (Option<i32>, Duration) {
        let started = Instant::now();
        signal(self.pid(), "TERM");
        let deadline = started + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return (status.code(), started.elapsed());
            }
            assert!(
                Instant::now() < deadline,
                "replica {} did not exit",
                self.port
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}
impl Drop for Replica {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.child.id())])
            .stderr(Stdio::null())
            .status();
        let _ = self.child.wait();
    }
}

/// A TCP proxy that sends each new connection to the next backend in turn,
/// skipping one that refuses (as a load balancer's health check would).
struct Proxy {
    port: u16,
    served: Arc<Vec<AtomicUsize>>,
}
impl Proxy {
    fn start(backends: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served: Arc<Vec<AtomicUsize>> =
            Arc::new(backends.iter().map(|_| AtomicUsize::new(0)).collect());
        let counts = served.clone();
        std::thread::spawn(move || {
            for (next, client) in listener.incoming().flatten().enumerate() {
                let upstream = (0..backends.len()).find_map(|i| {
                    let at = (next + i) % backends.len();
                    TcpStream::connect(("127.0.0.1", backends[at]))
                        .ok()
                        .map(|s| (at, s))
                });
                let Some((at, upstream)) = upstream else {
                    continue;
                };
                counts[at].fetch_add(1, Ordering::SeqCst);
                let (mut c_read, mut u_write) =
                    (client.try_clone().unwrap(), upstream.try_clone().unwrap());
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut c_read, &mut u_write);
                    let _ = u_write.shutdown(std::net::Shutdown::Write);
                });
                let (mut u_read, mut c_write) = (upstream, client);
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut u_read, &mut c_write);
                    let _ = c_write.shutdown(std::net::Shutdown::Write);
                });
            }
        });
        Self { port, served }
    }
    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn served(&self) -> Vec<usize> {
        self.served
            .iter()
            .map(|n| n.load(Ordering::SeqCst))
            .collect()
    }
}

/// Headers (and `:start`, the start line) and a JSON body.
type Message = (BTreeMap<String, String>, Value);

/// A webhook receiver that records every POST and answers 200.
#[derive(Clone)]
struct Receiver {
    port: u16,
    seen: Arc<Mutex<Vec<Message>>>,
}
impl Receiver {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let receiver = Self {
            port: listener.local_addr().unwrap().port(),
            seen: Arc::default(),
        };
        let seen = receiver.seen.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let Some((headers, body)) = read_message(&mut stream) else {
                    continue;
                };
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                );
                let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
                seen.lock().unwrap().push((headers, body));
            }
        });
        receiver
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }
    /// Distinct event IDs of `kind` for `exe`, and the delivery count.
    fn events(&self, kind: &str, exe: &str) -> (usize, usize) {
        let seen = self.seen.lock().unwrap();
        let matching: Vec<&Value> = seen
            .iter()
            .map(|(_, b)| b)
            .filter(|b| b["type"] == kind && b["execution"]["id"] == exe)
            .collect();
        let mut ids: Vec<&str> = matching.iter().filter_map(|b| b["id"].as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        (ids.len(), matching.len())
    }
}

/// Read one HTTP/1.1 message (headers and a Content-Length body).
fn read_message(stream: &mut TcpStream) -> Option<(BTreeMap<String, String>, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
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
    let mut headers = headers;
    headers.insert(
        ":start".into(),
        head.lines().next().unwrap_or("").to_owned(),
    );
    Some((headers, body))
}

/// A GET with an explicit `Host`, on a fresh connection; status and the
/// response's headers.
fn raw_get(port: u16, host: &str, path: &str) -> std::io::Result<(u16, BTreeMap<String, String>)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n"
    )?;
    let (headers, _) = read_message(&mut stream).ok_or(std::io::ErrorKind::UnexpectedEof)?;
    let status = headers[":start"]
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((status, headers))
}

struct Api {
    origin: String,
    token: String,
}
struct Reply {
    status: u16,
    etag: Option<String>,
    body: Value,
}
impl Api {
    /// One call on a fresh connection, so the proxy spreads calls across
    /// replicas.
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        let agent = ureq::AgentBuilder::new().max_idle_connections(0).build();
        let mut request = agent
            .request(method, &format!("{}{path}", self.origin))
            .timeout(Duration::from_secs(20))
            .set("authorization", &format!("Bearer {}", self.token));
        for (k, v) in headers {
            request = request.set(k, v);
        }
        let response = match match body {
            Some(body) => request
                .set("content-type", "application/json")
                .send_string(&body.to_string()),
            None => request.call(),
        } {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(e) => panic!("{method} {path}: {e}"),
        };
        let status = response.status();
        let etag = response.header("etag").map(str::to_owned);
        let text = response.into_string().unwrap_or_default();
        Reply {
            status,
            etag,
            body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        }
    }
    /// Create, publish and start a one-step run that waits `wait`.
    fn start_wait_run(&self, project: &str, slug: &str, wait: &str) -> String {
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {slug}, title: Host {slug}}}\nspec:\n  steps:\n    - {{id: pause, wait: {{for: {wait}}}}}\n"
        );
        let created = self.call(
            "POST",
            &format!("/v1/projects/{project}/runbooks"),
            Some(json!({ "source": source })),
            &[("idempotency-key", &format!("create-{slug}"))],
        );
        assert_eq!(created.status, 201, "{}", created.body);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = self.call("GET", &format!("/v1/runbooks/{rb}/draft"), None, &[]);
        assert_eq!(draft.status, 200, "{}", draft.body);
        let etag = draft.etag.unwrap();
        let published = self.call(
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            Some(json!({})),
            &[
                ("idempotency-key", &format!("publish-{slug}")),
                ("if-match", &etag),
            ],
        );
        assert_eq!(published.status, 201, "{}", published.body);
        let started = self.call(
            "POST",
            &format!("/v1/projects/{project}/executions"),
            Some(json!({ "runbook": rb })),
            &[("idempotency-key", &format!("start-{slug}"))],
        );
        assert_eq!(started.status, 201, "{}", started.body);
        started.body["id"].as_str().unwrap().to_owned()
    }
    fn status(&self, exe: &str) -> String {
        let r = self.call("GET", &format!("/v1/executions/{exe}"), None, &[]);
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["status"].as_str().unwrap_or("").to_owned()
    }
    fn wait_status(&self, exe: &str, want: &str, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            let status = self.status(exe);
            if status == want {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{exe} is {status}, expected {want}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

async fn timer_fired(db: &database::TestDatabase, exe: &str) -> i64 {
    let store = runbook_store_postgres::PostgresStore::connect(db.config())
        .await
        .unwrap();
    store
        .client()
        .query_one(
            "SELECT count(*) FROM runbook.execution_event \
             WHERE execution_id = $1 AND body::jsonb #>> '{event,type}' = 'TimerFired'",
            &[&exe],
        )
        .await
        .unwrap()
        .get(0)
}

#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn two_replicas_behind_a_proxy_share_one_database() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut db = database::TestDatabase::create_from_env().await.unwrap();
        let name = db.name().to_owned();
        let result = AssertUnwindSafe(scenario(&db, &name)).catch_unwind().await;
        db.finish(result).await;
    });
}

async fn scenario(db: &database::TestDatabase, name: &str) {
    use base64::Engine;
    let receiver = Receiver::start();
    let (port_a, port_b) = (free_port(), free_port());
    let proxy = Proxy::start(vec![port_a, port_b]);
    let key = |byte: u8| base64::engine::general_purpose::STANDARD.encode([byte; 32]);
    let env = vec![
        ("DATABASE_URL", database_url(name)),
        ("RUNBOOK_ORIGIN", proxy.origin()),
        // Single-port mode: user content on the same port, by host.
        (
            "RUNBOOK_USERCONTENT_ORIGIN",
            format!("http://{FILES_HOST}:{}", proxy.port),
        ),
        ("RUNBOOK_UPLOAD_SIGNING_KEY", key(5)),
        ("RUNBOOK_WEBHOOK_SECRET_KEY", key(6)),
        ("RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS", "true".into()),
        // A small managed tier: a few connections per replica.
        ("RUNBOOK_DATABASE_POOL_SIZE", "3".into()),
        ("RUNBOOK_LOG_FORMAT", "json".into()),
        ("RUNBOOK_SHUTDOWN_TIMEOUT_SECS", "5".into()),
    ];

    // Both start on the empty database at once: one applies the
    // migrations, the other waits on the advisory lock, then both serve.
    let mut a = Replica::spawn(&env, port_a);
    let mut b = Replica::spawn(&env, port_b);
    a.wait_ready();
    b.wait_ready();

    let out = server_command(&env, &["bootstrap"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let issued: Value = serde_json::from_slice(&out.stdout).unwrap();
    let project = issued["project_id"].as_str().unwrap().to_owned();
    let api = Api {
        origin: proxy.origin(),
        token: issued["pat"].as_str().unwrap().to_owned(),
    };

    // One port, two origins: the user-content host answers only `/u/*`.
    let (status, headers) = raw_get(
        proxy.port,
        &format!("{FILES_HOST}:{}", proxy.port),
        "/v1/meta",
    )
    .unwrap();
    assert_eq!(status, 404);
    assert!(headers["content-security-policy"].contains("sandbox"));
    let (status, headers) =
        raw_get(proxy.port, &format!("127.0.0.1:{}", proxy.port), "/v1/meta").unwrap();
    assert_eq!(status, 200);
    assert!(!headers.contains_key("content-security-policy"));

    // A run with a timer, and its webhooks.
    let hook = api.call(
        "POST",
        &format!("/v1/projects/{project}/webhooks"),
        Some(json!({"name": "host1", "url": receiver.url(),
                    "events": ["execution.started", "execution.completed"]})),
        &[("idempotency-key", "hook")],
    );
    assert_eq!(hook.status, 201, "{}", hook.body);
    let exe = api.start_wait_run(&project, "first", "2s");
    api.wait_status(&exe, "succeeded", Duration::from_secs(30));
    assert_eq!(timer_fired(db, &exe).await, 1, "the timer fired once");
    let deadline = Instant::now() + Duration::from_secs(20);
    while receiver.events("execution.completed", &exe).0 == 0 {
        assert!(Instant::now() < deadline, "no execution.completed webhook");
        std::thread::sleep(Duration::from_millis(100));
    }
    // Give a duplicate a moment to show up, then count.
    std::thread::sleep(Duration::from_secs(2));
    let (ids, deliveries) = receiver.events("execution.completed", &exe);
    assert_eq!((ids, deliveries), (1, 1), "one event, delivered once");
    assert_eq!(receiver.events("execution.started", &exe), (1, 1));
    assert!(
        receiver
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|(h, _)| h.contains_key("runbook-signature"))
    );
    let served = proxy.served();
    assert!(
        served.iter().all(|n| *n > 0),
        "both replicas served: {served:?}"
    );

    // Cloud Run without "CPU always allocated": both replicas get no CPU
    // past the timer's due time. Nothing is lost; the timer fires late, once.
    let exe = api.start_wait_run(&project, "throttled", "1s");
    signal(a.pid(), "STOP");
    signal(b.pid(), "STOP");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(timer_fired(db, &exe).await, 0, "nobody had CPU");
    signal(a.pid(), "CONT");
    signal(b.pid(), "CONT");
    api.wait_status(&exe, "succeeded", Duration::from_secs(30));
    assert_eq!(timer_fired(db, &exe).await, 1);

    // SIGTERM one replica: it exits 0 within its timeout, and the other
    // fires the timer of a run started just before.
    let exe = api.start_wait_run(&project, "handover", "2s");
    let (code, took) = a.terminate();
    assert_eq!(code, Some(0));
    assert!(took < Duration::from_secs(5), "took {took:?}");
    api.wait_status(&exe, "succeeded", Duration::from_secs(30));
    assert_eq!(timer_fired(db, &exe).await, 1);
    let (code, took) = b.terminate();
    assert_eq!(code, Some(0));
    assert!(took < Duration::from_secs(5), "took {took:?}");

    // Structured logs: every stdout line is a JSON object with a severity.
    for replica in [&a, &b] {
        let lines = replica.lines();
        assert!(
            lines.iter().any(|l| l.contains("listening on")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("stopped in")), "{lines:?}");
        for line in lines {
            let v: Value =
                serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON: {line}"));
            assert!(
                v["severity"].is_string() && v["message"].is_string(),
                "{line}"
            );
        }
    }
}

/// SIGTERM reaches the auth child through the server: the child is asked to
/// stop (SIGTERM), not killed, and the server exits 0 within its timeout.
#[test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
fn sigterm_stops_the_server_and_its_auth_child_gracefully() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut db = database::TestDatabase::create_from_env().await.unwrap();
        let name = db.name().to_owned();
        let result = AssertUnwindSafe(async {
            let dir = std::env::temp_dir().join(format!("rb-host1-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let marker = dir.join("child-got-term");
            let _ = std::fs::remove_file(&marker);
            // A stand-in auth child: ready line, then wait; on SIGTERM it
            // leaves a marker and exits 0.
            let script = format!(
                "trap 'touch {m}; exit 0' TERM; printf '{{\"event\":\"ready\",\"port\":4999,\"methods\":[]}}\\n'; while true; do sleep 0.1; done",
                m = marker.display()
            );
            std::fs::write(dir.join("child.sh"), &script).unwrap();
            let port = free_port();
            let env = vec![
                ("DATABASE_URL", database_url(&name)),
                ("RUNBOOK_ORIGIN", format!("http://127.0.0.1:{port}")),
                ("RUNBOOK_AUTH_SECRET", "test-auth-secret-which-is-at-least-32-bytes-long".into()),
                ("RUNBOOK_AUTH_COMMAND", format!("sh {}", dir.join("child.sh").display())),
                ("RUNBOOK_SHUTDOWN_TIMEOUT_SECS", "4".into()),
            ];
            let mut server = Replica::spawn(&env, port);
            server.wait_ready();
            assert_eq!(raw_get(port, "x", "/health/live").unwrap().0, 200);
            // Wait until the supervisor has seen the child's ready line.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !server.lines().iter().any(|l| l.contains("auth service ready")) {
                assert!(Instant::now() < deadline, "child never ready");
                std::thread::sleep(Duration::from_millis(50));
            }
            let (code, took) = server.terminate();
            assert_eq!(code, Some(0));
            assert!(took < Duration::from_secs(4), "took {took:?}");
            assert!(marker.exists(), "the auth child got SIGTERM");
            assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
            let _ = std::fs::remove_dir_all(dir);
        })
        .catch_unwind()
        .await;
        db.finish(result).await;
    });
}
