use super::*;
pub(super) fn register(paths: &mut Map<String, Value>) {
    for (method, path) in [
        ("post", "/v1/step-runs/{sr}/claim"),
        ("post", "/v1/step-runs/{sr}/heartbeat"),
        ("post", "/v1/step-runs/{sr}/logs"),
        ("post", "/v1/step-runs/{sr}/report"),
        ("post", "/v1/step-runs/{sr}/checklist"),
        ("post", "/v1/step-runs/{sr}/release"),
        ("post", "/v1/step-runs/{sr}/decisions"),
        ("post", "/v1/step-runs/{sr}/overrides"),
        ("patch", "/v1/executions/{exe}"),
        ("post", "/v1/executions/{exe}/signals/{name}"),
    ] {
        let op = &mut paths.get_mut(path).expect("actor draft path")[method];
        op["x-implementation"] = json!("postgres");
        op["description"] = json!(
            "Native actor transactions. Current PAT/service role, scope and project authorization precedes replay. Every POST requires Idempotency-Key; same principal/path/key/body returns the saved response even after step completion, changed payload is 409. State changes and replay commit together. PATCH is idempotent and outside POST replay. Agent/service credentials never become human signatures. X-Request-ID accompanies success and problem responses. CF13: a change whose run would keep a value past what one storage row holds (2,000,000 bytes on the Cloudflare edition; the native edition has no such limit) is refused with 422 execution.row_too_large and nothing is written; retrying cannot succeed, unlike 503 storage.unavailable. CF13 review round 1: a run's event log is held to a budget on both editions (8 MiB unless RUNBOOK_LIMIT_RUN_BYTES sets another); a request whose own event carries more than 4 KiB and would take the run past it is refused with 422 execution.too_large and nothing is written. Smaller writes (a claim, a release, a cancel, a short report) always pass, so a run at its budget can still be finished or cancelled."
        );
        op["x-required-scopes"] = json!([if method == "patch" || path.ends_with("/overrides") {
            "executions:admin"
        } else {
            "executions:act"
        }]);
        for (status, codes) in [
            ("401", vec!["auth.required", "auth.invalid_credential"]),
            (
                "403",
                vec![
                    "auth.forbidden",
                    "auth.scope_missing",
                    "approval.not_eligible",
                ],
            ),
            ("404", vec!["resource.not_found"]),
            ("405", vec!["request.method_not_allowed"]),
            (
                "409",
                vec![
                    "request.idempotency_key_reused",
                    "claim.stale_generation",
                    "execution.not_started",
                    "execution.closed",
                    "execution.paused",
                    "execution.deadline_exceeded",
                    "step.deadline_exceeded",
                    "approval.not_open",
                    "approval.deadline_exceeded",
                    "signal.not_waiting",
                ],
            ),
            (
                "422",
                vec![
                    "request.invalid",
                    "request.invalid_actor",
                    "request.step_mismatch",
                    "claim.release_reason_required",
                    "evidence.invalid_url",
                    "request.value_too_large",
                    // CF13: a row the run's storage cannot hold (runbook_core::ROW_TOO_LARGE).
                    "execution.row_too_large",
                    // CF13 review round 1: the run budget (runbook_core::run_budget).
                    "execution.too_large",
                ],
            ),
            ("503", vec!["storage.unavailable"]),
        ] {
            let mut codes = codes;
            // LANG7: strict assignment and take-over refusals on claim.
            if path.ends_with("/claim") {
                codes.extend(match status {
                    "403" => vec!["claim.not_assignee"],
                    "409" => vec!["step.not_claimable", "claim.take_over_live_claim"],
                    "422" => vec!["claim.actor_mismatch", "claim.take_over_not_allowed"],
                    _ => vec![],
                });
            }
            op["responses"][status] =
                problem_response("Stable problem code; no mutation on rejection.", &codes);
        }
        if method == "post" {
            op["responses"]["428"] = problem_response(
                "Idempotency-Key is required.",
                &["request.idempotency_key_required"],
            );
            for p in op["parameters"].as_array_mut().unwrap() {
                if p["name"] == "Idempotency-Key" {
                    p["schema"]["maxLength"] = json!(256);
                }
            }
        }
        let response_schema = if path.ends_with("/claim") {
            json!({"$ref":"#/components/schemas/ClaimRef"})
        } else if path.ends_with("/decisions") {
            json!({"$ref":"#/components/schemas/DecisionRecord"})
        } else if path.ends_with("/overrides") {
            json!({"$ref":"#/components/schemas/Amendment"})
        } else if path.ends_with("/heartbeat") {
            json!({"type":["integer","null"],"description":"Agent lease deadline in epoch milliseconds; null for human claims."})
        } else if path.ends_with("/logs") {
            json!({"type":"integer","minimum":0,"description":"Committed log sequence."})
        } else {
            json!({"type":"null"})
        };
        op["responses"]["200"]["content"]["application/json"]["schema"] = response_schema;
        if path.ends_with("/decisions") || path.ends_with("/claim") {
            op["x-actor-provenance"] = json!(
                "D26: agent credentials require nonblank session_id and actor.agent_name. Claim and decision share declaration validation; 422 request.invalid_actor includes field errors. Principal comes only from the credential. A human credential cannot declare actor.kind=agent. Agent decisions return DecisionRecord by=agent with agent_name, model and session_id, never a human signature; replays of decisions recorded before that variant existed return the original by=system, policy agent:<name> shape."
            );
        }
        if path.ends_with("/claim") {
            op["x-claim-policy"] = json!(
                "LANG7: an assignee is guidance unless the task sets `assignment: strict`; then only the assignee (the user, or an active member of the team) with project operator-or-higher access may claim, else 403 claim.not_assignee. On an `actor: any` or `actor: human` task the assignee is a person, so an agent principal always gets claim.not_assignee (and is not offered the step); on an `actor: agent` task agents still claim by `actor`, and the assignee binds only a person taking it over. A strict assignee that resolved to nobody is resolved again against the live directory at claim time (never more broadly); when it now names the claimant, StepAssigneeResolved is recorded before StepClaimed. A person claims an `actor: agent` task only with `take_over: true`, and only while it is ready (never claimed, or its agent claim expired or was released): a live claim answers 409 claim.take_over_live_claim (a project admin may release it first); an agent principal, or a task that is not `actor: agent`, answers 422 claim.take_over_not_allowed. A take-over appends StepTakenOver after StepClaimed."
            );
        }
        if method == "patch" {
            op["x-lifecycle-policy"] = json!(
                "D39: queued runs allow cancellation only (terminal, atomically dequeued, no cleanup); other queued PATCH changes return 409 execution.not_started. Terminal metadata changes return 409 execution.closed; terminal cancel is an idempotent no-op."
            );
        }
        if path.ends_with("/overrides") {
            op["x-amendment-policy"] = json!(
                "D35: human project admin only (executions:admin). Amends a finished step's verdict, outputs and summary with a mandatory reason; approvals cannot be amended and status cannot change. The original record is kept, evaluated consumers become stale, and a completed run never reopens. A retry with the same key replays the saved Amendment after authorization; agent, service and execution credentials are rejected."
            );
        }
        if path.ends_with("/release") {
            op["x-release-policy"] = json!(
                "Owner supplies the current claim reference. Admin supplies a nonblank reason and must be a human project admin; audit uses the actual admin and advances the generation. No owner impersonation."
            );
        }
        if path.ends_with("/logs") || path.ends_with("/report") {
            op["x-log-framing"] = json!(
                "D38: each logs entry is a complete line; embedded newlines split lines. Redact the entire request in order, adding a newline per entry, finish at the request boundary, then persist redacted lines. No cross-request redactor state."
            );
        }
    }
}
