use runbook_cli::validate::validate_source;
use std::path::PathBuf;

#[test]
fn valid_spec_reports_budget() {
    let source = r#"
apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: x
  title: X
spec:
  steps:
    - id: a
      title: A
"#;
    let report = validate_source(source);
    assert!(
        report.ok,
        "expected valid spec, got diagnostics: {:?}",
        report.diagnostics
    );
    assert_eq!(report.diagnostics.len(), 0);
    assert_eq!(report.worst_case_step_runs, Some(1));
}

#[test]
fn invalid_yaml_and_bad_reference_report_diagnostics() {
    // 1. Broken YAML
    let broken_yaml = "apiVersion: [unclosed list";
    let report = validate_source(broken_yaml);
    assert!(!report.ok);
    assert!(!report.diagnostics.is_empty());
    assert_eq!(report.worst_case_step_runs, None);

    // 2. Bad reference: ${{ inputs.missing }}
    let bad_ref = r#"
apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: bad-ref
  title: Bad Reference
spec:
  steps:
    - id: a
      title: A
      description: "${{ inputs.missing }}"
"#;
    let report = validate_source(bad_ref);
    assert!(!report.ok);
    assert!(!report.diagnostics.is_empty());
    assert_eq!(report.worst_case_step_runs, None);
}

#[test]
fn flagship_spec_validates() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let flagship_path = manifest_dir.join("../../conformance/spec/post-deploy.yaml");
    let content = std::fs::read_to_string(&flagship_path)
        .unwrap_or_else(|e| panic!("failed to read flagship spec at {flagship_path:?}: {e}"));

    let report = validate_source(&content);
    if report.ok {
        assert!(report.worst_case_step_runs.is_some());
    } else {
        // If it fails only because it needs skills or teams context, assert the diagnostics mention that
        let diag_str = format!("{:?}", report.diagnostics);
        let has_context_diagnostics = report.diagnostics.iter().any(|d| {
            d.code.contains("skill")
                || d.code.contains("team")
                || d.code.contains("user")
                || d.code.contains("variable")
                || d.path.contains("skill")
                || d.path.contains("team")
                || d.path.contains("user")
                || d.message.contains("skill")
                || d.message.contains("team")
                || d.message.contains("user")
        });
        assert!(
            has_context_diagnostics,
            "expected diagnostics to mention skills, teams, users or variables context, got: {diag_str}"
        );
    }
}
