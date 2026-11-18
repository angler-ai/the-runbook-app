use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidateReport {
    pub ok: bool,
    pub diagnostics: Vec<runbook_model::Diagnostic>,
    pub worst_case_step_runs: Option<u64>,
}

fn to_model_diagnostics(
    diagnostics: Vec<runbook_spec::Diagnostic>,
) -> Vec<runbook_model::Diagnostic> {
    diagnostics
        .into_iter()
        .map(|d| runbook_model::Diagnostic {
            message: d.message.unwrap_or_else(|| d.code.clone()),
            code: d.code,
            path: d.path,
            line: u32::try_from(d.line).ok().filter(|n| *n > 0),
            column: u32::try_from(d.column).ok().filter(|n| *n > 0),
        })
        .collect()
}

pub fn validate_source(source: &str) -> ValidateReport {
    let spec = match runbook_spec::parse(source) {
        Ok(spec) => spec,
        Err(diagnostics) => {
            return ValidateReport {
                ok: false,
                diagnostics: to_model_diagnostics(diagnostics),
                worst_case_step_runs: None,
            };
        }
    };

    let compile_context = runbook_spec::CompileContext::default();
    match runbook_spec::compile(&spec, &compile_context) {
        Ok(compiled) => ValidateReport {
            ok: true,
            diagnostics: Vec::new(),
            worst_case_step_runs: Some(compiled.worst_case_step_runs()),
        },
        Err(errors) => ValidateReport {
            ok: false,
            diagnostics: to_model_diagnostics(errors),
            worst_case_step_runs: None,
        },
    }
}
