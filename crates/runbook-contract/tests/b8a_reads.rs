use serde_json::json;

// Catches B8a reads that still claim to be execution-token only, lose the
// executions:read requirement for PAT/service callers, or omit the D32
// step-disclosure error for never-offered steps.
#[test]
fn b8a_reads_declare_pat_and_service_access() {
    let api = runbook_contract::openapi();
    for path in [
        "/v1/executions/{exe}/next",
        "/v1/step-runs/{sr}",
        "/v1/step-runs/{sr}/skill-files",
    ] {
        let op = &api["paths"][path]["get"];
        assert_eq!(op["x-implementation"], "postgres", "{path}");
        assert_eq!(
            op["x-required-scopes"],
            json!(["executions:read"]),
            "{path}"
        );
        let principals = op["x-principals"].as_array().unwrap();
        for kind in ["human_pat", "agent_pat", "service", "execution"] {
            assert!(principals.contains(&json!(kind)), "{path}: {kind}");
        }
        assert!(
            !op["description"].as_str().unwrap().contains("later task"),
            "{path}"
        );
        assert!(op["responses"]["404"].is_object(), "{path}");
        if path.starts_with("/v1/step-runs") {
            let codes = op["responses"]["409"]["x-error-codes"].as_array().unwrap();
            assert!(codes.contains(&json!("step.not_ready")), "{path}");
        }
    }
    assert_eq!(
        api["paths"]["/v1/executions/{exe}/next"]["get"]["x-audience"],
        "human PAT: human; agent PAT, service or execution token: agent"
    );
}
