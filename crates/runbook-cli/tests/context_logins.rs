//! CLI3: a context names a host and an identity, so one host can have
//! several logins. A context's own login is used whenever the resolved host
//! is that context's host; older host-keyed logins keep working.
mod support;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use support::*;

const PAT_A: &str = "trb_pat_alpha-secret";
const PAT_B: &str = "trb_pat_bravo-secret";

/// Run the binary without `--host`, so contexts decide the host.
fn run(home: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .args(args)
        .env_remove("RUNBOOK_TOKEN")
        .env_remove("RUNBOOK_HOST")
        .env_remove("RUNBOOK_CREDENTIAL_STORE")
        .env_remove("RUNBOOK_TEST_KEYCHAIN_DIR")
        .env("XDG_CONFIG_HOME", home)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn ok(o: &Output) {
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(o), err(o));
}

fn bearer(request: &str) -> String {
    header(request, "authorization").unwrap_or("").to_string()
}

/// `logins.json` (host logins, the pre-CLI3 shape) merged with
/// `logins.v2.json` (CLI3 records: context logins, keychain markers).
fn logins(home: &Path) -> Value {
    let dir = home.join("runbook").join("credentials");
    let read = |name: &str| -> Value {
        std::fs::read(dir.join(name))
            .map(|b| serde_json::from_slice(&b).unwrap())
            .unwrap_or(json!({}))
    };
    let mut all = read("logins.v2.json");
    all["hosts"] = read("logins.json")["hosts"].clone();
    all
}

fn orgs() -> Reply {
    reply(200, json!({"items": [], "next_cursor": null}))
}

#[test]
fn two_contexts_on_one_host_keep_separate_logins() {
    let home = tempfile::tempdir().unwrap();
    // Two verifications at login, then one whoami per context.
    let (host, server) = serve(vec![orgs(), orgs(), orgs(), orgs()]);
    ok(&run(
        home.path(),
        &["context", "add", "admin", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["context", "add", "bot", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "admin", "login", "--token-stdin", "--json"],
        PAT_A,
        &[],
    ));
    ok(&run(home.path(), &["context", "use", "bot"], "", &[]));
    let o = run(
        home.path(),
        &["login", "--token-stdin", "--json"],
        PAT_B,
        &[],
    );
    ok(&o);
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["context"], "bot", "{v}");

    ok(&run(
        home.path(),
        &["--context", "admin", "whoami", "--json"],
        "",
        &[],
    ));
    let o = run(home.path(), &["whoami", "--json"], "", &[]);
    ok(&o);
    // A stored login is the person's own credential: agents check this.
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["source"], "login", "{v}");

    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert_eq!(bearer(&seen[2]), format!("Bearer {PAT_A}"));
    assert_eq!(bearer(&seen[3]), format!("Bearer {PAT_B}"));

    // Context logins never become the host's login.
    let stored = logins(home.path());
    assert!(stored["hosts"].get(&host).is_none(), "{stored}");
    assert_eq!(stored["contexts"]["admin"]["host"], host.as_str());
}

#[test]
fn a_host_login_from_before_contexts_had_identities_still_works() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs()]);
    let creds = home.path().join("runbook").join("credentials");
    std::fs::create_dir_all(&creds).unwrap();
    std::fs::write(
        creds.join("logins.json"),
        json!({"hosts": {&host: PAT_A}}).to_string(),
    )
    .unwrap();
    ok(&run(
        home.path(),
        &["context", "add", "old", "--host", &host],
        "",
        &[],
    ));
    // Through a context with no login of its own, and through --host.
    ok(&run(
        home.path(),
        &["--context", "old", "whoami", "--json"],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--host", &host, "whoami", "--json"],
        "",
        &[],
    ));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[0]), format!("Bearer {PAT_A}"));
    assert_eq!(bearer(&seen[1]), format!("Bearer {PAT_A}"));
}

#[test]
fn context_remove_forgets_its_login_and_a_moved_context_drops_it() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs()]);
    ok(&run(
        home.path(),
        &["context", "add", "a", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["context", "add", "b", "--host", &host],
        "",
        &[],
    ));
    ok(&run(home.path(), &["context", "use", "a"], "", &[]));
    ok(&run(home.path(), &["login", "--token-stdin"], PAT_A, &[]));
    ok(&run(
        home.path(),
        &["--context", "b", "login", "--token-stdin"],
        PAT_B,
        &[],
    ));

    // Moving `b` to another host drops its login: it was for the old host.
    ok(&run(
        home.path(),
        &["context", "add", "b", "--host", "https://elsewhere.example"],
        "",
        &[],
    ));
    assert!(logins(home.path())["contexts"].get("b").is_none());

    let o = run(home.path(), &["context", "remove", "a"], "", &[]);
    ok(&o);
    assert!(logins(home.path())["contexts"].get("a").is_none());
    let list = run(home.path(), &["context", "list", "--json"], "", &[]);
    ok(&list);
    let v: Value = serde_json::from_str(&out(&list)).unwrap();
    assert_eq!(
        v,
        json!([{"name": "b", "host": "https://elsewhere.example", "current": false, "logged_in": false}])
    );
    // The removed context was current: nothing is current now, so the
    // default host applies rather than a stale-context error.
    let o = run(home.path(), &["--context", "a", "whoami"], "", &[]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("unknown context"), "{}", err(&o));
    let o = run(home.path(), &["context", "remove", "a"], "", &[]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    server.join().unwrap();
}

#[test]
fn logout_through_a_context_forgets_only_that_context() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs(), orgs()]);
    ok(&run(
        home.path(),
        &["context", "add", "a", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["context", "add", "b", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "a", "login", "--token-stdin"],
        PAT_A,
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "b", "login", "--token-stdin"],
        PAT_B,
        &[],
    ));
    let o = run(
        home.path(),
        &["--context", "a", "logout", "--json"],
        "",
        &[],
    );
    ok(&o);
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["logged_out"], true, "{v}");
    assert_eq!(v["context"], "a", "{v}");
    ok(&run(
        home.path(),
        &["--context", "b", "whoami", "--json"],
        "",
        &[],
    ));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[2]), format!("Bearer {PAT_B}"));
}

#[test]
fn run_commands_use_the_context_login() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        reply(200, json!({"items": [], "next_cursor": null})),
    ]);
    ok(&run(
        home.path(),
        &["context", "add", "a", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["context", "add", "b", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "a", "login", "--token-stdin"],
        PAT_A,
        &[],
    ));
    // `b` has no login and no host login exists: no credentials.
    let o = run(
        home.path(),
        &[
            "--context",
            "b",
            "run",
            "list",
            "--project",
            "ops",
            "--json",
        ],
        "",
        &[],
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    ok(&run(
        home.path(),
        &[
            "--context",
            "a",
            "run",
            "list",
            "--project",
            "ops",
            "--json",
        ],
        "",
        &[],
    ));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[1]), format!("Bearer {PAT_A}"));
}

/// Review H1: `logout` under a context that has no login of its own, and
/// so uses the host's login, forgets that host login (and says so), rather
/// than reporting "not logged in" while later commands still use it.
#[test]
fn logout_under_a_context_without_its_own_login_forgets_the_host_login_it_uses() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs()]);
    let creds = home.path().join("runbook").join("credentials");
    std::fs::create_dir_all(&creds).unwrap();
    std::fs::write(
        creds.join("logins.json"),
        json!({"hosts": {&host: PAT_A}}).to_string(),
    )
    .unwrap();
    ok(&run(
        home.path(),
        &["context", "add", "work", "--host", &host],
        "",
        &[],
    ));
    ok(&run(home.path(), &["context", "use", "work"], "", &[]));
    // The context uses the host's login.
    ok(&run(home.path(), &["whoami", "--json"], "", &[]));
    let o = run(home.path(), &["logout", "--json"], "", &[]);
    ok(&o);
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["logged_out"], true, "{v}");
    assert_eq!(v["login"], "host", "{v}");
    assert_eq!(v["context"], "work", "{v}");
    assert!(logins(home.path())["hosts"].get(&host).is_none());
    let o = run(home.path(), &["logout"], "", &[]);
    ok(&o);
    assert!(out(&o).starts_with("not logged in"), "{}", out(&o));
    // Nothing is sent with the forgotten token.
    let o = run(home.path(), &["whoami", "--json"], "", &[]);
    assert_ne!(o.status.code(), Some(0), "{}", out(&o));
    let seen: Vec<String> = server
        .join()
        .unwrap()
        .into_iter()
        .filter(|r| !r.starts_with("GET /.well-known/"))
        .collect();
    assert_eq!(seen.len(), 1, "{seen:?}");
}

/// Review H1: the text output names the login that was removed.
#[test]
fn logout_says_which_login_it_removed() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs()]);
    let creds = home.path().join("runbook").join("credentials");
    std::fs::create_dir_all(&creds).unwrap();
    std::fs::write(
        creds.join("logins.json"),
        json!({"hosts": {&host: PAT_A}}).to_string(),
    )
    .unwrap();
    ok(&run(
        home.path(),
        &["context", "add", "a", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "a", "login", "--token-stdin"],
        PAT_B,
        &[],
    ));
    // The context's own login goes first; the host login stays.
    let o = run(home.path(), &["--context", "a", "logout"], "", &[]);
    ok(&o);
    assert!(out(&o).contains("context a"), "{}", out(&o));
    assert_eq!(logins(home.path())["hosts"][&host], PAT_A);
    // Then the host login it falls back to.
    let o = run(home.path(), &["--context", "a", "logout"], "", &[]);
    ok(&o);
    assert!(out(&o).contains("host login"), "{}", out(&o));
    assert!(logins(home.path())["hosts"].get(&host).is_none());
    server.join().unwrap();
}

/// Review (low): an explicit `--context` that names another host than
/// `--host` or `RUNBOOK_HOST`, or names no context, is an error rather
/// than silently ignored.
#[test]
fn an_explicit_context_must_exist_and_agree_with_the_host() {
    let home = tempfile::tempdir().unwrap();
    ok(&run(
        home.path(),
        &["context", "add", "a", "--host", "https://a.example"],
        "",
        &[],
    ));
    for (args, env) in [
        (
            vec![
                "--host",
                "https://b.example",
                "--context",
                "a",
                "whoami",
                "--json",
            ],
            vec![],
        ),
        (
            vec!["--context", "a", "whoami", "--json"],
            vec![("RUNBOOK_HOST", "https://b.example")],
        ),
        (
            vec![
                "--context",
                "a",
                "login",
                "https://b.example",
                "--token-stdin",
                "--json",
            ],
            vec![],
        ),
    ] {
        let o = run(home.path(), &args, PAT_A, &env);
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", err(&o));
        assert!(err(&o).contains("context 'a'"), "{args:?}: {}", err(&o));
    }
    let o = run(
        home.path(),
        &[
            "--host",
            "https://a.example",
            "--context",
            "nope",
            "whoami",
            "--json",
        ],
        "",
        &[],
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("unknown context"), "{}", err(&o));
    // The same host is fine (no request: there is no login, so whoami
    // stops before sending anything).
    let o = run(
        home.path(),
        &[
            "--host",
            "https://a.example/",
            "--context",
            "a",
            "logout",
            "--json",
        ],
        "",
        &[],
    );
    ok(&o);
}

/// Re-check N10: a context whose host was written by hand without the
/// normalized form (here a trailing slash) still applies its identity.
#[test]
fn a_hand_written_context_host_is_normalized_before_it_is_compared() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs()]);
    let dir = home.path().join("runbook");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!("current_context = \"bot\"\n\n[contexts.bot]\nhost = \"{host}/\"\n"),
    )
    .unwrap();
    let o = run(
        home.path(),
        &["login", "--token-stdin", "--json"],
        PAT_A,
        &[],
    );
    ok(&o);
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["context"], "bot", "{v}");
    let stored = logins(home.path());
    assert_eq!(stored["contexts"]["bot"]["host"], host.as_str(), "{stored}");
    assert!(stored["hosts"].get(&host).is_none(), "{stored}");
    let o = run(home.path(), &["context", "list", "--json"], "", &[]);
    ok(&o);
    let rows: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(rows[0]["logged_in"], true, "{rows}");
    server.join().unwrap();
}

/// A context's default project (`context add --default-project`) is what
/// commands use when they name none; `--project` still wins.
#[test]
fn a_contexts_default_project_fills_in_project() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(200, json!({"items": [], "next_cursor": null})),
        reply(200, json!({"items": [], "next_cursor": null})),
    ]);
    let o = run(
        home.path(),
        &[
            "context",
            "add",
            "work",
            "--host",
            &host,
            "--default-project",
            "pilot",
        ],
        "",
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        run(home.path(), &["context", "use", "work"], "", &[])
            .status
            .code(),
        Some(0)
    );
    let listed = run(home.path(), &["context", "list", "--json"], "", &[]);
    let rows: Value = serde_json::from_str(&out(&listed)).unwrap();
    assert_eq!(rows[0]["project"], "pilot");

    let token = [("RUNBOOK_TOKEN", PAT_A)];
    let o = run(home.path(), &["run", "list", "--json"], "", &token);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = run(
        home.path(),
        &["run", "list", "--project", "other", "--json"],
        "",
        &token,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(
        seen[0].starts_with("GET /v1/projects/pilot/executions"),
        "{}",
        seen[0]
    );
    assert!(
        seen[1].starts_with("GET /v1/projects/other/executions"),
        "{}",
        seen[1]
    );
}
