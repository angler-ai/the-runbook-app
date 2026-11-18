//! Catalog search (D22): typed field projection and validation in core;
//! adapters execute the same filters against their indexes.
use crate::{
    StoreError,
    catalog::{RunSummary, TenantScope},
};
use runbook_model::{Data, Diagnostic, ExecutionId, ExecutionStatus, RunbookId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// D22 cloud default; self-hosted operators may configure their own.
pub const DEFAULT_MAX_INDEXED_FIELDS: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldFilter {
    Equals {
        field: String,
        value: Value,
    },
    /// Numeric range, min inclusive and max exclusive; either may be open.
    Range {
        field: String,
        min: Option<Value>,
        max: Option<Value>,
    },
}
impl FieldFilter {
    pub fn field(&self) -> &str {
        match self {
            Self::Equals { field, .. } | Self::Range { field, .. } => field,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunFilters {
    #[serde(default)]
    pub statuses: Vec<ExecutionStatus>,
    pub runbook_id: Option<RunbookId>,
    /// All of these tags must be present.
    #[serde(default)]
    pub tags_all: Vec<String>,
    /// Whole-word text over title and tags (adapter indexes may tokenize).
    pub text: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldFilter>,
    /// Created range, [from, to).
    pub created_from_ms: Option<i64>,
    pub created_to_ms: Option<i64>,
    pub actor: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunPage {
    pub runs: Vec<RunSummary>,
    pub next_cursor: Option<String>,
}

/// UI-1: the run statuses the runbook list counts as active (open runs).
/// The SQL adapters' partial indexes name exactly these labels.
pub const ACTIVE_STATUSES: [ExecutionStatus; 4] = [
    ExecutionStatus::Queued,
    ExecutionStatus::Running,
    ExecutionStatus::Waiting,
    ExecutionStatus::Paused,
];

/// UI-1: one runbook's run facts for the runbook list
/// ([`crate::catalog::CatalogStore::runbook_run_stats`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunbookRunStats {
    /// The newest run in D22 order (created time, then execution ID, both
    /// descending).
    pub last_run: Option<RunSummary>,
    /// Runs in [`ACTIVE_STATUSES`], counting at most the requested cap.
    pub active_runs: usize,
}

/// Opaque cursor bound to scope and filters (D22): a cursor minted for
/// another scope or filter set is rejected, never reinterpreted.
pub fn cursor_for(scope: &TenantScope, filters: &RunFilters, last: &RunSummary) -> String {
    format!(
        "v1.{}.{}.{}",
        last.created_at_ms,
        last.execution_id,
        filters_hash(scope, filters)
    )
}
pub fn cursor_from(
    scope: &TenantScope,
    filters: &RunFilters,
    cursor: &str,
) -> Result<(i64, ExecutionId), StoreError> {
    let invalid = || StoreError::InvalidBatch("request.invalid_cursor");
    let mut parts = cursor.splitn(4, '.');
    if parts.next() != Some("v1") {
        return Err(invalid());
    }
    let created: i64 = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let id: ExecutionId = parts
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if parts.next() != Some(filters_hash(scope, filters).as_str()) || parts.next().is_some() {
        return Err(invalid());
    }
    Ok((created, id))
}
fn filters_hash(scope: &TenantScope, filters: &RunFilters) -> String {
    let canonical = serde_json::to_string(&(scope, filters)).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());
    digest[..8]
        .iter()
        .fold(String::with_capacity(16), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Newest first, then execution ID (D22): true when `run` sorts after the
/// cursor position, so a page can continue without repeats.
pub fn is_after_cursor(run: &RunSummary, cursor: &(i64, ExecutionId)) -> bool {
    run.created_at_ms < cursor.0 || (run.created_at_ms == cursor.0 && run.execution_id < cursor.1)
}
pub fn sort_runs(runs: &mut [RunSummary]) {
    runs.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| b.execution_id.cmp(&a.execution_id))
    });
}

/// The portable filter semantics. Text matches case-insensitively as a
/// substring here; the Postgres adapter tokenizes with tsvector, so shared
/// cases use whole words.
pub fn matches(run: &RunSummary, filters: &RunFilters) -> bool {
    if !filters.statuses.is_empty() && !filters.statuses.contains(&run.status) {
        return false;
    }
    if filters
        .runbook_id
        .as_ref()
        .is_some_and(|id| id != &run.runbook_id)
    {
        return false;
    }
    if !filters.tags_all.iter().all(|tag| run.tags.contains(tag)) {
        return false;
    }
    if let Some(text) = &filters.text {
        let needle = text.to_lowercase();
        let in_title = run.title.to_lowercase().contains(&needle);
        let in_tags = run
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(&needle));
        if !in_title && !in_tags {
            return false;
        }
    }
    if filters
        .created_from_ms
        .is_some_and(|from| run.created_at_ms < from)
        || filters
            .created_to_ms
            .is_some_and(|to| run.created_at_ms >= to)
    {
        return false;
    }
    if filters
        .actor
        .as_ref()
        .is_some_and(|actor| actor != &run.created_by)
    {
        return false;
    }
    filters.fields.iter().all(|filter| match filter {
        FieldFilter::Equals { field, value } => run.fields.get(field) == Some(value),
        FieldFilter::Range { field, min, max } => {
            let Some(number) = run.fields.get(field).and_then(Value::as_f64) else {
                return false;
            };
            min.as_ref()
                .and_then(Value::as_f64)
                .is_none_or(|lo| number >= lo)
                && max
                    .as_ref()
                    .and_then(Value::as_f64)
                    .is_none_or(|hi| number < hi)
        }
    })
}

fn wire(diagnostic: runbook_spec::Diagnostic) -> Diagnostic {
    Diagnostic {
        path: diagnostic.path,
        message: diagnostic
            .message
            .unwrap_or_else(|| diagnostic.code.clone()),
        code: diagnostic.code,
        line: u32::try_from(diagnostic.line).ok().filter(|n| *n > 0),
        column: u32::try_from(diagnostic.column).ok().filter(|n| *n > 0),
    }
}
fn properties(
    schema: &runbook_spec::DataSchema,
) -> Option<&std::collections::BTreeMap<String, runbook_spec::DataSchema>> {
    match &schema.kind {
        runbook_spec::DataType::Object { properties } => Some(properties),
        _ => None,
    }
}

/// Fields declared `indexed: true` in a runbook's fields schema.
pub fn indexed_properties(
    schema: &runbook_spec::DataSchema,
) -> Vec<(&String, &runbook_spec::DataSchema)> {
    properties(schema)
        .map(|props| {
            props
                .iter()
                .filter(|(_, prop)| prop.indexed == Some(true))
                .collect()
        })
        .unwrap_or_default()
}

/// Project only indexed fields for search, typed by the compiler's data
/// rules, under the configured indexed-field policy.
pub fn project_search_fields(
    schema: &runbook_spec::DataSchema,
    fields: &Data,
    max_indexed: usize,
) -> Result<Data, Vec<Diagnostic>> {
    let indexed = indexed_properties(schema);
    if indexed.len() > max_indexed {
        return Err(vec![Diagnostic {
            path: String::new(),
            code: "query.too_many_indexed_fields".into(),
            message: "The runbook declares more indexed fields than policy allows.".into(),
            line: None,
            column: None,
        }]);
    }
    let mut projected = Data::new();
    for (name, prop) in indexed {
        if let Some(value) = fields.get(name) {
            let errors = runbook_spec::validate_data(prop, value);
            if !errors.is_empty() {
                return Err(errors.into_iter().map(wire).collect());
            }
            projected.insert(name.clone(), value.clone());
        }
    }
    Ok(projected)
}

/// Validate a custom-field patch against the compiled fields schema:
/// locked fields reject edits, and the merged result must satisfy the
/// compiler's data rules (types, enums, ranges, unknown fields). Returns
/// the merged fields on success; nothing mutates on failure. B6 wires
/// actor mutations through this.
pub fn validate_field_update(
    schema: &runbook_spec::DataSchema,
    current: &Data,
    patch: &Data,
) -> Result<Data, Vec<Diagnostic>> {
    if let Some(props) = properties(schema) {
        let locked: Vec<Diagnostic> = patch
            .keys()
            .filter(|name| {
                props
                    .get(*name)
                    .is_some_and(|prop| prop.locked == Some(true))
            })
            .map(|name| Diagnostic {
                path: format!("/{name}"),
                code: "data.locked_field".into(),
                message: "This field is locked after start.".into(),
                line: None,
                column: None,
            })
            .collect();
        if !locked.is_empty() {
            return Err(locked);
        }
    }
    let mut merged = current.clone();
    for (name, value) in patch {
        merged.insert(name.clone(), value.clone());
    }
    let object: serde_json::Map<String, Value> = merged.clone().into_iter().collect();
    let errors = runbook_spec::validate_data(schema, &Value::Object(object));
    if errors.is_empty() {
        Ok(merged)
    } else {
        Err(errors.into_iter().map(wire).collect())
    }
}
