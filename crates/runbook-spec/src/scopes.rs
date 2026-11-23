//! Private scope/value checkpoint. A5e will seal these tables with normalized IR.
// No public executable or snapshot API: the normalized input is still separate.
#![allow(dead_code)]

use crate::{DataSchema, DataType, Diagnostic, SourceSpan, ir::*};
use runbook_expr::{
    CheckedTemplate, FieldDecl, ResultType, SchemaDef, TemplateMode, TypeDef, TypeEnv, TypeId,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[path = "scopes/corpus.rs"]
pub(crate) mod corpus;
#[path = "scopes/sites.rs"]
mod sites;
#[path = "scopes/types.rs"]
mod types;
#[path = "scopes/values.rs"]
mod values;
use sites::{Expected, SourceValue};
use types::Arena;
pub use values::CheckedValue;
use values::{Site, ValueChecker};
#[cfg(test)]
#[path = "scopes/tests.rs"]
mod tests;

type Result<T> = std::result::Result<T, Diagnostic>;
type Spans = BTreeMap<String, SourceSpan>;

fn error(code: &str, path: &str) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: None,
        path: path.into(),
        line: 0,
        column: 0,
    }
}
fn expr_error(e: runbook_expr::ExprDiagnostic, path: &str) -> Diagnostic {
    Diagnostic {
        code: e.code.into(),
        message: Some(e.message),
        ..error(e.code, path)
    }
}
fn pointer(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}
fn field(ty: TypeId, required: bool) -> FieldDecl {
    FieldDecl {
        ty,
        nullable: false,
        required,
    }
}

/// One root table supports lexical runtime binding handoff. It duplicates roots
/// (not records) in TypeEnv; aggregate accounting explicitly charges this copy.
#[derive(Debug)]
struct Environment {
    handle: Arc<TypeEnv>,
    roots: BTreeMap<String, FieldDecl>,
    types: usize,
}
#[derive(Debug)]
pub(crate) struct CheckedScopes {
    environments: Vec<Environment>,
    scopes: Vec<LexicalScope>,
    sites: Vec<Site>,
    values: BTreeMap<String, CheckedValue>,
    initialization_order: Vec<String>,
    resources: Resources,
    discovery: sites::Discovery,
}
#[derive(Debug)]
struct LexicalScope {
    environment: usize,
    parent: Option<usize>,
    region: Region,
}
#[derive(Debug)]
enum Region {
    Base,
    Initialization,
    Foreach { path: String, alias: String },
    Loop { path: String },
    Report,
}
#[derive(Default, Debug)]
struct Resources {
    type_slots: usize,
    field_slots: usize,
}
impl Resources {
    fn admit(&mut self, types: usize, fields: usize, path: &str) -> Result<()> {
        if self.type_slots + types > 65_536 || self.field_slots + fields > 262_144 {
            return Err(error("spec.compile_limit", path));
        }
        self.type_slots += types;
        self.field_slots += fields;
        Ok(())
    }
}

fn base(
    ir: &NormalizedRunbook,
    project: &DataSchema,
) -> Result<(Arena, BTreeMap<String, FieldDecl>)> {
    let mut arena = Arena::default();
    let inputs = arena.data(&ir.inputs, "/spec/inputs", false)?;
    let fields = arena.data(&ir.fields, "/spec/fields", false)?;
    let vars = arena.record(BTreeMap::new(), "/spec/vars")?;
    let project_vars = arena.data(project, "/project/vars", false)?;
    let project = arena.record(
        BTreeMap::from([("vars".into(), field(project_vars, true))]),
        "/project",
    )?;
    let mut nodes = BTreeMap::new();
    node_types(&mut arena, &ir.steps, &mut nodes)?;
    node_types(&mut arena, &ir.finally, &mut nodes)?;
    let steps = arena.record(nodes, "/spec/steps")?;
    let string = arena.intern(TypeDef::String, "/spec/execution")?;
    let timestamp = arena.intern(TypeDef::Timestamp, "/spec/execution")?;
    let tags = arena.intern(TypeDef::List(string), "/spec/execution/tags")?;
    let mut execution = BTreeMap::new();
    for name in ["id", "title", "url", "web_url", "created_by"] {
        execution.insert(name.into(), field(string, true));
    }
    execution.insert("tags".into(), field(tags, true));
    // Queued executions exist before admission sets started_at. Absence is not null.
    execution.insert("started_at".into(), field(timestamp, false));
    let execution = arena.record(execution, "/spec/execution")?;
    let roots = BTreeMap::from([
        ("inputs".into(), field(inputs, true)),
        ("fields".into(), field(fields, true)),
        ("vars".into(), field(vars, true)),
        ("project".into(), field(project, true)),
        ("steps".into(), field(steps, true)),
        ("execution".into(), field(execution, true)),
    ]);
    Ok((arena, roots))
}

fn child_groups(node: &Node) -> Vec<&[Node]> {
    match &node.kind {
        NodeKind::Group(g) => vec![&g.children, &g.catch, &g.finally],
        NodeKind::Parallel { children, .. } => vec![children],
        NodeKind::Foreach(f) => vec![&f.children],
        NodeKind::Loop(l) => vec![&l.children],
        NodeKind::Switch(s) => s
            .cases
            .iter()
            .map(|c| c.children.as_slice())
            .chain(std::iter::once(s.default.as_slice()))
            .collect(),
        _ => vec![],
    }
}
fn node_types(
    arena: &mut Arena,
    nodes: &[Node],
    all: &mut BTreeMap<String, FieldDecl>,
) -> Result<()> {
    for node in nodes {
        let mut descendants = BTreeMap::new();
        for children in child_groups(node) {
            node_types(arena, children, &mut descendants)?;
        }
        let p = &node.source_path;
        let string = arena.intern(TypeDef::String, p)?;
        let boolean = arena.intern(TypeDef::Bool, p)?;
        let int = arena.intern(TypeDef::Int, p)?;
        let time = arena.intern(TypeDef::Timestamp, p)?;
        let outputs = match &node.kind {
            NodeKind::Task(t) => arena.data(&t.outputs, &pointer(p, "outputs"), true)?,
            NodeKind::Call(c) => {
                arena.data(&c.outputs, &pointer(&pointer(p, "call"), "outputs"), true)?
            }
            // LANG9: a signal wait's checked payload is its outputs.
            NodeKind::Wait(w) => match &w.outputs {
                Some(outputs) => {
                    arena.data(outputs, &pointer(&pointer(p, "wait"), "outputs"), true)?
                }
                None => arena.record(BTreeMap::new(), p)?,
            },
            NodeKind::Approval(_) => arena.record(
                ["decision", "comment", "decided_by"]
                    .into_iter()
                    .map(|n| (n.into(), field(string, false)))
                    .collect(),
                &pointer(p, "outputs"),
            )?,
            _ => arena.record(BTreeMap::new(), p)?,
        };
        let mut record = BTreeMap::from([
            ("status".into(), field(string, true)),
            ("attempts".into(), field(int, true)),
            ("amended".into(), field(boolean, true)),
            ("stale".into(), field(boolean, true)),
            ("outputs".into(), field(outputs, false)),
        ]);
        for (name, ty) in [
            ("verdict", string),
            ("actor", string),
            ("started_at", time),
            ("finished_at", time),
        ] {
            record.insert(
                name.into(),
                FieldDecl {
                    ty,
                    nullable: true,
                    required: true,
                },
            );
        }
        if matches!(node.kind, NodeKind::Foreach(_) | NodeKind::Loop(_)) {
            let iteration = arena.record(descendants.clone(), p)?;
            let collection = arena.intern(TypeDef::List(iteration), p)?;
            let name = if matches!(node.kind, NodeKind::Foreach(_)) {
                "items"
            } else {
                "iterations"
            };
            record.insert(name.into(), field(collection, false));
            if matches!(node.kind, NodeKind::Loop(_)) {
                for name in ["satisfied", "exhausted"] {
                    record.insert(name.into(), field(boolean, true));
                }
            }
        }
        let ty = arena.record(record, p)?;
        if all.len() + descendants.len() + 1 > 256 {
            return Err(error("expression.schema_limit", p));
        }
        all.extend(descendants);
        all.insert(node.id.clone(), field(ty, false));
    }
    Ok(())
}

#[cfg(test)]
fn check_for_test(ir: &NormalizedRunbook, source: &crate::Spec) -> Result<CheckedScopes> {
    check(
        ir,
        &source.source_spans,
        &crate::parse_declarations(&serde_json::json!({})).unwrap(),
    )
}

pub(crate) fn check(
    ir: &NormalizedRunbook,
    spans: &Spans,
    project: &DataSchema,
) -> Result<CheckedScopes> {
    check_inner(ir, project).map_err(|e| locate(e, spans))
}
/// Point `e` at the nearest source span of its path or a parent's.
pub(crate) fn locate(mut e: Diagnostic, spans: &Spans) -> Diagnostic {
    {
        let mut p = e.path.as_str();
        loop {
            if let Some(span) = spans.get(p) {
                e.line = span.line;
                e.column = span.column;
                break;
            }
            if let Some((parent, _)) = p.rsplit_once('/') {
                p = parent;
            } else {
                break;
            }
        }
    }
    e
}
/// The most outputs a runbook may declare.
pub(crate) const MAX_OUTPUTS: usize = 256;
/// At most [`MAX_OUTPUTS`] outputs, with or without a `report` (CF13 review
/// round 1: the engine's budget on all of a run's outputs reserves an entry
/// for each). Publish and a new run's start apply it; `restore` applies it
/// only with a `report`, as before CF13, so a version published earlier with
/// more outputs and no report still restores and its runs still load
/// (CF13b).
pub(crate) fn outputs_limit(ir: &NormalizedRunbook) -> Result<()> {
    if ir.outputs.len() > MAX_OUTPUTS {
        return Err(error("expression.schema_limit", "/spec/outputs"));
    }
    Ok(())
}
/// LANG9b: the type environment of a webhook trigger's expressions: only
/// `payload`, typed by the trigger's declared schema (none: no roots). Its
/// own arena, so triggers never shift the run's type budget.
pub fn trigger_env(payload: Option<&DataSchema>, path: &str) -> Result<TypeEnv> {
    let mut arena = Arena::default();
    let mut roots = BTreeMap::new();
    if let Some(schema) = payload {
        let ty = arena.data(schema, &pointer(path, "payload"), false)?;
        roots.insert("payload".to_owned(), field(ty, true));
    }
    arena.environment(&roots, path)
}
/// LANG9b: each webhook trigger's `when` is a whole-value boolean, `dedupe`
/// a whole-value scalar, and string `inputs` values check as templates.
fn check_triggers(ir: &NormalizedRunbook) -> Result<()> {
    for (i, trigger) in ir.triggers.webhook.iter().enumerate() {
        let path = format!("/spec/triggers/webhook/{i}");
        let env = trigger_env(trigger.payload.as_ref(), &path)?;
        let check = |source: &str, at: &str| {
            runbook_expr::check_template(source, &env).map_err(|e| expr_error(e, at))
        };
        let whole = |template: &CheckedTemplate| -> Option<ResultType> {
            if template.mode() != TemplateMode::WholeValue {
                return None;
            }
            template
                .expressions()
                .next()
                .map(|e| e.expression.result_type())
        };
        if let Some(when) = &trigger.when {
            let at = pointer(&path, "when");
            let template = check(when, &at)?;
            if !matches!(whole(&template), Some(ResultType::Bool)) {
                return Err(error("spec.trigger_when_type", &at));
            }
        }
        if let Some(dedupe) = &trigger.dedupe {
            let at = pointer(&path, "dedupe");
            let template = check(dedupe, &at)?;
            if matches!(
                whole(&template),
                Some(ResultType::List(_) | ResultType::Record { .. } | ResultType::Null)
            ) {
                return Err(error("spec.invalid_value", &at));
            }
        }
        for (name, value) in &trigger.inputs {
            if let Some(source) = value.as_str() {
                check(source, &pointer(&pointer(&path, "inputs"), name))?;
            }
        }
    }
    Ok(())
}
fn check_inner(ir: &NormalizedRunbook, project: &DataSchema) -> Result<CheckedScopes> {
    check_triggers(ir)?;
    // Enforce the existing complete-record limit before DAG expansion or
    // inferred report-field allocation; neither source-size guard bounds their
    // intermediate cross-products tightly enough on its own.
    if ir.variables.len() > 256 {
        return Err(error("expression.schema_limit", "/spec/vars"));
    }
    // A `report` reads the outputs as one closed record, held to the
    // record limit before it is built. Without a report the publish-time
    // cap (`outputs_limit`) holds them; restore does not apply it (CF13b).
    if ir.report.is_some() {
        outputs_limit(ir)?;
    }
    // This borrowed pass reserves every retained segment before any checker call.
    let discovery = sites::discover(ir)?;
    let (mut arena, mut roots) = base(ir, project)?;
    let order = variable_order(ir)?;
    let mut vars = BTreeMap::new();
    for name in &order {
        let p = pointer("/spec/vars", name);
        types::validate_name(name, &p)?;
        // At most one scratch environment and one candidate; prior programs and
        // the scratch resolver die at the end of this iteration.
        let before = arena.types.len();
        let ty = arena.record(vars.clone(), "/spec/vars")?;
        let mut scratch_roots = roots.clone();
        scratch_roots.insert("vars".into(), field(ty, true));
        let env = Arc::new(arena.environment(&scratch_roots, &p)?);
        arena.truncate(before);
        #[cfg(test)]
        tests::record_scratch(&env);
        let (_, ty) = ValueChecker {
            arena: &mut arena,
            env: &env,
            sites: &mut vec![],
            scope: 0,
            retain: false,
        }
        .value(
            SourceValue::Json(&ir.variables[name]),
            &p,
            Expected::Any,
            None,
            None,
            true,
        )?;
        vars.insert(name.clone(), ty.unwrap());
    }
    let var_type = arena.record(vars.clone(), "/spec/vars")?;
    roots.insert("vars".into(), field(var_type, true));
    let mut out = CheckedScopes {
        environments: vec![],
        scopes: vec![],
        sites: vec![],
        values: BTreeMap::new(),
        initialization_order: order,
        resources: Resources::default(),
        discovery,
    };
    let base_env = out.environment(&mut arena, &roots, "/spec")?;
    out.scopes.push(LexicalScope {
        environment: base_env,
        parent: None,
        region: Region::Base,
    });
    // Initializers run in dependency order. Their binding snapshot may omit vars
    // not initialized yet; normal execution still requires the complete record.
    // One shared closed environment avoids per-variable resolver expansion.
    let mut init_arena = out.arena(0);
    let init_scope = if ir.variables.values().any(values::contains_templates) {
        let mut init_roots = roots.clone();
        let optional_vars = vars
            .iter()
            .map(|(name, d)| {
                let mut optional = d.clone();
                optional.required = false;
                (name.clone(), optional)
            })
            .collect();
        let init_type = init_arena.record(optional_vars, "/spec/vars")?;
        init_roots.insert("vars".into(), field(init_type, true));
        let init_environment = out.environment(&mut init_arena, &init_roots, "/spec/vars")?;
        let scope = out.scopes.len();
        out.scopes.push(LexicalScope {
            environment: init_environment,
            parent: Some(0),
            region: Region::Initialization,
        });
        scope
    } else {
        0
    };
    for (name, value) in &ir.variables {
        let path = pointer("/spec/vars", name);
        out.value(
            &mut init_arena,
            init_scope,
            &path,
            SourceValue::Json(value),
            Expected::Variable,
            Some(&vars[name]),
            None,
            false,
        )?;
    }
    drop(init_arena);
    sites::global_sites(ir, |p, v, e| {
        out.value(&mut arena, 0, &p, v, e, None, None, false)
            .map(|_| ())
    })?;
    drop(arena);
    out.nodes(ir, &ir.steps, 0, &vars)?;
    out.nodes(ir, &ir.finally, 0, &vars)?;
    let mut arena = out.arena(0);
    let mut output_fields = BTreeMap::new();
    for (name, value) in &ir.outputs {
        let p = pointer("/spec/outputs", name);
        // Only report needs an inferred closed outputs record. Opaque output
        // data otherwise remains arbitrary JSON, including null/empty lists.
        let ty = out.value(
            &mut arena,
            0,
            &p,
            SourceValue::Json(value),
            Expected::Any,
            None,
            None,
            ir.report.is_some(),
        )?;
        if let Some(ty) = ty {
            output_fields.insert(name.clone(), ty);
        }
    }
    if let Some(report) = &ir.report {
        let outputs = arena.record(output_fields, "/spec/outputs")?;
        roots.insert("outputs".into(), field(outputs, true));
        let env = out.environment(&mut arena, &roots, "/spec/report")?;
        let scope = out.scopes.len();
        out.scopes.push(LexicalScope {
            environment: env,
            parent: Some(0),
            region: Region::Report,
        });
        out.value(
            &mut arena,
            scope,
            "/spec/report",
            SourceValue::Text(report),
            Expected::String,
            None,
            None,
            false,
        )?;
    }
    Ok(out)
}
fn variable_order(ir: &NormalizedRunbook) -> Result<Vec<String>> {
    #[cfg(test)]
    tests::DAG_BUILDS.with(|v| v.set(v.get() + 1));
    let names: BTreeSet<_> = ir.variables.keys().cloned().collect();
    let mut edges = BTreeMap::new();
    for (name, value) in &ir.variables {
        let mut dependencies = BTreeSet::new();
        sites::strings(
            SourceValue::Json(value),
            &pointer("/spec/vars", name),
            &mut |p, s| {
                if !s.contains("${{") {
                    return Ok(());
                }
                let summary =
                    runbook_expr::summarize_template_syntax(s).map_err(|e| expr_error(e, p))?;
                for expression in summary.expressions {
                    for dependency in expression.dependencies {
                        if dependency.root == "vars" {
                            match dependency.member {
                                Some(member) => {
                                    if !names.contains(&member) {
                                        return Err(error("spec.unknown_variable", p));
                                    }
                                    dependencies.insert(member);
                                }
                                None => dependencies.extend(names.iter().cloned()),
                            }
                        }
                    }
                }
                Ok(())
            },
        )?;
        edges.insert(name.clone(), dependencies);
    }
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    while order.len() < edges.len() {
        let next = edges
            .iter()
            .find(|(name, deps)| !done.contains(*name) && deps.is_subset(&done))
            .map(|(name, _)| name.clone());
        let Some(next) = next else {
            let name = edges.keys().find(|n| !done.contains(*n)).unwrap();
            return Err(error("spec.variable_cycle", &pointer("/spec/vars", name)));
        };
        done.insert(next.clone());
        order.push(next);
    }
    Ok(order)
}
impl CheckedScopes {
    fn arena(&self, scope: usize) -> Arena {
        let env = &self.environments[self.scopes[scope].environment];
        Arena::from_types(
            (0..env.types)
                .map(|i| {
                    env.handle
                        .type_definition(TypeId(i as u32))
                        .unwrap()
                        .clone()
                })
                .collect(),
        )
    }
    fn environment(
        &mut self,
        arena: &mut Arena,
        roots: &BTreeMap<String, FieldDecl>,
        path: &str,
    ) -> Result<usize> {
        for (i, env) in self.environments.iter().enumerate() {
            if env.roots == *roots
                && env.types == arena.types.len()
                && arena
                    .types
                    .iter()
                    .enumerate()
                    .all(|(i, t)| env.handle.type_definition(TypeId(i as u32)) == Some(t))
            {
                return Ok(i);
            }
        }
        let records: usize = arena
            .types
            .iter()
            .map(|t| match t {
                TypeDef::Record(fields) => fields.len(),
                _ => 0,
            })
            .sum();
        // TypeEnv schema + one closed resolver's record fields + compact root
        // handoff copy. Roots are not duplicated in the resolver's record map.
        self.resources
            .admit(arena.types.len(), records * 2 + roots.len() * 2, path)?;
        let env = arena.environment(roots, path)?;
        let id = self.environments.len();
        self.environments.push(Environment {
            handle: Arc::new(env),
            roots: roots.clone(),
            types: arena.types.len(),
        });
        Ok(id)
    }
    #[allow(clippy::too_many_arguments)]
    fn value(
        &mut self,
        arena: &mut Arena,
        scope: usize,
        path: &str,
        value: SourceValue<'_>,
        expected: Expected,
        destination: Option<&FieldDecl>,
        schema: Option<&DataSchema>,
        infer: bool,
    ) -> Result<Option<FieldDecl>> {
        let env = &self.environments[self.scopes[scope].environment].handle;
        let (value, ty) = ValueChecker {
            arena,
            env,
            sites: &mut self.sites,
            scope,
            retain: true,
        }
        .value(value, path, expected, destination, schema, infer)?;
        self.values.insert(path.into(), value);
        Ok(ty)
    }
    fn nodes(
        &mut self,
        ir: &NormalizedRunbook,
        nodes: &[Node],
        scope: usize,
        vars: &BTreeMap<String, FieldDecl>,
    ) -> Result<()> {
        for node in nodes {
            let mut candidate = self.arena(scope);
            let roots = self.environments[self.scopes[scope].environment]
                .roots
                .clone();
            let mut items = None;
            sites::node_sites(node, |p, v, e| {
                let name = p.rsplit('/').next().unwrap();
                let schema = if e == Expected::Field {
                    match &ir.fields.kind {
                        DataType::Object { properties } => Some(
                            properties
                                .get(name)
                                .ok_or_else(|| error("spec.unknown_field", &p))?,
                        ),
                        _ => unreachable!(),
                    }
                } else {
                    None
                };
                let destination = if e == Expected::Variable {
                    Some(
                        vars.get(name)
                            .ok_or_else(|| error("spec.unknown_variable", &p))?
                            .clone(),
                    )
                } else if let Some(s) = schema {
                    Some(field(candidate.data(s, &p, false)?, true))
                } else {
                    None
                };
                let ty = self.value(
                    &mut candidate,
                    scope,
                    &p,
                    v,
                    e,
                    destination.as_ref(),
                    schema,
                    false,
                )?;
                if e == Expected::List {
                    items = ty;
                }
                Ok(())
            })?;
            match &node.kind {
                NodeKind::Foreach(f) => {
                    let p = pointer(&node.source_path, "foreach");
                    if roots.contains_key(&f.alias)
                        || matches!(f.alias.as_str(), "outputs" | "index" | "iteration")
                    {
                        return Err(error("spec.alias_collision", &pointer(&p, "as")));
                    }
                    types::validate_name(&f.alias, &pointer(&p, "as"))?;
                    let TypeDef::List(element) = candidate.types[items.unwrap().ty.0 as usize]
                    else {
                        unreachable!()
                    };
                    let int = candidate.intern(TypeDef::Int, &p)?;
                    let mut child_roots = roots.clone();
                    child_roots.insert(f.alias.clone(), field(element, true));
                    child_roots.insert("index".into(), field(int, true));
                    let environment = self.environment(&mut candidate, &child_roots, &p)?;
                    let child_scope = self.scopes.len();
                    self.scopes.push(LexicalScope {
                        environment,
                        parent: Some(scope),
                        region: Region::Foreach {
                            path: p.clone(),
                            alias: f.alias.clone(),
                        },
                    });
                    if let Some(key) = &f.key {
                        self.value(
                            &mut candidate,
                            child_scope,
                            &pointer(&p, "key"),
                            SourceValue::Text(key),
                            Expected::String,
                            None,
                            None,
                            false,
                        )?;
                    }
                    drop(candidate);
                    self.nodes(ir, &f.children, child_scope, vars)?;
                }
                NodeKind::Loop(l) => {
                    let p = pointer(&node.source_path, "loop");
                    let int = candidate.intern(TypeDef::Int, &p)?;
                    let mut child_roots = roots.clone();
                    child_roots.insert("iteration".into(), field(int, true));
                    let environment = self.environment(&mut candidate, &child_roots, &p)?;
                    let child_scope = self.scopes.len();
                    self.scopes.push(LexicalScope {
                        environment,
                        parent: Some(scope),
                        region: Region::Loop { path: p.clone() },
                    });
                    self.value(
                        &mut candidate,
                        child_scope,
                        &pointer(&p, l.condition().key()),
                        SourceValue::Text(l.condition().source()),
                        Expected::Bool,
                        None,
                        None,
                        false,
                    )?;
                    drop(candidate);
                    self.nodes(ir, &l.children, child_scope, vars)?;
                }
                _ => {
                    drop(candidate);
                    for children in child_groups(node) {
                        self.nodes(ir, children, scope, vars)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Read-only checked template and its lexical environment. Programs are created
/// only by the checker; snapshot restoration rebuilds them from typed semantics.
#[derive(Clone, Copy, Debug)]
pub struct CheckedSite<'a> {
    pub path: &'a str,
    /// The template text as written; expression spans index into it.
    pub source: &'a str,
    pub template: &'a CheckedTemplate,
    pub environment: &'a TypeEnv,
}
impl CheckedScopes {
    pub(crate) fn value_at(&self, path: &str) -> Option<&CheckedValue> {
        self.values.get(path)
    }
    pub(crate) fn base_environment(&self) -> &TypeEnv {
        &self.environments[self.scopes[0].environment].handle
    }
    pub(crate) fn site(&self, index: usize) -> Option<CheckedSite<'_>> {
        self.sites.get(index).map(|s| CheckedSite {
            path: &s.path,
            source: &s.source,
            template: &s.template,
            environment: &self.environments[self.scopes[s.scope].environment].handle,
        })
    }
    pub(crate) fn sites(&self) -> impl Iterator<Item = CheckedSite<'_>> {
        self.sites.iter().map(|s| CheckedSite {
            path: &s.path,
            source: &s.source,
            template: &s.template,
            environment: &self.environments[self.scopes[s.scope].environment].handle,
        })
    }
    pub(crate) fn initialization_order(&self) -> &[String] {
        &self.initialization_order
    }
}
