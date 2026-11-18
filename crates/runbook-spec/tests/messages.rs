use runbook_spec::Diagnostic;
use runbook_spec::authoring::wire_diagnostics;
use runbook_spec::messages::message_for;

const CODES: &[&str] = &[
    "approval.no_escalation_target",
    "approval.not_approval",
    "data.depth_limit",
    "data.enum",
    "data.range",
    "data.required",
    "data.type_error",
    "data.unknown_field",
    "expression.schema_error",
    "expression.schema_limit",
    "expression.type_error",
    "spec.agent_approval_destructive",
    "spec.agent_approval_requires_min_one",
    "spec.agents_dispatch_removed",
    "spec.agents_params_too_large",
    "spec.alias_collision",
    "spec.approvers_required",
    "spec.auto_decision_destructive",
    "spec.auto_option_required",
    "spec.both_skill_and_skills",
    "spec.catch_on_without_catch",
    "spec.compile_limit",
    "spec.context_limit",
    "spec.control_conflict",
    "spec.control_depth_limit",
    "spec.depth_limit",
    "spec.duplicate_agent_hint",
    "spec.duplicate_id",
    "spec.duplicate_key",
    "spec.duplicate_skill",
    "spec.end_in_finally",
    "spec.escalation_recipients_required",
    "spec.followup_invalid",
    "spec.duplicate_trigger",
    "spec.invalid_cron",
    "spec.invalid_timezone",
    "spec.trigger_input_invalid",
    "spec.trigger_when_type",
    "spec.handoff_requires_task",
    "spec.indexed_field_limit",
    "spec.invalid_id",
    "spec.invalid_ir",
    "spec.invalid_labels",
    "spec.invalid_node_field",
    "spec.invalid_schema",
    "spec.invalid_shape",
    "spec.invalid_snapshot",
    "spec.invalid_value",
    "spec.limit_exceeded",
    "spec.node_limit",
    "spec.non_json_scalar",
    "spec.non_string_key",
    "spec.on_timeout_requires_timeout",
    "spec.required",
    "spec.schema_limit",
    "spec.snapshot_budget_mismatch",
    "spec.snapshot_hash_mismatch",
    "spec.snapshot_limit",
    "spec.snapshot_pins_mismatch",
    "spec.source_limit",
    "spec.source_path_limit",
    "spec.source_shape",
    "spec.source_value_limit",
    "spec.step_budget_overflow",
    "spec.timeout_requires_task",
    "spec.too_many_skills",
    "spec.type_error",
    "spec.uninferable_type",
    "spec.unknown_field",
    "spec.unknown_key",
    "spec.unknown_variable",
    "spec.unresolved_skill",
    "spec.unresolved_team",
    "spec.unresolved_user",
    "spec.unsupported_api_version",
    "spec.unsupported_feature",
    "spec.unsupported_kind",
    "spec.unsupported_profile",
    "spec.unsupported_type",
    "spec.unsupported_value",
    "spec.variable_cycle",
    "spec.wait_outputs_requires_signal",
    "spec.yaml_document_count",
    "spec.yaml_feature_forbidden",
    "spec.yaml_nesting_limit",
    "spec.yaml_syntax",
];

#[test]
fn all_81_codes_produce_valid_messages() {
    assert_eq!(CODES.len(), 86);
    for &code in CODES {
        let msg = message_for(code, "/spec/x");
        assert!(!msg.is_empty(), "code {code} produced empty message");
        assert_ne!(msg, code, "code {code} must not equal code");
        assert!(
            msg.ends_with('.'),
            "code {code} message {msg:?} must end with a period"
        );
    }
}

#[test]
fn unknown_codes_use_default_arm() {
    assert_eq!(
        message_for("custom.unknown_code", "/spec/steps/0"),
        "Problem at /spec/steps/0 (custom.unknown_code)."
    );
    assert_eq!(
        message_for("custom.unknown_code", ""),
        "Problem in the runbook (custom.unknown_code)."
    );
}

#[test]
fn wire_diagnostics_uses_catalog_when_none_and_keeps_explicit_message() {
    let diag_none = Diagnostic {
        code: "spec.invalid_value".into(),
        message: None,
        path: "/spec/steps/0/timeout".into(),
        line: 10,
        column: 5,
    };
    let diag_some = Diagnostic {
        code: "spec.invalid_value".into(),
        message: Some("Explicit custom message.".into()),
        path: "/spec/steps/0/timeout".into(),
        line: 10,
        column: 5,
    };

    let wire = wire_diagnostics(vec![diag_none, diag_some]);
    assert_eq!(wire.len(), 2);

    // First diagnostic: message was None, should use catalog
    assert_eq!(
        wire[0].message,
        message_for("spec.invalid_value", "/spec/steps/0/timeout")
    );
    assert_eq!(wire[0].code, "spec.invalid_value");
    assert_eq!(wire[0].path, "/spec/steps/0/timeout");
    assert_eq!(wire[0].line, Some(10));
    assert_eq!(wire[0].column, Some(5));

    // Second diagnostic: message was Some, should keep explicit message
    assert_eq!(wire[1].message, "Explicit custom message.");
    assert_eq!(wire[1].code, "spec.invalid_value");
    assert_eq!(wire[1].path, "/spec/steps/0/timeout");
    assert_eq!(wire[1].line, Some(10));
    assert_eq!(wire[1].column, Some(5));
}
