//! Exact provenance. Persistent trace state is separate from per-call aliases.
use crate::bindings::{BoundNode, BoundValue, Slot};
use crate::{Bindings, EvalError, Origin, ReadKind, ReadRef, TypeDef};
use cel_core::{BudgetError, EvalObserver, EvaluationBudget, ObserverStop, Value};
use std::{cell::RefCell, sync::Arc};

const MAX_READS: usize = 4096;
const MAX_SLOTS: usize = 256;

#[derive(Debug, Clone, Default)]
pub(crate) struct ReadTracker {
    reads: Vec<ReadRef>,
    limited: bool,
}
impl ReadTracker {
    pub(crate) fn snapshot(&self) -> Vec<ReadRef> {
        self.reads.clone()
    }
    pub(crate) fn is_limited(&self) -> bool {
        self.limited
    }

    /// Used by both semantic observation and T10's bounded output traversal.
    pub(crate) fn record(
        &mut self,
        origin: &Origin,
        kind: ReadKind,
        budget: &EvaluationBudget,
    ) -> Result<(), EvalError> {
        self.record_path(origin, None, kind, budget).map(|_| ())
    }

    pub(crate) fn missing_error(&self, index: usize, budget: &EvaluationBudget) -> EvalError {
        let path = &self.reads[index].origin.path;
        if !budget.charge(path.len() as u64) {
            return budget_error(budget.error().expect("failed charge"));
        }
        EvalError::MissingValue { path: path.clone() }
    }

    fn record_path(
        &mut self,
        origin: &Origin,
        child: Option<&str>,
        kind: ReadKind,
        budget: &EvaluationBudget,
    ) -> Result<usize, EvalError> {
        if let Some(error) = budget.error() {
            return Err(budget_error(error));
        }
        if self.limited {
            return Err(EvalError::ReadLimit);
        }
        let dot = usize::from(child.is_some() && !origin.path.is_empty());
        let length = origin.path.len() + dot + child.map_or(0, str::len);
        if length > 1024 {
            self.limited = true;
            return Err(EvalError::ReadLimit);
        }
        // Compare borrowed pieces before copying any path/identity metadata.
        if let Some(index) = self.reads.iter().position(|read| {
            read.kind == kind
                && read.origin.record == origin.record
                && if let Some(child) = child {
                    read.origin.path.len() == length
                        && read.origin.path.starts_with(&origin.path)
                        && read.origin.path[origin.path.len()..].strip_prefix(if dot == 1 {
                            "."
                        } else {
                            ""
                        }) == Some(child)
                } else {
                    read.origin.path == origin.path
                }
        }) {
            return Ok(index);
        }
        if self.reads.len() == MAX_READS {
            self.limited = true;
            return Err(EvalError::ReadLimit);
        }
        let identity_bytes = origin.record.execution_id.len()
            + origin.record.step_run_id.as_ref().map_or(0, String::len)
            + origin
                .record
                .iteration_path
                .iter()
                .map(|item| item.node_id.len() + item.instance_id.len())
                .sum::<usize>();
        if !budget.charge((1 + length + identity_bytes) as u64) {
            return Err(budget_error(budget.error().expect("failed charge")));
        }
        let mut path = String::with_capacity(length);
        path.push_str(&origin.path);
        if dot == 1 {
            path.push('.');
        }
        if let Some(child) = child {
            path.push_str(child);
        }
        let index = self.reads.len();
        self.reads.push(ReadRef {
            origin: Origin {
                path,
                record: origin.record.clone(),
            },
            kind,
        });
        Ok(index)
    }
}
fn budget_error(error: BudgetError) -> EvalError {
    match error {
        BudgetError::CostExceeded => EvalError::CostExceeded,
        BudgetError::Cancelled => EvalError::Cancelled,
        BudgetError::ReadLimit => EvalError::ReadLimit,
    }
}
fn stop(error: EvalError) -> ObserverStop {
    match error {
        EvalError::ReadLimit => ObserverStop::ReadLimit,
        EvalError::Cancelled => ObserverStop::Cancelled,
        _ => ObserverStop::CostExceeded,
    }
}

struct OriginSlot {
    scope: u64,
    id: i64,
    node: Arc<BoundNode>,
}
struct Alias {
    scope: u64,
    name: [u8; 128],
    len: usize,
    node: Arc<BoundNode>,
}
struct CallState<'a> {
    tracker: &'a mut ReadTracker,
    slots: [Option<OriginSlot>; MAX_SLOTS],
    alias: Option<Alias>,
    missing: Option<usize>,
}
impl CallState<'_> {
    fn node(&self, scope: u64, id: i64) -> Option<Arc<BoundNode>> {
        self.slots
            .iter()
            .flatten()
            .find(|slot| slot.id == id && slot.scope == scope)
            .map(|slot| slot.node.clone())
    }
    fn clear(&mut self, id: i64) {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|slot| slot.id == id) {
                *slot = None;
            }
        }
    }
    fn set(&mut self, scope: u64, id: i64, node: Arc<BoundNode>) -> Result<(), ObserverStop> {
        self.clear(id);
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(ObserverStop::CostExceeded)?;
        *slot = Some(OriginSlot { scope, id, node });
        Ok(())
    }
}

/// Constructed afresh for each expression. No scope/AST identity survives calls.
pub(crate) struct ReadObserver<'a> {
    bindings: &'a Bindings,
    budget: &'a EvaluationBudget,
    state: RefCell<CallState<'a>>,
}
impl<'a> ReadObserver<'a> {
    pub(crate) fn new(
        bindings: &'a Bindings,
        budget: &'a EvaluationBudget,
        tracker: &'a mut ReadTracker,
    ) -> Self {
        Self {
            bindings,
            budget,
            state: RefCell::new(CallState {
                tracker,
                slots: std::array::from_fn(|_| None),
                alias: None,
                missing: None,
            }),
        }
    }
    pub(crate) fn result_source(&self, id: i64) -> Option<Arc<BoundNode>> {
        self.state.borrow().node(0, id)
    }
    pub(crate) fn missing_index(&self) -> Option<usize> {
        self.state.borrow().missing
    }
    fn capture(&self, scope: u64, id: i64, node: Arc<BoundNode>) -> Result<(), ObserverStop> {
        let mut state = self.state.borrow_mut();
        // Null records carry owner information, but are intermediate projections.
        if !matches!(
            self.bindings.schema.type_definition(node.ty),
            Some(TypeDef::Record(_) | TypeDef::List(_))
        ) {
            state
                .tracker
                .record(&node.origin, ReadKind::Value, self.budget)
                .map_err(stop)?;
        }
        state.set(scope, id, node)
    }
    fn absent(&self, enclosing: &Origin, name: &str) -> Result<(), ObserverStop> {
        let mut state = self.state.borrow_mut();
        let index = state
            .tracker
            .record_path(enclosing, Some(name), ReadKind::Absence, self.budget)
            .map_err(stop)?;
        state.missing.get_or_insert(index);
        Ok(())
    }
    fn membership(&self, value: &Value) -> Result<Option<Arc<BoundNode>>, ObserverStop> {
        let Some(node) = self.bindings.cel_list_node(value) else {
            return Ok(None);
        };
        self.state
            .borrow_mut()
            .tracker
            .record(&node.origin, ReadKind::Membership, self.budget)
            .map_err(stop)?;
        Ok(Some(node.clone()))
    }
}
impl EvalObserver for ReadObserver<'_> {
    fn consume_value(&self, scope: u64, id: i64, value: &Value) -> Result<(), ObserverStop> {
        if matches!(value, Value::Error(_)) {
            return Ok(());
        }
        let Some(node) = self.state.borrow().node(scope, id) else {
            return Ok(());
        };
        if matches!(
            self.bindings.schema.type_definition(node.ty),
            Some(TypeDef::Record(_))
        ) {
            self.state
                .borrow_mut()
                .tracker
                .record(&node.origin, ReadKind::Value, self.budget)
                .map_err(stop)?;
        }
        Ok(())
    }

    fn begin_expr(&self, _: u64, id: i64) -> Result<(), ObserverStop> {
        self.state.borrow_mut().clear(id);
        Ok(())
    }
    fn resolve(&self, scope: u64, id: i64, name: &str, _: &Value) -> Result<(), ObserverStop> {
        let alias = self
            .state
            .borrow()
            .alias
            .as_ref()
            .filter(|alias| alias.scope == scope && &alias.name[..alias.len] == name.as_bytes())
            .map(|alias| alias.node.clone());
        if let Some(node) = alias {
            return self.capture(scope, id, node);
        }
        match self.bindings.root(name) {
            Some(Slot::Present(node)) => self.capture(scope, id, node.clone()),
            Some(Slot::Missing {
                enclosing, name, ..
            }) => self.absent(enclosing, name),
            None => Ok(()), // The compiler's hygienic accumulator has no declared root.
        }
    }
    fn field(
        &self,
        scope: u64,
        id: i64,
        receiver_id: i64,
        receiver: &Value,
        field: &str,
    ) -> Result<(), ObserverStop> {
        if matches!(receiver, Value::Error(_)) {
            return Ok(());
        }
        let Some(node) = self.state.borrow().node(scope, receiver_id) else {
            return Ok(());
        };
        match &node.value {
            BoundValue::Record { fields, .. } => {
                if let Some(child) = fields.get(field) {
                    self.capture(scope, id, child.clone())
                } else {
                    self.absent(&node.origin, field)
                }
            }
            BoundValue::Null
                if matches!(
                    self.bindings.schema.type_definition(node.ty),
                    Some(TypeDef::Record(_))
                ) =>
            {
                self.absent(&node.origin, field)
            }
            _ => Ok(()),
        }
    }
    fn index(
        &self,
        scope: u64,
        id: i64,
        _: i64,
        list: &Value,
        index: i64,
    ) -> Result<(), ObserverStop> {
        let Some(node) = self.membership(list)? else {
            return Ok(());
        };
        let BoundValue::List(items) = &node.value else {
            return Ok(());
        };
        let actual = if index < 0 {
            index + items.len() as i64
        } else {
            index
        };
        if let Ok(index) = usize::try_from(actual)
            && let Some(item) = items.get(index)
        {
            self.capture(scope, id, item.clone())?;
        }
        Ok(())
    }
    fn select_result(&self, scope: u64, id: i64, selected: i64) -> Result<(), ObserverStop> {
        let node = self.state.borrow().node(scope, selected);
        if let Some(node) = node {
            self.state.borrow_mut().set(scope, id, node)?;
        }
        Ok(())
    }
    fn consume_range(&self, _: u64, _: i64, list: &Value) -> Result<(), ObserverStop> {
        self.membership(list).map(|_| ())
    }
    fn bind_item(
        &self,
        scope: u64,
        name: &str,
        list: &Value,
        index: usize,
    ) -> Result<(), ObserverStop> {
        let Some(node) = self.bindings.cel_list_node(list) else {
            return Ok(());
        };
        let BoundValue::List(items) = &node.value else {
            return Ok(());
        };
        let Some(node) = items.get(index) else {
            return Ok(());
        };
        if name.len() > 128 {
            return Err(ObserverStop::CostExceeded);
        }
        let mut bytes = [0; 128];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        self.state.borrow_mut().alias = Some(Alias {
            scope,
            name: bytes,
            len: name.len(),
            node: node.clone(),
        });
        Ok(())
    }
    fn leave_scope(&self, scope: u64) {
        let mut state = self.state.borrow_mut();
        if state
            .alias
            .as_ref()
            .is_some_and(|alias| alias.scope == scope)
        {
            state.alias = None;
        }
        for slot in &mut state.slots {
            if slot.as_ref().is_some_and(|slot| slot.scope == scope) {
                *slot = None;
            }
        }
    }
}

#[cfg(test)]
mod tests;
