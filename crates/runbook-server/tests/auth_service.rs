//! AUTH5: the real auth service (`auth/dist/server.mjs`, AUTH1) spawned by
//! the real `runbook-server serve` binary, end to end over loopback HTTP:
//! request a code, read it from the `capture` transport, sign in through the
//! `/api/auth` proxy, and use the session cookie on `/v1/me`.
//!
//! Needs `DATABASE_URL` (an owned `rb_test_` database is created and dropped)
//! and a built auth service: `cd auth && npm ci --ignore-scripts && npm run
//! build`. Runs it with Bun when `bun` is on PATH, otherwise Node;
//! `RUNBOOK_TEST_AUTH_RUNTIME` overrides.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
#[allow(dead_code)]
mod database;
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const AUTH_SECRET: &str = "test-auth-secret-which-is-at-least-32-bytes-long";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `bun` when installed, else `node`; both run the same bundle.
fn auth_command() -> String {
    let bundle = repo_root().join("auth/dist/server.mjs");
    assert!(
        bundle.is_file(),
        "build the auth service first: cd auth && npm ci --ignore-scripts && npm run build"
    );
    let runtime = std::env::var("RUNBOOK_TEST_AUTH_RUNTIME").unwrap_or_else(|_| {
        let bun = Command::new("bun")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if bun { "bun" } else { "node" }.to_owned()
    });
    format!("{runtime} {}", bundle.canonicalize().unwrap().display())
}

/// The owned database's URL: `DATABASE_URL` with its database name replaced.
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
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A scratch directory under the target dir, unique per call.
fn scratch(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("auth-service-{}-{tag}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `runbook-server <args>` with a clean environment: only what is passed.
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

/// `runbook-server serve` in its own process group, so dropping it stops the
/// server and the auth child it supervises together.
struct Serve {
    child: Child,
    origin: String,
    capture: PathBuf,
}
impl Drop for Serve {
    fn drop(&mut self) {
        let group = format!("-{}", self.child.id());
        let kill = |signal: &str| {
            let _ = Command::new("kill")
                .args([signal, "--", &group])
                .stderr(Stdio::null())
                .status();
        };
        kill("-TERM");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        kill("-KILL");
        let _ = self.child.wait();
    }
}

fn base_env(database: &str, origin: &str, listen: &str) -> Vec<(&'static str, String)> {
    vec![
        ("DATABASE_URL", database_url(database)),
        ("RUNBOOK_ORIGIN", origin.to_owned()),
        ("RUNBOOK_LISTEN_ADDR", listen.to_owned()),
    ]
}

/// `methods` is RUNBOOK_AUTH_METHODS; empty leaves the child to enable each
/// configured method itself.
fn serve(database: &str, methods: &str) -> Serve {
    use std::os::unix::process::CommandExt;
    let port = free_port();
    let origin = format!("http://127.0.0.1:{port}");
    let dir = scratch("serve");
    let capture = dir.join("codes.jsonl");
    let mut env = base_env(database, &origin, &format!("127.0.0.1:{port}"));
    env.extend([
        ("RUNBOOK_AUTH_SECRET", AUTH_SECRET.to_owned()),
        ("RUNBOOK_AUTH_METHODS", methods.to_owned()),
        ("RUNBOOK_AUTH_ALLOWED_DOMAINS", "example.test".to_owned()),
        ("RUNBOOK_AUTH_EMAIL_TRANSPORT", "capture".to_owned()),
        ("RUNBOOK_AUTH_CAPTURE_FILE", capture.display().to_string()),
        ("RUNBOOK_AUTH_COMMAND", auth_command()),
    ]);
    let mut command = server_command(&env, &["serve"]);
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command.spawn().unwrap();
    // The supervisor reports the child's ready line on stdout; echo it all.
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            println!("serve: {line}");
        }
    });
    let serve = Serve {
        child,
        origin,
        capture,
    };
    // Up when the API answers and the auth child serves `/api/auth/ok`.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(r) = ureq::get(&format!("{}/api/auth/ok", serve.origin)).call()
            && r.status() == 200
        {
            return serve;
        }
        assert!(
            Instant::now() < deadline,
            "serve and its auth child did not come up"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Reply {
    status: u16,
    cookies: Vec<String>,
    body: Value,
}
fn call(method: &str, url: &str, headers: &[(&str, &str)], body: Option<Value>) -> Reply {
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    let mut request = agent.request(method, url).timeout(Duration::from_secs(20));
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
        Err(e) => panic!("{method} {url}: {e}"),
    };
    let status = response.status();
    let cookies = response
        .all("set-cookie")
        .into_iter()
        .map(str::to_owned)
        .collect();
    let text = response.into_string().unwrap_or_default();
    Reply {
        status,
        cookies,
        body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
    }
}

/// The newest code the capture transport wrote for `email`.
fn captured_code(capture: &Path, email: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let code = std::fs::read_to_string(capture)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["to"] == email && v["type"] == "sign-in")
            .filter_map(|v| v["code"].as_str().map(str::to_owned))
            .next_back();
        if let Some(code) = code {
            return code;
        }
        assert!(Instant::now() < deadline, "no code captured for {email}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Email code sign-in through the proxy; returns the `Cookie` header value.
fn sign_in(serve: &Serve, email: &str) -> String {
    let origin = serve.origin.as_str();
    let r = call(
        "POST",
        &format!("{origin}/api/auth/email-otp/send-verification-otp"),
        &[("origin", origin)],
        Some(json!({"email": email, "type": "sign-in"})),
    );
    assert_eq!(r.status, 200, "send code: {}", r.body);
    let code = captured_code(&serve.capture, email);
    let r = call(
        "POST",
        &format!("{origin}/api/auth/sign-in/email-otp"),
        &[("origin", origin)],
        Some(json!({"email": email, "otp": code})),
    );
    assert_eq!(r.status, 200, "sign in: {}", r.body);
    let session = r
        .cookies
        .iter()
        .find(|c| c.starts_with("runbook.session_token="))
        .unwrap_or_else(|| panic!("no session cookie in {:?}", r.cookies));
    assert!(session.contains("HttpOnly") && session.contains("SameSite=Lax"));
    session.split(';').next().unwrap().to_owned()
}

fn with_database(f: impl AsyncFnOnce(&str)) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut db = database::TestDatabase::create_from_env().await.unwrap();
        let name = db.name().to_owned();
        let result = AssertUnwindSafe(f(&name)).catch_unwind().await;
        db.finish(result).await;
    });
}

/// D43–D46 end to end: the child gets the contract's variables from the
/// supervisor, signs an invited address in, and Rust resolves the cookie
/// through `/internal/session` to a session principal; a cookie write
/// without `X-Runbook-CSRF` is refused.
#[test]
#[ignore = "requires DATABASE_URL and a built auth/ (npm run build); creates and drops only test-owned databases"]
fn an_invited_email_signs_in_through_the_real_auth_service() {
    with_database(async |database| {
        // Headless bootstrap (a PAT) so the owner can invite someone.
        let port = free_port();
        let env = base_env(database, "http://127.0.0.1:1", &format!("127.0.0.1:{port}"));
        let out = server_command(&env, &["bootstrap"]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let issued: Value = serde_json::from_slice(&out.stdout).unwrap();
        let pat = issued["pat"].as_str().unwrap().to_owned();
        let org = issued["org_id"].as_str().unwrap().to_owned();

        let serve = serve(database, "email");
        let origin = serve.origin.clone();
        let bearer = format!("Bearer {pat}");
        let r = call(
            "POST",
            &format!("{origin}/v1/orgs/{org}/invitations"),
            &[("authorization", &bearer), ("idempotency-key", "invite-1")],
            Some(json!({"email": "invitee@example.test", "role": "member",
                        "projects": [{"project": "default", "role": "operator"}]})),
        );
        assert_eq!(r.status, 201, "{}", r.body);

        let cookie = sign_in(&serve, "invitee@example.test");
        let me = call(
            "GET",
            &format!("{origin}/v1/me"),
            &[("cookie", &cookie)],
            None,
        );
        assert_eq!(me.status, 200, "{}", me.body);
        assert_eq!(me.body["credential"]["kind"], "session", "{}", me.body);

        // D46: a cookie write with the right Origin but no X-Runbook-CSRF.
        let r = call(
            "POST",
            &format!("{origin}/v1/orgs/{org}/teams"),
            &[
                ("cookie", &cookie),
                ("origin", &origin),
                ("idempotency-key", "t1"),
            ],
            Some(json!({"slug": "oncall", "title": "On call"})),
        );
        assert_eq!((r.status, &r.body["code"]), (403, &json!("auth.csrf")));

        // An address with no account or invitation is refused after the code.
        let r = call(
            "POST",
            &format!("{origin}/api/auth/email-otp/send-verification-otp"),
            &[("origin", &origin)],
            Some(json!({"email": "stranger@example.test", "type": "sign-in"})),
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let code = captured_code(&serve.capture, "stranger@example.test");
        let r = call(
            "POST",
            &format!("{origin}/api/auth/sign-in/email-otp"),
            &[("origin", &origin)],
            Some(json!({"email": "stranger@example.test", "otp": code})),
        );
        assert_eq!(
            (r.status, &r.body["code"]),
            (403, &json!("auth.registration_refused")),
            "{}",
            r.body
        );
        drop(serve);
    });
}

/// AUTH5: `bootstrap --owner-email` then the owner's first sign-in maps to
/// that owner: same user ID, owner role, a session credential, no token.
#[test]
#[ignore = "requires DATABASE_URL and a built auth/ (npm run build); creates and drops only test-owned databases"]
fn the_owner_bootstrapped_by_email_signs_in_as_the_owner() {
    with_database(async |database| {
        let env = base_env(database, "http://127.0.0.1:1", "127.0.0.1:1");
        let out = server_command(
            &env,
            &[
                "bootstrap",
                "--owner-email",
                "owner@example.test",
                "--owner-name",
                "Owner",
            ],
        )
        .output()
        .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let printed = String::from_utf8(out.stdout).unwrap();
        assert!(!printed.contains("trb_"), "no token without --with-token");
        let issued: Value = serde_json::from_str(&printed).unwrap();
        let user = issued["user_id"].as_str().unwrap().to_owned();

        // Compose passes RUNBOOK_AUTH_METHODS empty when unset: the child
        // enables email (capture is configured) and discovery says so.
        let serve = serve(database, "");
        let discovery = call(
            "GET",
            &format!("{}/.well-known/runbook", serve.origin),
            &[],
            None,
        );
        assert_eq!(
            discovery.body["auth"]["methods"],
            json!(["email"]),
            "{}",
            discovery.body
        );
        let cookie = sign_in(&serve, "owner@example.test");
        let me = call(
            "GET",
            &format!("{}/v1/me", serve.origin),
            &[("cookie", &cookie)],
            None,
        );
        assert_eq!(me.status, 200, "{}", me.body);
        assert_eq!(me.body["credential"]["kind"], "session", "{}", me.body);
        assert_eq!(me.body["user"]["id"], json!(user), "{}", me.body);
        assert_eq!(me.body["org"]["role"], "owner", "{}", me.body);
        // The memory routes accept the browser session like every other
        // route. They were bearer-only and answered a signed-in browser 401,
        // which the web app takes as a lost session and signs out.
        let project = issued["project_id"].as_str().unwrap();
        let memory = call(
            "GET",
            &format!("{}/v1/projects/{project}/memory", serve.origin),
            &[("cookie", &cookie)],
            None,
        );
        assert_eq!(memory.status, 200, "{}", memory.body);
        assert!(memory.body["items"].is_array(), "{}", memory.body);
        let unknown = call(
            "GET",
            &format!(
                "{}/v1/runbooks/rb_00000000000000000000000001/memory",
                serve.origin
            ),
            &[("cookie", &cookie)],
            None,
        );
        assert_eq!(unknown.status, 404, "{}", unknown.body);
        drop(serve);
    });
}

// ------------------------------------------------------------------ MCP2

const REDIRECT: &str = "http://127.0.0.1:53682/callback";

struct Raw {
    status: u16,
    location: Option<String>,
    cookies: Vec<String>,
    headers: Vec<(String, String)>,
    text: String,
}
fn raw(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    form: Option<&[(&str, &str)]>,
    json_body: Option<Value>,
) -> Raw {
    try_raw(method, url, headers, form, json_body).unwrap_or_else(|e| panic!("{method} {url}: {e}"))
}
fn try_raw(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    form: Option<&[(&str, &str)]>,
    json_body: Option<Value>,
) -> Result<Raw, Box<ureq::Error>> {
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    let mut request = agent.request(method, url).timeout(Duration::from_secs(20));
    for (k, v) in headers {
        request = request.set(k, v);
    }
    let response = match (form, json_body) {
        (Some(form), _) => request.send_form(form),
        (None, Some(body)) => request
            .set("content-type", "application/json")
            .send_string(&body.to_string()),
        (None, None) => request.call(),
    };
    let response = match response {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(Box::new(e)),
    };
    let headers = response
        .headers_names()
        .into_iter()
        .flat_map(|n| {
            response
                .all(&n)
                .into_iter()
                .map(|v| (n.clone(), v.to_owned()))
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(Raw {
        status: response.status(),
        location: response.header("location").map(str::to_owned),
        cookies: response
            .all("set-cookie")
            .into_iter()
            .map(str::to_owned)
            .collect(),
        headers,
        text: response.into_string().unwrap_or_default(),
    })
}
fn header<'a>(r: &'a Raw, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}
fn query_of(location: &str) -> String {
    location.split_once('?').map(|(_, q)| q.to_owned()).unwrap()
}
fn query_param(location: &str, name: &str) -> Option<String> {
    url_pairs(&query_of(location))
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
}
fn url_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}
fn decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            out.push(
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap(), 16).unwrap(),
            );
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// An OAuth client's connection: what the token endpoint returned.
struct Connected {
    client_id: String,
    /// The browser session the sign-in page set.
    cookie: String,
    access: String,
    refresh: String,
    scope: String,
}

/// RFC 7591 registration, then authorize with PKCE (S256), the sign-in page,
/// the consent page (ticking `tick`) and the token exchange.
fn oauth_connect(serve: &Serve, email: &str, scope: &str, tick: &[&str]) -> Connected {
    use base64::Engine;
    let origin = serve.origin.as_str();
    let registered = raw(
        "POST",
        &format!("{origin}/api/auth/oauth2/register"),
        &[],
        None,
        Some(
            json!({"client_name": "Test MCP client", "redirect_uris": [REDIRECT],
                    "token_endpoint_auth_method": "none"}),
        ),
    );
    assert_eq!(registered.status, 201, "{}", registered.text);
    let client_id = serde_json::from_str::<Value>(&registered.text).unwrap()["client_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let verifier = format!("{:064x}", rand_u128(email));
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(runbook_core::credentials::secret_hash(&verifier).as_bytes());
    let authorize = format!(
        "{origin}/api/auth/oauth2/authorize?response_type=code&client_id={client_id}&redirect_uri={}&state=s1&code_challenge={challenge}&code_challenge_method=S256&scope={}&resource={}",
        encode(REDIRECT),
        encode(scope),
        encode(&format!("{origin}/mcp")),
    );
    let r = raw("GET", &authorize, &[], None, None);
    assert_eq!(r.status, 302, "{}", r.text);
    let to_sign_in = r.location.unwrap();
    assert!(
        to_sign_in.starts_with(&format!("{origin}/api/auth/oauth/sign-in?")),
        "{to_sign_in}"
    );
    let q = query_of(&to_sign_in);
    let page = raw("GET", &to_sign_in, &[], None, None);
    assert_eq!(page.status, 200);
    assert!(
        header(&page, "content-security-policy")
            .unwrap()
            .contains("default-src 'none'")
    );
    let sent = raw(
        "POST",
        &format!("{origin}/api/auth/oauth/sign-in"),
        &[("origin", origin)],
        Some(&[("oauth_query", &q), ("email", email)]),
        None,
    );
    assert_eq!(sent.status, 200, "{}", sent.text);
    let code = captured_code(&serve.capture, email);
    let signed_in = raw(
        "POST",
        &format!("{origin}/api/auth/oauth/sign-in"),
        &[("origin", origin)],
        Some(&[("oauth_query", &q), ("email", email), ("otp", &code)]),
        None,
    );
    assert_eq!(signed_in.status, 303, "{}", signed_in.text);
    let cookie = signed_in
        .cookies
        .iter()
        .find(|c| c.starts_with("runbook.session_token="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let r = raw(
        "GET",
        &format!("{origin}{}", signed_in.location.unwrap()),
        &[("cookie", &cookie)],
        None,
        None,
    );
    assert_eq!(r.status, 302, "{}", r.text);
    let consent = r.location.unwrap();
    assert!(consent.contains("/api/auth/oauth/consent?"), "{consent}");
    let page = raw("GET", &consent, &[("cookie", &cookie)], None, None);
    assert_eq!(page.status, 200);
    assert!(
        page.text.contains("<code>runbooks:read</code>"),
        "{}",
        page.text
    );
    let q = query_of(&consent);
    let mut fields = vec![("oauth_query", q.as_str()), ("decision", "allow")];
    fields.extend(tick.iter().map(|s| ("scope", *s)));
    let decided = raw(
        "POST",
        &format!("{origin}/api/auth/oauth/consent"),
        &[("origin", origin), ("cookie", &cookie)],
        Some(&fields),
        None,
    );
    assert_eq!(decided.status, 303, "{}", decided.text);
    let back = decided.location.unwrap();
    assert!(back.starts_with(REDIRECT), "{back}");
    let code = query_param(&back, "code").unwrap();
    let resource = format!("{origin}/mcp");
    let token = raw(
        "POST",
        &format!("{origin}/api/auth/oauth2/token"),
        &[],
        Some(&[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &code),
            ("code_verifier", &verifier),
            ("redirect_uri", REDIRECT),
            ("resource", &resource),
        ]),
        None,
    );
    assert_eq!(token.status, 200, "{}", token.text);
    let t: Value = serde_json::from_str(&token.text).unwrap();
    Connected {
        client_id,
        cookie,
        access: t["access_token"].as_str().unwrap().to_owned(),
        refresh: t["refresh_token"].as_str().unwrap_or_default().to_owned(),
        scope: t["scope"].as_str().unwrap().to_owned(),
    }
}

/// How the auth service stores an access token: base64url(SHA-256) of the
/// token without its prefix.
fn token_hash(token: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        runbook_core::credentials::secret_hash(token.strip_prefix("trb_oat_").unwrap()).as_bytes(),
    )
}

fn rand_u128(seed: &str) -> u128 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let digest = runbook_core::credentials::secret_hash(&format!("{seed}{now}"));
    u128::from_be_bytes(digest.as_bytes()[..16].try_into().unwrap())
}

/// One JSON-RPC message to `/mcp` with a bearer (or none).
fn mcp(serve: &Serve, bearer: Option<&str>, extra: &[(&str, &str)], message: Value) -> Raw {
    let auth = bearer.map(|b| format!("Bearer {b}"));
    let mut headers: Vec<(&str, &str)> = vec![("accept", "application/json")];
    if let Some(auth) = &auth {
        headers.push(("authorization", auth));
    }
    headers.extend_from_slice(extra);
    let url = format!("{}/mcp", serve.origin);
    // A refusal (401) is answered before the body is read, so on macOS the
    // client's body write can race it and fail (EINVAL). `initialize` has no
    // effect, so it is retried; tool calls are never retried.
    let attempts = if message["method"] == "initialize" {
        3
    } else {
        1
    };
    for attempt in 1..=attempts {
        match try_raw("POST", &url, &headers, None, Some(message.clone())) {
            Ok(r) => return r,
            Err(e) if attempt == attempts => panic!("POST {url}: {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    unreachable!()
}
fn tool(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}
fn tool_result(r: &Raw) -> Value {
    assert_eq!(r.status, 200, "{}", r.text);
    let v: Value = serde_json::from_str(&r.text).unwrap();
    v["result"].clone()
}

/// MCP2 end to end on the native server: an MCP client discovers the
/// authorization server from `/mcp`'s 401, registers, signs a person in
/// with PKCE and consent, and calls tools with an introspected token whose
/// scopes are the consent's, narrowed to the person's role.
#[test]
#[ignore = "requires DATABASE_URL and a built auth/ (npm run build); creates and drops only test-owned databases"]
fn an_mcp_client_connects_with_oauth() {
    with_database(async |database| {
        let port = free_port();
        let env = base_env(database, "http://127.0.0.1:1", &format!("127.0.0.1:{port}"));
        let out = server_command(&env, &["bootstrap"]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let issued: Value = serde_json::from_slice(&out.stdout).unwrap();
        let pat = issued["pat"].as_str().unwrap().to_owned();
        let org = issued["org_id"].as_str().unwrap().to_owned();
        let serve = serve(database, "email");
        let origin = serve.origin.clone();
        let bearer = format!("Bearer {pat}");
        for (email, role, key) in [
            ("operator@example.test", "operator", "inv-op"),
            ("editor@example.test", "editor", "inv-ed"),
        ] {
            let r = call(
                "POST",
                &format!("{origin}/v1/orgs/{org}/invitations"),
                &[("authorization", &bearer), ("idempotency-key", key)],
                Some(json!({"email": email, "role": "member",
                            "projects": [{"project": "default", "role": role}]})),
            );
            assert_eq!(r.status, 201, "{}", r.body);
        }

        // Discovery: /mcp's 401 names the protected resource metadata, which
        // names the authorization server, whose metadata the proxy serves.
        let init = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
        let r = mcp(&serve, None, &[], init.clone());
        assert_eq!(r.status, 401);
        let challenge = header(&r, "www-authenticate").unwrap().to_owned();
        let metadata_url = format!("{origin}/.well-known/oauth-protected-resource/mcp");
        assert_eq!(
            challenge,
            format!("Bearer resource_metadata=\"{metadata_url}\"")
        );
        let prm = raw("GET", &metadata_url, &[], None, None);
        assert_eq!(prm.status, 200);
        let prm: Value = serde_json::from_str(&prm.text).unwrap();
        assert_eq!(prm["resource"], json!(format!("{origin}/mcp")));
        assert_eq!(
            prm["authorization_servers"],
            json!([format!("{origin}/api/auth")])
        );
        let root = raw(
            "GET",
            &format!("{origin}/.well-known/oauth-protected-resource"),
            &[],
            None,
            None,
        );
        assert_eq!(serde_json::from_str::<Value>(&root.text).unwrap(), prm);
        let asm = raw(
            "GET",
            &format!("{origin}/.well-known/oauth-authorization-server/api/auth"),
            &[],
            None,
            None,
        );
        assert_eq!(asm.status, 200, "{}", asm.text);
        let asm: Value = serde_json::from_str(&asm.text).unwrap();
        assert_eq!(asm["issuer"], json!(format!("{origin}/api/auth")));
        assert_eq!(asm["code_challenge_methods_supported"], json!(["S256"]));

        // The operator consents to read and write: write is wider than the
        // role, so the token works for reads and is refused for drafts.
        let op = oauth_connect(
            &serve,
            "operator@example.test",
            "runbooks:read runbooks:write executions:read executions:start offline_access",
            &[
                "runbooks:read",
                "runbooks:write",
                "executions:read",
                "executions:start",
                "offline_access",
            ],
        );
        assert!(op.access.starts_with("trb_oat_"), "{}", op.access);
        assert!(op.scope.contains("runbooks:write"));
        let r = mcp(&serve, Some(&op.access), &[], init.clone());
        assert_eq!(r.status, 200, "{}", r.text);
        let listed = tool_result(&mcp(
            &serve,
            Some(&op.access),
            &[],
            tool("list_runbooks", json!({"project": "default"})),
        ));
        assert_ne!(listed["isError"], json!(true), "{listed}");
        let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: oauth-check, title: OAuth check}\nspec:\n  steps:\n    - id: check\n      title: Check\n      actor: agent\n      instructions: Check.\n";
        let refused = tool_result(&mcp(
            &serve,
            Some(&op.access),
            &[],
            tool(
                "save_runbook_draft",
                json!({"source": source, "expected_revision": 0, "project": "default"}),
            ),
        ));
        assert_eq!(refused["isError"], json!(true), "{refused}");
        assert!(refused.to_string().contains("403"), "{refused}");

        // The OAuth token is for /mcp only: the REST API refuses it.
        let rest = call(
            "GET",
            &format!("{origin}/v1/orgs"),
            &[("authorization", &format!("Bearer {}", op.access))],
            None,
        );
        assert_eq!(rest.status, 401, "{}", rest.body);

        // Refresh: a new access token that works; the refresh rotated.
        let refreshed = raw(
            "POST",
            &format!("{origin}/api/auth/oauth2/token"),
            &[],
            Some(&[
                ("grant_type", "refresh_token"),
                ("client_id", &op.client_id),
                ("refresh_token", &op.refresh),
            ]),
            None,
        );
        assert_eq!(refreshed.status, 200, "{}", refreshed.text);
        let next: Value = serde_json::from_str(&refreshed.text).unwrap();
        let next_access = next["access_token"].as_str().unwrap().to_owned();
        assert_ne!(next_access, op.access);
        // Revoked before its first use (so not cached): refused.
        let revoked = raw(
            "POST",
            &format!("{origin}/api/auth/oauth2/revoke"),
            &[],
            Some(&[
                ("token", &next_access),
                ("token_type_hint", "access_token"),
                ("client_id", &op.client_id),
            ]),
            None,
        );
        assert_eq!(revoked.status, 200, "{}", revoked.text);
        let r = mcp(&serve, Some(&next_access), &[], init.clone());
        assert_eq!(r.status, 401, "{}", r.text);
        assert_eq!(
            header(&r, "www-authenticate").unwrap(),
            format!("Bearer resource_metadata=\"{metadata_url}\", error=\"invalid_token\"")
        );
        // An unknown token of the OAuth shape, and a cookie without a bearer.
        let r = mcp(
            &serve,
            Some("trb_oat_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            &[],
            init.clone(),
        );
        assert_eq!(r.status, 401);
        let r = mcp(&serve, None, &[("cookie", &op.cookie)], init.clone());
        assert_eq!(r.status, 401, "a cookie never authenticates /mcp");
        // A PAT still works as before.
        assert_eq!(mcp(&serve, Some(&pat), &[], init.clone()).status, 200);

        // Review 7 and 6 against the real auth service and Postgres.
        let pg = runbook_store_postgres::PostgresStore::connect(
            &runbook_store_postgres::DatabaseConfig::from_values(
                Some(database_url(database)),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let refresh = |token: &str| {
            let r = raw(
                "POST",
                &format!("{origin}/api/auth/oauth2/token"),
                &[],
                Some(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &op.client_id),
                    ("refresh_token", token),
                ]),
                None,
            );
            assert_eq!(r.status, 200, "{}", r.text);
            let v: Value = serde_json::from_str(&r.text).unwrap();
            (
                v["access_token"].as_str().unwrap().to_owned(),
                v["refresh_token"].as_str().unwrap().to_owned(),
            )
        };
        let (b, r3) = refresh(next["refresh_token"].as_str().unwrap());
        // Audience: the same token row bound to another resource is refused,
        // because Rust names its own /mcp and the auth service checks it.
        pg.client()
            .execute(
                "UPDATE auth.oauth_access_token SET resources = array_replace(resources, $1, 'https://elsewhere.example/mcp') WHERE token = $2",
                &[&format!("{origin}/mcp"), &token_hash(&b)],
            )
            .await
            .unwrap();
        let r = mcp(&serve, Some(&b), &[], init.clone());
        assert_eq!(r.status, 401, "a token for another resource: {}", r.text);
        // Negative cache: a token the auth service called inactive stays
        // refused for a few seconds, even once it becomes valid.
        let (c, r4) = refresh(&r3);
        let t = format!("trb_oat_{}", "Q".repeat(32));
        assert_eq!(mcp(&serve, Some(&t), &[], init.clone()).status, 401);
        pg.client()
            .execute(
                "UPDATE auth.oauth_access_token SET token = $1 WHERE token = $2",
                &[&token_hash(&t), &token_hash(&c)],
            )
            .await
            .unwrap();
        assert_eq!(
            mcp(&serve, Some(&t), &[], init.clone()).status,
            401,
            "still remembered as inactive"
        );
        std::thread::sleep(Duration::from_millis(5_500));
        assert_eq!(
            mcp(&serve, Some(&t), &[], init.clone()).status,
            200,
            "asked again after the window"
        );
        // Signing out of the web app does not end the grant: refresh still
        // works and its tokens call /mcp.
        let out = call(
            "POST",
            &format!("{origin}/api/auth/sign-out"),
            &[("origin", &origin), ("cookie", &op.cookie)],
            Some(json!({})),
        );
        assert_eq!(out.status, 200, "{}", out.body);
        let (d, r5) = refresh(&r4);
        assert_eq!(mcp(&serve, Some(&d), &[], init.clone()).status, 200);
        // An OAuth token is no bearer on the REST API: no device approval, no
        // execution credentials.
        for path in [
            "/v1/device/approve",
            "/v1/executions/exe_00000000000000000000000001/tokens",
        ] {
            let r = call(
                "POST",
                &format!("{origin}{path}"),
                &[
                    ("authorization", &format!("Bearer {d}")),
                    ("idempotency-key", "oauth-rest"),
                ],
                Some(json!({"user_code": "ABCD-EFGH", "approve": true})),
            );
            assert!(
                matches!(r.status, 401 | 403),
                "{path}: {} {}",
                r.status,
                r.body
            );
        }

        // Re-check 4: a token revoked through the proxy stops at once (the
        // proxied revoke clears the cache that holds `d`).
        let revoked = raw(
            "POST",
            &format!("{origin}/api/auth/oauth2/revoke"),
            &[],
            Some(&[
                ("token", &d),
                ("token_type_hint", "access_token"),
                ("client_id", &op.client_id),
            ]),
            None,
        );
        assert_eq!(revoked.status, 200, "{}", revoked.text);
        assert_eq!(mcp(&serve, Some(&d), &[], init.clone()).status, 401);
        // A member removed from the organization: their grant authorizes
        // nothing (403), though the auth service still vouches for the token.
        let users = call(
            "GET",
            &format!("{origin}/v1/orgs/{org}/users"),
            &[("authorization", &bearer)],
            None,
        );
        let operator = users.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["name"] == "operator")
            .unwrap_or_else(|| panic!("{}", users.body))["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (e, _) = refresh(&r5);
        let off = call(
            "PATCH",
            &format!("{origin}/v1/orgs/{org}/users/{operator}"),
            &[("authorization", &bearer), ("idempotency-key", "remove-op")],
            Some(json!({"active": false})),
        );
        assert_eq!(off.status, 200, "{}", off.body);
        let r = mcp(&serve, Some(&e), &[], init.clone());
        assert_eq!(r.status, 403, "{}", r.text);

        // The editor leaves runbooks:publish unticked: drafts work, publishing
        // is handed to a person.
        let ed = oauth_connect(
            &serve,
            "editor@example.test",
            "runbooks:read runbooks:write runbooks:publish",
            &["runbooks:read", "runbooks:write"],
        );
        assert!(!ed.scope.contains("publish"), "{}", ed.scope);
        let saved = tool_result(&mcp(
            &serve,
            Some(&ed.access),
            &[],
            tool(
                "save_runbook_draft",
                json!({"source": source, "expected_revision": 0, "project": "default"}),
            ),
        ));
        assert_ne!(saved["isError"], json!(true), "{saved}");
        let published = tool_result(&mcp(
            &serve,
            Some(&ed.access),
            &[],
            tool(
                "publish_runbook",
                json!({"runbook": "oauth-check", "project": "default", "revision": 1}),
            ),
        ));
        assert!(
            published.to_string().contains("ask_human_to_publish"),
            "{published}"
        );
        // Re-check 1: connected apps. The editor's browser session lists the
        // grant and disconnects it; the token, cached by the call above,
        // stops at once.
        let apps = call(
            "GET",
            &format!("{origin}/v1/me/apps"),
            &[("cookie", &ed.cookie)],
            None,
        );
        assert_eq!(apps.status, 200, "{}", apps.body);
        let app = apps.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["client_id"] == json!(ed.client_id))
            .unwrap_or_else(|| panic!("{}", apps.body))
            .clone();
        assert_eq!(app["client_name"], "Test MCP client");
        assert_eq!(
            app["redirect_hosts"],
            json!(["127.0.0.1:53682 (an app on this computer)"])
        );
        let gone = call(
            "DELETE",
            &format!("{origin}/v1/me/apps/{}", ed.client_id),
            &[
                ("cookie", &ed.cookie),
                ("origin", &origin),
                ("x-runbook-csrf", "1"),
            ],
            None,
        );
        assert_eq!(gone.status, 204, "{}", gone.body);
        assert_eq!(mcp(&serve, Some(&ed.access), &[], init.clone()).status, 401);
        let again = call(
            "DELETE",
            &format!("{origin}/v1/me/apps/{}", ed.client_id),
            &[
                ("cookie", &ed.cookie),
                ("origin", &origin),
                ("x-runbook-csrf", "1"),
            ],
            None,
        );
        assert_eq!(again.status, 404, "{}", again.body);
        // The owner has no grants; a service-free PAT person lists none.
        let none = call(
            "GET",
            &format!("{origin}/v1/me/apps"),
            &[("authorization", &bearer)],
            None,
        );
        assert_eq!((none.status, &none.body), (200, &json!({"items": []})));
        drop(serve);
    });
}
