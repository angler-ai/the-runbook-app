//! The outline: the read-only tree the Graph (A7) and Outline (A8) views draw.
//! The shape is fixed by `Outline`/`OutlineNode` in `web/src/ui/types.ts`.
use runbook_spec::ir::{
    Actor, ApprovalTimeout, Assignee, EndStatus, FailurePolicy, Instructions, Join, Node, NodeKind,
    NormalizedRunbook, Recipient, WaitKind,
};
use runbook_spec::{DataSchema, DataType};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outline {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub steps: Vec<OutlineNode>,
    pub finally: Vec<OutlineNode>,
    /// The worst-case step-run count; null when the budget cannot be bounded.
    pub step_budget: Option<u64>,
}

/// One step. Every field after `branches` is optional detail for the graph's
/// node cards and side panel (UI-2); readers must not require any of it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineNode {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// RFC 6901 pointer to the step in the source, as diagnostics use.
    pub path: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<&'static str>,
    #[serde(skip_serializing_if = "is_false")]
    pub check: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub destructive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<OutlineNode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches: Option<Vec<OutlineBranch>>,
    /// The step's `if`, as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
    /// `continue` or `handoff`; omitted for the default `fail`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<&'static str>,
    /// The step timeout (task defaults applied), compact (`20m`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// Task instructions as written (the default audience's text when they
    /// are split by audience).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Task assignee as written (`team:oncall`, `user:Alice` or an expression).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<String>,
    /// Declared outputs of a task or call, in name order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutlineOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<OutlineApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<OutlineWait>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join: Option<OutlineJoin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreach: Option<OutlineForeach>,
    #[serde(rename = "loop", skip_serializing_if = "Option::is_none")]
    pub r#loop: Option<OutlineLoop>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<OutlineCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<OutlineSet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<OutlineEnd>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineBranch {
    pub label: String,
    /// `case`, `default`, `catch` or `finally`.
    pub kind: &'static str,
    /// A switch case's condition as written; `label` is its expression text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    pub children: Vec<OutlineNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineOutput {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "is_false")]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineApproval {
    pub prompt: String,
    pub options: Vec<OutlineOption>,
    pub approvers: Vec<String>,
    pub min: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// `escalate` or `decide`, only when there is a timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<&'static str>,
    /// The option `on_timeout: {decide}` picks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalate_to: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_timeout: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub allow_agents: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineOption {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "is_false")]
    pub auto: bool,
}

/// `kind` is `signal`, `for` or `until`; exactly the matching field is set.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineWait {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

/// `kind` is `all`, `any` or `at_least` (with `count`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineJoin {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineForeach {
    pub items: String,
    #[serde(rename = "as")]
    pub alias: String,
    pub concurrency: u32,
    pub max_items: u32,
    /// UI-3: the `key:` expression as written; omitted when iterations are
    /// keyed by the item's index (the run page then names them by item).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineLoop {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(rename = "while", skip_serializing_if = "Option::is_none")]
    pub r#while: Option<String>,
    pub max_iterations: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineCall {
    /// The child runbook's slug, as written.
    pub runbook: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// The child inputs the step supplies, by name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineSet {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineEnd {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !b
}

/// Build the outline of a normalized runbook. Pure and order-preserving.
pub fn outline(ir: &NormalizedRunbook) -> Outline {
    Outline {
        id: ir.metadata.id.clone(),
        title: ir.metadata.title.clone(),
        description: ir.metadata.description.clone(),
        steps: nodes(&ir.steps),
        finally: nodes(&ir.finally),
        step_budget: runbook_spec::estimate_step_runs(ir).ok(),
    }
}

fn nodes(nodes: &[Node]) -> Vec<OutlineNode> {
    nodes.iter().map(node).collect()
}

fn branch(label: String, kind: &'static str, body: &[Node]) -> OutlineBranch {
    OutlineBranch {
        label,
        kind,
        when: None,
        children: nodes(body),
    }
}

fn node(n: &Node) -> OutlineNode {
    let mut out = OutlineNode {
        id: n.id.clone(),
        title: n.title.clone(),
        path: n.source_path.clone(),
        kind: "",
        actor: None,
        check: false,
        destructive: false,
        hint: None,
        children: None,
        branches: None,
        guard: n.guard.clone(),
        on_failure: match n.on_failure {
            FailurePolicy::Fail => None,
            FailurePolicy::Continue => Some("continue"),
            FailurePolicy::Handoff => Some("handoff"),
        },
        timeout: n.timeout.map(|d| duration(d.millis)),
        instructions: None,
        assignee: None,
        skill: None,
        skills: vec![],
        checklist: vec![],
        outputs: vec![],
        approval: None,
        wait: None,
        join: None,
        foreach: None,
        r#loop: None,
        call: None,
        set: None,
        end: None,
    };
    let (kind, hint) = match &n.kind {
        NodeKind::Task(t) => {
            out.actor = Some(actor(t.actor));
            out.check = t.check;
            out.destructive = t.destructive;
            out.instructions = t.instructions.as_ref().and_then(|i| match i {
                Instructions::Markdown(s) => Some(s.clone()),
                Instructions::Audience {
                    default,
                    human,
                    agent,
                } => default.clone().or(human.clone()).or(agent.clone()),
            });
            out.assignee = t.assignee.as_ref().map(|a| match a {
                Assignee::Recipient(r) => recipient(r),
                Assignee::Expression(e) => e.clone(),
            });
            out.skill = t.skill.clone();
            out.skills = if t.skills.len() > 1 {
                t.skills.clone()
            } else {
                vec![]
            };
            out.checklist = t.checklist.clone();
            out.outputs = outputs(&t.outputs);
            let who = match t.actor {
                Actor::Human => "Human",
                Actor::Agent => "Agent",
                Actor::Any => "Anyone",
            };
            let hint = if t.destructive {
                format!("{who} · destructive")
            } else if t.check {
                format!("{who} · check")
            } else {
                who.to_owned()
            };
            ("task", hint)
        }
        NodeKind::Group(g) => {
            out.children = Some(nodes(&g.children));
            let extra: Vec<OutlineBranch> = [("catch", &g.catch), ("finally", &g.finally)]
                .into_iter()
                .filter(|(_, body)| !body.is_empty())
                .map(|(label, body)| branch(label.into(), label, body))
                .collect();
            if !extra.is_empty() {
                out.branches = Some(extra);
            }
            ("group", "Group".into())
        }
        NodeKind::Parallel { children, join, .. } => {
            out.children = Some(nodes(children));
            out.join = Some(match join {
                Join::All => OutlineJoin {
                    kind: "all",
                    count: None,
                },
                Join::AtLeast(1) => OutlineJoin {
                    kind: "any",
                    count: None,
                },
                Join::AtLeast(n) => OutlineJoin {
                    kind: "at_least",
                    count: Some(*n),
                },
            });
            let hint = match join {
                Join::All => "Run in parallel".into(),
                Join::AtLeast(1) => "Run in parallel until one succeeds".into(),
                Join::AtLeast(n) => {
                    format!("Run in parallel until {n} succeed")
                }
            };
            ("parallel", hint)
        }
        NodeKind::Foreach(f) => {
            out.children = Some(nodes(&f.children));
            out.foreach = Some(OutlineForeach {
                items: f.items.clone(),
                alias: f.alias.clone(),
                concurrency: f.concurrency,
                max_items: f.max_items,
                key: f.key.clone(),
            });
            ("foreach", "For each item".into())
        }
        NodeKind::Loop(l) => {
            out.children = Some(nodes(&l.children));
            out.r#loop = Some(OutlineLoop {
                until: l.until.clone(),
                r#while: l.r#while.clone(),
                max_iterations: l.max_iterations,
                delay: l.delay.map(|d| duration(d.millis)),
            });
            let plural = if l.max_iterations == 1 { "" } else { "s" };
            (
                "loop",
                format!("Repeat up to {} time{plural}", l.max_iterations),
            )
        }
        NodeKind::Switch(s) => {
            // Cases have no names in the source: label them by their
            // condition. The default branch is always present (possibly
            // empty) because the switch falls through when no case matches.
            let mut branches: Vec<OutlineBranch> = s
                .cases
                .iter()
                .map(|c| OutlineBranch {
                    label: expression_text(&c.when),
                    kind: "case",
                    when: Some(c.when.clone()),
                    children: nodes(&c.children),
                })
                .collect();
            branches.push(branch("default".into(), "default", &s.default));
            out.branches = Some(branches);
            ("switch", "Decision".into())
        }
        NodeKind::Wait(w) => {
            let timeout = w.timeout.map(|d| duration(d.millis));
            let (wait, hint) = match &w.kind {
                WaitKind::For(d) => (
                    OutlineWait {
                        kind: "for",
                        signal: None,
                        duration: Some(duration(d.millis)),
                        until: None,
                        timeout,
                    },
                    format!("Wait {}", duration(d.millis)),
                ),
                WaitKind::Until(t) => (
                    OutlineWait {
                        kind: "until",
                        signal: None,
                        duration: None,
                        until: Some(t.clone()),
                        timeout,
                    },
                    "Wait until a time".into(),
                ),
                WaitKind::Signal(s) => (
                    OutlineWait {
                        kind: "signal",
                        signal: Some(s.clone()),
                        duration: None,
                        until: None,
                        timeout,
                    },
                    "Wait for a signal".into(),
                ),
            };
            out.wait = Some(wait);
            ("wait", hint)
        }
        NodeKind::Approval(a) => {
            out.actor = Some(if a.allow_agents { "any" } else { "human" });
            let timeout = a.timeout.map(|d| duration(d.millis));
            out.approval = Some(OutlineApproval {
                prompt: a.prompt.clone(),
                options: a
                    .options
                    .iter()
                    .map(|o| OutlineOption {
                        id: o.id.clone(),
                        title: o.title.clone(),
                        auto: o.auto,
                    })
                    .collect(),
                approvers: a.approvers.iter().map(recipient).collect(),
                min: a.min,
                on_timeout: timeout.as_ref().map(|_| match a.on_timeout {
                    ApprovalTimeout::Escalate => "escalate",
                    ApprovalTimeout::Decide(_) => "decide",
                }),
                timeout_decision: match &a.on_timeout {
                    ApprovalTimeout::Decide(option) if timeout.is_some() => Some(option.clone()),
                    _ => None,
                },
                timeout,
                escalation_timeout: a
                    .escalate_to
                    .as_ref()
                    .and(a.escalation_timeout)
                    .map(|d| duration(d.millis)),
                escalate_to: a
                    .escalate_to
                    .as_ref()
                    .map(|to| to.iter().map(recipient).collect()),
                allow_agents: a.allow_agents,
            });
            ("approval", "Approval".into())
        }
        // LANG4: another runbook as a child run.
        NodeKind::Call(c) => {
            out.outputs = outputs(&c.outputs);
            out.call = Some(OutlineCall {
                runbook: c.runbook.clone(),
                version: c.version,
                with: c.with.keys().cloned().collect(),
            });
            (
                "call",
                match c.version {
                    Some(n) => format!("Run {} v{n}", c.runbook),
                    None => format!("Run {}", c.runbook),
                },
            )
        }
        NodeKind::Set(s) => {
            out.set = Some(OutlineSet {
                fields: s.fields.keys().cloned().collect(),
                vars: s.variables.keys().cloned().collect(),
            });
            (
                "set",
                match (s.fields.is_empty(), s.variables.is_empty()) {
                    (false, false) => "Set fields and vars",
                    (false, true) => "Set fields",
                    (true, false) => "Set vars",
                    (true, true) => "Set",
                }
                .into(),
            )
        }
        NodeKind::End(e) => {
            let status = match e.status {
                EndStatus::Succeeded => "succeeded",
                EndStatus::Failed => "failed",
                EndStatus::Cancelled => "cancelled",
            };
            out.end = Some(OutlineEnd {
                status,
                message: e.message.clone(),
            });
            ("end", format!("End: {status}"))
        }
    };
    out.kind = kind;
    out.hint = Some(hint);
    out
}

fn recipient(r: &Recipient) -> String {
    match r {
        Recipient::User(name) => format!("user:{name}"),
        Recipient::Team(name) => format!("team:{name}"),
    }
}

/// A condition's expression text: `${{ x }}` becomes `x`; anything else
/// (several templates, or literal text) stays as written.
fn expression_text(source: &str) -> String {
    let trimmed = source.trim();
    if let Some(inner) = trimmed
        .strip_prefix("${{")
        .and_then(|rest| rest.strip_suffix("}}"))
        && !inner.contains("${{")
        && !inner.contains("}}")
    {
        return inner.trim().to_owned();
    }
    source.to_owned()
}

/// Declared outputs (an object schema), in name order.
fn outputs(schema: &DataSchema) -> Vec<OutlineOutput> {
    match &schema.kind {
        DataType::Object { properties } => properties
            .iter()
            .map(|(name, s)| OutlineOutput {
                name: name.clone(),
                kind: type_name(&s.kind),
                required: s.required,
            })
            .collect(),
        _ => vec![],
    }
}

fn type_name(kind: &DataType) -> &'static str {
    match kind {
        DataType::String => "string",
        DataType::Text => "text",
        DataType::Number => "number",
        DataType::Integer => "integer",
        DataType::Boolean => "boolean",
        DataType::Enum { .. } => "enum",
        DataType::MultiEnum { .. } => "multi_enum",
        DataType::List { .. } => "list",
        DataType::Object { .. } => "object",
        DataType::Date => "date",
        DataType::DateTime => "datetime",
        DataType::Duration => "duration",
        DataType::Url => "url",
        DataType::Email => "email",
        DataType::User => "user",
    }
}

fn actor(a: Actor) -> &'static str {
    match a {
        Actor::Human => "human",
        Actor::Agent => "agent",
        Actor::Any => "any",
    }
}

/// Compact duration text, e.g. `5m`, `1h30m`, `250ms`.
fn duration(millis: u64) -> String {
    if millis == 0 {
        return "0s".into();
    }
    let mut left = millis;
    let mut out = String::new();
    for (unit, size) in [
        ("d", 86_400_000),
        ("h", 3_600_000),
        ("m", 60_000),
        ("s", 1_000),
        ("ms", 1),
    ] {
        if left >= size {
            out.push_str(&format!("{}{unit}", left / size));
            left %= size;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn conditions_lose_only_a_single_wrapping_template() {
        for (source, text) in [
            ("${{ inputs.risk == 'high' }}", "inputs.risk == 'high'"),
            ("  ${{x}}  ", "x"),
            ("${{ a }} and ${{ b }}", "${{ a }} and ${{ b }}"),
            ("true", "true"),
        ] {
            assert_eq!(super::expression_text(source), text);
        }
    }

    #[test]
    fn durations_are_compact() {
        for (millis, text) in [
            (0, "0s"),
            (250, "250ms"),
            (300_000, "5m"),
            (5_400_000, "1h30m"),
            (90_061_001, "1d1h1m1s1ms"),
        ] {
            assert_eq!(super::duration(millis), text);
        }
    }
}
