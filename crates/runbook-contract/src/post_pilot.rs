use super::*;

/// T0 contract freeze (plan 2026-11-12 §1): post-pilot operations, all
/// `x-implementation: planned`. The server answers 501 for each until its
/// package ships it. Memory routes are registered by the main table.
pub(super) fn register(paths: &mut Map<String, Value>) {
    for route in PLANNED_ROUTES {
        if route.path.contains("/memory") {
            continue;
        }
        let method = route.method.to_ascii_lowercase();
        let (package, decision, summary, description) = describe(route.method, route.path);
        let mut parameters: Vec<Value> = route
            .path
            .split('/')
            .filter_map(|p| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect();
        if route.method == "POST" {
            parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}}));
        }
        let security = json!([{"bearerAuth":[]},{"cookieAuth":[]}]);
        let mut op = json!({
            "operationId": format!("{}_{}", method, route.path.trim_start_matches('/').replace(['/', '{', '}'], "_")),
            "x-implementation": "planned",
            "x-package": package,
            "x-decision": decision,
            "summary": summary,
            "description": format!("Planned (T0 contract freeze): not implemented; the server answers 501 request.not_implemented and clients must not call it. {description}"),
            "security": security,
            "parameters": parameters,
            "responses": {
                "501": problem_response("Planned operation; not implemented yet.", &["request.not_implemented"]),
            }
        });
        let status = route.status.to_string();
        op["responses"][&status] = match route.response {
            Some(response) => {
                json!({"description":"Planned typed response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}})
            }
            None => json!({"description":"No content"}),
        };
        if let Some(request) = route.request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        for (status, codes) in errors() {
            op["responses"][status] = problem_response("Stable problem details.", codes);
        }
        paths.entry(route.path).or_insert_with(|| json!({}))[method] = op;
    }
}

/// Every planned route's package, decision and text. ARC3 shipped the last
/// T0 routes (the archives), so the list is empty for now.
fn describe(method: &str, path: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    unreachable!("every planned route is described: {method} {path}")
}

fn errors() -> Vec<(&'static str, &'static [&'static str])> {
    let mut out: Vec<(&'static str, &'static [&'static str])> = vec![
        ("404", &["resource.not_found"]),
        ("422", &["request.invalid"]),
        ("503", &["storage.unavailable"]),
    ];
    out.push(("401", &["auth.required", "auth.invalid_credential"]));
    out.push((
        "403",
        &["auth.forbidden", "auth.scope_missing", "auth.csrf"],
    ));
    out
}
