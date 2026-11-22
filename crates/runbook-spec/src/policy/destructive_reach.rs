//! R0-3 (runner design §6.5): a runner step's reach, and which tasks come
//! after a person's approval on every path to them.
use crate::ir::*;
use runbook_model::{AgentCeiling, Delivery, RunnerLabel};
use std::collections::BTreeMap;

/// The highest label among what a step can use: every tool of the agent,
/// its credentials (narrowed by `connections`, except for model delivery)
/// and its hosts (narrowed by `agent.network`). Returns the
/// label and the names of the items that carry it.
pub(crate) fn reach(
    ceiling: &AgentCeiling,
    connections: Option<&[String]>,
    network: Option<&[String]>,
) -> (RunnerLabel, Vec<String>) {
    let mut items: Vec<(RunnerLabel, String)> = ceiling
        .tools
        .iter()
        .map(|t| (t.label, format!("tool {}", t.name)))
        .collect();
    items.extend(
        ceiling
            .credentials
            .iter()
            .filter(|c| {
                c.delivery.contains(&Delivery::Model)
                    || connections.is_none_or(|ids| ids.contains(&c.id))
            })
            .map(|c| (c.label, format!("credential {}", c.name))),
    );
    items.extend(
        ceiling
            .hosts
            .iter()
            .filter(|h| network.is_none_or(|hosts| hosts.contains(&h.host)))
            .map(|h| (h.label, format!("host {}", h.host))),
    );
    let top = items
        .iter()
        .map(|(label, _)| *label)
        .max()
        .unwrap_or(RunnerLabel::Read);
    let names = items
        .into_iter()
        .filter(|(label, _)| *label == top)
        .map(|(_, name)| name)
        .collect();
    (top, names)
}

/// A person decides it: an unguarded approval that agents may not decide
/// and that never decides itself on timeout.
fn human_approval(node: &Node) -> bool {
    match &node.kind {
        NodeKind::Approval(a) => {
            node.guard.is_none()
                && !a.allow_agents
                && matches!(a.on_timeout, ApprovalTimeout::Escalate)
        }
        _ => false,
    }
}

/// Every task (by source path) and whether a person's approval precedes it
/// on every path from the run's start: an earlier sibling in a sequence
/// the task is in, or one an enclosing sequence ran before entering it.
/// A `catch` or `finally` block starts from its group's entry (it can run
/// before an approval inside the group); the runbook's `finally` starts
/// undominated; parallel branches and switch cases never dominate each
/// other or what follows them.
pub(crate) fn dominated_tasks(ir: &NormalizedRunbook) -> BTreeMap<String, bool> {
    let mut out = vec![];
    walk(ir, &mut out);
    out.into_iter()
        .map(|(task, approvals, _)| (task.source_path.clone(), !approvals.is_empty()))
        .collect()
}

/// Every task (by node ID) and the person-decided approvals (node IDs)
/// that precede it on every path from the run's start, as
/// [`dominated_tasks`] judges them. A claim checks that one of these was
/// actually decided by a person in the run (runner design §6.5 rule 2).
pub fn dominating_approvals(ir: &NormalizedRunbook) -> BTreeMap<String, Vec<String>> {
    let mut out = vec![];
    walk(ir, &mut out);
    out.into_iter()
        .map(|(task, approvals, _)| {
            (
                task.id.clone(),
                approvals
                    .iter()
                    .map(|approval| approval.id.clone())
                    .collect(),
            )
        })
        .collect()
}

/// Nonblocking warnings for legacy options on approvals used as destructive gates.
pub fn approval_option_warnings(ir: &NormalizedRunbook) -> Vec<crate::Diagnostic> {
    let mut tasks = vec![];
    walk(ir, &mut tasks);
    let mut warnings = BTreeMap::new();
    for (task, approvals, _) in tasks {
        if !matches!(&task.kind, NodeKind::Task(task) if task.destructive &&
                matches!(task.agent_target, Some(AgentTarget::Runner { .. })))
        {
            continue;
        }
        for node in approvals {
            if matches!(&node.kind, NodeKind::Approval(approval) if approval.approves.is_none()) {
                warnings.entry(node.id.as_str()).or_insert_with(|| crate::Diagnostic {
                    code: "spec.approval_options_unspecified".into(),
                    path: format!("{}/approval", node.source_path),
                    message: Some(format!("Approval '{}' lets any human decision unlock a destructive runner step. Set approves to the options that grant permission.", node.id)),
                    line: 0,
                    column: 0,
                });
            }
        }
    }
    warnings.into_values().collect()
}

/// Check newly published definitions. Existing snapshots retain the runtime
/// semantics of unspecified options, including the legacy warning above.
pub fn publication_approval_diagnostics(
    ir: &NormalizedRunbook,
) -> (Vec<crate::Diagnostic>, Vec<crate::Diagnostic>) {
    let mut tasks = vec![];
    walk(ir, &mut tasks);
    let mut errors = BTreeMap::new();
    let mut warnings = BTreeMap::new();
    for (task, approvals, outside_loop) in tasks {
        if !matches!(&task.kind, NodeKind::Task(task) if task.destructive &&
                matches!(task.agent_target, Some(AgentTarget::Runner { .. })))
        {
            continue;
        }
        for (index, node) in approvals.into_iter().enumerate() {
            let NodeKind::Approval(approval) = &node.kind else {
                continue;
            };
            if approval.approves.is_none() {
                let options: Vec<_> = approval
                    .options
                    .iter()
                    .map(|option| option.id.as_str())
                    .collect();
                let checks_decision = task.guard.as_deref().is_some_and(|source| {
                    runbook_expr::template_requires_string_equality(
                        source,
                        &["steps", &node.id, "outputs", "decision"],
                        &options,
                    )
                });
                if !checks_decision {
                    errors.entry(node.id.as_str()).or_insert_with(|| crate::Diagnostic {
                        code: "spec.destructive_approval_options".into(),
                        path: format!("{}/approval", node.source_path),
                        message: Some(format!("Approval '{}' gates a destructive runner step. Add approves: [...] to name the decisions that grant permission.", node.id)),
                        line: 0,
                        column: 0,
                    });
                }
            }
            if index < outside_loop {
                warnings.entry(node.id.as_str()).or_insert_with(|| crate::Diagnostic {
                    code: "spec.destructive_approval_loop".into(),
                    path: format!("{}/approval", node.source_path),
                    message: Some(format!("Approval '{}' covers every iteration of the loop containing destructive runner step '{}'. Put the approval inside the loop to require a decision for each iteration.", node.id, task.id)),
                    line: 0,
                    column: 0,
                });
            }
        }
    }
    (
        errors.into_values().collect(),
        warnings.into_values().collect(),
    )
}

type Dominated<'a> = Vec<(&'a Node, Vec<&'a Node>, usize)>;

fn walk<'a>(ir: &'a NormalizedRunbook, out: &mut Dominated<'a>) {
    sequence(&ir.steps, vec![], 0, out);
    sequence(&ir.finally, vec![], 0, out);
}

fn sequence<'a>(
    nodes: &'a [Node],
    mut approvals: Vec<&'a Node>,
    outside_loop: usize,
    out: &mut Dominated<'a>,
) {
    for node in nodes {
        descend(node, &approvals, outside_loop, out);
        if human_approval(node) {
            approvals.push(node);
        }
    }
}

fn descend<'a>(
    node: &'a Node,
    approvals: &[&'a Node],
    outside_loop: usize,
    out: &mut Dominated<'a>,
) {
    match &node.kind {
        NodeKind::Task(_) => out.push((node, approvals.to_vec(), outside_loop)),
        NodeKind::Group(g) => {
            sequence(&g.children, approvals.to_vec(), outside_loop, out);
            sequence(&g.catch, approvals.to_vec(), outside_loop, out);
            sequence(&g.finally, approvals.to_vec(), outside_loop, out);
        }
        NodeKind::Parallel { children, .. } => {
            for child in children {
                descend(child, approvals, outside_loop, out);
            }
        }
        NodeKind::Foreach(g) => sequence(&g.children, approvals.to_vec(), approvals.len(), out),
        NodeKind::Loop(g) => sequence(&g.children, approvals.to_vec(), approvals.len(), out),
        NodeKind::Switch(s) => {
            for case in &s.cases {
                sequence(&case.children, approvals.to_vec(), outside_loop, out);
            }
            sequence(&s.default, approvals.to_vec(), outside_loop, out);
        }
        _ => (),
    }
}
