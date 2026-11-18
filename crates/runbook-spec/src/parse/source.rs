//! Project the already parsed JSON tree without deserializing user map keys as
//! Serde's arbitrary-precision number transport marker.
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::model::{Diagnostic, Metadata, RunbookBody, SourceNode, SourceSpan, Spec};

pub(super) fn build(
    raw: &str,
    value: Value,
    spans: BTreeMap<String, SourceSpan>,
) -> Result<Spec, Diagnostic> {
    project(&value)
        .map(|mut spec| {
            spec.raw_source = raw.to_owned();
            spec.source_spans = spans.clone();
            spec.source_json = value;
            spec
        })
        .map_err(|path| {
            let span = spans
                .get(&path)
                .copied()
                .unwrap_or(SourceSpan { line: 1, column: 1 });
            Diagnostic {
                message: None,
                code: "spec.source_shape".into(),
                path,
                line: span.line,
                column: span.column,
            }
        })
}

fn project(value: &Value) -> Result<Spec, String> {
    let mut root = object(value.clone(), "")?;
    // LANG7: `apiVersion` and `kind` default to the only supported values;
    // an explicit value is still checked (capability validation).
    let api_version = defaulted(
        root.remove("apiVersion"),
        "/apiVersion",
        "therunbook.app/v1",
    )?;
    let kind = defaulted(root.remove("kind"), "/kind", "Runbook")?;
    let mut metadata = object(root.remove("metadata").unwrap_or(Value::Null), "/metadata")?;
    let id = string(metadata.remove("id"), "/metadata/id")?;
    // LANG7: a missing title is derived from the ID.
    let title = match metadata.remove("title") {
        None => crate::derived_title(&id),
        value => string(value, "/metadata/title")?,
    };
    let mut body = object(root.remove("spec").unwrap_or(Value::Null), "/spec")?;
    let steps = nodes(body.remove("steps").unwrap_or(Value::Null), "/spec/steps")?;
    let inputs = optional_object(body.remove("inputs"), "/spec/inputs")?;
    let fields = optional_object(body.remove("fields"), "/spec/fields")?;
    let finally_steps = match body.remove("finally") {
        Some(value) => nodes(value, "/spec/finally")?,
        None => Vec::new(),
    };
    Ok(Spec {
        api_version,
        kind,
        metadata: Metadata {
            id,
            title,
            extra: metadata,
        },
        spec: RunbookBody {
            steps,
            inputs,
            fields,
            finally_steps,
            extra: body,
        },
        extra: root,
        raw_source: String::new(),
        source_spans: BTreeMap::new(),
        source_json: Value::Null,
    })
}

fn object(value: Value, path: &str) -> Result<Map<String, Value>, String> {
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(path.into()),
    }
}

fn optional_object(value: Option<Value>, path: &str) -> Result<Map<String, Value>, String> {
    value.map_or_else(|| Ok(Map::new()), |value| object(value, path))
}

fn string(value: Option<Value>, path: &str) -> Result<String, String> {
    match value {
        Some(Value::String(text)) => Ok(text),
        _ => Err(path.into()),
    }
}

fn defaulted(value: Option<Value>, path: &str, default: &str) -> Result<String, String> {
    match value {
        None => Ok(default.to_owned()),
        value => string(value, path),
    }
}

fn nodes(value: Value, path: &str) -> Result<Vec<SourceNode>, String> {
    let Value::Array(values) = value else {
        return Err(path.into());
    };
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let item_path = format!("{path}/{index}");
            let mut fields = object(value, &item_path)?;
            let id = string(fields.remove("id"), &format!("{item_path}/id"))?;
            let title = match fields.remove("title") {
                None | Some(Value::Null) => None,
                value => Some(string(value, &format!("{item_path}/title"))?),
            };
            Ok(SourceNode { id, title, fields })
        })
        .collect()
}
