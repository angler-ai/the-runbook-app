//! Typed actor payloads; authentication and durable replay precede dispatch.
//! D38: each entry is a complete line, and redactor state ends with the request.
use crate::executions::ExecutionApiError as Error;
use runbook_core::redact::validate_evidence_url;
use runbook_model::*;
use serde::de::DeserializeOwned;
use serde_json::Value;

// Deliberately no Debug: unsanitized payloads must never reach diagnostic logs.
pub enum ActorRequest {
    Claim(ClaimRequest),
    Heartbeat(HeartbeatRequest),
    Logs(LogsRequest),
    Report(ReportRequest),
    Checklist(ChecklistRequest),
    Release(HeartbeatRequest),
    AdminRelease { reason: String },
    Decision(DecisionRequest),
    Patch(ExecutionPatchRequest),
    Signal(SignalRequest),
    Override(OverrideRequest),
}
fn typed<T: DeserializeOwned>(body: Value) -> Result<T, Error> {
    serde_json::from_value(body).map_err(|_| Error::new(422, "request.invalid"))
}
/// Decode only after computing replay identity from the original request body.
/// `step` is the route's step-run ID, not a client-supplied identity override.
pub fn decode(
    op: &str,
    step: &str,
    body: Value,
    evidence_schemes: &[&str],
) -> Result<ActorRequest, Error> {
    let request = decode_typed(op, step, body, evidence_schemes)?;
    capped(&request)?;
    Ok(request)
}

/// CF12b: each value the request records whole in one row is within its
/// cap (`field_caps`), so no store refuses it for its size.
fn capped(request: &ActorRequest) -> Result<(), crate::field_caps::TooLarge> {
    use crate::field_caps::*;
    match request {
        // CF12b review round 1: a line longer than one entry is split by
        // core after redaction (`claims::split_log_lines`), not refused.
        ActorRequest::Logs(_) => Ok(()),
        ActorRequest::Report(r) => {
            optional("/summary", r.summary.as_deref(), TEXT_BYTES)?;
            json("/logs", &r.logs, REPORT_LOG_BYTES)?;
            if r.evidence.len() > EVIDENCE_ENTRIES {
                return Err(TooLarge {
                    path: "/evidence".into(),
                    limit: EVIDENCE_ENTRIES,
                });
            }
            for (i, entry) in r.evidence.iter().enumerate() {
                text(&format!("/evidence/{i}"), entry, EVIDENCE_ENTRY_BYTES)?;
            }
            json("/evidence", &r.evidence, EVIDENCE_TOTAL_BYTES)
        }
        ActorRequest::Claim(r) => actor(Some(&r.session_id), r.actor.as_ref()),
        ActorRequest::Decision(r) => {
            optional("/comment", r.comment.as_deref(), TEXT_BYTES)?;
            actor(r.session_id.as_deref(), r.actor.as_ref())
        }
        ActorRequest::AdminRelease { reason } => text("/reason", reason, TEXT_BYTES),
        ActorRequest::Signal(r) => json("/payload", &r.payload, DATA_BYTES),
        ActorRequest::Patch(r) => {
            optional("/title", r.title.as_deref(), TEXT_BYTES)?;
            json("", r, DATA_BYTES)
        }
        ActorRequest::Override(r) => {
            text("/reason", &r.reason, TEXT_BYTES)?;
            optional("/to/summary", r.to.summary.as_deref(), TEXT_BYTES)?;
            json(
                "/to/outputs",
                &r.to.outputs,
                runbook_core::claims::MAX_STEP_OUTPUT_BYTES,
            )?;
            json("/to", &r.to, DATA_BYTES)
        }
        ActorRequest::Heartbeat(_) | ActorRequest::Checklist(_) | ActorRequest::Release(_) => {
            Ok(())
        }
    }
}

fn decode_typed(
    op: &str,
    step: &str,
    body: Value,
    evidence_schemes: &[&str],
) -> Result<ActorRequest, Error> {
    Ok(match op {
        "claim" => ActorRequest::Claim(typed(body)?),
        "heartbeat" => ActorRequest::Heartbeat(typed(body)?),
        // Core performs the single request-scoped redaction pass immediately
        // before persistence. Keep these bytes intact for that boundary.
        "logs" => ActorRequest::Logs(typed(body)?),
        "report" => {
            let r: ReportRequest = typed(body)?;
            if r.step_run_id.as_str() != step {
                return Err(Error::new(422, "request.step_mismatch"));
            }
            // UP1: an `evd_` ID names an uploaded file (D49); core checks it
            // is a finalized file of this step run. Anything else is a link.
            for url in r
                .evidence
                .iter()
                .filter(|e| e.parse::<EvidenceId>().is_err())
            {
                validate_evidence_url(url, evidence_schemes)
                    .map_err(|_| Error::new(422, "evidence.invalid_url"))?;
            }
            ActorRequest::Report(r)
        }
        "checklist" => ActorRequest::Checklist(typed(body)?),
        "release" => ActorRequest::Release(typed(body)?),
        "decision" => ActorRequest::Decision(typed(body)?),
        "patch" => ActorRequest::Patch(typed(body)?),
        "signal" => ActorRequest::Signal(typed(body)?),
        "override" => ActorRequest::Override(typed(body)?),
        _ => return Err(Error::new(404, "resource.not_found")),
    })
}

/// Carries identity only; the actor transaction owns replay lookup and writes.
#[derive(Debug, PartialEq, Eq)]
pub struct ActorReplay {
    pub key: String,
    pub principal: String,
    pub request_hash: [u8; 32],
}
pub fn post_identity(r: &crate::Request, principal: &str) -> Result<ActorReplay, Error> {
    use sha2::{Digest, Sha256};
    let key = r
        .idempotency_key
        .as_deref()
        .filter(|key| !key.trim().is_empty() && key.len() <= 256)
        .ok_or_else(|| Error::new(428, "request.idempotency_key_required"))?;
    let frame = serde_json::to_vec(&("actor.post", principal, &r.path, key))
        .map_err(|_| Error::new(422, "request.invalid"))?;
    let body = serde_json::to_vec(&r.body).map_err(|_| Error::new(422, "request.invalid"))?;
    Ok(ActorReplay {
        key: runbook_core::authoring::hex(&Sha256::digest(frame)),
        principal: principal.into(),
        request_hash: Sha256::digest(body).into(),
    })
}

/// Payload validation only. The route must separately require a current human
/// project admin and pass that admin's own principal to claims::admin_release.
pub fn admin_release_reason(body: &Value) -> Result<&str, Error> {
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty())
        .ok_or_else(|| Error::new(422, "claim.release_reason_required"))?;
    crate::field_caps::text("/reason", reason, crate::field_caps::TEXT_BYTES)?;
    Ok(reason)
}

/// D26: declaration describes provenance, never authority or principal identity.
pub fn validate_declaration(
    kind: ActorKind,
    session: Option<&str>,
    declaration: Option<&ActorDeclaration>,
    claim: bool,
) -> Result<(), Error> {
    let mut diagnostics = Vec::new();
    let mut invalid = |path: &str, message: &str| {
        diagnostics.push(Diagnostic {
            path: path.into(),
            code: "request.invalid_actor".into(),
            message: message.into(),
            line: None,
            column: None,
        })
    };
    if (claim || kind == ActorKind::Agent) && session.is_none_or(|s| s.trim().is_empty()) {
        invalid("/session_id", "A nonblank session ID is required.");
    }
    if declaration.is_some_and(|a| a.kind != kind) {
        invalid("/actor/kind", "Actor kind must match the credential.");
    }
    if kind == ActorKind::Agent
        && declaration
            .and_then(|a| a.agent_name.as_deref())
            .is_none_or(|s| s.trim().is_empty())
    {
        invalid("/actor/agent_name", "An agent name is required.");
    }
    if declaration
        .and_then(|a| a.model.as_deref())
        .is_some_and(|m| m.trim().is_empty())
    {
        invalid("/actor/model", "Model must be nonblank when supplied.");
    }
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(Error {
            status: 422,
            code: "request.invalid_actor",
            diagnostics,
        })
    }
}
