use cel_core::{
    BudgetError, Env, EvalObserver, EvaluationBudget, EvaluationRuntime, MapActivation,
    ObserverStop, Program, Timestamp, Value,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    sync::Arc,
};

fn program(source: &str) -> Program {
    let env = Env::with_standard_library();
    env.program(&env.parse_only(source).unwrap()).unwrap()
}
fn list(values: Vec<Value>) -> Value {
    Value::List(values.into())
}
fn identity(value: &Value) -> usize {
    let Value::List(values) = value else {
        panic!("expected list")
    };
    Arc::as_ptr(values) as *const Value as usize
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Begin(u64, i64),
    Resolve(u64, i64, String, bool),
    Field(Option<String>, String),
    Index(usize, i64),
    Range(usize),
    Enter(u64, u64),
    Bind(u64, String, usize, usize),
    Leave(u64),
    Select(u64, i64, i64),
}
#[derive(Default)]
struct Probe {
    events: RefCell<Vec<Event>>,
    slots: RefCell<BTreeMap<i64, (u64, String)>>,
    aliases: RefCell<BTreeMap<(u64, String), String>>,
    resolved_aliases: RefCell<Vec<String>>,
    stop_on: Cell<Option<&'static str>>,
    reason: Cell<Option<ObserverStop>>,
}
impl Probe {
    fn event(&self, kind: &'static str, event: Event) -> Result<(), ObserverStop> {
        self.events.borrow_mut().push(event);
        if self.stop_on.get() == Some(kind) {
            Err(self.reason.get().unwrap_or(ObserverStop::ReadLimit))
        } else {
            Ok(())
        }
    }
    fn origin(&self, scope: u64, expr: i64) -> Option<String> {
        self.slots
            .borrow()
            .get(&expr)
            .filter(|(owner, _)| *owner == scope)
            .map(|(_, origin)| origin.clone())
    }
    fn fields(&self) -> Vec<Option<String>> {
        self.events
            .borrow()
            .iter()
            .filter_map(|e| {
                if let Event::Field(origin, _) = e {
                    Some(origin.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    fn balanced(&self) {
        let events = self.events.borrow();
        let enters: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let Event::Enter(scope, _) = e {
                    Some(*scope)
                } else {
                    None
                }
            })
            .collect();
        let leaves: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let Event::Leave(scope) = e {
                    Some(*scope)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(enters, leaves);
        assert!(self.aliases.borrow().is_empty());
    }
}
impl EvalObserver for Probe {
    fn begin_expr(&self, scope: u64, expr: i64) -> Result<(), ObserverStop> {
        self.slots.borrow_mut().remove(&expr);
        self.event("begin", Event::Begin(scope, expr))
    }
    fn resolve(
        &self,
        scope: u64,
        expr: i64,
        name: &str,
        value: &Value,
    ) -> Result<(), ObserverStop> {
        let alias = self.aliases.borrow().get(&(scope, name.into())).cloned();
        if let Some(alias) = &alias {
            self.resolved_aliases.borrow_mut().push(alias.clone());
        }
        self.slots
            .borrow_mut()
            .insert(expr, (scope, alias.unwrap_or_else(|| name.into())));
        self.event(
            "resolve",
            Event::Resolve(scope, expr, name.into(), value.is_error()),
        )
    }
    fn field(
        &self,
        scope: u64,
        expr: i64,
        receiver: i64,
        _value: &Value,
        field: &str,
    ) -> Result<(), ObserverStop> {
        let origin = self.origin(scope, receiver);
        if let Some(origin) = &origin {
            self.slots
                .borrow_mut()
                .insert(expr, (scope, format!("{origin}.{field}")));
        }
        self.event("field", Event::Field(origin, field.into()))
    }
    fn index(
        &self,
        scope: u64,
        expr: i64,
        receiver: i64,
        value: &Value,
        index: i64,
    ) -> Result<(), ObserverStop> {
        if let (Some(origin), Value::List(values)) = (self.origin(scope, receiver), value) {
            let actual = if index < 0 {
                index + values.len() as i64
            } else {
                index
            };
            if actual >= 0 && actual < values.len() as i64 {
                self.slots
                    .borrow_mut()
                    .insert(expr, (scope, format!("{origin}[{actual}]")));
            }
        }
        self.event("index", Event::Index(identity(value), index))
    }
    fn select_result(&self, scope: u64, expr: i64, selected: i64) -> Result<(), ObserverStop> {
        if let Some(origin) = self.origin(scope, selected) {
            self.slots.borrow_mut().insert(expr, (scope, origin));
        }
        self.event("select", Event::Select(scope, expr, selected))
    }
    fn consume_range(&self, _scope: u64, _receiver: i64, list: &Value) -> Result<(), ObserverStop> {
        self.event("range", Event::Range(identity(list)))
    }
    fn enter_scope(&self, scope: u64, parent: u64) -> Result<(), ObserverStop> {
        self.event("enter", Event::Enter(scope, parent))
    }
    fn bind_item(
        &self,
        scope: u64,
        name: &str,
        list: &Value,
        index: usize,
    ) -> Result<(), ObserverStop> {
        self.aliases
            .borrow_mut()
            .insert((scope, name.into()), format!("{}[{index}]", identity(list)));
        self.event(
            "bind",
            Event::Bind(scope, name.into(), identity(list), index),
        )
    }
    fn leave_scope(&self, scope: u64) {
        self.aliases
            .borrow_mut()
            .retain(|(owner, _), _| *owner != scope);
        self.slots
            .borrow_mut()
            .retain(|_, (owner, _)| *owner != scope);
        self.events.borrow_mut().push(Event::Leave(scope));
    }
}

#[test]
fn observer_uses_collection_and_scope_identity() {
    let a = list(vec![Value::Int(1), Value::Int(9)]);
    let b = list(vec![Value::Int(1), Value::Int(8)]);
    let mut activation = MapActivation::new();
    activation.insert("a", a.clone());
    activation.insert("b", b.clone());
    let observer = Probe::default();
    let result = program("a.exists(x, x == 1) && b.exists(x, x == 1)").eval_with_observer(
        &activation,
        &EvaluationBudget::new(1000),
        &observer,
    );
    assert_eq!(result.result.unwrap(), Value::Bool(true));
    assert_ne!(identity(&a), identity(&b));
    assert_eq!(
        *observer.resolved_aliases.borrow(),
        vec![
            format!("{}[0]", identity(&a)),
            format!("{}[0]", identity(&b))
        ]
    );
    let events = observer.events.borrow();
    let scopes: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Event::Enter(id, parent) = e {
                assert_eq!(*parent, 0);
                Some(*id)
            } else {
                None
            }
        })
        .collect();
    assert!(scopes.windows(2).all(|pair| pair[0] < pair[1]));
    drop(events);
    observer.balanced();
    let unused = Probe::default();
    assert_eq!(
        program("a.exists(unused, true)")
            .eval_with_observer(&activation, &EvaluationBudget::new(100), &unused)
            .result
            .unwrap(),
        Value::Bool(true)
    );
    assert!(unused
        .events
        .borrow()
        .iter()
        .any(|e| matches!(e, Event::Bind(_, name, _, _) if name == "unused")));
    assert!(unused.resolved_aliases.borrow().is_empty());
    unused.balanced();
}

#[test]
fn empty_ranges_and_failed_indices_report_the_actual_collection() {
    let a = list(vec![]);
    let b = list(vec![]);
    assert_ne!(identity(&a), identity(&b));
    let mut activation = MapActivation::new();
    activation.insert("a", a.clone());
    activation.insert("b", b.clone());
    let observer = Probe::default();
    assert_eq!(
        program("a.exists(x, true) || b.exists(x, true)")
            .eval_with_observer(&activation, &EvaluationBudget::new(100), &observer)
            .result
            .unwrap(),
        Value::Bool(false)
    );
    let ranges: Vec<_> = observer
        .events
        .borrow()
        .iter()
        .filter_map(|e| {
            if let Event::Range(id) = e {
                Some(*id)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ranges, vec![identity(&a), identity(&b)]);
    assert!(!observer
        .events
        .borrow()
        .iter()
        .any(|e| matches!(e, Event::Bind(..))));
    let index = Probe::default();
    assert!(program("a[7]")
        .eval_with_observer(&activation, &EvaluationBudget::new(100), &index)
        .result
        .unwrap()
        .is_error());
    assert!(index
        .events
        .borrow()
        .contains(&Event::Index(identity(&a), 7)));
    observer.balanced();
}

#[test]
fn null_fields_use_selected_expression_origin_and_clear_reused_ids() {
    let mut activation = MapActivation::new();
    activation.insert("a", Value::Null);
    activation.insert("b", Value::Null);
    for (condition, expected) in [(true, "a"), (false, "b")] {
        activation.insert("condition", Value::Bool(condition));
        let observer = Probe::default();
        program("(condition ? a : b).child").eval_with_observer(
            &activation,
            &EvaluationBudget::new(100),
            &observer,
        );
        assert_eq!(observer.fields(), vec![Some(expected.into())]);
        let other = if condition { "b" } else { "a" };
        assert!(!observer
            .events
            .borrow()
            .iter()
            .any(|e| matches!(e, Event::Resolve(_, _, name, _) if name == other)));
    }
    activation.insert("flags", list(vec![Value::Bool(true), Value::Bool(false)]));
    let repeated = Probe::default();
    program("flags.exists(flag, (flag ? a : null).child == null)").eval_with_observer(
        &activation,
        &EvaluationBudget::new(1000),
        &repeated,
    );
    assert_eq!(repeated.fields(), vec![Some("a".into()), None]);
    repeated.balanced();
    let observer = Probe::default();
    program("a.child").eval_with_observer(&activation, &EvaluationBudget::new(100), &observer);
    program("null.child").eval_with_observer(&activation, &EvaluationBudget::new(100), &observer);
    assert_eq!(observer.fields(), vec![Some("a".into()), None]);
    let absent = Probe::default();
    program("missing").eval_with_observer(&activation, &EvaluationBudget::new(100), &absent);
    assert!(absent
        .events
        .borrow()
        .iter()
        .any(|e| matches!(e, Event::Resolve(_, _, name, true) if name == "missing")));
}

#[test]
fn all_semantic_stops_are_sticky_and_cleanup_is_balanced() {
    let mut activation = MapActivation::new();
    activation.insert("items", list(vec![Value::Null, Value::Null]));
    for (kind, source) in [
        ("begin", "items"),
        ("resolve", "items"),
        ("range", "items.exists(x, true)"),
        ("enter", "items.exists(x, true)"),
        ("bind", "items.exists(x, true)"),
        ("field", "items.exists(x, x.child == null)"),
        ("index", "items[8]"),
        ("select", "true ? items : items"),
    ] {
        for (reason, expected) in [
            (ObserverStop::ReadLimit, BudgetError::ReadLimit),
            (ObserverStop::CostExceeded, BudgetError::CostExceeded),
            (ObserverStop::Cancelled, BudgetError::Cancelled),
        ] {
            let observer = Probe::default();
            observer.stop_on.set(Some(kind));
            observer.reason.set(Some(reason));
            let budget = EvaluationBudget::new(1000);
            assert_eq!(
                program(source)
                    .eval_with_observer(&activation, &budget, &observer)
                    .result
                    .unwrap_err(),
                expected,
                "{kind}"
            );
            observer.balanced();
            let count = observer.events.borrow().len();
            let used = budget.used();
            budget.cancel();
            assert_eq!(
                program("true")
                    .eval_with_observer(&activation, &budget, &observer)
                    .result
                    .unwrap_err(),
                expected
            );
            assert_eq!(observer.events.borrow().len(), count);
            assert_eq!(budget.used(), used);
            // After the stopping event only bounded cleanup may run.
            let events = observer.events.borrow();
            let last_semantic = events
                .iter()
                .rposition(|e| !matches!(e, Event::Leave(_)))
                .unwrap();
            let stopping = &events[last_semantic];
            assert!(match kind {
                "begin" => matches!(stopping, Event::Begin(..)),
                "resolve" => matches!(stopping, Event::Resolve(..)),
                "range" => matches!(stopping, Event::Range(..)),
                "enter" => matches!(stopping, Event::Enter(..)),
                "bind" => matches!(stopping, Event::Bind(..)),
                "field" => matches!(stopping, Event::Field(..)),
                "index" => matches!(stopping, Event::Index(..)),
                "select" => matches!(stopping, Event::Select(..)),
                _ => unreachable!(),
            });
        }
    }
}

#[test]
fn failed_precharges_and_cancellation_precede_callbacks() {
    let mut activation = MapActivation::new();
    activation.insert("items", list(vec![Value::Null]));
    for limit in 0..40 {
        let observer = Probe::default();
        let budget = EvaluationBudget::new(limit);
        program("items.exists(x, x.child == null)").eval_with_observer(
            &activation,
            &budget,
            &observer,
        );
        observer.balanced();
        if budget.is_terminal() {
            let count = observer.events.borrow().len();
            program("items").eval_with_observer(&activation, &budget, &observer);
            assert_eq!(observer.events.borrow().len(), count);
        }
    }
    let observer = Probe::default();
    let budget = EvaluationBudget::new(4);
    assert_eq!(
        program("items[0]")
            .eval_with_observer(&activation, &budget, &observer)
            .result
            .unwrap_err(),
        BudgetError::CostExceeded
    );
    assert!(!observer
        .events
        .borrow()
        .iter()
        .any(|e| matches!(e, Event::Index(..))));
    let observer = Probe::default();
    let budget = EvaluationBudget::new(100);
    budget.cancel();
    program("items").eval_with_observer(&activation, &budget, &observer);
    assert!(observer.events.borrow().is_empty());
}

#[test]
fn runtime_context_and_observer_survive_children() {
    let mut activation = MapActivation::new();
    activation.insert("items", list(vec![Value::Int(1)]));
    let observer = Probe::default();
    let time = Timestamp::new(123, 456);
    let runtime = EvaluationRuntime {
        registry: None,
        event_time: Some(time),
        timestamp_literals: None,
    };
    assert_eq!(
        program("items.exists(x, now() == now())")
            .eval_with_runtime_context_and_observer(
                &activation,
                &EvaluationBudget::new(100),
                &runtime,
                &observer
            )
            .result
            .unwrap(),
        Value::Bool(true)
    );
    observer.balanced();
}

#[derive(Debug, Clone)]
struct Record;
impl cel_core::MessageValue for Record {
    fn type_name(&self) -> &str {
        "Record"
    }
    fn eq_message(&self, _: &dyn cel_core::MessageValue) -> bool {
        false
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn clone_boxed(&self) -> Box<dyn cel_core::MessageValue> {
        Box::new(self.clone())
    }
}
#[derive(Debug, Default)]
struct CountingRegistry(std::sync::atomic::AtomicUsize);
impl cel_core::ProtoTypeResolver for CountingRegistry {
    fn get_field_type(&self, _: &str, _: &str) -> Option<cel_core::CelType> {
        None
    }
    fn has_message(&self, _: &str) -> bool {
        true
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
impl cel_core::ProtoRegistry for CountingRegistry {
    fn construct_message(&self, _: &str, _: &[cel_core::StructFieldValue], _: bool) -> Value {
        unreachable!()
    }
    fn message_field_access(
        &self,
        _: &dyn cel_core::MessageValue,
        _: &str,
        _: bool,
        _: bool,
    ) -> Value {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Value::Null
    }
    fn message_has_field(&self, _: &dyn cel_core::MessageValue, _: &str) -> Value {
        unreachable!()
    }
    fn get_extension_value(
        &self,
        _: &dyn cel_core::MessageValue,
        _: &str,
        _: bool,
        _: bool,
    ) -> Option<Value> {
        None
    }
    fn has_extension(&self, _: &dyn cel_core::MessageValue, _: &str) -> Option<bool> {
        None
    }
}
#[test]
fn field_precharge_and_stop_precede_registry_consumption() {
    let mut activation = MapActivation::new();
    activation.insert("record", Value::Message(Box::new(Record)));
    let registry = CountingRegistry::default();
    let runtime = EvaluationRuntime {
        registry: Some(&registry),
        event_time: None,
        timestamp_literals: None,
    };
    let expression = program("record.child");
    let exhausted = Probe::default();
    assert_eq!(
        expression
            .eval_with_runtime_context_and_observer(
                &activation,
                &EvaluationBudget::new(3),
                &runtime,
                &exhausted
            )
            .result
            .unwrap_err(),
        BudgetError::CostExceeded
    );
    assert!(exhausted.fields().is_empty());
    assert_eq!(registry.0.load(std::sync::atomic::Ordering::Relaxed), 0);
    let stopped = Probe::default();
    stopped.stop_on.set(Some("field"));
    assert_eq!(
        expression
            .eval_with_runtime_context_and_observer(
                &activation,
                &EvaluationBudget::new(100),
                &runtime,
                &stopped
            )
            .result
            .unwrap_err(),
        BudgetError::ReadLimit
    );
    assert_eq!(registry.0.load(std::sync::atomic::Ordering::Relaxed), 0);
    let live = Probe::default();
    let result = expression.eval_with_runtime_context_and_observer(
        &activation,
        &EvaluationBudget::new(4),
        &runtime,
        &live,
    );
    assert_eq!(result.result.unwrap(), Value::Null);
    assert_eq!(result.used, 4);
    assert_eq!(registry.0.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(live.fields(), vec![Some("record".into())]);
}

#[test]
fn cancellation_inside_a_successful_hook_prevents_the_next_callback() {
    struct Cancels<'a> {
        budget: &'a EvaluationBudget,
        probe: Probe,
    }
    impl EvalObserver for Cancels<'_> {
        fn enter_scope(&self, scope: u64, parent: u64) -> Result<(), ObserverStop> {
            self.probe.enter_scope(scope, parent)
        }
        fn bind_item(
            &self,
            scope: u64,
            name: &str,
            list: &Value,
            index: usize,
        ) -> Result<(), ObserverStop> {
            self.probe.bind_item(scope, name, list, index)?;
            self.budget.cancel();
            Ok(())
        }
        fn begin_expr(&self, scope: u64, expr: i64) -> Result<(), ObserverStop> {
            self.probe.begin_expr(scope, expr)
        }
        fn leave_scope(&self, scope: u64) {
            self.probe.leave_scope(scope);
        }
    }
    let mut activation = MapActivation::new();
    activation.insert("items", list(vec![Value::Int(1)]));
    let budget = EvaluationBudget::new(100);
    let observer = Cancels {
        budget: &budget,
        probe: Probe::default(),
    };
    assert_eq!(
        program("items.exists(x, x == 1) || true")
            .eval_with_observer(&activation, &budget, &observer)
            .result
            .unwrap_err(),
        BudgetError::Cancelled
    );
    observer.probe.balanced();
    let events = observer.probe.events.borrow();
    assert!(matches!(events[events.len() - 2], Event::Bind(..)));
    assert!(matches!(events[events.len() - 1], Event::Leave(..)));
}

#[test]
fn derived_ranges_report_membership_before_binding_and_keep_receiver_ids() {
    let empty = list(vec![]);
    let nested = list(vec![empty.clone()]);
    let mut activation = MapActivation::new();
    activation.insert("items", empty.clone());
    activation.insert("nested", nested.clone());
    let mut fields = cel_core::ValueMap::new();
    fields.insert(cel_core::MapKey::String("items".into()), empty.clone());
    activation.insert("record", Value::Map(Arc::new(fields)));
    for source in [
        "record.items.exists(x, true)",
        "nested[0].exists(x, true)",
        "(true ? items : nested).exists(x, true)",
    ] {
        let observer = Probe::default();
        assert_eq!(
            program(source)
                .eval_with_observer(&activation, &EvaluationBudget::new(100), &observer)
                .result
                .unwrap(),
            Value::Bool(false)
        );
        let ranges: Vec<_> = observer
            .events
            .borrow()
            .iter()
            .filter_map(|e| {
                if let Event::Range(id) = e {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(ranges, vec![identity(&empty)], "{source}");
        assert!(!observer
            .events
            .borrow()
            .iter()
            .any(|e| matches!(e, Event::Bind(..))));
        observer.balanced();
    }
    let observer = Probe::default();
    activation.insert("items", list(vec![Value::Bool(false), Value::Bool(false)]));
    program("items.exists(x, x)").eval_with_observer(
        &activation,
        &EvaluationBudget::new(100),
        &observer,
    );
    let events = observer.events.borrow();
    let range = events
        .iter()
        .position(|e| matches!(e, Event::Range(_)))
        .unwrap();
    let bind = events
        .iter()
        .position(|e| matches!(e, Event::Bind(..)))
        .unwrap();
    assert!(range < bind);
    let x_ids: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Event::Resolve(scope, expr, name, _) = e {
                (name == "x").then_some((*scope, *expr))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(x_ids.len(), 2);
    assert_ne!(x_ids[0].0, x_ids[1].0);
    assert_eq!(x_ids[0].1, x_ids[1].1);
}

#[test]
fn null_items_retain_index_and_loop_alias_origins_without_value_matching() {
    let a = list(vec![Value::Null, Value::Null]);
    let b = list(vec![Value::Null]);
    let mut activation = MapActivation::new();
    activation.insert("a", a.clone());
    activation.insert("b", b.clone());
    let indexed = Probe::default();
    program("a[-1].child == null || b[0].child == null").eval_with_observer(
        &activation,
        &EvaluationBudget::new(100),
        &indexed,
    );
    assert_eq!(
        indexed.fields(),
        vec![Some("a[1]".into()), Some("b[0]".into())]
    );
    assert!(indexed
        .events
        .borrow()
        .contains(&Event::Index(identity(&a), -1)));
    let aliases = Probe::default();
    program("a.exists(x, x.child == null)").eval_with_observer(
        &activation,
        &EvaluationBudget::new(100),
        &aliases,
    );
    assert_eq!(
        aliases.fields(),
        vec![
            Some(format!("{}[0]", identity(&a))),
            Some(format!("{}[1]", identity(&a)))
        ]
    );
    aliases.balanced();
    let previous = aliases.resolved_aliases.borrow().len();
    program("x").eval_with_observer(&activation, &EvaluationBudget::new(100), &aliases);
    assert_eq!(aliases.resolved_aliases.borrow().len(), previous);
    assert!(aliases
        .events
        .borrow()
        .iter()
        .any(|event| matches!(event, Event::Resolve(0, _, name, true) if name == "x")));
}

#[test]
fn equality_consumes_operand_identity_and_stops_after_failed_hook() {
    struct Consumption {
        ids: RefCell<Vec<i64>>,
        stop: bool,
    }
    impl EvalObserver for Consumption {
        fn consume_value(&self, scope: u64, expr_id: i64, _: &Value) -> Result<(), ObserverStop> {
            assert_eq!(scope, 0);
            self.ids.borrow_mut().push(expr_id);
            if self.stop {
                Err(ObserverStop::ReadLimit)
            } else {
                Ok(())
            }
        }
    }
    let mut activation = MapActivation::new();
    activation.insert("a", Value::Null);
    for source in ["a == null", "null != a"] {
        let observer = Consumption {
            ids: RefCell::new(vec![]),
            stop: false,
        };
        assert!(program(source)
            .eval_with_observer(&activation, &EvaluationBudget::new(100), &observer)
            .result
            .is_ok());
        assert_eq!(observer.ids.borrow().len(), 2);
        assert_ne!(observer.ids.borrow()[0], observer.ids.borrow()[1]);
        let observer = Consumption {
            ids: RefCell::new(vec![]),
            stop: true,
        };
        let budget = EvaluationBudget::new(100);
        assert_eq!(
            program(source)
                .eval_with_observer(&activation, &budget, &observer)
                .result,
            Err(BudgetError::ReadLimit)
        );
        assert_eq!(observer.ids.borrow().len(), 1);
        let used = budget.used();
        program(source).eval_with_observer(&activation, &budget, &observer);
        assert_eq!(observer.ids.borrow().len(), 1);
        assert_eq!(budget.used(), used);
    }
    let observer = Consumption {
        ids: RefCell::new(vec![]),
        stop: false,
    };
    program("false && a == null").eval_with_observer(
        &activation,
        &EvaluationBudget::new(100),
        &observer,
    );
    assert!(observer.ids.borrow().is_empty());
}
