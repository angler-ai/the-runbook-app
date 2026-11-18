//! CLI3: logins in the OS keychain, with the 0600 file as the fallback.
//! Debug builds replace the OS keychain with a directory
//! (`RUNBOOK_TEST_KEYCHAIN_DIR`), so these tests never touch the real one.
mod support;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use support::*;

const PAT_A: &str = "trb_pat_alpha-secret";
const PAT_B: &str = "trb_pat_bravo-secret";

fn run(home: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .args(args)
        .env_remove("RUNBOOK_TOKEN")
        .env_remove("RUNBOOK_HOST")
        .env_remove("RUNBOOK_CREDENTIAL_STORE")
        .env_remove("RUNBOOK_TEST_KEYCHAIN_FAIL")
        .env("RUNBOOK_TEST_KEYCHAIN_DIR", home.join("keychain"))
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

fn json_of(o: &Output) -> Value {
    serde_json::from_str(&out(o)).unwrap_or_else(|_| panic!("{}\n{}", out(o), err(o)))
}

/// Both login files: `logins.json` (pre-CLI3 shape) and `logins.v2.json`.
fn logins_text(home: &Path) -> String {
    let dir = home.join("runbook").join("credentials");
    ["logins.json", "logins.v2.json"]
        .iter()
        .map(|name| std::fs::read_to_string(dir.join(name)).unwrap_or_default())
        .collect::<Vec<_>>()
        .concat()
}

/// Every secret the fake keychain holds.
fn keychain_secrets(home: &Path) -> Vec<String> {
    let mut secrets: Vec<String> = std::fs::read_dir(home.join("keychain"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    secrets.sort();
    secrets
}

fn bearer(request: &str) -> String {
    header(request, "authorization").unwrap_or("").to_string()
}

fn orgs() -> Reply {
    reply(200, json!({"items": [], "next_cursor": null}))
}

#[test]
fn login_stores_the_token_in_the_keychain_not_the_file() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs()]);
    let o = run(
        home.path(),
        &["--host", &host, "login", "--token-stdin", "--json"],
        PAT_A,
        &[],
    );
    ok(&o);
    assert_eq!(json_of(&o)["storage"], "keychain");
    assert!(!logins_text(home.path()).contains(PAT_A));
    assert_eq!(keychain_secrets(home.path()), vec![PAT_A.to_string()]);
    ok(&run(
        home.path(),
        &["--host", &host, "whoami", "--json"],
        "",
        &[],
    ));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[1]), format!("Bearer {PAT_A}"));
}

#[test]
fn a_keychain_that_refuses_the_write_falls_back_to_the_file() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs()]);
    let o = run(
        home.path(),
        &["--host", &host, "login", "--token-stdin", "--json"],
        PAT_A,
        &[("RUNBOOK_TEST_KEYCHAIN_FAIL", "1")],
    );
    ok(&o);
    assert_eq!(json_of(&o)["storage"], "file");
    assert!(err(&o).contains("keychain"), "{}", err(&o));
    assert!(logins_text(home.path()).contains(PAT_A));
    assert!(keychain_secrets(home.path()).is_empty());
    ok(&run(
        home.path(),
        &["--host", &host, "whoami", "--json"],
        "",
        &[],
    ));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[1]), format!("Bearer {PAT_A}"));
}

#[test]
fn file_logins_keep_working_and_migrate_to_the_keychain() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs(), orgs(), orgs()]);
    let file = [("RUNBOOK_CREDENTIAL_STORE", "file")];
    ok(&run(
        home.path(),
        &["context", "add", "bot", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--host", &host, "login", "--token-stdin"],
        PAT_A,
        &file,
    ));
    ok(&run(
        home.path(),
        &["--context", "bot", "login", "--token-stdin"],
        PAT_B,
        &file,
    ));
    assert!(keychain_secrets(home.path()).is_empty());

    let o = run(
        home.path(),
        &["login", "--migrate-to-keychain", "--json"],
        "",
        &[],
    );
    ok(&o);
    assert_eq!(json_of(&o)["migrated"], 2, "{}", out(&o));
    let text = logins_text(home.path());
    assert!(!text.contains(PAT_A) && !text.contains(PAT_B), "{text}");
    assert_eq!(
        keychain_secrets(home.path()),
        vec![PAT_A.to_string(), PAT_B.to_string()]
    );
    // Both identities still work, now read from the keychain.
    ok(&run(
        home.path(),
        &["--host", &host, "whoami", "--json"],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "bot", "whoami", "--json"],
        "",
        &[],
    ));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[2]), format!("Bearer {PAT_A}"));
    assert_eq!(bearer(&seen[3]), format!("Bearer {PAT_B}"));

    // Nothing left to migrate.
    let o = run(
        home.path(),
        &["login", "--migrate-to-keychain", "--json"],
        "",
        &[],
    );
    ok(&o);
    assert_eq!(json_of(&o)["migrated"], 0);
}

#[test]
fn migrating_without_a_keychain_is_an_error_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let creds = home.path().join("runbook").join("credentials");
    std::fs::create_dir_all(&creds).unwrap();
    let before = json!({"hosts": {"https://h.example": PAT_A}}).to_string();
    std::fs::write(creds.join("logins.json"), &before).unwrap();
    let o = run(
        home.path(),
        &["login", "--migrate-to-keychain"],
        "",
        &[("RUNBOOK_CREDENTIAL_STORE", "file")],
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("keychain"), "{}", err(&o));
    assert_eq!(logins_text(home.path()), before);
}

#[test]
fn logout_and_context_remove_delete_the_keychain_entry() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs()]);
    ok(&run(
        home.path(),
        &["context", "add", "bot", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--host", &host, "login", "--token-stdin"],
        PAT_A,
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "bot", "login", "--token-stdin"],
        PAT_B,
        &[],
    ));
    assert_eq!(keychain_secrets(home.path()).len(), 2);
    ok(&run(home.path(), &["--host", &host, "logout"], "", &[]));
    assert_eq!(keychain_secrets(home.path()), vec![PAT_B.to_string()]);
    ok(&run(home.path(), &["context", "remove", "bot"], "", &[]));
    assert!(keychain_secrets(home.path()).is_empty());
    server.join().unwrap();
}

#[test]
fn the_file_store_can_be_forced() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs()]);
    let o = run(
        home.path(),
        &["--host", &host, "login", "--token-stdin", "--json"],
        PAT_A,
        &[("RUNBOOK_CREDENTIAL_STORE", "file")],
    );
    ok(&o);
    assert_eq!(json_of(&o)["storage"], "file");
    assert!(keychain_secrets(home.path()).is_empty());
    server.join().unwrap();
}

/// Review H2: a CLI older than CLI3 rewrites `logins.json` with only the
/// host logins it knows. CLI3's records (context logins, keychain markers)
/// are in `logins.v2.json`, so such a rewrite loses none of them.
#[test]
fn an_older_cli_rewriting_logins_json_loses_nothing_cli3_stored() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs(), orgs(), orgs(), orgs()]);
    let other = "http://127.0.0.1:9";
    ok(&run(
        home.path(),
        &["context", "add", "bot", "--host", &host],
        "",
        &[],
    ));
    // A host login in the keychain, a context login in the file, and a
    // file host login for another host.
    ok(&run(
        home.path(),
        &["--host", &host, "login", "--token-stdin"],
        PAT_A,
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "bot", "login", "--token-stdin"],
        PAT_B,
        &[("RUNBOOK_CREDENTIAL_STORE", "file")],
    ));
    let dir = home.path().join("runbook").join("credentials");
    let legacy = "trb_pat_legacy-secret";
    let mut v1: Value =
        serde_json::from_slice(&std::fs::read(dir.join("logins.json")).unwrap_or(b"{}".to_vec()))
            .unwrap();
    // logins.json keeps exactly the pre-CLI3 shape.
    assert!(v1.as_object().unwrap().keys().all(|k| k == "hosts"), "{v1}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("logins.v2.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The older CLI: read `{hosts}`, add a login, write `{hosts}` back.
    v1["hosts"][other] = json!(legacy);
    std::fs::write(
        dir.join("logins.json"),
        json!({"hosts": v1["hosts"]}).to_string(),
    )
    .unwrap();

    ok(&run(
        home.path(),
        &["--host", &host, "whoami", "--json"],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "bot", "whoami", "--json"],
        "",
        &[],
    ));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[2]), format!("Bearer {PAT_A}"));
    assert_eq!(bearer(&seen[3]), format!("Bearer {PAT_B}"));
    // The older CLI's own login is kept by CLI3's next write.
    ok(&run(home.path(), &["--host", &host, "logout"], "", &[]));
    assert!(logins_text(home.path()).contains(legacy));
}

/// Review (low): `logout` through a context whose record was left for
/// another host (its config was edited) still deletes that record's
/// keychain entry.
#[test]
fn a_stale_context_record_still_loses_its_keychain_entry() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs()]);
    ok(&run(
        home.path(),
        &["context", "add", "bot", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "bot", "login", "--token-stdin"],
        PAT_B,
        &[],
    ));
    assert_eq!(keychain_secrets(home.path()), vec![PAT_B.to_string()]);
    let config = home.path().join("runbook").join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, text.replace(&host, "https://moved.example")).unwrap();
    ok(&run(home.path(), &["--context", "bot", "logout"], "", &[]));
    assert!(keychain_secrets(home.path()).is_empty());
    assert!(!logins_text(home.path()).contains("bot"));
    server.join().unwrap();
}

/// Re-check N5: an older CLI's login over a keychain login leaves a file
/// token beside the keychain marker. The file token wins, and `logout`
/// removes both, the keychain entry included.
#[test]
fn logout_removes_an_older_cli_login_and_the_keychain_login_it_shadowed() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs()]);
    ok(&run(
        home.path(),
        &["--host", &host, "login", "--token-stdin"],
        PAT_A,
        &[],
    ));
    let legacy = "trb_pat_legacy-secret";
    let dir = home.path().join("runbook").join("credentials");
    std::fs::write(
        dir.join("logins.json"),
        json!({"hosts": {host.as_str(): legacy}}).to_string(),
    )
    .unwrap();
    ok(&run(
        home.path(),
        &["--host", &host, "whoami", "--json"],
        "",
        &[],
    ));
    ok(&run(home.path(), &["--host", &host, "logout"], "", &[]));
    assert!(keychain_secrets(home.path()).is_empty());
    assert!(!logins_text(home.path()).contains(legacy));
    assert!(!logins_text(home.path()).contains(&host));
    let seen = server.join().unwrap();
    assert_eq!(bearer(&seen[1]), format!("Bearer {legacy}"));
}

/// Re-check N5: the next write by this CLI drops the shadowed keychain
/// login, so an older CLI's logout afterwards does not bring it back.
#[test]
fn an_older_cli_logout_does_not_revive_a_shadowed_keychain_login() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![orgs(), orgs()]);
    ok(&run(
        home.path(),
        &["--host", &host, "login", "--token-stdin"],
        PAT_A,
        &[],
    ));
    let dir = home.path().join("runbook").join("credentials");
    std::fs::write(
        dir.join("logins.json"),
        json!({"hosts": {host.as_str(): "trb_pat_legacy-secret"}}).to_string(),
    )
    .unwrap();
    // Any login write by this CLI.
    ok(&run(
        home.path(),
        &["context", "add", "bot", "--host", &host],
        "",
        &[],
    ));
    ok(&run(
        home.path(),
        &["--context", "bot", "login", "--token-stdin"],
        PAT_B,
        &[],
    ));
    assert_eq!(keychain_secrets(home.path()), vec![PAT_B.to_string()]);
    // The older CLI logs out.
    std::fs::write(dir.join("logins.json"), json!({"hosts": {}}).to_string()).unwrap();
    let o = run(home.path(), &["--host", &host, "whoami", "--json"], "", &[]);
    assert_ne!(o.status.code(), Some(0), "{}", out(&o));
    // Only the two logins' verifications: `whoami` found no login to send.
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2, "{seen:#?}");
}
