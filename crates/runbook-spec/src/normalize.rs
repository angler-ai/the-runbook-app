//! Typed normalization, before expression checking and publication policy.
use crate::ir::*;
use crate::{Diagnostic, Spec};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn normalize(spec: &Spec) -> std::result::Result<NormalizedRunbook, Vec<Diagnostic>> {
    preflight(spec).map_err(|e| vec![e])?;
    unique_nodes(spec).map_err(|e| vec![e])?;
    let capabilities = crate::validate_capabilities(spec);
    if !capabilities.is_empty() {
        return Err(capabilities);
    }
    Reader { spec }.run().map_err(|e| vec![e])
}

/// Reuse source-language invariants on data generated from an already bounded
/// stored IR. Effective defaults can expand its size beyond source-file limits;
/// the caller separately bounds the typed tree before generating this data.
pub(crate) fn normalize_lowered(
    value: Value,
) -> std::result::Result<NormalizedRunbook, Vec<Diagnostic>> {
    let spec = crate::parse::from_lowered(value)?;
    unique_nodes(&spec).map_err(|e| vec![e])?;
    let capabilities = crate::validate_capabilities(&spec);
    if !capabilities.is_empty() {
        return Err(capabilities);
    }
    Reader { spec: &spec }.run().map_err(|e| vec![e])
}
fn diagnostic(spec: &Spec, code: &str, path: &str) -> Diagnostic {
    let span = spec.source_spans.get(path);
    Diagnostic {
        message: None,
        code: code.into(),
        path: path.into(),
        line: span.map_or(0, |s| s.line),
        column: span.map_or(0, |s| s.column),
    }
}
pub const MAX_SOURCE_BYTES: usize = 1_048_576;
pub const MAX_SOURCE_VALUES: usize = 262_144;
pub const MAX_CONTROL_NODES: usize = 4_096;
pub const MAX_CONTROL_DEPTH: usize = 32;
/// PRD §17 guards with no overriding decision (R52), checked at publish.
pub const MAX_FOREACH_ITEMS: u32 = 100;
pub const MAX_LOOP_ITERATIONS: u32 = 50;
pub const MAX_PARALLEL_BRANCHES: usize = 20;
/// PRD §17: a single `wait` parks a run at most 7 days. `for` is checked
/// here; an `until` target is known only at runtime, where the engine
/// enforces the same bound (engine version 4).
pub const MAX_WAIT_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
/// `metadata.id` is a URL slug, at most 63 bytes like node and directory
/// slugs (R52).
pub const MAX_RUNBOOK_ID_BYTES: usize = 63;

fn unique_nodes(spec: &Spec) -> Result<()> {
    fn body<'a>(
        spec: &Spec,
        value: &'a Value,
        path: &str,
        depth: usize,
        seen: &mut BTreeSet<&'a str>,
        count: &mut usize,
    ) -> Result<()> {
        let Some(nodes) = value.as_array() else {
            return Ok(());
        };
        for (i, node) in nodes.iter().enumerate() {
            let p = pointer(path, &i.to_string());
            if depth > MAX_CONTROL_DEPTH {
                return Err(diagnostic(spec, "spec.control_depth_limit", &p));
            }
            if *count == MAX_CONTROL_NODES {
                return Err(diagnostic(spec, "spec.node_limit", &p));
            }
            *count += 1;
            if let Some(id) = node.get("id").and_then(Value::as_str)
                && !seen.insert(id)
            {
                return Err(diagnostic(spec, "spec.duplicate_id", &p));
            }
            for key in ["steps", "parallel"] {
                if let Some(v) = node.get(key) {
                    body(spec, v, &pointer(&p, key), depth + 1, seen, count)?;
                }
            }
            for key in ["foreach", "loop"] {
                if let Some(v) = node.get(key).and_then(|v| v.get("steps")) {
                    body(spec, v, &format!("{p}/{key}/steps"), depth + 1, seen, count)?;
                }
            }
            if let Some(s) = node.get("switch") {
                if let Some(cases) = s.get("cases").and_then(Value::as_array) {
                    for (j, c) in cases.iter().enumerate() {
                        if let Some(v) = c.get("steps") {
                            body(
                                spec,
                                v,
                                &format!("{p}/switch/cases/{j}/steps"),
                                depth + 1,
                                seen,
                                count,
                            )?;
                        }
                    }
                }
                if let Some(v) = s.get("default").and_then(|v| v.get("steps")) {
                    body(
                        spec,
                        v,
                        &format!("{p}/switch/default/steps"),
                        depth + 1,
                        seen,
                        count,
                    )?;
                }
            }
            for key in ["catch", "finally"] {
                if let Some(v) = node.get(key) {
                    body(spec, v, &pointer(&p, key), depth + 1, seen, count)?;
                }
            }
        }
        Ok(())
    }
    let mut seen = BTreeSet::new();
    let mut count = 0;
    body(
        spec,
        &spec.source_json()["spec"]["steps"],
        "/spec/steps",
        0,
        &mut seen,
        &mut count,
    )?;
    body(
        spec,
        &spec.source_json()["spec"]["finally"],
        "/spec/finally",
        0,
        &mut seen,
        &mut count,
    )
}
fn preflight(spec: &Spec) -> Result<()> {
    if spec.raw_source.len() > MAX_SOURCE_BYTES {
        return Err(diagnostic(spec, "spec.source_limit", ""));
    }
    let mut count = 0;
    let mut stack = vec![(spec.source_json(), 0)];
    while let Some((v, depth)) = stack.pop() {
        if depth > 128 {
            return Err(diagnostic(spec, "spec.yaml_nesting_limit", ""));
        }
        count += 1;
        if count > MAX_SOURCE_VALUES {
            return Err(diagnostic(spec, "spec.source_value_limit", ""));
        }
        match v {
            Value::Array(a) => stack.extend(a.iter().map(|v| (v, depth + 1))),
            Value::Object(m) => stack.extend(m.values().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}

struct Reader<'a> {
    spec: &'a Spec,
}
type Result<T> = std::result::Result<T, Diagnostic>;
fn pointer(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}

pub(crate) fn parse_duration_millis(text: &str) -> Option<u64> {
    let mut rest = text;
    let mut total = 0u64;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let amount = rest[..digits].parse::<u64>().ok()?;
        rest = &rest[digits..];
        let (unit, width) = if rest.starts_with("ms") {
            (1, 2)
        } else {
            match rest.as_bytes().first() {
                Some(b's') => (1_000, 1),
                Some(b'm') => (60_000, 1),
                Some(b'h') => (3_600_000, 1),
                Some(b'd') => (86_400_000, 1),
                _ => return None,
            }
        };
        total = amount
            .checked_mul(unit)
            .and_then(|n| total.checked_add(n))?;
        rest = &rest[width..];
    }
    if total == 0 {
        return None;
    }
    Some(total)
}
/// LANG7: the title shown for a runbook or task that has none: its ID with
/// `-` and `_` read as spaces (runs collapsed) and the first letter
/// capitalized, so `check-health` reads "Check health". It is written into
/// the compiled definition, so the API, outline, reports and the web show
/// the same text, and it never changes for a given ID.
pub fn derived_title(id: &str) -> String {
    let words: Vec<&str> = id.split(['-', '_']).filter(|w| !w.is_empty()).collect();
    let text = words.join(" ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => id.to_owned(),
    }
}
fn empty_schema() -> crate::DataSchema {
    crate::parse_declarations(&serde_json::json!({})).expect("empty schema")
}
/// LANG9b: the most schedules, and the most webhooks, one runbook declares.
pub const MAX_TRIGGERS: usize = 16;
/// LANG9: the follow-up keys a task or wait may carry.
const FOLLOWUP_KEYS: [&str; 4] = [
    "remind_after",
    "remind_every",
    "escalate_after",
    "escalate_to",
];
fn defaults() -> TaskDefaults {
    TaskDefaults {
        actor: Actor::Any,
        timeout: None,
        retry: Retry {
            max_attempts: 1,
            backoff: None,
            on: Vec::new(),
        },
        on_failure: FailurePolicy::Fail,
        followup: None,
        agent: None,
    }
}
impl Reader<'_> {
    fn err(&self, code: &str, path: &str) -> Diagnostic {
        diagnostic(self.spec, code, path)
    }
    fn object<'a>(&self, value: &'a Value, path: &str) -> Result<&'a Map<String, Value>> {
        value
            .as_object()
            .ok_or_else(|| self.err("spec.invalid_shape", path))
    }
    fn array<'a>(&self, value: &'a Value, path: &str) -> Result<&'a Vec<Value>> {
        value
            .as_array()
            .ok_or_else(|| self.err("spec.invalid_shape", path))
    }
    fn required<'a>(
        &self,
        map: &'a Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<&'a Value> {
        map.get(key)
            .ok_or_else(|| self.err("spec.required", &pointer(path, key)))
    }
    fn string(&self, value: &Value, path: &str) -> Result<String> {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| self.err("spec.invalid_shape", path))
    }
    fn required_string(&self, map: &Map<String, Value>, key: &str, path: &str) -> Result<String> {
        self.string(self.required(map, key, path)?, &pointer(path, key))
    }
    fn optional_string(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<Option<String>> {
        map.get(key)
            .map(|v| self.string(v, &pointer(path, key)))
            .transpose()
    }
    fn boolean(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
        default: bool,
    ) -> Result<bool> {
        map.get(key).map_or(Ok(default), |v| {
            v.as_bool()
                .ok_or_else(|| self.err("spec.invalid_shape", &pointer(path, key)))
        })
    }
    fn positive(&self, value: &Value, path: &str) -> Result<u32> {
        crate::schema::integer_value(value)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| self.err("spec.invalid_value", path))
    }
    /// A required positive count at most `cap` (`spec.limit_exceeded`).
    fn capped(&self, map: &Map<String, Value>, key: &str, path: &str, cap: u32) -> Result<u32> {
        let value = self.required_positive(map, key, path)?;
        if value > cap {
            return Err(self.err("spec.limit_exceeded", &pointer(path, key)));
        }
        Ok(value)
    }
    fn required_positive(&self, map: &Map<String, Value>, key: &str, path: &str) -> Result<u32> {
        self.positive(self.required(map, key, path)?, &pointer(path, key))
    }
    fn optional_positive(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
        default: u32,
    ) -> Result<u32> {
        map.get(key)
            .map_or(Ok(default), |v| self.positive(v, &pointer(path, key)))
    }
    /// LANG2: `all` (default), `any` (= `at_least: 1`) or `{at_least: n}`
    /// with 1 <= n <= the number of branches.
    fn join(&self, map: &Map<String, Value>, path: &str, branches: usize) -> Result<Join> {
        let p = pointer(path, "join");
        let Some(value) = map.get("join") else {
            return Ok(Join::All);
        };
        if let Value::Object(m) = value {
            if let Some(key) = m.keys().find(|k| *k != "at_least" && !k.starts_with("x-")) {
                return Err(self.err("spec.unknown_key", &pointer(&p, key)));
            }
            let at = pointer(&p, "at_least");
            let n = self.required_positive(m, "at_least", &p)?;
            if n as usize > branches {
                let mut error = self.err("spec.invalid_value", &at);
                error.message = Some(format!(
                    "`at_least` can be at most the number of branches ({branches})."
                ));
                return Err(error);
            }
            return Ok(Join::AtLeast(n));
        }
        match self.string(value, &p)?.as_str() {
            "all" => Ok(Join::All),
            "any" => Ok(Join::AtLeast(1)),
            _ => Err(self.err("spec.invalid_value", &p)),
        }
    }
    fn duration(&self, value: &Value, path: &str) -> Result<Duration> {
        let text = self.string(value, path)?;
        let total =
            parse_duration_millis(&text).ok_or_else(|| self.err("spec.invalid_value", path))?;
        Ok(Duration { millis: total })
    }
    fn optional_duration(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<Option<Duration>> {
        map.get(key)
            .map(|v| self.duration(v, &pointer(path, key)))
            .transpose()
    }
    fn actor(&self, map: &Map<String, Value>, path: &str, default: Actor) -> Result<Actor> {
        match self.optional_string(map, "actor", path)?.as_deref() {
            None => Ok(default),
            Some("human") => Ok(Actor::Human),
            Some("agent") => Ok(Actor::Agent),
            Some("any") => Ok(Actor::Any),
            _ => Err(self.err("spec.invalid_value", &pointer(path, "actor"))),
        }
    }
    fn failure(
        &self,
        map: &Map<String, Value>,
        path: &str,
        default: FailurePolicy,
    ) -> Result<FailurePolicy> {
        match self.optional_string(map, "on_failure", path)?.as_deref() {
            None => Ok(default),
            Some("fail") => Ok(FailurePolicy::Fail),
            Some("continue") => Ok(FailurePolicy::Continue),
            Some("handoff") => Ok(FailurePolicy::Handoff),
            _ => Err(self.err("spec.invalid_value", &pointer(path, "on_failure"))),
        }
    }
    fn schema(&self, map: &Map<String, Value>, key: &str, path: &str) -> Result<crate::DataSchema> {
        let Some(value) = map.get(key) else {
            return Ok(empty_schema());
        };
        let base = pointer(path, key);
        crate::parse_declarations(&declarations_without_extensions(value)).map_err(|errors| {
            let error = &errors[0];
            self.err(&error.code, &schema_pointer(&base, &error.path))
        })
    }
    fn fields(&self, map: &Map<String, Value>) -> Result<crate::DataSchema> {
        let schema = self.schema(map, "fields", "/spec")?;
        if let crate::DataType::Object { properties } = &schema.kind
            && properties
                .values()
                .filter(|s| s.indexed == Some(true))
                .count()
                > 10
        {
            return Err(self.err("spec.indexed_field_limit", "/spec/fields"));
        }
        Ok(schema)
    }
    fn retry(&self, map: &Map<String, Value>, path: &str, inherited: &Retry) -> Result<Retry> {
        let Some(value) = map.get("retry") else {
            return Ok(inherited.clone());
        };
        let p = pointer(path, "retry");
        let m = self.object(value, &p)?;
        Ok(Retry {
            max_attempts: m
                .get("max_attempts")
                .map(|v| self.positive(v, &pointer(&p, "max_attempts")))
                .transpose()?
                .unwrap_or(inherited.max_attempts),
            backoff: self
                .optional_duration(m, "backoff", &p)?
                .or(inherited.backoff),
            on: match m.get("on") {
                Some(v) => self.failure_types(v, &pointer(&p, "on"))?,
                None => inherited.on.clone(),
            },
        })
    }
    fn recipients(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<Vec<Recipient>> {
        self.optional_strings(map, key, path)?
            .into_iter()
            .enumerate()
            .map(|(i, s)| self.recipient(s, &pointer(&pointer(path, key), &i.to_string())))
            .collect()
    }
    fn string_map(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<BTreeMap<String, String>> {
        let Some(v) = map.get(key) else {
            return Ok(BTreeMap::new());
        };
        let p = pointer(path, key);
        self.object(v, &p)?
            .iter()
            .filter(|(k, _)| !k.starts_with("x-"))
            .map(|(k, v)| Ok((k.clone(), self.string(v, &pointer(&p, k))?)))
            .collect()
    }
    fn execution(&self, body: &Map<String, Value>) -> Result<ExecutionSettings> {
        let empty = Map::new();
        let p = "/spec/execution";
        let m = body
            .get("execution")
            .map(|v| self.object(v, p))
            .transpose()?
            .unwrap_or(&empty);
        let concurrency = if let Some(v) = m.get("concurrency") {
            let p = pointer(p, "concurrency");
            let m = self.object(v, &p)?;
            let on_conflict = match m.get("on_conflict") {
                None => ConcurrencyPolicy::Queue,
                Some(v) => match self.string(v, &pointer(&p, "on_conflict"))?.as_str() {
                    "queue" => ConcurrencyPolicy::Queue,
                    "reject" => ConcurrencyPolicy::Reject,
                    "cancel_previous" => ConcurrencyPolicy::CancelPrevious,
                    _ => {
                        return Err(self.err("spec.invalid_value", &pointer(&p, "on_conflict")));
                    }
                },
            };
            Some(Concurrency {
                key: self.required_string(m, "key", &p)?,
                limit: self.required_positive(m, "limit", &p)?,
                on_conflict,
            })
        } else {
            None
        };
        Ok(ExecutionSettings {
            title: self.optional_string(m, "title", p)?,
            tags: self.optional_strings(m, "tags", p)?,
            timeout: self.optional_duration(m, "timeout", p)?,
            cleanup_timeout: self
                .optional_duration(m, "cleanup_timeout", p)?
                .unwrap_or(Duration { millis: 1_800_000 }),
            concurrency,
        })
    }
    fn agent_params(&self, body: &Map<String, Value>) -> Result<Option<Map<String, Value>>> {
        let Some(agents) = body.get("agents") else {
            return Ok(None);
        };
        let agents = self.object(agents, "/spec/agents")?;
        agents
            .get("params")
            .map(|value| {
                // Capability validation checked the object and encoded-size budget
                // before this copy. Preserve every key and nested value verbatim.
                self.object(value, "/spec/agents/params").cloned()
            })
            .transpose()
    }
    fn task(&self, map: &Map<String, Value>, path: &str, def: &TaskDefaults) -> Result<Task> {
        let instructions = map
            .get("instructions")
            .map(|v| {
                let p = pointer(path, "instructions");
                if let Some(s) = v.as_str() {
                    Ok(Instructions::Markdown(s.into()))
                } else {
                    let m = self.object(v, &p)?;
                    Ok(Instructions::Audience {
                        default: self.optional_string(m, "default", &p)?,
                        human: self.optional_string(m, "human", &p)?,
                        agent: self.optional_string(m, "agent", &p)?,
                    })
                }
            })
            .transpose()?;
        let assignee = self
            .optional_string(map, "assignee", path)?
            .map(|s| {
                if s.contains("${{") {
                    Ok(Assignee::Expression(s))
                } else {
                    self.recipient(s, &pointer(path, "assignee"))
                        .map(Assignee::Recipient)
                }
            })
            .transpose()?;
        let assignment = match self.optional_string(map, "assignment", path)?.as_deref() {
            None | Some("guidance") => AssignmentMode::Guidance,
            Some("strict") if assignee.is_some() => AssignmentMode::Strict,
            Some("strict") => {
                let mut error = self.err(
                    "spec.strict_assignment_without_assignee",
                    &pointer(path, "assignment"),
                );
                error.message =
                    Some("`assignment: strict` needs an `assignee` to restrict to.".into());
                return Err(error);
            }
            _ => return Err(self.err("spec.invalid_value", &pointer(path, "assignment"))),
        };
        let agent = if let Some(v) = map.get("agent") {
            let agent_path = pointer(path, "agent");
            let m = self.object(v, &agent_path)?;
            // LANG9: `agent.hints.{tools,commands}` is the explicit, advisory
            // spelling; `agent.{tools,commands}` stays accepted. Both compile
            // to the same IR, so stored runbooks are unchanged. Giving one
            // hint both ways is ambiguous and refused.
            let hints = match m.get("hints") {
                Some(h) => Some(self.object(h, &pointer(&agent_path, "hints"))?),
                None => None,
            };
            let hint = |key: &str| -> Result<Option<Value>> {
                match (m.get(key), hints.and_then(|h| h.get(key))) {
                    (Some(_), Some(_)) => Err(self.err(
                        "spec.duplicate_agent_hint",
                        &pointer(&pointer(&agent_path, "hints"), key),
                    )),
                    (Some(v), None) | (None, Some(v)) => Ok(Some(v.clone())),
                    (None, None) => Ok(None),
                }
            };
            AgentHints {
                tools: hint("tools")?,
                commands: hint("commands")?,
            }
        } else {
            AgentHints::default()
        };
        // R0-3: the runner agent keys. The name falls back to
        // `spec.defaults.agent` for agent steps; publish resolves it.
        let actor = self.actor(map, path, def.actor)?;
        let (agent_name, agent_limits) = match map.get("agent") {
            Some(v) => {
                let agent_path = pointer(path, "agent");
                let m = self.object(v, &agent_path)?;
                (
                    self.agent_name(m, &agent_path)?,
                    self.agent_limits(m, &agent_path)?,
                )
            }
            None => (None, AgentLimits::default()),
        };
        if agent_name.is_some() && actor != Actor::Agent {
            return Err(self.err(
                "spec.agent_requires_agent_actor",
                &pointer(&pointer(path, "agent"), "name"),
            ));
        }
        let agent_target = match agent_name {
            Some(name) => Some(AgentTarget::Named { name }),
            None if actor == Actor::Agent => {
                def.agent.clone().map(|name| AgentTarget::Named { name })
            }
            None => None,
        };
        let connections = map
            .get("connections")
            .map(|value| self.strings(value, &pointer(path, "connections")))
            .transpose()?;
        let evidence = if let Some(v) = map.get("evidence") {
            let p = pointer(path, "evidence");
            let m = self.object(v, &p)?;
            Evidence {
                required: self.evidence_kinds(m, "required", &p)?,
                accepted: if m.contains_key("accepted") {
                    Some(self.evidence_kinds(m, "accepted", &p)?)
                } else {
                    None
                },
            }
        } else {
            Evidence::default()
        };
        let (skill, skills) = {
            let has_skill = map.contains_key("skill");
            let has_skills = map.contains_key("skills");
            if has_skill && has_skills {
                return Err(self.err("spec.both_skill_and_skills", &pointer(path, "skills")));
            }
            if has_skills {
                let raw_skills = self.strings(&map["skills"], &pointer(path, "skills"))?;
                if raw_skills.len() > 8 {
                    return Err(self.err("spec.too_many_skills", &pointer(path, "skills")));
                }
                let mut seen = std::collections::HashSet::new();
                for (i, s) in raw_skills.iter().enumerate() {
                    if !seen.insert(s) {
                        return Err(self.err(
                            "spec.duplicate_skill",
                            &pointer(&pointer(path, "skills"), &i.to_string()),
                        ));
                    }
                }
                let first = raw_skills.first().cloned();
                (first, raw_skills)
            } else if has_skill {
                let s = self.optional_string(map, "skill", path)?;
                let list = s.clone().into_iter().collect();
                (s, list)
            } else {
                (None, Vec::new())
            }
        };
        Ok(Task {
            actor,
            assignee,
            assignment,
            retry: self.retry(map, path, &def.retry)?,
            instructions,
            skill,
            skills,
            parameters: self.data_map(map, "with", path)?,
            checklist: self.optional_strings(map, "checklist", path)?,
            check: self.boolean(map, "check", path, false)?,
            skippable: self.boolean(map, "skippable", path, false)?,
            destructive: self.boolean(map, "destructive", path, false)?,
            allow_auto_decision: self.boolean(map, "allow_auto_decision", path, false)?,
            outputs: self.schema(map, "outputs", path)?,
            evidence,
            agent,
            agent_target,
            agent_limits: (!agent_limits.is_empty()).then_some(agent_limits),
            connections,
        })
    }
    /// R0-3: `agent.name` (a non-empty string of at most 128 characters).
    fn agent_name(&self, map: &Map<String, Value>, path: &str) -> Result<Option<String>> {
        let name = self.optional_string(map, "name", path)?;
        if let Some(name) = &name
            && (name.trim().is_empty() || name.chars().count() > 128)
        {
            return Err(self.err("spec.invalid_value", &pointer(path, "name")));
        }
        Ok(name)
    }
    /// R0-3: `agent.model`, `max_cost_usd` (> 0), `max_tokens` (a positive
    /// integer) and `network` (host names).
    fn agent_limits(&self, map: &Map<String, Value>, path: &str) -> Result<AgentLimits> {
        let max_cost_usd = map
            .get("max_cost_usd")
            .map(|v| {
                v.as_f64()
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .ok_or_else(|| self.err("spec.invalid_value", &pointer(path, "max_cost_usd")))
            })
            .transpose()?;
        let max_tokens = map
            .get("max_tokens")
            .map(|v| {
                crate::schema::integer_value(v)
                    .and_then(|n| u64::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| self.err("spec.invalid_value", &pointer(path, "max_tokens")))
            })
            .transpose()?;
        Ok(AgentLimits {
            model: self.optional_string(map, "model", path)?,
            max_cost_usd,
            max_tokens,
            network: map
                .contains_key("network")
                .then(|| self.optional_strings(map, "network", path))
                .transpose()?,
        })
    }
    fn evidence_kinds(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<Vec<EvidenceKind>> {
        self.optional_strings(map, key, path)?
            .iter()
            .enumerate()
            .map(|(i, s)| match s.as_str() {
                "link" => Ok(EvidenceKind::Link),
                "log" => Ok(EvidenceKind::Log),
                "file" => Ok(EvidenceKind::File),
                "screenshot" => Ok(EvidenceKind::Screenshot),
                _ => Err(self.err(
                    "spec.invalid_value",
                    &pointer(&pointer(path, key), &i.to_string()),
                )),
            })
            .collect()
    }
    fn runbook_slug(&self, metadata: &Map<String, Value>) -> Result<String> {
        let id = self.required_string(metadata, "id", "/metadata")?;
        if id.len() > MAX_RUNBOOK_ID_BYTES
            || !id.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        {
            return Err(self.err("spec.invalid_id", "/metadata/id"));
        }
        Ok(id)
    }

    fn run(&self) -> Result<NormalizedRunbook> {
        let root = self.object(self.spec.source_json(), "")?;
        let body = self.object(self.required(root, "spec", "")?, "/spec")?;
        let metadata = self.object(self.required(root, "metadata", "")?, "/metadata")?;
        let mut def = defaults();
        if let Some(value) = body.get("defaults") {
            let m = self.object(value, "/spec/defaults")?;
            def.actor = self.actor(m, "/spec/defaults", def.actor)?;
            def.timeout = self.optional_duration(m, "timeout", "/spec/defaults")?;
            def.on_failure = self.failure(m, "/spec/defaults", def.on_failure)?;
            def.retry = self.retry(m, "/spec/defaults", &def.retry)?;
            def.followup = self.followup(m, "/spec/defaults", None)?;
            if let Some(agent) = m.get("agent") {
                let p = "/spec/defaults/agent";
                let a = self.object(agent, p)?;
                def.agent = Some(
                    self.agent_name(a, p)?
                        .ok_or_else(|| self.err("spec.required", &pointer(p, "name")))?,
                );
            }
        }
        let inputs = self.schema(body, "inputs", "/spec")?;
        let triggers = self.triggers(body, &inputs)?;
        Ok(NormalizedRunbook {
            metadata: RunbookMetadata {
                id: self.runbook_slug(metadata)?,
                title: match self.optional_string(metadata, "title", "/metadata")? {
                    Some(title) => title,
                    None => derived_title(&self.runbook_slug(metadata)?),
                },
                description: self.optional_string(metadata, "description", "/metadata")?,
                tags: self.optional_strings(metadata, "tags", "/metadata")?,
                owners: self.recipients(metadata, "owners", "/metadata")?,
            },
            execution: self.execution(body)?,
            defaults: def.clone(),
            inputs,
            fields: self.fields(body)?,
            variables: self.data_map(body, "vars", "/spec")?,
            skills: self.string_map(body, "skills", "/spec")?,
            agent_params: self.agent_params(body)?,
            steps: self.body(self.required(body, "steps", "/spec")?, "/spec/steps", &def)?,
            finally: self.optional_body(body, "finally", "/spec", &def)?,
            outputs: self.data_map(body, "outputs", "/spec")?,
            report: self.optional_string(body, "report", "/spec")?,
            triggers,
            source_json: self.spec.source_json().clone(),
            source_yaml: self.spec.raw_source.clone(),
        })
    }
    /// LANG9b: `spec.triggers`. Each kind is a list (at most
    /// [`MAX_TRIGGERS`]) of entries with IDs unique across both kinds.
    /// Expressions are checked with the scopes; a schedule's inputs are
    /// literal values checked against the runbook's inputs here.
    fn triggers(&self, body: &Map<String, Value>, inputs: &crate::DataSchema) -> Result<Triggers> {
        let Some(value) = body.get("triggers") else {
            return Ok(Triggers::default());
        };
        let path = "/spec/triggers";
        let m = self.object(value, path)?;
        for key in m.keys() {
            if !matches!(key.as_str(), "schedule" | "webhook") && !key.starts_with("x-") {
                return Err(self.err("spec.unknown_key", &pointer(path, key)));
            }
        }
        let mut ids = BTreeSet::new();
        let mut entries = |kind: &str| -> Result<Vec<(String, &Map<String, Value>)>> {
            let Some(list) = m.get(kind) else {
                return Ok(vec![]);
            };
            let at = pointer(path, kind);
            let items = self.array(list, &at)?;
            if items.len() > MAX_TRIGGERS {
                return Err(self.err("spec.limit_exceeded", &at));
            }
            items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let p = pointer(&at, &i.to_string());
                    let map = self.object(item, &p)?;
                    let id = self.required_string(map, "id", &p)?;
                    if id.len() > 63
                        || !id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                        || !id
                            .bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                    {
                        return Err(self.err("spec.invalid_id", &pointer(&p, "id")));
                    }
                    if !ids.insert(id.clone()) {
                        return Err(self.err("spec.duplicate_trigger", &pointer(&p, "id")));
                    }
                    Ok((p, map))
                })
                .collect()
        };
        let schedule_maps = entries("schedule")?;
        let webhook_maps = entries("webhook")?;
        let allowed = |map: &Map<String, Value>, p: &str, keys: &[&str]| -> Result<()> {
            for key in map.keys() {
                if !keys.contains(&key.as_str()) && !key.starts_with("x-") {
                    return Err(self.err("spec.unknown_key", &pointer(p, key)));
                }
            }
            Ok(())
        };
        let mut schedule = vec![];
        for (p, map) in schedule_maps {
            allowed(map, &p, &["id", "cron", "timezone", "inputs", "catch_up"])?;
            let cron = self.required_string(map, "cron", &p)?;
            if crate::cron::Cron::parse(&cron).is_none() {
                return Err(self.err("spec.invalid_cron", &pointer(&p, "cron")));
            }
            let timezone = self
                .optional_string(map, "timezone", &p)?
                .unwrap_or_else(|| "UTC".to_owned());
            if crate::cron::time_zone(&timezone).is_none() {
                return Err(self.err("spec.invalid_timezone", &pointer(&p, "timezone")));
            }
            let catch_up = self.optional_duration(map, "catch_up", &p)?;
            if catch_up
                .is_some_and(|d| d.millis == 0 || d.millis > crate::cron::MAX_LOOKBACK_MS as u64)
            {
                return Err(self.err("spec.invalid_value", &pointer(&p, "catch_up")));
            }
            let values = match map.get("inputs") {
                None => BTreeMap::new(),
                Some(v) => self
                    .object(v, &pointer(&p, "inputs"))?
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            };
            self.trigger_inputs(inputs, &values, &pointer(&p, "inputs"), true)?;
            schedule.push(ScheduleTrigger {
                id: self.required_string(map, "id", &p)?,
                cron,
                timezone,
                inputs: values,
                catch_up,
            });
        }
        let mut webhook = vec![];
        for (p, map) in webhook_maps {
            allowed(map, &p, &["id", "payload", "when", "inputs", "dedupe"])?;
            let payload = map
                .contains_key("payload")
                .then(|| self.schema(map, "payload", &p))
                .transpose()?;
            let values: BTreeMap<String, Value> = match map.get("inputs") {
                None => BTreeMap::new(),
                Some(v) => self
                    .object(v, &pointer(&p, "inputs"))?
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            };
            // Mapped values are known only per call: check the names (and
            // literal values) here, the expressions with the scopes.
            self.trigger_inputs(inputs, &values, &pointer(&p, "inputs"), false)?;
            webhook.push(WebhookTrigger {
                id: self.required_string(map, "id", &p)?,
                payload,
                when: self.optional_string(map, "when", &p)?,
                inputs: values,
                dedupe: self.optional_string(map, "dedupe", &p)?,
            });
        }
        Ok(Triggers { schedule, webhook })
    }
    /// A trigger's inputs name declared inputs; literal values match their
    /// types; with `complete`, every required input without a default is
    /// given (a webhook's may come from expressions, checked per call).
    fn trigger_inputs(
        &self,
        inputs: &crate::DataSchema,
        values: &BTreeMap<String, Value>,
        path: &str,
        complete: bool,
    ) -> Result<()> {
        let empty = BTreeMap::new();
        let properties = match &inputs.kind {
            crate::DataType::Object { properties } => properties,
            _ => &empty,
        };
        for (name, value) in values {
            let at = pointer(path, name);
            let Some(schema) = properties.get(name) else {
                return Err(self.err("spec.trigger_input_invalid", &at));
            };
            // A schedule's inputs are literal values; a webhook's may be
            // expressions over its payload.
            let expression = !complete && value.as_str().is_some_and(|s| s.contains("${{"));
            if !expression && !crate::validate_data(schema, value).is_empty() {
                return Err(self.err("spec.trigger_input_invalid", &at));
            }
        }
        if complete {
            for (name, schema) in properties {
                if schema.required
                    && matches!(schema.default, crate::DefaultValue::Missing)
                    && !values.contains_key(name)
                {
                    return Err(self.err("spec.trigger_input_invalid", &pointer(path, name)));
                }
            }
        }
        Ok(())
    }
    fn body(&self, value: &Value, path: &str, def: &TaskDefaults) -> Result<Vec<Node>> {
        self.array(value, path)?
            .iter()
            .enumerate()
            .map(|(i, n)| self.node(n, &pointer(path, &i.to_string()), def))
            .collect()
    }
    fn optional_body(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
        def: &TaskDefaults,
    ) -> Result<Vec<Node>> {
        map.get(key)
            .map_or(Ok(vec![]), |v| self.body(v, &pointer(path, key), def))
    }
    fn node(&self, value: &Value, path: &str, def: &TaskDefaults) -> Result<Node> {
        let map = self.object(value, path)?;
        let id = self.required_string(map, "id", path)?;
        if id.len() > 63
            || !id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        {
            return Err(self.err("spec.invalid_id", &pointer(path, "id")));
        }
        let keys: Vec<_> = [
            "steps", "parallel", "foreach", "loop", "switch", "wait", "approval", "set", "end",
            "call",
        ]
        .into_iter()
        .filter(|k| map.contains_key(*k))
        .collect();
        if keys.len() > 1 {
            return Err(self.err("spec.control_conflict", path));
        }
        for key in map.keys() {
            let common = matches!(
                key.as_str(),
                "id" | "title" | "if" | "on_failure" | "timeout" | "labels"
            ) || key.starts_with("x-");
            let specific = match keys.first().copied() {
                None => {
                    FOLLOWUP_KEYS.contains(&key.as_str())
                        || matches!(
                            key.as_str(),
                            "actor"
                                | "assignee"
                                | "assignment"
                                | "retry"
                                | "instructions"
                                | "skill"
                                | "skills"
                                | "with"
                                | "checklist"
                                | "check"
                                | "skippable"
                                | "destructive"
                                | "allow_auto_decision"
                                | "outputs"
                                | "evidence"
                                | "agent"
                                | "connections"
                        )
                }
                Some("steps") => matches!(key.as_str(), "steps" | "catch" | "finally" | "catch_on"),
                Some("parallel") => matches!(key.as_str(), "parallel" | "join" | "fail_fast"),
                // LANG9: waits take reminders and escalation; approvals take
                // reminders (their escalation is `approval.escalate_to`).
                Some("wait") => key == "wait" || FOLLOWUP_KEYS.contains(&key.as_str()),
                Some("approval") => {
                    key == "approval" || matches!(key.as_str(), "remind_after" | "remind_every")
                }
                Some(control) => key == control,
            };
            if !common && !specific {
                return Err(self.err("spec.invalid_node_field", &pointer(path, key)));
            }
        }
        let control = keys.first().copied();
        // LANG7: a task without a title shows one derived from its ID;
        // control nodes keep an optional title.
        let title = match self.optional_string(map, "title", path)? {
            None if control.is_none() => Some(derived_title(&id)),
            title => title,
        };
        let on_failure = self.failure(
            map,
            path,
            if control.is_none() {
                def.on_failure
            } else {
                FailurePolicy::Fail
            },
        )?;
        let timeout = self
            .optional_duration(map, "timeout", path)?
            .or(if control.is_none() { def.timeout } else { None });
        let guard = self.optional_string(map, "if", path)?;
        let kind = match control {
            None => NodeKind::Task(Box::new(self.task(map, path, def)?)),
            Some("steps") => {
                let catch = self.optional_body(map, "catch", path, def)?;
                let catch_on = match map.get("catch_on") {
                    Some(v) => {
                        if catch.is_empty() {
                            return Err(
                                self.err("spec.catch_on_without_catch", &pointer(path, "catch_on"))
                            );
                        }
                        self.failure_types(v, &pointer(path, "catch_on"))?
                    }
                    None => Vec::new(),
                };
                NodeKind::Group(Group {
                    children: self.body(&map["steps"], &pointer(path, "steps"), def)?,
                    catch,
                    finally: self.optional_body(map, "finally", path, def)?,
                    catch_on,
                })
            }
            Some("parallel") => {
                let children = self.body(&map["parallel"], &pointer(path, "parallel"), def)?;
                if children.len() > MAX_PARALLEL_BRANCHES {
                    return Err(self.err("spec.limit_exceeded", &pointer(path, "parallel")));
                }
                let join = self.join(map, path, children.len())?;
                let fail_fast = self.boolean(map, "fail_fast", path, false)?;
                if fail_fast && !join.is_all() {
                    // LANG2: an `any`/`at_least` join already cancels the
                    // open branches once it is decided.
                    let mut error = self.err("spec.invalid_shape", &pointer(path, "fail_fast"));
                    error.message = Some("`fail_fast` applies to `join: all` only.".into());
                    return Err(error);
                }
                NodeKind::Parallel {
                    children,
                    fail_fast,
                    join,
                }
            }
            Some("foreach") => {
                let p = pointer(path, "foreach");
                let m = self.object(&map["foreach"], &p)?;
                NodeKind::Foreach(Foreach {
                    items: self.required_string(m, "items", &p)?,
                    alias: self
                        .optional_string(m, "as", &p)?
                        .unwrap_or_else(|| "item".into()),
                    key: self.optional_string(m, "key", &p)?,
                    concurrency: m
                        .get("concurrency")
                        .map(|v| self.positive(v, &pointer(&p, "concurrency")))
                        .transpose()?
                        .unwrap_or(1),
                    max_items: self.capped(m, "max_items", &p, MAX_FOREACH_ITEMS)?,
                    children: self.body(
                        self.required(m, "steps", &p)?,
                        &pointer(&p, "steps"),
                        def,
                    )?,
                })
            }
            Some("loop") => {
                let p = pointer(path, "loop");
                let m = self.object(&map["loop"], &p)?;
                // LANG1: exactly one of `until` and `while`.
                let (until, r#while) = match (m.contains_key("until"), m.contains_key("while")) {
                    (true, true) => {
                        let mut error = self.err("spec.invalid_shape", &p);
                        error.message =
                            Some("A loop takes either `until` or `while`, not both.".into());
                        return Err(error);
                    }
                    (false, true) => (None, Some(self.required_string(m, "while", &p)?)),
                    _ => (Some(self.required_string(m, "until", &p)?), None),
                };
                NodeKind::Loop(Loop {
                    until,
                    r#while,
                    max_iterations: self.capped(m, "max_iterations", &p, MAX_LOOP_ITERATIONS)?,
                    delay: self.optional_duration(m, "delay", &p)?,
                    children: self.body(
                        self.required(m, "steps", &p)?,
                        &pointer(&p, "steps"),
                        def,
                    )?,
                })
            }
            Some("switch") => {
                let p = pointer(path, "switch");
                let m = self.object(&map["switch"], &p)?;
                let cp = pointer(&p, "cases");
                let mut cases = vec![];
                for (i, c) in self
                    .array(self.required(m, "cases", &p)?, &cp)?
                    .iter()
                    .enumerate()
                {
                    let p = pointer(&cp, &i.to_string());
                    let c = self.object(c, &p)?;
                    cases.push(Case {
                        when: self.required_string(c, "when", &p)?,
                        source_path: p.clone(),
                        children: self.body(
                            self.required(c, "steps", &p)?,
                            &pointer(&p, "steps"),
                            def,
                        )?,
                    });
                }
                let default = if let Some(v) = m.get("default") {
                    let p = pointer(&p, "default");
                    let b = self.object(v, &p)?;
                    self.body(self.required(b, "steps", &p)?, &pointer(&p, "steps"), def)?
                } else {
                    vec![]
                };
                NodeKind::Switch(Switch { cases, default })
            }
            Some("wait") => {
                let p = pointer(path, "wait");
                let m = self.object(&map["wait"], &p)?;
                let kinds: Vec<_> = ["for", "until", "signal"]
                    .into_iter()
                    .filter(|k| m.contains_key(*k))
                    .collect();
                if kinds.len() != 1 {
                    return Err(self.err("spec.invalid_shape", &p));
                }
                let key = kinds[0];
                let kind = match key {
                    "for" => {
                        let at = pointer(&p, key);
                        let duration = self.duration(&m[key], &at)?;
                        if duration.millis > MAX_WAIT_MS {
                            let mut error = self.err("spec.limit_exceeded", &at);
                            error.message =
                                Some("A single wait can last at most 7 days (PRD §17).".into());
                            return Err(error);
                        }
                        WaitKind::For(duration)
                    }
                    "until" => WaitKind::Until(self.string(&m[key], &pointer(&p, key))?),
                    _ => WaitKind::Signal(self.string(&m[key], &pointer(&p, key))?),
                };
                // LANG9: only a signal carries a payload to check.
                let outputs = if m.contains_key("outputs") {
                    if key != "signal" {
                        return Err(
                            self.err("spec.wait_outputs_requires_signal", &pointer(&p, "outputs"))
                        );
                    }
                    Some(self.schema(m, "outputs", &p)?)
                } else {
                    None
                };
                NodeKind::Wait(Wait {
                    kind,
                    timeout: self.optional_duration(m, "timeout", &p)?,
                    outputs,
                })
            }
            Some("approval") => {
                NodeKind::Approval(self.approval(&map["approval"], &pointer(path, "approval"))?)
            }
            Some("set") => {
                let p = pointer(path, "set");
                let m = self.object(&map["set"], &p)?;
                NodeKind::Set(Assignments {
                    variables: self.data_map(m, "vars", &p)?,
                    fields: self.data_map(m, "fields", &p)?,
                })
            }
            Some("end") => {
                let p = pointer(path, "end");
                let m = self.object(&map["end"], &p)?;
                let status = match self.required_string(m, "status", &p)?.as_str() {
                    "succeeded" => EndStatus::Succeeded,
                    "failed" => EndStatus::Failed,
                    "cancelled" => EndStatus::Cancelled,
                    _ => return Err(self.err("spec.invalid_value", &pointer(&p, "status"))),
                };
                NodeKind::End(End {
                    status,
                    message: self.optional_string(m, "message", &p)?,
                })
            }
            Some("call") => {
                NodeKind::Call(Box::new(self.call(&map["call"], &pointer(path, "call"))?))
            }
            _ => unreachable!(),
        };
        let labels = self.labels(map, path)?;
        let followup = match control {
            None => self.followup(map, path, def.followup.as_ref())?,
            Some("wait" | "approval") => self.followup(map, path, None)?,
            _ => None,
        };
        Ok(Node {
            id,
            source_path: path.into(),
            title,
            guard,
            on_failure,
            timeout,
            kind,
            labels,
            followup,
        })
    }
    /// LANG9: `remind_after`, `remind_every`, `escalate_after` and
    /// `escalate_to`, each falling back to `inherited` (the defaults) when
    /// absent. `remind_every` needs `remind_after`; `escalate_after` and
    /// `escalate_to` come together; durations are positive.
    fn followup(
        &self,
        map: &Map<String, Value>,
        path: &str,
        inherited: Option<&Followup>,
    ) -> Result<Option<Followup>> {
        let inherited = inherited.cloned().unwrap_or_default();
        let mut escalate_to = inherited.escalate_to;
        if map.contains_key("escalate_to") {
            let names = self.optional_strings(map, "escalate_to", path)?;
            if names.is_empty() {
                return Err(self.err("spec.followup_invalid", &pointer(path, "escalate_to")));
            }
            escalate_to = names
                .into_iter()
                .enumerate()
                .map(|(i, name)| {
                    self.recipient(
                        name,
                        &pointer(&pointer(path, "escalate_to"), &i.to_string()),
                    )
                })
                .collect::<Result<_>>()?;
        }
        let followup = Followup {
            remind_after: self
                .optional_duration(map, "remind_after", path)?
                .or(inherited.remind_after),
            remind_every: self
                .optional_duration(map, "remind_every", path)?
                .or(inherited.remind_every),
            escalate_after: self
                .optional_duration(map, "escalate_after", path)?
                .or(inherited.escalate_after),
            escalate_to,
        };
        for (key, value) in [
            ("remind_after", followup.remind_after),
            ("remind_every", followup.remind_every),
            ("escalate_after", followup.escalate_after),
        ] {
            if value.is_some_and(|d| d.millis == 0) {
                return Err(self.err("spec.followup_invalid", &pointer(path, key)));
            }
        }
        if followup.remind_every.is_some() && followup.remind_after.is_none() {
            return Err(self.err("spec.followup_invalid", &pointer(path, "remind_every")));
        }
        if followup.escalate_after.is_some() != !followup.escalate_to.is_empty() {
            let key = if followup.escalate_after.is_some() {
                "escalate_after"
            } else {
                "escalate_to"
            };
            return Err(self.err("spec.followup_invalid", &pointer(path, key)));
        }
        Ok((!followup.is_empty()).then_some(followup))
    }
    /// LANG9: a non-empty list of distinct failure types (`retry.on`,
    /// `catch_on`).
    fn failure_types(&self, value: &Value, path: &str) -> Result<Vec<FailureType>> {
        let items = self.array(value, path)?;
        if items.is_empty() {
            return Err(self.err("spec.invalid_value", path));
        }
        let mut out = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let at = pointer(path, &i.to_string());
            let name = self.string(item, &at)?;
            let Some(kind) = FailureType::parse(&name) else {
                return Err(self.err("spec.invalid_value", &at));
            };
            if out.contains(&kind) {
                return Err(self.err("spec.invalid_value", &at));
            }
            out.push(kind);
        }
        Ok(out)
    }
    /// LANG9: `labels`, a flat map of short strings, at most 16 entries.
    /// Keys are lowercase (`[a-z0-9][a-z0-9_./-]*`, at most 63 bytes);
    /// values are strings of at most 256 bytes.
    fn labels(
        &self,
        map: &serde_json::Map<String, Value>,
        path: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let Some(v) = map.get("labels") else {
            return Ok(Default::default());
        };
        let p = pointer(path, "labels");
        let m = self.object(v, &p)?;
        if m.len() > 16 {
            return Err(self.err("spec.invalid_labels", &p));
        }
        let mut labels = std::collections::BTreeMap::new();
        for (key, value) in m {
            let kp = pointer(&p, key);
            let key_ok = !key.is_empty()
                && key.len() <= 63
                && key
                    .bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && key.bytes().all(|c| {
                    c.is_ascii_lowercase()
                        || c.is_ascii_digit()
                        || matches!(c, b'_' | b'.' | b'/' | b'-')
                });
            match value {
                Value::String(s) if key_ok && s.len() <= 256 => {
                    labels.insert(key.clone(), s.clone());
                }
                _ => return Err(self.err("spec.invalid_labels", &kp)),
            }
        }
        Ok(labels)
    }
    fn strings(&self, value: &Value, path: &str) -> Result<Vec<String>> {
        self.array(value, path)?
            .iter()
            .enumerate()
            .map(|(i, v)| self.string(v, &pointer(path, &i.to_string())))
            .collect()
    }
    fn optional_strings(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<Vec<String>> {
        map.get(key)
            .map_or(Ok(vec![]), |v| self.strings(v, &pointer(path, key)))
    }
    fn data_map(
        &self,
        map: &Map<String, Value>,
        key: &str,
        path: &str,
    ) -> Result<BTreeMap<String, Value>> {
        map.get(key).map_or(Ok(BTreeMap::new()), |v| {
            Ok(self
                .object(v, &pointer(path, key))?
                .iter()
                .filter(|(k, _)| !k.starts_with("x-"))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        })
    }
    fn recipient(&self, text: String, path: &str) -> Result<Recipient> {
        if let Some(name) = text.strip_prefix("team:") {
            if !name.is_empty() {
                return Ok(Recipient::Team(name.into()));
            }
        } else if let Some(name) = text.strip_prefix("user:") {
            if !name.is_empty() {
                return Ok(Recipient::User(name.into()));
            }
        } else if !text.is_empty() && !text.contains(':') {
            return Ok(Recipient::User(text));
        }
        Err(self.err("spec.invalid_value", path))
    }
    /// LANG4 `call`: `runbook` (a slug), an optional positive `version`,
    /// `with` (child inputs) and `outputs` (a data schema, as on a task).
    fn call(&self, value: &Value, path: &str) -> Result<Call> {
        let map = self.object(value, path)?;
        for key in map.keys() {
            if !matches!(key.as_str(), "runbook" | "version" | "with" | "outputs")
                && !key.starts_with("x-")
            {
                return Err(self.err("spec.invalid_node_field", &pointer(path, key)));
            }
        }
        let runbook = self.required_string(map, "runbook", path)?;
        if runbook_model::parse_runbook_ref(&runbook).is_err() || runbook.contains('@') {
            let mut error = self.err("spec.invalid_value", &pointer(path, "runbook"));
            error.message = Some("A called runbook is named by its slug (kebab-case).".into());
            return Err(error);
        }
        let version = map
            .get("version")
            .map(|v| self.positive(v, &pointer(path, "version")))
            .transpose()?;
        Ok(Call {
            runbook,
            version,
            with: self.data_map(map, "with", path)?,
            outputs: self.schema(map, "outputs", path)?,
            target: None,
        })
    }
    fn approval(&self, value: &Value, path: &str) -> Result<Approval> {
        let map = self.object(value, path)?;
        if !map.contains_key("timeout") {
            for key in ["on_timeout", "escalate_to", "escalation_timeout"] {
                if map.contains_key(key) {
                    return Err(self.err("spec.on_timeout_requires_timeout", &pointer(path, key)));
                }
            }
        }
        let p = pointer(path, "options");
        let mut options = vec![];
        let mut ids = BTreeSet::new();
        for (i, v) in self
            .array(self.required(map, "options", path)?, &p)?
            .iter()
            .enumerate()
        {
            let p = pointer(&p, &i.to_string());
            let option = if let Some(id) = v.as_str() {
                ApprovalOption {
                    id: id.into(),
                    title: id.into(),
                    auto: false,
                }
            } else {
                let m = self.object(v, &p)?;
                let id = self.required_string(m, "id", &p)?;
                ApprovalOption {
                    title: self
                        .optional_string(m, "title", &p)?
                        .unwrap_or_else(|| id.clone()),
                    id,
                    auto: self.boolean(m, "auto", &p, false)?,
                }
            };
            if option.id.is_empty() || !ids.insert(option.id.clone()) {
                return Err(self.err("spec.invalid_value", &p));
            }
            options.push(option);
        }
        if options.is_empty() {
            return Err(self.err("spec.invalid_value", &p));
        }
        let mut approvers = vec![];
        if let Some(v) = map.get("approvers") {
            let p = pointer(path, "approvers");
            let m = self.object(v, &p)?;
            for name in self.optional_strings(m, "users", &p)? {
                if name.is_empty() {
                    return Err(self.err("spec.invalid_value", &pointer(&p, "users")));
                }
                approvers.push(Recipient::User(name));
            }
            for name in self.optional_strings(m, "teams", &p)? {
                if name.is_empty() {
                    return Err(self.err("spec.invalid_value", &pointer(&p, "teams")));
                }
                approvers.push(Recipient::Team(name));
            }
        }
        let mut escalate_to = vec![];
        for (i, name) in self
            .optional_strings(map, "escalate_to", path)?
            .into_iter()
            .enumerate()
        {
            escalate_to.push(self.recipient(
                name,
                &pointer(&pointer(path, "escalate_to"), &i.to_string()),
            )?);
        }
        let timeout = self.optional_duration(map, "timeout", path)?;
        let on_timeout = match map.get("on_timeout") {
            None => ApprovalTimeout::Escalate,
            Some(Value::String(s)) if s == "escalate" => ApprovalTimeout::Escalate,
            Some(v) => {
                let p = pointer(path, "on_timeout");
                let m = self.object(v, &p)?;
                ApprovalTimeout::Decide(self.required_string(m, "decide", &p)?)
            }
        };
        let on_expired = match self.optional_string(map, "on_expired", path)?.as_deref() {
            None | Some("pause") => ExpiredPolicy::Pause,
            Some("fail") => ExpiredPolicy::Fail,
            _ => return Err(self.err("spec.invalid_value", &pointer(path, "on_expired"))),
        };
        let approves = if map.contains_key("approves") {
            let values = self.optional_strings(map, "approves", path)?;
            let mut seen = BTreeSet::new();
            for (index, option) in values.iter().enumerate() {
                if !ids.contains(option) || !seen.insert(option) {
                    return Err(self.err(
                        "spec.invalid_value",
                        &pointer(&pointer(path, "approves"), &index.to_string()),
                    ));
                }
            }
            Some(values)
        } else {
            None
        };
        let allow_agents = self.boolean(map, "allow_agents", path, false)?;
        let agent_path = pointer(path, "agent");
        let (agent_target, agent_limits, agent_after) = if let Some(agent) = map.get("agent") {
            if !allow_agents {
                return Err(self.err("spec.agent_requires_allow_agents", &agent_path));
            }
            let agent = self.object(agent, &agent_path)?;
            let limits = self.agent_limits(agent, &agent_path)?;
            let name = self
                .agent_name(agent, &agent_path)?
                .ok_or_else(|| self.err("spec.required", &pointer(&agent_path, "name")))?;
            let target = Some(AgentTarget::Named { name });
            (
                target,
                (!limits.is_empty()).then_some(limits),
                self.optional_duration(agent, "after", &agent_path)?,
            )
        } else {
            (None, None, None)
        };
        Ok(Approval {
            prompt: self.required_string(map, "prompt", path)?,
            options,
            approves,
            approvers,
            min: self.optional_positive(map, "min", path, 1)?,
            timeout,
            on_timeout,
            escalate_to: map.contains_key("escalate_to").then_some(escalate_to),
            escalation_timeout: self
                .optional_duration(map, "escalation_timeout", path)?
                .or(timeout),
            on_expired,
            allow_agents,
            agent_target,
            agent_limits,
            agent_after,
        })
    }
}

// Schema diagnostics use dotted identifiers and JSON-quoted bracket components.
// Translate that exact grammar into source JSON pointers, preserving key spelling.
fn schema_pointer(base: &str, mut rest: &str) -> String {
    let mut path = base.to_owned();
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix('.') {
            rest = tail;
            continue;
        }
        if let Some(tail) = rest.strip_prefix('[') {
            if tail.starts_with('"') {
                let mut stream = serde_json::Deserializer::from_str(tail).into_iter::<String>();
                if let Some(Ok(key)) = stream.next() {
                    let end = stream.byte_offset();
                    if tail.as_bytes().get(end) == Some(&b']') {
                        path = pointer(&path, &key);
                        rest = &tail[end + 1..];
                        continue;
                    }
                }
            } else if let Some(end) = tail.find(']') {
                path = pointer(&path, &tail[..end]);
                rest = &tail[end + 1..];
                continue;
            }
            // The schema parser owns this grammar; preserve the enclosing source
            // location if a future diagnostic format introduces a new component.
            break;
        }
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        path = pointer(&path, &rest[..end]);
        rest = &rest[end..];
    }
    path
}

// Source declaration maps reserve x-* keys for opaque metadata (A2). Standalone
// DataSchema APIs intentionally allow arbitrary user property names, so keep this
// source-language projection here and preserve the full original source above.
// normalize's source preflight bounds this tree before copying or descending.
fn declarations_without_extensions(value: &Value) -> Value {
    fn declaration(value: &Value) -> Value {
        let Value::Object(map) = value else {
            return value.clone();
        };
        Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let value = match key.as_str() {
                        "properties" => declarations_without_extensions(value),
                        "items" => declaration(value),
                        _ => value.clone(),
                    };
                    (key.clone(), value)
                })
                .collect(),
        )
    }
    let Value::Object(map) = value else {
        return value.clone();
    };
    Value::Object(
        map.iter()
            .filter(|(key, _)| !key.starts_with("x-"))
            .map(|(key, value)| (key.clone(), declaration(value)))
            .collect(),
    )
}
