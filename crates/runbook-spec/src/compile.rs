//! Immutable compiler boundary; publication policy is a separate validation pass.
use crate::{DataSchema, Diagnostic, Spec, ir::NormalizedRunbook};
use runbook_model::{TeamId, UserId};
use std::collections::BTreeMap;

pub const MAX_COMPILE_CONTEXT_BYTES: usize = 1_048_576;
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
#[derive(Debug)]
pub struct CompileContext {
    pub expression_profile: String,
    pub skills: BTreeMap<String, [u8; 32]>,
    pub users: BTreeMap<String, UserId>,
    pub teams: BTreeMap<String, TeamId>,
    pub project_variables: DataSchema,
    /// LANG4: callable runbook versions by [`crate::calls::call_key`].
    pub runbooks: BTreeMap<String, crate::ir::CallResolution>,
}
impl Default for CompileContext {
    fn default() -> Self {
        Self {
            expression_profile: runbook_expr::PROFILE.into(),
            skills: BTreeMap::new(),
            users: BTreeMap::new(),
            teams: BTreeMap::new(),
            project_variables: crate::parse_declarations(&serde_json::json!({}))
                .expect("empty declarations"),
            runbooks: BTreeMap::new(),
        }
    }
}
/// Checked immutable data, created only by compile or restore.
///
/// ```compile_fail
/// fn mutate(compiled: &mut runbook_spec::CompiledRunbook) {
///     compiled.ir().steps.clear();
/// }
/// ```
/// ```compile_fail
/// let _: runbook_spec::CompiledRunbook = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Debug)]
pub struct CompiledRunbook {
    ir: NormalizedRunbook,
    hash: [u8; 32],
    worst_case_step_runs: u64,
    snapshot: Vec<u8>,
    bindings: crate::bindings::Bindings,
    project_variables: DataSchema,
    scopes: crate::scopes::CheckedScopes,
}
impl CompiledRunbook {
    pub fn skill_hashes(&self) -> &BTreeMap<String, [u8; 32]> {
        &self.bindings.skills
    }
    pub fn users(&self) -> &BTreeMap<String, UserId> {
        &self.bindings.users
    }
    pub fn teams(&self) -> &BTreeMap<String, TeamId> {
        &self.bindings.teams
    }
    pub fn project_variables(&self) -> &DataSchema {
        &self.project_variables
    }
    pub fn checked_value(&self, path: &str) -> Option<&crate::CheckedValue> {
        self.scopes.value_at(path)
    }
    pub fn checked_site(&self, index: usize) -> Option<crate::CheckedSite<'_>> {
        self.scopes.site(index)
    }
    pub fn environment(&self) -> &runbook_expr::TypeEnv {
        self.scopes.base_environment()
    }
    pub fn checked_sites(&self) -> impl Iterator<Item = crate::scopes::CheckedSite<'_>> {
        self.scopes.sites()
    }
    pub fn initialization_order(&self) -> &[String] {
        self.scopes.initialization_order()
    }
    /// The publish-time limits a version published before them may pass,
    /// checked when a new run (or a call's child) starts on it: at most 256
    /// outputs with or without a `report` (CF13b). A restored version past
    /// them still loads, so its existing runs keep working.
    pub fn admit_new_run(&self) -> Result<(), Vec<Diagnostic>> {
        crate::scopes::outputs_limit(&self.ir).map_err(|e| vec![e])
    }
    pub fn ir(&self) -> &NormalizedRunbook {
        &self.ir
    }
    pub fn hash(&self) -> &[u8; 32] {
        &self.hash
    }
    pub fn worst_case_step_runs(&self) -> u64 {
        self.worst_case_step_runs
    }
    /// LANG4: what a runbook calling this version needs to know about it.
    pub fn call_resolution(
        &self,
        runbook_id: &str,
        version_id: &str,
        version: u32,
    ) -> crate::ir::CallResolution {
        let mut resolution = crate::calls::resolution(&self.ir, runbook_id, version_id, version);
        for (name, kind) in &mut resolution.outputs {
            if let Some(value) = self.checked_value(&format!(
                "/spec/outputs/{}",
                name.replace('~', "~0").replace('/', "~1")
            )) {
                *kind = crate::calls::output_kind(self, value);
            }
        }
        resolution
    }
    pub fn snapshot_bytes(&self) -> &[u8] {
        &self.snapshot
    }
}
fn error(code: &str) -> Vec<Diagnostic> {
    vec![Diagnostic {
        code: code.into(),
        message: None,
        path: String::new(),
        line: 0,
        column: 0,
    }]
}
fn limits() -> crate::wire::Limits {
    crate::wire::Limits {
        bytes: MAX_SNAPSHOT_BYTES,
        values: 2_000_000,
        depth: 512,
    }
}
fn pins<'a>(
    bindings: &'a crate::bindings::Bindings,
    project: &'a DataSchema,
) -> crate::semantic::SelectedPins<'a> {
    crate::semantic::SelectedPins {
        skills: &bindings.skills,
        users: &bindings.users,
        teams: &bindings.teams,
        project_variables: project,
    }
}
fn finish(
    ir: NormalizedRunbook,
    bindings: crate::bindings::Bindings,
    project_variables: DataSchema,
    spans: &BTreeMap<String, crate::SourceSpan>,
    expected: Option<([u8; 32], u64)>,
) -> Result<CompiledRunbook, Vec<Diagnostic>> {
    let scopes = crate::scopes::check(&ir, spans, &project_variables).map_err(|e| vec![e])?;
    let worst_case_step_runs = crate::estimate_step_runs(&ir).map_err(|e| vec![e])?;
    let hash = crate::semantic::hash(&ir, &pins(&bindings, &project_variables));
    if let Some((claimed_hash, claimed_budget)) = expected {
        if hash != claimed_hash {
            return Err(error("spec.snapshot_hash_mismatch"));
        }
        if worst_case_step_runs != claimed_budget {
            return Err(error("spec.snapshot_budget_mismatch"));
        }
    }
    let snapshot = crate::snapshot::encode(
        crate::snapshot::SnapshotRef {
            ir: &ir,
            bindings: &bindings,
            project_variables: &project_variables,
            hash,
            worst_case_step_runs,
        },
        limits(),
    )
    .map_err(|_| error("spec.snapshot_limit"))?;
    Ok(CompiledRunbook {
        ir,
        hash,
        worst_case_step_runs,
        snapshot,
        bindings,
        project_variables,
        scopes,
    })
}
/// Compile immutable typed semantics. A6 publication policy remains a separate
/// validation pass; this function performs no lookup I/O or runtime evaluation.
pub fn compile(spec: &Spec, context: &CompileContext) -> Result<CompiledRunbook, Vec<Diagnostic>> {
    if context.expression_profile != runbook_expr::PROFILE {
        return Err(error("spec.unsupported_profile"));
    }
    let mut ir = crate::normalize(spec)?;
    // Publish-time only (CF13b): `restore` keeps admitting a version
    // published before the cap; a new run on one is refused instead
    // (`CompiledRunbook::admit_new_run`).
    crate::scopes::outputs_limit(&ir)
        .map_err(|e| vec![crate::scopes::locate(e, &spec.source_spans)])?;
    crate::calls::resolve(&mut ir, &context.runbooks)?;
    let bindings = crate::bindings::select(
        &ir,
        &crate::bindings::Catalog {
            skills: &context.skills,
            users: &context.users,
            teams: &context.teams,
        },
    )
    .map_err(|e| vec![e])?;
    crate::context::admit(&bindings, &context.project_variables)?;
    crate::admission::project_source(&context.project_variables)?;
    finish(
        ir,
        bindings,
        context.project_variables.clone(),
        &spec.source_spans,
        None,
    )
}
/// Restore checked semantics using only pinned facts. The stored hash is checked
/// for consistency, not authenticity; callers verify the expected catalog hash.
pub fn restore(bytes: &[u8]) -> Result<CompiledRunbook, Vec<Diagnostic>> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(error("spec.snapshot_limit"));
    }
    let stored = crate::snapshot::decode(bytes, limits()).map_err(|e| {
        error(if e == crate::wire::Error::Limit {
            "spec.snapshot_limit"
        } else {
            "spec.invalid_snapshot"
        })
    })?;
    crate::context::admit(&stored.bindings, &stored.project_variables)?;
    crate::admission::project_source(&stored.project_variables)?;
    let ir = crate::admission::validate_ir(stored.ir)?;
    // Rebuild the target lookup from pinned references, never a live catalog.
    let mut targets = BTreeMap::new();
    for (reference, hash) in &stored.bindings.skills {
        let target = if reference.starts_with("skill://") {
            reference
        } else {
            ir.skills
                .get(reference)
                .ok_or_else(|| error("spec.snapshot_pins_mismatch"))?
        };
        if let Some(old) = targets.insert(target.clone(), *hash)
            && old != *hash
        {
            return Err(error("spec.snapshot_pins_mismatch"));
        }
    }
    let selected = crate::bindings::select(
        &ir,
        &crate::bindings::Catalog {
            skills: &targets,
            users: &stored.bindings.users,
            teams: &stored.bindings.teams,
        },
    )
    .map_err(|e| vec![e])?;
    if selected != stored.bindings {
        return Err(error("spec.snapshot_pins_mismatch"));
    }
    finish(
        ir,
        selected,
        stored.project_variables,
        &BTreeMap::new(),
        Some((stored.hash, stored.worst_case_step_runs)),
    )
}

#[cfg(test)]
#[path = "compile/tests.rs"]
mod tests;
