//! Sealed deterministic sessions with exact reads and bounded typed output.
//! Raw CEL values and their canonical sources remain crate-private.
use crate::{ExprDiagnostic, Origin};
use cel_core::{BudgetError, EvaluationBudget};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ReadKind {
    Value,
    Membership,
    Absence,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReadRef {
    pub origin: Origin,
    pub kind: ReadKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalStats {
    pub cost: u64,
    pub loop_entries: u64,
    pub reads: Vec<ReadRef>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    CostExceeded,
    Cancelled,
    ReadLimit,
    OutputLimit,
    SchemaMismatch,
    MissingValue { path: String },
    TypeError,
    IndexOutOfBounds,
}
#[derive(Debug, Clone)]
pub struct EvalSession {
    pub(crate) budget: EvaluationBudget,
    // First wrapper-level terminal error wins; budget terminal errors are also sticky.
    pub(crate) terminal: Option<EvalError>,
    pub(crate) reads: crate::reads::ReadTracker,
    pub(crate) output: crate::output::OutputBudget,
}
impl EvalSession {
    pub fn new(budget: u64) -> Result<Self, ExprDiagnostic> {
        if budget > 100_000 {
            return Err(ExprDiagnostic {
                code: "expression.evaluation_limit",
                span: 0..0,
                message: "Evaluation fuel exceeds the hard limit of 100,000".into(),
            });
        }
        Ok(Self {
            budget: EvaluationBudget::new(budget),
            terminal: None,
            reads: crate::reads::ReadTracker::default(),
            output: crate::output::OutputBudget::default(),
        })
    }
    pub fn cancel(&mut self) {
        if self.terminal_error().is_none() {
            self.budget.cancel();
        }
    }
    pub fn stats(&self) -> EvalStats {
        EvalStats {
            cost: self.budget.used(),
            loop_entries: self.budget.iterations(),
            reads: self.reads.snapshot(),
        }
    }
    pub(crate) fn terminal_error(&self) -> Option<EvalError> {
        self.terminal
            .clone()
            .or_else(|| self.reads.is_limited().then_some(EvalError::ReadLimit))
            .or_else(|| {
                self.budget.error().map(|error| match error {
                    BudgetError::CostExceeded => EvalError::CostExceeded,
                    BudgetError::Cancelled => EvalError::Cancelled,
                    BudgetError::ReadLimit => EvalError::ReadLimit,
                })
            })
    }
}

use crate::bindings::{BoundNode, BoundValue};
use std::{collections::BTreeMap, sync::Arc};

/// Holds only descendants and canonical metadata, never the containing node or
/// bindings/index. This direction prevents reference cycles.
#[derive(Debug, Clone)]
pub(crate) struct PreparedRecord {
    pub(crate) name: String,
    pub(crate) fields: Arc<BTreeMap<String, Arc<BoundNode>>>,
}
impl cel_core::MessageValue for PreparedRecord {
    fn type_name(&self) -> &str {
        &self.name
    }
    fn eq_message(&self, _other: &dyn cel_core::MessageValue) -> bool {
        false
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn clone_boxed(&self) -> Box<dyn cel_core::MessageValue> {
        Box::new(self.clone())
    }
}

/// Called only in the validated, iterative bindings construction pass.
pub(crate) fn prepare_value(value: &BoundValue) -> cel_core::Value {
    use cel_core::Value;
    match value {
        BoundValue::Null => Value::Null,
        BoundValue::Bool(value) => Value::Bool(*value),
        BoundValue::Int(value) => Value::Int(*value),
        BoundValue::Double(value) => Value::Double(*value),
        BoundValue::String(value) => Value::String(value.clone()),
        BoundValue::Timestamp(value) => Value::Timestamp(cel_core::Timestamp::new(
            value.unix_seconds,
            value.nanos as i32,
        )),
        BoundValue::List(items) => Value::List(
            items
                .iter()
                .map(|node| node.cel.clone())
                .collect::<Vec<_>>()
                .into(),
        ),
        BoundValue::Record { ty, fields } => Value::Message(Box::new(PreparedRecord {
            name: format!("runbook_record_{}", ty.0),
            fields: fields.clone(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Bindings, EventTime, FieldDecl, InputNode, InputValue, RecordIdentity, SchemaDef, TypeDef,
        TypeEnv, TypeId, check_expression,
    };
    fn origin(path: impl Into<String>) -> Origin {
        Origin {
            path: path.into(),
            record: RecordIdentity {
                execution_id: "e".into(),
                step_run_id: None,
                iteration_path: vec![],
                revision: 1,
            },
        }
    }
    fn fixture(types: Vec<TypeDef>, ty: u32, value: InputValue) -> (TypeEnv, Bindings) {
        let env = TypeEnv::new(&SchemaDef {
            types,
            roots: BTreeMap::from([(
                "x".into(),
                FieldDecl {
                    ty: TypeId(ty),
                    required: true,
                    nullable: false,
                },
            )]),
        })
        .unwrap();
        let bindings = Bindings::new(
            &env,
            &BTreeMap::from([(
                "x".into(),
                InputNode {
                    value,
                    origin: origin("x"),
                },
            )]),
            &origin(""),
            EventTime {
                unix_seconds: 123,
                nanos: 456,
            },
        )
        .unwrap();
        (env, bindings)
    }
    #[test]
    fn fuel_exhaustion_is_terminal_with_prepared_large_activation() {
        let (env, bindings) = fixture(
            vec![TypeDef::Int, TypeDef::List(TypeId(0))],
            1,
            InputValue::List(
                (0..100_000)
                    .map(|i| InputNode {
                        value: InputValue::Int(i),
                        origin: origin(format!("x[{i}]")),
                    })
                    .collect(),
            ),
        );
        let expression = check_expression("x.exists(i, i < 0) || true", &env).unwrap();
        let mut session = EvalSession::new(100).unwrap();
        let cel_core::Value::List(list) = &bindings.roots["x"].cel else {
            panic!()
        };
        let before = list.as_ptr();
        assert_eq!(
            evaluate_raw(&expression, &bindings, &mut session).unwrap_err(),
            EvalError::CostExceeded
        );
        assert!(session.stats().cost <= 100);
        assert!(session.stats().loop_entries < 100_000);
        let stats = session.stats();
        assert_eq!(
            evaluate_raw(&expression, &bindings, &mut session).unwrap_err(),
            EvalError::CostExceeded
        );
        assert_eq!(session.stats(), stats);
        assert_eq!(list.as_ptr(), before);
    }
    #[test]
    fn public_maximum_string_is_precharged_before_comparison() {
        let (env, bindings) = fixture(
            vec![TypeDef::String],
            0,
            InputValue::String("a".repeat(65_536)),
        );
        let expr = check_expression("x == x", &env).unwrap();
        let mut session = EvalSession::new(100).unwrap();
        assert_eq!(
            evaluate_raw(&expr, &bindings, &mut session).unwrap_err(),
            EvalError::CostExceeded
        );
        assert_eq!(session.stats().cost, 8);
    }
    #[test]
    fn fixed_time_and_session_reuse_preserve_cumulative_cost() {
        let (env, bindings) = fixture(vec![TypeDef::Int], 0, InputValue::Int(42));
        let now = check_expression("now()", &env).unwrap();
        let timestamp =
            check_expression("timestamp('1970-01-01T00:02:03.000000456Z')", &env).unwrap();
        let mut session = EvalSession::new(10).unwrap();
        let expected = cel_core::Value::Timestamp(cel_core::Timestamp::new(123, 456));
        assert_eq!(
            evaluate_raw(&now, &bindings, &mut session).unwrap(),
            expected
        );
        assert_eq!(
            evaluate_raw(&timestamp, &bindings, &mut session).unwrap(),
            expected
        );
        assert_eq!(session.stats().cost, 2);
        session.cancel();
        let stats = session.stats();
        assert_eq!(
            evaluate_raw(&now, &bindings, &mut session).unwrap_err(),
            EvalError::Cancelled
        );
        assert_eq!(session.stats(), stats);
    }
    #[test]
    fn nonnull_record_can_be_compared_with_null() {
        let (env, bindings) = fixture(
            vec![TypeDef::Record(BTreeMap::new())],
            0,
            InputValue::Record(BTreeMap::new()),
        );
        for (source, expected) in [("x == null", false), ("null != x", true)] {
            let expr = check_expression(source, &env).unwrap();
            let mut session = EvalSession::new(100).unwrap();
            assert_eq!(
                evaluate_raw(&expr, &bindings, &mut session).unwrap(),
                cel_core::Value::Bool(expected)
            );
        }
    }
    #[test]
    fn record_field_uses_prepared_shared_value() {
        let (env, bindings) = fixture(
            vec![
                TypeDef::Int,
                TypeDef::Record(BTreeMap::from([(
                    "n".into(),
                    FieldDecl {
                        ty: TypeId(0),
                        nullable: false,
                        required: true,
                    },
                )])),
            ],
            1,
            InputValue::Record(BTreeMap::from([(
                "n".into(),
                InputNode {
                    origin: origin("x.n"),
                    value: InputValue::Int(42),
                },
            )])),
        );
        let expr = check_expression("x.n", &env).unwrap();
        let mut session = EvalSession::new(9).unwrap();
        assert_eq!(
            evaluate_raw(&expr, &bindings, &mut session).unwrap(),
            cel_core::Value::Int(42)
        );
        assert_eq!(session.stats().cost, 9);
    }
}

// Only the sealed wrapper can construct this activation. Every resolution clones
// a prepared scalar/Arc or bounded record metadata; it never walks a collection.
struct PreparedActivation<'a>(&'a crate::Bindings);
impl cel_core::Activation for PreparedActivation<'_> {
    fn resolve(&self, name: &str) -> Option<cel_core::Value> {
        self.0.roots.get(name).map(|node| node.cel.clone())
    }
}

#[derive(Debug)]
struct PreparedRegistry;
impl cel_core::ProtoTypeResolver for PreparedRegistry {
    fn get_field_type(&self, _: &str, _: &str) -> Option<cel_core::CelType> {
        None
    }
    fn has_message(&self, _: &str) -> bool {
        false
    }
    fn is_extension(&self, _: &str, _: &str) -> bool {
        false
    }
    fn get_enum_value(&self, _: &str, _: &str) -> Option<i32> {
        None
    }
    fn resolve_qualified(&self, _: &[&str], _: &str) -> Option<cel_core::ResolvedProtoType> {
        None
    }
    fn resolve_message_name(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
impl cel_core::ProtoRegistry for PreparedRegistry {
    fn construct_message(
        &self,
        _: &str,
        _: &[cel_core::StructFieldValue],
        _: bool,
    ) -> cel_core::Value {
        denied()
    }
    fn message_field_access(
        &self,
        msg: &dyn cel_core::MessageValue,
        field: &str,
        _: bool,
        _: bool,
    ) -> cel_core::Value {
        let Some(record) = msg.as_any().downcast_ref::<PreparedRecord>() else {
            return denied();
        };
        record
            .fields
            .get(field)
            .map(|node| node.cel.clone())
            .unwrap_or_else(denied)
    }
    fn message_has_field(&self, _: &dyn cel_core::MessageValue, _: &str) -> cel_core::Value {
        denied()
    }
    fn get_extension_value(
        &self,
        _: &dyn cel_core::MessageValue,
        _: &str,
        _: bool,
        _: bool,
    ) -> Option<cel_core::Value> {
        None
    }
    fn has_extension(&self, _: &dyn cel_core::MessageValue, _: &str) -> Option<bool> {
        None
    }
}
fn denied() -> cel_core::Value {
    cel_core::Value::error(cel_core::EvalError::invalid_argument(
        "Unsupported runtime operation",
    ))
}

// Raw evaluation is private so output limits cannot be bypassed.
pub(crate) fn evaluate_raw_with_source(
    expr: &crate::TypedExpression,
    bindings: &crate::Bindings,
    session: &mut EvalSession,
) -> Result<(cel_core::Value, Option<Arc<BoundNode>>), EvalError> {
    if let Some(error) = session.terminal_error() {
        return Err(error);
    }
    if expr.schema_fingerprint() != bindings.schema.fingerprint() {
        return Err(EvalError::SchemaMismatch);
    }
    let runtime = cel_core::EvaluationRuntime {
        registry: Some(&PreparedRegistry),
        event_time: Some(cel_core::Timestamp::new(
            bindings.event_time.unix_seconds,
            bindings.event_time.nanos as i32,
        )),
        timestamp_literals: Some(&expr.timestamp_literals),
    };
    let observer = crate::reads::ReadObserver::new(bindings, &session.budget, &mut session.reads);
    let result = expr.program.eval_with_runtime_context_and_observer(
        &PreparedActivation(bindings),
        &session.budget,
        &runtime,
        &observer,
    );
    let source = observer.result_source(expr.program.ast().expr().id);
    let missing = observer.missing_index();
    drop(observer);
    if let Some(error) = session.terminal_error() {
        return Err(error);
    }
    if let Some(index) = missing {
        return Err(session.reads.missing_error(index, &session.budget));
    }
    match result.result {
        Ok(cel_core::Value::Error(error)) => {
            Err(if error.kind == cel_core::EvalErrorKind::IndexOutOfBounds {
                EvalError::IndexOutOfBounds
            } else {
                EvalError::TypeError
            })
        }
        Ok(value) => Ok((value, source)),
        Err(BudgetError::CostExceeded) => Err(EvalError::CostExceeded),
        Err(BudgetError::Cancelled) => Err(EvalError::Cancelled),
        Err(BudgetError::ReadLimit) => Err(EvalError::ReadLimit),
    }
}

#[cfg(test)] // Private raw-evaluation test seam.
pub(crate) fn evaluate_raw(
    expr: &crate::TypedExpression,
    bindings: &crate::Bindings,
    session: &mut EvalSession,
) -> Result<cel_core::Value, EvalError> {
    evaluate_raw_with_source(expr, bindings, session).map(|(value, _)| value)
}

/// A typed JSON result and the session's cumulative accounting snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedValue {
    pub value: serde_json::Value,
    pub stats: EvalStats,
}

pub fn evaluate_in(
    expr: &crate::TypedExpression,
    bindings: &crate::Bindings,
    session: &mut EvalSession,
) -> Result<EvaluatedValue, EvalError> {
    let (value, source) = evaluate_raw_with_source(expr, bindings, session)?;
    let value = crate::output::materialize(&value, source.as_deref(), session)?;
    if let Some(error) = session.terminal_error() {
        return Err(error);
    }
    Ok(EvaluatedValue {
        value,
        stats: session.stats(),
    })
}

pub fn evaluate(
    expr: &crate::TypedExpression,
    bindings: &crate::Bindings,
    budget: u64,
) -> Result<EvaluatedValue, EvalError> {
    let mut session = EvalSession::new(budget).map_err(|_| EvalError::CostExceeded)?;
    evaluate_in(expr, bindings, &mut session)
}
