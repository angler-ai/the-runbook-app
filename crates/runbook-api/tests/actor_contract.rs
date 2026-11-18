#[test]
fn actor_contract_has_public_routes_replay_and_problem_codes() {
    let doc = runbook_contract::openapi();
    for (method, path, _) in runbook_api::actors::ROUTES {
        let op = &doc["paths"][path][method.to_lowercase()];
        assert!(op.is_object(), "{method} {path}");
        assert_eq!(op["x-implementation"], "postgres");
        assert_eq!(
            op["x-required-scopes"],
            serde_json::json!([if *method == "PATCH" || path.ends_with("/overrides") {
                "executions:admin"
            } else {
                "executions:act"
            }])
        );
        let keys = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["name"] == "Idempotency-Key")
            .count();
        assert_eq!(keys, usize::from(*method == "POST"));
        for status in ["401", "403", "404", "409", "422"] {
            assert!(
                op["responses"][status]["content"]
                    .get("application/problem+json")
                    .is_some()
            );
        }
        if *method == "POST" {
            assert!(op["responses"]["428"].is_object());
        }
    }
    // F56: PRD section 9 names the plural route; the singular is a
    // deprecated server alias, never part of the contract.
    assert!(doc["paths"].get("/v1/step-runs/{sr}/decision").is_none());
    assert!(doc["paths"]["/v1/step-runs/{sr}/decisions"]["post"].is_object());
    assert!(
        doc["paths"]["/v1/step-runs/{sr}/checklist"]
            .get("patch")
            .is_none()
    );
}

#[test]
fn decision_contract_exposes_provenance_and_audited_response() {
    let doc = runbook_contract::openapi();
    let request = &doc["components"]["schemas"]["DecisionRequest"];
    assert!(request["properties"]["session_id"].is_object());
    assert!(request["properties"]["actor"].is_object());
    assert!(
        !request["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "session_id" || v == "actor")
    );
    let op = &doc["paths"]["/v1/step-runs/{sr}/decisions"]["post"];
    assert_eq!(
        op["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/DecisionRecord"
    );
    assert!(
        op["x-actor-provenance"]
            .as_str()
            .unwrap()
            .contains("credential")
    );
}
