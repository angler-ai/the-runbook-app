use super::*;
pub(super) fn register(paths: &mut Map<String, Value>) {
    for (method, path, request, response, status) in [
        (
            "post",
            "/v1/executions/{exe}/tokens",
            Some("MintTokenRequest"),
            "ExecutionTokenPair",
            "201",
        ),
        (
            "post",
            "/v1/executions/{exe}/tokens/refresh",
            Some("RefreshRequest"),
            "ExecutionTokenPair",
            "201",
        ),
        (
            "delete",
            "/v1/executions/{exe}/credentials/{family}",
            None,
            "",
            "200",
        ),
        (
            "get",
            "/v1/executions/{exe}/next",
            None,
            "ResumeBrief",
            "200",
        ),
        ("get", "/v1/step-runs/{sr}", None, "ClaimedStepRead", "200"),
        (
            "get",
            "/v1/step-runs/{sr}/skill-files",
            None,
            "SkillFileRead",
            "200",
        ),
    ] {
        let mut params: Vec<Value> = path
            .split('/')
            .filter_map(|p| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect();
        if method == "post" {
            params.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}}));
        }
        if path.ends_with("skill-files") {
            params.push(json!({"name":"path","in":"query","schema":{"type":"string","default":"SKILL.md"},"description":"Relative path in the claimed step's immutable pinned manifest; never a caller-selected hash."}));
        }
        let mut op = json!({"operationId":format!("{}_{}",method,path.replace(['/', '{', '}'],"_")),"x-implementation":"postgres","security":[{"bearerAuth":[]}],"parameters":params,"responses":{},"description":if method=="get"{"D32 execution-token reads only, bound to the owning execution and currently live claim. The brief exposes completed summaries/outputs/decisions and ready references, never future instructions. Step and skill reads require a current claim held by this credential family. PAT/service read expansion is a later task."}else{"D30/D31: mint requires current operator-or-above and executions:act; queued 409 execution.not_started. Secrets returned only once after commit, never in replay or audit. Same principal/key/body mint returns 409 credentials.already_issued; changed body 409 request.idempotency_key_reused. Refresh authenticates its body token independently of bearer access, never replays a pair, and reuse revokes the whole family before 409 credentials.refresh_reused. Revoke requires current executions:act and either the minting principal or a human project admin. Execution tokens cannot mint/revoke families."}});
        if path.ends_with("/refresh") {
            op["security"] = json!([]);
        }
        if method == "get" {
            // B8a: the same reads for human/agent PATs and service tokens.
            op["description"] = json!(
                "B8a/D32 step-at-a-time reads. Execution tokens: only their own run; step and skill reads require a current claim held by the credential family (403 otherwise). Human PATs, agent PATs and service tokens: executions:read on the run's project (missing scope is 403 auth.scope_missing); the run or step run is found by an org-scoped lookup and anything outside the caller's projects or organization is 404 resource.not_found. Step and skill reads disclose a task once it has been offered to an actor (ready, claimed, waiting after a report, or finished); a step skipped by its condition answers 409 step.not_ready, and pending steps have no step run ID. The brief never includes future instructions."
            );
            op["x-required-scopes"] = json!(["executions:read"]);
            op["x-principals"] = json!(["human_pat", "agent_pat", "service", "execution"]);
            if path.ends_with("/next") {
                op["x-audience"] =
                    json!("human PAT: human; agent PAT, service or execution token: agent");
            }
        }
        if let Some(request) = request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        op["responses"][status] = json!({"description":"Committed response; credentials are shown once.","headers":{"Cache-Control":{"schema":{"type":"string","const":"no-store"}}},"content":{"application/json":{"schema":if response.is_empty(){json!({"type":"null"})}else{json!({"$ref":format!("#/components/schemas/{response}")})}}}});
        if method == "get" && path == "/v1/step-runs/{sr}" {
            op["description"] = json!(format!(
                "{} Runner claim tokens also read their leased approval as decision work.",
                op["description"].as_str().unwrap_or_default()
            ));
            op["responses"]["200"]["content"]["application/json"]["schema"] = json!({
                "oneOf": [
                    {"$ref":"#/components/schemas/ClaimedStepRead"},
                    {"$ref":"#/components/schemas/ClaimedDecisionRead"}
                ]
            });
            op["x-principals"]
                .as_array_mut()
                .expect("GET principal list")
                .push(json!("claim_token"));
        }
        for (code, codes) in [
            (
                "401",
                vec![
                    "auth.invalid_credential",
                    "credentials.invalid",
                    "credentials.expired",
                    "credentials.revoked",
                ],
            ),
            ("403", vec!["auth.forbidden", "credentials.forbidden"]),
            ("404", vec!["resource.not_found"]),
            ("405", vec!["request.method_not_allowed"]),
            (
                "409",
                vec![
                    "execution.not_started",
                    "execution.closed",
                    "credentials.already_issued",
                    "credentials.refresh_reused",
                    "request.idempotency_key_reused",
                ],
            ),
            (
                "422",
                if path.ends_with("/tokens") && method == "post" {
                    vec!["request.invalid", "request.value_too_large"]
                } else {
                    vec!["request.invalid"]
                },
            ),
            ("428", vec!["request.idempotency_key_required"]),
            ("503", vec!["storage.unavailable"]),
        ] {
            op["responses"][code] = problem_response("Stable credential/lifecycle error.", &codes);
        }
        if method == "get" && path.starts_with("/v1/step-runs") {
            op["responses"]["409"] = problem_response(
                "The step was never offered to an actor (D32), or is not a task.",
                &["step.not_ready", "step.not_task", "step.not_renderable"],
            );
            op["responses"]["403"] = problem_response(
                "Execution token without a current claim, or a credential lacking executions:read.",
                &["auth.forbidden", "auth.scope_missing"],
            );
        }
        paths.entry(path).or_insert_with(|| json!({}))[method] = op;
    }
    let read = &mut paths["/v1/executions/{exe}"]["get"];
    read["responses"]["200"]["content"]["application/json"]["schema"] = json!({"oneOf":[{"$ref":"#/components/schemas/ExecutionView"},{"$ref":"#/components/schemas/ResumeBrief"}]});
    read["x-execution-token-policy"] = json!(
        "D32: an execution token receives only ResumeBrief for its own run. PAT/service receives ExecutionView. Every access rechecks issuer validity and per-org membership."
    );
    for path in ["/v1/executions/{exe}/events", "/v1/executions/{exe}/report"] {
        paths[path]["get"]["x-execution-token-policy"] = json!(
            "Always 403 auth.forbidden, including the token's own run. No sanitized stream or report variant."
        );
    }
}
