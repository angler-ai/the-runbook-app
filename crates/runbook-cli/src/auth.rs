//! C2 `runbook login` / `runbook logout`: a personal access token kept in
//! the private credential store (`logins.json`, 0600), keyed by the
//! resolved host, so each context has its own login.
//!
//! Precedence for user-level commands (`user`, `project`, `team`, `token`,
//! `webhook`, `run`, `whoami`, and the authoring `list`, `get`, `pull`,
//! `push`, `publish`): `RUNBOOK_TOKEN`, then the stored login for
//! the host. Agent-loop `step` commands keep their own order and never fall
//! back to a human login. The OAuth device grant is not implemented yet, so
//! Without `--token-stdin`, `runbook login` runs the RFC 8628 OAuth device
//! authorization grant (AUTH4, D47): requests user and device codes, prints
//! verification URL to stderr, optionally opens a browser, polls for the
//! personal access token, and stores it privately without ever printing it.
use crate::agent::Env;
use crate::api::{ApiClient, ApiError, DeviceTokenPoll};
use crate::cliutil::{Exit, OK, api_failed, fail, print_json, usage};
use crate::session::now_ms;
use clap::Args;
use runbook_core::credentials::Scope;
use serde_json::json;
use std::io::{IsTerminal, Read};
use std::time::Duration;

#[derive(Args, Debug)]
pub struct LoginArgs {
    #[arg(help = "Host URL to log in to")]
    pub host: Option<String>,
    #[arg(long = "token-stdin", help = "Read a personal access token from stdin")]
    pub token_stdin: bool,
    #[arg(
        long = "migrate-to-keychain",
        conflicts_with_all = ["host", "token_stdin", "no_browser", "scopes", "admin"],
        help = "Move every login stored in the 0600 file into the OS keychain"
    )]
    pub migrate_to_keychain: bool,
    #[arg(long = "no-browser", help = "Do not attempt to open a browser")]
    pub no_browser: bool,
    #[arg(
        long = "scope",
        value_name = "SCOPE",
        help = "Ask for exactly this scope (repeatable) instead of the default set"
    )]
    pub scopes: Vec<String>,
    #[arg(
        long,
        help = "Also ask for the admin scopes (tokens:manage, project:admin, webhooks:manage, executions:admin)"
    )]
    pub admin: bool,
    #[arg(long, help = "Output as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct LogoutArgs {
    #[arg(long, help = "Output as JSON")]
    pub json: bool,
}

/// `env` with its token filled from the stored login when `RUNBOOK_TOKEN`
/// is unset.
pub fn with_login(mut env: Env) -> Env {
    if env.token.is_none()
        && let Some(store) = &env.store
    {
        match crate::logins::load(store, &crate::logins::LoginKey::of(&env)) {
            Ok(token) => env.token = token,
            Err(e) => eprintln!("warning: {e}"),
        }
    }
    env
}

fn credential_kind(token: &str) -> Option<&'static str> {
    if token.starts_with("trb_pat_") {
        Some("personal")
    } else if token.starts_with("trb_svc_") {
        Some("service")
    } else {
        None
    }
}

/// Sign-in gate fix 3: what `runbook login` asks for by default: what an
/// operator or agent CLI needs to author, publish and run runbooks. The
/// approver may narrow it further.
pub const DEFAULT_LOGIN_SCOPES: [Scope; 8] = [
    Scope::RunbooksRead,
    Scope::RunbooksWrite,
    Scope::RunbooksPublish,
    Scope::ExecutionsRead,
    Scope::ExecutionsStart,
    Scope::ExecutionsAct,
    Scope::SkillsRead,
    Scope::SkillsWrite,
];
/// Added by `--admin`: managing tokens, webhooks, projects, and other
/// people's runs.
pub const ADMIN_LOGIN_SCOPES: [Scope; 4] = [
    Scope::TokensManage,
    Scope::WebhooksManage,
    Scope::ProjectAdmin,
    Scope::ExecutionsAdmin,
];

fn scope_name(scope: Scope) -> String {
    serde_json::to_value(scope)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The scope names `runbook login` asks for: `--scope` names exactly, or
/// the default set; `--admin` adds the admin scopes. `Err` names an
/// unknown scope.
pub fn login_scopes(named: &[String], admin: bool) -> Result<Vec<String>, String> {
    let mut scopes: std::collections::BTreeSet<Scope> = if named.is_empty() {
        DEFAULT_LOGIN_SCOPES.into()
    } else {
        named
            .iter()
            .map(|n| {
                serde_json::from_value::<Scope>(serde_json::Value::String(n.trim().to_owned()))
                    .map_err(|_| n.clone())
            })
            .collect::<Result<_, _>>()?
    };
    if admin {
        scopes.extend(ADMIN_LOGIN_SCOPES);
    }
    Ok(scopes.into_iter().map(scope_name).collect())
}

fn poll_sleep(seconds: u32) {
    #[cfg(debug_assertions)]
    if let Ok(scale) = std::env::var("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS")
        && let Ok(ms_per_sec) = scale.parse::<u64>()
    {
        std::thread::sleep(Duration::from_millis(seconds as u64 * ms_per_sec));
        return;
    }
    std::thread::sleep(Duration::from_secs(seconds as u64));
}

fn http_origin(raw: &str) -> Option<(&'static str, String, u16)> {
    let (scheme, rest) = if let Some(rest) = raw.strip_prefix("http://") {
        ("http", rest)
    } else {
        let rest = raw.strip_prefix("https://")?;
        ("https", rest)
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    // Gate fix 4: no userinfo and no backslash (browsers read `\` as `/`),
    // so the host checked is the host a browser opens.
    if raw.contains(['@', '\\']) {
        return None;
    }
    let host_port = authority;
    if host_port.is_empty() {
        return None;
    }
    let (host, port) = if host_port.starts_with('[') {
        let close_bracket = host_port.find(']')?;
        let host = &host_port[..=close_bracket];
        let rest = &host_port[close_bracket + 1..];
        let port = if let Some(port_str) = rest.strip_prefix(':') {
            port_str.parse::<u16>().ok()?
        } else if rest.is_empty() {
            if scheme == "http" { 80 } else { 443 }
        } else {
            return None;
        };
        (host.to_ascii_lowercase(), port)
    } else if let Some((host, port_str)) = host_port.split_once(':') {
        let port = port_str.parse::<u16>().ok()?;
        (host.to_ascii_lowercase(), port)
    } else {
        let port = if scheme == "http" { 80 } else { 443 };
        (host_port.to_ascii_lowercase(), port)
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme, host, port))
}

/// Validate that a server-provided browser URL matches the login host's origin
/// and contains no control characters, whitespace, userinfo (`@`) or
/// backslash.
pub fn browser_target_url<'a>(
    login_host: &str,
    complete_uri: &'a str,
    base_uri: &'a str,
) -> Option<&'a str> {
    let candidate = if !complete_uri.is_empty() {
        complete_uri
    } else {
        base_uri
    };
    if candidate.is_empty()
        || candidate
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let target_origin = http_origin(candidate)?;
    let host_origin = http_origin(login_host)?;
    if target_origin != host_origin {
        return None;
    }
    Some(candidate)
}

fn try_open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");

    #[cfg(not(target_os = "windows"))]
    cmd.arg(url);

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let _ = cmd.spawn();
}

pub fn handle_login(env: &Env, a: LoginArgs) -> i32 {
    let run = || -> Exit<i32> {
        let store = env
            .store
            .as_ref()
            .ok_or_else(|| usage("no config directory for the credential store"))?;
        if a.migrate_to_keychain {
            let moved = crate::logins::migrate_to_keychain(store).map_err(fail)?;
            if a.json {
                print_json(&json!({"migrated": moved, "storage": "keychain"}));
            } else {
                println!("moved {moved} login(s) into the OS keychain");
            }
            return Ok(OK);
        }

        if a.token_stdin {
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .map_err(|_| usage("cannot read the token from stdin"))?;
            let token = input.trim();
            if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(usage("stdin must contain exactly one token"));
            }
            let kind = credential_kind(token).ok_or_else(|| {
                usage(
                    "login takes a personal access token (trb_pat_…) or service token (trb_svc_…); \
                     execution credentials are installed with `runbook run token <exe>`",
                )
            })?;
            // Verify before storing: 403 is a valid credential without a
            // directory scope; anything else is refused.
            match ApiClient::new(env.host.clone(), token.to_string()).get_json("/v1/orgs") {
                Ok(_) | Err(ApiError::Problem { status: 403, .. }) => {}
                Err(e) => return Err(api_failed(e)),
            }
            let storage = crate::logins::save(store, &crate::logins::LoginKey::of(env), token)
                .map_err(fail)?;
            if env.token.is_some() {
                eprintln!(
                    "warning: RUNBOOK_TOKEN is set and takes precedence over the stored login"
                );
            }
            if a.json {
                print_json(&with_context(
                    json!({
                        "host": env.host,
                        "logged_in": true,
                        "credential": kind,
                        "storage": storage.as_str(),
                    }),
                    env,
                ));
            } else {
                println!(
                    "logged in to {}{} with a {kind} token",
                    env.host,
                    as_context(env)
                );
                eprintln!("{}", stored_note(storage));
            }
            return Ok(OK);
        }

        // AUTH4: RFC 8628 OAuth device authorization grant
        let scopes = login_scopes(&a.scopes, a.admin).map_err(|name| {
            usage(format!(
                "unknown scope {name:?}; scopes are {}",
                Scope::ALL.map(scope_name).join(", ")
            ))
        })?;
        let client = ApiClient::new(env.host.clone(), String::new());
        let code_view = client
            .device_code("runbook CLI", &scopes)
            .map_err(api_failed)?;

        eprintln!("Verification URI: {}", code_view.verification_uri);
        eprintln!("User code: {}", code_view.user_code);

        let should_open_browser = !a.no_browser
            && std::env::var_os("CI").is_none()
            && std::io::stdout().is_terminal()
            && std::io::stderr().is_terminal();

        if should_open_browser
            && let Some(target_url) = browser_target_url(
                &env.host,
                &code_view.verification_uri_complete,
                &code_view.verification_uri,
            )
        {
            try_open_browser(target_url);
        }

        let timeout = {
            #[cfg(debug_assertions)]
            if let Ok(scale) = std::env::var("RUNBOOK_TEST_DEVICE_POLL_SCALE_MS")
                && let Ok(ms_per_sec) = scale.parse::<u64>()
            {
                Duration::from_millis(((code_view.expires_in as u64) + 1) * ms_per_sec)
            } else {
                Duration::from_secs((code_view.expires_in as u64) + 5)
            }
            #[cfg(not(debug_assertions))]
            Duration::from_secs((code_view.expires_in as u64) + 5)
        };
        let deadline = std::time::Instant::now() + timeout;

        let mut interval = if code_view.interval == 0 {
            5
        } else {
            code_view.interval
        };
        let token_resp = loop {
            if std::time::Instant::now() >= deadline {
                return Err(fail(
                    "device code expired; run `runbook login` to try again",
                ));
            }
            match client.device_token(&code_view.device_code) {
                Ok(resp) => break resp,
                Err(DeviceTokenPoll::Pending) => {
                    poll_sleep(interval);
                }
                Err(DeviceTokenPoll::SlowDown { server_interval }) => {
                    interval += 5;
                    if let Some(s) = server_interval {
                        interval = interval.max(s as u32);
                    }
                    poll_sleep(interval);
                }
                Err(DeviceTokenPoll::Expired) => {
                    return Err(fail(
                        "device code expired; run `runbook login` to try again",
                    ));
                }
                Err(DeviceTokenPoll::Denied) => {
                    return Err(fail(
                        "access denied; the login request was rejected on the approval page",
                    ));
                }
                Err(DeviceTokenPoll::Api(e)) => {
                    return Err(api_failed(e));
                }
            }
        };

        let access_token = token_resp["access_token"]
            .as_str()
            .ok_or_else(|| fail("malformed response: missing access_token"))?;
        let storage = crate::logins::save(store, &crate::logins::LoginKey::of(env), access_token)
            .map_err(fail)?;

        if env.token.is_some() {
            eprintln!("warning: RUNBOOK_TOKEN is set and takes precedence over the stored login");
        }

        let mut user_id = token_resp["user_id"].as_str().map(str::to_owned);
        let mut expires_at: Option<i64> = token_resp["expires_at_ms"]
            .as_i64()
            .or_else(|| token_resp["expires_at"].as_i64());

        if user_id.is_none() || expires_at.is_none() {
            let auth_client = ApiClient::new(env.host.clone(), access_token.to_string());
            if let Ok(me) = auth_client.get_json("/v1/me") {
                if user_id.is_none() {
                    user_id = me["user"]["id"].as_str().map(str::to_owned);
                }
                if expires_at.is_none() {
                    expires_at = me["credential"]["expires_at_ms"].as_i64();
                }
            }
        }
        if expires_at.is_none()
            && let Some(exp_in) = token_resp["expires_in"].as_u64()
        {
            expires_at = Some(now_ms() + (exp_in as i64) * 1000);
        }

        if a.json {
            print_json(&with_context(
                json!({
                    "host": env.host,
                    "user_id": user_id,
                    "expires_at": expires_at,
                    "storage": storage.as_str(),
                }),
                env,
            ));
        } else {
            match &user_id {
                Some(uid) => println!("logged in to {}{} as {uid}", env.host, as_context(env)),
                None => println!("logged in to {}{}", env.host, as_context(env)),
            }
            eprintln!("{}", stored_note(storage));
        }
        Ok(OK)
    };
    run().unwrap_or_else(|code| code)
}

/// `value` with `context` added when a context's identity applies, so
/// output without contexts keeps its pre-CLI3 shape.
fn with_context(mut value: serde_json::Value, env: &Env) -> serde_json::Value {
    if let Some(context) = &env.context {
        value["context"] = json!(context);
    }
    value
}

fn stored_note(storage: crate::logins::Storage) -> &'static str {
    match storage {
        crate::logins::Storage::Keychain => "the token is stored in the OS keychain",
        crate::logins::Storage::File => "the token is stored in the 0600 credentials file",
    }
}

/// ` (context <name>)` when a context's identity applies.
fn as_context(env: &Env) -> String {
    env.context
        .as_ref()
        .map(|c| format!(" (context {c})"))
        .unwrap_or_default()
}

/// Forget the login this command would use: the context's own when a
/// context applies and has one, else the host's login (which such a
/// context falls back to). The output says which.
pub fn handle_logout(env: &Env, a: LogoutArgs) -> i32 {
    let removed = match &env.store {
        Some(store) => match crate::logins::logout(store, &crate::logins::LoginKey::of(env)) {
            Ok(removed) => removed,
            Err(e) => return fail(e),
        },
        None => None,
    };
    if a.json {
        let mut value = with_context(
            json!({"host": env.host, "logged_out": removed.is_some()}),
            env,
        );
        if env.context.is_some() {
            value["login"] = json!(removed.map(|r| match r {
                crate::logins::Removed::Context => "context",
                crate::logins::Removed::Host => "host",
            }));
        }
        print_json(&value);
    } else {
        match (removed, &env.context) {
            (None, _) => println!("not logged in to {}{}", env.host, as_context(env)),
            (Some(crate::logins::Removed::Host), Some(context)) => println!(
                "logged out of {} (its host login, which context {context} was using)",
                env.host
            ),
            (Some(_), _) => println!("logged out of {}{}", env.host, as_context(env)),
        }
    }
    OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_target_url_origin_validation() {
        let host = "https://example.com";
        // Same origin http/https with paths/queries is allowed
        assert_eq!(
            browser_target_url(
                host,
                "https://example.com/device?user_code=ABCD",
                "https://example.com/device"
            ),
            Some("https://example.com/device?user_code=ABCD")
        );
        // Falls back to base URI if complete URI is empty
        assert_eq!(
            browser_target_url(host, "", "https://example.com/device"),
            Some("https://example.com/device")
        );
        // Different host rejected
        assert_eq!(
            browser_target_url(
                host,
                "https://evil.com/device",
                "https://example.com/device"
            ),
            None
        );
        // Different scheme rejected
        assert_eq!(
            browser_target_url(
                "http://example.com",
                "https://example.com/device",
                "http://example.com/device"
            ),
            None
        );
        // Different port rejected
        assert_eq!(
            browser_target_url(
                "https://example.com:8443",
                "https://example.com:8080/device",
                ""
            ),
            None
        );
        // Non-http schemes rejected
        assert_eq!(browser_target_url(host, "javascript:alert(1)", ""), None);
        assert_eq!(browser_target_url(host, "file:///etc/passwd", ""), None);
        // Control characters or whitespace rejected
        assert_eq!(
            browser_target_url(host, "https://example.com/device\n", ""),
            None
        );
        assert_eq!(
            browser_target_url(host, "https://example.com/device ", ""),
            None
        );
        // Gate fix 4: a backslash or userinfo can make a browser read
        // another host than the one checked here.
        let login = "https://login.host";
        for evil in [
            "https://evil.example\\@login.host/device",
            "https://evil.example\\login.host/device",
            "https://login.host\\@evil.example/device",
            "https://user@login.host/device",
            "https://login.host@evil.example/device",
            "https://login.host:443@evil.example/device",
            "https://user:pass@login.host/device",
            "https://login.host/device?x=a@b",
            "https://login.host/de\u{7f}vice",
            "https://login.host/device\u{0}",
        ] {
            assert_eq!(browser_target_url(login, evil, ""), None, "{evil:?}");
            assert_eq!(browser_target_url(login, "", evil), None, "{evil:?}");
        }
        assert_eq!(
            browser_target_url(login, "https://login.host/device?user_code=WDJB-MJHT", ""),
            Some("https://login.host/device?user_code=WDJB-MJHT")
        );
    }
}
