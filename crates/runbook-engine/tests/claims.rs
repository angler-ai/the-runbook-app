//! Claim lifecycle replay: claim, report while claimed, release, expiry.
mod support;
use runbook_engine::Event;
use runbook_model::{ActorRef, ReportStatus, StepStatus};
use support::run_case;

fn claim(trace: &mut support::Trace, event: Event) {
    let actor = ActorRef::Human {
        user_id: "usr_00000000000000000000000001".parse().unwrap(),
    };
    trace.append(event, actor, trace.state.at_ms);
    trace.settle();
}

#[test]
fn claim_lifecycle_replays() {
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work}, {id: next, title: Next}]}",
        &[],
    );
    let id = trace.step("work").id.clone().unwrap();
    claim(
        &mut trace,
        Event::StepClaimed {
            step_run_id: id.clone(),
        },
    );
    assert_eq!(trace.step("work").status, StepStatus::Claimed);
    // A report from the claimed state completes the step.
    let mut reported = run_case(
        "spec: {steps: [{id: work, title: Work}, {id: next, title: Next}]}",
        &[],
    );
    let rid = reported.step("work").id.clone().unwrap();
    claim(&mut reported, Event::StepClaimed { step_run_id: rid });
    let actor = ActorRef::Human {
        user_id: "usr_00000000000000000000000001".parse().unwrap(),
    };
    reported.append(
        Event::StepReported {
            step_run_id: reported.step("work").id.clone().unwrap(),
            report: runbook_engine::WorkReport {
                appended_logs: false,
                files: vec![],
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Default::default(),
                summary: None,
                evidence: vec![],
                logs: vec![],
            },
        },
        actor,
        reported.state.at_ms,
    );
    reported.settle();
    assert_eq!(reported.step("work").status, StepStatus::Succeeded);
    assert_eq!(reported.state, reported.replay());
    // Release and expiry both return a claimed step to ready.
    claim(
        &mut trace,
        Event::StepClaimReleased {
            step_run_id: id.clone(),
            reason: None,
        },
    );
    assert_eq!(trace.step("work").status, StepStatus::Ready);
    claim(
        &mut trace,
        Event::StepClaimed {
            step_run_id: id.clone(),
        },
    );
    claim(&mut trace, Event::StepClaimExpired { step_run_id: id });
    assert_eq!(trace.step("work").status, StepStatus::Ready);
    assert_eq!(trace.state, trace.replay());
}
