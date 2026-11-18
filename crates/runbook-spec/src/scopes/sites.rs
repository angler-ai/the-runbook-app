//! The explicit source allowlist, shared by discovery and checking.
use super::*;
#[derive(Clone, Copy)]
pub(super) enum SourceValue<'a> {
    Text(&'a str),
    Json(&'a Value),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Expected {
    Any,
    String,
    Bool,
    List,
    Timestamp,
    Variable,
    Field,
}
pub(super) fn node_sites<'a>(
    node: &'a Node,
    mut visit: impl FnMut(String, SourceValue<'a>, Expected) -> Result<()>,
) -> Result<()> {
    let p = &node.source_path;
    if let Some(s) = &node.title {
        visit(pointer(p, "title"), SourceValue::Text(s), Expected::String)?;
    }
    if let Some(s) = &node.guard {
        visit(pointer(p, "if"), SourceValue::Text(s), Expected::Bool)?;
    }
    match &node.kind {
        NodeKind::Task(t) => {
            if let Some(Assignee::Expression(s)) = &t.assignee {
                visit(
                    pointer(p, "assignee"),
                    SourceValue::Text(s),
                    Expected::String,
                )?;
            }
            match &t.instructions {
                Some(Instructions::Markdown(s)) => visit(
                    pointer(p, "instructions"),
                    SourceValue::Text(s),
                    Expected::String,
                )?,
                Some(Instructions::Audience {
                    default,
                    human,
                    agent,
                }) => {
                    for (name, s) in [("default", default), ("human", human), ("agent", agent)] {
                        if let Some(s) = s {
                            visit(
                                pointer(&pointer(p, "instructions"), name),
                                SourceValue::Text(s),
                                Expected::String,
                            )?;
                        }
                    }
                }
                None => {}
            }
            for (name, v) in &t.parameters {
                visit(
                    pointer(&pointer(p, "with"), name),
                    SourceValue::Json(v),
                    Expected::Any,
                )?;
            }
            for (i, s) in t.checklist.iter().enumerate() {
                visit(
                    pointer(&pointer(p, "checklist"), &i.to_string()),
                    SourceValue::Text(s),
                    Expected::String,
                )?;
            }
        }
        NodeKind::Foreach(f) => visit(
            pointer(&pointer(p, "foreach"), "items"),
            SourceValue::Text(&f.items),
            Expected::List,
        )?,
        // key/until use their child scopes and are visited by lexical traversal.
        NodeKind::Switch(s) => {
            for c in &s.cases {
                visit(
                    pointer(&c.source_path, "when"),
                    SourceValue::Text(&c.when),
                    Expected::Bool,
                )?;
            }
        }
        NodeKind::Approval(a) => visit(
            pointer(&pointer(p, "approval"), "prompt"),
            SourceValue::Text(&a.prompt),
            Expected::String,
        )?,
        NodeKind::Set(a) => {
            for (name, v) in &a.variables {
                visit(
                    pointer(&pointer(&pointer(p, "set"), "vars"), name),
                    SourceValue::Json(v),
                    Expected::Variable,
                )?;
            }
            for (name, v) in &a.fields {
                visit(
                    pointer(&pointer(&pointer(p, "set"), "fields"), name),
                    SourceValue::Json(v),
                    Expected::Field,
                )?;
            }
        }
        NodeKind::Wait(Wait {
            kind: WaitKind::Until(s),
            ..
        }) => visit(
            pointer(&pointer(p, "wait"), "until"),
            SourceValue::Text(s),
            Expected::Timestamp,
        )?,
        NodeKind::Call(c) => {
            for (name, v) in &c.with {
                visit(
                    pointer(&pointer(&pointer(p, "call"), "with"), name),
                    SourceValue::Json(v),
                    Expected::Any,
                )?;
            }
        }
        NodeKind::End(e) => {
            if let Some(s) = &e.message {
                visit(
                    pointer(&pointer(p, "end"), "message"),
                    SourceValue::Text(s),
                    Expected::String,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(super) fn global_sites<'a>(
    ir: &'a NormalizedRunbook,
    mut visit: impl FnMut(String, SourceValue<'a>, Expected) -> Result<()>,
) -> Result<()> {
    if let Some(s) = &ir.execution.title {
        visit(
            "/spec/execution/title".into(),
            SourceValue::Text(s),
            Expected::String,
        )?;
    }
    if let Some(c) = &ir.execution.concurrency {
        visit(
            "/spec/execution/concurrency/key".into(),
            SourceValue::Text(&c.key),
            Expected::String,
        )?;
    }
    Ok(())
}
pub(super) fn strings(
    value: SourceValue<'_>,
    path: &str,
    visit: &mut impl FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    match value {
        SourceValue::Text(s) => visit(path, s)?,
        SourceValue::Json(Value::String(s)) => visit(path, s)?,
        SourceValue::Json(Value::Array(values)) => {
            for (i, v) in values.iter().enumerate() {
                strings(SourceValue::Json(v), &pointer(path, &i.to_string()), visit)?;
            }
        }
        SourceValue::Json(Value::Object(values)) => {
            for (n, v) in values {
                strings(SourceValue::Json(v), &pointer(path, n), visit)?;
            }
        }
        _ => {}
    }
    Ok(())
}
#[derive(Default, Debug)]
pub(super) struct Discovery {
    pub segments: usize,
    pub values: usize,
    pub declaration_nodes: usize,
    pub declaration_fields: usize,
    pub strings: usize,
    pub bytes: usize,
    pub expression_bytes: usize,
    pub lexical_scopes: usize,
}
pub(super) fn discover(ir: &NormalizedRunbook) -> Result<Discovery> {
    let mut found = Discovery::default();
    let mut visit = |p: String, v, e| {
        let _ = e;
        found.values += value_count(v);
        strings(v, &p, &mut |p, s| {
            found.strings += 1;
            found.bytes += s.len();
            if s.contains("${{") {
                let summary =
                    runbook_expr::summarize_template_syntax(s).map_err(|e| expr_error(e, p))?;
                found.segments += summary.expressions.len();
                found.expression_bytes += summary
                    .expressions
                    .iter()
                    .map(|e| e.source_span.len())
                    .sum::<usize>();
                if found.segments > 1024 {
                    return Err(error("spec.compile_limit", p));
                }
            }
            Ok(())
        })
    };
    global_sites(ir, &mut visit)?;
    for (n, v) in &ir.variables {
        visit(
            pointer("/spec/vars", n),
            SourceValue::Json(v),
            Expected::Any,
        )?;
    }
    fn walk<'a>(
        nodes: &'a [Node],
        visit: &mut impl FnMut(String, SourceValue<'a>, Expected) -> Result<()>,
    ) -> Result<()> {
        for n in nodes {
            node_sites(n, &mut *visit)?;
            match &n.kind {
                NodeKind::Foreach(f) => {
                    if let Some(s) = &f.key {
                        visit(
                            pointer(&pointer(&n.source_path, "foreach"), "key"),
                            SourceValue::Text(s),
                            Expected::String,
                        )?;
                    }
                }
                NodeKind::Loop(l) => visit(
                    pointer(&pointer(&n.source_path, "loop"), l.condition().key()),
                    SourceValue::Text(l.condition().source()),
                    Expected::Bool,
                )?,
                _ => {}
            }
            for children in child_groups(n) {
                walk(children, visit)?;
            }
        }
        Ok(())
    }
    walk(&ir.steps, &mut visit)?;
    walk(&ir.finally, &mut visit)?;
    for (n, v) in &ir.outputs {
        visit(
            pointer("/spec/outputs", n),
            SourceValue::Json(v),
            Expected::Any,
        )?;
    }
    if let Some(s) = &ir.report {
        visit(
            "/spec/report".into(),
            SourceValue::Text(s),
            Expected::String,
        )?;
    }
    fn scopes(nodes: &[Node]) -> usize {
        nodes
            .iter()
            .map(|n| {
                usize::from(matches!(n.kind, NodeKind::Foreach(_) | NodeKind::Loop(_)))
                    + child_groups(n).iter().map(|c| scopes(c)).sum::<usize>()
            })
            .sum()
    }
    found.lexical_scopes =
        1 + scopes(&ir.steps) + scopes(&ir.finally) + usize::from(ir.report.is_some());
    fn declarations(schema: &DataSchema) -> (usize, usize) {
        match &schema.kind {
            DataType::Object { properties } => properties
                .values()
                .map(declarations)
                .fold((1, properties.len()), |(nodes, fields), (n, f)| {
                    (nodes + n, fields + f)
                }),
            DataType::List { items } => {
                let (n, f) = declarations(items);
                (n + 1, f)
            }
            _ => (1, 0),
        }
    }
    let mut schemas = vec![&ir.inputs, &ir.fields];
    let mut pending: Vec<_> = ir.steps.iter().chain(&ir.finally).collect();
    while let Some(node) = pending.pop() {
        match &node.kind {
            NodeKind::Task(t) => schemas.push(&t.outputs),
            NodeKind::Call(c) => schemas.push(&c.outputs),
            _ => {}
        }
        for children in child_groups(node) {
            pending.extend(children);
        }
    }
    for schema in schemas {
        let (nodes, fields) = declarations(schema);
        found.declaration_nodes += nodes;
        found.declaration_fields += fields;
    }
    Ok(found)
}
fn value_count(value: SourceValue<'_>) -> usize {
    1 + match value {
        SourceValue::Json(Value::Array(values)) => values
            .iter()
            .map(|v| value_count(SourceValue::Json(v)))
            .sum(),
        SourceValue::Json(Value::Object(values)) => values
            .values()
            .map(|v| value_count(SourceValue::Json(v)))
            .sum(),
        _ => 0,
    }
}
