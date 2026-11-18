//! F65: a refresh refused with `credentials.forbidden` (D31: the issuer
//! lost the right, so the family is refused from then on) is terminal, and
//! a plain 401 `credentials.revoked` forgets the stored pair (only the same
//! family). F70: the store never uses a fixed temp-file name, and
//! `run token` saves under the execution's refresh lock.
mod support;
use runbook_cli::credstore::{ExecutionCredential, StateDir};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use support::*;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn store(home: &Path) -> StateDir {
    StateDir::at(home.join("runbook").join("credentials"))
}

fn credential(host: &str, family: &str, expires_at_ms: i64) -> ExecutionCredential {
    ExecutionCredential {
        host: host.into(),
        execution_id: EXE.into(),
        family_id: family.into(),
        generation: 1,
        access_token: format!("trb_exe_access-{family}"),
        refresh_token: format!("trb_exr_refresh-{family}"),
        expires_at_ms,
        refresh_expires_at_ms: now_ms() + 86_400_000,
    }
}

fn install(home: &Path, host: &str, expires_at_ms: i64) {
    let s = store(home);
    s.save_execution(&credential(host, "fam_1", expires_at_ms))
        .unwrap();
    s.set_active(host, EXE).unwrap();
}

#[test]
fn a_forbidden_refresh_is_terminal_and_forgets_the_pair() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![problem(403, "credentials.forbidden")]);
    // Expiring: the CLI refreshes before the request.
    install(home.path(), &host, now_ms() + 1_000);
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["next_action"], "refresh_credentials", "{v}");
    assert!(store(home.path()).load_execution(EXE).unwrap().is_none());
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn a_revoked_access_token_forgets_the_stored_pair() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![problem(401, "credentials.revoked")]);
    install(home.path(), &host, now_ms() + 3_600_000);
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("credentials.revoked"), "{}", err(&o));
    assert!(store(home.path()).load_execution(EXE).unwrap().is_none());
    assert!(store(home.path()).active().unwrap().is_none());
    server.join().unwrap();
}

#[test]
fn a_revocation_never_forgets_a_newer_family() {
    let home = tempfile::tempdir().unwrap();
    // The request is slow; meanwhile a launcher installs a new family.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let host = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    install(home.path(), &host, now_ms() + 3_600_000);
    let s = store(home.path());
    let newer = credential(&host, "fam_2", now_ms() + 3_600_000);
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        s.save_execution(&newer).unwrap();
        let body = r#"{"code":"credentials.revoked"}"#;
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 401 X\r\nContent-Type: application/problem+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    });
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    server.join().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let kept = store(home.path()).load_execution(EXE).unwrap().unwrap();
    assert_eq!(kept.family_id, "fam_2");
}

#[test]
fn a_leftover_temp_file_never_blocks_or_clobbers_a_save() {
    let home = tempfile::tempdir().unwrap();
    let s = store(home.path());
    s.save_execution(&credential("http://h", "fam_1", 1))
        .unwrap();
    // Whatever a crashed or concurrent writer left at the old fixed name.
    let dir = home.path().join("runbook/credentials/executions");
    std::fs::create_dir(dir.join(format!("{EXE}.tmp"))).unwrap();
    s.save_execution(&credential("http://h", "fam_2", 2))
        .unwrap();
    assert_eq!(s.load_execution(EXE).unwrap().unwrap().family_id, "fam_2");
    s.set_finished("http://h", EXE, true).unwrap();
    assert!(s.finished("http://h", EXE));
    assert_eq!(s.load_execution(EXE).unwrap().unwrap().family_id, "fam_2");
}

#[test]
fn run_token_saves_under_the_refresh_lock() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![reply(
        201,
        json!({"family_id": "fam_9", "generation": 1, "access_token": "trb_exe_a", "refresh_token": "trb_exr_r",
               "expires_at_ms": now_ms() + 3_600_000, "refresh_expires_at_ms": now_ms() + 86_400_000}),
    )]);
    let s = store(home.path());
    let guard = s.lock(EXE, Duration::from_secs(1)).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_runbook"))
        .args(["--host", &host, "run", "token", EXE, "--session", "s1"])
        .env_remove("RUNBOOK_HOST")
        .env("RUNBOOK_TOKEN", "trb_svc_launcher")
        .env("XDG_CONFIG_HOME", home.path())
        .env("HOME", home.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        s.load_execution(EXE).unwrap().is_none(),
        "run token wrote the pair while a refresh held the lock"
    );
    drop(guard);
    assert!(child.wait().unwrap().success());
    assert_eq!(s.load_execution(EXE).unwrap().unwrap().family_id, "fam_9");
    server.join().unwrap();
}
