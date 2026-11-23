//! Checked-value evaluation and atomic data facts. The reducer never evaluates CEL.
mod bindings;
mod context;
use crate::{EngineError, Event, ExecutionState};
use runbook_expr::{EvalError, EvalSession, Origin, ReadRef, RecordIdentity};
use runbook_model::{ActorRef, Data, ExecutionStatus, StepRunId, StepStatus};
use runbook_spec::{
    CheckedValue,
    ir::{Node, NodeKind},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StartContext {
    #[serde(default)]
    pub limits: crate::ExecutionLimits,
    #[serde(default, with = "runbook_spec::stored_data")]
    pub inputs: Data,
    #[serde(default, with = "runbook_spec::stored_data")]
    pub fields: Data,
    #[serde(default, with = "runbook_spec::stored_data")]
    pub project_vars: Data,
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub url: String,
    pub created_by: String,
    /// LANG4: set on a child execution started by a `call` step. Absent
    /// otherwise, so other start records keep their bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<crate::ParentLink>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InitialData {
    pub context: StartContext,
    #[serde(with = "runbook_spec::stored_data")]
    pub vars: Data,
    pub reads: Vec<ReadRef>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataChanged {
    pub step_run_id: StepRunId,
    pub base_sequence: u64,
    #[serde(with = "runbook_spec::stored_data")]
    pub fields: Data,
    #[serde(with = "runbook_spec::stored_data")]
    pub vars: Data,
    pub reads: Vec<ReadRef>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutionOutputs {
    #[serde(with = "runbook_spec::stored_data")]
    pub values: Data,
    pub reads: Vec<ReadRef>,
}
fn object(data: &Data) -> Value {
    Value::Object(data.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}
fn pointer(base: &str, key: &str) -> String {
    format!("{base}/{}", key.replace('~', "~0").replace('/', "~1"))
}
// Every compiled literal is already limited by source depth128, and evaluated
// output by depth32. Reject deeper event/request trees before recursive cloning.
pub(crate) fn guard_copy(data: &Data) -> Result<(), EngineError> {
    fn visit(v: &Value, depth: usize) -> Result<(), EngineError> {
        if depth > 128 {
            return Err(EngineError::new("data.depth_limit"));
        }
        match v {
            Value::Array(v) => {
                for item in v {
                    visit(item, depth + 1)?;
                }
            }
            Value::Object(v) => {
                for item in v.values() {
                    visit(item, depth + 1)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    for value in data.values() {
        visit(value, 1)?;
    }
    Ok(())
}
// Bound JSON copies by the existing expression-ingress ceilings. The typed
// binding helper still performs the authoritative metadata/type validation.
#[derive(Default)]
struct CopyAdmission {
    nodes: usize,
    bytes: usize,
}
impl CopyAdmission {
    fn text(&mut self, text: &str) -> Result<(), EngineError> {
        if text.len() > 65_536 {
            return Err(EngineError::new("expression.binding_limit"));
        }
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .filter(|n| *n <= 8_388_608)
            .ok_or(EngineError::new("expression.binding_limit"))?;
        Ok(())
    }
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), EngineError> {
        if depth > 32 || self.nodes == 262_144 {
            return Err(EngineError::new("expression.binding_limit"));
        }
        self.nodes += 1;
        match value {
            Value::String(text) => self.text(text)?,
            Value::Number(number) => {
                self.bytes = self
                    .bytes
                    .checked_add(number.as_str().len())
                    .filter(|n| *n <= 8_388_608)
                    .ok_or(EngineError::new("expression.binding_limit"))?;
            }
            Value::Array(items) => {
                if items.len() > 100_000 {
                    return Err(EngineError::new("expression.binding_limit"));
                }
                for item in items {
                    self.value(item, depth + 1)?;
                }
            }
            Value::Object(items) => {
                for (key, item) in items {
                    self.text(key)?;
                    self.value(item, depth + 1)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    fn data(&mut self, data: &Data) -> Result<(), EngineError> {
        for (key, value) in data {
            self.text(key)?;
            self.value(value, 1)?;
        }
        Ok(())
    }
    fn overlay(&mut self, base: &Data, writes: Option<&Data>) -> Result<(), EngineError> {
        for (key, value) in base
            .iter()
            .filter(|(key, _)| writes.is_none_or(|w| !w.contains_key(*key)))
        {
            self.text(key)?;
            self.value(value, 1)?;
        }
        if let Some(writes) = writes {
            self.data(writes)?;
        }
        Ok(())
    }
    fn metadata(
        &mut self,
        title: Option<&str>,
        tags: &[String],
        url: &str,
        by: &str,
    ) -> Result<(), EngineError> {
        if let Some(title) = title {
            self.text(title)?;
        }
        if tags.len() > 100_000 {
            return Err(EngineError::new("expression.binding_limit"));
        }
        for tag in tags {
            self.text(tag)?;
        }
        self.text(url)?;
        self.text(by)
    }
}
fn admit_context(context: &StartContext) -> Result<(), EngineError> {
    let mut admission = CopyAdmission::default();
    for data in [&context.inputs, &context.fields, &context.project_vars] {
        guard_copy(data)?;
        admission.data(data)?;
    }
    admission.metadata(
        context.title.as_deref(),
        &context.tags,
        &context.url,
        &context.created_by,
    )
}
fn admit_runtime(state: &ExecutionState, changes: Option<&DataChanged>) -> Result<(), EngineError> {
    let mut admission = CopyAdmission::default();
    admission.data(&state.inputs)?;
    admission.data(&state.project_vars)?;
    admission.overlay(&state.fields, changes.map(|c| &c.fields))?;
    admission.overlay(&state.vars, changes.map(|c| &c.vars))?;
    admission.metadata(
        Some(&state.title),
        &state.tags,
        &state.url,
        &state.created_by,
    )?;
    for step in state.steps.iter().filter(|s| s.id.is_some()) {
        admission.text(&step.node_id)?;
        if let Some(snapshot) = &step.foreach {
            admission.value(&snapshot.items, 0)?;
        }
        if step.status == StepStatus::Succeeded {
            admission.data(&step.outputs)?;
        }
        match &step.actor {
            Some(ActorRef::Agent { principal_id, .. }) => admission.text(principal_id)?,
            Some(ActorRef::Human { user_id }) => admission.text(user_id.as_str())?,
            _ => (),
        }
    }
    Ok(())
}
fn resolve(schema: &runbook_spec::DataSchema, data: &Data) -> Result<Data, EngineError> {
    guard_copy(data)?;
    CopyAdmission::default().data(data)?;
    let resolved = runbook_spec::resolve_data(schema, &object(data)).map_err(|errors| {
        EngineError::Rejected {
            code: "execution.invalid_data",
            errors: errors
                .into_iter()
                .map(|d| runbook_model::FieldError {
                    path: d.path,
                    code: d.code,
                    message: "Execution data does not satisfy its declaration.".into(),
                })
                .collect(),
        }
    })?;
    let Value::Object(map) = resolved else {
        return Err(EngineError::new("execution.invalid_data"));
    };
    Ok(map.into_iter().collect())
}
fn identity(state: &ExecutionState, revision: u64) -> RecordIdentity {
    RecordIdentity {
        execution_id: state.id.to_string(),
        step_run_id: None,
        iteration_path: vec![],
        revision,
    }
}
fn at_ms(ms: i64) -> runbook_expr::EventTime {
    runbook_expr::EventTime {
        unix_seconds: ms.div_euclid(1000),
        nanos: ms.rem_euclid(1000) as u32 * 1_000_000,
    }
}
fn timestamp(ms: i64) -> Result<Value, EngineError> {
    runbook_model::Timestamp::from_timestamp(
        ms.div_euclid(1000),
        ms.rem_euclid(1000) as u32 * 1_000_000,
    )
    .map(|t| Value::String(t.to_rfc3339()))
    .ok_or(EngineError::new("expression.binding_error"))
}
fn roots(state: &ExecutionState) -> Result<BTreeMap<String, Value>, EngineError> {
    let mut execution = serde_json::Map::from_iter([
        ("id".into(), json!(state.id.as_str())),
        ("title".into(), json!(state.title)),
        ("url".into(), json!(state.url)),
        (
            "web_url".into(),
            json!(runbook_model::execution_web_url(&state.url, &state.id)),
        ),
        ("created_by".into(), json!(state.created_by)),
        ("tags".into(), json!(state.tags)),
    ]);
    if let Some(time) = state.started_at_ms {
        execution.insert("started_at".into(), timestamp(time)?);
    }
    Ok(BTreeMap::from([
        ("inputs".into(), object(&state.inputs)),
        ("fields".into(), object(&state.fields)),
        ("vars".into(), object(&state.vars)),
        (
            "project".into(),
            json!({"vars":object(&state.project_vars)}),
        ),
        ("execution".into(), Value::Object(execution)),
        ("steps".into(), json!({})),
    ]))
}
fn bound(
    state: &ExecutionState,
    env: &runbook_expr::TypeEnv,
) -> Result<runbook_expr::Bindings, EngineError> {
    bound_scoped(state, env, &[], None)
}
fn bound_scoped(
    state: &ExecutionState,
    env: &runbook_expr::TypeEnv,
    scope: &[crate::IterationRef],
    extra: Option<context::ExtraItem<'_>>,
) -> Result<runbook_expr::Bindings, EngineError> {
    admit_runtime(state, None)?;
    let mut values = roots(state)?;
    if env.binding_roots().contains_key("outputs") {
        // Charge before cloning completion data, then charge the full projection below.
        CopyAdmission::default().data(&state.outputs)?;
        values.insert("outputs".into(), object(&state.outputs));
    }
    let (mut overrides, aliases) = context::populate(state, &mut values, scope, extra)?;
    if env.binding_roots().contains_key("outputs") {
        if state.output_revision == 0 {
            return Err(EngineError::invariant(
                "report outputs have no completion revision",
            ));
        }
        overrides.insert("outputs".into(), identity(state, state.output_revision));
    }
    let root = Origin {
        path: String::new(),
        record: identity(state, state.data_revision.max(1)),
    };
    bindings::build_scoped(
        env,
        &values,
        &state.revisions,
        &overrides,
        &aliases,
        &root,
        at_ms(state.at_ms),
    )
}
/// LANG9b: the bindings of a webhook trigger's expressions, outside any
/// run: only `payload` (already reduced to its declared fields).
pub(crate) fn trigger_bindings(
    env: &runbook_expr::TypeEnv,
    payload: Option<&Value>,
    now_ms: i64,
) -> Result<runbook_expr::Bindings, EngineError> {
    let record = RecordIdentity {
        execution_id: "trigger".into(),
        step_run_id: None,
        iteration_path: vec![],
        revision: 1,
    };
    let mut values = BTreeMap::new();
    let mut revisions = BTreeMap::new();
    if let Some(payload) = payload {
        values.insert("payload".to_owned(), payload.clone());
        revisions.insert("payload".to_owned(), record.clone());
    }
    let root = Origin {
        path: String::new(),
        record,
    };
    bindings::build_scoped(
        env,
        &values,
        &revisions,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &root,
        at_ms(now_ms),
    )
}
pub(crate) fn trigger_eval_error(e: EvalError) -> EngineError {
    eval_error(e)
}
fn eval_error(e: EvalError) -> EngineError {
    EngineError::new(match e {
        EvalError::CostExceeded => "expression.cost_exceeded",
        EvalError::Cancelled => "expression.cancelled",
        EvalError::ReadLimit => "expression.read_limit",
        EvalError::OutputLimit => "expression.output_limit",
        EvalError::SchemaMismatch => "expression.schema_mismatch",
        EvalError::MissingValue { .. } => "expression.missing_value",
        EvalError::TypeError => "expression.type_error",
        EvalError::IndexOutOfBounds => "expression.index_out_of_bounds",
    })
}
pub(crate) fn append_reads(out: &mut Vec<ReadRef>, incoming: impl IntoIterator<Item = ReadRef>) {
    let mut seen: BTreeSet<_> = out.iter().cloned().collect();
    out.extend(incoming.into_iter().filter(|r| seen.insert(r.clone())));
}
fn evaluate_value(
    state: &ExecutionState,
    value: &CheckedValue,
    scope: &[crate::IterationRef],
    extra: Option<context::ExtraItem<'_>>,
    reads: &mut Vec<ReadRef>,
) -> Result<Value, EngineError> {
    match value {
        CheckedValue::Literal(value) => Ok(value.clone()),
        CheckedValue::Template(index) => {
            let site = state
                .compiled()
                .checked_site(*index)
                .ok_or(EngineError::new("engine.invalid_definition"))?;
            let bindings = bound_scoped(state, site.environment, scope, extra)?;
            let mut session = EvalSession::new(100_000).map_err(|e| EngineError::new(e.code))?;
            let evaluated = runbook_expr::render_template(site.template, &bindings, &mut session)
                .map_err(eval_error)?;
            append_reads(reads, evaluated.stats.reads);
            Ok(evaluated.value)
        }
        CheckedValue::List(items) => items
            .iter()
            .map(|v| evaluate_value(state, v, scope, extra, reads))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        CheckedValue::Object(fields) => fields
            .iter()
            .map(|(k, v)| Ok((k.clone(), evaluate_value(state, v, scope, extra, reads)?)))
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(Value::Object),
    }
}
/// F62: the template text at `path` as written, adding each of its
/// expressions' values to `values`, keyed by the expression's trimmed source.
/// A literal string site is its own template and binds nothing.
pub(crate) fn template_parts(
    state: &ExecutionState,
    scope: &[crate::IterationRef],
    path: &str,
    values: &mut Data,
) -> Result<String, EngineError> {
    match state
        .compiled()
        .checked_value(path)
        .ok_or(EngineError::new("engine.invalid_definition"))?
    {
        CheckedValue::Literal(Value::String(text)) => Ok(text.clone()),
        CheckedValue::Template(index) => {
            let site = state
                .compiled()
                .checked_site(*index)
                .ok_or(EngineError::new("engine.invalid_definition"))?;
            let bindings = bound_scoped(state, site.environment, scope, None)?;
            let mut session = EvalSession::new(100_000).map_err(|e| EngineError::new(e.code))?;
            for expression in site.template.expressions() {
                let key = site
                    .source
                    .get(expression.source_span.clone())
                    .ok_or(EngineError::invariant(
                        "template span is outside its source",
                    ))?
                    .trim()
                    .to_owned();
                let value =
                    runbook_expr::evaluate_in(expression.expression, &bindings, &mut session)
                        .map_err(eval_error)?
                        .value;
                values.insert(key, value);
            }
            Ok(site.source.to_owned())
        }
        _ => Err(EngineError::new("expression.type_error")),
    }
}
pub(crate) fn evaluate_path(
    state: &ExecutionState,
    path: &str,
    reads: &mut Vec<ReadRef>,
) -> Result<Value, EngineError> {
    evaluate_scoped_path(state, &[], path, reads)
}
pub(crate) fn evaluate_scoped_path(
    state: &ExecutionState,
    scope: &[crate::IterationRef],
    path: &str,
    reads: &mut Vec<ReadRef>,
) -> Result<Value, EngineError> {
    let value = state
        .compiled()
        .checked_value(path)
        .ok_or(EngineError::new("engine.invalid_definition"))?;
    evaluate_value(state, value, scope, None, reads)
}
pub(crate) fn evaluate_key(
    state: &ExecutionState,
    parent: &crate::StepState,
    alias: &str,
    index: u32,
    item: &Value,
    path: &str,
    reads: &mut Vec<ReadRef>,
) -> Result<Value, EngineError> {
    let value = state
        .compiled()
        .checked_value(path)
        .ok_or(EngineError::new("engine.invalid_definition"))?;
    evaluate_value(
        state,
        value,
        &parent.scope,
        Some((parent, alias, index, item)),
        reads,
    )
}
/// Prepare the one start fact; applying/replaying it never runs an initializer.
pub fn prepare_start(
    state: &ExecutionState,
    context: &StartContext,
    time: i64,
) -> Result<Event, EngineError> {
    if state.sequence != 0 || state.status != ExecutionStatus::Queued {
        return Err(EngineError::new("execution.already_started"));
    }
    admit_context(context)?;
    let mut working = state.fresh();
    working.inputs = resolve(&state.compiled().ir().inputs, &context.inputs)?;
    working.fields = resolve(&state.compiled().ir().fields, &context.fields)?;
    working.project_vars = resolve(state.compiled().project_variables(), &context.project_vars)?;
    working.title = context
        .title
        .clone()
        .unwrap_or_else(|| state.compiled().ir().metadata.title.clone());
    working.tags = context.tags.clone();
    working.url = context.url.clone();
    working.created_by = context.created_by.clone();
    working.at_ms = time;
    working.started_at_ms = Some(time);
    working.data_revision = 1;
    for root in ["inputs", "fields", "vars", "project", "execution", "steps"] {
        working.revisions.insert(root.into(), identity(state, 1));
    }
    let mut reads = vec![];
    for name in state.compiled().initialization_order() {
        let value = evaluate_path(&working, &pointer("/spec/vars", name), &mut reads)?;
        working.vars.insert(name.clone(), value);
    }
    // This verifies the complete required vars record after staged initialization.
    bound(&working, state.compiled().environment())?;
    Ok(Event::ExecutionStarted {
        data: Box::new(InitialData {
            context: StartContext {
                limits: context.limits,
                inputs: working.inputs,
                fields: working.fields,
                project_vars: working.project_vars,
                title: Some(working.title),
                tags: working.tags,
                url: working.url,
                created_by: working.created_by,
                parent: context.parent.clone(),
            },
            vars: working.vars,
            reads,
        }),
        engine_version: crate::ENGINE_VERSION,
    })
}
/// All RHS values use this same immutable state. No partial change escapes errors.
pub fn evaluate_assignments(
    state: &ExecutionState,
    node: &Node,
) -> Result<DataChanged, EngineError> {
    evaluate_assignments_for(state, crate::tree::unique_ready(state, node)?)
}
pub(crate) fn evaluate_assignments_for(
    state: &ExecutionState,
    record: &crate::StepState,
) -> Result<DataChanged, EngineError> {
    let node = state
        .node(&record.step_id)
        .ok_or(EngineError::new("step.not_found"))?;
    let NodeKind::Set(assignments) = &node.kind else {
        return Err(EngineError::new("step.not_assignment"));
    };
    if record.status != StepStatus::Ready {
        return Err(EngineError::new("step.not_ready"));
    }
    let mut changed = DataChanged {
        step_run_id: record
            .id
            .clone()
            .ok_or(EngineError::new("step.not_found"))?,
        base_sequence: state.sequence,
        fields: Data::new(),
        vars: Data::new(),
        reads: vec![],
    };
    for name in assignments.fields.keys() {
        changed.fields.insert(
            name.clone(),
            evaluate_scoped_path(
                state,
                &record.scope,
                &pointer(&format!("{}/set/fields", node.source_path), name),
                &mut changed.reads,
            )?,
        );
    }
    for name in assignments.variables.keys() {
        changed.vars.insert(
            name.clone(),
            evaluate_scoped_path(
                state,
                &record.scope,
                &pointer(&format!("{}/set/vars", node.source_path), name),
                &mut changed.reads,
            )?,
        );
    }
    validate_changes(state, node, &changed)?;
    Ok(changed)
}
/// Render the execution title template against this state, if one is set.
pub fn evaluate_execution_title(state: &ExecutionState) -> Result<Option<String>, EngineError> {
    if state.compiled().ir().execution.title.is_none() {
        return Ok(None);
    }
    let mut reads = vec![];
    let value = evaluate_path(state, "/spec/execution/title", &mut reads)?;
    value
        .as_str()
        .map(|title| Some(title.to_owned()))
        .ok_or(EngineError::new("expression.type_error"))
}
/// Evaluate the concurrency key template once, against this state.
pub fn evaluate_concurrency_key(state: &ExecutionState) -> Result<Option<String>, EngineError> {
    if state.compiled().ir().execution.concurrency.is_none() {
        return Ok(None);
    }
    let mut reads = vec![];
    let value = evaluate_path(state, "/spec/execution/concurrency/key", &mut reads)?;
    value
        .as_str()
        .map(|key| Some(key.to_owned()))
        .ok_or(EngineError::new("expression.type_error"))
}
/// The most a run's outputs may hold together (engine version 5, CF13
/// review round 1), as [`outputs_bytes`] measures them: the values and
/// their reads in the stored form `ExecutionCompleted` keeps (each value
/// tagged by type, never shorter than plain JSON). The rows that carry a
/// run's outputs stay within one Durable Object row by construction: the
/// head keeps the values and `result` (a copy of one of them) in the same
/// stored form, so at most twice this; `ExecutionCompleted`, a parent's
/// `CallCompleted` and its call step keep them once. Two of these and
/// 64 KiB for everything else stay under the 2,000,000-byte row
/// (`runbook-store-do` asserts it when it compiles). 900 KiB is the
/// largest round budget that does.
pub const MAX_OUTPUTS_BYTES: usize = 900 * 1024;
/// The reason a version 5 run that would succeed with outputs past
/// [`MAX_OUTPUTS_BYTES`] fails with instead.
pub const OUTPUTS_TOO_LARGE: &str = "execution.outputs_too_large";
/// The bytes `outputs` holds in the stored form `ExecutionCompleted`
/// records (values tagged by type, and their reads).
pub fn outputs_bytes(outputs: &ExecutionOutputs) -> usize {
    serde_json::to_vec(outputs).map_or(usize::MAX, |bytes| bytes.len())
}
fn budgeted(state: &ExecutionState) -> bool {
    state.engine_version >= crate::version::OUTPUTS_BUDGET
}
/// A version 5 run whose succeeding outputs pass the budget: it fails with
/// [`OUTPUTS_TOO_LARGE`] instead (CF13).
pub(crate) fn outputs_too_large(state: &ExecutionState, outputs: &ExecutionOutputs) -> bool {
    budgeted(state) && outputs_bytes(outputs) > MAX_OUTPUTS_BYTES
}
/// Whether a version 5 run that would now succeed is past the budget: the
/// reducer's check of a recorded [`OUTPUTS_TOO_LARGE`].
pub(crate) fn succeeding_outputs_too_large(state: &ExecutionState) -> Result<bool, EngineError> {
    Ok(budgeted(state) && outputs_too_large(state, &evaluate_outputs(state)?))
}
/// The outputs a run closing as `status` records. A succeeded run must
/// produce every declared output. A failed or cancelled run may close
/// before the values its outputs read exist (a step that never reported),
/// or with values its outputs cannot evaluate: such an output is null, so
/// the run can always close. That is what lets
/// cancelling a parent cancel a child whose outputs read its steps (LANG4).
/// From engine version 5 an output that would take the outputs past
/// [`MAX_OUTPUTS_BYTES`] is null too (in name order), with no reads.
pub(crate) fn evaluate_closing_outputs(
    state: &ExecutionState,
    status: runbook_model::ExecutionStatus,
) -> Result<ExecutionOutputs, EngineError> {
    if status == runbook_model::ExecutionStatus::Succeeded {
        return evaluate_outputs(state);
    }
    let mut out = ExecutionOutputs::default();
    let empty = outputs_bytes(&out);
    // An entry's share of the stored form, a comma included.
    let cost = |name: &String, value: Value, reads: Vec<ReadRef>| {
        let alone = ExecutionOutputs {
            values: Data::from([(name.clone(), value)]),
            reads,
        };
        outputs_bytes(&alone).saturating_sub(empty) + 2
    };
    // Every output is recorded, at least as null: reserve that first.
    let names = state.compiled().ir().outputs.keys();
    let mut used = names.fold(empty, |used, name| {
        used.saturating_add(cost(name, Value::Null, vec![]))
    });
    for name in state.compiled().ir().outputs.keys() {
        let mut reads = vec![];
        let value = match evaluate_path(state, &pointer("/spec/outputs", name), &mut reads) {
            Ok(value) => value,
            // Any evaluation error (a missing value, a type or cost error):
            // the run closes regardless. Broken definitions still stop it.
            Err(EngineError::Rejected { .. }) => Value::Null,
            Err(error) => return Err(error),
        };
        if budgeted(state) {
            let fresh: Vec<ReadRef> = {
                let seen: BTreeSet<_> = out.reads.iter().collect();
                reads
                    .iter()
                    .filter(|r| !seen.contains(r))
                    .cloned()
                    .collect()
            };
            let extra =
                cost(name, value.clone(), fresh).saturating_sub(cost(name, Value::Null, vec![]));
            if used.saturating_add(extra) > MAX_OUTPUTS_BYTES {
                out.values.insert(name.clone(), Value::Null);
                continue;
            }
            used += extra;
        }
        append_reads(&mut out.reads, reads);
        out.values.insert(name.clone(), value);
    }
    Ok(out)
}
pub fn evaluate_outputs(state: &ExecutionState) -> Result<ExecutionOutputs, EngineError> {
    let mut out = ExecutionOutputs::default();
    for name in state.compiled().ir().outputs.keys() {
        out.values.insert(
            name.clone(),
            evaluate_path(state, &pointer("/spec/outputs", name), &mut out.reads)?,
        );
    }
    Ok(out)
}
pub(crate) fn validate_changes(
    state: &ExecutionState,
    node: &Node,
    changed: &DataChanged,
) -> Result<(), EngineError> {
    let NodeKind::Set(assignments) = &node.kind else {
        return Err(EngineError::new("step.not_assignment"));
    };
    if changed.base_sequence != state.sequence {
        return Err(EngineError::new("event.sequence_conflict"));
    }
    if assignments.fields.keys().ne(changed.fields.keys())
        || assignments.variables.keys().ne(changed.vars.keys())
    {
        return Err(EngineError::new("step.invalid_assignment"));
    }
    guard_copy(&changed.fields)?;
    guard_copy(&changed.vars)?;
    let mut admission = CopyAdmission::default();
    admission.data(&changed.fields)?;
    admission.data(&changed.vars)?;
    admit_runtime(state, Some(changed))?;
    admit_runtime(state, None)?;
    // Do not clone the execution record (audit data and unused provenance are
    // irrelevant). Build only bounded expression roots, then overlay the writes.
    let mut values = roots(state)?;
    let (overrides, aliases) = context::populate(state, &mut values, &[], None)?;
    for (root, writes) in [("fields", &changed.fields), ("vars", &changed.vars)] {
        let map = values
            .get_mut(root)
            .and_then(Value::as_object_mut)
            .ok_or(EngineError::new("engine.invalid_definition"))?;
        for (key, value) in writes {
            map.insert(key.clone(), value.clone());
        }
    }
    if !runbook_spec::validate_data(&state.compiled().ir().fields, &values["fields"]).is_empty() {
        return Err(EngineError::new("step.invalid_assignment"));
    }
    let origin = Origin {
        path: String::new(),
        record: identity(state, state.data_revision.max(1)),
    };
    bindings::build_scoped(
        state.compiled().environment(),
        &values,
        &state.revisions,
        &overrides,
        &aliases,
        &origin,
        at_ms(state.at_ms),
    )?;
    Ok(())
}
pub(crate) fn install_start(
    state: &mut ExecutionState,
    data: &InitialData,
    seq: u64,
    time: i64,
) -> Result<(), EngineError> {
    admit_context(&data.context)?;
    guard_copy(&data.vars)?;
    CopyAdmission::default().data(&data.vars)?;
    state.inputs = resolve(&state.compiled().ir().inputs, &data.context.inputs)?;
    state.fields = resolve(&state.compiled().ir().fields, &data.context.fields)?;
    state.project_vars = resolve(
        state.compiled().project_variables(),
        &data.context.project_vars,
    )?;
    if state.inputs != data.context.inputs
        || state.fields != data.context.fields
        || state.project_vars != data.context.project_vars
    {
        return Err(EngineError::new("execution.invalid_data"));
    }
    guard_copy(&data.vars)?;
    state.vars = data.vars.clone();
    state.title = data
        .context
        .title
        .clone()
        .ok_or(EngineError::new("execution.invalid_data"))?;
    state.tags = data.context.tags.clone();
    state.url = data.context.url.clone();
    state.created_by = data.context.created_by.clone();
    state.parent = data.context.parent.clone();
    state.limits = data.context.limits;
    state.started_at_ms = Some(time);
    state.at_ms = time;
    state.data_revision = seq;
    state.initialization_reads = data.reads.clone();
    for root in ["inputs", "fields", "vars", "project", "execution", "steps"] {
        state.revisions.insert(root.into(), identity(state, seq));
    }
    let record = identity(state, seq);
    for (root, data) in [("fields", &state.fields), ("vars", &state.vars)] {
        for name in data.keys() {
            state
                .revisions
                .insert(format!("{root}.{name}"), record.clone());
        }
    }
    bound(state, state.compiled().environment())?;
    Ok(())
}
pub(crate) fn note_step_revision(state: &mut ExecutionState, index: usize, seq: u64) {
    let mut record = identity(state, seq);
    record.step_run_id = state.steps[index].id.as_ref().map(ToString::to_string);
    state
        .revisions
        .insert(format!("steps.{}", state.steps[index].step_id), record);
    state.steps[index].revision = seq;
    state.data_revision = seq;
    state.revisions.insert("steps".into(), identity(state, seq));
}
pub(crate) fn apply_changes(
    state: &mut ExecutionState,
    index: usize,
    changed: &DataChanged,
    seq: u64,
    time: i64,
) {
    for (root, writes) in [("fields", &changed.fields), ("vars", &changed.vars)] {
        for name in writes.keys() {
            state
                .revisions
                .insert(format!("{root}.{name}"), identity(state, seq));
        }
        if !writes.is_empty() {
            state.revisions.insert(root.into(), identity(state, seq));
        }
    }
    state.fields.extend(changed.fields.clone());
    state.vars.extend(changed.vars.clone());
    state.steps[index].status = StepStatus::Succeeded;
    state.steps[index].finished_at_ms = Some(time);
    append_reads(&mut state.steps[index].reads, changed.reads.iter().cloned());
    note_step_revision(state, index, seq);
}
pub(crate) fn admit_foreach(items: &Value, keys: &[String]) -> Result<(), EngineError> {
    let mut admission = CopyAdmission::default();
    admission.value(items, 0)?;
    for key in keys {
        admission.text(key)?;
    }
    Ok(())
}
mod shape;
pub(crate) fn validate_foreach_items(
    state: &ExecutionState,
    node: &Node,
    items: &Value,
) -> Result<(), EngineError> {
    let path = format!("{}/foreach/items", node.source_path);
    let Some(CheckedValue::Template(index)) = state.compiled().checked_value(&path) else {
        return Err(EngineError::new("engine.invalid_definition"));
    };
    let site = state
        .compiled()
        .checked_site(*index)
        .ok_or(EngineError::new("engine.invalid_definition"))?;
    let expression = site
        .template
        .expressions()
        .next()
        .ok_or(EngineError::new("engine.invalid_definition"))?;
    if !shape::result(
        items,
        &expression.expression.result_type(),
        site.environment,
    ) {
        return Err(EngineError::new("foreach.invalid_snapshot"));
    }
    Ok(())
}

/// Metadata is persisted as a fact; expressions read the new field revision.
pub(crate) fn apply_metadata(
    state: &mut ExecutionState,
    title: &Option<String>,
    tags: &Option<Vec<String>>,
    fields: &Data,
    seq: u64,
) -> Result<(), EngineError> {
    if state.status == ExecutionStatus::Queued {
        return Err(EngineError::new("execution.not_started"));
    }
    if matches!(
        state.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    ) {
        return Err(EngineError::new("execution.closed"));
    }
    guard_copy(fields)?;
    let mut merged = state.fields.clone();
    merged.extend(fields.clone());
    if !runbook_spec::validate_data(&state.compiled().ir().fields, &object(&merged)).is_empty() {
        return Err(EngineError::new("data.invalid_fields"));
    }
    state.fields = merged;
    for name in fields.keys() {
        state
            .revisions
            .insert(format!("fields.{name}"), identity(state, seq));
    }
    if !fields.is_empty() {
        state
            .revisions
            .insert("fields".into(), identity(state, seq));
    }
    if let Some(title) = title {
        state.title = title.clone();
    }
    if let Some(tags) = tags {
        state.tags = tags.clone();
    }
    state
        .revisions
        .insert("execution".into(), identity(state, seq));
    state.data_revision = seq;
    bound(state, state.compiled().environment())?;
    Ok(())
}
