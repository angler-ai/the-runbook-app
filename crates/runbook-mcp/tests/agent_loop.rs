//! The agent loop the MCP server teaches, driven against a fake that keeps
//! the server's rules: D32 (an execution credential reads only the step it
//! currently claims) and run closure (the final report ends the run and the
//! credential family is revoked, so the brief answers 401
//! `credentials.revoked`). The end-to-end version against the real server
//! is `runbook-cli/tests/agent_loop_e2e.rs`.
use runbook_mcp::backend::{Backend, BackendError, HttpCall, HttpReply};
use runbook_mcp::{call_tool, handle_message};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const EXE: &str = "exe_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SR: &str = "sr_01JABCDEFGHJKMNPQRSTVWXYZ1";
const CLAIM: &str = "clm_01JABCDEFGHJKMNPQRSTVWXYZ3";

/// One agent step in one run, under D32 and run closure.
#[derive(Default)]
struct Server {
    claimed: bool,
    reported: bool,
    /// The brief fails with this status after the report (the run closed).
    revoked: bool,
    /// Answer the brief after the report with this transient failure.
    brief_failure: Option<BackendError>,
    remembered: Vec<(String, String)>,
    finished: BTreeSet<String>,
    calls: Vec<String>,
}

fn reply(body: Value) -> Result<HttpReply, BackendError> {
    Ok(HttpReply { body, etag: None })
}

impl Backend for Server {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        self.calls.push(format!("{} {}", call.method, call.path));
        if self.revoked {
            return Err(BackendError::problem(401, "credentials.revoked", ""));
        }
        let step = format!("/v1/step-runs/{SR}");
        match (call.method, call.path.as_str()) {
            ("GET", p) if p == format!("/v1/executions/{EXE}/next") => {
                if self.reported
                    && let Some(e) = self.brief_failure.take()
                {
                    return Err(e);
                }
                let ready = if self.reported || self.claimed {
                    json!([])
                } else {
                    json!([{"step_run_id": SR, "path": "check", "title": "Check", "actor_kind": "agent"}])
                };
                let next = if self.reported || self.claimed {
                    "wait"
                } else {
                    "claim_ready_work"
                };
                reply(
                    json!({"execution_id": EXE, "completed_steps": [], "decisions": [], "comments": [],
                             "ready_work": ready, "next_action": next}),
                )
            }
            ("GET", p) if p == step => {
                if !self.claimed {
                    return Err(BackendError::problem(403, "auth.forbidden", ""));
                }
                reply(
                    json!({"step_run_id": SR, "instructions": "Check the service.", "checklist": [],
                             "parameters": {}, "output_schema": {"type": "object"}, "skill": null}),
                )
            }
            ("POST", p) if p == format!("{step}/claim") => {
                self.claimed = true;
                reply(json!({"id": CLAIM, "generation": 1}))
            }
            ("POST", p) if p == format!("{step}/report") => {
                assert!(self.claimed, "report without a claim");
                self.claimed = false;
                self.reported = true;
                reply(json!({"status": "succeeded"}))
            }
            other => panic!("unexpected call {other:?}"),
        }
    }
    fn random_token(&mut self) -> Result<String, BackendError> {
        Ok("nonce".into())
    }
    fn remember_step(&mut self, sr: &str, exe: &str) {
        self.remembered.push((sr.into(), exe.into()));
    }
    fn execution_of_step(&mut self, sr: &str) -> Option<String> {
        self.remembered
            .iter()
            .find(|(s, _)| s == sr)
            .map(|(_, e)| e.clone())
    }
    fn note_finished(&mut self, exe: &str, finished: bool) {
        if finished {
            self.finished.insert(exe.into());
        } else {
            self.finished.remove(exe);
        }
    }
    fn finished(&mut self, exe: &str) -> bool {
        self.finished.contains(exe)
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap().to_owned()
}
fn control(result: &Value) -> Value {
    serde_json::from_str(text(result).lines().next().unwrap()).unwrap()
}
fn is_error(result: &Value) -> bool {
    result["isError"] == json!(true)
}

fn instructions() -> String {
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}});
    let mut b = Server::default();
    let r = handle_message(&mut b, &init).unwrap();
    r["result"]["instructions"].as_str().unwrap().to_owned()
}

/// The tools of the taught loop, in the order the instructions give them.
fn taught_loop() -> Vec<String> {
    let names: BTreeSet<String> = runbook_mcp::tool_definitions()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let text = instructions();
    let start = text.find("Loop:").expect("the instructions teach a loop");
    let rest = &text[start + "Loop:".len()..];
    let end = rest.find(';').unwrap_or(rest.len());
    rest[..end]
        .split("->")
        .filter_map(|stage| {
            let word = stage.split_whitespace().next()?;
            names.contains(word).then(|| word.to_owned())
        })
        .collect()
}

fn args(tool: &str, claim: &Value) -> Value {
    match tool {
        "get_next_steps" => json!({"execution_id": EXE}),
        "get_step" => json!({"step_run_id": SR}),
        "claim_step" => {
            json!({"step_run_id": SR, "session_id": "s1", "agent_name": "agent", "model": "m"})
        }
        "report_step" => json!({"step_run_id": SR, "claim": claim, "status": "succeeded"}),
        other => panic!("the loop names {other}, which this test does not drive"),
    }
}

/// F60: the loop the MCP instructions teach works for an execution
/// credential (D32 kept: reading an unclaimed step is refused).
#[test]
fn the_taught_loop_works_under_d32() {
    let order = taught_loop();
    assert!(
        order.iter().any(|t| t == "get_step") && order.iter().any(|t| t == "claim_step"),
        "{order:?}"
    );
    let mut b = Server::default();
    let mut claim = Value::Null;
    for tool in &order {
        let r = call_tool(&mut b, tool, &args(tool, &claim));
        assert!(
            !is_error(&r),
            "{tool} failed in the taught loop {order:?}: {}",
            text(&r)
        );
        if tool == "claim_step" {
            claim = control(&r)["claim"].clone();
        }
    }
    assert!(b.reported, "{:?}", b.calls);
}

/// F60: the hint that comes with claim_ready_work names claim_step before
/// get_step, and a step read after the claim is work in progress.
#[test]
fn hints_say_claim_then_get_and_a_claimed_step_is_not_ready_work() {
    let mut b = Server::default();
    let r = call_tool(&mut b, "get_next_steps", &json!({"execution_id": EXE}));
    let hint = control(&r)["hint"].as_str().unwrap().to_owned();
    let (claim, get) = (hint.find("claim_step"), hint.find("get_step"));
    assert!(
        matches!((claim, get), (Some(c), Some(g)) if c < g),
        "{hint}"
    );
    let r = call_tool(&mut b, "claim_step", &args("claim_step", &Value::Null));
    assert!(!is_error(&r), "{}", text(&r));
    let r = call_tool(&mut b, "get_step", &args("get_step", &Value::Null));
    assert!(!is_error(&r), "{}", text(&r));
    assert_ne!(
        control(&r)["next_action"],
        "claim_ready_work",
        "{}",
        text(&r)
    );
}

/// F60: a get_step refused because the step is not claimed says to claim it.
#[test]
fn get_step_before_claim_says_to_claim_first() {
    let mut b = Server::default();
    let r = call_tool(&mut b, "get_step", &args("get_step", &Value::Null));
    assert!(is_error(&r));
    let c = control(&r);
    assert_eq!(c["next_action"], "claim_ready_work", "{}", text(&r));
    assert!(c["hint"].as_str().unwrap().contains("claim_step"), "{c}");
}

fn claimed() -> (Server, Value) {
    let mut b = Server::default();
    call_tool(&mut b, "get_next_steps", &json!({"execution_id": EXE}));
    let r = call_tool(&mut b, "claim_step", &args("claim_step", &Value::Null));
    let claim = control(&r)["claim"].clone();
    (b, claim)
}

/// F61: the final report closes the run and the family is revoked; the
/// report is still a success that says exit, and so is a later brief.
#[test]
fn revocation_after_the_final_report_is_a_clean_exit() {
    let (mut b, claim) = claimed();
    let r = call_tool(
        &mut Revoking(&mut b),
        "report_step",
        &args("report_step", &claim),
    );
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(control(&r)["next_action"], "exit", "{}", text(&r));
    let r = call_tool(&mut b, "get_next_steps", &json!({"execution_id": EXE}));
    assert!(!is_error(&r), "{}", text(&r));
    assert_eq!(control(&r)["next_action"], "exit", "{}", text(&r));
}

/// A new claim means the agent is working again: a later revocation is not
/// "after its own final report".
#[test]
fn a_claim_clears_the_finished_state() {
    let mut b = Server::default();
    b.finished.insert(EXE.into());
    call_tool(&mut b, "get_next_steps", &json!({"execution_id": EXE}));
    let r = call_tool(&mut b, "claim_step", &args("claim_step", &Value::Null));
    assert!(!is_error(&r), "{}", text(&r));
    assert!(b.finished.is_empty());
}

/// Revocation the agent did not cause by its own report stays an error.
#[test]
fn revocation_without_an_own_report_is_not_an_exit() {
    let mut b = Server {
        revoked: true,
        ..Default::default()
    };
    let r = call_tool(&mut b, "get_next_steps", &json!({"execution_id": EXE}));
    assert!(is_error(&r), "{}", text(&r));
    assert_ne!(control(&r)["next_action"], "exit");
}

/// A report that succeeded is not turned into an error by a transient
/// failure of the brief that follows it.
#[test]
fn a_transient_brief_failure_after_a_report_is_not_a_tool_error() {
    let (mut b, claim) = claimed();
    b.brief_failure = Some(BackendError::problem(503, "server.unavailable", ""));
    let r = call_tool(&mut b, "report_step", &args("report_step", &claim));
    assert!(!is_error(&r), "{}", text(&r));
    let c = control(&r);
    assert!(c["next_action"].is_null(), "{c}");
    assert!(
        c["hint"].as_str().unwrap().contains("get_next_steps"),
        "{c}"
    );
}

/// Wraps the server so every call after the report write is revoked.
struct Revoking<'a>(&'a mut Server);

impl Backend for Revoking<'_> {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        let reported = self.0.reported;
        let result = self.0.call(call);
        if !reported && self.0.reported {
            // The report itself committed and closed the run.
            self.0.revoked = true;
        }
        result
    }
    fn random_token(&mut self) -> Result<String, BackendError> {
        self.0.random_token()
    }
    fn remember_step(&mut self, sr: &str, exe: &str) {
        self.0.remember_step(sr, exe)
    }
    fn execution_of_step(&mut self, sr: &str) -> Option<String> {
        self.0.execution_of_step(sr)
    }
    fn note_finished(&mut self, exe: &str, finished: bool) {
        self.0.note_finished(exe, finished)
    }
    fn finished(&mut self, exe: &str) -> bool {
        self.0.finished(exe)
    }
}
