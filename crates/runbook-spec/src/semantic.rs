//! Private v1 semantic projection and incremental SHA-256 framing.
//!
//! Preconditions: the compiler has admitted/bounded normalized IR (including
//! opaque values), selected only referenced immutable facts, and validated the
//! object-shaped project-variable schema. Publicly constructible IR is not an
//! admission boundary. This module performs no resolution, evaluation or I/O.
//! Source parse/normalize already bound source bytes/value depth/control depth;
//! A5e must bound external facts before calling this private checkpoint.
//!
//! Scratch: one owned semantic JSON tree clones included strings/numbers/opaque
//! values once; source documents, ignored extensions and checked programs are
//! never cloned. Framing streams into the digest; object sorting uses a temporary
//! vector of borrowed members per object and one temporary Number text at a time.
//! No complete preimage is allocated.
#![allow(dead_code)] // Private checkpoint consumed by A5e's sealing pass.
use crate::{DataSchema, DataType, DefaultValue, ir::*};
use runbook_model::{TeamId, UserId};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Source reference -> selected immutable fact, never the caller's full catalog.
/// Separate identity maps preserve user/team namespaces; no membership snapshot.
pub(crate) struct SelectedPins<'a> {
    pub skills: &'a BTreeMap<String, [u8; 32]>,
    pub users: &'a BTreeMap<String, UserId>,
    pub teams: &'a BTreeMap<String, TeamId>,
    pub project_variables: &'a DataSchema,
}

fn object<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn string(value: &str) -> Value {
    Value::String(value.into())
}
fn optional<T>(value: Option<&T>, f: impl FnOnce(&T) -> Value) -> Value {
    value.map_or(Value::Null, f)
}
fn array<T>(values: &[T], f: impl Fn(&T) -> Value) -> Value {
    Value::Array(values.iter().map(f).collect())
}
fn strings(values: &[String]) -> Value {
    array(values, |s| string(s))
}
fn map<T>(values: &BTreeMap<String, T>, f: impl Fn(&T) -> Value) -> Value {
    Value::Object(values.iter().map(|(k, v)| (k.clone(), f(v))).collect())
}
fn duration(value: &Duration) -> Value {
    value.millis.into()
}
fn actor(value: Actor) -> Value {
    string(match value {
        Actor::Human => "human",
        Actor::Agent => "agent",
        Actor::Any => "any",
    })
}
fn failure(value: FailurePolicy) -> Value {
    string(match value {
        FailurePolicy::Fail => "fail",
        FailurePolicy::Continue => "continue",
        FailurePolicy::Handoff => "handoff",
    })
}
fn retry(value: &Retry) -> Value {
    let mut projected = object([
        ("max_attempts", value.max_attempts.into()),
        ("backoff", optional(value.backoff.as_ref(), duration)),
    ]);
    // LANG9: projected only when set, so existing hashes don't change.
    if !value.on.is_empty()
        && let Value::Object(m) = &mut projected
    {
        m.insert("on".into(), failure_types(&value.on));
    }
    projected
}
fn followup(value: &Followup) -> Value {
    object([
        (
            "remind_after",
            optional(value.remind_after.as_ref(), duration),
        ),
        (
            "remind_every",
            optional(value.remind_every.as_ref(), duration),
        ),
        (
            "escalate_after",
            optional(value.escalate_after.as_ref(), duration),
        ),
        ("escalate_to", array(&value.escalate_to, recipient)),
    ])
}
fn failure_types(value: &[FailureType]) -> Value {
    Value::Array(value.iter().map(|t| string(t.as_str())).collect())
}
fn recipient(value: &Recipient) -> Value {
    match value {
        Recipient::User(s) => object([("user", string(s))]),
        Recipient::Team(s) => object([("team", string(s))]),
    }
}
fn instructions(value: &Instructions) -> Value {
    match value {
        Instructions::Markdown(s) => object([("markdown", string(s))]),
        Instructions::Audience {
            default,
            human,
            agent,
        } => object([(
            "audience",
            object([
                ("default", optional(default.as_ref(), |s| string(s))),
                ("human", optional(human.as_ref(), |s| string(s))),
                ("agent", optional(agent.as_ref(), |s| string(s))),
            ]),
        )]),
    }
}
fn evidence_kind(value: &EvidenceKind) -> Value {
    string(match value {
        EvidenceKind::Link => "link",
        EvidenceKind::Log => "log",
        EvidenceKind::File => "file",
        EvidenceKind::Screenshot => "screenshot",
    })
}
fn schema(value: &DataSchema) -> Value {
    let kind = match &value.kind {
        DataType::String => object([("string", Value::Null)]),
        DataType::Text => object([("text", Value::Null)]),
        DataType::Number => object([("number", Value::Null)]),
        DataType::Integer => object([("integer", Value::Null)]),
        DataType::Boolean => object([("boolean", Value::Null)]),
        DataType::Enum { options } => object([("enum", strings(options))]),
        DataType::MultiEnum { options } => object([("multi_enum", strings(options))]),
        DataType::List { items } => object([("list", schema(items))]),
        DataType::Object { properties } => object([("object", map(properties, schema))]),
        DataType::Date => object([("date", Value::Null)]),
        DataType::DateTime => object([("datetime", Value::Null)]),
        DataType::Duration => object([("duration", Value::Null)]),
        DataType::Url => object([("url", Value::Null)]),
        DataType::Email => object([("email", Value::Null)]),
        DataType::User => object([("user", Value::Null)]),
    };
    let mut map = match object([
        ("kind", kind),
        ("required", value.required.into()),
        (
            "default",
            match &value.default {
                DefaultValue::Missing => object([("missing", Value::Null)]),
                DefaultValue::Value(v) => object([("value", v.clone())]),
            },
        ),
        (
            "min",
            optional(value.min.as_ref(), |n| Value::Number(n.clone())),
        ),
        (
            "max",
            optional(value.max.as_ref(), |n| Value::Number(n.clone())),
        ),
        ("title", optional(value.title.as_ref(), |s| string(s))),
        (
            "description",
            optional(value.description.as_ref(), |s| string(s)),
        ),
        ("indexed", optional(value.indexed.as_ref(), |b| (*b).into())),
        ("locked", optional(value.locked.as_ref(), |b| (*b).into())),
    ]) {
        Value::Object(m) => m,
        _ => unreachable!(),
    };
    if let Some(pattern) = &value.pattern {
        map.insert("pattern".into(), string(pattern));
    }
    Value::Object(map)
}
fn opaque_optional(value: Option<&Value>) -> Value {
    match value {
        None => object([("missing", Value::Null)]),
        Some(value) => object([("value", value.clone())]),
    }
}
fn task(value: &Task) -> Value {
    let mut projected = task_fields(value);
    // LANG7: only a strict assignment is projected, so every definition
    // without one keeps its pre-LANG7 projection and hash.
    if value.assignment.is_strict()
        && let Value::Object(map) = &mut projected
    {
        map.insert("assignment".into(), string("strict"));
    }
    // LANG8: when multiple skills are attached to a task, project them.
    // A single skill (or none) keeps its pre-LANG8 projection and hash.
    if value.skills.len() > 1
        && let Value::Object(map) = &mut projected
    {
        map.insert("skills".into(), array(&value.skills, |s| string(s)));
    }
    // R0-3: runner targeting and limits, projected only when present so
    // every earlier runbook keeps its hash.
    if let Value::Object(map) = &mut projected {
        if let Some(target) = &value.agent_target {
            map.insert(
                "agent_target".into(),
                serde_json::to_value(target).unwrap_or(Value::Null),
            );
        }
        if let Some(limits) = &value.agent_limits {
            map.insert(
                "agent_limits".into(),
                serde_json::to_value(limits).unwrap_or(Value::Null),
            );
        }
        if let Some(connections) = &value.connections {
            map.insert("connections".into(), array(connections, |s| string(s)));
        }
    }
    projected
}
fn task_fields(value: &Task) -> Value {
    object([
        ("actor", actor(value.actor)),
        (
            "assignee",
            optional(value.assignee.as_ref(), |a| match a {
                Assignee::Recipient(r) => object([("recipient", recipient(r))]),
                Assignee::Expression(s) => object([("expression", string(s))]),
            }),
        ),
        ("retry", retry(&value.retry)),
        (
            "instructions",
            optional(value.instructions.as_ref(), instructions),
        ),
        ("skill", optional(value.skill.as_ref(), |s| string(s))),
        ("parameters", map(&value.parameters, Clone::clone)),
        ("checklist", strings(&value.checklist)),
        ("check", value.check.into()),
        ("skippable", value.skippable.into()),
        ("destructive", value.destructive.into()),
        ("allow_auto_decision", value.allow_auto_decision.into()),
        ("outputs", schema(&value.outputs)),
        (
            "evidence",
            object([
                ("required", array(&value.evidence.required, evidence_kind)),
                (
                    "accepted",
                    optional(value.evidence.accepted.as_ref(), |v| {
                        array(v, evidence_kind)
                    }),
                ),
            ]),
        ),
        (
            "agent",
            object([
                ("tools", opaque_optional(value.agent.tools.as_ref())),
                ("commands", opaque_optional(value.agent.commands.as_ref())),
            ]),
        ),
    ])
}
fn approval(value: &Approval) -> Value {
    let mut projection = object([
        ("prompt", string(&value.prompt)),
        (
            "options",
            array(&value.options, |v| {
                object([
                    ("id", string(&v.id)),
                    ("title", string(&v.title)),
                    ("auto", v.auto.into()),
                ])
            }),
        ),
        ("approvers", array(&value.approvers, recipient)),
        ("min", value.min.into()),
        ("timeout", optional(value.timeout.as_ref(), duration)),
        (
            "on_timeout",
            match &value.on_timeout {
                ApprovalTimeout::Escalate => object([("escalate", Value::Null)]),
                ApprovalTimeout::Decide(s) => object([("decide", string(s))]),
            },
        ),
        (
            "escalate_to",
            optional(value.escalate_to.as_ref(), |v| array(v, recipient)),
        ),
        (
            "escalation_timeout",
            optional(value.escalation_timeout.as_ref(), duration),
        ),
        (
            "on_expired",
            string(match value.on_expired {
                ExpiredPolicy::Pause => "pause",
                ExpiredPolicy::Fail => "fail",
            }),
        ),
        ("allow_agents", value.allow_agents.into()),
    ]);
    if let Some(target) = &value.agent_target {
        projection["agent_target"] = serde_json::to_value(target).unwrap_or(Value::Null);
    }
    if let Some(limits) = &value.agent_limits {
        projection["agent_limits"] = serde_json::to_value(limits).unwrap_or(Value::Null);
    }
    if let Some(after) = &value.agent_after {
        projection["agent_after"] = duration(after);
    }
    if let Some(approves) = &value.approves {
        projection["approves"] = array(approves, |option| string(option));
    }
    projection
}
fn node(value: &Node) -> Value {
    let kind = match &value.kind {
        NodeKind::Task(t) => object([("task", task(t))]),
        NodeKind::Group(g) => {
            let mut body = object([
                ("children", array(&g.children, node)),
                ("catch", array(&g.catch, node)),
                ("finally", array(&g.finally, node)),
            ]);
            // LANG9: projected only when set, so existing hashes don't change.
            if !g.catch_on.is_empty()
                && let Value::Object(m) = &mut body
            {
                m.insert("catch_on".into(), failure_types(&g.catch_on));
            }
            object([("group", body)])
        }
        NodeKind::Parallel {
            children,
            fail_fast,
            join,
        } => {
            let mut body = object([
                ("children", array(children, node)),
                ("fail_fast", (*fail_fast).into()),
            ]);
            // LANG2: join-all keeps its pre-LANG2 projection and hash.
            if let (Join::AtLeast(n), Value::Object(m)) = (join, &mut body) {
                m.insert("join".into(), object([("at_least", (*n).into())]));
            }
            object([("parallel", body)])
        }
        NodeKind::Foreach(f) => object([(
            "foreach",
            object([
                ("items", string(&f.items)),
                ("alias", string(&f.alias)),
                ("key", optional(f.key.as_ref(), |s| string(s))),
                ("concurrency", f.concurrency.into()),
                ("max_items", f.max_items.into()),
                ("children", array(&f.children, node)),
            ]),
        )]),
        NodeKind::Loop(l) => object([(
            "loop",
            object([
                (l.condition().key(), string(l.condition().source())),
                ("max_iterations", l.max_iterations.into()),
                ("delay", optional(l.delay.as_ref(), duration)),
                ("children", array(&l.children, node)),
            ]),
        )]),
        NodeKind::Switch(s) => object([(
            "switch",
            object([
                (
                    "cases",
                    array(&s.cases, |c| {
                        object([
                            ("when", string(&c.when)),
                            ("children", array(&c.children, node)),
                        ])
                    }),
                ),
                ("default", array(&s.default, node)),
            ]),
        )]),
        NodeKind::Wait(w) => {
            let mut body = object([
                (
                    "kind",
                    match &w.kind {
                        WaitKind::For(d) => object([("for", duration(d))]),
                        WaitKind::Until(s) => object([("until", string(s))]),
                        WaitKind::Signal(s) => object([("signal", string(s))]),
                    },
                ),
                ("timeout", optional(w.timeout.as_ref(), duration)),
            ]);
            // LANG9: projected only when present, so waits without a payload
            // schema keep their pre-LANG9 projection and hash.
            if let (Some(outputs), Value::Object(m)) = (&w.outputs, &mut body) {
                m.insert("outputs".into(), schema(outputs));
            }
            object([("wait", body)])
        }
        NodeKind::Approval(a) => object([("approval", approval(a))]),
        NodeKind::Set(s) => object([(
            "set",
            object([
                ("variables", map(&s.variables, Clone::clone)),
                ("fields", map(&s.fields, Clone::clone)),
            ]),
        )]),
        NodeKind::Call(c) => object([(
            "call",
            object([
                ("runbook", string(&c.runbook)),
                ("version", optional(c.version.as_ref(), |v| (*v).into())),
                ("with", map(&c.with, Clone::clone)),
                ("outputs", schema(&c.outputs)),
                (
                    "target",
                    optional(c.target.as_ref(), |t| {
                        object([
                            ("runbook_id", string(&t.runbook_id)),
                            ("version_id", string(&t.version_id)),
                            ("version", t.version.into()),
                            ("depth", t.depth.into()),
                            ("reaches", strings(&t.reaches)),
                            ("destructive", t.destructive.into()),
                        ])
                    }),
                ),
            ]),
        )]),
        NodeKind::End(e) => object([(
            "end",
            object([
                (
                    "status",
                    string(match e.status {
                        EndStatus::Succeeded => "succeeded",
                        EndStatus::Failed => "failed",
                        EndStatus::Cancelled => "cancelled",
                    }),
                ),
                ("message", optional(e.message.as_ref(), |s| string(s))),
            ]),
        )]),
    };
    let mut projected = object([
        ("id", string(&value.id)),
        ("title", optional(value.title.as_ref(), |s| string(s))),
        ("guard", optional(value.guard.as_ref(), |s| string(s))),
        ("on_failure", failure(value.on_failure)),
        ("timeout", optional(value.timeout.as_ref(), duration)),
        ("kind", kind),
    ]);
    // LANG9: follow-up is projected only when set.
    if let (Some(f), Value::Object(map)) = (&value.followup, &mut projected) {
        map.insert("followup".into(), followup(f));
    }
    // LANG9: labels are projected only when present, so every step without
    // them keeps its pre-LANG9 projection and hash; adding or changing
    // labels makes a new version.
    if !value.labels.is_empty()
        && let Value::Object(map) = &mut projected
    {
        map.insert(
            "labels".into(),
            Value::Object(
                value
                    .labels
                    .iter()
                    .map(|(k, v)| (k.clone(), string(v)))
                    .collect(),
            ),
        );
    }
    projected
}
/// LANG9b: a runbook's triggers, projected only when it has some.
fn triggers(value: &Triggers) -> Value {
    object([
        (
            "schedule",
            array(&value.schedule, |t| {
                object([
                    ("id", string(&t.id)),
                    ("cron", string(&t.cron)),
                    ("timezone", string(&t.timezone)),
                    ("inputs", map(&t.inputs, Clone::clone)),
                    ("catch_up", optional(t.catch_up.as_ref(), duration)),
                ])
            }),
        ),
        (
            "webhook",
            array(&value.webhook, |t| {
                object([
                    ("id", string(&t.id)),
                    ("payload", optional(t.payload.as_ref(), schema)),
                    ("when", optional(t.when.as_ref(), |s| string(s))),
                    ("inputs", map(&t.inputs, Clone::clone)),
                    ("dedupe", optional(t.dedupe.as_ref(), |s| string(s))),
                ])
            }),
        ),
    ])
}
pub(crate) fn projection(ir: &NormalizedRunbook, pins: &SelectedPins<'_>) -> Value {
    let mut projected = projection_base(ir, pins);
    if !ir.triggers.is_empty()
        && let Value::Object(m) = &mut projected
    {
        m.insert("triggers".into(), triggers(&ir.triggers));
    }
    projected
}
fn projection_base(ir: &NormalizedRunbook, pins: &SelectedPins<'_>) -> Value {
    object([
        (
            "metadata",
            object([
                ("id", string(&ir.metadata.id)),
                ("title", string(&ir.metadata.title)),
                (
                    "description",
                    optional(ir.metadata.description.as_ref(), |s| string(s)),
                ),
                ("tags", strings(&ir.metadata.tags)),
                ("owners", array(&ir.metadata.owners, recipient)),
            ]),
        ),
        (
            "execution",
            object([
                (
                    "title",
                    optional(ir.execution.title.as_ref(), |s| string(s)),
                ),
                ("tags", strings(&ir.execution.tags)),
                ("timeout", optional(ir.execution.timeout.as_ref(), duration)),
                ("cleanup_timeout", duration(&ir.execution.cleanup_timeout)),
                (
                    "concurrency",
                    optional(ir.execution.concurrency.as_ref(), |c| {
                        object([
                            ("key", string(&c.key)),
                            ("limit", c.limit.into()),
                            ("on_conflict", string(c.on_conflict.as_str())),
                        ])
                    }),
                ),
            ]),
        ),
        ("defaults", {
            let mut d = object([
                ("actor", actor(ir.defaults.actor)),
                ("timeout", optional(ir.defaults.timeout.as_ref(), duration)),
                ("retry", retry(&ir.defaults.retry)),
                ("on_failure", failure(ir.defaults.on_failure)),
            ]);
            // LANG9: projected only when set.
            if let (Some(f), Value::Object(m)) = (&ir.defaults.followup, &mut d) {
                m.insert("followup".into(), followup(f));
            }
            d
        }),
        ("inputs", schema(&ir.inputs)),
        ("fields", schema(&ir.fields)),
        ("variables", map(&ir.variables, Clone::clone)),
        ("skills", map(&ir.skills, |s| string(s))),
        (
            "agent_params",
            optional(ir.agent_params.as_ref(), |v| Value::Object(v.clone())),
        ),
        ("steps", array(&ir.steps, node)),
        ("finally", array(&ir.finally, node)),
        ("outputs", map(&ir.outputs, Clone::clone)),
        ("report", optional(ir.report.as_ref(), |s| string(s))),
        (
            "pins",
            object([
                ("skills", map(pins.skills, |d| string(&hex(d)))),
                ("users", map(pins.users, |id| string(id.as_str()))),
                ("teams", map(pins.teams, |id| string(id.as_str()))),
                ("project_variables", schema(pins.project_variables)),
            ]),
        ),
    ])
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut text, "{byte:02x}").expect("write to String");
    }
    text
}
fn frame(value: &Value, write: &mut impl FnMut(&[u8])) {
    match value {
        Value::Null => write(&[0]),
        Value::Bool(value) => write(&[1, u8::from(*value)]),
        Value::Number(value) => {
            write(&[2]);
            let text = crate::decimal::canonical(value.as_str());
            write(&(text.len() as u64).to_le_bytes());
            write(text.as_bytes());
        }
        Value::String(value) => {
            write(&[3]);
            write(&(value.len() as u64).to_le_bytes());
            write(value.as_bytes());
        }
        Value::Array(values) => {
            write(&[4]);
            write(&(values.len() as u64).to_le_bytes());
            for value in values {
                frame(value, write);
            }
        }
        Value::Object(values) => {
            write(&[5]);
            write(&(values.len() as u64).to_le_bytes());
            let mut sorted: Vec<_> = values.iter().collect();
            sorted.sort_unstable_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            for (key, value) in sorted {
                write(&[3]);
                write(&(key.len() as u64).to_le_bytes());
                write(key.as_bytes());
                frame(value, write);
            }
        }
    }
}
fn hash_value(value: &Value) -> [u8; 32] {
    let mut state = Sha256::new();
    for prefix in [
        "therunbook.compiled.v1",
        runbook_expr::PROFILE,
        runbook_expr::COST_MODEL,
    ] {
        state.update(prefix.as_bytes());
        state.update([0]);
    }
    frame(value, &mut |bytes| state.update(bytes));
    state.finalize().into()
}
pub(crate) fn hash(ir: &NormalizedRunbook, pins: &SelectedPins<'_>) -> [u8; 32] {
    hash_value(&projection(ir, pins))
}
#[cfg(test)]
#[path = "semantic/tests.rs"]
mod tests;

#[path = "semantic/vectors.rs"]
pub(crate) mod vectors;
