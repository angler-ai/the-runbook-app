//! Re-admit saved typed semantics without interpreting saved source annotations.
use crate::ir::*;
use crate::{DataSchema, DataType, DefaultValue, Diagnostic};
use serde_json::{Map, Value};

fn invalid(code: &str) -> Vec<Diagnostic> {
    vec![Diagnostic {
        code: code.into(),
        path: String::new(),
        message: None,
        line: 0,
        column: 0,
    }]
}
fn object() -> Map<String, Value> {
    Map::new()
}
fn text(s: &str) -> Value {
    Value::String(s.into())
}
fn strings<'a>(values: impl IntoIterator<Item = &'a String>) -> Value {
    Value::Array(values.into_iter().map(|s| text(s)).collect())
}
fn optional(map: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    if let Some(v) = value {
        map.insert(key.into(), text(v));
    }
}
fn duration(map: &mut Map<String, Value>, key: &str, value: Option<Duration>) {
    if let Some(v) = value {
        map.insert(key.into(), text(&format!("{}ms", v.millis)));
    }
}
fn actor(v: Actor) -> &'static str {
    match v {
        Actor::Human => "human",
        Actor::Agent => "agent",
        Actor::Any => "any",
    }
}
fn failure(v: FailurePolicy) -> &'static str {
    match v {
        FailurePolicy::Fail => "fail",
        FailurePolicy::Continue => "continue",
        FailurePolicy::Handoff => "handoff",
    }
}
fn recipient(v: &Recipient) -> String {
    match v {
        Recipient::User(s) => format!("user:{s}"),
        Recipient::Team(s) => format!("team:{s}"),
    }
}
fn recipients(values: &[Recipient]) -> Value {
    Value::Array(values.iter().map(|v| text(&recipient(v))).collect())
}
/// LANG9: a step's or the defaults' follow-up keys, rebuilt for a restore.
fn followup(m: &mut serde_json::Map<String, Value>, f: Option<&Followup>) {
    let Some(f) = f else {
        return;
    };
    duration(m, "remind_after", f.remind_after);
    duration(m, "remind_every", f.remind_every);
    duration(m, "escalate_after", f.escalate_after);
    if !f.escalate_to.is_empty() {
        m.insert("escalate_to".into(), recipients(&f.escalate_to));
    }
}
fn data(values: &std::collections::BTreeMap<String, Value>) -> Value {
    Value::Object(values.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}
fn retry(v: &Retry) -> Value {
    let mut m = object();
    m.insert("max_attempts".into(), v.max_attempts.into());
    duration(&mut m, "backoff", v.backoff);
    if !v.on.is_empty() {
        m.insert("on".into(), failure_types(&v.on));
    }
    Value::Object(m)
}
/// LANG9: `retry.on` and `catch_on`, rebuilt for a restore.
fn failure_types(v: &[FailureType]) -> Value {
    Value::Array(v.iter().map(|t| text(t.as_str())).collect())
}

// Iterative bounds precede every recursive schema/value copy.
fn value_bound(value: &Value, limit: usize, count: &mut usize) -> Result<(), Vec<Diagnostic>> {
    let mut pending = vec![(value, 0)];
    while let Some((v, d)) = pending.pop() {
        *count += 1;
        if *count > 2_000_000 {
            return Err(invalid("spec.source_value_limit"));
        }
        if d > limit {
            return Err(invalid("spec.depth_limit"));
        }
        match v {
            Value::Array(a) => pending.extend(a.iter().map(|v| (v, d + 1))),
            Value::Object(m) => pending.extend(m.values().map(|v| (v, d + 1))),
            _ => {}
        }
    }
    Ok(())
}
fn schema_bound(root: &DataSchema) -> Result<(), Vec<Diagnostic>> {
    let mut pending = vec![(root, 0)];
    let mut declarations = 0;
    let mut values = 0;
    while let Some((s, d)) = pending.pop() {
        declarations += 1;
        if declarations > 4096 {
            return Err(invalid("spec.schema_limit"));
        }
        if d > 64 {
            return Err(invalid("spec.depth_limit"));
        }
        if let DefaultValue::Value(v) = &s.default {
            value_bound(v, 64, &mut values)?;
        }
        if s.extensions.keys().any(|key| !key.starts_with("x-")) {
            return Err(invalid("spec.invalid_schema"));
        }
        for v in s.extensions.values() {
            value_bound(v, 64, &mut values)?;
        }
        match &s.kind {
            DataType::List { items } => pending.push((items, d + 1)),
            DataType::Object { properties } => {
                pending.extend(properties.values().map(|s| (s, d + 1)))
            }
            _ => {}
        }
    }
    Ok(())
}
fn schema_source(s: &DataSchema) -> Value {
    let mut m: Map<String, Value> = s
        .extensions
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let name = match &s.kind {
        DataType::String => "string",
        DataType::Text => "text",
        DataType::Number => "number",
        DataType::Integer => "integer",
        DataType::Boolean => "boolean",
        DataType::Enum { options } => {
            m.insert("options".into(), strings(options));
            "enum"
        }
        DataType::MultiEnum { options } => {
            m.insert("options".into(), strings(options));
            "multi_enum"
        }
        DataType::List { items } => {
            m.insert("items".into(), schema_source(items));
            "list"
        }
        DataType::Object { properties } => {
            m.insert(
                "properties".into(),
                Value::Object(
                    properties
                        .iter()
                        .map(|(k, v)| (k.clone(), schema_source(v)))
                        .collect(),
                ),
            );
            "object"
        }
        DataType::Date => "date",
        DataType::DateTime => "datetime",
        DataType::Duration => "duration",
        DataType::Url => "url",
        DataType::Email => "email",
        DataType::User => "user",
    };
    m.insert("type".into(), text(name));
    m.insert("required".into(), s.required.into());
    if let DefaultValue::Value(v) = &s.default {
        m.insert("default".into(), v.clone());
    }
    if let Some(v) = &s.min {
        m.insert("min".into(), Value::Number(v.clone()));
    }
    if let Some(v) = &s.max {
        m.insert("max".into(), Value::Number(v.clone()));
    }
    optional(&mut m, "pattern", &s.pattern);
    optional(&mut m, "title", &s.title);
    optional(&mut m, "description", &s.description);
    if let Some(v) = s.indexed {
        m.insert("indexed".into(), v.into());
    }
    if let Some(v) = s.locked {
        m.insert("locked".into(), v.into());
    }
    Value::Object(m)
}
pub(crate) fn project_source(schema: &DataSchema) -> Result<Value, Vec<Diagnostic>> {
    schema_bound(schema)?;
    let DataType::Object { properties } = &schema.kind else {
        return Err(invalid("spec.invalid_schema"));
    };
    let value = Value::Object(
        properties
            .iter()
            .map(|(k, v)| (k.clone(), schema_source(v)))
            .collect(),
    );
    if crate::parse_declarations(&value)? != *schema {
        return Err(invalid("spec.invalid_schema"));
    }
    Ok(value)
}
fn body(nodes: &[Node]) -> Result<Value, Vec<Diagnostic>> {
    nodes
        .iter()
        .map(node_source)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}
fn node_source(n: &Node) -> Result<Value, Vec<Diagnostic>> {
    let mut m = object();
    m.insert("id".into(), text(&n.id));
    optional(&mut m, "title", &n.title);
    optional(&mut m, "if", &n.guard);
    m.insert("on_failure".into(), text(failure(n.on_failure)));
    duration(&mut m, "timeout", n.timeout);
    duration(&mut m, "due_after", n.due_after);
    followup(&mut m, n.followup.as_ref());
    // LANG9: labels are part of the step; a restore must rebuild them.
    if !n.labels.is_empty() {
        m.insert(
            "labels".into(),
            Value::Object(n.labels.iter().map(|(k, v)| (k.clone(), text(v))).collect()),
        );
    }
    match &n.kind {
        NodeKind::Task(t) => {
            m.insert("actor".into(), text(actor(t.actor)));
            m.insert("retry".into(), retry(&t.retry));
            if let Some(a) = &t.assignee {
                m.insert(
                    "assignee".into(),
                    text(&match a {
                        Assignee::Recipient(r) => recipient(r),
                        Assignee::Expression(s) => s.clone(),
                    }),
                );
            }
            if t.assignment.is_strict() {
                m.insert("assignment".into(), text("strict"));
            }
            if let Some(i) = &t.instructions {
                m.insert(
                    "instructions".into(),
                    match i {
                        Instructions::Markdown(s) => text(s),
                        Instructions::Audience {
                            default,
                            human,
                            agent,
                        } => {
                            let mut a = object();
                            optional(&mut a, "default", default);
                            optional(&mut a, "human", human);
                            optional(&mut a, "agent", agent);
                            Value::Object(a)
                        }
                    },
                );
            }
            if t.skills.len() > 1 {
                m.insert("skills".into(), strings(&t.skills));
            } else {
                optional(&mut m, "skill", &t.skill);
            }
            m.insert("with".into(), data(&t.parameters));
            m.insert("checklist".into(), strings(&t.checklist));
            for (k, v) in [
                ("check", t.check),
                ("skippable", t.skippable),
                ("destructive", t.destructive),
                ("allow_auto_decision", t.allow_auto_decision),
            ] {
                m.insert(k.into(), v.into());
            }
            m.insert("outputs".into(), project_source(&t.outputs)?);
            let kinds = |v: &[EvidenceKind]| {
                Value::Array(
                    v.iter()
                        .map(|v| {
                            text(match v {
                                EvidenceKind::Link => "link",
                                EvidenceKind::Log => "log",
                                EvidenceKind::File => "file",
                                EvidenceKind::Screenshot => "screenshot",
                            })
                        })
                        .collect(),
                )
            };
            let mut e = object();
            e.insert("required".into(), kinds(&t.evidence.required));
            if let Some(v) = &t.evidence.accepted {
                e.insert("accepted".into(), kinds(v));
            }
            m.insert("evidence".into(), Value::Object(e));
            let mut a = object();
            if let Some(v) = &t.agent.tools {
                a.insert("tools".into(), v.clone());
            }
            if let Some(v) = &t.agent.commands {
                a.insert("commands".into(), v.clone());
            }
            // R0-3: the step's own limits are source; its resolved target
            // and credential IDs are pins, compared apart.
            if let Some(l) = &t.agent_limits {
                optional(&mut a, "model", &l.model);
                if let Some(cost) = l.max_cost_usd {
                    a.insert("max_cost_usd".into(), cost.into());
                }
                if let Some(tokens) = l.max_tokens {
                    a.insert("max_tokens".into(), tokens.into());
                }
                if let Some(network) = &l.network {
                    a.insert("network".into(), strings(network));
                }
            }
            m.insert("agent".into(), Value::Object(a));
        }
        NodeKind::Group(g) => {
            m.insert("steps".into(), body(&g.children)?);
            m.insert("catch".into(), body(&g.catch)?);
            m.insert("finally".into(), body(&g.finally)?);
            if !g.catch_on.is_empty() {
                m.insert("catch_on".into(), failure_types(&g.catch_on));
            }
        }
        NodeKind::Parallel {
            children,
            fail_fast,
            join,
        } => {
            m.insert("parallel".into(), body(children)?);
            m.insert("fail_fast".into(), (*fail_fast).into());
            if let Join::AtLeast(n) = join {
                let mut j = object();
                j.insert("at_least".into(), (*n).into());
                m.insert("join".into(), Value::Object(j));
            }
        }
        NodeKind::Foreach(g) => {
            let mut a = object();
            a.insert("items".into(), text(&g.items));
            a.insert("as".into(), text(&g.alias));
            optional(&mut a, "key", &g.key);
            a.insert("concurrency".into(), g.concurrency.into());
            a.insert("max_items".into(), g.max_items.into());
            a.insert("steps".into(), body(&g.children)?);
            m.insert("foreach".into(), Value::Object(a));
        }
        NodeKind::Loop(g) => {
            let mut a = object();
            // Checked here so `Loop::condition` never sees a stored loop
            // with no condition or with both.
            if g.until.is_some() == g.r#while.is_some() {
                return Err(invalid("spec.invalid_ir"));
            }
            let condition = g.condition();
            a.insert(condition.key().into(), text(condition.source()));
            a.insert("max_iterations".into(), g.max_iterations.into());
            duration(&mut a, "delay", g.delay);
            a.insert("steps".into(), body(&g.children)?);
            m.insert("loop".into(), Value::Object(a));
        }
        NodeKind::Switch(g) => {
            let mut a = object();
            let cases = g
                .cases
                .iter()
                .map(|c| {
                    let mut a = object();
                    a.insert("when".into(), text(&c.when));
                    a.insert("steps".into(), body(&c.children)?);
                    Ok(Value::Object(a))
                })
                .collect::<Result<Vec<_>, Vec<Diagnostic>>>()?;
            a.insert("cases".into(), Value::Array(cases));
            let mut d = object();
            d.insert("steps".into(), body(&g.default)?);
            a.insert("default".into(), Value::Object(d));
            m.insert("switch".into(), Value::Object(a));
        }
        NodeKind::Wait(w) => {
            let mut a = object();
            match &w.kind {
                WaitKind::For(v) => duration(&mut a, "for", Some(*v)),
                WaitKind::Until(s) => {
                    a.insert("until".into(), text(s));
                }
                WaitKind::Signal(s) => {
                    a.insert("signal".into(), text(s));
                }
            }
            duration(&mut a, "timeout", w.timeout);
            if let Some(outputs) = &w.outputs {
                a.insert("outputs".into(), project_source(outputs)?);
            }
            m.insert("wait".into(), Value::Object(a));
        }
        NodeKind::Approval(a) => {
            let mut v = object();
            v.insert("prompt".into(), text(&a.prompt));
            v.insert(
                "options".into(),
                Value::Array(
                    a.options
                        .iter()
                        .map(|o| {
                            let mut m = object();
                            m.insert("id".into(), text(&o.id));
                            m.insert("title".into(), text(&o.title));
                            m.insert("auto".into(), o.auto.into());
                            Value::Object(m)
                        })
                        .collect(),
                ),
            );
            let mut ap = object();
            ap.insert(
                "users".into(),
                Value::Array(
                    a.approvers
                        .iter()
                        .filter_map(|r| {
                            if let Recipient::User(s) = r {
                                Some(text(s))
                            } else {
                                None
                            }
                        })
                        .collect(),
                ),
            );
            ap.insert(
                "teams".into(),
                Value::Array(
                    a.approvers
                        .iter()
                        .filter_map(|r| {
                            if let Recipient::Team(s) = r {
                                Some(text(s))
                            } else {
                                None
                            }
                        })
                        .collect(),
                ),
            );
            v.insert("approvers".into(), Value::Object(ap));
            v.insert("min".into(), a.min.into());
            duration(&mut v, "timeout", a.timeout);
            match &a.on_timeout {
                ApprovalTimeout::Escalate => {
                    if a.timeout.is_some() {
                        v.insert("on_timeout".into(), text("escalate"));
                    }
                }
                ApprovalTimeout::Decide(s) => {
                    let mut d = object();
                    d.insert("decide".into(), text(s));
                    v.insert("on_timeout".into(), Value::Object(d));
                }
            }
            if let Some(r) = &a.escalate_to {
                v.insert("escalate_to".into(), recipients(r));
            }
            duration(&mut v, "escalation_timeout", a.escalation_timeout);
            v.insert(
                "on_expired".into(),
                text(match a.on_expired {
                    ExpiredPolicy::Pause => "pause",
                    ExpiredPolicy::Fail => "fail",
                }),
            );
            v.insert("allow_agents".into(), a.allow_agents.into());
            if a.agent_target.is_some() || a.agent_limits.is_some() || a.agent_after.is_some() {
                let mut agent = object();
                if let Some(limits) = &a.agent_limits {
                    optional(&mut agent, "model", &limits.model);
                    if let Some(cost) = limits.max_cost_usd {
                        agent.insert("max_cost_usd".into(), cost.into());
                    }
                    if let Some(tokens) = limits.max_tokens {
                        agent.insert("max_tokens".into(), tokens.into());
                    }
                }
                if let Some(AgentTarget::Runner { name, .. }) = &a.agent_target {
                    agent.insert("name".into(), text(name));
                }
                duration(&mut agent, "after", a.agent_after);
                v.insert("agent".into(), Value::Object(agent));
            }
            if !a.exclude_completers_of.is_empty() {
                v.insert(
                    "exclude_completers_of".into(),
                    strings(&a.exclude_completers_of),
                );
            }
            if let Some(approves) = &a.approves {
                v.insert("approves".into(), strings(approves));
            }
            m.insert("approval".into(), Value::Object(v));
        }
        NodeKind::Set(a) => {
            let mut v = object();
            v.insert("vars".into(), data(&a.variables));
            v.insert("fields".into(), data(&a.fields));
            m.insert("set".into(), Value::Object(v));
        }
        NodeKind::Call(c) => {
            let mut v = object();
            v.insert("runbook".into(), text(&c.runbook));
            if let Some(version) = c.version {
                v.insert("version".into(), version.into());
            }
            v.insert("with".into(), data(&c.with));
            v.insert("outputs".into(), project_source(&c.outputs)?);
            m.insert("call".into(), Value::Object(v));
        }
        NodeKind::End(e) => {
            let mut v = object();
            v.insert(
                "status".into(),
                text(match e.status {
                    EndStatus::Succeeded => "succeeded",
                    EndStatus::Failed => "failed",
                    EndStatus::Cancelled => "cancelled",
                }),
            );
            optional(&mut v, "message", &e.message);
            m.insert("end".into(), Value::Object(v));
        }
    }
    Ok(Value::Object(m))
}
fn preflight(ir: &NormalizedRunbook) -> Result<(), Vec<Diagnostic>> {
    crate::budget::estimate_step_runs(ir).map_err(|e| vec![e])?;
    let mut count = 0;
    if ir.source_yaml.len() > crate::normalize::MAX_SOURCE_BYTES {
        return Err(invalid("spec.source_limit"));
    }
    value_bound(&ir.source_json, 128, &mut count)?;
    if count > crate::normalize::MAX_SOURCE_VALUES {
        return Err(invalid("spec.source_value_limit"));
    }
    count = 0;
    for v in ir.variables.values().chain(ir.outputs.values()) {
        value_bound(v, 128, &mut count)?;
    }
    if let Some(m) = &ir.agent_params {
        for v in m.values() {
            value_bound(v, 128, &mut count)?;
        }
    }
    schema_bound(&ir.inputs)?;
    schema_bound(&ir.fields)?;
    let mut pending: Vec<_> = ir.steps.iter().chain(&ir.finally).collect();
    while let Some(n) = pending.pop() {
        match &n.kind {
            NodeKind::Task(t) => {
                schema_bound(&t.outputs)?;
                for v in t
                    .parameters
                    .values()
                    .chain(t.agent.tools.iter())
                    .chain(t.agent.commands.iter())
                {
                    value_bound(v, 128, &mut count)?;
                }
            }
            NodeKind::Set(a) => {
                for v in a.variables.values().chain(a.fields.values()) {
                    value_bound(v, 128, &mut count)?;
                }
            }
            NodeKind::Wait(w) => {
                if let Some(outputs) = &w.outputs {
                    schema_bound(outputs)?;
                }
            }
            NodeKind::Call(c) => {
                schema_bound(&c.outputs)?;
                for v in c.with.values() {
                    value_bound(v, 128, &mut count)?;
                }
            }
            NodeKind::Group(g) => {
                pending.extend(g.children.iter().chain(&g.catch).chain(&g.finally))
            }
            NodeKind::Parallel { children, .. } => pending.extend(children),
            NodeKind::Foreach(g) => pending.extend(&g.children),
            NodeKind::Loop(g) => pending.extend(&g.children),
            NodeKind::Switch(s) => {
                pending.extend(&s.default);
                for c in &s.cases {
                    pending.extend(&c.children);
                }
            }
            _ => {}
        }
    }
    Ok(())
}
pub(crate) fn validate_ir(mut ir: NormalizedRunbook) -> Result<NormalizedRunbook, Vec<Diagnostic>> {
    preflight(&ir)?;
    let mut metadata = object();
    metadata.insert("id".into(), text(&ir.metadata.id));
    metadata.insert("title".into(), text(&ir.metadata.title));
    optional(&mut metadata, "description", &ir.metadata.description);
    metadata.insert("tags".into(), strings(&ir.metadata.tags));
    metadata.insert("owners".into(), recipients(&ir.metadata.owners));
    let mut spec = object();
    let d = &ir.defaults;
    let mut defaults = object();
    defaults.insert("actor".into(), text(actor(d.actor)));
    defaults.insert("retry".into(), retry(&d.retry));
    duration(&mut defaults, "timeout", d.timeout);
    defaults.insert("on_failure".into(), text(failure(d.on_failure)));
    followup(&mut defaults, d.followup.as_ref());
    spec.insert("defaults".into(), Value::Object(defaults));
    let e = &ir.execution;
    let mut execution = object();
    optional(&mut execution, "title", &e.title);
    execution.insert("tags".into(), strings(&e.tags));
    duration(&mut execution, "timeout", e.timeout);
    duration(&mut execution, "cleanup_timeout", Some(e.cleanup_timeout));
    if let Some(c) = &e.concurrency {
        let mut m = object();
        m.insert("key".into(), text(&c.key));
        m.insert("limit".into(), c.limit.into());
        m.insert("on_conflict".into(), text(c.on_conflict.as_str()));
        execution.insert("concurrency".into(), Value::Object(m));
    }
    spec.insert("execution".into(), Value::Object(execution));
    spec.insert("inputs".into(), project_source(&ir.inputs)?);
    spec.insert("fields".into(), project_source(&ir.fields)?);
    spec.insert("vars".into(), data(&ir.variables));
    spec.insert("outputs".into(), data(&ir.outputs));
    spec.insert(
        "skills".into(),
        Value::Object(
            ir.skills
                .iter()
                .map(|(k, v)| (k.clone(), text(v)))
                .collect(),
        ),
    );
    if let Some(p) = &ir.agent_params {
        let mut a = object();
        a.insert("params".into(), Value::Object(p.clone()));
        spec.insert("agents".into(), Value::Object(a));
    }
    optional(&mut spec, "report", &ir.report);
    // LANG9b: a stored version's triggers are part of its source.
    if !ir.triggers.is_empty() {
        let mut triggers = object();
        if !ir.triggers.schedule.is_empty() {
            let mut list = vec![];
            for t in &ir.triggers.schedule {
                let mut m = object();
                m.insert("id".into(), text(&t.id));
                m.insert("cron".into(), text(&t.cron));
                m.insert("timezone".into(), text(&t.timezone));
                if !t.inputs.is_empty() {
                    m.insert("inputs".into(), data(&t.inputs));
                }
                duration(&mut m, "catch_up", t.catch_up);
                list.push(Value::Object(m));
            }
            triggers.insert("schedule".into(), Value::Array(list));
        }
        if !ir.triggers.webhook.is_empty() {
            let mut list = vec![];
            for t in &ir.triggers.webhook {
                let mut m = object();
                m.insert("id".into(), text(&t.id));
                if let Some(payload) = &t.payload {
                    m.insert("payload".into(), project_source(payload)?);
                }
                optional(&mut m, "when", &t.when);
                if !t.inputs.is_empty() {
                    m.insert("inputs".into(), data(&t.inputs));
                }
                optional(&mut m, "dedupe", &t.dedupe);
                list.push(Value::Object(m));
            }
            triggers.insert("webhook".into(), Value::Array(list));
        }
        spec.insert("triggers".into(), Value::Object(triggers));
    }
    spec.insert("steps".into(), body(&ir.steps)?);
    spec.insert("finally".into(), body(&ir.finally)?);
    let mut root = object();
    root.insert("apiVersion".into(), text("therunbook.app/v1"));
    root.insert("kind".into(), text("Runbook"));
    root.insert("metadata".into(), Value::Object(metadata));
    root.insert("spec".into(), Value::Object(spec));
    let source = Value::Object(root);
    // Count the complete semantic projection too, including scalar fields and lists.
    // Opaque depths were checked separately, before cloning them into this tree.
    value_bound(&source, usize::MAX, &mut 0)?;
    let mut normalized = crate::normalize::normalize_lowered(source)?;
    normalized.source_json = Value::Null;
    normalized.source_yaml.clear();
    let annotations = (
        std::mem::replace(&mut ir.source_json, Value::Null),
        std::mem::take(&mut ir.source_yaml),
    );
    // LANG4: call pins are not source; a stored IR must carry a valid one
    // for every call, and they are compared apart from the lowered tree.
    let mut targets = vec![];
    crate::calls::take_targets(&mut ir, &mut targets);
    let agents = crate::agents::take_pins(&mut ir);
    if normalized != ir
        || !targets.iter().all(crate::calls::valid_target)
        || !crate::agents::valid_pins(&agents)
    {
        return Err(invalid("spec.invalid_ir"));
    }
    crate::calls::put_targets(&mut ir, &mut targets.into_iter());
    crate::agents::put_pins(&mut ir, agents);
    ir.source_json = annotations.0;
    ir.source_yaml = annotations.1;
    Ok(ir)
}

#[cfg(test)]
mod tests;
