//! ARC3: daily archive routes (PRD §14 "Retention and daily archives";
//! D53–D54), implemented.
use super::problem_response;
use runbook_model::ARCHIVE_ROUTES;
use serde_json::{Map, Value, json};

pub(super) fn register(paths: &mut Map<String, Value>) {
    for route in ARCHIVE_ROUTES {
        let mut parameters: Vec<Value> = route
            .path
            .split('/')
            .filter_map(|p| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .map(|name| json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect();
        let (summary, description) = match route.path {
            "/v1/runbooks/{rb}/archives" => {
                parameters.extend([
                    json!({"name":"from","in":"query","schema":{"type":"string","format":"date"},"description":"First UTC day, inclusive (YYYY-MM-DD)."}),
                    json!({"name":"to","in":"query","schema":{"type":"string","format":"date"},"description":"Last UTC day, inclusive (YYYY-MM-DD)."}),
                    json!({"name":"format","in":"query","schema":{"type":"string","enum":["json","zip"],"default":"json"},"description":"zip downloads from..to (both required, at most 92 days) as one bundle."}),
                    json!({"name":"include_evidence","in":"query","schema":{"type":"boolean","default":false},"description":"zip only: add the runs' evidence objects that are still held."}),
                ]);
                (
                    "List a runbook's daily archive files",
                    "Implemented on Postgres (ARC3). Requires executions:read on the runbook's project; a runbook outside the caller's reach is 404. One item per UTC day with a file, newest first, within the optional inclusive from/to dates: run count, size, SHA-256, write time, whether an amendment or deletion awaits the next daily rewrite (stale), and the expiry under the project's current retention (null keeps it indefinitely, the self-hosted default). format=zip returns a bundle (application/zip; attachment `<rb>-<from>-<to>.zip`): bundle.json, each day's file as `<YYYY-MM-DD>.zip`, and with include_evidence=true the evidence objects still held as `evidence/<exe>/<evd_id>` (objects of runs whose live detail retention deleted are listed with file null). Unknown parameters, bad dates, from after to, a zip without both dates, or include_evidence without zip are 400 request.invalid_query; no file in range is 404; a stale day in range is 409 archive.rewrite_pending with Retry-After; over 92 days is 422 archive.range_too_large; over 512 MiB is 422 archive.bundle_too_large. Evidence follows the archive (PRD §14): objects stay after live detail is deleted and go when the file expires or the run is deleted on request.",
                )
            }
            "/v1/runbooks/{rb}/archives/{date}" => {
                parameters.push(json!({"name":"include_evidence","in":"query","schema":{"type":"boolean","default":false},"description":"Return a one-day bundle with the evidence objects still held instead of the bare file."}));
                (
                    "Download one day's archive file",
                    "Implemented on Postgres (ARC3). Requires executions:read on the runbook's project. The day's ZIP exactly as stored (D53: deterministic bytes; manifest.json, one folder per run with report.md, execution.json, events.ndjson and logs/, and project/, the portable context), as an attachment `<rb>-<YYYY-MM-DD>.zip`. It imports through the D3 portable importer. With include_evidence=true, a one-day bundle as in the list route's zip format. An unknown runbook, a malformed date or a day without a file is 404. A stale day answers 409 archive.rewrite_pending with Retry-After until the daily job rewrites it.",
                )
            }
            _ => (
                "Open an archived run read-only",
                "Implemented on Postgres (ARC3). Requires executions:read on the run's project. Reads the run's summary row and only its folder of the day's file, by range through the ZIP index (PRD §14), whether its live detail still exists or retention deleted it: version, status, title, creator, times, the file's date and the folder, its files with sizes and SHA-256, its evidence list, and the archived Markdown report. format=markdown returns just that report as text/markdown. A run without a summary row (not archived yet, deleted on request, or past summary retention) is 404; a summary row whose file has expired is 410 execution.history_expired.",
            ),
        };
        if route.path == "/v1/executions/{exe}/archive" {
            parameters.push(json!({"name":"format","in":"query","schema":{"type":"string","enum":["json","markdown"],"default":"json"}}));
        }
        let mut op = json!({
            "operationId": format!("get_{}", route.path.trim_start_matches('/').replace(['/', '{', '}'], "_")),
            "x-implementation": "implemented",
            "x-package": "ARC3",
            "x-decision": "D53",
            "x-required-scopes": ["executions:read"],
            "summary": summary,
            "description": description,
            "security": [{"bearerAuth":[]},{"cookieAuth":[]}],
            "parameters": parameters,
            "responses": {}
        });
        op["responses"]["200"] = match route.response {
            Some(response) => {
                let mut ok = json!({"description":"OK","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}});
                if route.path == "/v1/runbooks/{rb}/archives" {
                    ok["content"]["application/zip"] =
                        json!({"schema":{"type":"string","contentMediaType":"application/zip"}});
                }
                if route.path == "/v1/executions/{exe}/archive" {
                    ok["content"]["text/markdown"] = json!({"schema":{"type":"string"}});
                }
                ok
            }
            None => {
                json!({"description":"The day's ZIP (D53 layout)","content":{"application/zip":{"schema":{"type":"string","contentMediaType":"application/zip"}}}})
            }
        };
        for (status, description, codes) in [
            ("400", "Invalid query.", &["request.invalid_query"][..]),
            (
                "401",
                "Missing or invalid credential.",
                &["auth.required", "auth.invalid_credential"][..],
            ),
            (
                "403",
                "Missing executions:read.",
                &["auth.scope_missing", "auth.csrf"][..],
            ),
            (
                "404",
                "Unknown, foreign or absent.",
                &["resource.not_found"][..],
            ),
            ("405", "Only GET.", &["request.method_not_allowed"][..]),
            ("503", "Storage unavailable.", &["storage.unavailable"][..]),
        ] {
            op["responses"][status] = problem_response(description, codes);
        }
        if route.path == "/v1/runbooks/{rb}/archives"
            || route.path == "/v1/runbooks/{rb}/archives/{date}"
        {
            op["responses"]["422"] = problem_response(
                "The bundle is over its day or size limit.",
                &["archive.range_too_large", "archive.bundle_too_large"],
            );
            let mut pending = problem_response(
                "A day in the download is stale (an amendment, late evidence or a single-run deletion since it was written) and may still hold a run deleted on request; it is not served until the daily job rewrites it.",
                &["archive.rewrite_pending"],
            );
            pending["headers"] = json!({"Retry-After":{"description":"Seconds until the job's next scheduled pass.","schema":{"type":"integer"}}});
            op["responses"]["409"] = pending;
        } else {
            op["responses"]["410"] = problem_response(
                "The run's file has expired; only its summary row remains.",
                &["execution.history_expired"],
            );
        }
        paths.entry(route.path).or_insert_with(|| json!({}))["get"] = op;
    }
}
