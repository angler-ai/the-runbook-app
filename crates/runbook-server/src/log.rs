//! HOST1: the server's own log lines. `text` (the default) keeps the
//! terminal format: notices on stdout, `warning:` and `error:` lines on
//! stderr. `json` (RUNBOOK_LOG_FORMAT=json) writes one object per line on
//! stdout with `severity`, `message`, `time` and `component`, the shape
//! Cloud Logging and most collectors parse without configuration.
//!
//! The server's own messages are fixed text and codes: never a URL,
//! secret, token, email address or tenant ID. [`passthrough`] forwards the
//! auth child's lines as the child wrote them (wrapped in json mode); the
//! auth service keeps to the same rule for what it prints.
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Text,
    Json,
}
impl Format {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}
impl Severity {
    fn name(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        }
    }
}

static FORMAT: OnceLock<Format> = OnceLock::new();

/// Set once, by `serve`, before anything logs; later calls change nothing.
pub fn init(format: Format) {
    let _ = FORMAT.set(format);
}

fn format() -> Format {
    FORMAT.get().copied().unwrap_or_default()
}

pub fn info(component: &str, message: &str) {
    emit(Severity::Info, component, message);
}
pub fn warning(component: &str, message: &str) {
    emit(Severity::Warning, component, message);
}
pub fn error(component: &str, message: &str) {
    emit(Severity::Error, component, message);
}

pub fn emit(severity: Severity, component: &str, message: &str) {
    match format() {
        Format::Json => println!("{}", json_line(severity, component, message, now_ms())),
        Format::Text => match severity {
            Severity::Info => println!("{message}"),
            Severity::Warning => eprintln!("warning: {message}"),
            Severity::Error => eprintln!("error: {message}"),
        },
    }
}

/// A line another process wrote (the auth child's stdout or stderr):
/// passed through as is in text mode, wrapped in json mode. A line that is
/// already a JSON object is kept whole under `child`.
pub fn passthrough(component: &str, line: &str, stderr: bool) {
    match format() {
        Format::Text if stderr => eprintln!("{line}"),
        Format::Text => println!("{line}"),
        Format::Json => {
            let severity = if stderr {
                Severity::Warning
            } else {
                Severity::Info
            };
            let mut value: serde_json::Value =
                serde_json::from_str(&json_line(severity, component, line, now_ms()))
                    .unwrap_or_default();
            if let Ok(child @ serde_json::Value::Object(_)) =
                serde_json::from_str::<serde_json::Value>(line)
            {
                value["child"] = child;
            }
            println!("{value}");
        }
    }
}

pub fn json_line(severity: Severity, component: &str, message: &str, at_ms: i64) -> String {
    serde_json::json!({
        "severity": severity.name(),
        "message": message,
        "time": rfc3339(at_ms),
        "component": component,
    })
    .to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC) from Unix milliseconds.
pub fn rfc3339(ms: i64) -> String {
    let (days, rem) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1_000 % 60,
        rem % 1_000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_lines_carry_severity_message_time_and_component() {
        let line = json_line(
            Severity::Warning,
            "drain",
            "archive job: 1 step(s) failed",
            0,
        );
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["severity"], "WARNING");
        assert_eq!(v["message"], "archive job: 1 step(s) failed");
        assert_eq!(v["time"], "1970-01-01T00:00:00.000Z");
        assert_eq!(v["component"], "drain");
        assert!(!line.contains('\n'));
    }
    #[test]
    fn times_are_utc_rfc3339() {
        assert_eq!(rfc3339(1_763_000_000_123), "2025-11-13T02:13:20.123Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59.999Z");
    }
    #[test]
    fn formats_parse_by_name() {
        assert_eq!(Format::parse("json"), Some(Format::Json));
        assert_eq!(Format::parse("text"), Some(Format::Text));
        assert_eq!(Format::parse("JSON"), None);
        assert_eq!(Format::default(), Format::Text);
    }
}
