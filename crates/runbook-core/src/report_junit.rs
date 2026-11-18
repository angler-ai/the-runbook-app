//! REP1: pure and portable JUnit XML report renderer for execution documents.
//!
//! Maps an [`ExecutionDocumentView`] to standard JUnit XML 1.0:
//! - Root `<testsuites>` element containing one `<testsuite>` for the execution.
//! - One `<testcase>` per step section in document order.
//! - `<failure message="...">` when verdict is fail or status is failed.
//! - `<skipped/>` when status is skipped or cancelled.
//! - `<system-out>` containing the summary and outputs as `key: value` lines.
//! - XML escaping for predefined entities and stripping invalid XML 1.0 control characters.
//!
//! REP2: [`render_junit_named`] is what `?format=junit` serves. It adds, per
//! step, the actor and approval decisions with people named the way the page
//! names them, and, once the run is terminal (D34), the custom report as the
//! suite's `<system-out>`. [`render_junit`] keeps its output.

use runbook_engine::ExecutionState;
use runbook_model::{
    ActorRef, DecisionRecord, ExecutionDocumentView, ExecutionView, StepSection, StepStatus,
    UserId, Verdict,
};
use std::collections::BTreeMap;

/// Render an execution document as a JUnit XML report.
pub fn render_junit(document: &ExecutionDocumentView) -> String {
    render(document, None, None)
}

/// [`render_junit`] for people: actors and approval decisions named from
/// `names` (anyone else by id), plus the custom report once the run is
/// terminal (D34).
pub fn render_junit_named(
    state: &ExecutionState,
    document: &ExecutionDocumentView,
    names: &BTreeMap<UserId, String>,
) -> String {
    let report = crate::execution_document::terminal_report(state);
    render(document, Some(names), report.as_deref())
}

fn render(
    document: &ExecutionDocumentView,
    names: Option<&BTreeMap<UserId, String>>,
    report: Option<&str>,
) -> String {
    let tests = document.sections.len();
    let failures = document.sections.iter().filter(|s| is_failure(s)).count();
    let skipped = document
        .sections
        .iter()
        .filter(|s| !is_failure(s) && is_skipped(s))
        .count();

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<testsuites>\n");
    out.push_str(&format!(
        "  <testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" skipped=\"{skipped}\">\n",
        xml_escape(&document.header.title)
    ));

    let classname = runbook_classname(&document.header);
    let escaped_classname = xml_escape(classname);

    for section in &document.sections {
        let name = if section.path.is_empty() {
            &section.title
        } else {
            &section.path
        };
        let escaped_name = xml_escape(name);

        let failure = is_failure(section);
        let skipped = !failure && is_skipped(section);
        let mut system_out = format_system_out(section);
        if let Some(names) = names {
            for line in people_lines(section, names) {
                if !system_out.is_empty() {
                    system_out.push('\n');
                }
                system_out.push_str(&line);
            }
        }

        if !failure && !skipped && system_out.is_empty() {
            out.push_str(&format!(
                "    <testcase classname=\"{escaped_classname}\" name=\"{escaped_name}\"/>\n"
            ));
        } else {
            out.push_str(&format!(
                "    <testcase classname=\"{escaped_classname}\" name=\"{escaped_name}\">\n"
            ));
            if failure {
                let msg = xml_escape(failure_message(section));
                out.push_str(&format!("      <failure message=\"{msg}\"/>\n"));
            } else if skipped {
                out.push_str("      <skipped/>\n");
            }
            if !system_out.is_empty() {
                let escaped_system_out = xml_escape(&system_out);
                out.push_str(&format!(
                    "      <system-out>{escaped_system_out}</system-out>\n"
                ));
            }
            out.push_str("    </testcase>\n");
        }
    }

    if let Some(report) = report.filter(|r| !r.is_empty()) {
        out.push_str(&format!(
            "    <system-out>{}</system-out>\n",
            xml_escape(report)
        ));
    }
    out.push_str("  </testsuite>\n");
    out.push_str("</testsuites>\n");
    out
}

fn is_failure(section: &StepSection) -> bool {
    section.verdict == Some(Verdict::Fail) || section.status == StepStatus::Failed
}

fn is_skipped(section: &StepSection) -> bool {
    matches!(section.status, StepStatus::Skipped | StepStatus::Cancelled)
}

fn failure_message(section: &StepSection) -> &str {
    if let Some(summary) = section.summary.as_deref().filter(|s| !s.is_empty()) {
        return summary;
    }
    if section.verdict == Some(Verdict::Fail) {
        "fail"
    } else {
        "failed"
    }
}

fn runbook_classname(header: &ExecutionView) -> &str {
    if let Some(slug) = header.runbook_slug.as_deref().filter(|s| !s.is_empty()) {
        return slug;
    }
    if let Some(id) = &header.runbook_id {
        return id.as_str();
    }
    header.runbook_version_id.as_str()
}

fn format_system_out(section: &StepSection) -> String {
    let mut out = String::new();
    if let Some(summary) = section.summary.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(summary);
    }
    for (key, val) in &section.outputs {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(key);
        out.push_str(": ");
        match val {
            serde_json::Value::String(s) => out.push_str(s),
            _ => out.push_str(&val.to_string()),
        }
    }
    out
}

/// The step's actor and approval decisions, people by name (else by id).
fn people_lines(section: &StepSection, names: &BTreeMap<UserId, String>) -> Vec<String> {
    let person = |id: &UserId| {
        names
            .get(id)
            .cloned()
            .unwrap_or_else(|| format!("human {id}"))
    };
    let comment = |c: &Option<String>| c.as_deref().map(|c| format!(": {c}")).unwrap_or_default();
    let mut lines = Vec::new();
    if let Some(actor) = &section.actor {
        let label = match actor {
            ActorRef::Human { user_id } => person(user_id),
            ActorRef::Agent {
                name,
                model: Some(model),
                ..
            } => format!("agent {name} ({model})"),
            ActorRef::Agent { name, .. } => format!("agent {name}"),
            ActorRef::System => "system".into(),
        };
        lines.push(format!("Actor: {label}"));
    }
    for decision in section.approval.iter().flat_map(|a| &a.decisions) {
        lines.push(match decision {
            DecisionRecord::Human {
                user_id,
                option,
                comment: c,
                ..
            } => format!("Decision: {} chose {option}{}", person(user_id), comment(c)),
            DecisionRecord::System { policy, option } => {
                format!("Decision: system policy ({policy}) chose {option}")
            }
            DecisionRecord::Agent {
                agent_name,
                model,
                option,
                comment: c,
                ..
            } => format!(
                "Decision: agent {agent_name}{} chose {option}{}",
                model
                    .as_deref()
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default(),
                comment(c)
            ),
        });
    }
    lines
}

/// Escape XML 1.0 predefined entities (`&`, `<`, `>`, `"`, `'`) and strip
/// characters that are invalid in XML 1.0 (control chars except tab, LF and CR).
pub fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c.is_control() => {}
            c if (c as u32) == 0xFFFE || (c as u32) == 0xFFFF => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_handles_entities_and_controls() {
        assert_eq!(
            xml_escape("<script>&\"'\u{0001}"),
            "&lt;script&gt;&amp;&quot;&apos;"
        );
        assert_eq!(
            xml_escape("hello\tworld\nfoo\rbar"),
            "hello\tworld\nfoo\rbar"
        );
        assert_eq!(
            xml_escape("null\u{0000}control\u{001f}del\u{007f}"),
            "nullcontroldel"
        );
        assert_eq!(xml_escape("normal text 123"), "normal text 123");
    }
}
