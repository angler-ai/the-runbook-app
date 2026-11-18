use super::*;
pub(super) fn register(paths: &mut Map<String, Value>) {
    for route in AUTHORING_ROUTES {
        let method = route.method.to_ascii_lowercase();
        let mut op = paths
            .get(route.path)
            .and_then(|p| p.get(&method))
            .cloned()
            .unwrap_or_else(|| json!({"parameters":[],"responses":{}}));
        op["x-implementation"] = json!("implemented");
        op["summary"] = json!("Public authoring and catalog operation");
        op["description"] = json!(
            "Current PAT/service authorization intersects project role, scope and project allow-list. Mutating POST replay lasts 24h and includes If-Match; authorization precedes replay and replay precedes stale preconditions. Compilation and immutable blobs precede a short atomic catalog/replay transaction. Execution credentials cannot use authoring routes. Draft save accepts invalid YAML and returns diagnostics; publication requires validity and the fixed slug. Source size overflow returns 413 spec.source_limit. No UI-only handlers."
        );
        op["operationId"] = json!(format!(
            "{}_{}",
            method,
            route
                .path
                .trim_start_matches('/')
                .replace(['/', '{', '}'], "_")
        ));
        op["security"] = json!([{"bearerAuth":[]}]);
        let scope = if route.path.contains("/skills") {
            if route.method == "GET" {
                "skills:read"
            } else {
                "skills:write"
            }
        } else if route.method == "GET" {
            "runbooks:read"
        } else if route.path.ends_with("/versions") {
            "runbooks:publish"
        } else {
            "runbooks:write"
        };
        op["x-required-scopes"] = json!([scope]);
        let mut parameters:Vec<Value>=route.path.split('/').filter_map(|p|p.strip_prefix('{').and_then(|p|p.strip_suffix('}')).map(|name|json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))).collect();
        if route.method == "POST" && route.path != "/v1/validate" {
            parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}}));
        }
        if route.method == "PATCH" || route.method == "POST" && route.path.ends_with("/versions") {
            parameters.push(json!({"name":"If-Match","in":"header","required":true,"schema":{"type":"string","pattern":"^\"[^\",]+\"$"},"description":"Exact strong ETag selecting the expected draft revision. Missing 428; wildcard/weak/mismatched/stale 412. A prior successful POST replays before this precondition."}));
        }
        if route
            .response
            .is_some_and(|v| v.starts_with("DirectoryPage"))
        {
            parameters.extend([json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":100}}),json!({"name":"cursor","in":"query","schema":{"type":"string"},"description":"Opaque scope/filter-bound cursor; mismatches return 400 request.invalid_cursor."})]);
        }
        if route.method == "GET" && route.path == "/v1/projects/{p}/runbooks" {
            parameters.extend([json!({"name":"q","in":"query","schema":{"type":"string"},"description":"UI-1: case-insensitive substring over title, slug, description and tags; each field is matched within its first 2,000 characters (UI-1b). Empty means absent. Bound into the cursor. A filtered page may be shorter than limit, even empty, while next_cursor is set (the Workers edition bounds the work per request): follow next_cursor until it is null; a short page is not the end of the list."}),json!({"name":"tag","in":"query","schema":{"type":"string"},"description":"UI-1: comma-separated tags the runbook must all carry (exact match after trimming). Write a comma inside a tag as \\, and a backslash as \\\\. Empty means absent. Bound into the cursor."})]);
        }
        if route.method == "GET" && route.path == "/v1/projects/{p}/runbooks/{slug}" {
            op["description"] = json!(
                "UI-1c: the project's runbook whose slug is exactly {slug} (case-sensitive; the project by ID or slug), as GET /v1/runbooks/{rb} answers it: the same view, with last_run and active_runs only when the credential also holds executions:read on the project. 404 resource.not_found when no runbook of the project has that slug. Requires runbooks:read."
            );
            if let Some(slug) = parameters.iter_mut().find(|p| p["name"] == "slug") {
                slug["description"] = json!("The runbook's slug (metadata.id), not its rb_ ID.");
            }
        }
        if route.method == "GET" && route.path.ends_with("/versions") {
            parameters.extend([json!({"name":"preview","in":"query","schema":{"type":"boolean","default":false}}),json!({"name":"branch","in":"query","schema":{"type":"string"},"description":"Only with preview=true; never changes latest published."})]);
        }
        if route.method == "GET" && route.path == "/v1/projects/{p}/skills/{hash}" {
            parameters.push(json!({"name":"files","in":"query","schema":{"type":"boolean","default":false},"description":"true adds each file's base64 `content` (CLI1 `runbook skill pull`). Any value other than true or false is 400 request.invalid_query."}));
        }
        if route.path == "/v1/validate" {
            op["description"] = json!(
                "Never saves; no Idempotency-Key required. Optional project ID/slug resolves project pins and declarations and requires runbooks:read. Without project, a current runbooks:write or runbooks:read credential compiles with empty project context, so project references yield diagnostics. Source size overflow returns 413 spec.source_limit."
            );
            op["x-context-scopes"] = json!({"with_project":["runbooks:read"],"without_project_any_of":["runbooks:read","runbooks:write"]});
        }
        op["parameters"] = json!(parameters);
        for status in [
            "400", "401", "403", "404", "405", "409", "412", "413", "415", "422", "428", "503",
            "default",
        ] {
            if op["responses"].get(status).is_none() {
                op["responses"][status] =
                    problem_response("Stable problem details and X-Request-ID.", &[]);
            }
        }
        op["responses"]["403"]["x-error-codes"] = json!(["auth.scope_missing"]);
        // CF12b review round 1: more than 200 files, or a reference past
        // 64 KiB (`field_caps`).
        if route.method == "POST" && route.path == "/v1/projects/{p}/skills" {
            op["responses"]["422"]["x-error-codes"] = json!(["request.value_too_large"]);
        }
        op["responses"][route.status.to_string()] = json!({"description":"Typed public response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{}",route.response.unwrap())}}},"headers":{"X-Request-ID":{"schema":{"type":"string"}},"X-RateLimit-Limit":{"schema":{"type":"string"}}}});
        if route.path.ends_with("/draft")
            || route.method == "POST" && route.path.ends_with("/runbooks")
        {
            op["responses"][route.status.to_string()]["headers"]["ETag"] = json!({"schema":{"type":"string"},"description":"Strong opaque draft revision ETag"});
        }
        if let Some(request) = route.request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        paths.entry(route.path).or_insert_with(|| json!({}))[method] = op;
    }
}
