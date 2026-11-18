//! Durable API writes: directory CAS, project variables and POST replay commit together.
use crate::directory::*;
use runbook_model::{OrganizationId, ProjectId, ProjectVariables};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredResponse {
    pub status: u16,
    pub body: Value,
    pub etag: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestReplay {
    pub principal: String,
    pub operation: String,
    pub key: String,
    pub payload: Value,
}
#[derive(Clone, Debug)]
pub struct DirectoryOperation {
    pub commit: DirectoryCommit,
    pub variables: Option<VariablesWrite>,
    pub replay: Option<RequestReplay>,
    pub response: StoredResponse,
}
#[derive(Clone, Debug, PartialEq)]
pub enum OperationResult {
    Applied(StoredResponse),
    Conflict,
    KeyReused,
    VariablesConflict,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayResult {
    Missing,
    Applied(StoredResponse),
    Conflict,
    KeyReused,
}
pub trait DirectoryOperations: DirectoryStore {
    /// Reauthorize via the expected directory revision before replay, even when
    /// the new payload is invalid. A missing entry is checked again at commit.
    fn lookup_replay(
        &self,
        org: &OrganizationId,
        expected_revision: u64,
        replay: &RequestReplay,
    ) -> impl Future<Output = Result<ReplayResult, DirectoryError>>;

    fn current_time_ms(&self) -> impl Future<Output = Result<i64, DirectoryError>>;
    fn project_variables(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> impl Future<Output = Result<VariablesRecord, DirectoryError>>;
    /// Fences the snapshot used for authorization before reading a replay entry.
    fn apply_operation(
        &self,
        operation: DirectoryOperation,
    ) -> impl Future<Output = Result<OperationResult, DirectoryError>>;
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VariablesRecord {
    pub revision: u64,
    pub variables: ProjectVariables,
}
#[derive(Clone, Debug)]
pub struct VariablesWrite {
    pub project_id: ProjectId,
    pub expected_revision: u64,
    pub variables: ProjectVariables,
}
