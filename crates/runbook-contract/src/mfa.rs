use serde_json::{Map, Value, json};

pub(crate) fn register(paths: &mut Map<String, Value>) {
    let policy = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["require_mfa", "allow_passkeys", "allow_authenticator", "fresh_minutes"],
        "properties": {
            "require_mfa": {"enum": ["off", "admins", "everyone"]},
            "allow_passkeys": {"type": "boolean"},
            "allow_authenticator": {"type": "boolean"},
            "fresh_minutes": {"type": "integer", "minimum": 5, "maximum": 60}
        }
    });
    let view = json!({
        "type": "object",
        "required": ["mfa_required", "mfa_partial", "mfa_enrolled", "allow_passkeys", "allow_authenticator", "fresh_minutes", "mfa_method", "mfa_verified_at"],
        "properties": {
            "mfa_required": {"type": "boolean"},
            "mfa_partial": {"type": "boolean"},
            "mfa_enrolled": {"type": "boolean"},
            "allow_passkeys": {"type": "boolean"},
            "allow_authenticator": {"type": "boolean"},
            "fresh_minutes": {"type": "integer", "minimum": 5, "maximum": 60},
            "mfa_method": {"enum": ["passkey", "totp", "backup_code", null]},
            "mfa_verified_at": {"type": ["integer", "null"]}
        }
    });
    let mut mine = view.clone();
    mine["required"]
        .as_array_mut()
        .expect("required")
        .extend([json!("authenticator_enrolled"), json!("passkey_count")]);
    mine["properties"]["authenticator_enrolled"] = json!({
        "type": ["boolean", "null"],
        "description": "Whether the signed-in account has a confirmed authenticator app. Null when the sign-in service could not say."
    });
    mine["properties"]["passkey_count"] = json!({
        "type": ["integer", "null"], "minimum": 0,
        "description": "How many passkeys the signed-in account has. Null when the sign-in service could not say."
    });
    let member = json!({
        "type": "object",
        "required": ["user_id", "name", "email", "reason"],
        "properties": {
            "user_id": {"type": "string"},
            "name": {"type": "string"},
            "email": {"type": ["string", "null"]},
            "reason": {
                "enum": ["factor_not_allowed", "cannot_enroll"],
                "description": "Why the member would be unable to sign in."
            }
        }
    });
    let preview = json!({
        "type": "object",
        "required": ["dry_run", "policy", "affected"],
        "description": "What the proposed policy would do to members it requires MFA from. Nothing is saved.",
        "properties": {
            "dry_run": {"const": true},
            "policy": policy,
            "affected": {
                "type": "array", "items": member,
                "description": "Members who could not finish signing in and could not enroll an allowed factor themselves."
            }
        }
    });
    let put_answer = json!({"oneOf": [policy, preview]});
    for (path, method, operation_id, description, schema) in [
        (
            "/v1/me/security",
            "get",
            "get_v1_me_security",
            "Read the browser session's MFA state and the signed-in account's factors.",
            &mine,
        ),
        (
            "/v1/orgs/{org}/security",
            "get",
            "get_v1_orgs_org_security",
            "Read the organization security policy as its owner.",
            &policy,
        ),
        (
            "/v1/orgs/{org}/security",
            "put",
            "put_v1_orgs_org_security",
            "Replace the organization security policy as its owner. With `dry_run=true`, save nothing and list the members the policy would lock out.",
            &put_answer,
        ),
    ] {
        let org = path.contains("{org}");
        let mut operation = json!({
            "operationId": operation_id,
            "x-implementation": "implemented",
            "description": description,
            "security": [{"cookieAuth": []}],
            "parameters": if org {json!([{
                "name": "org", "in": "path", "required": true, "schema": {"type": "string"}
            }])} else {json!([])},
            "responses": {
                "200": {"description": description, "content": {"application/json": {"schema": schema}}},
                "401": super::problem_response("A valid credential is required.", &["auth.required", "auth.invalid_credential"]),
                "403": super::problem_response("The operation is forbidden or MFA is required.", &["auth.forbidden", "auth.mfa_required", "auth.mfa_step_up", "auth.csrf"]),
                "503": super::problem_response("The directory or sign-in service is unavailable.", &["storage.unavailable", "auth.unavailable"])
            }
        });
        if org && method == "get" {
            operation["security"] = json!([{"cookieAuth": []}, {"bearerAuth": []}]);
        }
        if org {
            operation["responses"]["404"] =
                super::problem_response("Organization not found.", &["resource.not_found"]);
        }
        if method == "put" {
            operation["parameters"]
                .as_array_mut()
                .expect("parameters")
                .push(json!({
                    "name": "dry_run", "in": "query", "required": false,
                    "description": "Preview who the policy would lock out. Requires the same freshly verified owner session as saving. Nothing is saved.",
                    "schema": {"type": "boolean"}
                }));
            operation["requestBody"] =
                json!({"required": true, "content": {"application/json": {"schema": policy}}});
            operation["responses"]["400"] =
                super::problem_response("Invalid security policy.", &["request.invalid"]);
            operation["responses"]["409"] = super::problem_response(
                "Organization changed concurrently.",
                &["directory.concurrent_change"],
            );
        }
        paths.entry(path.to_owned()).or_insert_with(|| json!({}))[method] = operation;
    }

    paths.insert("/v1/orgs/{org}/users/{user}/mfa/reset".into(), json!({"post": {
        "operationId": "post_v1_orgs_org_users_user_mfa_reset",
        "x-implementation": "implemented",
        "description": "Reset another member's factors and revoke their sessions and OAuth tokens. Requires a freshly verified organization owner.",
        "security": [{"cookieAuth": []}],
        "parameters": (["org", "user"].map(|name| json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"}}))),
        "responses": {
            "200": {"description": "Factors reset", "content": {"application/json": {"schema": {
                "type": "object", "required": ["reset"], "properties": {"reset": {"const": true}}
            }}}},
            "401": super::problem_response("A valid credential is required.", &["auth.required", "auth.invalid_credential"]),
            "403": super::problem_response("The operation is forbidden or MFA is required.", &["auth.forbidden", "auth.mfa_required", "auth.mfa_step_up", "auth.csrf"]),
            "404": super::problem_response("Organization or member not found.", &["resource.not_found"]),
            "503": super::problem_response("The directory or sign-in service is unavailable.", &["storage.unavailable", "auth.unavailable"])
        }
    }}));
    let me =
        &mut paths["/v1/me"]["get"]["responses"]["200"]["content"]["application/json"]["schema"];
    let normal = me.take();
    *me = json!({"oneOf": [normal, {
        "type": "object",
        "required": ["user", "org", "security"],
        "properties": {
            "user": {"type": "object", "required": ["id", "email"], "properties": {
                "id": {"type": "string"}, "email": {"type": "string"}
            }},
            "org": {"type": "object", "required": ["id"], "properties": {"id": {"type": "string"}}},
            "security": view
        }
    }]});
}
