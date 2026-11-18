//! LANG9 follow-up facts in run views: reminders already sent and a
//! follow-up escalation, so the run page can say a step was reminded or
//! escalated without replaying notification intents.
use runbook_core::execution_document::{build_document, followup_of, render_markdown};
use runbook_engine::{EscalationRequest, EscalationSnapshot, ExecutionState, StepState};
use runbook_model::{
    Data, ExecutionPhase, ExecutionView, RecipientRef, StepFollowupView, StepStatus,
};
use std::{num::NonZeroU32, rc::Rc};

const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: followup, title: Follow-up}\n\
    spec:\n  steps:\n    \
    - {id: work, title: Work, remind_after: 1h, escalate_after: 2h, escalate_to: [team:oncall]}\n    \
    - {id: untouched, title: Untouched}\n";

fn state() -> ExecutionState {
    let spec = runbook_spec::parse(SPEC).unwrap();
    let mut context = runbook_spec::CompileContext::default();
    context.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let compiled = runbook_spec::compile(&spec, &context).unwrap();
    ExecutionState::new(
        "exe_00000000000000000000000001".parse().unwrap(),
        Rc::new(compiled),
    )
}

fn team() -> RecipientRef {
    RecipientRef::Team("team_00000000000000000000000001".into())
}

fn escalate(step: &mut StepState) {
    let step_run_id = step
        .id
        .get_or_insert_with(|| "sr_00000000000000000000000001".parse().unwrap())
        .clone();
    step.followup_escalation = Some(EscalationSnapshot {
        request: EscalationRequest {
            base_sequence: 1,
            step_run_id,
            deadline_at_ms: 7_200_000,
            targets: vec![team()],
        },
        recipients: vec![team()],
        directory_revision: 1,
    });
}

fn header(state: &ExecutionState) -> ExecutionView {
    ExecutionView {
        schema_revision: NonZeroU32::MIN,
        id: state.id.clone(),
        runbook_id: None,
        runbook_slug: None,
        project_id: None,
        created_at_ms: None,
        created_by: None,
        started_at_ms: state.started_at_ms,
        completed_at_ms: None,
        runbook_version_id: "rbv_00000000000000000000000001".parse().unwrap(),
        runbook_version: Some(1),
        title: state.title.clone(),
        status: state.status,
        phase: ExecutionPhase::Ordinary,
        admission: None,
        execution_deadline_at_ms: state.execution_deadline_at_ms,
        result: None,
        pending_terminal_status: None,
        terminal_reason: None,
        inputs: state.inputs.clone(),
        fields: state.fields.clone(),
        tags: state.tags.clone(),
        steps: vec![],
        amended: false,
        amendments: vec![],
        stale_step_ids: vec![],
        rerun_of: None,
        resume_brief: None,
        notifications_not_delivered: vec![],
        superseded_by: None,
        parent: None,
        progress: None,
    }
}

#[test]
fn followup_of_reports_only_followups_that_happened() {
    let mut state = state();
    let work = state
        .steps
        .iter_mut()
        .find(|s| s.step_id == "work")
        .unwrap();
    work.status = StepStatus::Ready;
    assert_eq!(followup_of(work), None);

    work.reminders_sent = 2;
    assert_eq!(
        followup_of(work),
        Some(StepFollowupView {
            reminders_sent: 2,
            escalated: false,
            escalated_to: vec![],
        })
    );

    escalate(work);
    assert_eq!(
        followup_of(work),
        Some(StepFollowupView {
            reminders_sent: 2,
            escalated: true,
            escalated_to: vec![team()],
        })
    );
}

#[test]
fn escalation_without_a_reminder_is_still_reported() {
    let mut state = state();
    let work = state
        .steps
        .iter_mut()
        .find(|s| s.step_id == "work")
        .unwrap();
    escalate(work);
    assert_eq!(
        followup_of(work),
        Some(StepFollowupView {
            reminders_sent: 0,
            escalated: true,
            escalated_to: vec![team()],
        })
    );
}

#[test]
fn the_document_and_markdown_show_followup_facts() {
    let mut state = state();
    for (index, step) in state.steps.iter_mut().enumerate() {
        step.id = Some(format!("sr_{:026}", index + 1).parse().unwrap());
    }
    let work = state
        .steps
        .iter_mut()
        .find(|s| s.step_id == "work")
        .unwrap();
    work.reminders_sent = 2;
    escalate(work);

    let document = build_document(&state, header(&state), |id| {
        format!("/v1/step-runs/{id}/logs")
    });
    let section = document
        .sections
        .iter()
        .find(|s| s.path == "work")
        .expect("the work section");
    assert_eq!(
        section.followup,
        Some(StepFollowupView {
            reminders_sent: 2,
            escalated: true,
            escalated_to: vec![team()],
        })
    );
    let untouched = document
        .sections
        .iter()
        .find(|s| s.path == "untouched")
        .expect("the untouched section");
    assert_eq!(untouched.followup, None);

    let markdown = render_markdown(&state, &document);
    assert!(markdown.contains("- Reminded: 2 times"), "{markdown}");
    assert!(markdown.contains("- Escalated to: team:"), "{markdown}");
}

#[test]
fn data_is_an_object_smoke_check() {
    let _: Data = Data::new();
}
