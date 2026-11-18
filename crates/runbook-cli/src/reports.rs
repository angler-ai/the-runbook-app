//! C2 `run report --format md|json` (PRD §10) and REP2's `junit|pdf`. The
//! execution document is the report: `md` reads `?format=markdown` (a
//! `text/markdown` body, printed verbatim), `json` reads `?format=json`,
//! `junit` reads `?format=junit` (XML) and `pdf` reads `?format=pdf`.
//! `--out <file>` writes the body to a file instead of stdout; a PDF is
//! binary, so without `--out` it is written only to a stdout that is not a
//! terminal (a pipe or a redirect).
use crate::api::{ApiClient, ApiError};
use crate::cliutil::{OK, api_failed, fail, print_json, usage};
use clap::ValueEnum;
use std::io::{IsTerminal, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ReportFormat {
    #[value(alias = "markdown")]
    Md,
    Json,
    Junit,
    Pdf,
}

/// Print (or, with `out`, save) the report of `exe` in `format`; returns
/// the exit code.
pub fn print_report(
    client: &ApiClient,
    exe: &str,
    format: ReportFormat,
    out: Option<&Path>,
) -> i32 {
    if refuses_terminal(format, out, std::io::stdout().is_terminal()) {
        return usage(
            "a PDF report is binary: pass --out <file> or redirect stdout to a file or pipe",
        );
    }
    let path = format!("/v1/executions/{exe}/report");
    let body: Result<Vec<u8>, ApiError> = match format {
        ReportFormat::Md => client
            .get_text(&path, &[("format", "markdown")], "text/markdown")
            .map(|mut text| {
                if out.is_none() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.into_bytes()
            }),
        ReportFormat::Json => match client.get_json_query(&path, &[("format", "json")]) {
            Ok(report) if out.is_none() => {
                print_json(&report);
                return OK;
            }
            Ok(report) => Ok(format!(
                "{}\n",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            )
            .into_bytes()),
            Err(e) => Err(e),
        },
        ReportFormat::Junit => client.get_bytes(&path, &[("format", "junit")], "application/xml"),
        ReportFormat::Pdf => client.get_bytes(&path, &[("format", "pdf")], "application/pdf"),
    };
    let body = match body {
        Ok(body) => body,
        Err(e) => return api_failed(e),
    };
    let written = match out {
        Some(file) => std::fs::write(file, &body)
            .map_err(|e| format!("could not write {}: {e}", file.display())),
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(&body)
                .and_then(|()| stdout.flush())
                .map_err(|e| format!("could not write the report: {e}"))
        }
    };
    match written {
        Ok(()) => OK,
        Err(message) => fail(message),
    }
}

/// A PDF never goes to a terminal: only to `--out` or a redirected stdout.
fn refuses_terminal(format: ReportFormat, out: Option<&Path>, stdout_is_terminal: bool) -> bool {
    format == ReportFormat::Pdf && out.is_none() && stdout_is_terminal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_goes_to_a_terminal_never() {
        let file = Path::new("report.pdf");
        assert!(refuses_terminal(ReportFormat::Pdf, None, true));
        assert!(!refuses_terminal(ReportFormat::Pdf, None, false));
        assert!(!refuses_terminal(ReportFormat::Pdf, Some(file), true));
        for text in [ReportFormat::Md, ReportFormat::Json, ReportFormat::Junit] {
            assert!(!refuses_terminal(text, None, true), "{text:?}");
        }
    }
}
