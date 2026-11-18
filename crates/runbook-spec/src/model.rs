use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A one-based location in the original YAML source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub code: String,
    /// RFC 6901 JSON pointer into the source document; empty means its root.
    pub path: String,
    pub line: usize,
    pub column: usize,
}

/// Source syntax, before capability checks, normalization or compilation.
#[derive(Clone, Debug)]
pub struct Spec {
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: RunbookBody,
    pub extra: Map<String, Value>,
    pub raw_source: String,
    pub source_spans: BTreeMap<String, SourceSpan>,
    pub(crate) source_json: Value,
}

impl Spec {
    pub fn steps(&self) -> &[SourceNode] {
        &self.spec.steps
    }

    /// Complete JSON-compatible source, including extensions and unknown keys.
    pub fn source_json(&self) -> &Value {
        &self.source_json
    }
}

#[derive(Clone, Debug)]
pub struct Metadata {
    pub id: String,
    pub title: String,
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct RunbookBody {
    pub steps: Vec<SourceNode>,
    pub inputs: Map<String, Value>,
    pub fields: Map<String, Value>,
    pub finally_steps: Vec<SourceNode>,
    /// Execution settings, defaults, outputs, report, agent params and other syntax.
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct SourceNode {
    pub id: String,
    // Control nodes (for example `set`) need not have a title.
    pub title: Option<String>,
    /// All task properties and control bodies remain available to later passes.
    pub fields: Map<String, Value>,
}
