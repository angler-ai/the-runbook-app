use serde_json::{Value, json};

// Catches a missing route, a scope-only policy that admits service/agent credentials,
// or a response that loses the project identity needed by a cross-project inbox.
#[test]
fn inbox_route_declares_human_scope_and_cross_project_page() {
    let api = runbook_contract::openapi();
    let operation = &api["paths"]["/v1/me/tasks"]["get"];
    assert_eq!(operation["x-implementation"], "implemented");
    assert_eq!(operation["x-required-scopes"], json!(["executions:read"]));
    assert_eq!(operation["x-required-principal"], "human");
    assert_eq!(
        operation["x-project-access"],
        "directory_and_credential_intersection"
    );
    assert_eq!(
        operation["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/InboxPage"
    );
    assert!(
        operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "cursor" && p["in"] == "query")
    );
    assert_eq!(
        operation["responses"]["403"]["content"]["application/problem+json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );
}

#[test]
fn inbox_example_validates_multiple_projects_and_empty_page() {
    let api = runbook_contract::openapi();
    assert!(api["components"]["schemas"].get("InboxPage").is_some());
    let validator = jsonschema::validator_for(&json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "$ref":"#/components/schemas/InboxPage", "components":api["components"]
    }))
    .unwrap();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api/examples/my-tasks.json");
    let example: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(validator.is_valid(&example));
    let typed: runbook_model::InboxPage = serde_json::from_value(example.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), example);
    let items = example["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_ne!(items[0]["project_id"], items[1]["project_id"]);
    assert_eq!(items[0]["kind"], "approval");
    assert_eq!(items[1]["kind"], "human_task");
    assert!(validator.is_valid(&json!({"items":[], "next_cursor":null})));
    let mut invalid = example.clone();
    invalid["items"][0]["kind"] = json!("agent_task");
    assert!(!validator.is_valid(&invalid));
    invalid = example.clone();
    invalid["items"][0]["project_id"] = json!("prj_invalid");
    assert!(!validator.is_valid(&invalid));
}

/// U28: the query the server accepts (me_tasks.rs: kind, limit, cursor) is
/// declared as real parameters, so generated clients can filter and page.
#[test]
fn inbox_query_parameters_are_declared() {
    let api = runbook_contract::openapi();
    let parameters = api["paths"]["/v1/me/tasks"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let query = |name: &str| -> &Value {
        let matching: Vec<&Value> = parameters
            .iter()
            .filter(|p| p["name"] == name && p["in"] == "query")
            .collect();
        assert_eq!(matching.len(), 1, "{name}: {parameters:?}");
        matching[0]
    };
    let kind = &query("kind")["schema"];
    assert_eq!(kind["type"], "string");
    let pattern = regex_lite(kind["pattern"].as_str().unwrap());
    for ok in [
        "approval",
        "human_task",
        "approval,human_task",
        "human_task,approval",
    ] {
        assert!(pattern(ok), "{ok}");
    }
    for bad in [
        "",
        "agent_task",
        "approval,",
        ",approval",
        "approval human_task",
    ] {
        assert!(!pattern(bad), "{bad}");
    }
    let limit = &query("limit")["schema"];
    assert_eq!(
        *limit,
        json!({"type": "integer", "minimum": 1, "maximum": 100, "default": 50})
    );
    assert_eq!(query("cursor")["schema"]["type"], "string");
    assert!(
        parameters.iter().all(|p| p["in"] != "query"
            || ["kind", "limit", "cursor"].contains(&p["name"].as_str().unwrap())),
        "the server refuses any other query parameter: {parameters:?}"
    );
}

/// The declared pattern, checked with a JSON Schema validator (ECMA regex).
fn regex_lite(pattern: &str) -> impl Fn(&str) -> bool {
    let validator =
        jsonschema::validator_for(&json!({"type": "string", "pattern": pattern})).unwrap();
    move |value| validator.is_valid(&json!(value))
}
