//! CF13b, item 1: the hard ceiling on a run's event log (engine version 6).
//! The host measures the log; past the ceiling it records
//! `ExecutionFaulted { code: limit.run_size_exceeded }` and the run closes
//! failed in the same settle, without its cleanup (a cancelled run stays
//! cancelled). Runs under versions 1 to 5 refuse the fault, so their logs
//! replay unchanged.
mod support;
use runbook_engine::*;
use runbook_model::{ActorRef, ExecutionStatus, StepStatus};
use support::*;

fn human() -> ActorRef {
    ActorRef::Human {
        user_id: "usr_00000000000000000000000001".parse().unwrap(),
    }
}

fn fault(trace: &mut Trace) {
    let at = trace.state.at_ms;
    trace.append(
        Event::ExecutionFaulted {
            fault: RuntimeFault::run_size_exceeded(),
        },
        ActorRef::System,
        at,
    );
}

fn cancel(trace: &mut Trace) {
    let at = trace.state.at_ms;
    trace.append(
        Event::CancellationRequested {
            superseded_by: None,
            started_by: None,
        },
        human(),
        at,
    );
}

const WITH_CLEANUP: &str = "spec:\n  steps:\n    - {id: work, title: Work}\n    - {id: last, title: Last}\n  finally: [{id: tidy, title: Tidy}]\n  outputs: {a: '${{ steps.work.status }}'}";

#[test]
fn a_run_past_its_ceiling_closes_failed_without_its_cleanup() {
    let mut trace = run_case(WITH_CLEANUP, &[succeeded("work")]);
    assert_eq!(trace.state.engine_version, 6);
    assert!(run_ceiling_applies(&trace.state));
    fault(&mut trace);
    trace.settle();
    assert_eq!(trace.error_codes, [RUN_SIZE_EXCEEDED]);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(
        trace.state.terminal_reason.as_deref(),
        Some(RUN_SIZE_EXCEEDED)
    );
    assert_eq!(trace.step("last").status, StepStatus::Cancelled);
    // The cleanup never ran: its step was never made ready.
    assert!(trace.step("tidy").id.is_none());
    assert_eq!(trace.count("ExecutionCompleted"), 1);
    assert_eq!(trace.state.outputs["a"], "succeeded");
    assert!(!run_ceiling_applies(&trace.state));
    assert_eq!(trace.state, trace.replay());
}

/// A closed run refuses a second fault.
#[test]
fn the_fault_is_recorded_once() {
    let mut trace = run_case(WITH_CLEANUP, &[]);
    fault(&mut trace);
    trace.settle();
    fault(&mut trace);
    assert_eq!(
        trace.error_codes,
        [RUN_SIZE_EXCEEDED, "execution.invalid_fault"]
    );
}

/// A run cancelled with cleanup in progress stays cancelled; its cleanup
/// is cut short.
#[test]
fn a_cancelled_run_cleaning_up_stays_cancelled() {
    let mut trace = run_case(WITH_CLEANUP, &[succeeded("work")]);
    cancel(&mut trace);
    trace.settle();
    assert_eq!(trace.step("tidy").status, StepStatus::Ready);
    fault(&mut trace);
    trace.settle();
    assert_eq!(trace.error_codes, [RUN_SIZE_EXCEEDED]);
    assert_eq!(trace.state.status, ExecutionStatus::Cancelled);
    assert_eq!(trace.state.terminal_reason.as_deref(), Some("cancelled"));
    assert_eq!(trace.step("tidy").status, StepStatus::Cancelled);
    assert_eq!(trace.state, trace.replay());
}

/// A run whose steps all succeeded and that is running its cleanup fails
/// for its size.
#[test]
fn a_succeeding_run_cleaning_up_fails_for_its_size() {
    let mut trace = run_case(WITH_CLEANUP, &[succeeded("work"), succeeded("last")]);
    assert_eq!(trace.step("tidy").status, StepStatus::Ready);
    fault(&mut trace);
    trace.settle();
    assert_eq!(trace.error_codes, [RUN_SIZE_EXCEEDED]);
    assert_eq!(trace.state.status, ExecutionStatus::Failed);
    assert_eq!(
        trace.state.terminal_reason.as_deref(),
        Some(RUN_SIZE_EXCEEDED)
    );
    assert_eq!(trace.step("tidy").status, StepStatus::Cancelled);
    assert_eq!(trace.state, trace.replay());
}

/// Runs recorded before version 6 are not held to the ceiling: they refuse
/// the fault, and their logs replay unchanged.
#[test]
fn runs_before_engine_version_6_refuse_the_fault() {
    let mut trace = run_case(WITH_CLEANUP, &[]);
    let Event::ExecutionStarted { engine_version, .. } = &mut trace.events[0].event else {
        panic!("first event starts the run")
    };
    *engine_version = 5;
    trace.state = trace
        .events
        .iter()
        .try_fold(trace.initial.clone(), |state, event| apply(&state, event))
        .unwrap();
    assert!(!run_ceiling_applies(&trace.state));
    let before = trace.state.clone();
    fault(&mut trace);
    assert_eq!(trace.error_codes, ["step.not_found"]);
    assert_eq!(trace.state, before);
}

/// The fault names no step.
#[test]
fn a_fault_naming_a_step_is_refused() {
    let mut trace = run_case(WITH_CLEANUP, &[]);
    let at = trace.state.at_ms;
    trace.append(
        Event::ExecutionFaulted {
            fault: RuntimeFault {
                step_id: "work".into(),
                ..RuntimeFault::run_size_exceeded()
            },
        },
        ActorRef::System,
        at,
    );
    assert_eq!(trace.error_codes, ["execution.invalid_fault"]);
    assert_eq!(trace.state.status, ExecutionStatus::Running);
}
