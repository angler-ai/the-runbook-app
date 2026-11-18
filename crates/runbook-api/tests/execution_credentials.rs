#[test]
fn execution_credential_contract_excludes_document_and_history() {
    let doc = runbook_contract::openapi();
    for (method, path) in runbook_api::execution_credentials::ROUTES {
        let op = &doc["paths"][path][method.to_lowercase()];
        assert_eq!(op["x-implementation"], "postgres");
        if *method == "POST" {
            assert!(
                op["parameters"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == "Idempotency-Key" && p["required"] == true)
            );
            assert_eq!(
                op["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ExecutionTokenPair"
            );
        }
    }
    for path in ["/v1/executions/{exe}/events", "/v1/executions/{exe}/report"] {
        assert!(
            doc["paths"][path]["get"]["x-execution-token-policy"]
                .as_str()
                .unwrap()
                .contains("403")
        );
    }
    assert!(doc["paths"]["/v1/executions/{exe}"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["oneOf"].is_array());
}
