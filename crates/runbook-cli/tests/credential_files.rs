//! F66: an agent allowed only `runbook …` commands cannot read the CLI's
//! credential store through a file argument (`step report --log-file`,
//! `spec validate`, `push`): a path inside the store, or a link to a file in
//! it, is refused before anything is read or sent.
mod support;
use runbook_cli::credstore::{ExecutionCredential, StateDir, StepState};
use serde_json::json;
use std::path::Path;
use support::*;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const SECRET: &str = "trb_exe_access-store-secret";

fn store(home: &Path) -> StateDir {
    StateDir::at(home.join("runbook").join("credentials"))
}

/// A stored pair and claim; returns the pair file's path.
fn install(home: &Path, host: &str) -> std::path::PathBuf {
    let s = store(home);
    s.save_execution(&ExecutionCredential {
        host: host.into(),
        execution_id: EXE.into(),
        family_id: "fam_1".into(),
        generation: 1,
        access_token: SECRET.into(),
        refresh_token: "trb_exr_refresh-store-secret".into(),
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
    s.root().join("executions").join(format!("{EXE}.json"))
}

fn refused(o: &std::process::Output) {
    assert_eq!(o.status.code(), Some(2), "{}\n{}", out(o), err(o));
    assert!(err(o).contains("credential store"), "{}", err(o));
    assert!(!out(o).contains(SECRET) && !err(o).contains(SECRET));
}

#[test]
fn step_report_refuses_a_log_file_in_the_credential_store() {
    let home = tempfile::tempdir().unwrap();
    // No request may be made at all.
    let (host, server) = serve(vec![]);
    let pair = install(home.path(), &host);
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--log-file",
            pair.to_str().unwrap(),
        ],
        &[],
    );
    refused(&o);
    // Also through a relative path with `..` and through a symlink.
    let link = home.path().join("innocent.log");
    std::os::unix::fs::symlink(&pair, &link).unwrap();
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--log-file",
            link.to_str().unwrap(),
        ],
        &[],
    );
    refused(&o);
    let hard = home.path().join("hard.log");
    std::fs::hard_link(&pair, &hard).unwrap();
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--log-file",
            hard.to_str().unwrap(),
        ],
        &[],
    );
    refused(&o);
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn spec_validate_and_push_refuse_a_file_in_the_credential_store() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);
    let pair = install(home.path(), &host);
    let o = cli(
        &host,
        home.path(),
        &["spec", "validate", pair.to_str().unwrap()],
        &[],
    );
    refused(&o);
    let o = cli(
        &host,
        home.path(),
        &[
            "push",
            pair.to_str().unwrap(),
            "--draft",
            "--project",
            "default",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    refused(&o);
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn an_ordinary_log_file_is_still_attached() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        reply(200, json!({"status": "succeeded"})),
        reply(
            200,
            json!({"execution_id": EXE, "ready_work": [], "next_action": "wait"}),
        ),
    ]);
    install(home.path(), &host);
    let log = home.path().join("work.log");
    std::fs::write(&log, "checked\n").unwrap();
    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--log-file",
            log.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    // CF12b review round 2: a file that fits goes in the report.
    assert!(seen[0].contains("/report "), "{}", seen[0]);
    assert_eq!(body_of(&seen[0])["logs"], json!(["checked"]));
}

#[test]
fn step_evidence_and_report_refuse_file_in_credential_store() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);
    let pair = install(home.path(), &host);

    let o = cli(
        &host,
        home.path(),
        &["step", "evidence", SR, pair.to_str().unwrap()],
        &[],
    );
    refused(&o);

    let o = cli(
        &host,
        home.path(),
        &[
            "step",
            "report",
            SR,
            "--status",
            "succeeded",
            "--evidence",
            pair.to_str().unwrap(),
        ],
        &[],
    );
    refused(&o);
    assert!(server.join().unwrap().is_empty());
}
