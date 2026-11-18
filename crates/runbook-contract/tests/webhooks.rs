//! C1: webhook routes are implemented, scoped, typed and carry their
//! problem codes; the delivery contract documents signing and the catalog.
use serde_json::json;

#[test]
fn webhook_routes_are_implemented_with_scope_types_and_codes() {
    let doc = runbook_contract::openapi();
    for route in runbook_model::WEBHOOK_ROUTES {
        let op = &doc["paths"][route.path][route.method.to_ascii_lowercase()];
        assert_eq!(
            op["x-implementation"], "implemented",
            "{} {}",
            route.method, route.path
        );
        assert_eq!(op["x-required-scopes"], json!(["webhooks:manage"]));
        let status = route.status.to_string();
        assert!(
            op["responses"].get(&status).is_some(),
            "{} {}",
            route.method,
            route.path
        );
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
        let codes = op["responses"]["403"]["x-error-codes"].to_string();
        assert!(codes.contains("auth.scope_missing") && codes.contains("auth.forbidden"));
        assert!(
            op["responses"]["404"]["x-error-codes"]
                .to_string()
                .contains("resource.not_found")
        );
        let key = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "Idempotency-Key" && p["required"] == true);
        assert_eq!(
            key,
            route.method == "POST",
            "{} {}",
            route.method,
            route.path
        );
    }
    let create = &doc["paths"]["/v1/projects/{p}/webhooks"]["post"];
    let codes = create["responses"]["409"]["x-error-codes"].to_string();
    assert!(
        codes.contains("webhooks.limit_reached")
            && codes.contains("webhooks.secret_already_issued")
    );
    let invalid = create["responses"]["422"]["x-error-codes"].to_string();
    for code in [
        "webhooks.target_forbidden",
        "webhooks.url_https_required",
        "webhooks.events_invalid",
    ] {
        assert!(invalid.contains(code), "{code}");
    }
    // Secrets appear only in the one-time create/rotate view.
    let views = ["WebhookView", "WebhookPage", "DeliveryView", "DeliveryPage"];
    for name in views {
        assert!(
            !doc["components"]["schemas"][name]
                .to_string()
                .contains("secret\""),
            "{name}"
        );
    }
    assert!(
        doc["components"]["schemas"]["WebhookSecretView"]["properties"]
            .get("secret")
            .is_some()
    );
    let delivery = &doc["x-webhook-delivery"];
    assert_eq!(delivery["signature"]["header"], "Runbook-Signature");
    for event in [
        "agent.work_available",
        "approval.requested",
        "step.blocked",
        "webhook.test",
    ] {
        assert!(delivery["events"].to_string().contains(event), "{event}");
    }
    assert!(
        doc["components"]["schemas"]["ExecutionView"]["properties"]
            .get("notifications_not_delivered")
            .is_some()
    );
}
