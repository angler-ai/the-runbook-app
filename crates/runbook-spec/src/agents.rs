//! R0-3: resolve agent steps against the project's agent directory at
//! publish (runner design §6.3) and check them against the agent's
//! ceiling and the destructive-reach rules (§6.5). Pure: the directory
//! arrives in the [`crate::CompileContext`], so publish needs no call to
//! the runner and works while it is offline.
use crate::{
    Diagnostic,
    ir::*,
    policy::destructive_reach::{dominated_tasks, reach},
};
use runbook_model::{AgentDirectoryEntry, AgentStatus, EXTERNAL_AGENT, RunnerLabel};
use std::collections::BTreeMap;

fn diagnostic(code: &str, path: String, message: Option<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message,
        path,
        line: 0,
        column: 0,
    }
}

/// Resolve every agent step's target (the step's `agent.name`, else
/// `spec.defaults.agent`, else the project's default agent), map step
/// `connections` to credential IDs, and check limits and reach. Returns
/// every problem found.
pub(crate) fn resolve(
    ir: &mut NormalizedRunbook,
    directory: &BTreeMap<String, AgentDirectoryEntry>,
    default_agent: Option<&str>,
) -> Result<(), Vec<Diagnostic>> {
    let dominated = dominated_tasks(ir);
    let mut errors = vec![];
    let mut visit = |node: &mut Node| {
        if let NodeKind::Approval(approval) = &mut node.kind {
            let path = format!("{}/approval", node.source_path);
            if let Ok(Some(entry)) = resolve_target(
                &path,
                &mut approval.agent_target,
                directory,
                false,
                &mut errors,
            ) && let Some(limits) = &approval.agent_limits
            {
                check_limits(
                    limits,
                    entry,
                    &|key, message| {
                        diagnostic(
                            "spec.agent_outside_ceiling",
                            format!("{path}/{key}"),
                            Some(message),
                        )
                    },
                    &mut errors,
                );
            }
            return;
        }
        let NodeKind::Task(task) = &mut node.kind else {
            return;
        };
        check_task(
            &node.source_path,
            task,
            directory,
            default_agent,
            dominated.get(node.source_path.as_str()).copied(),
            &mut errors,
        );
    };
    for_each_node(&mut ir.steps, &mut visit);
    for_each_node(&mut ir.finally, &mut visit);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn for_each_node(nodes: &mut [Node], f: &mut impl FnMut(&mut Node)) {
    for node in nodes {
        f(node);
        match &mut node.kind {
            NodeKind::Group(g) => {
                for_each_node(&mut g.children, f);
                for_each_node(&mut g.catch, f);
                for_each_node(&mut g.finally, f);
            }
            NodeKind::Parallel { children, .. } => for_each_node(children, f),
            NodeKind::Foreach(g) => for_each_node(&mut g.children, f),
            NodeKind::Loop(g) => for_each_node(&mut g.children, f),
            NodeKind::Switch(s) => {
                for c in &mut s.cases {
                    for_each_node(&mut c.children, f);
                }
                for_each_node(&mut s.default, f);
            }
            _ => (),
        }
    }
}

fn resolve_target<'a>(
    path: &str,
    target: &mut Option<AgentTarget>,
    directory: &'a BTreeMap<String, AgentDirectoryEntry>,
    external: bool,
    errors: &mut Vec<Diagnostic>,
) -> Result<Option<&'a AgentDirectoryEntry>, ()> {
    Ok(match target.take() {
        Some(AgentTarget::Named { name }) if external && name == EXTERNAL_AGENT => {
            *target = Some(AgentTarget::External);
            None
        }
        Some(AgentTarget::Named { name }) => {
            match directory
                .get(&name)
                .filter(|e| e.status != AgentStatus::Archived && e.name != EXTERNAL_AGENT)
            {
                Some(entry) => {
                    *target = Some(AgentTarget::Runner {
                        agent_id: entry.agent_id.clone(),
                        name,
                        group_id: entry.group_id.to_string(),
                    });
                    Some(entry)
                }
                None => {
                    errors.push(diagnostic(
                        "spec.agent_unknown",
                        format!("{path}/agent/name"),
                        Some(format!(
                            "No runner agent named '{name}' in this project's agent directory."
                        )),
                    ));
                    return Err(());
                }
            }
        }
        other => {
            *target = other;
            None
        }
    })
}
fn check_limits(
    limits: &AgentLimits,
    entry: &AgentDirectoryEntry,
    outside: &impl Fn(&str, String) -> Diagnostic,
    errors: &mut Vec<Diagnostic>,
) {
    let ceiling = &entry.ceiling;
    if let Some(model) = &limits.model
        && !ceiling.profiles.contains(model)
    {
        errors.push(outside(
            "agent/model",
            format!(
                "Agent '{}' does not allow model profile '{model}' (allowed: {}).",
                entry.name,
                ceiling.profiles.join(", ")
            ),
        ));
    }
    if let Some(cost) = limits.max_cost_usd
        && cost > ceiling.max_cost_usd
    {
        errors.push(outside(
            "agent/max_cost_usd",
            format!(
                "max_cost_usd {cost} is above agent '{}''s ceiling of {}.",
                entry.name, ceiling.max_cost_usd
            ),
        ));
    }
    if let Some(tokens) = limits.max_tokens
        && tokens > ceiling.max_tokens
    {
        errors.push(outside(
            "agent/max_tokens",
            format!(
                "max_tokens {tokens} is above agent '{}''s ceiling of {}.",
                entry.name, ceiling.max_tokens
            ),
        ));
    }
}

fn check_task(
    path: &str,
    task: &mut Task,
    directory: &BTreeMap<String, AgentDirectoryEntry>,
    default_agent: Option<&str>,
    dominated: Option<bool>,
    errors: &mut Vec<Diagnostic>,
) {
    if task.actor == Actor::Agent
        && task.agent_target.is_none()
        && let Some(name) = default_agent
    {
        task.agent_target = Some(AgentTarget::Named { name: name.into() });
    }
    let Ok(entry) = resolve_target(path, &mut task.agent_target, directory, true, errors) else {
        return;
    };
    let outside = |key: &str, message: String| {
        diagnostic(
            "spec.agent_outside_ceiling",
            format!("{path}/{key}"),
            Some(message),
        )
    };
    let limits = task.agent_limits.clone().unwrap_or_default();
    let Some(entry) = entry else {
        // Runner keys need a runner agent to bound them.
        let key = if limits.model.is_some() {
            Some("agent/model")
        } else if limits.max_cost_usd.is_some() {
            Some("agent/max_cost_usd")
        } else if limits.max_tokens.is_some() {
            Some("agent/max_tokens")
        } else if limits.network.is_some() {
            Some("agent/network")
        } else if task.connections.is_some() {
            Some("connections")
        } else {
            None
        };
        if let Some(key) = key {
            errors.push(outside(
                key,
                "Only a step aimed at a runner agent (agent.name) can set this.".into(),
            ));
        }
        return;
    };
    let ceiling = &entry.ceiling;
    let before = errors.len();
    check_limits(&limits, entry, &outside, errors);
    if let Some(host) = limits
        .network
        .iter()
        .flatten()
        .find(|h| !ceiling.hosts.iter().any(|c| &c.host == *h))
    {
        errors.push(outside(
            "agent/network",
            format!("Agent '{}' cannot reach host '{host}'.", entry.name),
        ));
    }
    let mut ids = vec![];
    for name in task.connections.iter().flatten() {
        match ceiling.credentials.iter().find(|c| &c.name == name) {
            Some(credential) => ids.push(credential.id.clone()),
            None => errors.push(outside(
                "connections",
                format!("Agent '{}' has no credential '{name}'.", entry.name),
            )),
        }
    }
    if errors.len() > before {
        return;
    }
    task.connections = task.connections.as_ref().map(|_| ids);
    // §6.5: what the step can reach decides whether it is destructive.
    let (label, items) = reach(
        ceiling,
        task.connections.as_deref(),
        limits.network.as_deref(),
    );
    if label == RunnerLabel::Destructive && !task.destructive {
        errors.push(diagnostic(
            "spec.destructive_reach",
            format!("{path}/destructive"),
            Some(format!(
                "This step can use destructive tools or credentials ({}): mark it destructive or narrow its tools.",
                items.join(", ")
            )),
        ));
    }
    if task.destructive && dominated != Some(true) {
        errors.push(diagnostic(
            "spec.destructive_needs_approval",
            path.to_owned(),
            Some(
                "A destructive step aimed at a runner agent must come after a person's approval in the same run, on every path to it."
                    .into(),
            ),
        ));
    }
}

/// What publish resolved, which a stored IR carries beside its source:
/// `spec.defaults.agent` and each task's target and credential IDs.
pub(crate) struct Pins {
    defaults: Option<String>,
    tasks: Vec<(Option<AgentTarget>, Option<Vec<String>>)>,
    approvals: Vec<Option<AgentTarget>>,
}
/// Take the resolution out of a stored IR, so the rest compares with the
/// IR its lowered source normalizes to (as call pins do).
pub(crate) fn take_pins(ir: &mut NormalizedRunbook) -> Pins {
    let mut tasks = vec![];
    let mut approvals = vec![];
    let mut take = |node: &mut Node| {
        if let NodeKind::Task(task) = &mut node.kind {
            tasks.push((task.agent_target.take(), task.connections.take()));
        } else if let NodeKind::Approval(approval) = &mut node.kind {
            let target = approval.agent_target.take();
            if let Some(AgentTarget::Runner { name, .. }) = &target {
                approval.agent_target = Some(AgentTarget::Named { name: name.clone() });
            }
            approvals.push(target);
        }
    };
    for_each_node(&mut ir.steps, &mut take);
    for_each_node(&mut ir.finally, &mut take);
    Pins {
        defaults: ir.defaults.agent.take(),
        tasks,
        approvals,
    }
}
/// A stored resolution is complete: no name left unresolved.
pub(crate) fn valid_pins(pins: &Pins) -> bool {
    pins.tasks
        .iter()
        .all(|(target, _)| !matches!(target, Some(AgentTarget::Named { .. })))
        && pins
            .approvals
            .iter()
            .all(|target| matches!(target, None | Some(AgentTarget::Runner { .. })))
}
pub(crate) fn put_pins(ir: &mut NormalizedRunbook, pins: Pins) {
    ir.defaults.agent = pins.defaults;
    let mut tasks = pins.tasks.into_iter();
    let mut approvals = pins.approvals.into_iter();
    let mut put = |node: &mut Node| {
        if let NodeKind::Task(task) = &mut node.kind
            && let Some((target, connections)) = tasks.next()
        {
            task.agent_target = target;
            task.connections = connections;
        } else if let NodeKind::Approval(approval) = &mut node.kind {
            approval.agent_target = approvals.next().flatten();
        }
    };
    for_each_node(&mut ir.steps, &mut put);
    for_each_node(&mut ir.finally, &mut put);
}
