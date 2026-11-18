use super::*;
pub(super) fn register(paths: &mut Map<String, Value>) {
    register_log_reads(paths);
    for route in EXECUTION_ROUTES.iter().chain(STREAM_ROUTES) {
        let method = route.method.to_ascii_lowercase();
        let op = &mut paths.get_mut(route.path).expect("A3 execution route")[&method];
        op["x-implementation"] = json!("implemented");
        op["summary"] = json!(if route.path.ends_with("/rerun") {
            "Rerun an execution"
        } else if route.method == "POST" {
            "Start an execution"
        } else {
            "Read an execution"
        });
        op["x-required-scopes"] = json!([if route.method == "POST" {
            "executions:start"
        } else {
            "executions:read"
        }]);
        op["description"] = json!(
            "Current PAT/service authorization intersects project role, scope and project allow-list. Start requires operator or above and Idempotency-Key. Same principal/key/payload replays the original start response; changed payload is 409, missing key 428. Version is pinned before queueing. Admission is separate from engine status: status=queued with admission.state=admitted is awaiting durable activation. Admission time anchors the deadline. A full concurrency pool follows the version's on_conflict (LANG3): queue waits; reject is 409 execution.concurrency_conflict with nothing written (the key stays unspent); cancel_previous, in the same transaction, cancels every waiter and the holder (with limit 1; the oldest holder otherwise), records superseded_by on them, and queues this run first in line, admitted as soon as a cancelled holder closes (at once when it has no cleanup left). Its other 409s write nothing: execution.concurrency_too_many_holders (over 50 runs to cancel), execution.concurrency_holder_unavailable (errors name the run), execution.concurrency_contended (the pool kept changing). GET reads current state; POST replay does not change as the execution advances."
        );
        for status in ["401", "403", "404", "405", "409", "422", "428", "503"] {
            op["responses"][status] =
                problem_response("Stable problem details and X-Request-ID.", &[]);
        }
        op["responses"]["404"] = problem_response(
            "Unknown or foreign execution/runbook/version.",
            &["resource.not_found", "runbook.version_not_found"],
        );
        if route.method == "POST" {
            op["parameters"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .filter(|p| p["name"] == "Idempotency-Key")
                .for_each(|p| p["schema"]["maxLength"] = json!(256));
            op["responses"]["409"] = problem_response(
                "Idempotency-Key already used with a different payload.",
                &["request.idempotency_key_reused"],
            );
            op["responses"]["428"] = problem_response(
                "Idempotency-Key required.",
                &["request.idempotency_key_required"],
            );
        }
        op["responses"]["400"] = problem_response(
            "Invalid cursor/query or unindexed field.",
            &[
                "request.invalid_cursor",
                "request.invalid_query",
                "query.field_not_indexed",
            ],
        );
        if let Some(response) = route.response {
            op["responses"][route.status.to_string()] = json!({"description":"Typed execution response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}});
        }
        if route.path.ends_with("/report") || route.path.ends_with("/rerun") {
            op["responses"]["410"] = problem_response(
                "The run is past retention (D33); only its tombstone remains.",
                &["execution.history_expired"],
            );
        }
        if route.path.ends_with("/report") {
            op["summary"] = json!("Read the execution document");
            op["parameters"].as_array_mut().unwrap().push(json!({"name":"format","in":"query","required":false,"schema":{"type":"string","enum":["json","markdown","junit","pdf"],"default":"json"},"description":"json returns ExecutionDocumentView; markdown returns the same document rendered as text/markdown (custom report only once terminal, untrusted text escaped). REP2: junit returns it as JUnit XML (application/xml, a testcase per step, actors and decisions named, the custom report as the suite's system-out once terminal); pdf returns the Markdown rendered as a PDF (application/pdf, deterministic bytes). junit and pdf are downloads with Content-Disposition: attachment; filename=\"<exe>-report.xml|pdf\". Other values are 400 request.invalid_query."}));
            op["responses"]["200"]["content"]["text/markdown"] =
                json!({"schema":{"type":"string"}});
            op["responses"]["200"]["content"]["application/xml"] =
                json!({"schema":{"type":"string"}});
            op["responses"]["200"]["content"]["application/pdf"] =
                json!({"schema":{"type":"string","format":"binary"}});
            op["responses"]["200"]["headers"] = json!({"Content-Disposition":{"description":"Present for format=junit and format=pdf: attachment; filename=\"<exe>-report.xml\" or \"<exe>-report.pdf\".","schema":{"type":"string"}}});
            op["description"] = json!(
                "B8d/D34: executions:read. One document from a single consistent revision backs the live JSON view and the terminal report: sections in execution order with step path, actor, verdict, outputs, evidence, approval decisions (system policy is never shown as a human signature), memory-read refs, amendment and staleness markers, and paged log links. Untrusted text stays raw in JSON. Execution tokens are refused (D32)."
            );
        }
        if route.path == runbook_model::TRIGGER_PATH {
            op["summary"] = json!("Call a webhook trigger");
            op["description"] = json!(
                "LANG9b: starts a run of the runbook's latest published version from this webhook trigger (`spec.triggers.webhook`). The body is the caller's own JSON payload: fields the trigger's `payload` schema doesn't declare are ignored, the rest are checked (422 trigger.invalid_payload names each field), `when` decides whether to start (200, started false, reason when), and `inputs` maps the payload to the runbook's inputs. The run records webhook:<id> as who started it. A call whose `dedupe` value (else Idempotency-Key, else none) matches an earlier call returns that run (200, duplicate true). Needs executions:start on the project, as a token in Authorization or, for senders that can't set headers, in ?token= (use a service token limited to this project). Unknown runbook or trigger is 404."
            );
            op["responses"]["200"] = json!({"description":"Not started (when false) or a duplicate of an earlier call","content":{"application/json":{"schema":{"$ref":"#/components/schemas/TriggerResponse"}}}});
            op["responses"]["404"] = problem_response(
                "Unknown runbook, no published version, or unknown trigger.",
                &["resource.not_found", "trigger.not_found"],
            );
            op["responses"]["422"] = problem_response(
                "The payload doesn't match its schema, or an expression failed.",
                &[
                    "trigger.invalid_payload",
                    "trigger.evaluation_failed",
                    "request.value_too_large",
                ],
            );
            // Optional here: `dedupe` (or nothing) keys a call.
            if let Some(responses) = op["responses"].as_object_mut() {
                responses.remove("428");
            }
            if let Some(parameters) = op["parameters"].as_array_mut() {
                for p in parameters
                    .iter_mut()
                    .filter(|p| p["name"] == "Idempotency-Key")
                {
                    p["required"] = json!(false);
                }
            }
        }
        if route.method == "POST" && route.path == "/v1/projects/{p}/executions" {
            // U10: invalid start data shares rerun's code and field errors.
            op["description"] = json!(format!(
                "{} Inputs or fields that do not satisfy the pinned version's schemas return 422 execution.inputs_invalid with one field error per problem (`path` under `inputs.`/`fields.`, `code` such as data.required, data.type_error, data.enum, data.range or data.unknown_field, and `message`), create nothing and leave the Idempotency-Key unspent, so a corrected retry of the same action may reuse it. Other start failures (an unusable snapshot, concurrency key or vars initialization) return 422 start.invalid. CF13: on the Cloudflare edition each of the new run's first rows is measured; one past what a storage row holds (2,000,000 bytes) is refused with 422 execution.row_too_large and nothing is written (the start record's own bound, request.value_too_large, usually refuses such a start first).",
                op["description"].as_str().unwrap()
            ));
            op["responses"]["422"] = problem_response(
                "Start data is invalid for the pinned version.",
                &[
                    "execution.inputs_invalid",
                    "start.invalid",
                    "request.value_too_large",
                    // CF13 review round 1: a first row past one object row.
                    "execution.row_too_large",
                ],
            );
        }
        if route.path.ends_with("/rerun") {
            op["description"] = json!(
                "B9 rerun: executions:start on the original run's project and a required Idempotency-Key (D20, namespaced to this original). The new run reuses the original's resolved inputs and initial fields (never amended or later-edited fields) on the original's pinned version unless `version` selects another; it gets a new identity and rerun_of, goes through D24/D25 admission, and copies no credentials, claims or signatures. Data invalid for the target version returns 422 execution.inputs_invalid with field errors and creates nothing. Unknown or foreign versions are 404 runbook.version_not_found. ARC2 (PRD §14): once retention has reduced the original to its summary row (live detail deleted, the run kept in its daily archive), the original inputs are gone and the rerun returns 422 execution.inputs_expired; a run deleted for any other reason is 410 execution.history_expired. CF13: on the Cloudflare edition a new run's first row past one storage row is 422 execution.row_too_large, as for a start."
            );
            op["responses"]["422"] = problem_response(
                "The original's data is invalid for the target version, or retention reduced the original to its summary row, or the new run's start record would pass one row (2,000,000 bytes less 64 KiB, with the target version's defaults: request.value_too_large).",
                &[
                    "execution.inputs_invalid",
                    "execution.inputs_expired",
                    "request.value_too_large",
                    // CF13 review round 1: a first row past one object row.
                    "execution.row_too_large",
                ],
            );
        }
        if route.method == "GET" && route.path == "/v1/projects/{p}/executions" {
            op["summary"] = json!("Search project runs");
            op["description"] = json!(
                "PAT/service executions:read and project read permission. Newest creation time first, then execution ID. Cursors bind scope and filters. All tags must match. field.<name>=<JSON scalar> tests equality; field.<name>.min=<number> is inclusive and .max=<number> exclusive. With runbook, field indexing is validated against its latest published schema, even before any run stores a value. Unknown query parameters are rejected. Time values are UTC epoch milliseconds, [from,to)."
            );
            for name in [
                "status", "runbook", "tags", "actor", "text", "from", "to", "cursor", "limit",
            ] {
                op["parameters"].as_array_mut().unwrap().push(json!({"name":name,"in":"query","schema":{"type":"string"},"description":match name {"status"=>"Comma-separated execution statuses", "tags"=>"Comma-separated tags (all of)","runbook"=>"Runbook ID or slug","limit"=>"Page size 1..100 (default 100)",_=>"Optional search filter"}}));
            }
        }
        if route.path.ends_with("/events") {
            op["summary"] = json!("Stream committed execution events");
            op["description"] = json!(
                "PAT/service executions:read and project read permission. SSE id is the committed sequence; event is the domain event name; data is the JSON event envelope (execution_id, seq, at_ms, actor, event). Subscribe before reading history; notifications only trigger committed reads. Access rechecked on every event and every 30 seconds while idle; authorization loss closes the stream. The terminal event is delivered and then the stream closes. Retained history unavailable at connect returns 410 after authorization; during streaming an error event closes the stream. Slow clients may be disconnected; reconnect using the last received ID."
            );
            op["parameters"].as_array_mut().unwrap().push(json!({"name":"Last-Event-ID","in":"header","schema":{"type":"string","pattern":"^[0-9]+$"},"description":"Last committed sequence received (default 0). A future sequence returns 400."}));
            op["responses"]["200"] = json!({"description":"SSE committed events and keep-alive comments","content":{"text/event-stream":{"schema":{"type":"string"}}}});
            op["responses"]["410"] = problem_response(
                "Authorized history is beyond retention.",
                &["execution.history_expired"],
            );
            // F48: stream admission limits.
            op["responses"]["429"] = problem_response(
                "This credential already holds its maximum number of open streams (default 4). Close one or wait for it to end.",
                &["stream.too_many"],
            );
            op["responses"]["503"] = problem_response(
                "The server's stream capacity is full or live notifications are unavailable; retry later.",
                &[
                    "stream.capacity",
                    "stream.unavailable",
                    "storage.unavailable",
                ],
            );
            op["x-stream-lifetime"] = json!(
                "A stream closes after the server's stream lifetime (default one hour) even when the run is still open; reconnect with Last-Event-ID to resume without a gap."
            );
        }
    }
}

/// F46: the paged log read behind every document section's `logs.href`.
fn register_log_reads(paths: &mut Map<String, Value>) {
    for route in STEP_LOG_ROUTES {
        let op = &mut paths.get_mut(route.path).expect("log read path")["get"];
        op["x-implementation"] = json!("implemented");
        op["summary"] = json!("Read a step run's log lines");
        op["description"] = json!(
            "F46/D33: executions:read on the run's project (PAT or service token). Lines are the persisted, already redacted (D34/D38) log lines of the step run, in sequence order from 1. cursor is the last sequence read (default 0); the page holds up to limit lines after it (1-1000, default 100). next_cursor is the last sequence returned, or the request cursor when none followed, so a reader can poll a running step; has_more says another page is ready. Unknown query parameters are 400. Execution tokens are refused with 403 (D32), as for events. Step runs of another organization or of a project the caller cannot read are 404. A run purged by retention answers 410 after authorization."
        );
        op["x-required-scopes"] = json!(["executions:read"]);
        op["x-execution-token-policy"] =
            json!("Always 403 auth.forbidden, including the token's own run.");
        op["parameters"].as_array_mut().unwrap().extend([
            json!({"name":"cursor","in":"query","schema":{"type":"string","pattern":"^[0-9]+$"},"description":"Last sequence read; default 0."}),
            json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":1000,"default":100}}),
        ]);
        for (status, description, codes) in [
            (
                "400",
                "Invalid cursor, limit or unknown query parameter.",
                &["request.invalid_query"][..],
            ),
            (
                "401",
                "Missing or invalid credential.",
                &["auth.required", "auth.invalid_credential"],
            ),
            (
                "403",
                "Missing executions:read, or an execution token.",
                &["auth.scope_missing", "auth.forbidden"],
            ),
            (
                "404",
                "Unknown or foreign step run.",
                &["resource.not_found"],
            ),
            (
                "410",
                "The run is past retention (D33).",
                &["execution.history_expired"],
            ),
            ("503", "Storage unavailable.", &["storage.unavailable"]),
        ] {
            op["responses"][status] = problem_response(description, codes);
        }
        op["responses"].as_object_mut().unwrap().remove("422");
    }
}
