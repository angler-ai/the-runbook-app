use crate::{Event, EventEnvelope, ExecutionState};
use runbook_model::{ExecutionId, ReadyWork, RecipientRef, ReportStatus, StepRunId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A replayable notification fact. Delivery and credentials belong to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotificationIntent {
    pub event_id: String,
    pub kind: String,
    pub execution_id: ExecutionId,
    pub sequence: u64,
    pub at_ms: i64,
    pub step_run_ids: Vec<StepRunId>,
    pub recipients: Vec<RecipientRef>,
    pub reason: Option<String>,
    pub link: String,
    pub ready_steps: Vec<ReadyWork>,
    #[serde(default, with = "optional_stored_value")]
    pub params: Option<serde_json::Value>,
    /// Type-specific facts (decision option, step status, run result) for
    /// catalog events beyond the human/agent notices. Absent on intents
    /// stored before the C1 catalog grew.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_stored_value"
    )]
    pub detail: Option<serde_json::Value>,
}

/// Project a committed event against the state immediately after that event.
pub fn notification_for(
    event: &EventEnvelope,
    state: &ExecutionState,
) -> Option<NotificationIntent> {
    if event.execution_id != state.id || event.seq != state.sequence {
        return None;
    }
    let mut intent = NotificationIntent {
        event_id: event_id(&event.execution_id, event.seq, event.at_ms),
        kind: String::new(),
        execution_id: event.execution_id.clone(),
        sequence: event.seq,
        at_ms: event.at_ms,
        step_run_ids: vec![],
        recipients: vec![],
        reason: None,
        link: state.url.clone(),
        ready_steps: vec![],
        params: None,
        detail: None,
    };
    match &event.event {
        Event::ApprovalRequested { snapshot } => {
            intent.kind = "approval.requested".into();
            intent
                .step_run_ids
                .push(snapshot.request.step_run_id.clone());
            intent.recipients = snapshot.recipients.clone();
            // SLACK1 stage 2: the step's title, prompt and options, so a
            // chat message can offer one button per option.
            intent.detail = state
                .steps
                .iter()
                .find(|step| step.id.as_ref() == Some(&snapshot.request.step_run_id))
                .and_then(|step| state.node(&step.step_id))
                .and_then(|node| match &node.kind {
                    runbook_spec::ir::NodeKind::Approval(approval) => Some(serde_json::json!({
                        "step_title": node.title,
                        "prompt": approval.prompt,
                        "options": approval
                            .options
                            .iter()
                            .map(|o| serde_json::json!({"id": o.id, "title": o.title}))
                            .collect::<Vec<_>>(),
                    })),
                    _ => None,
                });
        }
        Event::DecisionLeaseEnded {
            step_run_id,
            lease_id,
            reason,
        } => {
            intent.kind = "lease.revoked".into();
            intent.step_run_ids.push(step_run_id.clone());
            intent.reason = Some(reason.clone());
            intent.detail = Some(serde_json::json!({ "lease_id": lease_id, "reason": reason }));
        }
        Event::AgentWorkAvailable { work } => {
            intent.kind = "agent.work_available".into();
            intent.step_run_ids = work
                .ready_steps
                .iter()
                .map(|s| s.step_run_id.clone())
                .collect();
            intent.ready_steps = work.ready_steps.clone();
            intent.reason = Some(work.reason.as_str().into());
            intent.params = work.params.clone();
        }
        Event::ApprovalNotificationQueued { notice } => {
            intent.kind = notice.event_type().into();
            intent.step_run_ids.push(notice.step_run_id.clone());
            intent.recipients = notice.recipients.clone();
            // The pending notice keeps the configured targets (teams), so
            // the identity survives when recipients are expanded to users.
            intent.detail = state
                .steps
                .iter()
                .find(|step| step.id.as_ref() == Some(&notice.step_run_id))
                .and_then(|step| step.approval.as_ref())
                .and_then(|approval| approval.notices.iter().find(|n| n.kind == notice.kind))
                .map(|pending| serde_json::json!({ "targets": pending.recipients }));
        }
        // LANG9: reminders and follow-up escalation, for people.
        Event::StepOverdue { step_id, due_at_ms } => {
            let step = state.steps.iter().find(|step| step.step_id == *step_id)?;
            let open = crate::followup::is_open(step);
            intent.kind = "step.overdue".into();
            intent.step_run_ids.extend(step.id.clone());
            intent.recipients = if open {
                crate::followup::reminder_recipients(state, step)
            } else {
                crate::followup::owner_recipients(state)
            };
            intent.reason = Some(if open { "still_open" } else { "not_open" }.into());
            intent.detail = Some(serde_json::json!({
                "path": step_id,
                "step_title": crate::step_title(state, step),
                "due_at_ms": due_at_ms,
            }));
        }
        Event::StepReminded { step_run_id, .. } => {
            intent.kind = "step.reminder".into();
            intent.step_run_ids.push(step_run_id.clone());
            intent.recipients = state
                .steps
                .iter()
                .find(|step| step.id.as_ref() == Some(step_run_id))
                .map(|step| crate::followup::reminder_recipients(state, step))
                .unwrap_or_default();
        }
        Event::StepEscalated { snapshot } => {
            intent.kind = "step.escalated".into();
            intent
                .step_run_ids
                .push(snapshot.request.step_run_id.clone());
            intent.recipients = snapshot.recipients.clone();
            intent.detail = Some(serde_json::json!({ "targets": snapshot.request.targets }));
        }
        Event::StepBlockedNotificationQueued { notification } => {
            intent.kind = notification.event_type().into();
            intent.step_run_ids.push(notification.step_run_id.clone());
            intent.recipients = notification.recipients.clone();
            intent.reason = Some(
                match notification.reason {
                    ReportStatus::Succeeded => "succeeded",
                    ReportStatus::Failed => "failed",
                    ReportStatus::Blocked => "blocked",
                    ReportStatus::Skipped => "skipped",
                }
                .into(),
            );
        }
        Event::ExecutionStarted { .. } => {
            intent.kind = "execution.started".into();
            intent.detail = Some(serde_json::json!({
                "status": crate::reported_status(state),
                "created_by": state.created_by,
                "parent_execution_id": state.parent.as_ref().map(|parent| &parent.execution_id),
            }));
        }
        Event::ExecutionCompleted { status, .. } => {
            intent.kind = "execution.completed".into();
            intent.detail = Some(serde_json::json!({
                "status": status,
                "result": state.result,
            }));
        }
        // D40: a review notice. Only identifiers: subscribers read the entry
        // (untrusted text) through the API.
        Event::MemoryProposed {
            step_run_id,
            memory_id,
        } => {
            intent.kind = "memory.proposed".into();
            intent.step_run_ids.push(step_run_id.clone());
            intent.detail = Some(serde_json::json!({ "memory_id": memory_id }));
        }
        // MEM6: a live save is notified the same way, so people can follow
        // (and revert) what agents write.
        Event::MemorySaved {
            step_run_id,
            memory_id,
        } => {
            intent.kind = "memory.saved".into();
            intent.step_run_ids.push(step_run_id.clone());
            intent.detail = Some(serde_json::json!({ "memory_id": memory_id }));
        }
        Event::MemoryEdited {
            step_run_id,
            memory_id,
        } => {
            intent.kind = "memory.edited".into();
            intent.step_run_ids.push(step_run_id.clone());
            intent.detail = Some(serde_json::json!({ "memory_id": memory_id }));
        }
        Event::ApprovalDecided { decision } => {
            intent.kind = "approval.decided".into();
            intent.step_run_ids.push(decision.step_run_id.clone());
            intent.detail = Some(serde_json::json!({
                "option": decision.option,
                "decided_by": decision.decided_by,
                "signers": decision.signers,
                "automatic": false,
            }));
        }
        _ => return None,
    }
    Some(intent)
}

/// Every catalog intent one committed event produces: the event's own notice
/// (`notification_for`) followed by `step.completed` for each step run that
/// reached a terminal status at this event, in step order. `before` is the
/// state immediately preceding the event.
pub fn notifications_for(
    event: &EventEnvelope,
    before: &ExecutionState,
    after: &ExecutionState,
) -> Vec<NotificationIntent> {
    let repeated_overdue = matches!(&event.event, Event::StepOverdue { step_id, .. }
        if after.steps.iter().find(|step| step.step_id == *step_id).is_some_and(|step|
            before.steps.iter().any(|prior| prior.node_id == step.node_id && prior.overdue_since_ms.is_some())));
    let mut out: Vec<NotificationIntent> = if repeated_overdue {
        vec![]
    } else {
        notification_for(event, after).into_iter().collect()
    };
    if event.execution_id != after.id || event.seq != after.sequence {
        return out;
    }
    let prior: std::collections::BTreeMap<&StepRunId, runbook_model::StepStatus> = before
        .steps
        .iter()
        .filter_map(|step| step.id.as_ref().map(|id| (id, step.status)))
        .collect();
    for step in &after.steps {
        let Some(id) = &step.id else { continue };
        if !terminal(step.status) || prior.get(id).is_some_and(|status| terminal(*status)) {
            continue;
        }
        out.push(NotificationIntent {
            event_id: scoped_event_id(&event.execution_id, event.seq, event.at_ms, id.as_str()),
            kind: "step.completed".into(),
            execution_id: event.execution_id.clone(),
            sequence: event.seq,
            at_ms: event.at_ms,
            step_run_ids: vec![id.clone()],
            recipients: vec![],
            reason: None,
            link: after.url.clone(),
            ready_steps: vec![],
            params: None,
            detail: Some(serde_json::json!({
                "status": step.status,
                "verdict": step.verdict,
                "path": step.step_id,
            })),
        });
    }
    out
}

fn terminal(status: runbook_model::StepStatus) -> bool {
    use runbook_model::StepStatus as S;
    matches!(
        status,
        S::Succeeded | S::Failed | S::Skipped | S::Cancelled | S::TimedOut | S::Expired
    )
}

fn event_id(execution_id: &ExecutionId, sequence: u64, at_ms: i64) -> String {
    scoped_event_id(execution_id, sequence, at_ms, "")
}

/// A secondary intent of the same event (one per step run) hashes its
/// discriminator too; the primary intent keeps the original identity.
fn scoped_event_id(
    execution_id: &ExecutionId,
    sequence: u64,
    at_ms: i64,
    discriminator: &str,
) -> String {
    let execution = execution_id.as_str().as_bytes();
    let mut hash = Sha256::new();
    hash.update((execution.len() as u64).to_be_bytes());
    hash.update(execution);
    hash.update(sequence.to_be_bytes());
    if !discriminator.is_empty() {
        hash.update((discriminator.len() as u64).to_be_bytes());
        hash.update(discriminator.as_bytes());
    }
    let digest = hash.finalize();
    let timestamp = (at_ms.max(0) as u64).min((1_u64 << 48) - 1);
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&timestamp.to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(&digest[..10]);
    let mut value = u128::from_be_bytes(bytes);
    let mut encoded = [b'0'; 26];
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    for byte in encoded.iter_mut().rev() {
        *byte = ALPHABET[(value & 31) as usize];
        value >>= 5;
    }
    format!(
        "evt_{}",
        encoded.into_iter().map(char::from).collect::<String>()
    )
}

pub(crate) mod optional_stored_value {
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    #[derive(Serialize)]
    struct Borrowed<'a>(#[serde(with = "runbook_spec::stored_value")] &'a Value);
    #[derive(Deserialize)]
    struct Owned(#[serde(with = "runbook_spec::stored_value")] Value);

    pub fn serialize<S: serde::Serializer>(
        value: &Option<Value>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_ref().map(Borrowed).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Value>, D::Error> {
        Ok(Option::<Owned>::deserialize(deserializer)?.map(|value| value.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_ids_are_stable_and_scoped_to_execution_and_sequence() {
        let first: ExecutionId = "exe_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
        let second: ExecutionId = "exe_01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap();
        let id = event_id(&first, 7, 1_234);
        assert_eq!(id, event_id(&first, 7, 1_234));
        assert_ne!(id, event_id(&first, 8, 1_234));
        assert_ne!(id, event_id(&second, 7, 1_234));
        assert_eq!(id.len(), 30);
        assert!(id.starts_with("evt_0"));
        assert_eq!(event_id(&first, 7, -1), event_id(&first, 7, 0));
    }
}
