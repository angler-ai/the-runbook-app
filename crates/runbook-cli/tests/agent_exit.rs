//! F61 / Gate D: the final report ends the run and the server revokes the
//! execution pair, so the brief answers 401 `credentials.revoked`. After the
//! agent's own final write that is a clean exit (`next_action: exit`, exit
//! 0), from the report and from a later `step next`; a revocation the agent
//! did not cause by its own report stays an error. Scripted fake API; the
//! real-server version is `agent_loop_e2e.rs`.
mod support;
use runbook_cli::credstore::{ExecutionCredential, StateDir, StepState};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use support::*;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const CLAIM: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn store(home: &Path) -> StateDir {
    StateDir::at(home.join("runbook").join("credentials"))
}

/// A stored pair and a claimed step, as after `run token` and `step claim`.
fn claimed(home: &Path, host: &str) {
    let s = store(home);
    s.save_execution(&ExecutionCredential {
        host: host.into(),
        execution_id: EXE.into(),
        family_id: "fam_1".into(),
        generation: 1,
        access_token: "trb_exe_access-secret".into(),
        refresh_token: "trb_exr_refresh-secret".into(),
        expires_at_ms: now_ms() + 3_600_000,
        refresh_expires_at_ms: now_ms() + 86_400_000,
    })
    .unwrap();
    s.set_active(host, EXE).unwrap();
    s.save_step(
        SR,
        &StepState {
            host: host.into(),
            execution_id: Some(EXE.into()),
            claim: Some(json!({"id": CLAIM, "generation": 1})),
        },
    )
    .unwrap();
}

fn json_out(o: &std::process::Output) -> Value {
    serde_json::from_str(&out(o)).unwrap_or_else(|_| panic!("{}\n{}", out(o), err(o)))
}

#[test]
fn the_final_report_with_a_revoked_pair_is_a_clean_exit() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(200, json!({"status": "succeeded"})),
        problem(401, "credentials.revoked"),
    ]);
    claimed(home.path(), &host);
    let o = cli(
        &host,
        home.path(),
        &["step", "report", SR, "--status", "succeeded", "--json"],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(json_out(&o)["next_action"], "exit");
    assert!(!err(&o).contains("credentials.revoked"), "{}", err(&o));

    // The revoked pair was forgotten (F65); a later `step next` still knows
    // the agent's work ended and says exit without calling the server.
    assert!(store(home.path()).load_execution(EXE).unwrap().is_none());
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(json_out(&o)["next_action"], "exit");
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2, "{seen:?}");
}

#[test]
fn step_next_with_a_still_stored_but_revoked_pair_after_the_final_report_exits() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![problem(401, "credentials.revoked")]);
    claimed(home.path(), &host);
    store(home.path()).set_finished(&host, EXE, true).unwrap();
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(json_out(&o)["next_action"], "exit");
    server.join().unwrap();
}

#[test]
fn step_next_without_an_own_final_report_still_fails_on_revocation() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![problem(401, "credentials.revoked")]);
    claimed(home.path(), &host);
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    assert_ne!(o.status.code(), Some(0), "{}", out(&o));
    assert!(err(&o).contains("credentials.revoked"), "{}", err(&o));
    server.join().unwrap();
}

#[test]
fn a_new_claim_ends_the_finished_state() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(200, json!({"status": "succeeded"})),
        problem(401, "credentials.revoked"),
        reply(200, json!({"id": CLAIM, "generation": 2})),
        problem(401, "credentials.revoked"),
    ]);
    claimed(home.path(), &host);
    let o = cli(
        &host,
        home.path(),
        &["step", "report", SR, "--status", "succeeded", "--json"],
        &[],
    );
    assert_eq!(json_out(&o)["next_action"], "exit");
    // Working again (a pair is installed and the agent claims): a
    // revocation now is not the end of this agent's work.
    claimed(home.path(), &host);
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "claim",
            SR,
            "--session",
            "s1",
            "--agent-name",
            "a",
            "--json",
        ],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(&host, home.path(), &["step", "next", EXE, "--json"], &[]);
    assert_ne!(o.status.code(), Some(0), "{}", out(&o));
    server.join().unwrap();
}

#[test]
fn a_transient_brief_failure_after_a_report_keeps_the_report_a_success() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(200, json!({"status": "succeeded"})),
        problem(503, "server.unavailable"),
    ]);
    claimed(home.path(), &host);
    let o = cli(
        &host,
        home.path(),
        &["step", "report", SR, "--status", "succeeded", "--json"],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(json_out(&o)["next_action"].is_null());
    assert!(err(&o).contains("step next"), "{}", err(&o));
    server.join().unwrap();
}
