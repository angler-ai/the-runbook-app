//! Approval declaration and destructive-path checks over sealed semantics.
use crate::{Diagnostic, ir::*};

/// Requires admitted normalized IR. This checks declarations even in guarded
/// branches; it neither evaluates conditions nor decides destructive reachability.
#[cfg(test)]
pub(crate) fn validate_automatic_options(ir: &NormalizedRunbook) -> Vec<Diagnostic> {
    let mut diagnostics = vec![];
    visit(&ir.steps, &mut diagnostics);
    visit(&ir.finally, &mut diagnostics);
    diagnostics
}
#[cfg(test)]
fn visit(nodes: &[Node], diagnostics: &mut Vec<Diagnostic>) {
    for node in nodes {
        match &node.kind {
            NodeKind::Approval(approval) => {
                if let ApprovalTimeout::Decide(id) = &approval.on_timeout
                    && !automatic_option(approval, id)
                {
                    diagnostics.push(Diagnostic {
                        code: "spec.auto_option_required".into(),
                        message: Some(
                            "Automatic timeout decisions must name an option declared auto: true."
                                .into(),
                        ),
                        path: format!("{}/approval/on_timeout/decide", node.source_path),
                        line: 0,
                        column: 0,
                    });
                }
            }
            NodeKind::Group(group) => {
                visit(&group.children, diagnostics);
                visit(&group.catch, diagnostics);
                visit(&group.finally, diagnostics);
            }
            NodeKind::Parallel { children, .. } => visit(children, diagnostics),
            NodeKind::Foreach(each) => visit(&each.children, diagnostics),
            NodeKind::Loop(repeat) => visit(&repeat.children, diagnostics),
            NodeKind::Switch(switch) => {
                for case in &switch.cases {
                    visit(&case.children, diagnostics);
                }
                visit(&switch.default, diagnostics);
            }
            NodeKind::Task(_)
            | NodeKind::Wait(_)
            | NodeKind::Set(_)
            | NodeKind::End(_)
            | NodeKind::Call(_) => (),
        }
    }
}
#[cfg(test)]
mod tests;

#[path = "policy/destructive_reach.rs"]
pub(crate) mod destructive_reach;
#[path = "policy/region.rs"]
mod region;
fn collect<'a>(body: &'a [Node], nodes: &mut Vec<&'a Node>) {
    for n in body {
        nodes.push(n);
        match &n.kind {
            NodeKind::Group(g) => {
                collect(&g.children, nodes);
                collect(&g.catch, nodes);
                collect(&g.finally, nodes);
            }
            NodeKind::Parallel { children, .. } => collect(children, nodes),
            NodeKind::Foreach(g) => collect(&g.children, nodes),
            NodeKind::Loop(g) => collect(&g.children, nodes),
            NodeKind::Switch(s) => {
                for c in &s.cases {
                    collect(&c.children, nodes);
                }
                collect(&s.default, nodes);
            }
            _ => (),
        }
    }
}
fn automatic_option(approval: &Approval, id: &str) -> bool {
    approval.options.iter().any(|o| o.id == id && o.auto)
}
fn diagnostic(node: &Node, field: &str, code: &str, message: Option<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message,
        path: format!("{}/approval/{field}", node.source_path),
        line: 0,
        column: 0,
    }
}
/// Publication policy over checked immutable semantics. This does not consult
/// live team membership, evaluate conditions, or perform any runtime effects.
pub fn validate_policy(compiled: &crate::CompiledRunbook) -> Vec<Diagnostic> {
    let ir = compiled.ir();
    let mut nodes = vec![];
    collect(&ir.steps, &mut nodes);
    collect(&ir.finally, &mut nodes);
    let mut errors = vec![];
    for node in &nodes {
        for (invalid, field, code) in [
            (
                node.timeout.is_some() && !matches!(node.kind, NodeKind::Task(_)),
                "timeout",
                "spec.timeout_requires_task",
            ),
            (
                matches!(node.kind, NodeKind::End(_))
                    && node.source_path.split('/').any(|part| part == "finally"),
                "end",
                "spec.end_in_finally",
            ),
        ] {
            if invalid {
                errors.push(Diagnostic {
                    code: code.into(),
                    message: None,
                    path: format!("{}/{field}", node.source_path),
                    line: 0,
                    column: 0,
                });
            }
        }
        if node.on_failure == FailurePolicy::Handoff && !matches!(node.kind, NodeKind::Task(_)) {
            errors.push(Diagnostic {
                code: "spec.handoff_requires_task".into(),
                message: Some("Failure handoff is only supported for Tasks.".into()),
                path: format!("{}/on_failure", node.source_path),
                line: 0,
                column: 0,
            });
        }
        if let NodeKind::Call(call) = &node.kind
            && call
                .target
                .as_ref()
                .is_some_and(|target| target.agent_decidable.unwrap_or(true))
            && let Some(task) = region::analyze(ir, &node.id, false, true).agent
        {
            errors.push(Diagnostic {
                code: "spec.agent_approval_destructive".into(),
                message: Some(format!(
                    "An approval in the called runbook can reach destructive task '{}' at {}.",
                    task.id, task.source_path
                )),
                path: format!("{}/call", node.source_path),
                line: 0,
                column: 0,
            });
        }
        let NodeKind::Approval(approval) = &node.kind else {
            continue;
        };
        if approval.allow_agents && approval.min != 1 {
            errors.push(diagnostic(
                node,
                "min",
                "spec.agent_approval_requires_min_one",
                None,
            ));
        }
        let auto = match &approval.on_timeout {
            ApprovalTimeout::Decide(id) => {
                let valid = automatic_option(approval, id);
                if !valid {
                    errors.push(diagnostic(
                        node,
                        "on_timeout/decide",
                        "spec.auto_option_required",
                        None,
                    ));
                }
                valid
            }
            ApprovalTimeout::Escalate => false,
        };
        if approval.approvers.is_empty() {
            errors.push(diagnostic(
                node,
                "approvers",
                "spec.approvers_required",
                None,
            ));
        }
        if approval.timeout.is_some()
            && matches!(approval.on_timeout, ApprovalTimeout::Escalate)
            && approval
                .escalate_to
                .as_ref()
                .unwrap_or(&ir.metadata.owners)
                .is_empty()
        {
            errors.push(diagnostic(
                node,
                "escalate_to",
                "spec.escalation_recipients_required",
                None,
            ));
        }
        if auto || approval.allow_agents {
            let found = region::analyze(ir, &node.id, auto, approval.allow_agents);
            debug_assert!(found.memo_entries <= nodes.len() * 3);
            if let Some(task) = found.auto {
                errors.push(diagnostic(node,"on_timeout/decide","spec.auto_decision_destructive",Some(format!("Automatic decision can reach destructive task '{}' at {} without allow_auto_decision.",task.id,task.source_path))));
            }
            if let Some(task) = found.agent {
                errors.push(diagnostic(
                    node,
                    "allow_agents",
                    "spec.agent_approval_destructive",
                    Some(format!(
                        "Agent approval can reach destructive task '{}' at {}.",
                        task.id, task.source_path
                    )),
                ));
            }
        }
    }
    errors
}

/// Whether an approval decision can lead to a destructive task or child call.
pub fn approval_reaches_destructive(ir: &crate::ir::NormalizedRunbook, node_id: &str) -> bool {
    region::analyze(ir, node_id, false, true).agent.is_some()
}
