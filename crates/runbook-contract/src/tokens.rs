use super::*;

/// B4.1/C2 token management, all under `tokens:manage`.
pub(super) fn register(paths: &mut Map<String, Value>) {
    for route in TOKEN_ROUTES {
        let method = route.method.to_ascii_lowercase();
        let mut parameters: Vec<Value> = route
            .path
            .split('/')
            .filter_map(|p| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect();
        if route.method == "POST" {
            parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}}));
        }
        if route.method == "GET" {
            parameters.extend([
                json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":100}}),
                json!({"name":"cursor","in":"query","schema":{"type":"string"},"description":"Opaque next_cursor from the previous page (token ID order)."}),
            ]);
        }
        let description = match route.method {
            "POST" => {
                "Issue a personal access token (trb_pat_) to the caller, or, for a human org owner or admin (D17), a project service token (trb_svc_). Every field narrows the calling credential: scopes and projects must be held by it, a personal token never outlives it (and lasts at most a year, default 90 days), an agent-kind token (D26) never carries tokens:manage (not inherited; refused with 422 if requested), and a service token's role never exceeds the caller's project role. Omitted scopes/projects on a personal token inherit the caller's. user_id, if given, must be the caller: the API never issues tokens for another user. The secret is returned once, here; only its SHA-256 is stored. A replay of the same key and body answers 409 tokens.secret_already_issued without a secret."
            }
            "GET" => {
                "List the caller's live personal tokens and, for human org owners and admins, the organization's service tokens. Metadata only, with last use; never secrets or hashes."
            }
            _ => {
                "Revoke a token the caller can list, or (D17, org owner or admin) another user's personal token when that user's org role is not above the caller's: owners revoke anyone's, admins never an owner's. It stops authenticating at once; tokens it minted stay valid (independent records). Unknown, foreign, revoked or not permitted is 404."
            }
        };
        let mut op = json!({
            "operationId": format!("{}_{}", method, route.path.trim_start_matches('/').replace(['/', '{', '}'], "_")),
            "x-implementation": "implemented",
            "x-required-scopes": ["tokens:manage"],
            "x-required-principal": "user",
            "summary": description,
            "description": format!("{description} Requires tokens:manage on an active user's human-kind personal token; agent-kind PATs, service and execution credentials cannot manage tokens."),
            "security": [{"bearerAuth": []}],
            "parameters": parameters,
            "responses": {}
        });
        op["responses"][route.status.to_string()] = if let Some(response) = route.response {
            json!({"description":"Typed response","headers":{"Cache-Control":{"schema":{"type":"string","const":"no-store"}}},"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}})
        } else {
            json!({"description":"Revoked"})
        };
        if let Some(request) = route.request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        let mut forbidden = vec!["auth.scope_missing", "auth.forbidden"];
        let mut errors: Vec<(&str, Vec<&str>)> = vec![
            ("401", vec!["auth.required", "auth.invalid_credential"]),
            ("404", vec!["resource.not_found"]),
            ("405", vec!["request.method_not_allowed"]),
            ("503", vec!["storage.unavailable"]),
        ];
        match route.method {
            "POST" => {
                forbidden.push("tokens.exceeds_caller");
                errors.push(("422", vec!["request.invalid"]));
                errors.push(("428", vec!["request.idempotency_key_required"]));
                errors.push((
                    "409",
                    vec![
                        "tokens.secret_already_issued",
                        "request.idempotency_key_reused",
                    ],
                ));
            }
            "GET" => {
                errors.push(("400", vec!["cursor.invalid"]));
                errors.push(("422", vec!["request.invalid"]));
            }
            _ => {}
        }
        errors.push(("403", forbidden));
        for (status, codes) in errors {
            op["responses"][status] = problem_response("Stable problem details.", &codes);
        }
        paths.entry(route.path).or_insert_with(|| json!({}))[method.as_str()] = op;
    }
}
