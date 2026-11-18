//! F67: under `--json`, `step` and `run` commands that fail on an API or
//! network error still print one JSON document on stdout (the error and a
//! `next_action`), so an agent parsing stdout is never left with nothing.
mod support;
use runbook_cli::credstore::{ExecutionCredential, StateDir, StepState};
use serde_json::{Value, json};
use std::path::Path;
use support::*;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";

fn install(home: &Path, host: &str) {
    let s = StateDir::at(home.join("runbook").join("credentials"));
    s.save_execution(&ExecutionCredential {
        host: host.into(),
        execution_id: EXE.into(),
        family_id: "fam_1".into(),
        generation: 1,
        access_token: "trb_exe_a".into(),
        refresh_token: "trb_exr_r".into(),
        expires_at_ms: i64::MAX,
        refresh_expires_at_ms: i64::MAX,
    })
    .unwrap();
    s.set_active(host, EXE).unwrap();
    s.save_step(
        SR,
        &StepState {
            host: host.into(),
            execution_id: Some(EXE.into()),
            claim: Some(json!({"id": "clm_01JABCDEFGHJKMNPQRSTVWXYZ3", "generation": 1})),
        },
    )
    .unwrap();
}

fn stdout_json(o: &std::process::Output) -> Value {
    serde_json::from_str(&out(o))
        .unwrap_or_else(|_| panic!("no JSON on stdout: {:?}\nstderr: {}", out(o), err(o)))
}

#[test]
fn a_lost_claim_prints_json_saying_reconcile() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![problem(409, "claim.stale_generation")]);
    install(home.path(), &host);
    let o = cli(
        &host,
        home.path(),
        &["step", "report", SR, "--status", "succeeded", "--json"],
        &[],
    );
    assert_eq!(o.status.code(), Some(1));
    let v = stdout_json(&o);
    assert_eq!(v["error"]["code"], "claim.stale_generation");
    assert_eq!(v["error"]["status"], 409);
    assert_eq!(v["next_action"], "reconcile");
    server.join().unwrap();
}

#[test]
fn step_next_network_errors_print_json_saying_retry() {
    let home = tempfile::tempdir().unwrap();
    // Nothing listens on this port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let host = format!("http://127.0.0.1:{port}");
    install(home.path(), &host);
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    assert_eq!(o.status.code(), Some(1));
    let v = stdout_json(&o);
    assert_eq!(v["next_action"], "retry", "{v}");
    assert!(v["error"]["message"].as_str().is_some());
}

#[test]
fn run_commands_print_json_errors() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        problem(404, "execution.not_found"),
        problem(429, "rate_limited"),
    ]);
    let token = [("RUNBOOK_TOKEN", PAT)];
    let o = cli(&host, home.path(), &["run", "get", EXE, "--json"], &token);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout_json(&o)["error"]["code"], "execution.not_found");
    let o = cli(
        &host,
        home.path(),
        &["run", "list", "--project", "default", "--json"],
        &token,
    );
    assert_eq!(o.status.code(), Some(4));
    let v = stdout_json(&o);
    assert_eq!(v["error"]["status"], 429);
    assert_eq!(v["next_action"], "wait");
    server.join().unwrap();
}

#[test]
fn without_json_nothing_is_printed_on_stdout() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![problem(404, "execution.not_found")]);
    let o = cli(
        &host,
        home.path(),
        &["run", "get", EXE],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).is_empty());
    server.join().unwrap();
}
