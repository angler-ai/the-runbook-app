//! Resolve static escalation recipients using only sealed compiler bindings.
use crate::{
    CompiledRunbook, Diagnostic,
    ir::{Node, NodeKind, Recipient},
};
use runbook_model::RecipientRef;

fn diagnostic(code: &str, path: &str) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: None,
        path: path.into(),
        line: 0,
        column: 0,
    }
}

/// Use explicit recipients when supplied, otherwise the runbook owners.
/// An empty result is a routing error; it never broadens eligibility.
pub fn resolve_escalation(
    explicit: &[RecipientRef],
    owners: &[RecipientRef],
) -> Result<Vec<RecipientRef>, Diagnostic> {
    let selected = if explicit.is_empty() {
        owners
    } else {
        explicit
    };
    if selected.is_empty() {
        Err(diagnostic("approval.no_escalation_target", ""))
    } else {
        Ok(selected.to_vec())
    }
}

/// Resolve an approval node's static escalation targets to pinned identities.
/// Omission falls back to owners; an explicitly empty declaration remains invalid.
/// This reads no directory and does not expand team membership.
pub fn resolve_approval_escalation(
    compiled: &CompiledRunbook,
    node: &Node,
) -> Result<Vec<RecipientRef>, Diagnostic> {
    let path = format!("{}/approval/escalate_to", node.source_path);
    let NodeKind::Approval(approval) = &node.kind else {
        return Err(diagnostic("approval.not_approval", &node.source_path));
    };
    let selected = approval
        .escalate_to
        .as_deref()
        .unwrap_or(&compiled.ir().metadata.owners);
    if selected.is_empty() {
        return Err(diagnostic("approval.no_escalation_target", &path));
    }
    selected
        .iter()
        .map(|recipient| match recipient {
            Recipient::User(name) => compiled
                .users()
                .get(name)
                .cloned()
                .map(RecipientRef::User)
                .ok_or_else(|| diagnostic("spec.unresolved_user", &path)),
            Recipient::Team(name) => compiled
                .teams()
                .get(name)
                .map(|id| RecipientRef::Team(id.to_string()))
                .ok_or_else(|| diagnostic("spec.unresolved_team", &path)),
        })
        .collect()
}

/// LANG9: a task's or wait's follow-up escalation targets, resolved to
/// pinned identities. Reads no directory and doesn't expand teams.
pub fn resolve_followup_escalation(
    compiled: &CompiledRunbook,
    node: &Node,
) -> Result<Vec<RecipientRef>, Diagnostic> {
    let path = format!("{}/escalate_to", node.source_path);
    let selected = node
        .followup
        .as_ref()
        .map(|f| f.escalate_to.as_slice())
        .unwrap_or_default();
    if selected.is_empty() {
        return Err(diagnostic("step.no_escalation_target", &path));
    }
    selected
        .iter()
        .map(|recipient| match recipient {
            Recipient::User(name) => compiled
                .users()
                .get(name)
                .cloned()
                .map(RecipientRef::User)
                .ok_or_else(|| diagnostic("spec.unresolved_user", &path)),
            Recipient::Team(name) => compiled
                .teams()
                .get(name)
                .map(|id| RecipientRef::Team(id.to_string()))
                .ok_or_else(|| diagnostic("spec.unresolved_team", &path)),
        })
        .collect()
}
