//! T0 contract freeze (plan 2026-11-12 §1): the planned post-pilot routes,
//! the cookie scheme and the discovery/meta additions.
use serde_json::Value;
use std::collections::BTreeSet;

fn resolve<'a>(api: &'a Value, reference: &Value) -> &'a Value {
    let name = reference
        .as_str()
        .and_then(|r| r.strip_prefix("#/components/schemas/"))
        .unwrap_or_else(|| panic!("not a schema ref: {reference}"));
    let schema = &api["components"]["schemas"][name];
    assert!(schema.is_object(), "missing schema {name}");
    schema
}

#[test]
fn planned_operations_are_exactly_the_planned_route_list() {
    let api = runbook_contract::openapi();
    let mut planned = BTreeSet::new();
    for (path, operations) in api["paths"].as_object().unwrap() {
        for (method, operation) in operations.as_object().unwrap() {
            if operation["x-implementation"] == "planned" {
                planned.insert((method.to_ascii_uppercase(), path.clone()));
            }
        }
    }
    let listed: BTreeSet<_> = runbook_model::PLANNED_ROUTES
        .iter()
        .map(|r| (r.method.to_owned(), r.path.to_owned()))
        .collect();
    assert_eq!(planned, listed);
    // UP1 shipped the evidence routes: implemented, never planned.
    for path in [
        "/v1/step-runs/{sr}/evidence",
        "/v1/step-runs/{sr}/evidence/{evd}/finalize",
    ] {
        assert_eq!(
            api["paths"][path]["post"]["x-implementation"], "implemented",
            "{path}"
        );
        assert!(!listed.contains(&("POST".to_owned(), path.to_owned())));
    }
    // ARC3 shipped the archive routes: implemented, never planned.
    for path in [
        "/v1/runbooks/{rb}/archives",
        "/v1/runbooks/{rb}/archives/{date}",
        "/v1/executions/{exe}/archive",
    ] {
        assert_eq!(
            api["paths"][path]["get"]["x-implementation"], "implemented",
            "{path}"
        );
        assert!(!listed.contains(&("GET".to_owned(), path.to_owned())));
    }
}

#[test]
fn planned_routes_reference_real_schemas_and_answer_501() {
    let api = runbook_contract::openapi();
    for route in runbook_model::PLANNED_ROUTES {
        let op = &api["paths"][route.path][route.method.to_ascii_lowercase()];
        assert_eq!(op["x-implementation"], "planned", "{}", route.path);
        if let Some(request) = route.request {
            let r = &op["requestBody"]["content"]["application/json"]["schema"]["$ref"];
            assert_eq!(r, &format!("#/components/schemas/{request}"));
            resolve(&api, r);
        }
        if let Some(response) = route.response {
            let r = &op["responses"][route.status.to_string()]["content"]["application/json"]["schema"]
                ["$ref"];
            assert_eq!(r, &format!("#/components/schemas/{response}"));
            resolve(&api, r);
        }
        if route.path.contains("/memory") {
            continue;
        }
        assert_eq!(
            op["responses"]["501"]["x-error-codes"],
            serde_json::json!(["request.not_implemented"]),
            "{} {}",
            route.method,
            route.path
        );
        assert!(op["x-package"].is_string());
    }
}

/// AUTH2 (D46): a person's /v1 operations accept the session cookie beside
/// the bearer; execution-credential minting, refresh and revocation and
/// `DELETE /v1/me/token` do not.
#[test]
fn cookie_scheme_sits_beside_bearer_on_human_routes() {
    let api = runbook_contract::openapi();
    let schemes = &api["components"]["securitySchemes"];
    assert_eq!(schemes["bearerAuth"]["scheme"], "bearer");
    assert_eq!(schemes["cookieAuth"]["type"], "apiKey");
    assert_eq!(schemes["cookieAuth"]["in"], "cookie");
    for (path, operations) in api["paths"].as_object().unwrap() {
        for (method, op) in operations.as_object().unwrap() {
            let cookie = op["security"]
                .as_array()
                .is_some_and(|s| s.iter().any(|r| r.get("cookieAuth").is_some()));
            let bearer = op["security"]
                .as_array()
                .is_some_and(|s| s.iter().any(|r| r.get("bearerAuth").is_some()));
            // Device approval, personal security and MFA writes need a browser session.
            let session_only = matches!(
                path.as_str(),
                "/v1/device/approve"
                    | "/v1/device/lookup"
                    | "/v1/me/security"
                    | "/v1/orgs/{org}/users/{user}/mfa/reset"
            ) || (path == "/v1/orgs/{org}/security" && method == "put");
            if cookie && !session_only {
                assert!(bearer, "{method} {path}");
            }
            let refused = path.starts_with("/v1/executions/{exe}/tokens")
                || path.starts_with("/v1/executions/{exe}/credentials")
                || (path == "/v1/me/token" && method == "delete");
            if bearer && path.starts_with("/v1/") {
                assert_eq!(cookie, !refused, "{method} {path}");
            }
        }
    }
    for (path, method) in [
        ("/v1/me", "get"),
        ("/v1/orgs/{org}/invitations", "post"),
        ("/v1/projects/{p}/executions", "post"),
    ] {
        let security = api["paths"][path][method]["security"].to_string();
        assert!(security.contains("cookieAuth"), "{method} {path}");
    }
    let approve = &api["paths"]["/v1/device/approve"]["post"];
    assert_eq!(approve["security"], serde_json::json!([{"cookieAuth":[]}]));
    assert_eq!(
        api["paths"]["/v1/device/token"]["post"]["security"],
        serde_json::json!([])
    );
}

#[test]
fn discovery_has_auth_block_and_meta_has_upload_limits() {
    let api = runbook_contract::openapi();
    let schemas = &api["components"]["schemas"];
    let discovery = &schemas["DiscoveryView"]["properties"]["auth"];
    assert_eq!(discovery["$ref"], "#/components/schemas/AuthDiscovery");
    let auth = &schemas["AuthDiscovery"]["properties"];
    assert!(auth["methods"].is_object());
    assert!(auth["device_authorization_endpoint"].is_object());
    let uploads = schemas["ApiLimits"]["properties"]["uploads"].to_string();
    assert!(
        uploads.contains("#/components/schemas/UploadLimits"),
        "{uploads}"
    );
    for field in [
        "image_types",
        "image_bytes",
        "file_bytes",
        "image_max_dimension",
        "images_per_step_run",
        "files_per_execution",
        "storage_bytes",
        "client_resize_long_edge",
    ] {
        assert!(
            schemas["UploadLimits"]["properties"][field].is_object(),
            "{field}"
        );
    }
}
