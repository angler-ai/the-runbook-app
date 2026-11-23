use super::*;

/// Who may call a runner route (runner design §12, plan §Shared contracts).
struct Describe {
    summary: &'static str,
    /// `public`, `person`, `bootstrap_token`, `join_token`, `directory_token`,
    /// `claim_token`, `release_token` or `person_or_agent`.
    principal: &'static str,
    scopes: &'static [&'static str],
    errors: &'static [(&'static str, &'static [&'static str])],
}

const COMMON: &[(&str, &[&str])] = &[
    ("401", &["auth.required", "auth.invalid_credential"]),
    ("403", &["auth.forbidden", "auth.scope_missing"]),
    ("404", &["resource.not_found"]),
    ("422", &["request.invalid"]),
    ("503", &["storage.unavailable", "signing.unavailable"]),
];

fn describe(method: &str, path: &str) -> Describe {
    match (method, path) {
        ("GET", "/.well-known/jwks.json") => Describe {
            summary: "The server's Ed25519 public keys (RFC 8037) that sign runner grants (typ rb-grant+jwt) and user assertions (typ rb-user+jwt): one key per purpose, plus keys retired in the last 24 hours. Verifiers accept only alg EdDSA and allow 30 seconds of clock skew. Empty when the server has no secret key to seal its signing keys with.",
            principal: "public",
            scopes: &[],
            errors: &[("503", &["signing.unavailable", "storage.unavailable"])],
        },
        _ => describe_more(method, path),
    }
}

/// The registry (R0-2).
fn describe_more(method: &str, path: &str) -> Describe {
    match (method, path) {
        ("POST", "/v1/orgs/{org}/runner/bootstrap-tokens") => Describe {
            summary: "Create the single-use bootstrap token that registers the organization's runner install at this URL (shown once, valid 24 hours; an earlier unused token stops working). The URL follows the webhook target rules: https, no credentials, never a link-local or metadata host; private addresses only when the operator sets RUNBOOK_RUNNER_PRIVATE_NETWORKS=true. A human org owner or admin with runner:admin and an unrestricted project list.",
            principal: "person",
            scopes: &["runner:admin"],
            errors: &[
                ("409", &["runner.already_registered"]),
                ("422", &["runner.url_invalid", "runner.url_forbidden"]),
            ],
        },
        ("GET", "/v1/orgs/{org}/runner") => Describe {
            summary: "The organization's runner install: online while it heartbeats (offline after 90 seconds without one), none when no runner is registered; its version, groups with their allowed projects, and runners. Any active member with runner:read.",
            principal: "person",
            scopes: &["runner:read"],
            errors: &[],
        },
        ("PUT", "/v1/orgs/{org}/runner/groups/{g}/projects") => Describe {
            summary: "Replace the projects allowed on a runner group. A human org owner or admin with runner:admin.",
            principal: "person",
            scopes: &["runner:admin"],
            errors: &[("422", &["runner.project_unknown"])],
        },
        ("PUT", "/v1/orgs/{org}/runner/vault-identity") => Describe {
            summary: "Record or replace the registered vault identity key and audit the change. An unrestricted human org owner with runner:admin.",
            principal: "person",
            scopes: &["runner:admin"],
            errors: &[("422", &["runner.vault_identity_invalid"])],
        },
        ("POST", "/v1/runner-installs/register") => Describe {
            summary: "The runner's control plane registers with its bootstrap token (single use, bound to the URL it was created for) and receives its directory token (runner:directory), release token (runner:release) and join token, each shown once and stored hashed. One install per organization. May also record the vault identity key supplied through a trusted registration channel.",
            principal: "bootstrap_token",
            scopes: &[],
            errors: &[
                ("409", &["runner.already_registered"]),
                ("403", &["runner.url_mismatch"]),
                (
                    "422",
                    &[
                        "runner.contract_unsupported",
                        "runner.url_invalid",
                        "runner.url_forbidden",
                    ],
                ),
            ],
        },
        ("POST", "/v1/runner-installs/{ri}/runners") => Describe {
            summary: "A runner joins the install with the join token and receives its claim token (runner:claim), which claims only steps for agents of its group. Joining again under the same name in the same group keeps the runner ID and replaces its claim token.",
            principal: "join_token",
            scopes: &[],
            errors: &[("409", &["runner.group_taken"])],
        },
        ("POST", "/v1/runner-installs/{ri}/heartbeat") => Describe {
            summary: "The control plane's heartbeat: group names and each runner's status, running steps and capacity. The install is offline after 90 seconds without one.",
            principal: "directory_token",
            scopes: &[],
            errors: &[],
        },
        _ => describe_later(method, path),
    }
}

/// Routes added after R0-2.
fn describe_later(method: &str, path: &str) -> Describe {
    match (method, path) {
        ("PUT", "/v1/runner-installs/{ri}/agents/{agent_id}") => Describe {
            summary: "Sync one runner agent's current version into the agent directory: name, project, group, status, version and its non-secret ceiling (allowed model profiles, limits, credentials with delivery modes, tools and hosts with labels). The project must be allowed on the agent's group; the name is unique in the project (first writer wins, a rename frees it) and external is reserved. Publish and claim check steps against this entry without calling the runner.",
            principal: "directory_token",
            scopes: &[],
            errors: &[
                ("409", &["directory.name_taken"]),
                (
                    "403",
                    &["directory.project_not_allowed", "directory.group_unknown"],
                ),
                ("422", &["directory.name_reserved"]),
            ],
        },
        ("DELETE", "/v1/runner-installs/{ri}/agents/{agent_id}") => Describe {
            summary: "Remove an agent from the directory; its name is free again.",
            principal: "directory_token",
            scopes: &[],
            errors: &[],
        },
        _ => describe_r04(method, path),
    }
}

/// Routes added after R0-3.
fn describe_r04(method: &str, path: &str) -> Describe {
    match (method, path) {
        ("GET", "/v1/runner-installs/{ri}/projects/{project_id}/skills/{name}") => Describe {
            summary: "Resolve the latest immutable skill folder hash and SKILL.md in a project assigned to this install.",
            principal: "directory_token",
            scopes: &[],
            errors: &[("422", &["skill.too_large"])],
        },
        (_, path) if path.starts_with("/v1/step-runs/{sr}/context/") => Describe {
            summary: match (method, path.rsplit('/').next()) {
                ("GET", Some("run")) => "Overview of the run containing the claimed step.",
                ("GET", Some("step")) => {
                    "Outputs, bounded logs and evidence metadata for one completed step of the same run."
                }
                ("GET", Some("runs")) => {
                    "Up to ten earlier runs of the same runbook in this project, newest first."
                }
                ("GET", Some("past-run")) => {
                    "Overview of one earlier run of the same runbook in this project."
                }
                ("GET", Some("memory")) => "Search active, unexpired runbook and project memory.",
                ("POST", Some("memory")) => {
                    "Propose runbook memory for human review, up to five proposals per step."
                }
                _ => unreachable!(),
            },
            principal: "claim_token",
            scopes: &[],
            errors: &[
                ("400", &["request.invalid_query"]),
                (
                    "409",
                    &["memory.proposal_limit", "request.idempotency_key_reused"],
                ),
                ("428", &["request.idempotency_key_required"]),
            ],
        },
        ("POST", "/v1/runner-installs/{ri}/waiting") => Describe {
            summary: "Why ready steps of this install's agents wait (agent_paused, agent_outside_ceiling, runner_offline or limit:<level>), recorded once per reason on each step and shown on the run page until it is claimed. At most 100 items.",
            principal: "directory_token",
            scopes: &[],
            errors: &[],
        },
        ("POST", "/v1/runner-installs/{ri}/events") => Describe {
            summary: "A runner event (runner.budget_threshold, runner.probe_succeeded, runner.secret_in_response, runner.binding_mismatch or runner.release_denied) with its data (at most 16 KiB), sent as a webhook to every project subscription of that event type in the organization. Internal runner.decision_cost and runner.task_cost reports reconcile settled decision leases and task claims with the step record instead.",
            principal: "directory_token",
            scopes: &[],
            errors: &[],
        },
        ("GET", "/v1/runner-installs/{ri}/groups") => Describe {
            summary: "Read the group and project registry for the directory token's own install.",
            principal: "directory_token",
            scopes: &[],
            errors: &[],
        },
        ("GET", "/v1/runner-installs/{ri}/ready") => Describe {
            summary: "Ready task and decision work aimed at this install's agents in one group, oldest first (limit 1 to 100, default 50): the slow sweep behind the agent.work_available webhook.",
            principal: "directory_token",
            scopes: &[],
            errors: &[],
        },
        (
            "POST",
            "/v1/runner/decisions/{lease}/decide" | "/v1/runner/decisions/{lease}/abstain",
        ) => Describe {
            summary: if path.ends_with("/decide") {
                "Decide the approval under the runner's live decision lease. The decision records runner and agent provenance and evidence references. A closed approval returns approval.not_open."
            } else {
                "Record an agent's reason for abstaining and end its decision lease. The approval remains open for people."
            },
            principal: "claim_token",
            scopes: &[],
            errors: &[
                ("404", &["claim.not_found"]),
                (
                    "409",
                    &[
                        "approval.not_open",
                        "approval.conflict",
                        "approval.deadline_exceeded",
                        "claim.stale_generation",
                        "request.idempotency_key_reused",
                    ],
                ),
                (
                    "422",
                    &["approval.invalid_option", "approval.invalid_evidence"],
                ),
                ("428", &["request.idempotency_key_required"]),
            ],
        },
        ("POST", "/v1/step-runs/{sr}/grants") => Describe {
            summary: "A grant (JWT, typ rb-grant+jwt, EdDSA, aud runner:<runner id>, exp the lease deadline) for the runner's live claim of the step: kind capability for the step itself, or kind credential for one credential the step's connections narrowing and its agent's published ceiling allow with that delivery mode, which records CredentialReleased. Refused once the claim moved on or a person took the step over (409 grant.claim_moved), after the lease lapsed (409 grant.lease_expired), or outside what the step and agent allow (403 grant.not_allowed). Renew by asking again before expiry.",
            principal: "release_token",
            scopes: &[],
            errors: &[
                ("409", &["grant.claim_moved", "grant.lease_expired"]),
                ("403", &["grant.not_allowed"]),
            ],
        },
        ("POST", "/v1/step-runs/{sr}/grants/revoked") => Describe {
            summary: "Record that a credential released to the step was revoked (CredentialRevoked), with the reason.",
            principal: "release_token",
            scopes: &[],
            errors: &[("403", &["grant.not_allowed"])],
        },
        _ => describe_r05(method, path),
    }
}

/// Routes added after R0-4.
fn describe_r05(method: &str, path: &str) -> Describe {
    match path {
        "/v1/runner/oauth/callback/{credential_id}" => Describe {
            summary: "Consume the OAuth provider callback for a runner credential. Exchange requires the initiating browser session and nonce. Redirects to Runner settings without carrying the callback parameters into the redirect.",
            principal: "public",
            scopes: &[],
            errors: &[],
        },
        "/v1/runners/{ri}/proxy/{*path}" => Describe {
            summary: "Proxy to the organization's runner API: {path} (for example runner/v1/groups) must be one of the runner's public routes for this method. Needs the X-Runbook-CSRF: 1 header and a credential holding runner:read or runner:admin (sessions get them by org role). The core forwards the JSON body with a request-bound user assertion (Runbook-Assertion: an rb-user+jwt JWT, EdDSA, 60 seconds, single use, binding method, path and body hash, with the person's org and project roles and runner scopes) and never cookies; the runner authorizes the action. Responses are the runner's, JSON only (an event stream on the transcript route), with nosniff and Content-Security-Policy: sandbox. 503 runner.offline while the runner does not heartbeat.",
            principal: "person_or_agent",
            scopes: &["runner:read", "runner:admin"],
            errors: &[
                ("403", &["auth.csrf"]),
                ("404", &["runner.route_unknown"]),
                (
                    "502",
                    &[
                        "runner.bad_response",
                        "runner.oauth_clock_skew",
                        "runner.unreachable",
                        "runner.url_forbidden",
                    ],
                ),
                ("503", &["runner.offline"]),
                ("504", &["runner.timeout"]),
            ],
        },
        _ => unreachable!("every runner route is described: {method} {path}"),
    }
}

pub(super) fn register(paths: &mut Map<String, Value>) {
    for route in RUNNER_ROUTES {
        let method = route.method.to_ascii_lowercase();
        let d = describe(route.method, route.path);
        let mut parameters: Vec<Value> = route
            .path
            .split('/')
            .filter_map(|p| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .map(|name| {
                let name = name.trim_start_matches('*');
                json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}})
            })
            .collect();
        if route.status == 303 && route.path.starts_with("/v1/runner/oauth/callback/") {
            for name in ["code", "state", "iss", "error"] {
                parameters.push(json!({"name":name,"in":"query","schema":{"type":"string"}}));
            }
        }
        if route.path.contains("/proxy/") {
            parameters.push(json!({"name":"X-Runbook-CSRF","in":"header","required":true,"schema":{"type":"string","const":"1"}}));
        }
        if route.path.contains("/ready") {
            parameters.push(json!({"name":"project","in":"query","schema":{"type":"string"},"description":"Return only ready steps in this project."}));
            parameters.push(json!({"name":"cursor","in":"query","schema":{"type":"string"},"description":"Opaque next_cursor from the previous page, with the same group and project filter."}));
            parameters.push(
                json!({"name":"group","in":"query","required":true,"schema":{"type":"string"}}),
            );
            parameters.push(json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":50}}));
        }
        if route.path.starts_with("/v1/runner/decisions/") {
            parameters.push(json!({"name":"step_run_id","in":"query","required":true,"schema":{"type":"string"},"description":"The approval step run holding the lease."}));
            parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string"}}));
        }
        if route.path.contains("/context/") {
            parameters.push(
                json!({"name":"claim_id","in":"query","required":true,"schema":{"type":"string"}}),
            );
            parameters.push(json!({"name":"generation","in":"query","required":true,"schema":{"type":"integer","minimum":1}}));
            match (route.method, route.path.rsplit('/').next()) {
                ("GET", Some("step")) => parameters.push(json!({"name":"step_run_id","in":"query","required":true,"schema":{"type":"string"}})),
                ("GET", Some("past-run")) => parameters.push(json!({"name":"run_id","in":"query","required":true,"schema":{"type":"string"}})),
                ("GET", Some("runs" | "memory")) => parameters.push(json!({"name":"q","in":"query","schema":{"type":"string","maxLength":512}})),
                ("POST", Some("memory")) => parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string"}})),
                _ => {}
            }
        }
        let security = match d.principal {
            "public" => json!([]),
            "person" | "person_or_agent" => json!([{"bearerAuth":[]},{"cookieAuth":[]}]),
            _ => json!([{"bearerAuth":[]}]),
        };
        let mut op = json!({
            "operationId": format!("{}_{}", method, route.path.trim_start_matches('/').replace(['/', '{', '}', '*', '.', '-'], "_")),
            "x-implementation": "implemented",
            "x-required-principal": d.principal,
            "x-required-scopes": d.scopes,
            "summary": d.summary,
            "description": d.summary,
            "security": security,
            "parameters": parameters,
            "responses": {}
        });
        op["responses"][route.status.to_string()] = match route.response {
            Some(response) => {
                json!({"description":"Typed response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}})
            }
            None if route.path.contains("/context/") => json!({
                "description": "Claim-scoped context, bounded to 48 KiB.",
                "content": {"application/json": {"schema": {"type": "object", "additionalProperties": true}}}
            }),
            None if route.status == 303 => json!({
                "description":"Redirect to Runner settings",
                "headers": {
                    "Location": {"schema":{"type":"string"}},
                    "Cache-Control": {"schema":{"type":"string","const":"no-store"}},
                    "Referrer-Policy": {"schema":{"type":"string","const":"no-referrer"}}
                }
            }),
            None if route.status == 204 => json!({"description":"No content"}),
            None => json!({"description":"The runner's response, passed through"}),
        };
        if let Some(request) = route.request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        let errors: &[(&str, &[&str])] = if d.principal == "public" {
            d.errors
        } else {
            COMMON
        };
        for (status, codes) in errors.iter().chain(if d.principal == "public" {
            [].iter()
        } else {
            d.errors.iter()
        }) {
            let existing = op["responses"][*status]["x-error-codes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut all: Vec<String> = existing
                .iter()
                .filter_map(|c| c.as_str().map(str::to_owned))
                .collect();
            for code in *codes {
                if !all.iter().any(|c| c == code) {
                    all.push((*code).to_owned());
                }
            }
            let all: Vec<&str> = all.iter().map(String::as_str).collect();
            op["responses"][*status] = problem_response("Stable problem details.", &all);
        }
        paths.entry(route.path).or_insert_with(|| json!({}))[method] = op;
    }
}
