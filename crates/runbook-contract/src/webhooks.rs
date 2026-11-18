use super::*;

/// C1 subscription routes, all under `webhooks:manage`.
pub(super) fn register(paths: &mut Map<String, Value>) {
    for route in WEBHOOK_ROUTES {
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
        if route.path.ends_with("/deliveries") {
            parameters.extend([
                json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":50}}),
                json!({"name":"cursor","in":"query","schema":{"type":"string"},"description":"Opaque next_cursor from the previous page (newest first)."}),
            ]);
        }
        let description = match (route.method, route.path) {
            ("POST", "/v1/projects/{p}/webhooks") => {
                "Create a subscription (limit 20 per project). The URL must be https with no credentials and must not resolve to a private, loopback, link-local or unspecified address; self-hosted operators may allow private targets (and plain http). The signing secret is returned once, here; only an encrypted form is stored. A replay of the same key and body answers 409 webhooks.secret_already_issued."
            }
            ("PATCH", _) => {
                "Change name, URL, events (catalog types or [\"*\"]), runbook/tag filters or enabled. Omitted fields are unchanged; the secret changes only through rotate-secret. Re-enabling clears an automatic disable."
            }
            ("DELETE", _) => "Delete the subscription with its deliveries and attempt log.",
            ("POST", "/v1/projects/{p}/webhooks/{id}/test") => {
                "Queue a webhook.test event for this subscription (sent even while disabled). Replays return the saved delivery."
            }
            ("GET", "/v1/projects/{p}/webhooks/{id}/deliveries") => {
                "Delivery log, newest first: every attempt with status, latency and a redacted response snippet. Kept 90 days. Never contains secrets or signatures."
            }
            ("POST", "/v1/projects/{p}/webhooks/{id}/deliveries/{d}/redeliver") => {
                "Send the same event (same event ID and body) again under a new delivery ID with a fresh 24-hour retry window."
            }
            ("POST", "/v1/projects/{p}/webhooks/{id}/rotate-secret") => {
                "Issue a new signing secret, returned once. For 24 hours each delivery carries a v1 signature for both the new and the previous secret. A replay answers 409 webhooks.secret_already_issued."
            }
            _ => "Read subscriptions. Views never include secret material.",
        };
        let mut op = json!({
            "operationId": format!("{}_{}", method, route.path.trim_start_matches('/').replace(['/', '{', '}', '-'], "_")),
            "x-implementation": "implemented",
            "x-required-scopes": ["webhooks:manage"],
            "x-project-role": "admin",
            "summary": description,
            "description": format!("{description} Requires webhooks:manage and the project Manage role (human admin or a service principal of the project). Another organization's project is 404."),
            "security": [{"bearerAuth": []}],
            "parameters": parameters,
            "responses": {}
        });
        let success = if let Some(response) = route.response {
            json!({"description":"Typed response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}})
        } else {
            json!({"description":"No content"})
        };
        op["responses"][route.status.to_string()] = success;
        if let Some(request) = route.request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        let mut errors: Vec<(&str, Vec<&str>)> = vec![
            ("401", vec!["auth.required", "auth.invalid_credential"]),
            ("403", vec!["auth.scope_missing", "auth.forbidden"]),
            ("404", vec!["resource.not_found"]),
            ("405", vec!["request.method_not_allowed"]),
            (
                "503",
                vec!["storage.unavailable", "webhooks.not_configured"],
            ),
        ];
        if route.method == "POST" {
            errors.push(("428", vec!["request.idempotency_key_required"]));
            errors.push((
                "409",
                vec![
                    "request.idempotency_key_reused",
                    "webhooks.secret_already_issued",
                    "webhooks.limit_reached",
                    "webhooks.conflict",
                ],
            ));
        }
        if route.request.is_some() {
            errors.push((
                "422",
                vec![
                    "request.invalid",
                    "webhooks.name_invalid",
                    "webhooks.events_invalid",
                    "webhooks.runbooks_invalid",
                    "webhooks.tags_invalid",
                    "webhooks.url_invalid",
                    "webhooks.url_https_required",
                    "webhooks.url_credentials",
                    "webhooks.target_forbidden",
                    "webhooks.target_unresolved",
                ],
            ));
        }
        if route.path.ends_with("/deliveries") {
            errors.push(("400", vec!["request.invalid_query"]));
        }
        for (status, codes) in errors {
            op["responses"][status] = problem_response("Stable problem details.", &codes);
        }
        paths.entry(route.path).or_insert_with(|| json!({}))[method.as_str()] = op;
    }
}

/// What receivers can rely on (PRD section 9).
pub(super) fn delivery_contract() -> Value {
    json!({
        "envelope": ["id (evt_…)", "type", "version (1)", "created_at", "org", "project", "execution {id, title, runbook, runbook_version_id, runbook_version, url}", "sequence", "data"],
        "events": catalog(),
        "headers": ["Runbook-Event-Id", "Runbook-Event-Type", "Runbook-Delivery-Id", "Runbook-Timestamp", "Runbook-Signature"],
        "signature": {
            "header": "Runbook-Signature",
            "format": "v1=<hex HMAC-SHA256 of \"{timestamp}.{body}\"> keyed by the whsec_ secret string; comma-separated, one per active secret during a 24-hour rotation overlap",
            "tolerance_seconds": 300
        },
        "delivery": "At least once. 2xx within 10 s delivers; timeouts, 5xx, 408 and 429 retry with exponential backoff (30 s doubling, 2 h cap) for 24 hours; other statuses fail at once; redirects are not followed. Deduplicate on the event ID; order one execution's events by sequence. Three days of continuous failure disables the subscription.",
        "credentials": "Payloads never contain credentials. Launchers mint execution credentials with their own service token."
    })
}

fn catalog() -> Value {
    json!([
        "agent.work_available",
        "approval.requested",
        "approval.escalated",
        "approval.expired",
        "approval.decided",
        "step.blocked",
        "step.completed",
        "step.reminder",
        "step.escalated",
        "execution.queued",
        "execution.started",
        "execution.completed",
        "memory.proposed",
        "memory.saved",
        "memory.edited",
        "runbook.published",
        "webhook.test"
    ])
}
