//! R0-5 on Postgres: the hardened proxy to the runner, against a local
//! mock runner that records what reaches it.
#[path = "support/auth_stub.rs"]
mod auth_stub;
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/runner_fixture.rs"]
mod runner_fixture;
use runbook_core::{
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore},
    signing::{Jwks, USER_TYP, UserAssertionClaims, verify},
};
use runbook_server::config::AuthConfig;
use runbook_store_postgres::PostgresDirectory;
use runner_fixture::{Fixture, Registered, now_ms, with_fixture_config};
use serde_json::{Value, json};

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
struct Seen {
    target: String,
    headers: BTreeMap<String, String>,
}
#[derive(Clone)]
struct MockRunner {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}
impl MockRunner {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let runner = Self {
            port: listener.local_addr().unwrap().port(),
            seen: Arc::default(),
        };
        let shared = runner.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                    if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(i);
                    }
                };
                let Some(end) = end else { continue };
                let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                let mut lines = head.lines();
                let request = lines.next().unwrap_or_default().to_owned();
                let mut parts = request.split(' ');
                parts.next();
                let target = parts.next().unwrap_or_default().to_owned();
                let headers: BTreeMap<String, String> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let length: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = buffer[end + 4..].to_vec();
                while body.len() < length {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                    }
                }
                shared.seen.lock().unwrap().push(Seen { target, headers });
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}",
                );
            }
        });
        runner
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn last(&self) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a request reached the runner")
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

async fn with_runner(f: impl AsyncFnOnce(&Fixture, &MockRunner, &Registered)) {
    with_fixture_config(
        |config| config.runner.private_networks = true,
        async |fixture| {
            let runner = MockRunner::start();
            let reg = fixture.register_runner(&runner.url()).await;
            fixture.heartbeat(&reg).await;
            f(fixture, &runner, &reg).await;
        },
    )
    .await;
}
fn proxy(reg: &Registered, path: &str) -> String {
    format!("/v1/runners/{}/proxy/{path}", reg.install)
}
const CSRF: (&str, &str) = ("x-runbook-csrf", "1");

async fn assertion(f: &Fixture, seen: &Seen) -> UserAssertionClaims {
    let jwks: Jwks = serde_json::from_value(
        f.request("GET", "/.well-known/jwks.json", None, &[], Value::Null)
            .await
            .body,
    )
    .unwrap();
    verify(
        &jwks,
        &seen.headers["runbook-assertion"],
        USER_TYP,
        now_ms() / 1000,
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn path_outside_allow_list_is_404() {
    with_runner(async |f, runner, reg| {
        for path in [
            "runner/v1/secrets",
            "internal/seal",
            "runner/v1/groups/../credentials",
            "runner/v1/policy/extra",
            "runner/v1/groups/%2e%2e",
            "runner/v1/groups/.",
            "runner/v1/groups/%2F",
        ] {
            let reply = f
                .request(
                    "GET",
                    &proxy(reg, path),
                    Some(&reg.admin),
                    &[CSRF],
                    Value::Null,
                )
                .await;
            assert_eq!(reply.status, 404, "{path}: {}", reply.body);
        }
        // A known path with a method it doesn't have.
        let reply = f
            .request(
                "DELETE",
                &proxy(reg, "runner/v1/policy"),
                Some(&reg.admin),
                &[CSRF],
                Value::Null,
            )
            .await;
        assert_eq!(reply.status, 404);
        assert_eq!(runner.count(), 0, "nothing reached the runner");
        let ok = f
            .request(
                "GET",
                &proxy(reg, "runner/v1/groups"),
                Some(&reg.admin),
                &[CSRF],
                Value::Null,
            )
            .await;
        assert_eq!(ok.status, 200, "{}", ok.body);
        assert_eq!(ok.body, json!({"ok": true}));
        assert_eq!(runner.last().target, "/runner/v1/groups");
        // Another install's ID is not found.
        let other = f
            .request(
                "GET",
                "/v1/runners/ri_00000000000000000000000001/proxy/runner/v1/groups",
                Some(&reg.admin),
                &[CSRF],
                Value::Null,
            )
            .await;
        assert_eq!(other.status, 404);
    })
    .await;
}

#[test]
fn auth_stub_child() {
    if std::env::var_os(auth_stub::ENV).is_some() {
        auth_stub::run();
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn hosted_mcp_proxies_runner_reads_with_the_oauth_agent_scope() {
    let files = auth_stub::Files::new("mcp-runner");
    files.put_session("browser", "owner-auth", "owner@example.test");
    let grants = [
        ("trb_oat_runner_read", "runner:read", None),
        (
            "trb_oat_runbooks_read",
            "runbooks:read",
            Some("auth.scope_missing"),
        ),
    ];
    files.update(|sessions| {
        for (token, scope, _) in grants {
            let mut grant = sessions["browser"].clone();
            grant["session_id"] = json!(token);
            grant["scope"] = json!(scope);
            grant["resource"] = json!("https://runbooks.example/mcp");
            sessions.insert(token.into(), grant);
        }
    });
    let mut auth = AuthConfig::new(auth_stub::command("auth_stub_child"));
    auth.child_env = files.env();
    auth.methods = vec!["email".into()];
    with_fixture_config(
        |config| {
            config.runner.private_networks = true;
            config.auth = Some(auth);
        },
        async |f| {
            let directory = PostgresDirectory::new(f.config.database.clone());
            directory
                .commit(DirectoryCommit {
                    org_id: f.org.clone(),
                    expected_revision: directory.snapshot(&f.org).await.unwrap().revision,
                    mutations: vec![DirectoryMutation::BindVerifiedEmail {
                        user_id: f.user.clone(),
                        email: "owner@example.test".into(),
                    }],
                })
                .await
                .unwrap();
            let client = f.worker.client();
            let child = client.auth_child().unwrap();
            assert!(child.wait_ready(Duration::from_secs(20)));
            let runner = MockRunner::start();
            let reg = f.register_runner(&runner.url()).await;
            f.heartbeat(&reg).await;

            for (token, _, expected_error) in grants {
                let reply = f
                    .request(
                        "POST",
                        "/mcp",
                        Some(token),
                        &[("accept", "application/json, text/event-stream")],
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                            "name":"list_runner_agents",
                            "arguments":{"install_id":reg.install,"project_id":f.project}
                        }}),
                    )
                    .await;
                assert_eq!(reply.status, 200, "{}", reply.body);
                let result = &reply.body["result"];
                let content = result["content"][0]["text"].as_str().unwrap();
                if let Some(code) = expected_error {
                    assert_eq!(result["isError"], true, "{result}");
                    let control: Value =
                        serde_json::from_str(content.lines().next().unwrap()).unwrap();
                    assert_eq!(control["error"]["code"], code);
                } else {
                    assert_eq!(result["isError"], false, "{result}");
                    assert!(
                        content.lines().any(|line| line == r#"{"ok":true}"#),
                        "{result}"
                    );
                    let claims = assertion(f, &runner.last()).await;
                    assert_eq!(claims.principal, "agent");
                    assert_eq!(claims.sponsor_id.as_deref(), Some(f.user.as_str()));
                    assert_eq!(claims.scopes, ["runner:read"]);
                }
            }
            let browser = f
                .request(
                    "GET",
                    &proxy(&reg, "runner/v1/groups"),
                    None,
                    &[("cookie", "__Host-runbook.session_token=browser")],
                    Value::Null,
                )
                .await;
            assert_eq!(browser.status, 403, "{}", browser.body);
            assert_eq!(browser.code(), "auth.csrf");
            assert_eq!(runner.count(), 1);
        },
    )
    .await;
}
