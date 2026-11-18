//! Borrowed semantic events for the separately validated bounded profile.
//!
//! Arbitrary implementations are not a work bound. The sealed runbook adapter
//! must bound its metadata, reads and cleanup; these hooks do not make general
//! CEL callbacks, optional operations or other out-of-profile paths safe.
use super::{BudgetError, Value};
use std::cell::Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserverStop {
    ReadLimit,
    CostExceeded,
    Cancelled,
}
impl From<ObserverStop> for BudgetError {
    fn from(stop: ObserverStop) -> Self {
        match stop {
            ObserverStop::ReadLimit => Self::ReadLimit,
            ObserverStop::CostExceeded => Self::CostExceeded,
            ObserverStop::Cancelled => Self::Cancelled,
        }
    }
}

/// Events are delivered only while the shared evaluation budget is live.
/// Expression IDs are AST IDs, reused by loop iterations; scope 0 is the root.
/// Scope IDs increase within one evaluation and restart for the next call.
/// Consumers must clear expression origin slots at `begin_expr`, propagate only
/// the selected branch, and never identify primitive origins by value equality.
pub trait EvalObserver {
    fn begin_expr(&self, _scope: u64, _expr_id: i64) -> Result<(), ObserverStop> {
        Ok(())
    }
    fn resolve(
        &self,
        _scope: u64,
        _expr_id: i64,
        _name: &str,
        _value: &Value,
    ) -> Result<(), ObserverStop> {
        Ok(())
    }
    /// An equality operand is consumed as a value, including record presence.
    /// Unlike member projection, comparing a record with null depends on its
    /// own revision. The expression ID preserves binding-derived null origins.
    fn consume_value(
        &self,
        _scope: u64,
        _expr_id: i64,
        _value: &Value,
    ) -> Result<(), ObserverStop> {
        Ok(())
    }
    fn field(
        &self,
        _scope: u64,
        _expr_id: i64,
        _receiver_expr_id: i64,
        _receiver: &Value,
        _field: &str,
    ) -> Result<(), ObserverStop> {
        Ok(())
    }
    fn index(
        &self,
        _scope: u64,
        _expr_id: i64,
        _receiver_expr_id: i64,
        _list: &Value,
        _index: i64,
    ) -> Result<(), ObserverStop> {
        Ok(())
    }
    fn select_result(
        &self,
        _scope: u64,
        _expr_id: i64,
        _selected_expr_id: i64,
    ) -> Result<(), ObserverStop> {
        Ok(())
    }
    fn consume_range(
        &self,
        _scope: u64,
        _receiver_expr_id: i64,
        _list: &Value,
    ) -> Result<(), ObserverStop> {
        Ok(())
    }
    fn enter_scope(&self, _scope: u64, _parent: u64) -> Result<(), ObserverStop> {
        Ok(())
    }
    fn bind_item(
        &self,
        _scope: u64,
        _name: &str,
        _list: &Value,
        _index: usize,
    ) -> Result<(), ObserverStop> {
        Ok(())
    }
    /// Always paired with an attempted enter, including when enter returns Err.
    /// Infallible cleanup only: remove bounded private aliases/origin slots.
    /// Must not allocate, record reads, evaluate, charge fuel or fail. This is
    /// the only callback permitted after terminal failure.
    fn leave_scope(&self, _scope: u64) {}
}

pub(super) struct NoopObserver;
impl EvalObserver for NoopObserver {}
pub(super) static NOOP_OBSERVER: NoopObserver = NoopObserver;

pub(super) struct Observation<'a> {
    pub observer: &'a dyn EvalObserver,
    pub next_scope: Cell<u64>,
}
/// RAII pairs every enter attempt with exactly one bounded cleanup on all exits.
pub(super) struct ScopeGuard<'a> {
    pub scope: u64,
    pub observer: Option<&'a dyn EvalObserver>,
}
impl Drop for ScopeGuard<'_> {
    fn drop(&mut self) {
        if let Some(observer) = self.observer {
            observer.leave_scope(self.scope);
        }
    }
}
