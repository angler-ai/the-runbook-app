//! LANG2 (later-plans rule 1): `join: any` and `join: {at_least: n}`. A
//! parallel starts every branch; its join is met once `n` branches have
//! succeeded and can no longer be met once fewer than `n` could still
//! succeed. Either way the open branches are cancelled at once
//! (`SiblingsCancelled`, the fail-fast fact) and the parallel completes:
//! `succeeded` when met, `failed` when not. Branch failures before that are
//! absorbed by the join. Pinned as a conformance trace
//! (`conformance/execution/parallel-join.json`).
mod support;
use runbook_model::{ExecutionStatus, ReportStatus, StepStatus, Verdict};
use support::*;

const ANY: &str = "spec:\n  steps:\n    - id: mirrors\n      join: any\n      parallel:\n        - {id: east, title: Fetch from east, check: true}\n        - {id: west, title: Fetch from west, check: true}\n        - {id: south, title: Fetch from south, check: true}\n    - {id: after, title: Use the artifact}\n  outputs:\n    mirrors: \"${{ steps.mirrors.status }}\"\n";

fn two_of_three() -> String {
    ANY.replace("join: any", "join: {at_least: 2}")
}
fn report(step: &str, status: ReportStatus, verdict: Option<Verdict>) -> Input {
    let mut input = succeeded(step);
    let Input::Report {
        status: s,
        verdict: v,
        ..
    } = &mut input
    else {
        unreachable!()
    };
    *s = status;
    *v = verdict;
    input
}
fn pass(step: &str) -> Input {
    report(step, ReportStatus::Succeeded, Some(Verdict::Pass))
}
fn fail(step: &str) -> Input {
    report(step, ReportStatus::Failed, None)
}

#[test]
fn any_completes_on_the_first_success_and_cancels_the_rest() {
    let started = run_case(ANY, &[]);
    assert_eq!(started.ready, ["east", "west", "south"]);
    let trace = run_case(ANY, &[pass("west")]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("west").status, StepStatus::Succeeded);
    for open in ["east", "south"] {
        assert_eq!(trace.step(open).status, StepStatus::Cancelled, "{open}");
    }
    assert_eq!(trace.count("SiblingsCancelled"), 1);
    assert_eq!(trace.step("mirrors").status, StepStatus::Succeeded);
    assert_eq!(trace.step("mirrors").verdict, Some(Verdict::Pass));
    assert_eq!(trace.ready, ["after"]);
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn failures_are_absorbed_while_the_join_can_still_be_met() {
    let trace = run_case(ANY, &[fail("east"), fail("south")]);
    assert_eq!(trace.state.status, ExecutionStatus::Running);
    assert_eq!(trace.ready, ["west"]);
    assert_eq!(trace.count("SiblingsCancelled"), 0);
    let done = run_case(ANY, &[fail("east"), fail("south"), pass("west")]);
    assert_eq!(done.step("mirrors").status, StepStatus::Succeeded);
    assert_eq!(done.ready, ["after"]);
    assert_eq!(done.state, done.replay());
}

#[test]
fn any_fails_when_every_branch_failed() {
    let trace = run_case(ANY, &[fail("east"), fail("south"), fail("west")]);
    assert_eq!(trace.step("mirrors").status, StepStatus::Failed);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert!(trace.step("after").id.is_none());
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn at_least_waits_for_its_count() {
    let spec = two_of_three();
    let one = run_case(&spec, &[pass("east")]);
    assert_eq!(one.ready, ["west", "south"]);
    let two = run_case(&spec, &[pass("east"), pass("south")]);
    assert_eq!(two.step("west").status, StepStatus::Cancelled);
    assert_eq!(two.step("mirrors").status, StepStatus::Succeeded);
    assert_eq!(two.ready, ["after"]);
    assert_eq!(two.state, two.replay());
}

#[test]
fn at_least_fails_as_soon_as_it_can_no_longer_be_met() {
    let spec = two_of_three();
    // One failure leaves two that could still succeed: keep going.
    let one = run_case(&spec, &[fail("east")]);
    assert_eq!(one.ready, ["west", "south"]);
    // A second failure leaves one: the join is lost, the last branch is
    // cancelled at once and the run fails.
    let lost = run_case(&spec, &[fail("east"), fail("west")]);
    assert!(lost.error_codes.is_empty(), "{:?}", lost.error_codes);
    assert_eq!(lost.step("south").status, StepStatus::Cancelled);
    assert_eq!(lost.step("mirrors").status, StepStatus::Failed);
    assert_eq!(lost.state.status, ExecutionStatus::Failed);
    assert_eq!(lost.state, lost.replay());
}

#[test]
fn skipped_branches_do_not_count_toward_the_join() {
    let spec = ANY
        .replace(
            "{id: east, title: Fetch from east, check: true}",
            "{id: east, title: Fetch from east, check: true, if: '${{ false }}'}",
        )
        .replace(
            "{id: west, title: Fetch from west, check: true}",
            "{id: west, title: Fetch from west, check: true, if: '${{ false }}'}",
        );
    let waiting = run_case(&spec, &[]);
    assert_eq!(waiting.ready, ["south"]);
    let lost = run_case(&spec, &[fail("south")]);
    assert_eq!(lost.step("mirrors").status, StepStatus::Failed);
    assert_eq!(lost.state, lost.replay());
}

#[test]
fn a_lost_join_is_routed_like_any_failure() {
    let spec = ANY.replace(
        "    - id: mirrors\n      join: any\n",
        "    - id: mirrors\n      join: any\n      on_failure: continue\n",
    );
    let trace = run_case(&spec, &[fail("east"), fail("south"), fail("west")]);
    assert_eq!(trace.step("mirrors").status, StepStatus::Failed);
    assert_eq!(trace.ready, ["after"]);
    let done = run_case(
        &spec,
        &[
            fail("east"),
            fail("south"),
            fail("west"),
            succeeded("after"),
        ],
    );
    assert_eq!(done.step("mirrors").status, StepStatus::Failed);
    assert_eq!(done.state.status, ExecutionStatus::Succeeded);
    assert_eq!(done.state, done.replay());
    assert_eq!(trace.state, trace.replay());
}

#[test]
fn cancellation_of_open_nested_work_is_exact() {
    let spec = "spec:\n  steps:\n    - id: race\n      join: any\n      parallel:\n        - id: slow\n          steps:\n            - {id: one, title: One}\n            - {id: two, title: Two}\n        - {id: fast, title: Fast}\n";
    let trace = run_case(spec, &[succeeded("fast")]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    for id in ["slow", "one"] {
        assert_eq!(trace.step(id).status, StepStatus::Cancelled, "{id}");
    }
    // Never-entered work stays unmaterialized.
    assert!(trace.step("two").id.is_none());
    assert_eq!(trace.step("race").status, StepStatus::Succeeded);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}

/// The recorded cancellation is checked against the join on replay: a
/// `SiblingsCancelled` before the join is decided is refused (the parallel
/// is not even on the frontier while its branches run).
#[test]
fn a_cancellation_before_the_join_is_decided_is_refused() {
    use runbook_engine::Event;
    let mut trace = run_case(ANY, &[]);
    let parallel = trace.step("mirrors").id.clone().unwrap();
    let targets = ["east", "south"].map(|s| trace.step(s).id.clone().unwrap());
    trace.append(
        Event::SiblingsCancelled {
            parallel_run_id: parallel,
            step_run_ids: targets.to_vec(),
        },
        runbook_model::ActorRef::System,
        5,
    );
    assert_eq!(trace.error_codes, ["step.not_ready"]);
    assert_eq!(trace.count("SiblingsCancelled"), 0);
}

#[test]
fn parallel_join_trace_matches_checked_in_contract() {
    use serde_json::json;
    let spec = two_of_three();
    let met = run_case(&spec, &[fail("west"), pass("east"), pass("south")]);
    let lost = run_case(&spec, &[fail("east"), fail("west")]);
    let fixture = json!({
        "format": "pure-engine-trace-v1",
        "runbook": spec,
        "met": {"events": met.events, "commands": met.commands, "status": met.state.status},
        "lost": {"events": lost.events, "commands": lost.commands, "status": lost.state.status},
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/execution/parallel-join.json");
    if std::env::var_os("RUNBOOK_UPDATE_FIXTURES").is_some() {
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
        )
        .unwrap();
    }
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, fixture, "{}", path.display());
}

/// A cancelled branch's group `finally` still runs (as for `fail_fast`)
/// before the parallel completes and later work starts.
#[test]
fn a_met_join_unwinds_cancelled_groups_through_their_finally() {
    let spec = "spec: {steps: [{id: fan, join: any, parallel: [{id: fast, title: Fast}, {id: slow, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}]}, {id: after, title: After}]}";
    let during = run_case(spec, &[succeeded("fast")]);
    assert!(during.error_codes.is_empty(), "{:?}", during.error_codes);
    assert_eq!(during.step("work").status, StepStatus::Cancelled);
    assert_eq!(during.ready, ["clean"]);
    let done = run_case(
        spec,
        &[succeeded("fast"), succeeded("clean"), succeeded("after")],
    );
    assert!(done.error_codes.is_empty(), "{:?}", done.error_codes);
    assert_eq!(done.step("slow").status, StepStatus::Cancelled);
    assert_eq!(done.step("fan").status, StepStatus::Succeeded);
    assert_eq!(done.state.status, ExecutionStatus::Succeeded);
    assert_eq!(done.state, done.replay());
}

/// A branch already in its `finally` when the join is met keeps cleaning up.
#[test]
fn a_met_join_preserves_an_already_started_finally() {
    let spec = "spec: {steps: [{id: fan, join: any, parallel: [{id: fast, title: Fast}, {id: slow, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}]}, {id: after, title: After}]}";
    let trace = run_case(
        spec,
        &[
            succeeded("work"),
            succeeded("fast"),
            succeeded("clean"),
            succeeded("after"),
        ],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("clean").status, StepStatus::Succeeded);
    assert_eq!(trace.step("fan").status, StepStatus::Succeeded);
    assert_eq!(trace.state.status, ExecutionStatus::Succeeded);
    assert_eq!(trace.state, trace.replay());
}

// Review follow-ups (LANG2).

fn verdict(step: &str, status: ReportStatus, finding: Option<Verdict>) -> Input {
    report(step, status, finding)
}

/// A met join's verdict comes from the branches counted toward it only:
/// an absorbed loser that carried a `fail` finding does not count.
#[test]
fn a_met_join_takes_its_verdict_from_the_counted_branches() {
    let spec = "spec: {steps: [{id: fan, join: any, parallel: [{id: east, steps: [{id: look, title: Look, check: true}, {id: act, title: Act}]}, {id: west, title: West, check: true}]}]}";
    let trace = run_case(
        spec,
        &[
            verdict("look", ReportStatus::Succeeded, Some(Verdict::Fail)),
            fail("act"),
            pass("west"),
        ],
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("east").status, StepStatus::Failed);
    assert_eq!(trace.step("east").verdict, Some(Verdict::Fail));
    assert_eq!(trace.step("fan").status, StepStatus::Succeeded);
    assert_eq!(trace.step("fan").verdict, Some(Verdict::Pass));
    assert_eq!(trace.state, trace.replay());
}

/// The same holds for a branch cancelled after its own check found `fail`,
/// and for a failed check whose report carried a verdict.
#[test]
fn cancelled_and_failed_losers_add_no_finding() {
    let spec = "spec: {steps: [{id: fan, join: any, parallel: [{id: east, steps: [{id: look, title: Look, check: true}, {id: act, title: Act}]}, {id: west, title: West, check: true}]}]}";
    let trace = run_case(
        spec,
        &[
            verdict("look", ReportStatus::Succeeded, Some(Verdict::Fail)),
            pass("west"),
        ],
    );
    assert_eq!(trace.step("east").status, StepStatus::Cancelled);
    assert_eq!(trace.step("fan").verdict, Some(Verdict::Pass));
    let failed_check = run_case(
        ANY,
        &[
            verdict("east", ReportStatus::Failed, Some(Verdict::Fail)),
            pass("west"),
        ],
    );
    assert_eq!(failed_check.step("mirrors").status, StepStatus::Succeeded);
    assert_eq!(failed_check.step("mirrors").verdict, Some(Verdict::Pass));
}

/// A branch waiting out a retry backoff is still open: the join waits for it.
#[test]
fn a_branch_in_retry_backoff_keeps_an_any_join_open() {
    let spec = "spec: {steps: [{id: fan, join: any, parallel: [{id: east, title: East, retry: {max_attempts: 2, backoff: 1m}}, {id: west, title: West}]}]}";
    let waiting = run_case(spec, &[fail("east"), fail("west")]);
    assert!(waiting.error_codes.is_empty(), "{:?}", waiting.error_codes);
    assert_eq!(waiting.step("east").status, StepStatus::Waiting);
    assert_eq!(waiting.step("fan").status, StepStatus::Ready);
    assert_eq!(waiting.state.status, ExecutionStatus::Running);
    let done = run_case(
        spec,
        &[
            fail("east"),
            fail("west"),
            Input::Tick { at_ms: 60_000 },
            succeeded("east"),
        ],
    );
    assert_eq!(done.step("fan").status, StepStatus::Succeeded);
    assert_eq!(done.state.status, ExecutionStatus::Succeeded);
    assert_eq!(done.state, done.replay());
}

/// A branch handed off to a person is still open: the join waits for it.
#[test]
fn a_branch_awaiting_handoff_keeps_an_any_join_open() {
    let source = |spec: &str| {
        format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: test, title: Test, owners: [user:alice]}}\n{spec}"
        )
    };
    let context = || {
        let mut context = runbook_spec::CompileContext::default();
        context.users.insert(
            "alice".into(),
            "usr_00000000000000000000000001".parse().unwrap(),
        );
        context
    };
    let spec = "spec: {steps: [{id: fan, join: any, parallel: [{id: east, title: East, actor: agent, on_failure: handoff}, {id: west, title: West}]}]}";
    let waiting = run_document(&source(spec), &[fail("east"), fail("west")], context());
    assert!(waiting.error_codes.is_empty(), "{:?}", waiting.error_codes);
    assert_eq!(waiting.count("StepHandedOff"), 1);
    assert!(!matches!(
        waiting.step("east").status,
        StepStatus::Succeeded | StepStatus::Failed | StepStatus::Cancelled | StepStatus::Skipped
    ));
    assert_eq!(waiting.step("fan").status, StepStatus::Ready);
    assert_eq!(waiting.state.status, ExecutionStatus::Running);
    let done = run_document(
        &source(spec),
        &[fail("east"), fail("west"), succeeded("east")],
        context(),
    );
    assert_eq!(done.step("fan").status, StepStatus::Succeeded);
    assert_eq!(done.state, done.replay());
}

#[test]
fn an_any_join_with_every_branch_skipped_fails() {
    let spec = "spec: {steps: [{id: fan, join: any, parallel: [{id: a, title: A, if: '${{ false }}'}, {id: b, title: B, if: '${{ false }}'}]}, {id: after, title: After}]}";
    let trace = run_case(spec, &[]);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("a").status, StepStatus::Skipped);
    assert_eq!(trace.step("fan").status, StepStatus::Failed);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert!(trace.step("after").id.is_none());
    assert_eq!(trace.state, trace.replay());
}
