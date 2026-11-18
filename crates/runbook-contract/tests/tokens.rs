//! B4.1/C2: `/v1/tokens` is implemented, needs `tokens:manage`, is typed,
//! and documents secrets-once replay and narrowing.
use serde_json::json;

#[test]
fn token_routes_are_implemented_with_scope_types_and_codes() {
    let doc = runbook_contract::openapi();
    assert_eq!(runbook_model::TOKEN_ROUTES.len(), 3);
    for route in runbook_model::TOKEN_ROUTES {
        let op = &doc["paths"][route.path][route.method.to_ascii_lowercase()];
        let at = format!("{} {}", route.method, route.path);
        assert_eq!(op["x-implementation"], "implemented", "{at}");
        assert_eq!(op["x-required-scopes"], json!(["tokens:manage"]), "{at}");
        let status = route.status.to_string();
        assert!(op["responses"].get(&status).is_some(), "{at}");
        if let Some(response) = route.response {
            assert_eq!(
                op["responses"][&status]["content"]["application/json"]["schema"]["$ref"],
                format!("#/components/schemas/{response}")
            );
            assert!(doc["components"]["schemas"].get(response).is_some());
        }
        if let Some(request) = route.request {
            assert_eq!(
                op["requestBody"]["content"]["application/json"]["schema"]["$ref"],
                format!("#/components/schemas/{request}")
            );
        }
        let forbidden = op["responses"]["403"]["x-error-codes"].to_string();
        assert!(forbidden.contains("auth.scope_missing") && forbidden.contains("auth.forbidden"));
        let key = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "Idempotency-Key" && p["required"] == true);
        assert_eq!(key, route.method == "POST", "{at}");
    }
    let create = &doc["paths"]["/v1/tokens"]["post"];
    let conflict = create["responses"]["409"]["x-error-codes"].to_string();
    assert!(
        conflict.contains("tokens.secret_already_issued")
            && conflict.contains("request.idempotency_key_reused")
    );
    assert!(
        create["responses"]["403"]["x-error-codes"]
            .to_string()
            .contains("tokens.exceeds_caller")
    );
    // Only the create response carries a secret; views never do.
    let schemas = &doc["components"]["schemas"];
    assert!(
        schemas["TokenSecretView"]["properties"]
            .get("secret")
            .is_some()
    );
    let fields: Vec<&String> = schemas["TokenView"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert!(
        fields
            .iter()
            .all(|f| !f.contains("secret") && !f.contains("hash")),
        "{fields:?}"
    );
}
