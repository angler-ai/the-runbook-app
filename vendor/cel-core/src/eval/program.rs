//! Compiled CEL program ready for evaluation.
//!
//! A `Program` combines a parsed/checked AST with a function registry,
//! providing a convenient interface for evaluating expressions.

use std::collections::HashMap;
use std::sync::Arc;

use super::proto_registry::ProtoRegistry;
use super::{Activation, EmptyActivation, Evaluator, FunctionRegistry, Value};
use crate::Ast;

/// A compiled CEL program ready for evaluation.
///
/// The program holds a reference to the AST and a function registry,
/// providing a convenient interface for evaluating the expression
/// against different variable bindings.
#[derive(Clone)]
pub struct Program {
    ast: Arc<Ast>,
    functions: Arc<FunctionRegistry>,
    proto_registry: Option<Arc<dyn ProtoRegistry>>,
    abbreviations: Option<Arc<HashMap<String, String>>>,
    strong_enums: bool,
}

/// Borrowed, prevalidated runtime inputs for the restricted profile. The host
/// wrapper supplies these privately; constructing this struct performs no work.
#[derive(Default)]
pub struct EvaluationRuntime<'a> {
    pub registry: Option<&'a dyn ProtoRegistry>,
    pub event_time: Option<super::Timestamp>,
    pub timestamp_literals: Option<&'a std::collections::BTreeMap<i64, super::Timestamp>>,
}

impl Program {
    /// Create a new program from an AST and function registry.
    pub fn new(ast: Arc<Ast>, functions: Arc<FunctionRegistry>) -> Self {
        Self {
            ast,
            functions,
            proto_registry: None,
            abbreviations: None,
            strong_enums: true,
        }
    }

    /// Create a new program with a type registry.
    pub fn with_proto_registry(
        ast: Arc<Ast>,
        functions: Arc<FunctionRegistry>,
        proto_registry: Arc<dyn ProtoRegistry>,
    ) -> Self {
        Self {
            ast,
            functions,
            proto_registry: Some(proto_registry),
            abbreviations: None,
            strong_enums: true,
        }
    }

    /// Create a new program with abbreviations.
    pub fn with_abbreviations(
        ast: Arc<Ast>,
        functions: Arc<FunctionRegistry>,
        abbreviations: HashMap<String, String>,
    ) -> Self {
        Self {
            ast,
            functions,
            proto_registry: None,
            abbreviations: Some(Arc::new(abbreviations)),
            strong_enums: true,
        }
    }

    /// Create a new program with a type registry and abbreviations.
    pub fn with_proto_registry_and_abbreviations(
        ast: Arc<Ast>,
        functions: Arc<FunctionRegistry>,
        proto_registry: Arc<dyn ProtoRegistry>,
        abbreviations: HashMap<String, String>,
    ) -> Self {
        Self {
            ast,
            functions,
            proto_registry: Some(proto_registry),
            abbreviations: Some(Arc::new(abbreviations)),
            strong_enums: true,
        }
    }

    /// Use legacy (weak) enum mode where enum values are returned as plain integers.
    pub fn with_legacy_enums(mut self) -> Self {
        self.strong_enums = false;
        self
    }

    /// Get the AST for this program.
    pub fn ast(&self) -> &Ast {
        &self.ast
    }

    /// Get the function registry for this program.
    pub fn functions(&self) -> &FunctionRegistry {
        &self.functions
    }

    /// Evaluate the program with the given variable bindings.
    pub fn eval(&self, activation: &dyn Activation) -> Value {
        self.eval_with_container(activation, "")
    }

    /// Evaluate the program with the given variable bindings and container namespace.
    ///
    /// The container is used for resolving unqualified type names following
    /// C++ namespace rules. For example, with container "cel.expr.conformance.proto3"
    /// and type name "TestAllTypes", resolution tries:
    /// 1. cel.expr.conformance.proto3.TestAllTypes
    /// 2. cel.expr.conformance.TestAllTypes
    /// 3. cel.expr.TestAllTypes
    /// 4. cel.TestAllTypes
    /// 5. TestAllTypes
    pub fn eval_with_container(&self, activation: &dyn Activation, container: &str) -> Value {
        self.eval_internal(activation, container, None, None, None)
    }

    /// EXPERIMENTAL: dispatch/iteration/string-comparison meter, not a complete CEL cost model.
    /// Caller must enforce the separately audited restricted profile and bound callbacks.
    pub fn eval_with_budget(
        &self,
        activation: &dyn Activation,
        limit: u64,
    ) -> super::fuel::MeteredResult {
        let fuel = super::fuel::EvaluationBudget::new(limit);
        self.eval_with_context(activation, &fuel)
    }

    /// Evaluate using shared, sticky fuel across calls and child evaluators.
    pub fn eval_with_context(
        &self,
        activation: &dyn Activation,
        budget: &super::fuel::EvaluationBudget,
    ) -> super::fuel::MeteredResult {
        let value = self.eval_internal(
            activation,
            "",
            Some(budget),
            None,
            Some(&super::observer::NOOP_OBSERVER),
        );
        budget.finish(value)
    }

    /// Evaluate with fixed runtime data without rebuilding a registry or activation.
    pub fn eval_with_runtime_context(
        &self,
        activation: &dyn Activation,
        budget: &super::fuel::EvaluationBudget,
        runtime: &EvaluationRuntime<'_>,
    ) -> super::fuel::MeteredResult {
        let value = self.eval_internal(
            activation,
            "",
            Some(budget),
            Some(runtime),
            Some(&super::observer::NOOP_OBSERVER),
        );
        budget.finish(value)
    }

    /// Evaluate with bounded-profile semantic events. The caller must bound its
    /// observer; callbacks are not accepted by the public runbook wrapper.
    pub fn eval_with_observer(
        &self,
        activation: &dyn Activation,
        budget: &super::EvaluationBudget,
        observer: &dyn super::EvalObserver,
    ) -> super::MeteredResult {
        budget.finish(self.eval_internal(activation, "", Some(budget), None, Some(observer)))
    }

    /// Combine immutable runtime data with a separately borrowed observer.
    pub fn eval_with_runtime_context_and_observer(
        &self,
        activation: &dyn Activation,
        budget: &super::EvaluationBudget,
        runtime: &EvaluationRuntime<'_>,
        observer: &dyn super::EvalObserver,
    ) -> super::MeteredResult {
        budget.finish(self.eval_internal(
            activation,
            "",
            Some(budget),
            Some(runtime),
            Some(observer),
        ))
    }

    fn eval_internal(
        &self,
        activation: &dyn Activation,
        container: &str,
        fuel: Option<&super::fuel::EvaluationBudget>,
        runtime: Option<&EvaluationRuntime<'_>>,
        observer: Option<&dyn super::EvalObserver>,
    ) -> Value {
        let observation = observer.map(|observer| super::observer::Observation {
            observer,
            next_scope: std::cell::Cell::new(0),
        });
        let mut evaluator = Evaluator::new(activation, &self.functions);
        evaluator.fuel = fuel;
        evaluator.runtime = runtime;
        evaluator.observation = observation.as_ref();

        // Pass type info from checked AST
        if let Some(type_info) = self.ast.type_info() {
            evaluator = evaluator.with_reference_map(&type_info.reference_map);
        }

        // Pass type registry
        if let Some(ref proto_registry) = self.proto_registry {
            evaluator = evaluator.with_proto_registry(proto_registry.as_ref());
        }

        if let Some(registry) = runtime.and_then(|runtime| runtime.registry) {
            evaluator = evaluator.with_proto_registry(registry);
        }

        // Set container for type resolution
        if !container.is_empty() {
            evaluator = evaluator.with_container(container);
        }

        // Pass abbreviations
        if let Some(ref abbreviations) = self.abbreviations {
            evaluator = evaluator.with_abbreviations(abbreviations);
        }

        // Pass strong enum setting
        if !self.strong_enums {
            evaluator = evaluator.with_legacy_enums();
        }

        evaluator.eval(self.ast.expr())
    }

    /// Evaluate the program with no variable bindings.
    pub fn eval_empty(&self) -> Value {
        self.eval(&EmptyActivation)
    }
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Program")
            .field("ast", &self.ast)
            .field("functions", &format!("{} functions", self.functions.len()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::MapActivation;
    use crate::parse;

    fn create_program(source: &str) -> Program {
        let result = parse(source);
        assert!(result.errors.is_empty());
        let ast = Ast::new_unchecked(result.ast.unwrap(), source);
        Program::new(Arc::new(ast), Arc::new(FunctionRegistry::new()))
    }

    #[test]
    fn test_eval_literal() {
        let program = create_program("42");
        assert_eq!(program.eval_empty(), Value::Int(42));
    }

    #[test]
    fn test_eval_with_variables() {
        let program = create_program("x + y");
        let mut activation = MapActivation::new();
        activation.insert("x", Value::Int(1));
        activation.insert("y", Value::Int(2));
        assert_eq!(program.eval(&activation), Value::Int(3));
    }

    #[test]
    fn test_reuse_program() {
        let program = create_program("x * 2");

        let mut act1 = MapActivation::new();
        act1.insert("x", Value::Int(5));
        assert_eq!(program.eval(&act1), Value::Int(10));

        let mut act2 = MapActivation::new();
        act2.insert("x", Value::Int(21));
        assert_eq!(program.eval(&act2), Value::Int(42));
    }
}

#[cfg(test)]
mod shared_budget_tests {
    use super::*;
    use crate::eval::{BudgetError, EvaluationBudget, Function, MapActivation, Overload};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn program(source: &str, calls: Arc<AtomicUsize>) -> Program {
        let parsed = crate::parse(source);
        assert!(parsed.errors.is_empty());
        let mut registry = FunctionRegistry::new();
        registry.register(Function::new("later").with_overload(Overload::new(
            "later",
            false,
            0,
            Arc::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Value::Bool(true)
            }),
        )));
        Program::new(
            Arc::new(Ast::new_unchecked(parsed.ast.unwrap(), source)),
            Arc::new(registry),
        )
    }

    #[test]
    fn fuel_exhaustion_is_terminal_across_children_and_calls() {
        let mut activation = MapActivation::new();
        activation.insert(
            "items",
            Value::List((0..100_000).map(Value::Int).collect::<Vec<_>>().into()),
        );
        for source in [
            "items.exists(x, x < 0) && later()",
            "items.exists(x, x < 0) || later()",
            "items.exists(x, x < 0) || true",
            "items.exists(x, x < 0) ? later() : later()",
            "items.exists(x, x < 0 || later())",
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            // The last case exhausts before the first predicate's callback.
            let budget = EvaluationBudget::new(if source.ends_with("|| later())") {
                7
            } else {
                100
            });
            let result = program(source, calls.clone()).eval_with_context(&activation, &budget);
            assert_eq!(
                result.result.unwrap_err(),
                BudgetError::CostExceeded,
                "{source}"
            );
            assert!(result.used <= 100);
            assert!(result.iterations < 100_000);
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{source}");
            program("later()", calls.clone()).eval_with_context(&activation, &budget);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn exact_precharges_and_cancel_are_sticky() {
        let budget = EvaluationBudget::new(10);
        assert!(budget.charge(10));
        assert_eq!(budget.used(), 10);
        assert!(!budget.charge(u64::MAX));
        assert_eq!(budget.used(), 10);
        assert!(!budget.charge(0));
        budget.cancel();
        assert_eq!(budget.error(), Some(BudgetError::CostExceeded));
        let budget = EvaluationBudget::new(10);
        budget.cancel();
        let calls = Arc::new(AtomicUsize::new(0));
        let result = program("later()", calls.clone()).eval_with_context(&EmptyActivation, &budget);
        assert_eq!(result.result.unwrap_err(), BudgetError::Cancelled);
        assert_eq!(result.used, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn comparison_bytes_are_precharged_before_work() {
        let mut activation = MapActivation::new();
        activation.insert("a", Value::String("a".repeat(1_000_000).into()));
        activation.insert("b", Value::String("a".repeat(1_000_000).into()));
        for operator in ["==", "!=", "<", "<=", ">", ">="] {
            let budget = EvaluationBudget::new(100);
            let result = program(&format!("a {operator} b"), Arc::default())
                .eval_with_context(&activation, &budget);
            assert_eq!(result.result.unwrap_err(), BudgetError::CostExceeded);
            assert_eq!(result.used, 5);
        }
    }
    #[test]
    fn index_lookup_requires_its_own_precharge() {
        let mut activation = MapActivation::new();
        activation.insert("items", Value::List(vec![Value::Int(42)].into()));
        let budget = EvaluationBudget::new(4);
        let result = program("items[0]", Arc::default()).eval_with_context(&activation, &budget);
        assert_eq!(result.result.unwrap_err(), BudgetError::CostExceeded);
        assert_eq!(result.used, 4);
    }

    #[test]
    fn runtime_constants_use_supplied_time_and_call_identity() {
        let now = program("now()", Arc::default());
        let timestamp = program(
            "timestamp(\"not parsed during evaluation\")",
            Arc::default(),
        );
        let time = super::super::Timestamp::new(123, 456);
        let constants = std::collections::BTreeMap::from([(timestamp.ast().expr().id, time)]);
        let runtime = EvaluationRuntime {
            registry: None,
            event_time: Some(time),
            timestamp_literals: Some(&constants),
        };
        let budget = EvaluationBudget::new(2);
        assert_eq!(
            now.eval_with_runtime_context(&EmptyActivation, &budget, &runtime)
                .result
                .unwrap(),
            Value::Timestamp(time)
        );
        assert_eq!(
            timestamp
                .eval_with_runtime_context(&EmptyActivation, &budget, &runtime)
                .result
                .unwrap(),
            Value::Timestamp(time)
        );
        assert_eq!(budget.used(), 2);
        assert_eq!(
            now.eval_with_runtime_context(&EmptyActivation, &budget, &runtime)
                .result
                .unwrap_err(),
            BudgetError::CostExceeded
        );
    }

    #[test]
    fn exists_keeps_the_extra_accumulator_recognition_entry() {
        let mut activation = MapActivation::new();
        activation.insert(
            "items",
            Value::List(vec![Value::Int(0), Value::Int(1), Value::Int(2)].into()),
        );
        // Base dispatch/range/accumulator/result: 6. Each false predicate: 11;
        // matching predicate: 9; recognizing true accumulator next entry: 4.
        for (target, used, iterations, expected) in [
            (0, 19, 2, true),
            (1, 30, 3, true),
            (2, 37, 3, true),
            (3, 39, 3, false),
        ] {
            let budget = EvaluationBudget::new(used);
            let result = program(&format!("items.exists(x, x == {target})"), Arc::default())
                .eval_with_context(&activation, &budget);
            assert_eq!(result.result.unwrap(), Value::Bool(expected));
            assert_eq!(result.used, used);
            assert_eq!(result.iterations, iterations);
        }
    }

    #[test]
    fn cancellation_inside_callback_stops_overload_retry_and_later_branch() {
        use std::{cell::RefCell, rc::Rc};
        thread_local! { static BUDGET: RefCell<Option<Rc<EvaluationBudget>>> = const { RefCell::new(None) }; }
        let budget = Rc::new(EvaluationBudget::new(100));
        BUDGET.with(|slot| *slot.borrow_mut() = Some(budget.clone()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = FunctionRegistry::new();
        registry.register(
            Function::new("cancel")
                .with_overload(Overload::new(
                    "cancel",
                    false,
                    0,
                    Arc::new(|_| {
                        BUDGET.with(|slot| slot.borrow().as_ref().unwrap().cancel());
                        Value::error(super::super::EvalError::invalid_argument("cancelled"))
                    }),
                ))
                .with_overload(Overload::new("must_not_retry", false, 0, {
                    let calls = calls.clone();
                    Arc::new(move |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Value::Bool(true)
                    })
                })),
        );
        let parsed = crate::parse("cancel() || true");
        let program = Program::new(
            Arc::new(Ast::new_unchecked(parsed.ast.unwrap(), "cancel() || true")),
            Arc::new(registry),
        );
        let result = program.eval_with_context(&EmptyActivation, &budget);
        assert_eq!(result.result.unwrap_err(), BudgetError::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(result.used, 2);
        BUDGET.with(|slot| *slot.borrow_mut() = None);
    }
}
