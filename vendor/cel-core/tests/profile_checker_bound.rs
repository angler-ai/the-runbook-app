//! Conditional checker-envelope evidence, not the production profile gate.
//! Source construction uses the upstream parser on small trusted fixtures.
//! The portable wrapper must independently enforce the audited preconditions.
use cel_core::parser::parse;
use cel_core::types::{CelType, Expr, FunctionDecl, OverloadDecl, ResolvedProtoType, SpannedExpr};
use cel_core::CheckErrorKind;
use cel_core::Env;
use cel_core::{MessageValue, Value};
use cel_core::{ProtoRegistry, ProtoTypeResolver, StructFieldValue};
use std::any::Any;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
enum Shape {
    Scalar(CelType),
    List(usize),
    Record(BTreeMap<String, usize>),
}

#[derive(Clone, Debug)]
struct Schema {
    types: Vec<Shape>,
    roots: BTreeMap<String, usize>,
}

impl Schema {
    fn simple() -> Self {
        Self {
            types: vec![Shape::Scalar(CelType::Bool)],
            roots: BTreeMap::from([("x".into(), 0)]),
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.types.len() > 1024 || self.roots.len() > 256 {
            return Err("schema size");
        }
        let mut fields = 0;
        let mut edges = Vec::with_capacity(self.types.len());
        for ty in &self.types {
            let children = match ty {
                Shape::Scalar(ty) => {
                    let inner = if let CelType::Wrapper(inner) = ty {
                        inner.as_ref()
                    } else {
                        ty
                    };
                    if !matches!(
                        inner,
                        CelType::Bool
                            | CelType::Int
                            | CelType::Double
                            | CelType::String
                            | CelType::Timestamp
                    ) {
                        return Err("dynamic type");
                    }
                    vec![]
                }
                Shape::List(id) => vec![*id],
                Shape::Record(record) => {
                    fields += record.len();
                    if record.len() > 256 || fields > 4096 {
                        return Err("schema fields");
                    }
                    record.values().copied().collect()
                }
            };
            if children.iter().any(|id| *id >= self.types.len()) {
                return Err("undeclared type");
            }
            edges.push(children);
        }
        if self.roots.values().any(|id| *id >= self.types.len()) {
            return Err("undeclared type");
        }
        // Explicit DFS colors + cached heights: shared DAG children are not
        // exponentially revisited and a completed child still contributes depth.
        let mut color = vec![0; self.types.len()];
        let mut height = vec![0; self.types.len()];
        for root in 0..self.types.len() {
            let mut pending = vec![(root, false)];
            while let Some((id, exit)) = pending.pop() {
                if exit {
                    height[id] = edges[id]
                        .iter()
                        .map(|child| height[*child] + 1)
                        .max()
                        .unwrap_or(0);
                    if height[id] > 16 {
                        return Err("schema depth");
                    }
                    color[id] = 2;
                } else {
                    match color[id] {
                        1 => return Err("schema cycle"),
                        2 => continue,
                        _ => {}
                    }
                    color[id] = 1;
                    pending.push((id, true));
                    pending.extend(edges[id].iter().map(|child| (*child, false)));
                }
            }
        }
        Ok(())
    }

    fn cel_type(&self, id: usize) -> CelType {
        match &self.types[id] {
            Shape::Scalar(ty) => ty.clone(),
            Shape::List(inner) => CelType::list(self.cel_type(*inner)),
            Shape::Record(_) => CelType::message(&format!("R{id}")),
        }
    }
}

impl ProtoTypeResolver for Schema {
    fn get_field_type(&self, message: &str, field: &str) -> Option<CelType> {
        let id: usize = message.strip_prefix('R')?.parse().ok()?;
        match self.types.get(id)? {
            Shape::Record(fields) => fields.get(field).map(|id| self.cel_type(*id)),
            _ => None,
        }
    }
    fn has_message(&self, message: &str) -> bool {
        message
            .strip_prefix('R')
            .and_then(|id| id.parse::<usize>().ok())
            .is_some_and(|id| matches!(self.types.get(id), Some(Shape::Record(_))))
    }
    fn is_extension(&self, _: &str, _: &str) -> bool {
        false
    }
    fn get_enum_value(&self, _: &str, _: &str) -> Option<i32> {
        None
    }
    fn resolve_qualified(&self, _: &[&str], _: &str) -> Option<ResolvedProtoType> {
        None
    }
    fn resolve_message_name(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// Checker fixtures deliberately panic if a runtime-only registry method is used.
impl ProtoRegistry for Schema {
    fn construct_message(&self, _: &str, _: &[StructFieldValue], _: bool) -> Value {
        panic!("runtime")
    }
    fn message_field_access(&self, _: &dyn MessageValue, _: &str, _: bool, _: bool) -> Value {
        panic!("runtime")
    }
    fn message_has_field(&self, _: &dyn MessageValue, _: &str) -> Value {
        panic!("runtime")
    }
    fn get_extension_value(
        &self,
        _: &dyn MessageValue,
        _: &str,
        _: bool,
        _: bool,
    ) -> Option<Value> {
        panic!("runtime")
    }
    fn has_extension(&self, _: &dyn MessageValue, _: &str) -> Option<bool> {
        panic!("runtime")
    }
}

// This is the exact declaration table required of Task 5, including candidates
// tried for ill-typed/error-recovery inputs. No wholesale standard library.
fn declarations() -> Vec<FunctionDecl> {
    let t = CelType::type_param("T");
    let mut result = vec![];
    for (name, id, params, output, generic) in [
        (
            "_==_",
            "equals",
            vec![t.clone(), t.clone()],
            CelType::Bool,
            true,
        ),
        (
            "_!=_",
            "not_equals",
            vec![t.clone(), t.clone()],
            CelType::Bool,
            true,
        ),
        (
            "_?_:_",
            "conditional",
            vec![CelType::Bool, t.clone(), t.clone()],
            t.clone(),
            true,
        ),
        (
            "_[_]",
            "index_list",
            vec![CelType::list(t.clone()), CelType::Int],
            t,
            true,
        ),
        (
            "!_",
            "logical_not",
            vec![CelType::Bool],
            CelType::Bool,
            false,
        ),
        (
            "_&&_",
            "logical_and",
            vec![CelType::Bool, CelType::Bool],
            CelType::Bool,
            false,
        ),
        (
            "_||_",
            "logical_or",
            vec![CelType::Bool, CelType::Bool],
            CelType::Bool,
            false,
        ),
        (
            "timestamp",
            "string_to_timestamp",
            vec![CelType::String],
            CelType::Timestamp,
            false,
        ),
        ("now", "runbook_now", vec![], CelType::Timestamp, false),
    ] {
        let mut overload = OverloadDecl::function(id, params, output);
        if generic {
            overload = overload.with_type_params(vec!["T".into()]);
        }
        result.push(FunctionDecl::new(name).with_overload(overload));
    }
    result.push(
        FunctionDecl::new("-_")
            .with_overload(OverloadDecl::function(
                "negate_int64",
                vec![CelType::Int],
                CelType::Int,
            ))
            .with_overload(OverloadDecl::function(
                "negate_double",
                vec![CelType::Double],
                CelType::Double,
            )),
    );
    for (name, prefix) in [
        ("_<_", "less"),
        ("_<=_", "less_equals"),
        ("_>_", "greater"),
        ("_>=_", "greater_equals"),
    ] {
        let mut function = FunctionDecl::new(name);
        for (suffix, ty) in [
            ("bool", CelType::Bool),
            ("int64", CelType::Int),
            ("double", CelType::Double),
            ("string", CelType::String),
            ("timestamp", CelType::Timestamp),
        ] {
            function = function.with_overload(OverloadDecl::function(
                format!("{prefix}_{suffix}"),
                vec![ty.clone(), ty],
                CelType::Bool,
            ));
        }
        result.push(function);
    }
    result
}

fn ast(source: &str) -> SpannedExpr {
    assert!(source.len() <= 4096);
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "fixture syntax: {:?}",
        parsed.errors
    );
    parsed.ast.unwrap()
}

fn envelope(ast: &SpannedExpr) -> Result<usize, &'static str> {
    let mut pending = vec![(ast, 0)];
    let mut nodes = 0;
    while let Some((expr, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 256 {
            return Err("AST nodes");
        }
        if depth > 32 {
            return Err("AST depth");
        }
        let mut push = |child| pending.push((child, depth + 1));
        match &expr.node {
            Expr::Null
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Ident(_) => {}
            Expr::Unary { expr, .. }
            | Expr::Member {
                expr,
                optional: false,
                ..
            } => push(expr),
            Expr::Binary { left, right, .. } => {
                push(left);
                push(right);
            }
            Expr::Index {
                expr,
                index,
                optional: false,
            } => {
                push(expr);
                push(index);
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                push(cond);
                push(then_expr);
                push(else_expr);
            }
            Expr::Call { expr, args } => {
                push(expr);
                for arg in args {
                    push(arg);
                }
            }
            Expr::Comprehension(comp) => {
                for child in [
                    &comp.iter_range,
                    &comp.accu_init,
                    &comp.loop_condition,
                    &comp.loop_step,
                    &comp.result,
                ] {
                    push(child);
                }
            }
            _ => return Err("fixture outside audited forms"),
        }
    }
    // This measures trusted fixtures, not canonical-macro/profile validation.
    Ok(nodes)
}

fn checked(
    schema: &Schema,
    ast: &SpannedExpr,
    entries: &Cell<usize>,
) -> Result<cel_core::CheckResult, &'static str> {
    schema.validate()?;
    envelope(ast)?;
    let mut env = Env::new().with_proto_registry(Arc::new(schema.clone()));
    for function in declarations() {
        env.add_function(function);
    }
    for (name, id) in &schema.roots {
        env.add_variable(name, schema.cel_type(*id));
    }
    entries.set(entries.get() + 1);
    Ok(env.check(ast))
}

fn balanced(leaves: usize, leaf: &str) -> String {
    if leaves == 1 {
        return leaf.into();
    }
    format!(
        "({} && {})",
        balanced(leaves / 2, leaf),
        balanced(leaves - leaves / 2, leaf)
    )
}

#[test]
fn checker_envelope_rejects_outside_inputs() {
    let entries = Cell::new(0);
    let schema = Schema::simple();
    assert_eq!(
        checked(&schema, &ast(&format!("{}true", "!".repeat(33))), &entries).unwrap_err(),
        "AST depth"
    );
    assert_eq!(
        checked(
            &schema,
            &ast(&format!("!!{}", balanced(128, "true"))),
            &entries
        )
        .unwrap_err(),
        "AST nodes"
    );
    let invalid = [
        (
            Schema {
                types: vec![Shape::List(1)],
                roots: BTreeMap::new(),
            },
            "undeclared type",
        ),
        (
            Schema {
                types: vec![Shape::Scalar(CelType::Dyn)],
                roots: BTreeMap::new(),
            },
            "dynamic type",
        ),
        (
            Schema {
                types: vec![Shape::List(0)],
                roots: BTreeMap::new(),
            },
            "schema cycle",
        ),
        (nested_lists(17), "schema depth"),
    ];
    for (schema, reason) in invalid {
        assert_eq!(
            checked(&schema, &ast("true"), &entries).unwrap_err(),
            reason
        );
    }
    assert_eq!(entries.get(), 0, "rejection must precede checker entry");
    assert!(checked(&schema, &ast("x"), &entries).unwrap().is_ok());
    assert_eq!(entries.get(), 1);
}

fn nested_lists(depth: usize) -> Schema {
    let mut schema = Schema {
        types: vec![Shape::Scalar(CelType::Int)],
        roots: BTreeMap::from([("xs".into(), depth)]),
    };
    for id in 0..depth {
        schema.types.push(Shape::List(id));
    }
    schema
}

#[test]
fn fixed_signatures_match_upstream_and_exclude_other_candidates() {
    let upstream = Env::with_standard_library();
    let table = declarations();
    assert_eq!(table.len(), 14);
    for function in &table {
        assert!(function.overloads.len() <= 5);
        for overload in &function.overloads {
            assert!(!overload.is_member);
            assert!(overload.params.len() <= 3);
            assert!(overload.type_params.len() <= 1);
            if function.name == "now" {
                continue;
            }
            let original = upstream.functions()[&function.name]
                .overloads
                .iter()
                .find(|candidate| candidate.id == overload.id)
                .unwrap();
            assert_eq!(overload.params, original.params);
            assert_eq!(overload.result, original.result);
            assert_eq!(overload.type_params, original.type_params);
            assert_eq!(overload.is_member, original.is_member);
        }
    }
}

#[test]
fn bounded_checker_inputs_finish_on_small_stack() {
    const CHILD: &str = "RUNBOOK_CHECKER_SMALL_STACK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bounded_checker_inputs_finish_on_small_stack",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("checker-corpus-complete"));
        return;
    }
    // Fixture parsing occurs outside the small-stack checker thread.
    let fixtures = checker_fixtures();
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let entries = Cell::new(0);
            for (schema, tree, expected, error_kind) in fixtures {
                let nodes = envelope(&tree).unwrap();
                let result = checked(&schema, &tree, &entries).unwrap();
                assert!(result.errors.len() <= 2 * nodes);
                assert!(result.type_map.len() <= nodes);
                assert!(result.reference_map.len() <= nodes);
                assert_eq!(
                    result.is_ok(),
                    error_kind.is_none(),
                    "unexpected diagnostics: {:?}",
                    result.errors
                );
                if let Some(expected) = expected {
                    assert_eq!(result.get_type(tree.id), Some(&expected));
                }
                if let Some(kind) = error_kind {
                    assert!(result.errors.iter().any(|error| matches!(
                        (&error.kind, kind),
                        (CheckErrorKind::UndefinedField { .. }, "field")
                            | (CheckErrorKind::NoMatchingOverload { .. }, "overload")
                    )));
                }
            }
            assert!(entries.get() >= 10);
        })
        .unwrap()
        .join()
        .unwrap();
    println!("checker-corpus-complete");
}

type Fixture = (Schema, SpannedExpr, Option<CelType>, Option<&'static str>);

fn checker_fixtures() -> Vec<Fixture> {
    let basic = Schema::simple();
    let max_nodes = ast(&format!("!{}", balanced(128, "true")));
    assert_eq!(envelope(&max_nodes), Ok(256));
    let mut fixtures = vec![
        (basic.clone(), max_nodes, Some(CelType::Bool), None),
        (
            basic.clone(),
            ast(&format!("{}true", "!".repeat(32))),
            Some(CelType::Bool),
            None,
        ),
        (
            basic.clone(),
            ast("1 < 'bad'"),
            Some(CelType::Error),
            Some("overload"),
        ),
        (
            basic.clone(),
            ast("true ? 1 : 'bad'"),
            Some(CelType::Dyn),
            None,
        ),
        (basic.clone(), ast("1 == 'bad'"), Some(CelType::Bool), None),
        (
            basic.clone(),
            ast("now() >= timestamp('2026-11-09T00:00:00Z')"),
            Some(CelType::Bool),
            None,
        ),
    ];
    let lists = nested_lists(16);
    fixtures.push((
        lists.clone(),
        ast("true ? xs : xs"),
        Some(lists.cel_type(16)),
        None,
    ));
    fixtures.push((
        lists.clone(),
        ast(&format!("xs{}", "[0]".repeat(16))),
        Some(CelType::Int),
        None,
    ));
    fixtures.push((
        lists.clone(),
        ast("xs['bad']"),
        Some(CelType::Error),
        Some("overload"),
    ));
    let mut mismatched_lists = lists.clone();
    let first = mismatched_lists.types.len();
    mismatched_lists.types.push(Shape::Scalar(CelType::String));
    for id in first..first + 16 {
        mismatched_lists.types.push(Shape::List(id));
    }
    mismatched_lists.roots.insert("ys".into(), first + 16);
    fixtures.push((
        mismatched_lists.clone(),
        ast("true ? xs : ys"),
        Some(CelType::Dyn),
        None,
    ));
    let mut ternaries = "ys".to_owned();
    for _ in 0..32 {
        ternaries = format!("true ? xs : ({ternaries})");
    }
    fixtures.push((
        mismatched_lists,
        ast(&ternaries),
        Some(lists.cel_type(16)),
        None,
    ));
    let mut nullable_lists = lists.clone();
    nullable_lists.types[0] = Shape::Scalar(CelType::wrapper(CelType::Int));
    fixtures.push((
        nullable_lists.clone(),
        ast("true ? xs : xs"),
        Some(nullable_lists.cel_type(16)),
        None,
    ));
    fixtures.push((
        lists,
        ast("xs.no_such_field"),
        Some(CelType::Error),
        Some("field"),
    ));
    fixtures.push((
        nested_lists(1),
        ast("xs.exists(item, item > 0)"),
        Some(CelType::Bool),
        None,
    ));
    let nullable = Schema {
        types: vec![Shape::Scalar(CelType::wrapper(CelType::String))],
        roots: BTreeMap::from([("v".into(), 0)]),
    };
    fixtures.push((
        nullable.clone(),
        ast("v == null"),
        Some(CelType::Bool),
        None,
    ));
    fixtures.push((
        nullable,
        ast("true ? null : v"),
        Some(CelType::wrapper(CelType::String)),
        None,
    ));
    let mut records = Schema {
        types: vec![Shape::Scalar(CelType::Bool)],
        roots: BTreeMap::from([("r".into(), 16)]),
    };
    for id in 0..16 {
        records
            .types
            .push(Shape::Record(BTreeMap::from([("f".into(), id)])));
    }
    fixtures.push((
        records.clone(),
        ast(&format!("r{}", ".f".repeat(16))),
        Some(CelType::Bool),
        None,
    ));
    fixtures.push((
        records,
        ast(&format!("r{}", ".f".repeat(32))),
        Some(CelType::Error),
        Some("field"),
    ));
    fixtures.push((
        basic.clone(),
        ast(&format!("missing{}", ".f".repeat(32))),
        Some(CelType::Error),
        Some("field"),
    ));
    // Every schema capacity simultaneously: 1,024 nodes, 4,096 fields,
    // 256 fields/record, 256 roots. Record field types remain opaque names.
    let mut wide = Schema {
        types: vec![Shape::Scalar(CelType::Bool); 1024],
        roots: (0..256).map(|i| (format!("r{i}"), i)).collect(),
    };
    for id in 0..16 {
        wide.types[id] = Shape::Record((0..256).map(|i| (format!("f{i}"), 1023)).collect());
    }
    fixtures.push((wide, ast("r0.f255"), Some(CelType::Bool), None));
    // Many generic calls and malformed fields trigger persistent substitutions,
    // nongeneric candidates, diagnostics and finalization in one accepted tree.
    fixtures.push((
        basic.clone(),
        ast(&balanced(64, "(1 == 1)")),
        Some(CelType::Bool),
        None,
    ));
    fixtures.push((
        basic,
        ast(&balanced(64, "x.bad")),
        Some(CelType::Bool),
        Some("field"),
    ));
    fixtures
}
