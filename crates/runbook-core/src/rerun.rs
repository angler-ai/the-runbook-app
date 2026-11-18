//! B9: a rerun's start context, built from the original run's start fact.
//!
//! PRD §6: the original's resolved inputs (after defaults) and its *initial*
//! field values are validated against the target version's schemas. A new
//! optional input or field with a default takes that default. A newly required
//! input, a changed type, or an input the target no longer declares rejects the
//! rerun with `422 execution.inputs_invalid` and field-level errors. Fields are
//! taken from the start fact, never from later amendments or field updates.
use runbook_engine::{InitialData, StartContext};
use runbook_model::{Data, FieldError};
use runbook_spec::{CompiledRunbook, DataSchema};
use serde_json::Value;

/// The caller-supplied parts of a rerun that do not come from the original run.
#[derive(Clone, Debug, PartialEq)]
pub struct RerunInvocation {
    /// The new run's document URL.
    pub url: String,
    /// The principal starting the rerun, recorded as its creator.
    pub created_by: String,
    /// Project variables as they are now; they are environment, not history.
    pub project_vars: Data,
}

/// A rejected rerun: nothing is created and no quota is spent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RerunRejected {
    pub errors: Vec<FieldError>,
}
impl RerunRejected {
    pub const CODE: &'static str = "execution.inputs_invalid";
    pub const STATUS: u16 = 422;
}

/// Build the start context for a rerun of `original` on `target`, or the field
/// errors that make the original data invalid for that version.
pub fn rerun_context(
    original: &InitialData,
    target: &CompiledRunbook,
    invocation: RerunInvocation,
) -> Result<StartContext, RerunRejected> {
    let mut errors = vec![];
    let inputs = check_data(
        "inputs",
        &target.ir().inputs,
        &original.context.inputs,
        &mut errors,
        |_| RERUN_MESSAGE.into(),
    );
    let fields = check_data(
        "fields",
        &target.ir().fields,
        &original.context.fields,
        &mut errors,
        |_| RERUN_MESSAGE.into(),
    );
    if !errors.is_empty() {
        return Err(RerunRejected { errors });
    }
    Ok(StartContext {
        // An absolute deadline belongs to the original's start; the host
        // sets a fresh one for the rerun (F22).
        limits: runbook_engine::ExecutionLimits {
            hard_deadline_at_ms: None,
            ..original.context.limits
        },
        inputs,
        fields,
        project_vars: invocation.project_vars,
        title: original.context.title.clone(),
        tags: original.context.tags.clone(),
        url: invocation.url,
        created_by: invocation.created_by,
        // LANG4: a rerun of a child runs on its own; no parent waits for it.
        parent: None,
    })
}

const RERUN_MESSAGE: &str = "The original run's data does not satisfy the target version.";

/// Resolve `data` (defaults applied) against `schema`, or push one field
/// error per problem with its path under `root` (`inputs`/`fields`) and the
/// message `message` gives for its `data.*` code. Shared by start and rerun
/// so both answer `execution.inputs_invalid` with the same paths and codes.
pub(crate) fn check_data(
    root: &str,
    schema: &DataSchema,
    data: &Data,
    errors: &mut Vec<FieldError>,
    message: impl Fn(&str) -> String,
) -> Data {
    let value = Value::Object(data.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    match runbook_spec::resolve_data(schema, &value) {
        Ok(Value::Object(resolved)) => resolved.into_iter().collect(),
        Ok(_) => {
            errors.push(FieldError {
                path: root.into(),
                code: "data.type_error".into(),
                message: message("data.type_error"),
            });
            Data::new()
        }
        Err(diagnostics) => {
            errors.extend(diagnostics.into_iter().map(|d| FieldError {
                path: join(root, &d.path),
                message: message(&d.code),
                code: d.code,
            }));
            Data::new()
        }
    }
}

fn join(root: &str, path: &str) -> String {
    let path = path.trim_start_matches(['/', '.']);
    if path.is_empty() {
        root.into()
    } else if path.starts_with('[') {
        format!("{root}{path}")
    } else {
        format!("{root}.{path}")
    }
}
