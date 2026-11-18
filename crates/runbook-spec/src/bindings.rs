//! Select immutable bindings from caller-resolved catalogs, without I/O.
//! Input IR must already have passed source admission and normalization.
#![allow(dead_code)]
use crate::{Diagnostic, ir::*};
use runbook_model::{TeamId, UserId};
use std::collections::BTreeMap;

pub(crate) struct Catalog<'a> {
    pub skills: &'a BTreeMap<String, [u8; 32]>,
    pub users: &'a BTreeMap<String, UserId>,
    pub teams: &'a BTreeMap<String, TeamId>,
}
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bindings {
    pub skills: BTreeMap<String, [u8; 32]>,
    pub users: BTreeMap<String, UserId>,
    pub teams: BTreeMap<String, TeamId>,
}
fn unresolved(code: &str, path: &str) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: None,
        path: path.into(),
        line: 0,
        column: 0,
    }
}
pub(crate) fn select(
    ir: &NormalizedRunbook,
    catalog: &Catalog<'_>,
) -> Result<Bindings, Diagnostic> {
    let mut selected = Bindings::default();
    for (index, owner) in ir.metadata.owners.iter().enumerate() {
        selected.recipient(owner, &format!("/metadata/owners/{index}"), catalog)?;
    }
    selected.nodes(&ir.steps, ir, catalog)?;
    selected.nodes(&ir.finally, ir, catalog)?;
    Ok(selected)
}
impl Bindings {
    fn recipient(
        &mut self,
        recipient: &Recipient,
        path: &str,
        catalog: &Catalog<'_>,
    ) -> Result<(), Diagnostic> {
        match recipient {
            Recipient::User(name) => {
                let id = catalog
                    .users
                    .get(name)
                    .ok_or_else(|| unresolved("spec.unresolved_user", path))?;
                self.users.insert(name.clone(), id.clone());
            }
            Recipient::Team(name) => {
                let id = catalog
                    .teams
                    .get(name)
                    .ok_or_else(|| unresolved("spec.unresolved_team", path))?;
                self.teams.insert(name.clone(), id.clone());
            }
        }
        Ok(())
    }
    fn nodes(
        &mut self,
        nodes: &[Node],
        ir: &NormalizedRunbook,
        catalog: &Catalog<'_>,
    ) -> Result<(), Diagnostic> {
        for node in nodes {
            // LANG9: escalation targets are pinned like approvers.
            if let Some(followup) = &node.followup {
                for (index, recipient) in followup.escalate_to.iter().enumerate() {
                    self.recipient(
                        recipient,
                        &format!("{}/escalate_to/{index}", node.source_path),
                        catalog,
                    )?;
                }
            }
            match &node.kind {
                NodeKind::Task(task) => {
                    for (index, reference) in task.skills.iter().enumerate() {
                        let path = if task.skills.len() == 1 {
                            format!("{}/skill", node.source_path)
                        } else {
                            format!("{}/skills/{index}", node.source_path)
                        };
                        // Aliases bind directly to catalog targets; never recursively
                        // follow alias strings, resolve latest, or fetch a URI here.
                        let target = reference
                            .starts_with("skill://")
                            .then_some(reference.as_str())
                            .or_else(|| ir.skills.get(reference).map(String::as_str))
                            .ok_or_else(|| unresolved("spec.unresolved_skill", &path))?;
                        let hash = catalog
                            .skills
                            .get(target)
                            .ok_or_else(|| unresolved("spec.unresolved_skill", &path))?;
                        self.skills.insert(reference.clone(), *hash);
                    }
                    if let Some(Assignee::Recipient(recipient)) = &task.assignee {
                        self.recipient(
                            recipient,
                            &format!("{}/assignee", node.source_path),
                            catalog,
                        )?;
                    }
                }
                NodeKind::Approval(approval) => {
                    let mut user_index = 0;
                    let mut team_index = 0;
                    for recipient in &approval.approvers {
                        let (kind, index) = match recipient {
                            Recipient::User(_) => ("users", &mut user_index),
                            Recipient::Team(_) => ("teams", &mut team_index),
                        };
                        self.recipient(
                            recipient,
                            &format!("{}/approval/approvers/{kind}/{index}", node.source_path),
                            catalog,
                        )?;
                        *index += 1;
                    }
                    if let Some(recipients) = &approval.escalate_to {
                        for (index, recipient) in recipients.iter().enumerate() {
                            self.recipient(
                                recipient,
                                &format!("{}/approval/escalate_to/{index}", node.source_path),
                                catalog,
                            )?;
                        }
                    }
                }
                NodeKind::Group(group) => {
                    self.nodes(&group.children, ir, catalog)?;
                    self.nodes(&group.catch, ir, catalog)?;
                    self.nodes(&group.finally, ir, catalog)?;
                }
                NodeKind::Parallel { children, .. } => self.nodes(children, ir, catalog)?,
                NodeKind::Foreach(each) => self.nodes(&each.children, ir, catalog)?,
                NodeKind::Loop(repeat) => self.nodes(&repeat.children, ir, catalog)?,
                NodeKind::Switch(switch) => {
                    for case in &switch.cases {
                        self.nodes(&case.children, ir, catalog)?;
                    }
                    self.nodes(&switch.default, ir, catalog)?;
                }
                NodeKind::Wait(_) | NodeKind::Set(_) | NodeKind::End(_) | NodeKind::Call(_) => (),
            }
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "bindings/tests.rs"]
mod tests;
