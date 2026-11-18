use runbook_model::DIRECTORY_ROUTES;
#[test]
fn route_contract_has_no_private_api() {
    let doc = runbook_contract::openapi();
    for route in DIRECTORY_ROUTES
        .iter()
        .chain(runbook_model::AUTHORING_ROUTES)
        .chain(runbook_model::EXECUTION_ROUTES)
        .chain(runbook_model::STREAM_ROUTES)
        .chain(runbook_model::INBOX_ROUTES)
        .chain(runbook_model::WEBHOOK_ROUTES)
        .chain(runbook_model::TOKEN_ROUTES)
        .chain(runbook_model::ME_ROUTES)
        .chain(runbook_model::STEP_LOG_ROUTES)
        .chain(runbook_model::MEMORY_ROUTES)
        .chain(runbook_model::EVIDENCE_ROUTES)
        .chain(runbook_model::ARCHIVE_ROUTES)
        .chain(runbook_model::SLACK_ROUTES)
        .chain(runbook_model::PICTURE_ROUTES)
        .chain(runbook_model::UNIVERSAL_SEARCH_ROUTES)
    {
        let op = &doc["paths"][route.path][route.method.to_ascii_lowercase()];
        assert_eq!(
            op["x-implementation"], "implemented",
            "{} {}",
            route.method, route.path
        );
        if let Some(name) = route.response {
            assert!(
                doc["components"]["schemas"].get(name).is_some(),
                "missing {name}"
            );
        }
        assert!(route.path.starts_with("/v1/") || route.path == "/.well-known/runbook");
    }
    for (path, methods) in doc["paths"].as_object().unwrap() {
        for (method, op) in methods.as_object().unwrap() {
            if op["x-implementation"] == "implemented" {
                assert!(
                    DIRECTORY_ROUTES
                        .iter()
                        .chain(runbook_model::AUTHORING_ROUTES)
                        .chain(runbook_model::EXECUTION_ROUTES)
                        .chain(runbook_model::STREAM_ROUTES)
                        .chain(runbook_model::INBOX_ROUTES)
                        .chain(runbook_model::WEBHOOK_ROUTES)
                        .chain(runbook_model::TOKEN_ROUTES)
                        .chain(runbook_model::ME_ROUTES)
                        .chain(runbook_model::STEP_LOG_ROUTES)
                        .chain(runbook_model::MEMORY_ROUTES)
                        .chain(runbook_model::EVIDENCE_ROUTES)
                        .chain(runbook_model::INVITATION_ROUTES)
                        .chain(runbook_model::DEVICE_ROUTES)
                        .chain(runbook_model::ARCHIVE_ROUTES)
                        .chain(runbook_model::SLACK_ROUTES)
                        .chain(runbook_model::PICTURE_ROUTES)
                        .chain(runbook_model::UNIVERSAL_SEARCH_ROUTES)
                        .any(|r| r.path == path && r.method.eq_ignore_ascii_case(method))
                );
            }
        }
    }
    assert_eq!(
        doc["paths"]["/v1/executions/{exe}"]["get"]["x-implementation"],
        "implemented"
    );
    let schemas = doc["components"]["schemas"].to_string().to_lowercase();
    assert!(!schemas.contains("password") && !schemas.contains("passkey"));
}

#[test]
fn search_and_stream_contracts_describe_public_reads() {
    let doc = runbook_contract::openapi();
    let search = &doc["paths"]["/v1/projects/{p}/executions"]["get"];
    assert_eq!(
        search["x-required-scopes"],
        serde_json::json!(["executions:read"])
    );
    assert_eq!(
        search["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/RunSearchPage"
    );
    for name in [
        "status", "runbook", "tags", "actor", "from", "to", "cursor", "limit",
    ] {
        assert!(
            search["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == name && p["in"] == "query")
        );
    }
    let stream = &doc["paths"]["/v1/executions/{exe}/events"]["get"];
    assert_eq!(
        stream["x-required-scopes"],
        serde_json::json!(["executions:read"])
    );
    assert!(
        stream["responses"]["200"]["content"]
            .get("text/event-stream")
            .is_some()
    );
    assert!(
        stream["responses"]["410"]["content"]
            .get("application/problem+json")
            .is_some()
    );
    assert!(
        stream["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "Last-Event-ID" && p["in"] == "header")
    );
}
