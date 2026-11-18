use super::*;

/// AUTH2 (D45): organization invitations; AUTH3 adds the device grant.
pub(super) fn register(paths: &mut Map<String, Value>) {
    for route in INVITATION_ROUTES {
        let method = route.method.to_ascii_lowercase();
        let mut parameters: Vec<Value> = route
            .path
            .split('/')
            .filter_map(|p| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect();
        let description = match route.method {
            "GET" => {
                parameters.extend([
                    json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":100}}),
                    json!({"name":"cursor","in":"query","schema":{"type":"string"},"description":"Opaque next_cursor from the previous page."}),
                ]);
                "List the organization's invitations, newest first: pending, accepted, revoked and expired (a pending invitation past its expiry)."
            }
            "POST" => {
                parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}}));
                "Invite an email address with an org role (default member) and project roles (project ID or slug), valid 7 days. Signing in with that verified email consumes the invitation and applies its roles, never lowering a role already held (D45). Sign-in is invitation-only. An admin cannot invite an owner (D17). Retrying the same invitation answers it again; a different pending invitation for the email is 409 invitation.exists, and an email that already belongs to a member is 409 invitation.already_member."
            }
            _ => {
                "Revoke a pending invitation. Unknown, foreign, accepted or already revoked is 404."
            }
        };
        let mut op = json!({
            "operationId": format!("{}_{}", method, route.path.trim_start_matches('/').replace(['/', '{', '}'], "_")),
            "x-implementation": "implemented",
            "x-package": "AUTH2",
            "x-decision": "D45",
            "x-required-scopes": ["project:admin"],
            "x-required-principal": "human",
            "summary": description,
            "description": format!("{description} Requires a human org owner or admin through an org-wide credential holding project:admin (a personal token or a browser session)."),
            "security": [{"bearerAuth":[]},{"cookieAuth":[]}],
            "parameters": parameters,
            "responses": {}
        });
        op["responses"][route.status.to_string()] = match route.response {
            Some(response) => {
                json!({"description":"Typed response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}})
            }
            None => json!({"description":"Revoked"}),
        };
        if let Some(request) = route.request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        let mut errors: Vec<(&str, Vec<&str>)> = vec![
            ("401", vec!["auth.required", "auth.invalid_credential"]),
            (
                "403",
                vec!["auth.forbidden", "auth.scope_missing", "auth.csrf"],
            ),
            ("404", vec!["resource.not_found"]),
            ("405", vec!["request.method_not_allowed"]),
            ("503", vec!["storage.unavailable", "auth.unavailable"]),
        ];
        match route.method {
            "POST" => {
                errors.push((
                    "409",
                    vec!["invitation.exists", "invitation.already_member"],
                ));
                errors.push(("422", vec!["request.invalid", "request.value_too_large"]));
                errors.push(("428", vec!["request.idempotency_key_required"]));
            }
            "GET" => {
                errors.push(("400", vec!["cursor.invalid"]));
                errors.push(("422", vec!["request.invalid"]));
            }
            _ => {}
        }
        for (status, codes) in errors {
            op["responses"][status] = problem_response("Stable problem details.", &codes);
        }
        paths.entry(route.path).or_insert_with(|| json!({}))[method.as_str()] = op;
    }
}

/// AUTH3 (D47): the RFC 8628 device grant.
pub(super) fn register_device(paths: &mut Map<String, Value>) {
    for route in DEVICE_ROUTES {
        let (summary, description, security) = match route.path {
            "/v1/device/code" => (
                "Start a device login (RFC 8628 device authorization)",
                "No credential. Returns a high-entropy device_code for the CLI to poll with and a short user_code (8 characters from BCDFGHJKLMNPQRSTVWXZ23456789, shown as XXXX-XXXX) for the person to enter at verification_uri; both last 10 minutes and are stored only as SHA-256 hashes. scopes (PRD §9 names) is what the CLI asks for; omitted asks for every scope, and the approver narrows it (`runbook login` asks for a narrow default set). The client address (behind a reverse proxy listed in RUNBOOK_TRUSTED_PROXIES, the client it names) is recorded with client_name for the approval page. At most 20 codes per hour per client address (429 limit.rate with retry_after). Exists only when sign-in is configured (otherwise 404); /.well-known/runbook then names it as auth.device_authorization_endpoint.",
                json!([]),
            ),
            "/v1/device/token" => (
                "Poll a device login (RFC 8628 token request)",
                "No credential. grant_type must be urn:ietf:params:oauth:grant-type:device_code. Until a decision: 400 with code device.authorization_pending; device.slow_down when polled sooner than the interval (the interval grows 5 s, to at most 60 s, and the body's interval says the new seconds); device.expired_token for an unknown, expired or already used code; device.access_denied when the person denied it. The body also carries the RFC 8628 error name in `error`. Exactly one poll after approval returns an ordinary personal access token (trb_pat_, hash-only, listed in /v1/tokens and revocable; default 90 days), named after the client.",
                json!([]),
            ),
            "/v1/device/lookup" => (
                "Show a pending device login before deciding",
                "A signed-in person in a browser (session cookie plus CSRF; bearer tokens are refused with 403) reads a pending user code (case and dashes ignored): the client_name the CLI sent (client_name_verified is always false: any client can send any name, so show it as the client's claim), the address that requested the code (requester_ip, null when unknown) and the scopes asked for. Unknown, expired or decided codes are 404 and count as misses with approve (more than 5 a minute is 429 limit.rate). The approval page must never approve or deny from its query string: ?user_code= only fills the form, and the person submits it.",
                json!([{"cookieAuth":[]}]),
            ),
            _ => (
                "Approve or deny a device login",
                "A signed-in person in a browser (session cookie plus CSRF; bearer tokens are refused with 403) approves or denies a user code (case and dashes ignored). Approval issues the CLI a token for this person: scopes default to those the CLI asked for that the person's roles allow, and an explicit list must stay within both (outside the request: 422; beyond the person: 403 auth.forbidden). projects (IDs or slugs the person can see) narrow it further; expires_at_ms defaults to 90 days and may not pass one year. Unknown, expired or already decided codes are 404; more than 5 misses a minute is 429 limit.rate.",
                json!([{"cookieAuth":[]}]),
            ),
        };
        let mut parameters = Vec::new();
        if route.path == "/v1/device/approve" {
            parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}}));
        }
        if matches!(route.path, "/v1/device/approve" | "/v1/device/lookup") {
            parameters.push(json!({"name":"X-Runbook-CSRF","in":"header","required":true,"schema":{"type":"string","const":"1"},"description":"D46: required on cookie-authenticated unsafe methods, with Origin equal to the server origin."}));
        }
        let mut op = json!({
            "operationId": format!("post_{}", route.path.trim_start_matches('/').replace('/', "_")),
            "x-implementation": "implemented",
            "x-package": "AUTH3",
            "x-decision": "D47",
            "summary": summary,
            "description": description,
            "security": security,
            "parameters": parameters,
            "requestBody": {"required": route.path != "/v1/device/code","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{}", route.request.expect("typed request"))}}}},
            "responses": {}
        });
        op["responses"][route.status.to_string()] = match route.response {
            Some(response) => {
                json!({"description":"Typed response","headers":{"Cache-Control":{"schema":{"type":"string","const":"no-store"}}},"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}})
            }
            None => json!({"description":"Decided"}),
        };
        let mut errors: Vec<(&str, Vec<&str>)> = vec![
            ("404", vec!["resource.not_found"]),
            ("422", vec!["request.invalid"]),
            ("503", vec!["storage.unavailable", "server.entropy"]),
        ];
        match route.path {
            "/v1/device/code" => errors.push(("429", vec!["limit.rate"])),
            "/v1/device/token" => errors.push((
                "400",
                vec![
                    "device.authorization_pending",
                    "device.slow_down",
                    "device.expired_token",
                    "device.access_denied",
                    "request.invalid",
                ],
            )),
            "/v1/device/lookup" => {
                errors.push(("401", vec!["auth.required", "auth.invalid_credential"]));
                errors.push(("403", vec!["auth.forbidden", "auth.csrf"]));
                errors.push(("429", vec!["limit.rate"]));
            }
            _ => {
                errors.push(("401", vec!["auth.required", "auth.invalid_credential"]));
                errors.push(("403", vec!["auth.forbidden", "auth.csrf"]));
                errors.push(("428", vec!["request.idempotency_key_required"]));
                errors.push(("429", vec!["limit.rate"]));
            }
        }
        for (status, codes) in errors {
            op["responses"][status] = problem_response("Stable problem details.", &codes);
        }
        paths.entry(route.path).or_insert_with(|| json!({}))["post"] = op;
    }
}

/// D46: every `/v1` operation a person reaches with a bearer token also
/// accepts the browser session cookie, except those a session may not use:
/// minting or revoking execution credentials (D30/D31 need a stored issuing
/// token), refreshing them (the body authenticates) and revoking the
/// presenting token (`DELETE /v1/me/token`; sessions sign out through
/// `/api/auth/sign-out`).
pub(super) fn cookie_alternatives(paths: &mut Map<String, Value>) {
    for (path, operations) in paths.iter_mut() {
        let excluded = !path.starts_with("/v1/")
            || path.starts_with("/v1/executions/{exe}/tokens")
            || path.starts_with("/v1/executions/{exe}/credentials");
        for (method, op) in operations.as_object_mut().into_iter().flatten() {
            if excluded || (path == "/v1/me/token" && method == "delete") {
                continue;
            }
            let Some(security) = op.get_mut("security").and_then(Value::as_array_mut) else {
                continue;
            };
            let bearer = security.iter().any(|r| r.get("bearerAuth").is_some());
            let cookie = security.iter().any(|r| r.get("cookieAuth").is_some());
            if bearer && !cookie {
                security.push(json!({"cookieAuth": []}));
            }
        }
    }
}
