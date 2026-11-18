//! C2 `login`/`logout` (stored PAT) and `token create|list|revoke`.
mod fake_api;
use fake_api::*;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const LOGIN: &str = "trb_pat_login-secret";
const NEW_SECRET: &str = "trb_pat_brand-new-secret";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn logins_file(home: &std::path::Path) -> std::path::PathBuf {
    home.join("runbook").join("credentials").join("logins.json")
}

#[test]
fn login_verifies_and_stores_the_token_privately_without_printing_it() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs()]);
    let o = cli(
        &host,
        home.path(),
        &["login", "--token-stdin"],
        &[],
        Some(&format!("{LOGIN}\n")),
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!out(&o).contains(LOGIN) && !err(&o).contains(LOGIN));
    assert!(out(&o).contains("logged in"), "{}", out(&o));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "GET /v1/orgs");
    assert_eq!(
        header(&seen[0], "authorization"),
        Some(format!("Bearer {LOGIN}").as_str())
    );
    let file = logins_file(home.path());
    assert!(std::fs::read_to_string(&file).unwrap().contains(LOGIN));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn stored_login_is_used_per_host_and_runbook_token_wins() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(
            201,
            json!({"id":USR,"name":"A","active":true,"role":"member"}),
        ),
        Reply::json(
            201,
            json!({"id":USR,"name":"A","active":true,"role":"member"}),
        ),
        orgs(),
        Reply::json(200, json!({"id":EXE,"status":"running"})),
    ]);
    let login = cli(
        &host,
        home.path(),
        &["login", "--token-stdin"],
        &[],
        Some(LOGIN),
    );
    assert_eq!(login.status.code(), Some(0), "{}", err(&login));
    let create = ["user", "create", "--name", "A", "--org", ORG];
    assert_eq!(
        cli(&host, home.path(), &create, &[], None).status.code(),
        Some(0)
    );
    assert_eq!(
        cli(&host, home.path(), &create, &token(), None)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        cli(&host, home.path(), &["whoami"], &[], None)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        cli(&host, home.path(), &["run", "get", EXE], &[], None)
            .status
            .code(),
        Some(0)
    );
    let seen = server.join().unwrap();
    let bearer = |i: usize| header(&seen[i], "authorization").unwrap().to_string();
    assert_eq!(bearer(1), format!("Bearer {LOGIN}"));
    assert_eq!(bearer(2), format!("Bearer {PAT}"), "RUNBOOK_TOKEN wins");
    assert_eq!(bearer(3), format!("Bearer {LOGIN}"), "whoami");
    assert_eq!(bearer(4), format!("Bearer {LOGIN}"), "run get");

    // Another host has no stored login.
    let o = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["user", "create", "--name", "A", "--org", ORG],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("not logged in"), "{}", err(&o));
}

#[test]
fn authoring_commands_use_the_stored_login() {
    // docs/install.md section 6: the owner pushes and publishes after
    // `runbook login`, without RUNBOOK_TOKEN.
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(200, json!({"items":[],"next_cursor":null})),
    ]);
    let login = cli(
        &host,
        home.path(),
        &["login", "--token-stdin"],
        &[],
        Some(LOGIN),
    );
    assert_eq!(login.status.code(), Some(0), "{}", err(&login));
    let o = cli(
        &host,
        home.path(),
        &["list", "--project", "pilot"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        header(&seen[1], "authorization"),
        Some(format!("Bearer {LOGIN}").as_str())
    );
}

#[test]
fn rejected_or_malformed_login_stores_nothing() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![Reply::json(
        401,
        json!({"code":"auth.invalid_credential"}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &["login", "--token-stdin"],
        &[],
        Some(LOGIN),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("auth.invalid_credential"), "{}", err(&o));
    assert!(!logins_file(home.path()).exists());
    assert_eq!(server.join().unwrap().len(), 1);

    let (host, server) = serve(vec![]);
    for (args, stdin) in [
        (vec!["login", "--token-stdin"], Some("")),
        (
            vec!["login", "--token-stdin"],
            Some("trb_exe_execution-secret"),
        ),
    ] {
        let o = cli(&host, home.path(), &args, &[], stdin);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
        assert!(!err(&o).contains("execution-secret"));
    }
    assert!(server.join().unwrap().is_empty());
    assert!(!logins_file(home.path()).exists());
}

#[test]
fn logout_forgets_the_stored_token() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = serve(vec![orgs()]);
    let o = cli(
        &host,
        home.path(),
        &["login", "--token-stdin"],
        &[],
        Some(LOGIN),
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(&host, home.path(), &["logout", "--json"], &[], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        serde_json::from_str::<Value>(&out(&o)).unwrap(),
        json!({"host": host, "logged_out": true})
    );
    assert!(
        !std::fs::read_to_string(logins_file(home.path()))
            .unwrap()
            .contains(LOGIN)
    );
    let o = cli(
        &host,
        home.path(),
        &["user", "create", "--name", "A", "--org", ORG],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    let o = cli(&host, home.path(), &["logout"], &[], None);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("not logged in"), "{}", out(&o));
}

#[test]
fn token_create_sends_scopes_projects_and_expiry_and_prints_the_secret_once() {
    let home = tempfile::tempdir().unwrap();
    let created =
        json!({"token":{"id":TOK,"name":"ci","scopes":["runbooks:read"]},"secret":NEW_SECRET});
    let (host, server) = serve(vec![
        Reply::json(201, created.clone()),
        Reply::json(201, created.clone()),
    ]);
    let before = now_ms();
    let o = cli(
        &host,
        home.path(),
        &[
            "token",
            "create",
            "--name",
            "ci",
            "--scope",
            "runbooks:read",
            "--scope",
            "executions:start",
            "--project",
            PRJ,
            "--expires",
            "30d",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o).matches(NEW_SECRET).count(), 1, "{}", out(&o));
    assert!(out(&o).contains(TOK));
    assert!(!err(&o).contains(NEW_SECRET));
    let o = cli(
        &host,
        home.path(),
        &["token", "create", "--json", "--idempotency-key", "k1"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), created);
    assert!(!err(&o).contains(NEW_SECRET));

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "POST /v1/tokens");
    let body = body_of(&seen[0]);
    assert_eq!(body["name"], "ci");
    assert_eq!(body["scopes"], json!(["runbooks:read", "executions:start"]));
    assert_eq!(body["projects"], json!([PRJ]));
    let expires = body["expires_at_ms"].as_i64().unwrap();
    let thirty_days = 30 * 86_400_000;
    assert!(expires >= before + thirty_days && expires <= now_ms() + thirty_days);
    assert!(header(&seen[0], "idempotency-key").is_some());
    assert_eq!(body_of(&seen[1]), json!({}), "absent options are omitted");
    assert_eq!(header(&seen[1], "idempotency-key"), Some("k1"));
}

#[test]
fn token_create_agent_sends_the_d26_actor_kind() {
    let home = tempfile::tempdir().unwrap();
    let created = json!({"token":{"id":TOK,"name":"bot","actor_kind":"agent"},"secret":NEW_SECRET});
    let (host, server) = serve(vec![Reply::json(201, created)]);
    let o = cli(
        &host,
        home.path(),
        &["token", "create", "--name", "bot", "--agent"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(
        body_of(&seen[0]),
        json!({"name": "bot", "actor_kind": "agent"})
    );
}

#[test]
fn token_create_rejects_bad_options_before_any_request() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);
    for args in [
        vec!["token", "create", "--scope", "runbooks:everything"],
        vec!["token", "create", "--expires", "soon"],
        vec!["token", "create", "--expires", "366d"],
        vec!["token", "create", "--expires", "0h"],
        vec!["token", "create", "--project", "Not A Slug"],
        vec!["token", "create", "--user", "alice"],
        vec!["token", "revoke", "tok_nope"],
    ] {
        let o = cli(&host, home.path(), &args, &token(), None);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn token_create_for_another_user_surfaces_the_server_rejection() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![Reply::json(
        403,
        json!({"code":"auth.forbidden","detail":"bootstrap issuance is local only"}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &["token", "create", "--user", USR],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("auth.forbidden"), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(body_of(&seen[0]), json!({"user_id": USR}));
}

#[test]
fn token_list_and_revoke() {
    let home = tempfile::tempdir().unwrap();
    let page = json!({"items":[{"id":TOK,"name":"ci","scopes":["runbooks:read"],"expires_at_ms":null,"last_used_at_ms":null}],"next_cursor":null});
    let (host, server) = serve(vec![
        Reply::json(200, page.clone()),
        Reply::json(200, page.clone()),
        Reply::empty(204),
        Reply::json(404, json!({"code":"resource.not_found"})),
    ]);
    let o = cli(&host, home.path(), &["token", "list"], &token(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains(TOK) && out(&o).contains("ci"),
        "{}",
        out(&o)
    );
    let o = cli(
        &host,
        home.path(),
        &["token", "list", "--json"],
        &token(),
        None,
    );
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), page);
    let o = cli(
        &host,
        home.path(),
        &["token", "revoke", TOK],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(&format!("revoked {TOK}")));
    let o = cli(
        &host,
        home.path(),
        &["token", "revoke", TOK],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "GET /v1/tokens");
    assert_eq!(line(&seen[2]), format!("DELETE /v1/tokens/{TOK}"));
}

/// MCP2 re-check 1: `runbook apps list|revoke` against /v1/me/apps.
#[test]
fn apps_list_and_revoke() {
    let home = tempfile::tempdir().unwrap();
    let page = json!({"items":[{"client_id":"abcDEF","client_name":"Claude","redirect_hosts":["claude.ai"],
        "scopes":["runbooks:read"],"created_at_ms":1,"last_used_at_ms":null,"expires_at_ms":2}]});
    let (host, server) = serve(vec![
        Reply::json(200, page.clone()),
        Reply::json(200, page.clone()),
        Reply::empty(204),
        Reply::json(404, json!({"code":"resource.not_found"})),
    ]);
    let o = cli(&host, home.path(), &["apps", "list"], &token(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains("abcDEF")
            && out(&o).contains("\"Claude\" (unverified)")
            && out(&o).contains("returns to claude.ai"),
        "{}",
        out(&o)
    );
    let o = cli(
        &host,
        home.path(),
        &["apps", "list", "--json"],
        &token(),
        None,
    );
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), page);
    let o = cli(
        &host,
        home.path(),
        &["apps", "revoke", "abcDEF"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("disconnected abcDEF"));
    let o = cli(
        &host,
        home.path(),
        &["apps", "revoke", "abcDEF"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "GET /v1/me/apps");
    assert_eq!(line(&seen[2]), "DELETE /v1/me/apps/abcDEF");
    // A malformed ID never reaches the server.
    let o = cli(
        &host,
        home.path(),
        &["apps", "revoke", "../tokens"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
}
