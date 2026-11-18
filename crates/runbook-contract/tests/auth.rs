//! AUTH2/AUTH3: sign-in routes in the contract are implemented, typed and
//! list their stable codes.
use serde_json::json;

#[test]
fn invitation_routes_are_implemented_for_org_admins() {
    let doc = runbook_contract::openapi();
    assert_eq!(runbook_model::INVITATION_ROUTES.len(), 3);
    for route in runbook_model::INVITATION_ROUTES {
        let at = format!("{} {}", route.method, route.path);
        let op = &doc["paths"][route.path][route.method.to_ascii_lowercase()];
        assert_eq!(op["x-implementation"], "implemented", "{at}");
        assert_eq!(op["x-required-scopes"], json!(["project:admin"]), "{at}");
        assert_eq!(
            op["security"],
            json!([{"bearerAuth":[]},{"cookieAuth":[]}]),
            "{at}"
        );
        assert!(
            op["responses"].get(route.status.to_string()).is_some(),
            "{at}"
        );
        if let Some(response) = route.response {
            let r = &op["responses"][route.status.to_string()]["content"]["application/json"]["schema"]
                ["$ref"];
            assert_eq!(r, &json!(format!("#/components/schemas/{response}")));
            assert!(
                doc["components"]["schemas"].get(response).is_some(),
                "{response}"
            );
        }
        assert!(
            op["responses"]["403"]["x-error-codes"]
                .to_string()
                .contains("auth.csrf")
        );
        let key = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "Idempotency-Key" && p["required"] == true);
        assert_eq!(key, route.method == "POST", "{at}");
        assert!(
            !runbook_model::PLANNED_ROUTES
                .iter()
                .any(|p| p.path == route.path && p.method == route.method)
        );
    }
    let create = &doc["paths"]["/v1/orgs/{org}/invitations"]["post"];
    let conflict = create["responses"]["409"]["x-error-codes"].to_string();
    assert!(
        conflict.contains("invitation.exists") && conflict.contains("invitation.already_member")
    );
}

#[test]
fn device_routes_are_implemented_without_bearer() {
    let doc = runbook_contract::openapi();
    assert_eq!(runbook_model::DEVICE_ROUTES.len(), 4);
    for route in runbook_model::DEVICE_ROUTES {
        let op = &doc["paths"][route.path]["post"];
        assert_eq!(op["x-implementation"], "implemented", "{}", route.path);
        assert!(
            !op["security"].to_string().contains("bearerAuth"),
            "{}",
            route.path
        );
        assert!(
            !runbook_model::PLANNED_ROUTES
                .iter()
                .any(|p| p.path == route.path)
        );
    }
    let token =
        doc["paths"]["/v1/device/token"]["post"]["responses"]["400"]["x-error-codes"].to_string();
    for code in [
        "authorization_pending",
        "slow_down",
        "expired_token",
        "access_denied",
    ] {
        assert!(token.contains(&format!("device.{code}")), "{code}");
    }
    assert_eq!(
        doc["paths"]["/v1/device/approve"]["post"]["security"],
        json!([{"cookieAuth":[]}])
    );
    assert!(doc["paths"]["/v1/device/code"]["post"]["responses"]["429"].is_object());
}
