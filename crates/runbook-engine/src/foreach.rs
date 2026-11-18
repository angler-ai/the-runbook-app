//! Persisted item snapshots and bounded iteration admission.
use crate::{Command, EngineError, ExecutionState, StepState};
use runbook_model::{StepStatus, Verdict};
use runbook_spec::ir::{Node, NodeKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IterationRef {
    pub parent: String,
    pub index: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForeachSnapshot {
    pub base_sequence: u64,
    #[serde(with = "runbook_spec::stored_value")]
    pub items: Value,
    pub keys: Vec<String>,
    pub reads: Vec<runbook_expr::ReadRef>,
    pub admitted: u32,
}
pub(crate) fn scope_for(parent: &StepState, index: u32) -> Vec<IterationRef> {
    let mut scope = parent.scope.clone();
    scope.push(IterationRef {
        parent: parent.step_id.clone(),
        index,
    });
    scope
}
pub(crate) fn frontier<'a>(
    state: &'a ExecutionState,
    parent: &'a StepState,
) -> Result<Vec<&'a StepState>, EngineError> {
    let node = state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Foreach(f) = &node.kind else {
        return Err(EngineError::new("step.not_foreach"));
    };
    let snapshot = parent
        .foreach
        .as_ref()
        .ok_or(EngineError::new("foreach.not_started"))?;
    let mut active = vec![];
    for index in 0..snapshot.admitted {
        let scope = scope_for(parent, index);
        if crate::tree::has_failure(state, &f.children, &scope)? {
            continue;
        }
        active.extend(crate::tree::sequence(state, &f.children, &scope)?);
    }
    // Admission is a control action and must get a turn even while other actors run.
    if snapshot.admitted < snapshot.keys.len() as u32
        && active_iterations(state, parent)? < f.concurrency
    {
        return Ok(vec![parent]);
    }
    Ok(active)
}
fn active_iterations(state: &ExecutionState, parent: &StepState) -> Result<u32, EngineError> {
    let NodeKind::Foreach(f) = &state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    else {
        return Err(EngineError::new("step.not_foreach"));
    };
    let mut active = 0;
    for index in 0..parent
        .foreach
        .as_ref()
        .ok_or(EngineError::new("foreach.not_started"))?
        .admitted
    {
        let scope = scope_for(parent, index);
        if !crate::tree::has_failure(state, &f.children, &scope)?
            && !crate::tree::sequence(state, &f.children, &scope)?.is_empty()
        {
            active += 1;
        }
    }
    Ok(active)
}
pub fn advance_foreach(state: &ExecutionState, node: &Node) -> Result<Vec<Command>, EngineError> {
    advance_for(state, crate::tree::unique_ready(state, node)?)
}
pub(crate) fn advance_for(
    state: &ExecutionState,
    parent: &StepState,
) -> Result<Vec<Command>, EngineError> {
    let NodeKind::Foreach(f) = &state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    else {
        return Err(EngineError::new("step.not_foreach"));
    };
    let snapshot = parent
        .foreach
        .as_ref()
        .ok_or(EngineError::new("foreach.not_started"))?;
    if snapshot.admitted < snapshot.keys.len() as u32
        && active_iterations(state, parent)? < f.concurrency
    {
        return Ok(vec![Command::AdmitIteration {
            foreach_run_id: parent
                .id
                .clone()
                .ok_or(EngineError::new("step.not_found"))?,
            index: snapshot.admitted,
        }]);
    }
    let children = frontier(state, parent)?;
    if !children.is_empty() {
        return crate::decider::advance_nodes(state, &children);
    }
    let (status, verdict) = outcome(state, parent)?;
    Ok(vec![Command::CompleteStep {
        step_run_id: parent
            .id
            .clone()
            .ok_or(EngineError::new("step.not_found"))?,
        status,
        verdict,
    }])
}
pub(crate) fn outcome(
    state: &ExecutionState,
    parent: &StepState,
) -> Result<(StepStatus, Option<Verdict>), EngineError> {
    let NodeKind::Foreach(f) = &state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    else {
        return Err(EngineError::new("step.not_foreach"));
    };
    let snapshot = parent
        .foreach
        .as_ref()
        .ok_or(EngineError::new("foreach.not_started"))?;
    if snapshot.admitted as usize != snapshot.keys.len() || active_iterations(state, parent)? != 0 {
        return Err(EngineError::new("step.not_complete"));
    }
    let mut failed = false;
    let mut verdicts = vec![];
    for index in 0..snapshot.admitted {
        let scope = scope_for(parent, index);
        failed |= crate::tree::has_failure(state, &f.children, &scope)?;
        for child in &f.children {
            verdicts.push(crate::tree::record_in(state, child, &scope)?.verdict);
        }
    }
    if failed
        && state
            .node(&parent.step_id)
            .ok_or(EngineError::new("step.not_found"))?
            .on_failure
            == runbook_spec::ir::FailurePolicy::Handoff
    {
        return Err(EngineError::new("engine.unsupported_failure_policy"));
    }
    Ok((
        if failed {
            StepStatus::Failed
        } else {
            StepStatus::Succeeded
        },
        crate::report::worst_verdict(verdicts.into_iter()),
    ))
}
pub(crate) fn prepare(
    state: &ExecutionState,
    parent: &StepState,
    guard_reads: Vec<runbook_expr::ReadRef>,
) -> Result<ForeachSnapshot, EngineError> {
    let NodeKind::Foreach(f) = &state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    else {
        return Err(EngineError::new("step.not_foreach"));
    };
    let path = format!(
        "{}/foreach",
        state
            .node(&parent.step_id)
            .ok_or(EngineError::invariant("parent declaration is absent"))?
            .source_path
    );
    let mut reads = guard_reads;
    let items = crate::data::evaluate_scoped_path(
        state,
        &parent.scope,
        &format!("{path}/items"),
        &mut reads,
    )?;
    let values = items
        .as_array()
        .ok_or(EngineError::new("foreach.invalid_items"))?;
    if values.len() > f.max_items as usize {
        return Err(EngineError::new("foreach.max_items"));
    }
    let mut keys = vec![];
    let mut seen = std::collections::BTreeSet::new();
    for (index, item) in values.iter().enumerate() {
        let key = if f.key.is_some() {
            crate::data::evaluate_key(
                state,
                parent,
                &f.alias,
                index as u32,
                item,
                &format!("{path}/key"),
                &mut reads,
            )?
            .as_str()
            .ok_or(EngineError::new("foreach.invalid_key"))?
            .to_owned()
        } else {
            index.to_string()
        };
        if !seen.insert(key.clone()) {
            return Err(EngineError::new("foreach.duplicate_key"));
        }
        keys.push(key);
    }
    let snapshot = ForeachSnapshot {
        base_sequence: state.sequence,
        items,
        keys,
        reads,
        admitted: 0,
    };
    validate_snapshot(state, parent, &snapshot)?;
    Ok(snapshot)
}
pub(crate) fn validate_snapshot(
    state: &ExecutionState,
    parent: &StepState,
    snapshot: &ForeachSnapshot,
) -> Result<(), EngineError> {
    let NodeKind::Foreach(f) = &state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    else {
        return Err(EngineError::new("step.not_foreach"));
    };
    if snapshot.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    crate::data::admit_foreach(&snapshot.items, &snapshot.keys)?;
    crate::data::validate_foreach_items(
        state,
        state
            .node(&parent.step_id)
            .ok_or(EngineError::new("step.not_found"))?,
        &snapshot.items,
    )?;
    let items = snapshot
        .items
        .as_array()
        .ok_or(EngineError::new("foreach.invalid_items"))?;
    if items.len() > f.max_items as usize {
        return Err(EngineError::new("foreach.max_items"));
    }
    if items.len() != snapshot.keys.len() || snapshot.admitted != 0 {
        return Err(EngineError::new("foreach.invalid_snapshot"));
    }
    let unique: std::collections::BTreeSet<_> = snapshot.keys.iter().collect();
    if unique.len() != items.len() {
        return Err(EngineError::new("foreach.duplicate_key"));
    }
    Ok(())
}
pub(crate) fn admit(
    state: &mut ExecutionState,
    parent_index: usize,
    index: u32,
) -> Result<(), EngineError> {
    let parent = &state.steps[parent_index];
    let NodeKind::Foreach(f) = &state
        .node(&parent.step_id)
        .ok_or(EngineError::new("step.not_found"))?
        .kind
    else {
        return Err(EngineError::new("step.not_foreach"));
    };
    let snapshot = parent
        .foreach
        .as_ref()
        .ok_or(EngineError::new("foreach.not_started"))?;
    if parent.status != StepStatus::Ready
        || index != snapshot.admitted
        || index as usize >= snapshot.keys.len()
        || active_iterations(state, parent)? >= f.concurrency
    {
        return Err(EngineError::new("foreach.invalid_admission"));
    }
    let scope = scope_for(parent, index);
    let key = escape_key(&snapshot.keys[index as usize]);
    let prefix = format!("{}[{key}]", parent.step_id);
    let mut declarations = vec![];
    crate::tree::collect_local(&f.children, &mut declarations);
    let rows: Vec<_> = declarations
        .iter()
        .map(|node| StepState::pending(node, format!("{prefix}.{}", node.id), scope.clone()))
        .collect();
    state.steps.extend(rows);
    state.steps[parent_index]
        .foreach
        .as_mut()
        .ok_or(EngineError::invariant(
            "foreach snapshot disappeared during admission",
        ))?
        .admitted += 1;
    Ok(())
}
fn escape_key(key: &str) -> String {
    let mut out = String::new();
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(out, "%{byte:02X}").unwrap();
        }
    }
    out
}
