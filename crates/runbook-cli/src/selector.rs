use std::num::NonZeroU32;

/// Parse a CLI runbook reference: `<runbook>[@<n|rbv_id>]`
/// Returns `(runbook_name, Option<version_value>)` where version_value is:
/// - `None` if omitted
/// - `Some(serde_json::Value::Number)` for positive integers (e.g. `@3`)
/// - `Some(serde_json::Value::String)` for rbv_ IDs (e.g. `@rbv_<26 chars>`)
pub fn parse_runbook_ref(s: &str) -> Result<(String, Option<serde_json::Value>), String> {
    if s.is_empty() {
        return Err("runbook name cannot be empty".to_string());
    }

    let (name, raw_version) = match s.split_once('@') {
        Some((name, raw)) => (name, Some(raw)),
        None => (s, None),
    };

    if name.is_empty() {
        return Err("runbook name cannot be empty".to_string());
    }

    // Validate kebab-case slug for runbook name
    if !name.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    }) {
        return Err(format!("invalid runbook name: '{name}'"));
    }

    let Some(raw) = raw_version else {
        return Ok((name.to_string(), None));
    };

    if raw.is_empty() {
        return Err("version selector cannot be empty after '@'".to_string());
    }

    let val = parse_version_selector(raw)?;
    Ok((name.to_string(), Some(val)))
}

/// Parse a version selector string: either a positive integer (e.g. "3")
/// or an rbv_ ID (e.g. "rbv_<26 chars>").
pub fn parse_version_selector(raw: &str) -> Result<serde_json::Value, String> {
    if raw.is_empty() {
        return Err("version selector cannot be empty".to_string());
    }

    // Check if it is a positive integer
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        match raw.parse::<NonZeroU32>() {
            Ok(num) => Ok(serde_json::Value::Number(num.get().into())),
            Err(_) => Err(format!("invalid version number: '{raw}'")),
        }
    } else if let Some(ulid_part) = raw.strip_prefix("rbv_") {
        if ulid_part.len() == 26 {
            // Check Crockford Base32 characters if desired: ^[0-7][0-9A-HJKMNP-TV-Z]{25}$
            // Or general 26 chars for rbv_<26 chars>
            Ok(serde_json::Value::String(raw.to_string()))
        } else {
            Err(format!("invalid rbv_ version ID: '{raw}'"))
        }
    } else {
        Err(format!("invalid version selector: '{raw}'"))
    }
}

/// Validate a project identifier or kebab-case slug.
/// Accepts either a valid `ProjectId` (`prj_<26 Crockford Base32>`) or a kebab-case slug.
pub fn validate_project_ref(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("project cannot be empty".to_string());
    }
    if s.parse::<runbook_model::ProjectId>().is_ok() {
        return Ok(());
    }
    let is_kebab_case = s.split('-').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    if is_kebab_case {
        Ok(())
    } else {
        Err(format!("invalid project identifier or slug: '{s}'"))
    }
}

/// Validate a signal name as a kebab-case or snake_case identifier matching `^[a-z][a-z0-9_-]{0,63}$`.
pub fn validate_signal_name(s: &str) -> Result<(), String> {
    if s.is_empty() || s.len() > 64 {
        return Err(format!("invalid signal name: '{s}'"));
    }
    let mut bytes = s.bytes();
    let first = bytes.next().unwrap();
    if !first.is_ascii_lowercase() {
        return Err(format!("invalid signal name: '{s}'"));
    }
    if !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-') {
        return Err(format!("invalid signal name: '{s}'"));
    }
    Ok(())
}
