//! Private storage representation. Decoding does NOT certify executable IR.
#![allow(dead_code)]
use crate::{
    DataSchema,
    bindings::Bindings,
    ir::NormalizedRunbook,
    wire::{self, Error, Limits},
};

pub(crate) struct SnapshotRef<'a> {
    pub ir: &'a NormalizedRunbook,
    pub bindings: &'a Bindings,
    pub project_variables: &'a DataSchema,
    pub hash: [u8; 32],
    pub worst_case_step_runs: u64,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnvalidatedSnapshot {
    format: u8,
    profile: String,
    cost_model: String,
    pub ir: NormalizedRunbook,
    pub bindings: Bindings,
    pub project_variables: DataSchema,
    pub hash: [u8; 32],
    pub worst_case_step_runs: u64,
}
// This bound protects recursive serde conversion even when callers supply larger
// wire limits. It is not the public snapshot byte/value admission policy.
const TYPED_DEPTH: usize = 512;
#[derive(serde::Serialize)]
struct Envelope<'a> {
    format: u8,
    profile: &'static str,
    cost_model: &'static str,
    ir: &'a NormalizedRunbook,
    bindings: &'a Bindings,
    project_variables: &'a DataSchema,
    hash: [u8; 32],
    worst_case_step_runs: u64,
}
/// Inputs must already satisfy source/context admission; this is not validation.
pub(crate) fn encode(snapshot: SnapshotRef<'_>, limits: Limits) -> Result<Vec<u8>, Error> {
    let tree = serde_json::to_value(Envelope {
        format: 1,
        profile: runbook_expr::PROFILE,
        cost_model: runbook_expr::COST_MODEL,
        ir: snapshot.ir,
        bindings: snapshot.bindings,
        project_variables: snapshot.project_variables,
        hash: snapshot.hash,
        worst_case_step_runs: snapshot.worst_case_step_runs,
    })
    .map_err(|_| Error::Invalid)?;
    wire::encode(
        &tree,
        Limits {
            depth: limits.depth.min(TYPED_DEPTH),
            ..limits
        },
    )
}
pub(crate) fn decode(bytes: &[u8], limits: Limits) -> Result<UnvalidatedSnapshot, Error> {
    let tree = wire::decode(
        bytes,
        Limits {
            depth: limits.depth.min(TYPED_DEPTH),
            ..limits
        },
    )?;
    let snapshot: UnvalidatedSnapshot =
        serde_json::from_value(tree.into_value()).map_err(|_| Error::Invalid)?;
    if snapshot.format != 1
        || snapshot.profile != runbook_expr::PROFILE
        || snapshot.cost_model != runbook_expr::COST_MODEL
    {
        return Err(Error::Invalid);
    }
    Ok(snapshot)
}
#[cfg(test)]
#[path = "snapshot/tests.rs"]
mod tests;

#[path = "snapshot/corpus.rs"]
pub(crate) mod corpus;
