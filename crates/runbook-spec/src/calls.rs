//! LANG4 `call`: resolving child runbook versions at compile time.
//!
//! A `call` names a runbook in the same project by slug and optionally a
//! published version number. The compile context carries what the host
//! read from its catalog for each reference ([`CallResolution`], keyed by
//! [`call_key`]); compilation pins it into the IR ([`CallTarget`]), so a
//! restored snapshot needs no catalog. The checks here (PRD §5, §17):
//!
//! - the reference resolves (`spec.call_unresolved`);
//! - no cycles: the runbook being compiled is not reachable through its own
//!   calls (`spec.call_cycle`). Published versions are immutable and pin
//!   their own children, so checking the compiled runbook is enough;
//! - depth: a run, its child, grandchild and great-grandchild at most
//!   (`spec.call_depth_exceeded`, [`MAX_CALL_DEPTH`]);
//! - `with` names only inputs the child declares (`spec.call_unknown_input`)
//!   and supplies every input it requires (`spec.call_missing_input`).
//!   Values are checked against the child's schema when the child starts.
use crate::{
    Diagnostic,
    ir::{
        CallResolution, CallTarget, MAX_CALL_DEPTH, Node, NodeKind, NormalizedRunbook, OutputKind,
    },
};
use std::collections::{BTreeMap, BTreeSet};

/// The compile-context key of a call reference: the slug, then `@n` for a
/// pinned number.
pub fn call_key(runbook: &str, version: Option<u32>) -> String {
    match version {
        Some(n) => format!("{runbook}@{n}"),
        None => runbook.to_owned(),
    }
}

/// Every call reference in `ir` (slug and pinned number), sorted and
/// unique: what a host resolves into the compile context.
pub fn references(ir: &NormalizedRunbook) -> Vec<(String, Option<u32>)> {
    let mut out = BTreeSet::new();
    each(&ir.steps, &mut |call| {
        out.insert((call.runbook.clone(), call.version));
    });
    each(&ir.finally, &mut |call| {
        out.insert((call.runbook.clone(), call.version));
    });
    out.into_iter().collect()
}

/// What a caller of a compiled runbook needs to know about it.
pub fn resolution(
    ir: &NormalizedRunbook,
    runbook_id: &str,
    version_id: &str,
    version: u32,
) -> CallResolution {
    let mut depth = 0;
    let mut reaches = BTreeSet::new();
    let mut destructive = false;
    let mut visit = |call: &crate::ir::Call| {
        if let Some(t) = &call.target {
            depth = depth.max(t.depth);
            reaches.extend(t.reaches.iter().cloned());
            destructive |= t.destructive;
        }
    };
    each(&ir.steps, &mut visit);
    each(&ir.finally, &mut visit);
    destructive |= has_destructive_task(&ir.steps) || has_destructive_task(&ir.finally);
    let input_schemas = match &ir.inputs.kind {
        crate::DataType::Object { properties } => properties.clone(),
        _ => BTreeMap::new(),
    };
    let inputs = match &ir.inputs.kind {
        crate::DataType::Object { properties } => properties
            .iter()
            .map(|(name, s)| {
                (
                    name.clone(),
                    s.required && matches!(s.default, crate::DefaultValue::Missing),
                )
            })
            .collect(),
        _ => BTreeMap::new(),
    };
    CallResolution {
        runbook_id: runbook_id.into(),
        version_id: version_id.into(),
        version,
        depth,
        reaches: reaches.into_iter().collect(),
        destructive,
        inputs,
        input_schemas,
        outputs: ir
            .outputs
            .keys()
            .map(|k| (k.clone(), OutputKind::Any))
            .collect(),
    }
}

/// The shape of a checked output value.
pub(crate) fn output_kind(
    compiled: &crate::CompiledRunbook,
    value: &crate::CheckedValue,
) -> OutputKind {
    use crate::CheckedValue as C;
    use runbook_expr::ResultType as R;
    fn of_result(result: &R) -> OutputKind {
        match result {
            R::Null => OutputKind::Any,
            R::Bool => OutputKind::Boolean,
            R::Int => OutputKind::Integer,
            R::Double => OutputKind::Number,
            R::String => OutputKind::String,
            R::Timestamp => OutputKind::Timestamp,
            R::List(_) => OutputKind::List,
            R::Nullable(inner) => of_result(inner),
            R::Record { .. } => OutputKind::Object,
        }
    }
    match value {
        C::Literal(v) => match v {
            serde_json::Value::Null => OutputKind::Any,
            serde_json::Value::Bool(_) => OutputKind::Boolean,
            serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => OutputKind::Integer,
            serde_json::Value::Number(_) => OutputKind::Number,
            serde_json::Value::String(_) => OutputKind::String,
            serde_json::Value::Array(_) => OutputKind::List,
            serde_json::Value::Object(_) => OutputKind::Object,
        },
        C::List(_) => OutputKind::List,
        C::Object(_) => OutputKind::Object,
        C::Template(index) => match compiled.checked_site(*index) {
            Some(site) if site.template.mode() == runbook_expr::TemplateMode::Text => {
                OutputKind::String
            }
            Some(site) => site
                .template
                .expressions()
                .next()
                .map_or(OutputKind::Any, |e| of_result(&e.expression.result_type())),
            None => OutputKind::Any,
        },
    }
}

/// Whether a child output of shape `kind` can satisfy `schema`.
fn compatible(kind: OutputKind, schema: &crate::DataSchema) -> bool {
    use crate::DataType as T;
    match kind {
        OutputKind::Any => true,
        OutputKind::Boolean => matches!(schema.kind, T::Boolean),
        OutputKind::Integer => matches!(schema.kind, T::Integer | T::Number),
        // A number may be whole; the runtime checks the value (re-check 2).
        OutputKind::Number => matches!(schema.kind, T::Number | T::Integer),
        OutputKind::String => matches!(
            schema.kind,
            T::String
                | T::Text
                | T::Enum { .. }
                | T::Date
                | T::DateTime
                | T::Duration
                | T::Url
                | T::Email
                | T::User
        ),
        OutputKind::Timestamp => matches!(schema.kind, T::DateTime | T::String | T::Text),
        OutputKind::List => matches!(schema.kind, T::List { .. } | T::MultiEnum { .. }),
        OutputKind::Object => matches!(schema.kind, T::Object { .. }),
    }
}

fn contains_template(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => s.contains("${{"),
        serde_json::Value::Array(v) => v.iter().any(contains_template),
        serde_json::Value::Object(m) => m.values().any(contains_template),
        _ => false,
    }
}

fn has_destructive_task(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match &n.kind {
        NodeKind::Task(t) => t.destructive,
        _ => children(n).into_iter().any(has_destructive_task),
    })
}

fn children(node: &Node) -> Vec<&[Node]> {
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

fn each<'a>(nodes: &'a [Node], visit: &mut impl FnMut(&'a crate::ir::Call)) {
    for node in nodes {
        if let NodeKind::Call(call) = &node.kind {
            visit(call);
        }
        for group in children(node) {
            each(group, visit);
        }
    }
}

fn each_mut(nodes: &mut [Node], visit: &mut impl FnMut(&str, &mut crate::ir::Call)) {
    for node in nodes {
        let path = node.source_path.clone();
        match &mut node.kind {
            NodeKind::Call(call) => visit(&path, call),
            NodeKind::Group(g) => {
                each_mut(&mut g.children, visit);
                each_mut(&mut g.catch, visit);
                each_mut(&mut g.finally, visit);
            }
            NodeKind::Parallel { children, .. } => each_mut(children, visit),
            NodeKind::Foreach(f) => each_mut(&mut f.children, visit),
            NodeKind::Loop(l) => each_mut(&mut l.children, visit),
            NodeKind::Switch(s) => {
                for case in &mut s.cases {
                    each_mut(&mut case.children, visit);
                }
                each_mut(&mut s.default, visit);
            }
            _ => {}
        }
    }
}

fn escape(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

fn diagnostic(code: &str, path: String, message: String) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        message: Some(message),
        path,
        line: 0,
        column: 0,
    }
}

/// Pin every call in `ir` from `runbooks` (keyed by [`call_key`]), or
/// report each one that cannot be pinned.
pub(crate) fn resolve(
    ir: &mut NormalizedRunbook,
    runbooks: &BTreeMap<String, CallResolution>,
) -> Result<(), Vec<Diagnostic>> {
    let own = ir.metadata.id.clone();
    let mut errors = vec![];
    let mut visit = |path: &str, call: &mut crate::ir::Call| {
        let at = format!("{path}/call");
        let Some(found) = runbooks.get(&call_key(&call.runbook, call.version)) else {
            errors.push(diagnostic(
                "spec.call_unresolved",
                format!("{at}/runbook"),
                match call.version {
                    Some(n) => format!(
                        "Runbook '{}' has no published version {n} in this project.",
                        call.runbook
                    ),
                    None => format!(
                        "Runbook '{}' has no published version in this project.",
                        call.runbook
                    ),
                },
            ));
            return;
        };
        let mut reaches: BTreeSet<String> = found.reaches.iter().cloned().collect();
        reaches.insert(call.runbook.clone());
        if reaches.contains(&own) {
            errors.push(diagnostic(
                "spec.call_cycle",
                format!("{at}/runbook"),
                format!(
                    "Calling '{}' would run '{own}' again: `call` cannot recurse.",
                    call.runbook
                ),
            ));
            return;
        }
        let depth = found.depth.saturating_add(1);
        if depth > MAX_CALL_DEPTH {
            errors.push(diagnostic(
                "spec.call_depth_exceeded",
                format!("{at}/runbook"),
                format!("`call` nests at most {MAX_CALL_DEPTH} runbooks deep (PRD §17)."),
            ));
            return;
        }
        let mut ok = true;
        for name in call.with.keys() {
            if !found.inputs.contains_key(name) {
                ok = false;
                errors.push(diagnostic(
                    "spec.call_unknown_input",
                    format!("{at}/with/{}", escape(name)),
                    format!("Runbook '{}' declares no input '{name}'.", call.runbook),
                ));
            }
        }
        for (name, required) in &found.inputs {
            if *required && !call.with.contains_key(name) {
                ok = false;
                errors.push(diagnostic(
                    "spec.call_missing_input",
                    format!("{at}/with"),
                    format!("Runbook '{}' requires input '{name}'.", call.runbook),
                ));
            }
        }
        // Literal values are checked against the child's declarations now;
        // expressions when the child starts.
        for (name, value) in &call.with {
            if let Some(schema) = found.input_schemas.get(name)
                && !contains_template(value)
                // As the start resolves it: defaults applied (re-check 1).
                && crate::resolve_data(schema, value).is_err()
            {
                ok = false;
                errors.push(diagnostic(
                    "spec.call_input_type",
                    format!("{at}/with/{}", escape(name)),
                    format!(
                        "This value does not satisfy input '{name}' of runbook '{}'.",
                        call.runbook
                    ),
                ));
            }
        }
        // The step can expose only outputs the child produces, of a shape
        // its declaration accepts.
        if let crate::DataType::Object { properties } = &call.outputs.kind {
            for (name, schema) in properties {
                let at = format!("{at}/outputs/{}", escape(name));
                match found.outputs.get(name) {
                    None => {
                        ok = false;
                        errors.push(diagnostic(
                            "spec.call_unknown_output",
                            at,
                            format!("Runbook '{}' has no output '{name}'.", call.runbook),
                        ));
                    }
                    Some(kind) if !compatible(*kind, schema) => {
                        ok = false;
                        errors.push(diagnostic(
                            "spec.call_output_type",
                            at,
                            format!(
                                "Output '{name}' of runbook '{}' is not of this type.",
                                call.runbook
                            ),
                        ));
                    }
                    Some(_) => {}
                }
            }
        }
        if ok {
            call.target = Some(CallTarget {
                runbook_id: found.runbook_id.clone(),
                version_id: found.version_id.clone(),
                version: found.version,
                depth,
                reaches: reaches.into_iter().collect(),
                destructive: found.destructive,
            });
        }
    };
    each_mut(&mut ir.steps, &mut visit);
    each_mut(&mut ir.finally, &mut visit);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Move every call's target out of `ir`, in tree order (snapshot admission
/// compares the lowered tree without them).
pub(crate) fn take_targets(ir: &mut NormalizedRunbook, out: &mut Vec<Option<CallTarget>>) {
    let mut visit = |_: &str, call: &mut crate::ir::Call| out.push(call.target.take());
    each_mut(&mut ir.steps, &mut visit);
    each_mut(&mut ir.finally, &mut visit);
}

/// Put targets taken by [`take_targets`] back, in the same order.
pub(crate) fn put_targets(
    ir: &mut NormalizedRunbook,
    targets: &mut impl Iterator<Item = Option<CallTarget>>,
) {
    let mut visit = |_: &str, call: &mut crate::ir::Call| call.target = targets.next().flatten();
    each_mut(&mut ir.steps, &mut visit);
    each_mut(&mut ir.finally, &mut visit);
}

/// A stored pin is well formed: typed IDs, a depth within the limit and a
/// sorted, unique reach set that includes the child.
pub(crate) fn valid_target(target: &Option<CallTarget>) -> bool {
    let Some(t) = target else {
        return false;
    };
    t.runbook_id.parse::<runbook_model::RunbookId>().is_ok()
        && t.version_id.parse::<runbook_model::VersionId>().is_ok()
        && t.version > 0
        && (1..=MAX_CALL_DEPTH).contains(&t.depth)
        && !t.reaches.is_empty()
        && t.reaches.windows(2).all(|w| w[0] < w[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_carry_the_pinned_number() {
        assert_eq!(call_key("db-migrate", None), "db-migrate");
        assert_eq!(call_key("db-migrate", Some(3)), "db-migrate@3");
    }
}
