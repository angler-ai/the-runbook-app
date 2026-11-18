//! The flagship post-deploy runbook checked against hand-written expectations
//! taken from the PRD (section 3 "Flagship flow", section 5 "Full example",
//! "Checks and verdicts", "Approval timeout and destructive-action policy",
//! section 6 "Rules" and "Deadlines, cleanup and pauses").
//!
//! Every expected line below was written from the PRD text, not captured
//! from engine output. `flagship.rs` keeps the byte-level fixtures; this file
//! is the independent check that those fixtures describe the PRD's run, and it
//! also reads the checked-in conformance trace so a regenerated fixture that
//! drifts from the PRD fails here.
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, ExecutionStatus, ReportStatus, StepRunId, StepStatus, Verdict};
use serde_json::{Value, json};
use support::*;

const MIN: i64 = 60_000;

fn human(n: u32) -> ActorRef {
    ActorRef::Human {
        user_id: format!("usr_{n:026}").parse().unwrap(),
    }
}
fn agent() -> ActorRef {
    ActorRef::Agent {
        principal_id: "service-checker".into(),
        session_id: "session-current".into(),
        name: "fixture-agent".into(),
        model: None,
    }
}

/// One line per event: the facts the PRD talks about (which step, which
/// status and verdict, who acted, which branch, which decision).
fn project(trace: &Trace) -> Vec<String> {
    let path = |id: &StepRunId| -> String {
        trace
            .state
            .steps
            .iter()
            .find(|s| s.id.as_ref() == Some(id))
            .map_or_else(|| format!("?{id}"), |s| s.step_id.clone())
    };
    let who = |actor: &ActorRef| match actor {
        ActorRef::Human { .. } => "human",
        ActorRef::Agent { .. } => "agent",
        ActorRef::System => "system",
    };
    trace
        .events
        .iter()
        .map(|e| match &e.event {
            Event::ExecutionStarted { .. } => "start".into(),
            Event::StepReady { step_id, .. } => format!("ready {step_id}"),
            Event::StepSkipped { step_id, .. } => format!("skip {step_id}"),
            Event::ForeachStarted {
                step_id, snapshot, ..
            } => {
                format!("foreach {step_id} items={}", snapshot.keys.len())
            }
            Event::IterationAdmitted {
                foreach_run_id,
                index,
            } => format!("admit {}[{index}]", path(foreach_run_id)),
            // The event index is 0-based; iteration paths are 1-based.
            Event::LoopIterationStarted { loop_run_id, index } => {
                format!("iterate {}#{}", path(loop_run_id), index + 1)
            }
            Event::LoopChecked { step_run_id, check } => format!(
                "loop-check {} iteration={} satisfied={}",
                path(step_run_id),
                check.iteration,
                check.satisfied
            ),
            Event::TimerFired { step_run_id, .. } => format!("timer {}", path(step_run_id)),
            Event::AgentWorkAvailable { work } => format!(
                "agent-work {}",
                work.ready_steps
                    .iter()
                    .map(|w| w.path.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Event::ChecklistUpdated { step_run_id, .. } => {
                format!("checklist {} by {}", path(step_run_id), who(&e.actor))
            }
            Event::StepReported {
                step_run_id,
                report,
            } => format!(
                "report {} {}/{} by {}",
                path(step_run_id),
                word(&report.status),
                word(&report.verdict),
                who(&e.actor)
            ),
            Event::StepCompleted {
                step_run_id,
                status,
                verdict,
            } => format!(
                "complete {} {}/{}",
                path(step_run_id),
                word(status),
                word(verdict)
            ),
            Event::BranchSelected { selection } => format!(
                "branch {} {}",
                path(&selection.step_run_id),
                match selection.choice {
                    BranchChoice::Case(i) => format!("case{i}"),
                    BranchChoice::Default => "default".into(),
                }
            ),
            Event::ApprovalRequested { snapshot } => format!(
                "approval-requested {} recipients={}",
                path(&snapshot.request.step_run_id),
                snapshot.recipients.len()
            ),
            Event::ApprovalVoted { vote } => format!(
                "vote {} {} by {}",
                path(&vote.step_run_id),
                vote.option,
                who(&vote.actor)
            ),
            Event::ApprovalDecided { decision } => format!(
                "decided {} {} signers={}",
                path(&decision.step_run_id),
                decision.option,
                decision.signers.len()
            ),
            Event::ApprovalEscalated { snapshot } => format!(
                "escalated {} to {}",
                path(&snapshot.request.step_run_id),
                serde_json::to_string(&snapshot.request.targets).unwrap()
            ),
            Event::ApprovalNotificationQueued { notice } => format!(
                "notice {} {}",
                notice.event_type(),
                path(&notice.step_run_id)
            ),
            Event::ApprovalExpired { step_run_id, .. } => format!("expired {}", path(step_run_id)),
            Event::DecisionApplied { .. } => "decision-applied".into(),
            Event::StepHandedOff { handoff } => format!(
                "handoff {} {} via {}",
                path(&handoff.request.step_run_id),
                word(&handoff.request.reason),
                word(&handoff.source)
            ),
            Event::StepBlockedNotificationQueued { notification } => {
                format!("blocked-notice {}", path(&notification.step_run_id))
            }
            Event::DataChanged { change } => format!(
                "set {} fields={} vars={}",
                path(&change.step_run_id),
                Value::Object(change.fields.clone().into_iter().collect()),
                Value::Object(change.vars.clone().into_iter().collect())
            ),
            Event::ExecutionDeadlineElapsed { .. } => "execution-deadline".into(),
            Event::CleanupStarted { cause, reason } => {
                format!(
                    "cleanup {cause:?} reason={}",
                    reason.as_deref().unwrap_or("-")
                )
            }
            Event::CleanupRootCompleted { .. } => "cleanup-done".into(),
            Event::ExecutionCompleted { status, outputs } => format!(
                "completed {} result={}",
                word(status),
                outputs.values.get("result").cloned().unwrap_or(Value::Null)
            ),
            other => other.name().into(),
        })
        .collect()
}
fn word<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value).unwrap() {
        Value::String(s) => s,
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

fn assert_lines(actual: &[String], expected: &[&str]) {
    let first_diff = actual
        .iter()
        .map(String::as_str)
        .zip(expected.iter().copied())
        .position(|(a, e)| a != e)
        .unwrap_or(actual.len().min(expected.len()));
    assert!(
        actual.len() == expected.len() && first_diff == actual.len(),
        "first difference at line {first_diff}\nactual:\n  {}\nexpected:\n  {}",
        actual.join("\n  "),
        expected.join("\n  ")
    );
}

/// PRD section 3, steps 1-5 and 7; section 5 full example. Smoke finds a 503
/// (status succeeded, verdict fail), so `decide` routes to the on-call
/// approval; a human chooses rollback; the destructive rollback runs; the
/// `finally` notify always runs; result `rolled_back` with status succeeded.
const MANUAL_ROLLBACK: &[&str] = &[
    "start",
    // `smoke` is a parallel: both children start together.
    "ready smoke",
    "ready api_health",
    "ready login_flow",
    // Only the agent step is announced to the dispatcher (login_flow is human).
    "agent-work api_health",
    // HTTP 503: the agent performed the check, the finding is fail.
    "report api_health succeeded/fail by agent",
    "checklist login_flow by human",
    "report login_flow succeeded/pass by human",
    // Groups aggregate the worst child verdict; status stays succeeded.
    "complete smoke succeeded/fail",
    // A verdict alone never fails the run: the foreach still starts.
    "foreach regions items=2",
    "admit regions[0]",
    "admit regions[1]",
    "ready regions[0].error_rate",
    "ready regions[1].error_rate",
    "agent-work regions[0].error_rate",
    "agent-work regions[1].error_rate",
    "report regions[0].error_rate succeeded/pass by agent",
    "report regions[1].error_rate succeeded/pass by agent",
    "complete regions succeeded/pass",
    // No region above 0.01: the loop's guard is false.
    "skip stabilize",
    "ready decide",
    // smoke.verdict == 'fail' selects the approval case.
    "branch decide case0",
    "ready signoff",
    "approval-requested signoff recipients=2",
    "vote signoff rollback by human",
    "decided signoff rollback signers=1",
    // Destructive rollback is reached by a human decision, which is allowed.
    "ready rollback",
    "agent-work rollback",
    "report rollback succeeded/- by agent",
    "ready mark_major",
    r#"set mark_major fields={"severity":"major"} vars={"result":"rolled_back"}"#,
    "complete decide succeeded/-",
    // `finally` runs after `steps`, then the run completes.
    "cleanup Succeeded reason=-",
    "ready notify",
    "agent-work notify",
    "report notify succeeded/- by agent",
    "cleanup-done",
    r#"completed succeeded result="rolled_back""#,
];

/// PRD "Checks and verdicts": regions fail, the loop re-checks after its 5m
/// delay and recovers on iteration 2. The loop is satisfied but its verdict is
/// fail, so the conservative example still routes to a human, who keeps.
const RECOVERED_LOOP: &[&str] = &[
    "start",
    "ready smoke",
    "ready api_health",
    "ready login_flow",
    "agent-work api_health",
    "report api_health succeeded/pass by agent",
    "checklist login_flow by human",
    "report login_flow succeeded/pass by human",
    "complete smoke succeeded/pass",
    "foreach regions items=2",
    "admit regions[0]",
    "admit regions[1]",
    "ready regions[0].error_rate",
    "ready regions[1].error_rate",
    "agent-work regions[0].error_rate",
    "agent-work regions[1].error_rate",
    "report regions[0].error_rate succeeded/fail by agent",
    "report regions[1].error_rate succeeded/fail by agent",
    "complete regions succeeded/fail",
    // A region above 0.01: the loop runs.
    "ready stabilize",
    "iterate stabilize#1",
    "ready stabilize#1.recheck",
    "agent-work stabilize#1.recheck",
    "report stabilize#1.recheck succeeded/fail by agent",
    "loop-check stabilize iteration=1 satisfied=false",
    // `delay: 5m` between iterations.
    "timer stabilize",
    "iterate stabilize#2",
    "ready stabilize#2.recheck",
    "agent-work stabilize#2.recheck",
    "report stabilize#2.recheck succeeded/pass by agent",
    "loop-check stabilize iteration=2 satisfied=true",
    // Satisfied, but the worst verdict across iterations is fail.
    "complete stabilize succeeded/fail",
    "ready decide",
    "branch decide case0",
    "ready signoff",
    "approval-requested signoff recipients=2",
    "vote signoff keep by human",
    "decided signoff keep signers=1",
    // `if: decision == 'rollback'` is false: skipped with reason condition.
    "skip rollback",
    "ready mark_major",
    r#"set mark_major fields={"severity":"major"} vars={"result":"kept"}"#,
    "complete decide succeeded/-",
    "cleanup Succeeded reason=-",
    "ready notify",
    "agent-work notify",
    "report notify succeeded/- by agent",
    "cleanup-done",
    r#"completed succeeded result="kept""#,
];

#[test]
fn manual_rollback_follows_the_prd_flow() {
    let trace = support::flagship::run(false);
    assert_lines(&project(&trace), MANUAL_ROLLBACK);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state.result, json!("rolled_back"));
    assert_eq!(trace.state.fields["severity"], json!("major"));
    // A human signature, never a system decision.
    assert_eq!(trace.count("DecisionApplied"), 0);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn recovered_loop_follows_the_prd_flow() {
    let trace = support::flagship::run(true);
    assert_lines(&project(&trace), RECOVERED_LOOP);
    let stabilize = trace.step("stabilize");
    assert!(stabilize.satisfied && !stabilize.exhausted);
    assert_eq!(stabilize.verdict, Some(Verdict::Fail));
    // Iteration 2 starts exactly when the 5m delay elapses.
    let fired = trace
        .events
        .iter()
        .find(|e| e.event.name() == "TimerFired")
        .unwrap();
    let checked = trace
        .events
        .iter()
        .find(|e| e.event.name() == "LoopChecked")
        .unwrap();
    assert_eq!(fired.at_ms - checked.at_ms, 5 * MIN);
    assert_eq!(trace.state.result, json!("kept"));
    assert_eq!(trace.state, trace.replay());
}

/// The checked-in conformance trace is a fixture other implementations use;
/// it is regenerated from the engine, so check it against the PRD lines.
#[test]
fn checked_in_conformance_trace_matches_the_prd_flow() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../conformance/execution/post-deploy.json"
    ))
    .unwrap();
    for (key, expected, recover) in [
        ("manual_rollback", MANUAL_ROLLBACK, false),
        ("recovered_loop", RECOVERED_LOOP, true),
    ] {
        let events: Vec<EventEnvelope> =
            serde_json::from_value(fixture[key]["events"].clone()).unwrap();
        let mut trace = support::flagship::run(recover);
        let stored = replay_stored(trace.state.compiled(), &trace.state.id, &events);
        trace.events = events;
        trace.state = stored;
        assert_lines(&project(&trace), expected);
    }
}

// ---------------------------------------------------------------------------
// Flows the fixture never exercised.

fn start() -> Trace {
    let mut directory = runbook_spec::CompileContext::default();
    directory
        .skills
        .insert("./skills/check-error-rates".into(), [1; 32]);
    directory
        .skills
        .insert("./skills/rollback-service".into(), [2; 32]);
    for (name, n) in [("oncall", 1), ("eng-leads", 2)] {
        directory
            .teams
            .insert(name.into(), format!("team_{n:026}").parse().unwrap());
    }
    let start = StartContext {
        inputs: [
            ("service".into(), json!("api")),
            ("version".into(), json!("commit-a")),
            ("regions".into(), json!(["east"])),
        ]
        .into(),
        ..Default::default()
    };
    let mut trace = run_document_with_start(support::flagship::SOURCE, &[], directory, start);
    trace.team_members.insert(
        "team_00000000000000000000000001".into(),
        vec![
            "usr_00000000000000000000000001".parse().unwrap(),
            "usr_00000000000000000000000002".parse().unwrap(),
        ],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    trace
}
fn act(
    trace: &mut Trace,
    step: &str,
    status: ReportStatus,
    verdict: Option<Verdict>,
    outputs: Value,
    actor: ActorRef,
) {
    let succeeded = status == ReportStatus::Succeeded;
    let before = trace.error_codes.len();
    trace.append(
        Event::StepReported {
            step_run_id: trace.step(step).id.clone().unwrap(),
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status,
                verdict,
                outputs: outputs
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                summary: Some("done".into()),
                evidence: if succeeded && step.contains("error_rate") {
                    vec!["https://metrics.example.test/e".into()]
                } else {
                    vec![]
                },
                logs: vec![],
            },
        },
        actor,
        trace.state.at_ms + 1,
    );
    trace.settle();
    assert_eq!(trace.error_codes.len(), before, "{:?}", trace.error_codes);
}
fn smoke(trace: &mut Trace, api: Verdict) {
    act(
        trace,
        "api_health",
        ReportStatus::Succeeded,
        Some(api),
        json!({"status_code": 200}),
        agent(),
    );
    let tick = update_checklist(
        &trace.state,
        trace.step("login_flow").id.as_ref().unwrap(),
        &[true, true],
    )
    .unwrap();
    trace.append(tick, human(1), trace.state.at_ms + 1);
    act(
        trace,
        "login_flow",
        ReportStatus::Succeeded,
        Some(Verdict::Pass),
        json!({}),
        human(1),
    );
}
fn tick(trace: &mut Trace, at_ms: i64) {
    for _ in 0..100 {
        let commands = advance_time(&trace.state, at_ms).unwrap();
        if commands.is_empty() {
            break;
        }
        for command in commands {
            trace.commit_command(command, at_ms);
        }
    }
    trace.settle();
}

/// PRD section 5 example: every check passes, so the `default` case marks
/// the run healthy; no approval is requested and nothing is rolled back.
#[test]
fn healthy_deploy_takes_the_default_branch_without_an_approval() {
    let mut trace = start();
    smoke(&mut trace, Verdict::Pass);
    act(
        &mut trace,
        "regions[0].error_rate",
        ReportStatus::Succeeded,
        Some(Verdict::Pass),
        json!({"rate": 0.001}),
        agent(),
    );
    assert_eq!(trace.ready, ["notify"]);
    act(
        &mut trace,
        "notify",
        ReportStatus::Succeeded,
        None,
        json!({}),
        agent(),
    );
    let lines = project(&trace);
    let tail = &lines[lines
        .iter()
        .position(|l| l == "complete regions succeeded/pass")
        .unwrap()..];
    assert_lines(
        tail,
        &[
            "complete regions succeeded/pass",
            "skip stabilize",
            "ready decide",
            "branch decide default",
            "ready mark_healthy",
            r#"set mark_healthy fields={"severity":"none"} vars={"result":"healthy"}"#,
            "complete decide succeeded/-",
            "cleanup Succeeded reason=-",
            "ready notify",
            "agent-work notify",
            "report notify succeeded/- by agent",
            "cleanup-done",
            r#"completed succeeded result="healthy""#,
        ],
    );
    assert!(trace.step("signoff").id.is_none());
    assert!(trace.step("rollback").id.is_none());
    assert_eq!(trace.count("ApprovalRequested"), 0);
    assert_eq!(trace.state, trace.replay());
}

/// PRD section 3 step 5 and `defaults.on_failure: handoff`: a check the agent
/// could not perform (`status: failed`) moves to a human (no assignee, so the
/// owners), with no fabricated finding; the human then performs it.
#[test]
fn check_the_agent_cannot_perform_is_handed_to_the_owners() {
    let mut trace = start();
    act(
        &mut trace,
        "api_health",
        ReportStatus::Failed,
        None,
        json!({}),
        agent(),
    );
    let lines = project(&trace);
    // "reassigns the step to a human ... and notifies them".
    assert_eq!(
        lines[lines.len() - 3..],
        [
            "report api_health failed/- by agent",
            "handoff api_health failed via owners",
            "blocked-notice api_health",
        ]
    );
    let step = trace.step("api_health");
    assert!(step.human_continuation);
    assert_eq!(step.verdict, None);
    assert_eq!(
        step.handoff.as_ref().unwrap().eligible_recipients,
        [runbook_model::RecipientRef::Team(
            "team_00000000000000000000000001".into()
        )]
    );
    // The agent may not take it back; the human performs it.
    trace.append(
        Event::StepReported {
            step_run_id: step.id.clone().unwrap(),
            report: WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Succeeded,
                verdict: Some(Verdict::Pass),
                outputs: [("status_code".into(), json!(200))].into(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        agent(),
        trace.state.at_ms + 1,
    );
    assert_eq!(trace.error_codes, ["step.human_required"]);
    act(
        &mut trace,
        "api_health",
        ReportStatus::Succeeded,
        Some(Verdict::Pass),
        json!({"status_code": 200}),
        human(2),
    );
    assert_eq!(trace.step("api_health").status, StepStatus::Succeeded);
    assert_eq!(trace.step("api_health").attempt_history.len(), 2);
    assert_eq!(trace.state, trace.replay());
}

/// PRD "Approval timeout": nobody decides. At 30m the gate escalates to
/// `team:eng-leads`; at 30m + 30m it expires and the run pauses
/// (`on_expired: pause`). No human or system decision is fabricated. The 2h
/// execution timeout keeps running through the pause and fails the run with
/// reason `timeout`; `finally` still notifies; the result stays undetermined.
#[test]
fn unanswered_signoff_escalates_expires_pauses_and_times_out() {
    let mut trace = start();
    smoke(&mut trace, Verdict::Fail);
    act(
        &mut trace,
        "regions[0].error_rate",
        ReportStatus::Succeeded,
        Some(Verdict::Pass),
        json!({"rate": 0.001}),
        agent(),
    );
    let opened = trace
        .events
        .iter()
        .find(|e| matches!(&e.event, Event::StepReady { step_id, .. } if step_id == "signoff"))
        .unwrap()
        .at_ms;
    let started = trace.events[0].at_ms;
    assert_eq!(
        trace.step("signoff").deadline_at_ms,
        Some(opened + 30 * MIN)
    );

    tick(&mut trace, opened + 30 * MIN - 1);
    assert_eq!(trace.count("ApprovalEscalated"), 0);
    tick(&mut trace, opened + 30 * MIN);
    tick(&mut trace, opened + 60 * MIN - 1);
    assert_eq!(trace.step("signoff").status, StepStatus::Waiting);
    tick(&mut trace, opened + 60 * MIN);
    assert_eq!(trace.step("signoff").status, StepStatus::Expired);
    assert_eq!(trace.state.status, ExecutionStatus::Paused);
    assert!(
        trace
            .state
            .pause_reasons
            .contains(&PauseReason::ApprovalExpired(
                trace.step("signoff").id.clone().unwrap()
            ))
    );

    tick(&mut trace, started + 120 * MIN - 1);
    assert_eq!(trace.state.status, ExecutionStatus::Paused);
    tick(&mut trace, started + 120 * MIN);
    assert_eq!(trace.ready, ["notify"]);
    act(
        &mut trace,
        "notify",
        ReportStatus::Succeeded,
        None,
        json!({}),
        agent(),
    );

    let lines = project(&trace);
    let tail = &lines[lines.iter().position(|l| l == "ready signoff").unwrap()..];
    assert_lines(
        tail,
        &[
            "ready signoff",
            "approval-requested signoff recipients=2",
            r#"escalated signoff to [{"kind":"team","reference":"team_00000000000000000000000002"}]"#,
            "notice approval.escalated signoff",
            "expired signoff",
            "notice approval.expired signoff",
            "execution-deadline",
            "ready notify",
            "agent-work notify",
            "report notify succeeded/- by agent",
            "cleanup-done",
            r#"completed failed result="undetermined""#,
        ],
    );
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(trace.count("DecisionApplied"), 0);
    assert!(trace.step("rollback").id.is_none());
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(trace.state.terminal_reason.as_deref(), Some("timeout"));
    assert_eq!(trace.state, trace.replay());
}
