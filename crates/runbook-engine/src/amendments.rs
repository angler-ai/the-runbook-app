//! Step amendments (B9a/D35): validate an override of a finished step's
//! verdict, outputs and summary. History is preserved; nothing re-runs.
use crate::{EngineError, ExecutionState};
use runbook_model::{Amendment, StepRecord, StepRunId, StepStatus};
use runbook_spec::ir::NodeKind;
use serde::{Deserialize, Serialize};

/// What the step record was when it first finished, kept on amendment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OriginalStepRecord {
    pub verdict: Option<runbook_model::Verdict>,
    #[serde(with = "runbook_spec::stored_data")]
    pub outputs: runbook_model::Data,
    pub summary: Option<String>,
}

pub fn current_record(step: &crate::StepState) -> StepRecord {
    StepRecord {
        status: step.status,
        verdict: step.verdict,
        outputs: step.outputs.clone(),
        summary: step.summary.clone(),
    }
}

fn finished(step: &crate::StepState) -> bool {
    step.finished_at_ms.is_some()
        && matches!(
            step.status,
            StepStatus::Succeeded
                | StepStatus::Failed
                | StepStatus::Cancelled
                | StepStatus::Skipped
        )
}

/// Validate an override and build its Amendment (the caller fills `by`).
/// Approvals can never be amended; unfinished steps and empty reasons fail.
pub fn validate_override(
    state: &ExecutionState,
    step_run_id: &StepRunId,
    to: &StepRecord,
    reason: &str,
    by: runbook_model::ActorRef,
) -> Result<Amendment, EngineError> {
    if reason.trim().is_empty() {
        return Err(EngineError::new("amendment.reason_required"));
    }
    let index = state.step_index(step_run_id)?;
    let step = &state.steps[index];
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    if matches!(node.kind, NodeKind::Approval(_)) || step.approval.is_some() {
        return Err(EngineError::new("amendment.approval_forbidden"));
    }
    if !finished(step) {
        return Err(EngineError::new("amendment.step_not_finished"));
    }
    if to.status != step.status {
        return Err(EngineError::new("amendment.status_change"));
    }
    if let NodeKind::Task(task) = &node.kind {
        if task.check && to.status == StepStatus::Succeeded && to.verdict.is_none() {
            return Err(EngineError::new("step.verdict_required"));
        }
        let errors = crate::report::output_errors(
            &task.outputs,
            &to.outputs,
            to.status == runbook_model::StepStatus::Succeeded,
        )?;
        if !errors.is_empty() {
            return Err(EngineError::Rejected {
                code: "step.invalid_outputs",
                errors,
            });
        }
    }
    Ok(Amendment {
        step_run_id: step_run_id.clone(),
        from: current_record(step),
        to: to.clone(),
        reason: reason.to_owned(),
        by,
    })
}

/// Mark every already-evaluated consumer of the amended step stale,
/// transitively: direct step-output reads, reads of variables or fields a
/// stale step assigned, and foreach aggregates (nested ones included) over
/// affected iterations. Already-stale steps are traversed too, so a later
/// reader of their data is reached (F14).
/// Nothing is recomputed or re-run (D35/B9).
pub(crate) fn mark_stale(state: &mut ExecutionState, amended: &runbook_model::StepRunId) {
    use std::collections::BTreeSet;
    // Precompute owned provenance so mutation below borrows nothing.
    struct Facts {
        run_id: Option<String>,
        read_runs: BTreeSet<String>,
        read_vars: BTreeSet<String>,
        assigned_vars: BTreeSet<String>,
        parents: BTreeSet<String>,
    }
    let facts: Vec<Facts> = state
        .steps
        .iter()
        .map(|step| {
            let mut assigned_vars = BTreeSet::new();
            if let Some(node) = state.node(&step.step_id)
                && let runbook_spec::ir::NodeKind::Set(assignments) = &node.kind
            {
                assigned_vars.extend(assignments.variables.keys().map(|k| format!("vars.{k}")));
                assigned_vars.extend(assignments.fields.keys().map(|k| format!("fields.{k}")));
            }
            Facts {
                run_id: step.id.as_ref().map(ToString::to_string),
                read_runs: step
                    .reads
                    .iter()
                    .filter_map(|read| read.origin.record.step_run_id.clone())
                    .collect(),
                read_vars: step
                    .reads
                    .iter()
                    .filter_map(|read| {
                        ["vars.", "fields."].iter().find_map(|root| {
                            read.origin.path.strip_prefix(root).map(|name| {
                                format!("{root}{}", name.split('.').next().unwrap_or(name))
                            })
                        })
                    })
                    .collect(),
                assigned_vars,
                parents: step
                    .scope
                    .iter()
                    .map(|iteration| iteration.parent.clone())
                    .collect(),
            }
        })
        .collect();
    let step_id_of: Vec<String> = state.steps.iter().map(|s| s.step_id.clone()).collect();
    let amended_id = amended.to_string();
    let mut seen: BTreeSet<String> = [amended_id.clone()].into();
    let mut queue = vec![amended_id];
    let mut mark: BTreeSet<usize> = BTreeSet::new();
    while let Some(source) = queue.pop() {
        let stale_vars: BTreeSet<String> = facts
            .iter()
            .filter(|fact| fact.run_id.as_ref().is_some_and(|id| seen.contains(id)))
            .flat_map(|fact| fact.assigned_vars.iter().cloned())
            .collect();
        for (index, fact) in facts.iter().enumerate() {
            let Some(run_id) = &fact.run_id else { continue };
            if seen.contains(run_id) {
                continue;
            }
            let is_foreach_parent = facts.iter().any(|child| {
                child.run_id.as_ref() == Some(&source) && child.parents.contains(&step_id_of[index])
            });
            if fact.read_runs.contains(&source)
                || !fact.read_vars.is_disjoint(&stale_vars)
                || is_foreach_parent
            {
                mark.insert(index);
                seen.insert(run_id.clone());
                queue.push(run_id.clone());
            }
        }
    }
    for index in mark {
        state.steps[index].stale = true;
    }
}
