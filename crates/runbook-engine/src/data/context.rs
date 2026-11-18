//! Lexical iteration views over persisted instances; no expression evaluation.
use super::*;
use crate::{IterationRef, StepState};
type ContextOrigins = (BTreeMap<String, RecordIdentity>, BTreeMap<String, Origin>);
pub(super) type ExtraItem<'a> = (&'a StepState, &'a str, u32, &'a Value);
pub(super) fn iteration_path(
    state: &ExecutionState,
    scope: &[IterationRef],
) -> Result<Vec<runbook_expr::IterationIdentity>, EngineError> {
    scope
        .iter()
        .map(|frame| {
            let parent = state
                .steps
                .iter()
                .find(|s| s.step_id == frame.parent)
                .ok_or(EngineError::new("engine.invalid_state"))?;
            Ok(runbook_expr::IterationIdentity {
                node_id: parent.node_id.clone(),
                instance_id: parent
                    .id
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| parent.node_id.clone()),
                ordinal: frame.index,
            })
        })
        .collect()
}
fn step_identity(state: &ExecutionState, step: &StepState) -> Result<RecordIdentity, EngineError> {
    let mut record = identity(state, step.revision.max(1));
    record.step_run_id = step.id.as_ref().map(ToString::to_string);
    record.iteration_path = iteration_path(state, &step.scope)?;
    Ok(record)
}
fn step_value(
    state: &ExecutionState,
    step: &StepState,
    path: &str,
    revisions: &mut BTreeMap<String, RecordIdentity>,
) -> Result<Value, EngineError> {
    let actor = match &step.actor {
        Some(ActorRef::Human { user_id }) => Some(user_id.as_str()),
        Some(ActorRef::Agent { principal_id, .. }) => Some(principal_id.as_str()),
        Some(ActorRef::System) => Some("system"),
        None => None,
    };
    revisions.insert(path.into(), step_identity(state, step)?);
    let mut record = json!({"status":step.status,"verdict":step.verdict,"actor":actor,"started_at":null,"finished_at":null,"attempts":step.attempt,"amended":step.amended,"stale":step.stale});
    if let Some(time) = step.finished_at_ms {
        record["finished_at"] = timestamp(time)?;
    }
    if step.status == StepStatus::Succeeded {
        record["outputs"] = object(&step.outputs);
    }
    if matches!(
        state.node(&step.step_id).map(|n| &n.kind),
        Some(NodeKind::Loop(_))
    ) {
        record["satisfied"] = json!(step.satisfied);
        record["exhausted"] = json!(step.exhausted);
    }
    if let Some((name, count)) = collection(step) {
        let mut items = vec![];
        for index in 0..count {
            items.push(scope_steps(
                state,
                &crate::foreach::scope_for(step, index as u32),
                &format!("{path}.{name}[{index}]"),
                revisions,
            )?);
        }
        record[name] = Value::Array(items);
    }
    Ok(record)
}
fn collection(step: &StepState) -> Option<(&'static str, usize)> {
    step.foreach
        .as_ref()
        .map(|s| ("items", s.keys.len()))
        .or_else(|| {
            step.loop_state
                .as_ref()
                .map(|s| ("iterations", s.admitted as usize))
        })
}
fn scope_steps(
    state: &ExecutionState,
    scope: &[IterationRef],
    path: &str,
    revisions: &mut BTreeMap<String, RecordIdentity>,
) -> Result<Value, EngineError> {
    let mut records = serde_json::Map::new();
    for step in state
        .steps
        .iter()
        .filter(|s| s.scope == scope && s.id.is_some())
    {
        records.insert(
            step.node_id.clone(),
            step_value(state, step, &format!("{path}.{}", step.node_id), revisions)?,
        );
    }
    Ok(Value::Object(records))
}
pub(super) fn populate(
    state: &ExecutionState,
    roots: &mut BTreeMap<String, Value>,
    scope: &[IterationRef],
    extra: Option<ExtraItem<'_>>,
) -> Result<ContextOrigins, EngineError> {
    admit_projection(state, roots, scope, extra)?;
    let mut revisions = BTreeMap::new();
    let mut steps = serde_json::Map::new();
    for step in selected_steps(state, scope)?.values() {
        steps.insert(
            step.node_id.clone(),
            step_value(
                state,
                step,
                &format!("steps.{}", step.node_id),
                &mut revisions,
            )?,
        );
    }
    roots.insert("steps".into(), Value::Object(steps));
    let mut aliases = BTreeMap::new();
    for (depth, frame) in scope.iter().enumerate() {
        let parent = state
            .steps
            .iter()
            .find(|s| s.step_id == frame.parent)
            .ok_or(EngineError::new("engine.invalid_state"))?;
        if matches!(
            state.node(&parent.step_id).map(|n| &n.kind),
            Some(NodeKind::Loop(_))
        ) {
            let mut origin = identity(state, parent.revision.max(1));
            origin.iteration_path = iteration_path(state, &scope[..=depth])?;
            roots.insert("iteration".into(), json!(u64::from(frame.index) + 1));
            aliases.insert(
                "iteration".into(),
                Origin {
                    path: "iteration".into(),
                    record: origin,
                },
            );
            continue;
        }
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
        let item = snapshot
            .items
            .as_array()
            .and_then(|v| v.get(frame.index as usize))
            .ok_or(EngineError::new("foreach.invalid_snapshot"))?;
        let mut origin = identity(state, snapshot.base_sequence + 1);
        origin.iteration_path = iteration_path(state, &scope[..=depth])?;
        add_alias(roots, &mut aliases, &f.alias, frame.index, item, origin);
    }
    if let Some((parent, name, index, item)) = extra {
        let mut origin = identity(state, state.sequence.max(1));
        origin.iteration_path = iteration_path(state, scope)?;
        origin.iteration_path.push(runbook_expr::IterationIdentity {
            node_id: parent.node_id.clone(),
            instance_id: parent
                .id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| parent.node_id.clone()),
            ordinal: index,
        });
        add_alias(roots, &mut aliases, name, index, item, origin);
    }
    Ok((revisions, aliases))
}
/// Outside a repeated region use its last materialized input iteration. Inside,
/// constrain every descendant to the active iteration before choosing a row:
/// an unmaterialized current declaration must never expose another item's data.
fn selected_steps<'a>(
    state: &'a ExecutionState,
    scope: &[IterationRef],
) -> Result<BTreeMap<&'a str, &'a StepState>, EngineError> {
    let mut regions = vec![];
    for frame in scope {
        let parent = state
            .node(&frame.parent)
            .ok_or(EngineError::new("engine.invalid_state"))?;
        let mut descendants = vec![];
        for body in crate::tree::children(parent) {
            crate::tree::collect(body, &mut descendants);
        }
        regions.push(descendants);
    }
    let mut selected: BTreeMap<&str, &StepState> = BTreeMap::new();
    for step in state.steps.iter().filter(|s| s.id.is_some()) {
        if regions.iter().enumerate().any(|(depth, nodes)| {
            nodes.iter().any(|n| n.id == step.node_id) && !step.scope.starts_with(&scope[..=depth])
        }) {
            continue;
        }
        let replace = selected.get(step.node_id.as_str()).is_none_or(|prior| {
            step.scope
                .iter()
                .map(|f| f.index)
                .cmp(prior.scope.iter().map(|f| f.index))
                .is_gt()
        });
        if replace {
            selected.insert(&step.node_id, step);
        }
    }
    Ok(selected)
}
fn add_alias(
    roots: &mut BTreeMap<String, Value>,
    aliases: &mut BTreeMap<String, Origin>,
    name: &str,
    index: u32,
    item: &Value,
    identity: RecordIdentity,
) {
    roots.insert(name.into(), item.clone());
    roots.insert("index".into(), json!(index));
    aliases.insert(
        name.into(),
        Origin {
            path: name.into(),
            record: identity.clone(),
        },
    );
    aliases.insert(
        "index".into(),
        Origin {
            path: "index".into(),
            record: identity,
        },
    );
}

// Charge the entire projected view before constructing any aggregate or alias
// copies. The same instance can appear in history and the lexical current view.
fn admit_projection(
    state: &ExecutionState,
    roots: &BTreeMap<String, Value>,
    scope: &[IterationRef],
    extra: Option<ExtraItem<'_>>,
) -> Result<(), EngineError> {
    fn charge_nodes(
        budget: &mut CopyAdmission,
        count: usize,
        depth: usize,
    ) -> Result<(), EngineError> {
        budget.nodes = budget
            .nodes
            .checked_add(count)
            .filter(|n| *n <= 262_144)
            .ok_or(EngineError::new("expression.binding_limit"))?;
        if depth > 32 {
            return Err(EngineError::new("expression.binding_limit"));
        }
        Ok(())
    }
    fn visit(
        state: &ExecutionState,
        scope: &[IterationRef],
        depth: usize,
        budget: &mut CopyAdmission,
    ) -> Result<(), EngineError> {
        charge_nodes(budget, 1, depth)?;
        for step in state
            .steps
            .iter()
            .filter(|s| s.scope == scope && s.id.is_some())
        {
            visit_step(state, step, depth, budget)?;
        }
        Ok(())
    }
    fn visit_step(
        state: &ExecutionState,
        step: &StepState,
        depth: usize,
        budget: &mut CopyAdmission,
    ) -> Result<(), EngineError> {
        charge_nodes(budget, 13, depth + 1)?;
        budget.text(&step.node_id)?;
        // Fixed keys, status, finding and timestamps fit under this bound.
        budget.bytes = budget
            .bytes
            .checked_add(256)
            .filter(|n| *n <= 8_388_608)
            .ok_or(EngineError::new("expression.binding_limit"))?;
        match &step.actor {
            Some(ActorRef::Human { user_id }) => budget.text(user_id.as_str())?,
            Some(ActorRef::Agent { principal_id, .. }) => budget.text(principal_id)?,
            _ => (),
        }
        if step.status == StepStatus::Succeeded {
            for (key, value) in &step.outputs {
                budget.text(key)?;
                budget.value(value, depth + 3)?;
            }
        }
        if let Some((_, count)) = collection(step) {
            charge_nodes(budget, 3, depth + 2)?;
            for index in 0..count {
                visit(
                    state,
                    &crate::foreach::scope_for(step, index as u32),
                    depth + 3,
                    budget,
                )?;
            }
        }
        Ok(())
    }
    let mut budget = CopyAdmission::default();
    for value in roots.values() {
        budget.value(value, 0)?;
    }
    for step in selected_steps(state, scope)?.values() {
        visit_step(state, step, 1, &mut budget)?;
    }
    for frame in scope {
        let parent = state
            .steps
            .iter()
            .find(|s| s.step_id == frame.parent)
            .ok_or(EngineError::new("engine.invalid_state"))?;
        if parent.loop_state.is_some() {
            budget.value(&json!(frame.index), 0)?;
            continue;
        }
        let item = parent
            .foreach
            .as_ref()
            .and_then(|s| s.items.as_array())
            .and_then(|v| v.get(frame.index as usize))
            .ok_or(EngineError::new("foreach.invalid_snapshot"))?;
        budget.value(item, 0)?;
    }
    if let Some((_, _, _, item)) = extra {
        budget.value(item, 0)?;
    }
    Ok(())
}
