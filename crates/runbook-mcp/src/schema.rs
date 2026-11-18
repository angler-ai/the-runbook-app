//! The tool-argument check (CF7): JSON Schema for exactly the keywords the
//! tool catalog uses, so the server needs no general validator (and the
//! Cloudflare Worker stays under its size limit). `tests/schema_subset.rs`
//! fails if a tool schema uses anything else, and checks this against
//! `jsonschema` on a corpus of arguments.
//!
//! A problem reads `"/claim/id: \"x\" does not match \"^clm_…$\""`: the
//! instance path (a JSON pointer, empty at the root) and the message.
use serde_json::{Map, Value};

/// The validation keywords implemented here.
pub const KEYWORDS: &[&str] = &[
    "type",
    "required",
    "properties",
    "additionalProperties",
    "enum",
    "items",
    "oneOf",
    "pattern",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minProperties",
];
/// Keywords that annotate and never fail a value.
pub const ANNOTATIONS: &[&str] = &["description", "title"];

/// The schema paths (JSON pointers) of keywords this validator does not
/// implement, in `schema` and every subschema it reaches.
pub fn unsupported(schema: &Value) -> Vec<String> {
    let mut out = vec![];
    walk(schema, "", &mut out);
    out
}

fn walk(schema: &Value, at: &str, out: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        if !schema.is_boolean() {
            out.push(at.to_owned());
        }
        return;
    };
    for (key, value) in object {
        let here = format!("{at}/{}", escape(key));
        if !KEYWORDS.contains(&key.as_str()) && !ANNOTATIONS.contains(&key.as_str()) {
            out.push(here);
            continue;
        }
        match key.as_str() {
            "properties" => {
                for (name, sub) in value.as_object().into_iter().flatten() {
                    walk(sub, &format!("{here}/{}", escape(name)), out);
                }
            }
            "items" | "additionalProperties" => walk(value, &here, out),
            "oneOf" => {
                for (i, sub) in value.as_array().into_iter().flatten().enumerate() {
                    walk(sub, &format!("{here}/{i}"), out);
                }
            }
            _ => {}
        }
    }
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Every way `value` fails `schema`, with its instance path. Empty when
/// it is valid. A keyword outside [`KEYWORDS`] fails (closed).
pub fn validate(schema: &Value, value: &Value) -> Vec<String> {
    let mut out = vec![];
    check(schema, value, "", &mut out);
    out
}

fn problem(out: &mut Vec<String>, at: &str, message: String) {
    if at.is_empty() {
        out.push(message);
    } else {
        out.push(format!("{at}: {message}"));
    }
}

fn check(schema: &Value, value: &Value, at: &str, out: &mut Vec<String>) {
    let object = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            return problem(out, at, format!("False schema does not allow {value}"));
        }
        Value::Object(object) => object,
        _ => return problem(out, at, "tool schema error: not a schema".into()),
    };
    for (key, rule) in object {
        match key.as_str() {
            "type" => check_type(rule, value, at, out),
            "required" => {
                if let Some(present) = value.as_object() {
                    for name in rule.as_array().into_iter().flatten() {
                        if let Some(name) = name.as_str()
                            && !present.contains_key(name)
                        {
                            problem(out, at, format!("\"{name}\" is a required property"));
                        }
                    }
                }
            }
            "properties" => {
                if let (Some(present), Some(rules)) = (value.as_object(), rule.as_object()) {
                    for (name, sub) in rules {
                        if let Some(item) = present.get(name) {
                            check(sub, item, &format!("{at}/{}", escape(name)), out);
                        }
                    }
                }
            }
            "additionalProperties" => {
                if let Some(present) = value.as_object() {
                    additional(object, rule, present, at, out);
                }
            }
            "enum" => {
                let options = rule.as_array().map(Vec::as_slice).unwrap_or_default();
                if !options.iter().any(|o| equal(o, value)) {
                    problem(out, at, format!("{value} is not one of {rule}"));
                }
            }
            "items" => {
                if let Some(items) = value.as_array() {
                    for (i, item) in items.iter().enumerate() {
                        check(rule, item, &format!("{at}/{i}"), out);
                    }
                }
            }
            "oneOf" => {
                let options = rule.as_array().map(Vec::as_slice).unwrap_or_default();
                let passing = options
                    .iter()
                    .filter(|sub| validate(sub, value).is_empty())
                    .count();
                if passing == 0 {
                    problem(
                        out,
                        at,
                        format!(
                            "{value} is not valid under any of the schemas listed in the 'oneOf' keyword"
                        ),
                    );
                } else if passing > 1 {
                    problem(
                        out,
                        at,
                        format!(
                            "{value} is valid under more than one of the schemas listed in the 'oneOf' keyword"
                        ),
                    );
                }
            }
            "pattern" => {
                if let (Some(text), Some(pattern)) = (value.as_str(), rule.as_str()) {
                    match regex::Regex::new(pattern) {
                        Ok(re) if re.is_match(text) => {}
                        Ok(_) => problem(out, at, format!("{value} does not match \"{pattern}\"")),
                        Err(_) => problem(out, at, "tool schema error: bad pattern".into()),
                    }
                }
            }
            "minimum" | "maximum" => {
                if let (Some(n), Some(limit)) = (number(value), rule.as_f64()) {
                    if key == "minimum" && n < limit {
                        problem(
                            out,
                            at,
                            format!("{value} is less than the minimum of {rule}"),
                        );
                    }
                    if key == "maximum" && n > limit {
                        problem(
                            out,
                            at,
                            format!("{value} is greater than the maximum of {rule}"),
                        );
                    }
                }
            }
            "minLength" | "maxLength" => {
                if let (Some(text), Some(limit)) = (value.as_str(), rule.as_u64()) {
                    let chars = text.chars().count() as u64;
                    if key == "minLength" && chars < limit {
                        problem(
                            out,
                            at,
                            format!("{value} is shorter than {limit} character{}", plural(limit)),
                        );
                    }
                    if key == "maxLength" && chars > limit {
                        problem(
                            out,
                            at,
                            format!("{value} is longer than {limit} character{}", plural(limit)),
                        );
                    }
                }
            }
            "minProperties" => {
                if let (Some(present), Some(limit)) = (value.as_object(), rule.as_u64())
                    && (present.len() as u64) < limit
                {
                    problem(
                        out,
                        at,
                        format!(
                            "{value} has less than {limit} propert{}",
                            if limit == 1 { "y" } else { "ies" }
                        ),
                    );
                }
            }
            k if ANNOTATIONS.contains(&k) => {}
            other => problem(
                out,
                at,
                format!("tool schema error: unsupported keyword {other}"),
            ),
        }
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn additional(
    object: &Map<String, Value>,
    rule: &Value,
    present: &Map<String, Value>,
    at: &str,
    out: &mut Vec<String>,
) {
    let declared = object.get("properties").and_then(Value::as_object);
    let extra: Vec<&String> = present
        .keys()
        .filter(|k| !declared.is_some_and(|d| d.contains_key(*k)))
        .collect();
    match rule {
        Value::Bool(false) if !extra.is_empty() => {
            let names: Vec<String> = extra.iter().map(|k| format!("'{k}'")).collect();
            let verb = if extra.len() == 1 { "was" } else { "were" };
            problem(
                out,
                at,
                format!(
                    "Additional properties are not allowed ({} {verb} unexpected)",
                    names.join(", ")
                ),
            );
        }
        Value::Object(_) => {
            for key in extra {
                check(rule, &present[key], &format!("{at}/{}", escape(key)), out);
            }
        }
        _ => {}
    }
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64()
}

/// JSON equality with numbers compared by value (`1` equals `1.0`).
fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

fn is_type(name: &str, value: &Value) -> bool {
    match name {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value
                    .as_f64()
                    .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
        }
        _ => false,
    }
}

fn check_type(rule: &Value, value: &Value, at: &str, out: &mut Vec<String>) {
    let names: Vec<&str> = match rule {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    if !names.iter().any(|name| is_type(name, value)) {
        let expected = if names.len() == 1 {
            format!("\"{}\"", names[0])
        } else {
            rule.to_string()
        };
        problem(out, at, format!("{value} is not of type {expected}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_name_the_rule() {
        let s = json!({
            "type": "object",
            "properties": {"n": {"type": "integer", "minimum": 1, "maximum": 3}},
            "required": ["n"],
            "additionalProperties": false
        });
        assert_eq!(
            validate(&s, &json!({})),
            vec!["\"n\" is a required property"]
        );
        assert_eq!(
            validate(&s, &json!({"n": 0})),
            vec!["/n: 0 is less than the minimum of 1"]
        );
        assert_eq!(
            validate(&s, &json!({"n": 1, "a": 1, "b": 2})),
            vec!["Additional properties are not allowed ('a', 'b' were unexpected)"]
        );
        assert_eq!(
            validate(&s, &json!({"n": "1"})),
            vec!["/n: \"1\" is not of type \"integer\""]
        );
    }
}
