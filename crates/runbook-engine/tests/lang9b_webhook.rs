//! LANG9b: a webhook trigger's call: payload reduced to its declared fields
//! and checked, `when` filters, `inputs` maps, `dedupe` names the call.
use runbook_engine::{WebhookError, WebhookOutcome, webhook_start};
use runbook_spec::{CompileContext, compile, parse};
use serde_json::json;

fn trigger() -> runbook_spec::ir::WebhookTrigger {
    let source = r#"metadata: {id: deploy-check}
spec:
  inputs:
    service: {type: string, required: true}
    version: {type: string, default: latest}
  triggers:
    webhook:
      - id: on_deploy
        payload:
          status: {type: string, required: true}
          repository: {type: object, properties: {name: {type: string}}}
          ref: {type: string}
          deployment: {type: object, properties: {id: {type: integer}}}
        when: "${{ payload.status == 'success' }}"
        inputs:
          service: "${{ payload.repository.name }}"
          version: "${{ payload.ref }}"
        dedupe: "${{ payload.deployment.id }}"
  steps:
    - {id: check, title: Check}
"#;
    let c = compile(&parse(source).unwrap(), &CompileContext::default()).unwrap();
    c.ir().triggers.webhook[0].clone()
}

#[test]
fn a_matching_call_maps_its_payload_to_inputs() {
    let payload = json!({
        "status": "success",
        "repository": {"name": "api", "private": true},
        "ref": "v2.0.1",
        "deployment": {"id": 42, "sha": "abc"},
        "sender": {"login": "someone"}
    });
    assert_eq!(
        webhook_start(&trigger(), &payload, 0).unwrap(),
        WebhookOutcome::Start {
            inputs: [
                ("service".into(), json!("api")),
                ("version".into(), json!("v2.0.1"))
            ]
            .into_iter()
            .collect(),
            dedupe: Some("42".into()),
        }
    );
}

#[test]
fn when_false_starts_nothing() {
    let payload = json!({"status": "failure", "repository": {"name": "api"}});
    assert_eq!(
        webhook_start(&trigger(), &payload, 0).unwrap(),
        WebhookOutcome::Skipped
    );
}

#[test]
fn an_absent_optional_field_leaves_the_input_to_its_default() {
    let payload = json!({"status": "success", "repository": {"name": "api"}});
    let WebhookOutcome::Start { inputs, dedupe } = webhook_start(&trigger(), &payload, 0).unwrap()
    else {
        panic!("expected a start");
    };
    assert_eq!(inputs.get("version"), None);
    assert_eq!(dedupe, None);
}

#[test]
fn a_payload_that_breaks_its_schema_is_refused_with_field_errors() {
    let payload = json!({"repository": {"name": 5}});
    let Err(WebhookError::InvalidPayload(errors)) = webhook_start(&trigger(), &payload, 0) else {
        panic!("expected invalid payload");
    };
    assert!(
        errors.iter().all(|e| e.path.starts_with("payload")),
        "{errors:?}"
    );
    assert!(!errors.is_empty());
}
