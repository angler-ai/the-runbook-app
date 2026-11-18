//! Private admission gates for the exact, versioned checker profile.
use std::collections::BTreeSet;

use crate::compile::diagnostic;
use crate::{ExprDiagnostic, ResultType, TypeDef, TypeEnv, TypeId};
use cel_core::types::{BinaryOp, ComprehensionData, Expr, SpannedExpr, UnaryOp};
use cel_core::{CelType, CheckResult, FunctionDecl, OverloadDecl};

pub(crate) fn unsupported(expr: &SpannedExpr) -> ExprDiagnostic {
    diagnostic(
        "expression.unsupported",
        expr.span.clone(),
        "Operation or type is outside runbook-cel/1",
    )
}

// The only declaration table admitted by the checker audit. Implementations are
// installed by the sealed runtime, never supplied by callers or the full stdlib.
pub(crate) fn declarations() -> Vec<FunctionDecl> {
    let t = CelType::type_param("T");
    let mut result = Vec::new();
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

fn canonical_exists(data: &ComprehensionData) -> bool {
    let accu = |e: &SpannedExpr| matches!(&e.node, Expr::Ident(name) if name == &data.accu_var);
    data.iter_var2.is_empty()
        && data.accu_var == "@result"
        && data.iter_var != data.accu_var
        && matches!(data.accu_init.node, Expr::Bool(false))
        && matches!(&data.loop_condition.node, Expr::Unary { op: UnaryOp::Not, expr } if accu(expr))
        && matches!(&data.loop_step.node, Expr::Binary { op: BinaryOp::Or, right, .. } if accu(right))
        && accu(&data.result)
}

/// A bounded flat view, produced before any recursive checker operation.
/// Call callee identifiers count toward the envelope even though the checker
/// does not assign those non-value nodes types.
pub(crate) fn validate_parsed(root: &SpannedExpr) -> Result<Vec<&SpannedExpr>, ExprDiagnostic> {
    let mut nodes = Vec::new();
    let mut pending = vec![(root, 0)];
    let mut ids = BTreeSet::new();
    let mut comprehensions = 0;
    while let Some((node, depth)) = pending.pop() {
        if nodes.len() == 256 || depth > 32 {
            return Err(diagnostic(
                "expression.compile_limit",
                node.span.clone(),
                "Expression exceeds 256 nodes or depth 32",
            ));
        }
        if !ids.insert(node.id) {
            return Err(unsupported(node));
        }
        nodes.push(node);
        let mut child = |expr| pending.push((expr, depth + 1));
        match &node.node {
            Expr::Null | Expr::Bool(_) | Expr::Int(_) | Expr::String(_) | Expr::Ident(_) => {}
            Expr::Float(f) if f.is_finite() => {}
            Expr::Unary { op, expr } => {
                if *op == UnaryOp::Neg && !matches!(expr.node, Expr::Int(_) | Expr::Float(_)) {
                    return Err(unsupported(node));
                }
                // -(-9223372036854775808) has no int64 value: refuse it at
                // compile time instead of failing at runtime (R52).
                if *op == UnaryOp::Neg && matches!(expr.node, Expr::Int(i64::MIN)) {
                    return Err(diagnostic(
                        "expression.integer_overflow",
                        node.span.clone(),
                        "Negating the smallest int64 overflows",
                    ));
                }
                child(expr.as_ref());
            }
            Expr::Binary { op, left, right } => {
                if !matches!(
                    op,
                    BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::Lt
                        | BinaryOp::Le
                        | BinaryOp::Gt
                        | BinaryOp::Ge
                        | BinaryOp::And
                        | BinaryOp::Or
                ) {
                    return Err(unsupported(node));
                }
                child(right.as_ref());
                child(left.as_ref());
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                child(else_expr.as_ref());
                child(then_expr.as_ref());
                child(cond.as_ref());
            }
            Expr::Member {
                expr,
                optional: false,
                ..
            } => child(expr.as_ref()),
            Expr::Index {
                expr,
                index,
                optional: false,
            } => {
                child(index.as_ref());
                child(expr.as_ref());
            }
            Expr::Call { expr, args } => {
                match (&expr.node, args.as_slice()) {
                    (Expr::Ident(name), []) if name == "now" => {}
                    (Expr::Ident(name), [arg])
                        if name == "timestamp"
                            && matches!(&arg.node, Expr::String(s) if s.len() <= 64) => {}
                    _ => return Err(unsupported(node)),
                }
                for arg in args.iter().rev() {
                    child(arg);
                }
                child(expr.as_ref());
            }
            Expr::Comprehension(data) => {
                comprehensions += 1;
                if comprehensions > 1 || !canonical_exists(data) {
                    return Err(unsupported(node));
                }
                for expr in [
                    &data.result,
                    &data.loop_step,
                    &data.loop_condition,
                    &data.accu_init,
                    &data.iter_range,
                ] {
                    child(expr.as_ref());
                }
            }
            _ => return Err(unsupported(node)),
        }
    }
    Ok(nodes)
}

fn scalar(ty: &CelType) -> bool {
    let mut ty = ty;
    while let CelType::Wrapper(inner) = ty {
        ty = inner;
    }
    matches!(
        ty,
        CelType::Null
            | CelType::Bool
            | CelType::Int
            | CelType::Double
            | CelType::String
            | CelType::Timestamp
    )
}

fn string_type(mut ty: &CelType) -> bool {
    while let CelType::Wrapper(inner) = ty {
        ty = inner;
    }
    *ty == CelType::String
}

fn record_null_comparison(left: &SpannedExpr, right: &SpannedExpr, checked: &CheckResult) -> bool {
    matches!(
        (checked.get_type(left.id), checked.get_type(right.id)),
        (Some(CelType::Message(_)), Some(CelType::Null))
            | (Some(CelType::Null), Some(CelType::Message(_)))
    )
}

// Check forbidden typed operations even when upstream reports no matching
// overload (e.g. string indexing), preserving the public profile error class.
pub(crate) fn validate_operations(
    nodes: &[&SpannedExpr],
    checked: &CheckResult,
) -> Result<(), ExprDiagnostic> {
    for node in nodes {
        match &node.node {
            Expr::Binary {
                op: BinaryOp::Eq | BinaryOp::Ne,
                left,
                right,
            } => {
                if !record_null_comparison(left, right, checked)
                    && [left, right]
                        .iter()
                        .filter_map(|e| checked.get_type(e.id))
                        .any(|t| {
                            matches!(
                                t,
                                CelType::List(_) | CelType::Message(_) | CelType::Map(_, _)
                            )
                        })
                {
                    return Err(unsupported(node));
                }
            }
            Expr::Index { expr, .. } if checked.get_type(expr.id).is_some_and(string_type) => {
                return Err(unsupported(node));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn validate_checked(
    nodes: &[&SpannedExpr],
    checked: &CheckResult,
    env: &TypeEnv,
) -> Result<ResultType, ExprDiagnostic> {
    let allowed: BTreeSet<String> = declarations()
        .into_iter()
        .flat_map(|d| d.overloads.into_iter().map(|o| o.id))
        .collect();
    // Upstream represents nullable messages as Message, erasing nullability.
    // Conservatively retain it for every use of a record type that has any
    // nullable declaration. This can overapproximate, never promise non-null.
    let mut nullable_records = BTreeSet::new();
    let mut visit = |decl: &crate::FieldDecl| {
        if decl.nullable && matches!(env.type_definition(decl.ty), Some(TypeDef::Record(_))) {
            nullable_records.insert(decl.ty);
        }
    };
    for decl in env.binding_roots().values() {
        visit(decl);
    }
    for id in 0..crate::limits::TYPE_NODES {
        if let Some(TypeDef::Record(fields)) = env.type_definition(TypeId(id as u32)) {
            for decl in fields.values() {
                visit(decl);
            }
        }
    }
    for node in nodes {
        if let Some(ty) = checked.get_type(node.id) {
            result_type(ty, env, &nullable_records).ok_or_else(|| unsupported(node))?;
        }
        if let Some(reference) = checked.get_reference(node.id)
            && (reference.value.is_some()
                || reference.enum_type.is_some()
                || reference
                    .overload_ids
                    .iter()
                    .any(|id| !allowed.contains(id)))
        {
            return Err(unsupported(node));
        }
        match &node.node {
            Expr::Binary {
                op: BinaryOp::Eq | BinaryOp::Ne,
                left,
                right,
            } => {
                if !record_null_comparison(left, right, checked)
                    && ![left, right]
                        .iter()
                        .all(|e| checked.get_type(e.id).is_some_and(scalar))
                {
                    return Err(unsupported(node));
                }
            }
            Expr::Comprehension(data)
                if !matches!(checked.get_type(data.iter_range.id), Some(CelType::List(_))) =>
            {
                return Err(unsupported(node));
            }
            _ => {}
        }
    }
    let mut result = result_type(
        checked
            .get_type(nodes[0].id)
            .ok_or_else(|| unsupported(nodes[0]))?,
        env,
        &nullable_records,
    )
    .ok_or_else(|| unsupported(nodes[0]))?;
    // The real checker erases nullability in timestamp/record ternaries.
    // Supplement only that result metadata: do not replace type inference or
    // admit Dyn. Reverse preorder visits each value's children before it.
    let mut nullable_values = BTreeSet::new();
    for node in nodes.iter().rev() {
        let declared_nullable = checked
            .get_type(node.id)
            .and_then(|ty| result_type(ty, env, &nullable_records))
            .is_some_and(|ty| matches!(ty, ResultType::Null | ResultType::Nullable(_)));
        let branch_nullable = matches!(&node.node, Expr::Ternary { then_expr, else_expr, .. }
            if nullable_values.contains(&then_expr.id) || nullable_values.contains(&else_expr.id));
        if declared_nullable || branch_nullable {
            nullable_values.insert(node.id);
        }
    }
    if nullable_values.contains(&nodes[0].id)
        && !matches!(result, ResultType::Null | ResultType::Nullable(_))
    {
        result = ResultType::Nullable(Box::new(result));
    }
    Ok(result)
}

fn result_type(
    ty: &CelType,
    env: &TypeEnv,
    nullable_records: &BTreeSet<TypeId>,
) -> Option<ResultType> {
    // Types are a linear list/wrapper chain terminating at an opaque record or
    // primitive; build the owned result from the leaf without recursive walks.
    let mut current = ty;
    let mut wrappers = Vec::new();
    let mut result = loop {
        if wrappers.len() > 17 {
            return None;
        }
        match current {
            CelType::Null => break ResultType::Null,
            CelType::Bool => break ResultType::Bool,
            CelType::Int => break ResultType::Int,
            CelType::Double => break ResultType::Double,
            CelType::String => break ResultType::String,
            CelType::Timestamp => break ResultType::Timestamp,
            CelType::List(inner) => {
                wrappers.push(false);
                current = inner;
            }
            CelType::Wrapper(inner) => {
                wrappers.push(true);
                current = inner;
            }
            CelType::Message(name) => {
                let id = TypeId(name.strip_prefix("runbook_record_")?.parse().ok()?);
                if !matches!(env.type_definition(id), Some(TypeDef::Record(_)))
                    || name.as_ref() != format!("runbook_record_{}", id.0)
                {
                    return None;
                }
                let record = ResultType::Record {
                    type_id: id,
                    schema_fingerprint: env.fingerprint(),
                };
                break if nullable_records.contains(&id) {
                    ResultType::Nullable(Box::new(record))
                } else {
                    record
                };
            }
            _ => return None,
        }
    };
    for nullable in wrappers.into_iter().rev() {
        result = if nullable {
            ResultType::Nullable(Box::new(result))
        } else {
            ResultType::List(Box::new(result))
        };
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_match_exact_audited_upstream_signatures() {
        let declarations = declarations();
        assert_eq!(declarations.len(), 14);
        assert_eq!(
            declarations
                .iter()
                .map(|d| d.overloads.len())
                .sum::<usize>(),
            31
        );
        let mut expected = BTreeSet::from([
            "equals".to_string(),
            "not_equals".into(),
            "conditional".into(),
            "index_list".into(),
            "logical_not".into(),
            "logical_and".into(),
            "logical_or".into(),
            "negate_int64".into(),
            "negate_double".into(),
            "string_to_timestamp".into(),
            "runbook_now".into(),
        ]);
        for prefix in ["less", "less_equals", "greater", "greater_equals"] {
            for suffix in ["bool", "int64", "double", "string", "timestamp"] {
                expected.insert(format!("{prefix}_{suffix}"));
            }
        }
        let upstream = cel_core::Env::with_standard_library();
        let mut actual = BTreeSet::new();
        for declaration in declarations {
            for overload in declaration.overloads {
                assert!(actual.insert(overload.id.clone()));
                assert!(!overload.is_member);
                assert!(overload.implementation.is_none());
                if overload.id == "runbook_now" {
                    assert_eq!(declaration.name, "now");
                    assert!(overload.params.is_empty() && overload.type_params.is_empty());
                    assert_eq!(overload.result, CelType::Timestamp);
                } else {
                    let original = upstream.functions()[&declaration.name]
                        .overloads
                        .iter()
                        .find(|o| o.id == overload.id)
                        .unwrap();
                    assert_eq!(overload.params, original.params);
                    assert_eq!(overload.result, original.result);
                    assert_eq!(overload.type_params, original.type_params);
                    assert_eq!(overload.is_member, original.is_member);
                }
            }
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn canonical_exists_shape_is_required_before_checker() {
        let parsed = cel_core::parser::parse_bounded(
            "xs.exists(x, true)",
            &cel_core::CompileLimits::runbook_v1(),
        )
        .unwrap();
        let mut expr = parsed.ast.unwrap();
        assert!(validate_parsed(&expr).is_ok());
        if let Expr::Comprehension(data) = &mut expr.node {
            data.accu_init.node = Expr::Bool(true);
        }
        assert_eq!(
            validate_parsed(&expr).unwrap_err().code,
            "expression.unsupported"
        );
    }
}
