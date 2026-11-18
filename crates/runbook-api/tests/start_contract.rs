#[test]
fn start_contract_exposes_public_admission_state() {
    let doc = runbook_contract::openapi();
    let start = &doc["paths"]["/v1/projects/{p}/executions"]["post"];
    assert_eq!(start["x-implementation"], "implemented");
    assert_eq!(
        start["x-required-scopes"],
        serde_json::json!(["executions:start"])
    );
    assert!(start["responses"]["428"].is_object());
    assert!(start["responses"]["409"].is_object());
    assert_eq!(
        doc["paths"]["/v1/executions/{exe}"]["get"]["x-required-scopes"],
        serde_json::json!(["executions:read"])
    );
    let fields = &doc["components"]["schemas"]["ExecutionView"]["properties"];
    assert!(fields.get("admission").is_some());
    assert!(fields.get("execution_deadline_at_ms").is_some());
    assert!(
        doc["paths"]
            .get("/v1/projects/{p}/runbooks/{rb}/executions")
            .is_none()
    );
}
