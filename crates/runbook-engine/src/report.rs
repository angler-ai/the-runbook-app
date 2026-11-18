//! Work findings are independent of protocol outcomes. This layer verifies data,
//! not authorization or lease ownership (those belong to core).
use crate::{EngineError, Event, StepState};
use runbook_model::{
    Data, EvidenceFileRef, EvidenceId, FieldError, ReportRequest, ReportStatus, StepRunId, Verdict,
};
use runbook_spec::{
    ir::{Node, NodeKind},
    validate_data,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkReport {
    pub status: ReportStatus,
    pub verdict: Option<Verdict>,
    #[serde(default, with = "runbook_spec::stored_data")]
    pub outputs: Data,
    pub summary: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub logs: Vec<String>,
    /// UP1: the finalized uploads `evidence` names by `evd_` ID, with the
    /// types finalize verified. Absent from reports without uploads, so
    /// earlier logs keep their exact bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<EvidenceFileRef>,
    /// CF12b review round 2: the step run had a non-blank log line appended
    /// through the logs route when this report was accepted, which satisfies
    /// required `log` evidence as the report's own logs do. The core asks
    /// its store and records the answer here, so replay re-checks the rule
    /// without the store. Absent when false, so other reports keep their
    /// exact bytes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub appended_logs: bool,
}
struct BorrowedReport<'a> {
    status: ReportStatus,
    verdict: Option<Verdict>,
    outputs: &'a Data,
    summary: &'a Option<String>,
    evidence: &'a [String],
    logs: &'a [String],
    files: &'a [EvidenceFileRef],
    appended_logs: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedReport {
    step_run_id: StepRunId,
    work: WorkReport,
}
impl ValidatedReport {
    pub fn work(&self) -> &WorkReport {
        &self.work
    }
    pub fn into_event(self) -> Event {
        Event::StepReported {
            step_run_id: self.step_run_id,
            report: self.work,
        }
    }
}
/// The persisted step record supplies checklist/evidence accumulated before the
/// report. A two-argument source/request check could not enforce those facts.
/// Claim credentials are intentionally not checked by this pure engine function.
pub fn validate_report(
    node: &Node,
    report: &ReportRequest,
    record: &StepState,
) -> Result<ValidatedReport, EngineError> {
    validate_report_with_files(node, report, record, vec![])
}
/// UP1: as [`validate_report`], with the finalized uploads the report's
/// `evd_` entries name, resolved by the core from the upload store. Every
/// `evd_` entry needs its file and every file its entry
/// (`step.evidence_unknown`).
pub fn validate_report_with_files(
    node: &Node,
    report: &ReportRequest,
    record: &StepState,
    files: Vec<EvidenceFileRef>,
) -> Result<ValidatedReport, EngineError> {
    validate_report_with_logs(node, report, record, files, false)
}
/// CF12b review round 2: as [`validate_report_with_files`], where
/// `appended_logs` says whether the step run has a non-blank line appended
/// through the logs route (the core asks its store). Such lines satisfy
/// required `log` evidence; they are not report evidence, so `accepted`
/// does not judge them. The accepted report records the flag.
pub fn validate_report_with_logs(
    node: &Node,
    report: &ReportRequest,
    record: &StepState,
    files: Vec<EvidenceFileRef>,
    appended_logs: bool,
) -> Result<ValidatedReport, EngineError> {
    if record.id.as_ref() != Some(&report.step_run_id) || record.node_id != node.id {
        return Err(EngineError::new("step.not_found"));
    }
    let work = check(
        node,
        record,
        BorrowedReport {
            status: report.status,
            verdict: report.verdict,
            outputs: &report.outputs,
            summary: &report.summary,
            evidence: &report.evidence,
            logs: &report.logs,
            files: &files,
            appended_logs,
        },
    )?;
    Ok(ValidatedReport {
        step_run_id: report.step_run_id.clone(),
        work,
    })
}
pub(crate) fn validate_work(
    node: &Node,
    record: &StepState,
    report: &WorkReport,
) -> Result<WorkReport, EngineError> {
    check(
        node,
        record,
        BorrowedReport {
            status: report.status,
            verdict: report.verdict,
            outputs: &report.outputs,
            summary: &report.summary,
            evidence: &report.evidence,
            logs: &report.logs,
            files: &report.files,
            appended_logs: report.appended_logs,
        },
    )
}
fn check(
    node: &Node,
    record: &StepState,
    report: BorrowedReport<'_>,
) -> Result<WorkReport, EngineError> {
    let NodeKind::Task(task) = &node.kind else {
        return Err(EngineError::new("step.not_task"));
    };
    if report.status == ReportStatus::Succeeded && task.check && report.verdict.is_none() {
        return Err(EngineError::new("step.verdict_required"));
    }
    let errors = output_errors(
        &task.outputs,
        report.outputs,
        report.status == ReportStatus::Succeeded,
    )?;
    if !errors.is_empty() {
        return Err(EngineError::Rejected {
            code: "step.invalid_outputs",
            errors,
        });
    }
    if report.status == ReportStatus::Skipped {
        if !task.skippable {
            return Err(EngineError::new("step.not_skippable"));
        }
        if report.summary.as_ref().is_none_or(|s| s.trim().is_empty()) {
            return Err(EngineError::new("step.skip_reason_required"));
        }
    }
    // UP1: `evd_` entries are uploads; each must come with its file.
    let is_upload = |s: &String| s.parse::<EvidenceId>().is_ok();
    let named: Vec<&String> = report.evidence.iter().filter(|s| is_upload(s)).collect();
    if named.len() != report.files.len()
        || named
            .iter()
            .any(|s| !report.files.iter().any(|f| f.id.as_str() == s.as_str()))
    {
        return Err(EngineError::new("step.evidence_unknown"));
    }
    let files = || record.files.iter().chain(report.files);
    let has_file = files().next().is_some();
    let has_screenshot = files().any(EvidenceFileRef::is_image);
    let has_link = record
        .evidence
        .iter()
        .chain(report.evidence)
        .any(|s| !s.trim().is_empty() && !is_upload(s));
    let has_log = record
        .logs
        .iter()
        .chain(report.logs)
        .any(|s| !s.trim().is_empty());
    // CF12b review round 2: appended lines count toward a requirement only.
    let logged = has_log || report.appended_logs;
    use runbook_spec::ir::EvidenceKind;
    let present = |kind: &EvidenceKind| match kind {
        EvidenceKind::Link => has_link,
        EvidenceKind::Log => has_log,
        EvidenceKind::File => has_file,
        EvidenceKind::Screenshot => has_screenshot,
    };
    // An image is accepted as a screenshot or a file; any other upload only
    // as a file.
    if task.evidence.accepted.as_ref().is_some_and(|kinds| {
        [EvidenceKind::Link, EvidenceKind::Log]
            .iter()
            .any(|kind| present(kind) && !kinds.contains(kind))
            || files().any(|file| {
                !kinds.contains(&EvidenceKind::File)
                    && !(file.is_image() && kinds.contains(&EvidenceKind::Screenshot))
            })
    }) {
        return Err(EngineError::new("step.evidence_not_accepted"));
    }
    if report.status == ReportStatus::Succeeded {
        let present = |kind: &EvidenceKind| match kind {
            EvidenceKind::Log => logged,
            kind => present(kind),
        };
        if task.evidence.required.iter().any(|kind| !present(kind)) {
            return Err(EngineError::new("step.evidence_required"));
        }
        if record.checklist.len() != task.checklist.len()
            || record
                .checklist
                .iter()
                .zip(&task.checklist)
                .any(|(tick, title)| !tick.completed || tick.title != *title)
        {
            return Err(EngineError::new("step.checklist_incomplete"));
        }
    }
    let mut accepted = WorkReport {
        status: report.status,
        verdict: report.verdict,
        outputs: report.outputs.clone(),
        summary: report.summary.clone(),
        evidence: report.evidence.to_vec(),
        logs: report.logs.to_vec(),
        files: report.files.to_vec(),
        appended_logs: report.appended_logs,
    };
    if !task.check || report.status != ReportStatus::Succeeded {
        accepted.verdict = None;
    }
    Ok(accepted)
}

pub fn rollup_verdict(children: &[runbook_model::StepView]) -> Option<Verdict> {
    worst_verdict(children.iter().map(|c| c.verdict))
}
pub(crate) fn worst_verdict(values: impl Iterator<Item = Option<Verdict>>) -> Option<Verdict> {
    values.flatten().max_by_key(|v| match v {
        Verdict::Pass => 0,
        Verdict::Warn => 1,
        Verdict::Fail => 2,
    })
}

// Validate borrowed values before copying them. Rejected unknown or wrong-shaped
// payloads can be arbitrarily deep and must not be recursively cloned first.
pub(crate) fn output_errors(
    schema: &runbook_spec::DataSchema,
    outputs: &Data,
    required: bool,
) -> Result<Vec<FieldError>, EngineError> {
    let runbook_spec::DataType::Object { properties } = &schema.kind else {
        return Err(EngineError::new("step.invalid_schema"));
    };
    let mut errors = vec![];
    for (name, property) in properties {
        let path = field_path("outputs", name);
        if let Some(value) = outputs.get(name) {
            for d in validate_data(property, value) {
                if !required && d.code == "data.required" {
                    continue;
                }
                let suffix = if d.path.is_empty() || d.path.starts_with('[') {
                    d.path
                } else {
                    format!(".{}", d.path)
                };
                errors.push(FieldError {
                    path: format!("{path}{suffix}"),
                    code: d.code,
                    message: "Output does not satisfy its declaration.".into(),
                });
            }
        } else if required && property.required {
            errors.push(FieldError {
                path,
                code: "data.required".into(),
                message: "Required output is missing.".into(),
            });
        }
    }
    for name in outputs
        .keys()
        .filter(|name| !properties.contains_key(*name))
    {
        errors.push(FieldError {
            path: field_path("outputs", name),
            code: "data.unknown_field".into(),
            message: "Output is not declared.".into(),
        });
    }
    Ok(errors)
}
fn field_path(parent: &str, name: &str) -> String {
    let mut chars = name.chars();
    if chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        format!("{parent}.{name}")
    } else {
        format!("{parent}[{}]", serde_json::to_string(name).expect("string"))
    }
}
