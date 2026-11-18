//! Strict YAML source parsing for The Runbook.

pub mod model;
mod parse;

pub use model::{Diagnostic, Metadata, RunbookBody, SourceNode, SourceSpan, Spec};
pub use parse::parse;

pub mod capabilities;
pub use capabilities::{Support, classify, validate_capabilities};

pub mod schema;
pub use schema::{
    DataSchema, DataType, DefaultValue, parse_declarations, parse_schema, resolve_data,
    schema_json, validate_data,
};

pub mod expressions;

mod normalize;
pub use normalize::{MAX_WAIT_MS, derived_title, normalize};
pub mod ir;

mod budget;
pub use budget::estimate_step_runs;

mod scopes;
pub use scopes::trigger_env;

mod semantic;

mod wire;

mod bindings;

mod snapshot;

#[cfg(test)]
mod compiler_storage_tests;

mod policy;

mod decimal;

mod admission;

mod size;

mod compile;
pub use compile::{
    CompileContext, CompiledRunbook, MAX_COMPILE_CONTEXT_BYTES, MAX_SNAPSHOT_BYTES, compile,
    restore,
};

mod context;

pub mod calls;

pub mod authoring;
pub mod messages;

pub mod cron;

pub use scopes::{CheckedSite, CheckedValue};

#[path = "compile/corpus.rs"]
mod compiler_corpus;

pub use policy::validate_policy;

/// Tagged exact JSON values for internal storage, not the public REST wire shape.
/// Reused by execution events so numeric spellings and literal objects survive replay.
pub mod stored_data {
    pub use crate::schema::storage::{deserialize_map as deserialize, serialize_map as serialize};
}

/// Tagged exact JSON value for internal event storage, not public REST JSON.
pub mod stored_value {
    pub use crate::schema::storage::{
        deserialize_value as deserialize, serialize_value as serialize,
    };
}

mod identity_refs;
pub use identity_refs::{
    resolve_approval_escalation, resolve_escalation, resolve_followup_escalation,
};

pub mod render;
