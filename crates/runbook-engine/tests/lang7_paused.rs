//! LANG7 re-check 2, finding 1: a strict task whose retry comes due while
//! the run is paused must not ask for its assignee resolution (the reducer
//! accepts it only while running), or every settle on the paused run fails
//! with `execution.not_running` until it is resumed. Resume resolves it.
mod support;
use runbook_engine::Event;
use runbook_model::{ActorRef, ReportStatus, StepStatus};
use support::*;

const ALICE: &str = "usr_00000000000000000000000001";

fn paused_retry(strict: bool) -> Trace {
    let mut context = runbook_spec::CompileContext::default();
    context.users.insert("alice".into(), ALICE.parse().unwrap());
    let mut failed = succeeded("a");
    let Input::Report { status, .. } = &mut failed else {
        unreachable!()
    };
    *status = ReportStatus::Failed;
    let extra = if strict { ", assignment: strict" } else { "" };
    let mut trace = run_document(
        &format!(
            "metadata: {{id: test}}\nspec: {{steps: [{{id: a, assignee: user:alice{extra}, retry: {{max_attempts: 2, backoff: 1m}}}}]}}"
        ),
        &[failed],
        context,
    );
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    let at = trace.state.at_ms;
    let alice = ActorRef::Human {
        user_id: ALICE.parse().unwrap(),
    };
    trace.append(
        Event::ManualPauseChanged { paused: true },
        alice.clone(),
        at,
    );
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    // The retry comes due while paused: any operation's settle sees it.
    let due = at + 120_000;
    for _ in 0..10 {
        let commands = runbook_engine::advance_time(&trace.state, due).unwrap();
        if commands.is_empty() {
            break;
        }
        for command in commands {
            trace.commit_command(command, due);
        }
    }
    trace.settle();
    trace
}

#[test]
fn a_guidance_retry_comes_due_while_paused() {
    let trace = paused_retry(false);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("a").status, StepStatus::Ready);
}

#[test]
fn a_strict_retry_comes_due_while_paused_without_blocking_the_run() {
    let mut trace = paused_retry(true);
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert_eq!(trace.step("a").status, StepStatus::Ready);
    // Resuming resolves the assignee for the new attempt.
    let at = trace.state.at_ms;
    let alice = ActorRef::Human {
        user_id: ALICE.parse().unwrap(),
    };
    trace.append(Event::ManualPauseChanged { paused: false }, alice, at);
    trace.settle();
    assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
    assert!(
        trace
            .event_names()
            .iter()
            .rev()
            .any(|e| *e == "StepAssigneeResolved"),
        "{:?}",
        trace.event_names()
    );
}
