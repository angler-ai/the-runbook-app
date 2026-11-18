//! Public query grammar, translated to the shared D22 indexed search contract.
use crate::executions::ExecutionApiError as Error;
use runbook_core::search::{FieldFilter, RunFilters};
use serde_json::Value;
use std::collections::BTreeMap;
pub struct SearchQuery {
    pub filters: RunFilters,
    pub runbook: Option<String>,
    pub cursor: Option<String>,
    pub limit: usize,
}
pub fn parse_query(query: &BTreeMap<String, String>) -> Result<SearchQuery, Error> {
    let bad = || Error::new(400, "request.invalid_query");
    let mut parsed = SearchQuery {
        filters: RunFilters::default(),
        runbook: None,
        cursor: None,
        limit: 100,
    };
    let mut ranges: BTreeMap<String, (Option<Value>, Option<Value>)> = BTreeMap::new();
    for (key, value) in query {
        match key.as_str() {
            "status" => {
                parsed.filters.statuses = value
                    .split(',')
                    .map(|s| serde_json::from_value(Value::String(s.into())).map_err(|_| bad()))
                    .collect::<Result<_, _>>()?;
                parsed.filters.statuses.sort_by_key(|s| format!("{s:?}"));
                parsed.filters.statuses.dedup();
            }
            "tags" => {
                parsed.filters.tags_all = value.split(',').map(str::to_owned).collect();
                parsed.filters.tags_all.sort();
                parsed.filters.tags_all.dedup();
            }
            "runbook" => parsed.runbook = Some(value.clone()),
            "actor" => parsed.filters.actor = Some(value.clone()),
            "text" => parsed.filters.text = Some(value.clone()),
            "from" => parsed.filters.created_from_ms = Some(value.parse().map_err(|_| bad())?),
            "to" => parsed.filters.created_to_ms = Some(value.parse().map_err(|_| bad())?),
            "cursor" => parsed.cursor = Some(value.clone()),
            "limit" => {
                parsed.limit = value.parse().map_err(|_| bad())?;
                if !(1..=100).contains(&parsed.limit) {
                    return Err(bad());
                }
            }
            _ => {
                let field = key
                    .strip_prefix("field.")
                    .filter(|s| !s.is_empty())
                    .ok_or_else(bad)?;
                let value: Value = serde_json::from_str(value).map_err(|_| bad())?;
                if let Some((field, op)) = field
                    .rsplit_once('.')
                    .filter(|(_, op)| matches!(*op, "min" | "max"))
                {
                    if field.is_empty() || !value.is_number() {
                        return Err(bad());
                    }
                    let range = ranges.entry(field.into()).or_default();
                    if op == "min" {
                        range.0 = Some(value);
                    } else {
                        range.1 = Some(value);
                    }
                } else {
                    if value.is_object() || value.is_array() || value.is_null() {
                        return Err(bad());
                    }
                    parsed.filters.fields.push(FieldFilter::Equals {
                        field: field.into(),
                        value,
                    });
                }
            }
        }
    }
    if parsed
        .filters
        .created_from_ms
        .zip(parsed.filters.created_to_ms)
        .is_some_and(|(a, b)| a >= b)
    {
        return Err(bad());
    }
    for (field, (min, max)) in ranges {
        if min
            .as_ref()
            .and_then(Value::as_f64)
            .zip(max.as_ref().and_then(Value::as_f64))
            .is_some_and(|(a, b)| a >= b)
        {
            return Err(bad());
        }
        parsed
            .filters
            .fields
            .push(FieldFilter::Range { field, min, max });
    }
    Ok(parsed)
}
