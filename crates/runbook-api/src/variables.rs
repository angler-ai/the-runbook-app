//! D34/D37: variables are non-secret. Scan supplied values and defaults embedded in their declarations.
use serde_json::Value;
pub fn secret_like(value: &Value) -> bool {
    let pattern=regex::Regex::new(r"(?ix) trb_(?:pat|svc|exe|exr)_[A-Za-z0-9_-]{43} | (?:AKIA|ASIA)[A-Z0-9]{16} | (?:ghp_|gho_|ghs_|github_pat_)[A-Za-z0-9_]+ | xox[a-z]-[A-Za-z0-9-]+ | -----BEGIN\s+(?:[A-Z]+\s+)*PRIVATE\s+KEY----- | \b(?:authorization\s*[:=]\s*\S+|bearer\s+\S+|(?:password|secret|token)\s*=\s*\S+)").expect("fixed secret pattern");
    let mut pending = vec![value];
    while let Some(v) = pending.pop() {
        match v {
            Value::String(s) if pattern.is_match(s) => return true,
            Value::Array(a) => pending.extend(a),
            Value::Object(o) => pending.extend(o.values()),
            _ => {}
        }
    }
    false
}

/// Defaults are data too, including defaults nested in object/array schemas.
pub fn secret_default(declarations: &Value) -> bool {
    let mut pending = vec![declarations];
    while let Some(v) = pending.pop() {
        match v {
            Value::Object(o) => {
                if o.get("default").is_some_and(secret_like) {
                    return true;
                }
                pending.extend(o.values());
            }
            Value::Array(a) => pending.extend(a),
            _ => {}
        }
    }
    false
}
