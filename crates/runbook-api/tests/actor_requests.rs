use runbook_api::actor_requests::{ActorRequest, decode};
use serde_json::json;
fn step() -> String {
    format!("sr_{:026}", 1)
}
fn claim() -> serde_json::Value {
    json!({"id":format!("clm_{:026}",1),"generation":1})
}
#[test]
fn actor_log_payloads_are_preserved_for_core_redaction() {
    let body = json!({"claim":claim(),"logs":["password=hunter2\nhello", "-----BEGIN PRIVATE KEY-----", "sensitive", "-----END PRIVATE KEY-----"]});
    let ActorRequest::Logs(r) = decode("logs", &step(), body.clone(), &[]).unwrap() else {
        panic!()
    };
    assert_eq!(serde_json::to_value(r.logs).unwrap(), body["logs"]);
    let ActorRequest::Logs(r) = decode(
        "logs",
        &step(),
        json!({"claim":claim(),"logs":["ordinary"]}),
        &[],
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(r.logs, vec!["ordinary"]);
}
#[test]
fn actor_report_path_and_links_are_validated_before_mutation() {
    let mut body = json!({"step_run_id":step(),"claim":claim(),"status":"succeeded","evidence":["https://example.com/check"],"logs":["token=hidden"]});
    let ActorRequest::Report(r) = decode("report", &step(), body.clone(), &[]).unwrap() else {
        panic!()
    };
    assert_eq!(r.logs, ["token=hidden"]);
    body["step_run_id"] = json!(format!("sr_{:026}", 2));
    assert_eq!(
        decode("report", &step(), body.clone(), &[])
            .err()
            .unwrap()
            .code,
        "request.step_mismatch"
    );
    body["step_run_id"] = json!(step());
    body["evidence"] = json!(["javascript:alert(1)"]);
    assert_eq!(
        decode("report", &step(), body, &["javascript"])
            .err()
            .unwrap()
            .code,
        "evidence.invalid_url"
    );
}
#[test]
fn actor_claim_fences_require_nonzero_generation() {
    for op in ["heartbeat", "logs", "release", "checklist"] {
        let body =
            json!({"claim":{"id":format!("clm_{:026}",1),"generation":0},"logs":[],"checklist":[]});
        assert_eq!(decode(op, &step(), body, &[]).err().unwrap().status, 422);
    }
}
#[test]
fn actor_post_identity_binds_principal_route_and_original_payload() {
    use runbook_api::{Request, actor_requests::post_identity};
    let mut r = Request {
        method: "POST".into(),
        path: format!("/v1/step-runs/{}/logs", step()),
        query: Default::default(),
        body: json!({"claim":claim(),"logs":["password=first"]}),
        credential: None,
        idempotency_key: Some("retry".into()),
        request_id: "req_test".into(),
        new_id: "unused".into(),
        if_match: None,
        client_ip: None,
    };
    let a = post_identity(&r, "human:alice").unwrap();
    assert_eq!(a, post_identity(&r, "human:alice").unwrap());
    assert_ne!(a.key, post_identity(&r, "human:bob").unwrap().key);
    r.body["logs"] = json!(["password=second"]);
    let b = post_identity(&r, "human:alice").unwrap();
    assert_eq!(a.key, b.key);
    assert_ne!(a.request_hash, b.request_hash);
    r.path.push_str("/other");
    assert_ne!(a.key, post_identity(&r, "human:alice").unwrap().key);
    r.idempotency_key = None;
    assert_eq!(post_identity(&r, "human:alice").err().unwrap().status, 428);
}

#[test]
fn admin_release_requires_an_explicit_nonblank_reason() {
    use runbook_api::actor_requests::admin_release_reason;
    for body in [
        json!({}),
        json!({"reason":null}),
        json!({"reason":12}),
        json!({"reason":" \n\t"}),
    ] {
        let error = admin_release_reason(&body).unwrap_err();
        assert_eq!(error.status, 422);
        assert_eq!(error.code, "claim.release_reason_required");
    }
    let body = json!({"reason":"Owner is unavailable"});
    assert_eq!(admin_release_reason(&body).unwrap(), "Owner is unavailable");
    // Owner release still requires its current claim reference, not an admin reason.
    assert!(matches!(
        decode("release", &step(), json!({"claim":claim()}), &[]).unwrap(),
        ActorRequest::Release(_)
    ));
}

#[test]
fn agent_declarations_require_provenance_and_reject_human_spoofing() {
    use runbook_api::actor_requests::validate_declaration;
    use runbook_model::{ActorDeclaration, ActorKind};
    let agent = ActorDeclaration {
        kind: ActorKind::Agent,
        agent_name: Some("bot".into()),
        model: None,
    };
    let error = validate_declaration(ActorKind::Agent, None, Some(&agent), false).unwrap_err();
    assert_eq!(error.status, 422);
    assert!(error.diagnostics.iter().any(|e| e.path == "/session_id"));
    assert!(validate_declaration(ActorKind::Human, Some("web"), Some(&agent), false).is_err());
    assert!(validate_declaration(ActorKind::Agent, Some("session"), Some(&agent), false).is_ok());
    assert!(validate_declaration(ActorKind::Human, None, None, false).is_ok());
    assert!(validate_declaration(ActorKind::Human, Some("  "), None, true).is_err());
}

/// CF12b (finding 3): every value an actor request records whole in one
/// row is capped, alike on both editions, with `422 request.value_too_large`
/// naming its path; a value at its cap is decoded.
mod caps {
    use super::*;
    use runbook_api::{actor_requests::admin_release_reason, field_caps::*};
    use serde_json::Value;

    fn refused(op: &str, body: Value) -> String {
        let e = decode(op, &step(), body, &[]).err().expect("refused");
        assert_eq!((e.status, e.code), (422, CODE), "{op}");
        e.diagnostics[0].path.clone()
    }
    fn decoded(op: &str, body: Value) {
        if let Err(e) = decode(op, &step(), body, &[]) {
            panic!("{op}: {} {:?}", e.code, e.diagnostics);
        }
    }
    fn s(n: usize) -> String {
        "a".repeat(n)
    }
    fn report(extra: Value) -> Value {
        let mut body = json!({"step_run_id": step(), "claim": claim(), "status": "succeeded"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    }

    /// CF12b review round 1 (finding 1): a line longer than one log entry
    /// is not refused; core stores it as several entries after redaction
    /// (`claims::split_log_lines`), on both editions.
    #[test]
    fn a_long_log_line_is_decoded() {
        decoded("logs", json!({"claim": claim(), "logs": [s(TEXT_BYTES)]}));
        decoded(
            "logs",
            json!({"claim": claim(), "logs": ["ok", s(4 * TEXT_BYTES)]}),
        );
        decoded("report", report(json!({"logs": ["ok", s(4 * TEXT_BYTES)]})));
    }

    #[test]
    fn a_report() {
        decoded("report", report(json!({"summary": s(TEXT_BYTES)})));
        assert_eq!(
            refused("report", report(json!({"summary": s(TEXT_BYTES + 1)}))),
            "/summary"
        );
        // Logs: each line, and together as JSON.
        // 16 lines of 65,532 bytes: 1,048,561 bytes as a JSON array.
        let lines = vec![s(TEXT_BYTES - 4); REPORT_LOG_BYTES / TEXT_BYTES];
        assert_eq!(serde_json::to_vec(&lines).unwrap().len(), 1_048_561);
        decoded("report", report(json!({ "logs": lines })));
        let mut over = lines.clone();
        over.push(s(13));
        assert_eq!(refused("report", report(json!({ "logs": over }))), "/logs");
        // Evidence: how many links, and each.
        let link = |n: usize| format!("https://example.com/{}", s(n));
        decoded(
            "report",
            report(json!({"evidence": vec![link(1); EVIDENCE_ENTRIES]})),
        );
        assert_eq!(
            refused(
                "report",
                report(json!({"evidence": vec![link(1); EVIDENCE_ENTRIES + 1]}))
            ),
            "/evidence"
        );
        assert_eq!(
            refused(
                "report",
                report(json!({"evidence": [link(EVIDENCE_ENTRY_BYTES)]}))
            ),
            "/evidence/0"
        );
        // CF12b review round 1 (finding 6): a presigned link with a session
        // token passes 2 KiB; one of 8 KiB is decoded.
        assert_eq!(EVIDENCE_ENTRY_BYTES, 8_192);
        let presigned = link(EVIDENCE_ENTRY_BYTES - link(0).len());
        assert_eq!(presigned.len(), EVIDENCE_ENTRY_BYTES);
        decoded("report", report(json!({ "evidence": [presigned.clone()] })));
        // All of a report's evidence together stays within its share of the
        // event's row.
        let many = vec![presigned; EVIDENCE_TOTAL_BYTES / EVIDENCE_ENTRY_BYTES + 1];
        assert_eq!(
            refused("report", report(json!({ "evidence": many }))),
            "/evidence"
        );
    }

    #[test]
    fn a_decision_and_a_claim() {
        decoded(
            "decision",
            json!({"option": "go", "comment": s(TEXT_BYTES)}),
        );
        assert_eq!(
            refused(
                "decision",
                json!({"option": "go", "comment": s(TEXT_BYTES + 1)})
            ),
            "/comment"
        );
        assert_eq!(
            refused(
                "decision",
                json!({"option": "go", "session_id": s(NAME_BYTES + 1)})
            ),
            "/session_id"
        );
        decoded(
            "claim",
            json!({"session_id": s(NAME_BYTES), "actor": {"kind": "agent", "agent_name": s(NAME_BYTES), "model": s(NAME_BYTES)}}),
        );
        assert_eq!(
            refused("claim", json!({"session_id": s(NAME_BYTES + 1)})),
            "/session_id"
        );
        assert_eq!(
            refused(
                "claim",
                json!({"session_id": "s", "actor": {"kind": "agent", "agent_name": "a", "model": s(NAME_BYTES + 1)}})
            ),
            "/actor/model"
        );
        assert_eq!(
            refused(
                "claim",
                json!({"session_id": "s", "actor": {"kind": "agent", "agent_name": s(NAME_BYTES + 1)}})
            ),
            "/actor/agent_name"
        );
    }

    #[test]
    fn run_data_a_signal_a_patch_and_an_override() {
        // `{"k":"…"}` is 8 bytes more than its string.
        decoded("signal", json!({"payload": {"k": s(DATA_BYTES - 8)}}));
        assert_eq!(
            refused("signal", json!({"payload": {"k": s(DATA_BYTES - 7)}})),
            "/payload"
        );
        decoded("patch", json!({"fields": {"k": s(TEXT_BYTES)}}));
        assert_eq!(
            refused("patch", json!({"title": s(TEXT_BYTES + 1)})),
            "/title"
        );
        let fields: serde_json::Map<String, Value> = (0..5)
            .map(|i| (format!("k{i}"), json!(s(TEXT_BYTES - 100))))
            .collect();
        assert_eq!(refused("patch", json!({ "fields": fields })), "");
        let to = |summary: usize, outputs: usize| json!({"status": "succeeded", "outputs": {"k": s(outputs)}, "summary": s(summary)});
        decoded("override", json!({"to": to(1, 1), "reason": s(TEXT_BYTES)}));
        assert_eq!(
            refused(
                "override",
                json!({"to": to(1, 1), "reason": s(TEXT_BYTES + 1)})
            ),
            "/reason"
        );
        assert_eq!(
            refused(
                "override",
                json!({"to": to(TEXT_BYTES + 1, 1), "reason": "r"})
            ),
            "/to/summary"
        );
        // The report route's 64 KiB of outputs holds for an override too.
        assert_eq!(
            refused("override", json!({"to": to(1, 65_536), "reason": "r"})),
            "/to/outputs"
        );
    }

    #[test]
    fn an_admin_release_reason() {
        assert!(admin_release_reason(&json!({"reason": s(TEXT_BYTES)})).is_ok());
        let e = admin_release_reason(&json!({"reason": s(TEXT_BYTES + 1)})).unwrap_err();
        assert_eq!((e.status, e.code), (422, CODE));
    }
}
