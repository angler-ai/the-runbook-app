//! Org-scoped step target resolution before project authorization.
use crate::StoreError;
use runbook_model::{ExecutionId, OrganizationId, ProjectId, StepRunId};
use std::future::Future;

pub trait StepRunLookup {
    fn step_run_execution(
        &self,
        org_id: &OrganizationId,
        step_run_id: &StepRunId,
    ) -> impl Future<Output = Result<Option<(ExecutionId, ProjectId)>, StoreError>>;
}

/// Stable execution-specific IDs. SequenceIds is a fixture helper; production
/// callers must not give two executions the same step URL. Hashing uses no clock
/// or randomness and remains identical on native and Wasm replay. Public so
/// Workers hosts (the ExecutionDO, CF2) build the same IDs as native.
pub struct ExecutionStepIds {
    execution_id: ExecutionId,
    next: u128,
}
impl ExecutionStepIds {
    /// IDs for work committed after `after_sequence` (the head revision).
    pub fn new(execution_id: &ExecutionId, after_sequence: u64) -> Self {
        Self {
            execution_id: execution_id.clone(),
            next: u128::from(after_sequence) + 1,
        }
    }
}
impl crate::runtime::IdSource for ExecutionStepIds {
    fn next_step_run_id(&mut self) -> StepRunId {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"runbook.step-id.v1\0");
        hash.update(self.execution_id.as_str().as_bytes());
        hash.update(self.next.to_be_bytes());
        self.next += 1;
        let digest = hash.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        let mut number = u128::from_be_bytes(bytes);
        let mut encoded = [b'0'; 26];
        for byte in encoded.iter_mut().rev() {
            *byte = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[(number & 31) as usize];
            number >>= 5;
        }
        let body: String = encoded.into_iter().map(char::from).collect();
        format!("sr_{body}").parse().expect("128-bit Crockford ID")
    }
}
