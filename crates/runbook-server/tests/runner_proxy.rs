//! R0-5 on Postgres: the hardened proxy to the runner, against a local
//! mock runner that records what reaches it.
#[path = "support/auth_stub.rs"]
mod auth_stub;
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/runner_fixture.rs"]
mod runner_fixture;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use ed25519_dalek::{Signer, SigningKey};
use opentelemetry::{
    Context, global,
    propagation::TextMapPropagator,
    trace::{SpanKind, TraceContextExt, Tracer},
};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    propagation::TraceContextPropagator,
    trace::{Sampler, SdkTracerProvider, SpanData, SpanExporter},
};
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
    body: Vec<u8>,
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
                let data = if target.ends_with("/credentials/oauth_fixture/connect") {
                    oauth_start_reply(&headers, &body)
                } else if target == "/runner/v1/oauth/callback/oauth_fixture" {
                    json!({"connected":true,"test":{"ok":true}})
                } else {
                    json!({"ok":true})
                }
                .to_string();
                shared.seen.lock().unwrap().push(Seen {
                    target,
                    headers,
                    body,
                });
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
                    data.len()
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

async fn return_to_core(f: &Fixture, reg: &Registered, headers: &BTreeMap<String, String>) {
    let propagator = TraceContextPropagator::new();
    let mut inbound = http::HeaderMap::new();
    inbound.insert("traceparent", headers["traceparent"].parse().unwrap());
    let parent = propagator.extract_with_context(
        &Context::new(),
        &opentelemetry_http::HeaderExtractor(&inbound),
    );
    let tracer = global::tracer("runner-fixture");
    let server = parent.with_span(
        tracer
            .span_builder("runner.fixture.server")
            .with_kind(SpanKind::Server)
            .start_with_context(&tracer, &parent),
    );
    let client = server.with_span(
        tracer
            .span_builder("runner.fixture.client")
            .with_kind(SpanKind::Client)
            .start_with_context(&tracer, &server),
    );
    let mut carrier = std::collections::HashMap::new();
    propagator.inject_context(&client, &mut carrier);
    let response = f
        .request(
            "GET",
            &format!("/v1/runner-installs/{}/groups", reg.install),
            Some(&reg.directory),
            &[("traceparent", &carrier["traceparent"])],
            Value::Null,
        )
        .await;
    assert_eq!(response.status, 200);
    client.span().end();
    server.span().end();
}

#[derive(Clone, Debug, Default)]
struct TestSpans(Arc<Mutex<Vec<SpanData>>>);

impl SpanExporter for TestSpans {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        self.0.lock().unwrap().extend(batch);
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn sampled_trace_round_trips_through_native_runner_proxy() {
    let spans = TestSpans::default();
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)))
        .with_simple_exporter(spans.clone())
        .build();
    global::set_tracer_provider(provider);
    with_fixture_config(
        |config| config.runner.private_networks = true,
        async |f| {
            let runner = MockRunner::start();
            let reg = f.register_runner(&runner.url()).await;
            f.heartbeat(&reg).await;
            let trace_id = "391665d8aa4a13d6a8a5bea9ea3bebff";
            let browser_span = "9012233445566778";
            let traceparent = format!("00-{trace_id}-{browser_span}-01");
            let reply = f
                .request(
                    "GET",
                    &proxy(&reg, "runner/v1/groups"),
                    Some(&reg.admin),
                    &[
                        ("traceparent", &traceparent),
                        ("tracestate", "fixture=not-a-log-field"),
                        CSRF,
                    ],
                    Value::Null,
                )
                .await;
            assert_eq!(reply.status, 200, "{}", reply.body);
            let forwarded = runner.last();
            let forwarded_context = &forwarded.headers["traceparent"];
            let fields: Vec<_> = forwarded_context.split('-').collect();
            let core_trace = fields[1];
            assert_ne!(core_trace, trace_id);
            return_to_core(f, &reg, &forwarded.headers).await;
            let rejected = f
                .request(
                    "GET",
                    &format!("/v1/runner-installs/{}/groups", reg.install),
                    Some(&reg.claim),
                    &[("traceparent", &traceparent)],
                    Value::Null,
                )
                .await;
            assert_eq!(rejected.status, 403);
            {
                let spans = spans.0.lock().unwrap();
                assert!(
                    spans
                        .iter()
                        .all(|span| span.span_context.trace_id().to_string() != trace_id)
                );
                let root = spans
                    .iter()
                    .find(|span| {
                        span.name == "http.server"
                            && span.span_context.trace_id().to_string() == core_trace
                            && span.parent_span_id == opentelemetry::trace::SpanId::INVALID
                    })
                    .unwrap();
                let mut parent = root.span_context.span_id().to_string();
                for name in [
                    "runner.http.client",
                    "runner.fixture.server",
                    "runner.fixture.client",
                    "http.server",
                ] {
                    let span = spans
                        .iter()
                        .find(|span| span.parent_span_id.to_string() == parent && span.name == name)
                        .unwrap_or_else(|| panic!("missing span: {name}"));
                    assert_eq!(span.span_context.trace_id().to_string(), core_trace);
                    assert!(span.span_context.trace_state().header().is_empty());
                    parent = span.span_context.span_id().to_string();
                }
            }

            let dropped = TestSpans::default();
            let disabled_roots = SdkTracerProvider::builder()
                .with_sampler(Sampler::ParentBased(Box::new(Sampler::AlwaysOff)))
                .with_simple_exporter(dropped.clone())
                .build();
            global::set_tracer_provider(disabled_roots);
            let reply = f
                .request(
                    "GET",
                    "/v1/meta",
                    None,
                    &[("traceparent", &traceparent)],
                    Value::Null,
                )
                .await;
            assert_eq!(reply.status, 200);
            assert!(dropped.0.lock().unwrap().is_empty());
        },
    )
    .await;
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
fn oauth_start_reply(headers: &BTreeMap<String, String>, body: &[u8]) -> Value {
    let token = &headers["runbook-assertion"];
    let payload = B64.decode(token.split('.').nth(1).unwrap()).unwrap();
    let claims: UserAssertionClaims = serde_json::from_slice(&payload).unwrap();
    let url = format!("https://auth.example/authorize?state={OAUTH_STATE}");
    let nonce_hash = runbook_core::signing::body_hash(OAUTH_NONCE.as_bytes());
    let request: Value = serde_json::from_slice(body).unwrap();
    let expires = now_ms()
        + if request["binding_hash"] == "skewed" {
            780_000
        } else {
            660_000
        };
    let mut statement = Vec::new();
    for field in [
        "runbook-runner/oauth-start/v1",
        &claims.org_id,
        claims.aud.strip_prefix("runner:").unwrap(),
        "oauth_fixture",
        request["binding_hash"].as_str().unwrap(),
        &url,
        &nonce_hash,
        claims.session_hash.as_deref().unwrap(),
        &claims.jti,
        &expires.to_string(),
        "",
    ] {
        statement.extend((field.len() as u32).to_be_bytes());
        statement.extend(field.as_bytes());
    }
    let key = SigningKey::from_bytes(&[7; 32]);
    let signature = B64.encode(key.sign(&statement).to_bytes());
    let reply_url = if request["binding_hash"] == "tampered" {
        format!("{url}&unexpected=1")
    } else {
        url
    };
    json!({"authorization_url":reply_url,"nonce":OAUTH_NONCE,
        "identity_signature":signature,"expires_at_ms":expires,"request_id":claims.jti})
}

const OAUTH_STATE: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const OAUTH_NONCE: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
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

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn runner_oauth_callback_uses_the_initiating_browser_and_signs_a_fixed_post() {
    let files = auth_stub::Files::new("runner-oauth");
    for browser in ["fresh-browser", "other-browser"] {
        files.put_session(browser, "owner-auth", "owner@example.test");
    }
    files.update(|sessions| {
        sessions.get_mut("other-browser").unwrap()["mfa_enrolled"] = json!(true);
        let fresh = sessions.get_mut("fresh-browser").unwrap();
        fresh["mfa_verified_at"] = json!(now_ms());
        fresh["mfa_method"] = json!("totp");
        fresh["mfa_enrolled"] = json!(true);
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
            directory.commit(DirectoryCommit {
                org_id: f.org.clone(),
                expected_revision: directory.snapshot(&f.org).await.unwrap().revision,
                mutations: vec![DirectoryMutation::BindVerifiedEmail {
                    user_id: f.user.clone(), email: "owner@example.test".into(),
                }],
            }).await.unwrap();
            assert!(f.worker.client().auth_child().unwrap().wait_ready(Duration::from_secs(20)));
            let runner = MockRunner::start();
            let reg = f.register_runner(&runner.url()).await;
            f.heartbeat(&reg).await;
            let key = SigningKey::from_bytes(&[7; 32]);
            let identity = f.call("PUT", &format!("/v1/orgs/{}/runner/vault-identity", f.org),
                &f.admin_pat().await, json!({"public_key": B64.encode(key.verifying_key().to_bytes())})).await;
            assert_eq!(identity.status, 204, "{}", identity.body);
            let start = proxy(&reg, "runner/v1/credentials/oauth_fixture/connect");
            let origin = ("origin", "https://runbooks.example");
            let browser = "__Host-runbook.session_token=fresh-browser";
            let other = "__Host-runbook.session_token=other-browser";
            assert_eq!(f.request("POST", &start, None, &[CSRF, origin, ("cookie", other)], json!({"binding_hash":"fixture"}))
                .await.code(), "auth.mfa_step_up");
            let tampered = f.request("POST", &start, None, &[CSRF, origin, ("cookie", browser)],
                json!({"binding_hash":"tampered"})).await;
            assert_eq!(tampered.code(), "runner.bad_response");
            assert!(!tampered.headers.contains_key("set-cookie"));
            let skewed = f.request("POST", &start, None, &[CSRF, origin, ("cookie", browser)],
                json!({"binding_hash":"skewed"})).await;
            assert_eq!(skewed.code(), "runner.oauth_clock_skew");
            assert!(!skewed.headers.contains_key("set-cookie"));
            let launch = f.request("POST", &start, None, &[CSRF, origin, ("cookie", browser)], json!({"binding_hash":"fixture"})).await;
            assert_eq!(launch.status, 200, "{}", launch.body);
            assert!(launch.body.get("nonce").is_none());
            let nonce_cookie = launch.headers["set-cookie"].split(';').next().unwrap();
            assert_eq!(assertion(f, &runner.last()).await.fresh_mfa, Some(true));
            let code = "synthetic-native-callback-marker";
            let callback = format!("/v1/runner/oauth/callback/oauth_fixture?code={code}&state={OAUTH_STATE}&iss=https%3A%2F%2Fauth.example");
            for cookie in [browser.to_owned(), format!("{other}; {nonce_cookie}")] {
                let refusal = f.request("GET", &callback, None, &[("cookie", &cookie)], Value::Null).await;
                assert_eq!(refusal.status, 303);
                assert!(refusal.headers["location"].ends_with(".failed"));
            }
            assert_eq!(runner.count(), 3);
            let response = f.request("GET", &callback, None,
                &[("cookie", &format!("{browser}; {nonce_cookie}"))], Value::Null).await;
            assert_eq!(response.status, 303);
            assert_eq!(response.headers["cache-control"], "no-store");
            assert_eq!(response.headers["referrer-policy"], "no-referrer");
            assert!(response.headers["location"].ends_with(".connected"));
            assert!(response.headers["set-cookie"].contains("Max-Age=0"));
            let forwarded = runner.last();
            assert_eq!(forwarded.target, "/runner/v1/oauth/callback/oauth_fixture");
            assert!(!forwarded.headers.contains_key("cookie"));
            let body: Value = serde_json::from_slice(&forwarded.body).unwrap();
            assert_eq!(body, json!({"code":code,"state":OAUTH_STATE,"iss":"https://auth.example","nonce":OAUTH_NONCE}));
            let claims = assertion(f, &forwarded).await;
            assert_eq!(claims.htm, "POST");
            assert_eq!(claims.htu, forwarded.target);
            assert_eq!(claims.bh, runbook_core::signing::body_hash(&forwarded.body));
            assert_eq!(claims.session_hash.as_deref(), Some(runbook_core::signing::body_hash(b"sess_fresh-browser").as_str()));
            let confirm = proxy(&reg, "runner/v1/credentials/oauth_fixture/oauth-confirm");
            let confirmation = json!({"binding_hash":"fixture","confirm_account":OAUTH_STATE});
            let from_pat = f.request("POST", &confirm, Some(&f.admin_pat().await), &[CSRF, origin], confirmation.clone()).await;
            assert_eq!(from_pat.status, 403);
            let from_browser = f.request("POST", &confirm, None, &[CSRF, origin, ("cookie", browser)], confirmation).await;
            assert_eq!(from_browser.status, 200, "{}", from_browser.body);
            assert_eq!(assertion(f, &runner.last()).await.session_hash, claims.session_hash);
            assert!(!files.log().into_iter().any(|entry| entry.to_string().contains(code)));
        },
    ).await;
}
