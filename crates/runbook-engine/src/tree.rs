//! Structural traversal only. Never evaluates expressions or creates step IDs.
use crate::{BranchChoice, EngineError, ExecutionState};
use runbook_model::StepStatus;
use runbook_spec::ir::{Node, NodeKind};
pub(crate) fn children(node: &Node) -> Vec<&[Node]> {
    match &node.kind {
        NodeKind::Group(g) => vec![&g.children, &g.catch, &g.finally],
        NodeKind::Switch(s) => s
            .cases
            .iter()
            .map(|c| c.children.as_slice())
            .chain(std::iter::once(s.default.as_slice()))
            .collect(),
        NodeKind::Parallel { children, .. } => vec![children],
        NodeKind::Foreach(f) => vec![&f.children],
        NodeKind::Loop(l) => vec![&l.children],
        _ => vec![],
    }
}
pub(crate) fn collect<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        out.push(node);
        for body in children(node) {
            collect(body, out);
        }
    }
}
/// Declarations in one iteration. Repeated bodies are created only when admitted.
pub(crate) fn collect_local<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        out.push(node);
        if !matches!(node.kind, NodeKind::Foreach(_) | NodeKind::Loop(_)) {
            for body in children(node) {
                collect_local(body, out);
            }
        }
    }
}
pub(crate) fn find<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    for node in nodes {
        if node.id == id {
            return Some(node);
        }
        for body in children(node) {
            if let Some(found) = find(body, id) {
                return Some(found);
            }
        }
    }
    None
}
pub(crate) fn selected(node: &Node, choice: BranchChoice) -> Result<&[Node], EngineError> {
    let NodeKind::Switch(s) = &node.kind else {
        return Err(EngineError::new("step.not_switch"));
    };
    match choice {
        BranchChoice::Case(index) => s
            .cases
            .get(index as usize)
            .map(|c| c.children.as_slice())
            .ok_or(EngineError::new("step.invalid_branch")),
        BranchChoice::Default => Ok(&s.default),
    }
}
pub(crate) fn done(status: StepStatus) -> bool {
    matches!(status, StepStatus::Succeeded | StepStatus::Skipped)
}
pub(crate) fn failed(status: StepStatus) -> bool {
    matches!(
        status,
        StepStatus::Failed | StepStatus::TimedOut | StepStatus::Expired
    )
}
pub(crate) fn terminal(status: StepStatus) -> bool {
    done(status) || failed(status) || status == StepStatus::Cancelled
}
pub(crate) fn record_in<'a>(
    state: &'a ExecutionState,
    node: &Node,
    scope: &[crate::IterationRef],
) -> Result<&'a crate::StepState, EngineError> {
    state
        .steps
        .iter()
        .find(|s| s.node_id == node.id && s.scope == scope)
        .ok_or(EngineError::invariant("scoped step record is absent"))
}
pub(crate) fn first_failure<'a>(
    state: &'a ExecutionState,
    nodes: &[Node],
    scope: &[crate::IterationRef],
) -> Result<Option<&'a crate::StepState>, EngineError> {
    for node in nodes {
        let record = record_in(state, node, scope)?;
        if failed(record.status)
            && (node.on_failure != runbook_spec::ir::FailurePolicy::Continue
                || record.human_continuation)
        {
            return Ok(Some(record));
        }
    }
    Ok(None)
}
pub(crate) fn has_failure(
    state: &ExecutionState,
    nodes: &[Node],
    scope: &[crate::IterationRef],
) -> Result<bool, EngineError> {
    Ok(first_failure(state, nodes, scope)?.is_some())
}
pub(crate) fn unique_ready<'a>(
    state: &'a ExecutionState,
    node: &Node,
) -> Result<&'a crate::StepState, EngineError> {
    let mut matches = state
        .steps
        .iter()
        .filter(|s| s.node_id == node.id && s.status == StepStatus::Ready);
    let one = matches.next().ok_or(EngineError::new("step.not_ready"))?;
    if matches.next().is_some() {
        return Err(EngineError::new("step.ambiguous_instance"));
    }
    Ok(one)
}
pub(crate) fn sequence<'a>(
    state: &'a ExecutionState,
    nodes: &'a [Node],
    scope: &[crate::IterationRef],
) -> Result<Vec<&'a crate::StepState>, EngineError> {
    for node in nodes {
        let step = record_in(state, node, scope)?;
        if !done(step.status)
            && !(failed(step.status)
                && node.on_failure == runbook_spec::ir::FailurePolicy::Continue
                && !step.human_continuation)
        {
            return frontier_step(state, step);
        }
    }
    Ok(vec![])
}
pub(crate) fn frontier_step<'a>(
    state: &'a ExecutionState,
    step: &'a crate::StepState,
) -> Result<Vec<&'a crate::StepState>, EngineError> {
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    if step.status == StepStatus::Ready {
        let children = match &node.kind {
            NodeKind::Switch(_) if step.branch.is_some() => {
                let body = selected(
                    node,
                    step.branch
                        .ok_or(EngineError::invariant("selected switch has no branch"))?,
                )?;
                if has_failure(state, body, &step.scope)? {
                    vec![]
                } else {
                    sequence(state, body, &step.scope)?
                }
            }
            NodeKind::Parallel { children, .. } => {
                if crate::parallel::settled_early(state, step)? {
                    return Ok(vec![step]);
                }
                let mut active = vec![];
                for child in children {
                    let record = record_in(state, child, &step.scope)?;
                    if !terminal(record.status) {
                        active.extend(frontier_step(state, record)?);
                    }
                }
                active
            }
            NodeKind::Group(_) => crate::failure::frontier(state, step)?,
            NodeKind::Loop(_) => crate::loop_node::frontier(state, step)?,
            NodeKind::Foreach(_) => crate::foreach::frontier(state, step)?,
            _ => vec![],
        };
        if !children.is_empty() {
            return Ok(children);
        }
    }
    Ok(vec![step])
}
pub(crate) fn frontier(state: &ExecutionState) -> Result<Vec<&crate::StepState>, EngineError> {
    if let Some(root) = crate::cleanup::cancelled_root(state)? {
        return crate::cleanup::root_frontier(state, &root);
    }
    if state.phase == crate::ExecutionPhase::Cleanup {
        return crate::cleanup::frontier(state);
    }
    sequence(state, &state.compiled().ir().steps, &[])
}
pub(crate) fn is_frontier(state: &ExecutionState, id: &str) -> Result<bool, EngineError> {
    Ok(frontier(state)?.iter().any(|s| s.step_id == id))
}
