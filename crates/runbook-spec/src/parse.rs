use std::collections::BTreeMap;

use serde_json::{Map, Value};
use yaml_rust2::{
    Yaml,
    parser::{Event, Parser},
    scanner::{Marker, Scanner, TScalarStyle, TokenType},
};

use crate::{Diagnostic, SourceSpan, Spec};

mod source;

const MAX_SOURCE_PATH_BYTES: usize = 8 * 1024 * 1024;

/// Parse source syntax without running capability or semantic validation.
pub fn parse(source: &str) -> Result<Spec, Vec<Diagnostic>> {
    if source.len() > crate::normalize::MAX_SOURCE_BYTES {
        return Err(vec![Diagnostic {
            message: None,
            code: "spec.source_limit".into(),
            path: String::new(),
            line: 1,
            column: 1,
        }]);
    }
    parse_document(source).map_err(|error| vec![error])
}

/// Internal projection of already bounded, generated semantic data. This does
/// not parse or interpret saved source annotations during snapshot restoration.
pub(crate) fn from_lowered(value: Value) -> Result<Spec, Vec<Diagnostic>> {
    source::build("", value, BTreeMap::new()).map_err(|e| vec![e])
}

fn parse_document(source: &str) -> Result<Spec, Diagnostic> {
    // Inspect syntax tokens before resolving aliases or tags. Undefined aliases
    // must receive the same feature diagnostic as references to known anchors.
    let mut scanner = Scanner::new(source.chars());
    while let Some(token) = scanner
        .next_token()
        .map_err(|error| diagnostic("spec.yaml_syntax", "", *error.marker()))?
    {
        if matches!(
            token.1,
            TokenType::Anchor(_)
                | TokenType::Alias(_)
                | TokenType::Tag(_, _)
                | TokenType::TagDirective(_, _)
        ) || matches!(token.1, TokenType::VersionDirective(major, minor) if (major, minor) != (1, 2))
        {
            return Err(diagnostic("spec.yaml_feature_forbidden", "", token.0));
        }
    }
    let mut reader = Reader {
        parser: Parser::new_from_str(source),
        spans: BTreeMap::new(),
        values: 0,
        source_path_bytes: 0,
    };
    reader.next("")?; // StreamStart
    let (event, mark) = reader.next("")?;
    if event != Event::DocumentStart {
        return Err(diagnostic("spec.yaml_syntax", "", mark));
    }
    let (event, mark) = reader.next("")?;
    let value = reader.value(event, mark, "")?;
    let (event, mark) = reader.next("")?;
    if event != Event::DocumentEnd {
        return Err(diagnostic("spec.yaml_syntax", "", mark));
    }
    let (event, mark) = reader.next("")?;
    if event != Event::StreamEnd {
        return Err(diagnostic("spec.yaml_document_count", "", mark));
    }
    let spec = source::build(source, value, reader.spans)?;
    Ok(spec)
}

struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    spans: BTreeMap<String, SourceSpan>,
    values: usize,
    source_path_bytes: usize,
}

impl Reader<'_> {
    fn next(&mut self, path: &str) -> Result<(Event, Marker), Diagnostic> {
        self.parser
            .next_token()
            .map_err(|error| diagnostic("spec.yaml_syntax", path, *error.marker()))
    }

    fn value(&mut self, event: Event, mark: Marker, path: &str) -> Result<Value, Diagnostic> {
        // Bound parser recursion before allocating or descending further. Node
        // count and execution bounds are separate compiler validation passes.
        if path.bytes().filter(|byte| *byte == b'/').count() > 128 {
            return Err(diagnostic("spec.yaml_nesting_limit", path, mark));
        }
        if self.values == crate::normalize::MAX_SOURCE_VALUES {
            return Err(diagnostic("spec.source_value_limit", path, mark));
        }
        self.values += 1;
        // Charge the UTF-8 JSON pointer before retaining its allocation. A
        // wide value under a long key can otherwise amplify a small source.
        self.source_path_bytes = self
            .source_path_bytes
            .checked_add(path.len())
            .filter(|total| *total <= MAX_SOURCE_PATH_BYTES)
            .ok_or_else(|| diagnostic("spec.source_path_limit", path, mark))?;
        self.spans.insert(
            path.to_owned(),
            SourceSpan {
                line: mark.line(),
                column: mark.col() + 1,
            },
        );
        match event {
            Event::Scalar(text, style, _, _) => {
                if style != TScalarStyle::Plain {
                    return Ok(Value::String(text));
                }
                scalar(&text, path, mark)
            }
            Event::SequenceStart(_, _) => {
                let mut values = Vec::new();
                loop {
                    let (event, mark) = self.next(path)?;
                    if event == Event::SequenceEnd {
                        break;
                    }
                    values.push(self.value(event, mark, &format!("{path}/{}", values.len()))?);
                }
                Ok(Value::Array(values))
            }
            Event::MappingStart(_, _) => {
                let mut values = Map::new();
                loop {
                    let (event, mark) = self.next(path)?;
                    if event == Event::MappingEnd {
                        break;
                    }
                    let Event::Scalar(key, style, _, _) = event else {
                        return Err(diagnostic("spec.non_string_key", path, mark));
                    };
                    if !scalar_key_is_string(&key, style, path, mark)? {
                        return Err(diagnostic("spec.non_string_key", path, mark));
                    }
                    let child_path =
                        format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                    if key == "<<" {
                        return Err(diagnostic("spec.yaml_feature_forbidden", &child_path, mark));
                    }
                    if values.contains_key(&key) {
                        return Err(diagnostic("spec.duplicate_key", &child_path, mark));
                    }
                    let (event, mark) = self.next(&child_path)?;
                    values.insert(key, self.value(event, mark, &child_path)?);
                }
                Ok(Value::Object(values))
            }
            Event::Alias(_) => Err(diagnostic("spec.yaml_feature_forbidden", path, mark)),
            _ => Err(diagnostic("spec.yaml_syntax", path, mark)),
        }
    }
}

fn diagnostic(code: &str, path: &str, mark: Marker) -> Diagnostic {
    Diagnostic {
        message: None,
        code: code.to_owned(),
        path: path.to_owned(),
        line: mark.line(),
        column: mark.col() + 1,
    }
}

fn scalar_key_is_string(
    text: &str,
    style: TScalarStyle,
    path: &str,
    mark: Marker,
) -> Result<bool, Diagnostic> {
    Ok(style != TScalarStyle::Plain || scalar(text, path, mark)?.is_string())
}

fn scalar(text: &str, path: &str, mark: Marker) -> Result<Value, Diagnostic> {
    // Preserve JSON numbers as decimal text rather than rounding through f64.
    if let Ok(number) = text.parse::<serde_json::Number>() {
        return Ok(Value::Number(number));
    }
    // Recognize radix integer grammar before fixed-width YAML resolution. A
    // numeric spelling must never become a string merely because i64 overflows.
    let radix = text
        .strip_prefix("0x")
        .map(|digits| (16, digits))
        .or_else(|| text.strip_prefix("0o").map(|digits| (8, digits)));
    if let Some((base, digits)) = radix
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_digit(base))
    {
        let integer = u128::from_str_radix(digits, base)
            .map_err(|_| diagnostic("spec.non_json_scalar", path, mark))?;
        let number = integer
            .to_string()
            .parse::<serde_json::Number>()
            .map_err(|_| diagnostic("spec.non_json_scalar", path, mark))?;
        return Ok(Value::Number(number));
    }
    Ok(match text {
        "Null" | "NULL" => Value::Null,
        _ => match Yaml::from_str(text) {
            Yaml::Null => Value::Null,
            Yaml::Boolean(value) => Value::Bool(value),
            Yaml::Integer(value) => value.into(),
            Yaml::Real(value) => yaml_decimal_number(&value)
                .map(Value::Number)
                .ok_or_else(|| diagnostic("spec.non_json_scalar", path, mark))?,
            _ => Value::String(text.to_owned()),
        },
    })
}

fn yaml_decimal_number(text: &str) -> Option<serde_json::Number> {
    // YAML core permits a leading plus, omitted integer/fraction digits and
    // leading zeros. Normalize those spellings without a floating-point roundtrip.
    let text = text.strip_prefix('+').unwrap_or(text);
    let (sign, text) = text
        .strip_prefix('-')
        .map_or(("", text), |text| ("-", text));
    let exponent_at = text.find(['e', 'E']).unwrap_or(text.len());
    let (mantissa, exponent) = text.split_at(exponent_at);
    let (whole, fraction) = mantissa
        .split_once('.')
        .map_or((mantissa, None), |(whole, fraction)| {
            (whole, Some(fraction))
        });
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let mut number = format!("{sign}{whole}");
    if let Some(fraction) = fraction {
        number.push('.');
        number.push_str(if fraction.is_empty() { "0" } else { fraction });
    }
    number.push_str(exponent);
    number.parse().ok()
}
