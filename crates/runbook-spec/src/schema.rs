//! Recursive, strict data declarations independent of expression evaluation.
//!
//! Construct schemas with the parse functions; serde stores already compiled
//! schemas. Objects are closed and no supported type accepts null. `min`/`max`
//! measure numeric values, Unicode scalar counts, or list item counts. Integer
//! values must be exactly representable as i64, including integral decimal
//! spellings. Numbers lie within ±1.7976931348623157e308; later f64 expression
//! bindings may round or underflow, while validation compares decimal bounds
//! exactly and resolution preserves the supplied JSON number.
//!
//! Diagnostics use dotted ASCII identifiers, `[index]` for array elements and
//! JSON-quoted brackets for other keys, for example `services[0]["a.b"]`.
//! Source locations are zero because this layer does not have source spans.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value, json};

use crate::Diagnostic;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataSchema {
    pub kind: DataType,
    pub required: bool,
    pub default: DefaultValue,
    pub min: Option<Number>,
    pub max: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// Source metadata; neither flag constrains a data value.
    pub indexed: Option<bool>,
    pub locked: Option<bool>,
    #[serde(
        serialize_with = "storage::serialize_map",
        deserialize_with = "storage::deserialize_map"
    )]
    pub extensions: BTreeMap<String, Value>,
}

/// Missing and explicit JSON null remain distinct in stored schemas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DefaultValue {
    Missing,
    Value(
        #[serde(
            serialize_with = "storage::serialize_value",
            deserialize_with = "storage::deserialize_value"
        )]
        Value,
    ),
}

// serde_json's arbitrary_precision mode uses a private map marker for numbers.
// Storing arbitrary Value directly would reinterpret user objects carrying that
// key. Tag every stored value kind so literal objects remain literal objects.
pub(crate) mod storage {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Serialize, Deserialize)]
    enum StoredValue {
        Null,
        Bool(bool),
        Number(Number),
        String(String),
        Array(Vec<StoredValue>),
        Object(BTreeMap<String, StoredValue>),
    }

    impl From<&Value> for StoredValue {
        fn from(value: &Value) -> Self {
            match value {
                Value::Null => Self::Null,
                Value::Bool(value) => Self::Bool(*value),
                Value::Number(value) => Self::Number(value.clone()),
                Value::String(value) => Self::String(value.clone()),
                Value::Array(values) => Self::Array(values.iter().map(Self::from).collect()),
                Value::Object(values) => Self::Object(
                    values
                        .iter()
                        .map(|(key, value)| (key.clone(), Self::from(value)))
                        .collect(),
                ),
            }
        }
    }

    impl From<StoredValue> for Value {
        fn from(value: StoredValue) -> Self {
            match value {
                StoredValue::Null => Self::Null,
                StoredValue::Bool(value) => Self::Bool(value),
                StoredValue::Number(value) => Self::Number(value),
                StoredValue::String(value) => Self::String(value),
                StoredValue::Array(values) => {
                    Self::Array(values.into_iter().map(Self::from).collect())
                }
                StoredValue::Object(values) => Self::Object(
                    values
                        .into_iter()
                        .map(|(key, value)| (key, Self::from(value)))
                        .collect(),
                ),
            }
        }
    }

    pub fn serialize_value<S: Serializer>(value: &Value, serializer: S) -> Result<S::Ok, S::Error> {
        StoredValue::from(value).serialize(serializer)
    }

    pub fn deserialize_value<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Value, D::Error> {
        StoredValue::deserialize(deserializer).map(Value::from)
    }

    pub fn serialize_optional_value<S: Serializer>(
        value: &Option<Value>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_ref().map(StoredValue::from).serialize(serializer)
    }
    pub fn deserialize_optional_value<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Value>, D::Error> {
        Option::<StoredValue>::deserialize(deserializer).map(|v| v.map(Value::from))
    }
    pub fn serialize_optional_object<S: Serializer>(
        value: &Option<serde_json::Map<String, Value>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .map(|map| {
                StoredValue::Object(
                    map.iter()
                        .map(|(k, v)| (k.clone(), StoredValue::from(v)))
                        .collect(),
                )
            })
            .serialize(serializer)
    }
    pub fn deserialize_optional_object<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<serde_json::Map<String, Value>>, D::Error> {
        match deserialize_optional_value(deserializer)? {
            None => Ok(None),
            Some(Value::Object(map)) => Ok(Some(map)),
            Some(_) => Err(serde::de::Error::custom("expected stored object")),
        }
    }

    pub fn serialize_map<S: Serializer>(
        values: &BTreeMap<String, Value>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let stored: BTreeMap<_, _> = values
            .iter()
            .map(|(key, value)| (key, StoredValue::from(value)))
            .collect();
        stored.serialize(serializer)
    }

    pub fn deserialize_map<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<String, Value>, D::Error> {
        BTreeMap::<String, StoredValue>::deserialize(deserializer).map(|values| {
            values
                .into_iter()
                .map(|(key, value)| (key, Value::from(value)))
                .collect()
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum DataType {
    String,
    Text,
    Number,
    Integer,
    Boolean,
    Enum {
        options: Vec<String>,
    },
    MultiEnum {
        options: Vec<String>,
    },
    List {
        items: Box<DataSchema>,
    },
    Object {
        properties: BTreeMap<String, DataSchema>,
    },
    Date,
    DateTime,
    Duration,
    Url,
    Email,
    User,
}

/// Identifiers use dotted paths; other names use JSON-quoted bracket notation.
fn property_path(path: &str, name: &str) -> String {
    let mut chars = name.chars();
    let simple = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if simple {
        if path.is_empty() {
            name.into()
        } else {
            format!("{path}.{name}")
        }
    } else {
        format!(
            "{path}[{}]",
            serde_json::to_string(name).expect("string serialization")
        )
    }
}

fn diagnostic(code: &str, path: &str) -> Diagnostic {
    Diagnostic {
        message: None,
        code: code.into(),
        path: path.into(),
        line: 0,
        column: 0,
    }
}

/// Root is depth zero; a property or list item adds one level.
pub const MAX_DATA_DEPTH: usize = 64;
pub const MAX_SCHEMA_DECLARATIONS: usize = 4096;

// Iterative preflight runs before recursive cloning or validation, including
// unknown fields and opaque extension/default data.
fn check_value_depth(
    value: &Value,
    path: &str,
    depth: usize,
    code: &str,
) -> Result<(), Diagnostic> {
    let mut pending = vec![(value, path.to_owned(), depth)];
    while let Some((value, path, depth)) = pending.pop() {
        if depth > MAX_DATA_DEPTH {
            return Err(diagnostic(code, &path));
        }
        match value {
            Value::Object(map) => {
                for (name, value) in map.iter().rev() {
                    pending.push((value, property_path(&path, name), depth + 1));
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate().rev() {
                    pending.push((value, format!("{path}[{index}]"), depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn check_schema_tree(value: &Value, is_map: bool) -> Result<(), Vec<Diagnostic>> {
    let mut pending = Vec::new();
    let mut count = usize::from(is_map);
    if is_map {
        if let Some(map) = value.as_object() {
            for (name, value) in map.iter().rev() {
                pending.push((value, property_path("", name), 1));
            }
        }
    } else {
        pending.push((value, String::new(), 0));
    }
    while let Some((value, path, depth)) = pending.pop() {
        if depth > MAX_DATA_DEPTH {
            return Err(vec![diagnostic("spec.depth_limit", &path)]);
        }
        count += 1;
        if count > MAX_SCHEMA_DECLARATIONS {
            return Err(vec![diagnostic("spec.schema_limit", &path)]);
        }
        let Some(map) = value.as_object() else {
            continue;
        };
        for (key, value) in map {
            if key.starts_with("x-") || key == "default" {
                check_value_depth(value, &property_path(&path, key), 0, "spec.depth_limit")
                    .map_err(|error| vec![error])?;
            }
        }
        if let Some(items) = map.get("items") {
            pending.push((items, property_path(&path, "items"), depth + 1));
        }
        if let Some(properties) = map.get("properties").and_then(Value::as_object) {
            let path = property_path(&path, "properties");
            for (name, value) in properties.iter().rev() {
                pending.push((value, property_path(&path, name), depth + 1));
            }
        }
    }
    Ok(())
}

pub const MAX_PATTERN_CHARS: usize = 1024;
pub const MAX_PATTERN_BYTES: usize = 64 * 1024;

fn empty_schema(kind: DataType) -> DataSchema {
    DataSchema {
        kind,
        required: false,
        default: DefaultValue::Missing,
        min: None,
        max: None,
        pattern: None,
        title: None,
        description: None,
        indexed: None,
        locked: None,
        extensions: BTreeMap::new(),
    }
}

/// Parse a closed object whose properties are named source declarations.
pub fn parse_declarations(value: &Value) -> Result<DataSchema, Vec<Diagnostic>> {
    check_schema_tree(value, true)?;
    declarations(value, "")
}

fn declarations(value: &Value, path: &str) -> Result<DataSchema, Vec<Diagnostic>> {
    let map = value
        .as_object()
        .ok_or_else(|| vec![diagnostic("spec.invalid_schema", path)])?;
    let mut properties = BTreeMap::new();
    let mut errors = Vec::new();
    for (name, value) in map {
        match declaration(value, &property_path(path, name)) {
            Ok(schema) => {
                properties.insert(name.clone(), schema);
            }
            Err(mut diagnostics) => errors.append(&mut diagnostics),
        }
    }
    if errors.is_empty() {
        Ok(empty_schema(DataType::Object { properties }))
    } else {
        Err(errors)
    }
}

/// Parse one declaration, rejecting malformed or inapplicable constraints.
pub fn parse_schema(value: &Value) -> Result<DataSchema, Vec<Diagnostic>> {
    check_schema_tree(value, false)?;
    declaration(value, "")
}

fn declaration(value: &Value, path: &str) -> Result<DataSchema, Vec<Diagnostic>> {
    let invalid = |key: &str| {
        vec![diagnostic(
            "spec.invalid_schema",
            &if key.is_empty() {
                path.into()
            } else {
                property_path(path, key)
            },
        )]
    };
    let map = value.as_object().ok_or_else(|| invalid(""))?;
    for key in map.keys() {
        if !key.starts_with("x-")
            && !matches!(
                key.as_str(),
                "type"
                    | "required"
                    | "default"
                    | "min"
                    | "max"
                    | "title"
                    | "description"
                    | "indexed"
                    | "locked"
                    | "options"
                    | "items"
                    | "properties"
                    | "pattern"
            )
        {
            return Err(invalid(key));
        }
    }
    let name = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("type"))?;
    let kind = match name {
        "string" => DataType::String,
        "text" => DataType::Text,
        "number" => DataType::Number,
        "integer" => DataType::Integer,
        "boolean" => DataType::Boolean,
        "enum" => {
            let values = map
                .get("options")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("options"))?;
            let mut options = Vec::new();
            let mut seen = BTreeSet::new();
            for value in values {
                let option = value.as_str().ok_or_else(|| invalid("options"))?.to_owned();
                if !seen.insert(option.clone()) {
                    return Err(invalid("options"));
                }
                options.push(option);
            }
            if options.is_empty() {
                return Err(invalid("options"));
            }
            DataType::Enum { options }
        }
        "multi_enum" => {
            let values = map
                .get("options")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("options"))?;
            let mut options = Vec::new();
            let mut seen = BTreeSet::new();
            for value in values {
                let option = value.as_str().ok_or_else(|| invalid("options"))?.to_owned();
                if !seen.insert(option.clone()) {
                    return Err(invalid("options"));
                }
                options.push(option);
            }
            if options.is_empty() {
                return Err(invalid("options"));
            }
            DataType::MultiEnum { options }
        }
        "list" => DataType::List {
            items: Box::new(declaration(
                map.get("items").ok_or_else(|| invalid("items"))?,
                &property_path(path, "items"),
            )?),
        },
        "object" => match map.get("properties") {
            Some(value) => declarations(value, &property_path(path, "properties"))?.kind,
            None => DataType::Object {
                properties: BTreeMap::new(),
            },
        },
        "date" => DataType::Date,
        "datetime" => DataType::DateTime,
        "duration" => DataType::Duration,
        "url" => DataType::Url,
        "email" => DataType::Email,
        "user" => DataType::User,
        _ => {
            return Err(vec![diagnostic(
                "spec.unsupported_feature",
                &property_path(path, "type"),
            )]);
        }
    };
    for (key, applicable) in [
        ("options", matches!(name, "enum" | "multi_enum")),
        ("items", name == "list"),
        ("properties", name == "object"),
        (
            "pattern",
            matches!(
                name,
                "string" | "text" | "date" | "datetime" | "duration" | "url" | "email" | "user"
            ),
        ),
    ] {
        if map.contains_key(key) && !applicable {
            return Err(invalid(key));
        }
    }
    let mut schema = empty_schema(kind);
    if let Some(value) = map.get("pattern") {
        let pattern = value.as_str().ok_or_else(|| invalid("pattern"))?;
        if pattern.chars().count() > MAX_PATTERN_CHARS {
            return Err(vec![diagnostic(
                "spec.pattern_limit",
                &property_path(path, "pattern"),
            )]);
        }
        let anchored = format!(r"\A(?:{pattern})\z");
        match regex::RegexBuilder::new(&anchored)
            .size_limit(MAX_PATTERN_BYTES)
            .build()
        {
            Ok(_) => schema.pattern = Some(pattern.to_owned()),
            Err(regex::Error::CompiledTooBig(_)) => {
                return Err(vec![diagnostic(
                    "spec.pattern_limit",
                    &property_path(path, "pattern"),
                )]);
            }
            Err(_) => {
                return Err(vec![diagnostic(
                    "spec.invalid_pattern",
                    &property_path(path, "pattern"),
                )]);
            }
        }
    }
    for key in ["required", "indexed", "locked"] {
        if let Some(value) = map.get(key) {
            let value = value.as_bool().ok_or_else(|| invalid(key))?;
            match key {
                "required" => schema.required = value,
                "indexed" => schema.indexed = Some(value),
                _ => schema.locked = Some(value),
            }
        }
    }
    for key in ["title", "description"] {
        if let Some(value) = map.get(key) {
            let value = value.as_str().ok_or_else(|| invalid(key))?.to_owned();
            if key == "title" {
                schema.title = Some(value);
            } else {
                schema.description = Some(value);
            }
        }
    }
    for key in ["min", "max"] {
        if let Some(value) = map.get(key) {
            let bound = value
                .as_number()
                .filter(|n| n.as_f64().is_some_and(f64::is_finite))
                .ok_or_else(|| invalid(key))?;
            if !matches!(name, "number" | "integer" | "string" | "text" | "list")
                || (matches!(name, "string" | "text" | "list") && bound.as_u64().is_none())
            {
                return Err(invalid(key));
            }
            if key == "min" {
                schema.min = Some(bound.clone());
            } else {
                schema.max = Some(bound.clone());
            }
        }
    }
    if let (Some(min), Some(max)) = (&schema.min, &schema.max)
        && number_cmp(min, max).is_gt()
    {
        return Err(invalid("min"));
    }
    schema.extensions = map
        .iter()
        .filter(|(key, _)| key.starts_with("x-"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if let Some(value) = map.get("default") {
        if !validate_data(&schema, value).is_empty() {
            return Err(invalid("default"));
        }
        schema.default = DefaultValue::Value(value.clone());
    }
    Ok(schema)
}

// Compare JSON decimal values without rounding large integers through f64.
fn number_cmp(left: &Number, right: &Number) -> std::cmp::Ordering {
    use crate::decimal::shifted_exponent;
    fn exponent_cmp(left: &(bool, String), right: &(bool, String)) -> std::cmp::Ordering {
        if left.0 != right.0 {
            return if left.0 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let order = left
            .1
            .len()
            .cmp(&right.1.len())
            .then_with(|| left.1.cmp(&right.1));
        if left.0 { order.reverse() } else { order }
    }
    fn decimal(number: &Number) -> (bool, String, (bool, String)) {
        let text = number.to_string();
        let negative = text.starts_with('-');
        let text = text.trim_start_matches('-');
        let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
        let fraction = mantissa
            .split_once('.')
            .map_or(0, |(_, fraction)| fraction.len());
        let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
        let digits = digits.trim_start_matches('0').to_owned();
        if digits.is_empty() {
            return (false, "0".into(), (false, "0".into()));
        }
        let magnitude = shifted_exponent(exponent, digits.len() as i128 - fraction as i128);
        (negative, digits, magnitude)
    }
    let (ln, ld, lm) = decimal(left);
    let (rn, rd, rm) = decimal(right);
    if ln != rn {
        return if ln {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
    }
    let ordering = if ld == "0" || rd == "0" {
        (ld != "0").cmp(&(rd != "0"))
    } else {
        exponent_cmp(&lm, &rm).then_with(|| {
            let length = ld.len().max(rd.len());
            ld.bytes()
                .chain(std::iter::repeat(b'0'))
                .take(length)
                .cmp(rd.bytes().chain(std::iter::repeat(b'0')).take(length))
        })
    };
    if ln { ordering.reverse() } else { ordering }
}

/// Resolve absent properties only. Explicit null is validated as supplied.
pub fn resolve_data(schema: &DataSchema, value: &Value) -> Result<Value, Vec<Diagnostic>> {
    fn fill(
        schema: &DataSchema,
        value: &Value,
        path: &str,
        depth: usize,
    ) -> Result<Value, Diagnostic> {
        if depth > MAX_DATA_DEPTH {
            return Err(diagnostic("data.depth_limit", path));
        }
        match (&schema.kind, value) {
            (DataType::Object { properties }, Value::Object(map)) => {
                let mut result = map.clone();
                for (name, property) in properties {
                    let child_path = property_path(path, name);
                    let value = match (map.get(name), &property.default) {
                        (Some(value), _) => Some(value),
                        (None, DefaultValue::Value(value)) => {
                            check_value_depth(value, &child_path, depth + 1, "data.depth_limit")?;
                            Some(value)
                        }
                        _ => None,
                    };
                    if let Some(value) = value {
                        result.insert(name.clone(), fill(property, value, &child_path, depth + 1)?);
                    }
                }
                Ok(Value::Object(result))
            }
            (DataType::List { items }, Value::Array(values)) => {
                let values = values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        fill(items, value, &format!("{path}[{index}]"), depth + 1)
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Value::Array(values))
            }
            _ => Ok(value.clone()),
        }
    }
    check_value_depth(value, "", 0, "data.depth_limit").map_err(|error| vec![error])?;
    let resolved = fill(schema, value, "", 0).map_err(|error| vec![error])?;
    let errors = validate_data(schema, &resolved);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

fn is_valid_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let year: u32 = match s[0..4].parse() {
        Ok(y) => y,
        Err(_) => return false,
    };
    let month: u32 = match s[5..7].parse() {
        Ok(m) if (1..=12).contains(&m) => m,
        _ => return false,
    };
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let max_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    matches!(s[8..10].parse::<u32>(), Ok(d) if (1..=max_days).contains(&d))
}

fn is_valid_datetime(s: &str) -> bool {
    if s.len() < 20 {
        return false;
    }
    if !is_valid_date(&s[..10]) {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes[10] != b'T' && bytes[10] != b't' {
        return false;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return false;
    }
    let hour: u32 = match s[11..13].parse() {
        Ok(h) if h < 24 => h,
        _ => return false,
    };
    let _ = hour;
    let minute: u32 = match s[14..16].parse() {
        Ok(m) if m < 60 => m,
        _ => return false,
    };
    let _ = minute;
    let second: u32 = match s[17..19].parse() {
        Ok(sec) if sec <= 60 => sec,
        _ => return false,
    };
    let _ = second;
    let mut rest = &s[19..];
    if rest.starts_with('.') {
        rest = &rest[1..];
        let frac_len = rest.bytes().take_while(u8::is_ascii_digit).count();
        if frac_len == 0 {
            return false;
        }
        rest = &rest[frac_len..];
    }
    if rest == "Z" || rest == "z" {
        return true;
    }
    if rest.len() == 6
        && (rest.starts_with('+') || rest.starts_with('-'))
        && rest.as_bytes()[3] == b':'
    {
        let tz_hour: u32 = match rest[1..3].parse() {
            Ok(h) if h < 24 => h,
            _ => return false,
        };
        let _ = tz_hour;
        let tz_min: u32 = match rest[4..6].parse() {
            Ok(m) if m < 60 => m,
            _ => return false,
        };
        let _ = tz_min;
        return true;
    }
    false
}

fn is_valid_duration(s: &str) -> bool {
    crate::normalize::parse_duration_millis(s).is_some()
}

fn is_valid_url(s: &str) -> bool {
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    let Some((scheme, rest)) = s.split_once("://") else {
        return false;
    };
    let mut scheme_chars = scheme.chars();
    let Some(first) = scheme_chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    if !scheme_chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return false;
    }
    !rest.is_empty()
}

fn is_valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || local.len() > 64 || domain.len() > 255 {
        return false;
    }
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return false;
    }
    if !local.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '.' | '!'
                    | '#'
                    | '$'
                    | '%'
                    | '&'
                    | '\''
                    | '*'
                    | '+'
                    | '-'
                    | '/'
                    | '='
                    | '?'
                    | '^'
                    | '_'
                    | '`'
                    | '{'
                    | '|'
                    | '}'
                    | '~'
            )
    }) {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    for label in labels {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
    }
    true
}

fn is_valid_user(s: &str) -> bool {
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        return false;
    }
    if s.parse::<runbook_model::UserId>().is_ok() {
        return true;
    }
    let candidate = s.strip_prefix("user:").unwrap_or(s);
    !candidate.is_empty()
        && candidate
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@'))
}

/// Validate supplied values without injecting defaults or changing the source.
pub fn validate_data(schema: &DataSchema, value: &Value) -> Vec<Diagnostic> {
    fn visit(
        schema: &DataSchema,
        value: &Value,
        path: &str,
        depth: usize,
        errors: &mut Vec<Diagnostic>,
    ) {
        if depth > MAX_DATA_DEPTH {
            errors.push(diagnostic("data.depth_limit", path));
            return;
        }
        match &schema.kind {
            DataType::Integer if integer_value(value).is_some() => {}
            DataType::Number
                if value.as_number().is_some_and(|number| {
                    number_cmp(number, &Number::from_f64(-f64::MAX).unwrap()).is_ge()
                        && number_cmp(number, &Number::from_f64(f64::MAX).unwrap()).is_le()
                }) => {}
            DataType::String | DataType::Text if value.is_string() => {}
            DataType::Boolean if value.is_boolean() => {}
            DataType::Enum { options } if value.is_string() => {
                if !options.iter().any(|option| value == option) {
                    errors.push(diagnostic("data.enum", path));
                }
            }
            DataType::MultiEnum { options } if value.is_array() => {
                let mut seen = BTreeSet::new();
                for (index, item) in value.as_array().unwrap().iter().enumerate() {
                    let item_path = format!("{path}[{index}]");
                    if let Some(s) = item.as_str() {
                        if !options.iter().any(|opt| opt == s) || !seen.insert(s) {
                            errors.push(diagnostic("data.enum", &item_path));
                        }
                    } else {
                        errors.push(diagnostic("data.type_error", &item_path));
                    }
                }
            }
            DataType::Date if value.is_string() => {
                if !is_valid_date(value.as_str().unwrap()) {
                    errors.push(diagnostic("data.format", path));
                }
            }
            DataType::DateTime if value.is_string() => {
                if !is_valid_datetime(value.as_str().unwrap()) {
                    errors.push(diagnostic("data.format", path));
                }
            }
            DataType::Duration if value.is_string() => {
                if !is_valid_duration(value.as_str().unwrap()) {
                    errors.push(diagnostic("data.format", path));
                }
            }
            DataType::Url if value.is_string() => {
                if !is_valid_url(value.as_str().unwrap()) {
                    errors.push(diagnostic("data.format", path));
                }
            }
            DataType::Email if value.is_string() => {
                if !is_valid_email(value.as_str().unwrap()) {
                    errors.push(diagnostic("data.format", path));
                }
            }
            DataType::User if value.is_string() => {
                if !is_valid_user(value.as_str().unwrap()) {
                    errors.push(diagnostic("data.format", path));
                }
            }
            DataType::List { items } if value.is_array() => {
                for (index, value) in value.as_array().unwrap().iter().enumerate() {
                    visit(items, value, &format!("{path}[{index}]"), depth + 1, errors);
                }
            }
            DataType::Object { properties } if value.is_object() => {
                for (name, property) in properties {
                    let child_path = property_path(path, name);
                    if let Some(value) = value.get(name) {
                        visit(property, value, &child_path, depth + 1, errors);
                    } else if property.required {
                        errors.push(diagnostic("data.required", &child_path));
                    }
                }
                for name in value.as_object().unwrap().keys() {
                    if !properties.contains_key(name) {
                        errors.push(diagnostic("data.unknown_field", &property_path(path, name)));
                    }
                }
            }
            _ => {
                errors.push(diagnostic("data.type_error", path));
                return;
            }
        }
        if let Some(pattern) = &schema.pattern
            && let Some(text) = value.as_str()
        {
            let anchored = format!(r"\A(?:{pattern})\z");
            if let Ok(re) = regex::Regex::new(&anchored)
                && !re.is_match(text)
            {
                errors.push(diagnostic("data.pattern", path));
            }
        }
        let measured = match (&schema.kind, value) {
            (DataType::String | DataType::Text, Value::String(text)) => {
                Some(Number::from(text.chars().count()))
            }
            (DataType::List { .. }, Value::Array(items)) => Some(Number::from(items.len())),
            (DataType::Number | DataType::Integer, Value::Number(number)) => Some(number.clone()),
            _ => None,
        };
        if let Some(value) = measured
            && (schema
                .min
                .as_ref()
                .is_some_and(|min| number_cmp(&value, min).is_lt())
                || schema
                    .max
                    .as_ref()
                    .is_some_and(|max| number_cmp(&value, max).is_gt()))
        {
            errors.push(diagnostic("data.range", path));
        }
    }
    if let Err(error) = check_value_depth(value, "", 0, "data.depth_limit") {
        return vec![error];
    }
    let mut errors = Vec::new();
    visit(schema, value, "", 0, &mut errors);
    errors
}

/// Exact integer conversion for later typed expression bindings. Integral
/// decimal/exponent syntax is accepted; fractions and i64 overflow are rejected.
pub fn integer_value(value: &Value) -> Option<i64> {
    let text = value.as_number()?.to_string();
    let negative = text.starts_with('-');
    let text = text.trim_start_matches('-');
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    let without_zeros = digits.trim_end_matches('0');
    let zeroes = digits.len() - without_zeros.len();
    let exponent = exponent.parse::<i128>().ok()?;
    let scale = exponent
        .checked_sub(fraction as i128)?
        .checked_add(zeroes as i128)?;
    if !(0..=19).contains(&scale) || without_zeros.len() as i128 + scale > 19 {
        return None;
    }
    let magnitude = format!("{}{}", without_zeros, "0".repeat(scale as usize));
    if negative {
        format!("-{magnitude}").parse().ok()
    } else {
        magnitude.parse().ok()
    }
}

/// JSON Schema 2020-12 for supported value validation. Default is an annotation:
/// consumers must call `resolve_data` to apply it. Operational metadata and
/// opaque extensions are intentionally absent from the value schema.
pub fn schema_json(schema: &DataSchema) -> Value {
    fn emit(schema: &DataSchema, depth: usize) -> Value {
        if depth > MAX_DATA_DEPTH {
            return Value::Bool(false);
        }
        let mut result = match &schema.kind {
            DataType::String | DataType::Text => json!({"type": "string"}),
            DataType::Number => json!({"type": "number"}),
            DataType::Integer => json!({"type": "integer"}),
            DataType::Boolean => json!({"type": "boolean"}),
            DataType::Enum { options } => json!({"type": "string", "enum": options}),
            DataType::MultiEnum { options } => json!({
                "type": "array",
                "items": {"type": "string", "enum": options},
                "uniqueItems": true
            }),
            DataType::List { items } => json!({"type": "array", "items": emit(items, depth + 1)}),
            DataType::Object { properties } => {
                let emitted: serde_json::Map<String, Value> = properties
                    .iter()
                    .map(|(key, value)| (key.clone(), emit(value, depth + 1)))
                    .collect();
                let required: Vec<&str> = properties
                    .iter()
                    .filter(|(_, value)| value.required)
                    .map(|(key, _)| key.as_str())
                    .collect();
                let mut value =
                    json!({"type": "object", "properties": emitted, "additionalProperties": false});
                if !required.is_empty() {
                    value["required"] = json!(required);
                }
                value
            }
            DataType::Date => json!({"type": "string", "format": "date"}),
            DataType::DateTime => json!({"type": "string", "format": "date-time"}),
            DataType::Duration => json!({"type": "string", "format": "duration"}),
            DataType::Url => json!({"type": "string", "format": "uri"}),
            DataType::Email => json!({"type": "string", "format": "email"}),
            DataType::User => json!({"type": "string"}),
        };
        let domain = match schema.kind {
            DataType::Integer => Some((Number::from(i64::MIN), Number::from(i64::MAX))),
            DataType::Number => Some((
                Number::from_f64(-f64::MAX).unwrap(),
                Number::from_f64(f64::MAX).unwrap(),
            )),
            _ => None,
        };
        let (minimum, maximum) = match domain {
            Some((min, max)) => (
                Some(
                    schema
                        .min
                        .as_ref()
                        .filter(|bound| number_cmp(bound, &min).is_gt())
                        .unwrap_or(&min)
                        .clone(),
                ),
                Some(
                    schema
                        .max
                        .as_ref()
                        .filter(|bound| number_cmp(bound, &max).is_lt())
                        .unwrap_or(&max)
                        .clone(),
                ),
            ),
            None => (schema.min.clone(), schema.max.clone()),
        };
        let (min_key, max_key) = match schema.kind {
            DataType::String | DataType::Text => ("minLength", "maxLength"),
            DataType::List { .. } | DataType::MultiEnum { .. } => ("minItems", "maxItems"),
            _ => ("minimum", "maximum"),
        };
        if let Some(value) = minimum {
            result[min_key] = Value::Number(value);
        }
        if let Some(value) = maximum {
            result[max_key] = Value::Number(value);
        }
        if let Some(pattern) = &schema.pattern {
            result["pattern"] = json!(pattern);
        }
        if let DefaultValue::Value(value) = &schema.default {
            result["default"] = value.clone();
        }
        if let Some(value) = &schema.title {
            result["title"] = json!(value);
        }
        if let Some(value) = &schema.description {
            result["description"] = json!(value);
        }
        result
    }
    let mut value = emit(schema, 0);
    value["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    value
}
