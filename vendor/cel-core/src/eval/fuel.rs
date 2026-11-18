//! Reusable deterministic accounting for the separately enforced bounded profile.
use super::Value;
use std::cell::Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostExceeded;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetError {
    ReadLimit,
    CostExceeded,
    Cancelled,
}
#[derive(Debug)]
pub struct MeteredResult {
    pub result: Result<Value, BudgetError>,
    pub used: u64,
    pub iterations: u64,
}

#[derive(Debug, Clone)]
pub struct EvaluationBudget {
    limit: u64,
    used: Cell<u64>,
    iterations: Cell<u64>,
    terminal: Cell<Option<BudgetError>>,
}
impl EvaluationBudget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: Cell::new(0),
            iterations: Cell::new(0),
            terminal: Cell::new(None),
        }
    }
    pub fn charge(&self, units: u64) -> bool {
        if self.is_terminal() {
            return false;
        }
        if units > self.limit - self.used.get() {
            self.terminal.set(Some(BudgetError::CostExceeded));
            return false;
        }
        self.used.set(self.used.get() + units);
        true
    }
    pub fn visit(&self) -> bool {
        if !self.charge(1) {
            return false;
        }
        self.iterations.set(self.iterations.get() + 1);
        true
    }
    pub fn is_terminal(&self) -> bool {
        self.terminal.get().is_some()
    }
    pub fn error(&self) -> Option<BudgetError> {
        self.terminal.get()
    }
    pub fn used(&self) -> u64 {
        self.used.get()
    }
    pub fn iterations(&self) -> u64 {
        self.iterations.get()
    }
    pub(crate) fn stop(&self, error: BudgetError) {
        if !self.is_terminal() {
            self.terminal.set(Some(error));
        }
    }
    pub fn cancel(&self) {
        self.stop(BudgetError::Cancelled);
    }
    pub fn finish(&self, value: Value) -> MeteredResult {
        MeteredResult {
            result: if let Some(error) = self.error() {
                Err(error)
            } else {
                Ok(value)
            },
            used: self.used.get(),
            iterations: self.iterations.get(),
        }
    }
}
