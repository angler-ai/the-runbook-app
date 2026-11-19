//! SLACK1 stage 2: `POST /v1/slack/interactions` as the hosts hand it over.
#[path = "support/fake_backends.rs"]
mod fake_backends;
use fake_backends::{Fake, router, run};
use runbook_api::http::{Headers, decode};
use runbook_core::slack::SlackApp;
use serde_json::{Value, json};

const SECRET: &str = "signing-secret";

fn app() -> SlackApp {
    SlackApp {
        bot_token: "xoxb-test".into(),
        signing_secret: SECRET.into(),
        default_channel: None,
    }
}

/// Slack's form body around `payload`, signed at `ts` with `secret`.
fn slack_request(payload: &Value, ts: &str, secret: &str) -> runbook_api::Request {
    let raw = format!("payload={}", url_encode(&payload.to_string()));
    let mac = runbook_core::webhooks::hmac_sha256(
        secret.as_bytes(),
        &[b"v0:", ts.as_bytes(), b":", raw.as_bytes()],
    );
    let signature = format!(
        "v0={}",
        mac.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let mut headers = Headers::new();
    headers.push("content-type", "application/x-www-form-urlencoded");
    headers.push("x-slack-request-timestamp", ts);
    headers.push("x-slack-signature", signature);
    decode(
        "POST",
        "/v1/slack/interactions",
        None,
        &headers,
        raw.as_bytes(),
        None,
        "req_test",
        "00000000000000000000000001".into(),
    )
    .expect("Slack's form body decodes")
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn click(option: &str) -> Value {
    json!({
        "type": "block_actions",
        "user": {"id": "U0SLACK1", "team_id": "T1"},
        "response_url": "https://hooks.slack.com/actions/T1/1/abc",
        "actions": [{
            "action_id": format!("runbook.decide.{option}"),
            "action_ts": "1.0",
            "value": json!({
                "org": format!("org_{:026}", 1),
                "sr": format!("sr_{:026}", 9),
                "option": option,
            }).to_string(),
        }],
    })
}

#[test]
fn without_a_slack_app_the_endpoint_does_not_exist() {
    let r = router(false);
    let response = run(r.handle(slack_request(&click("yes"), "1", SECRET)));
    assert_eq!(response.status, 404);
}

#[test]
fn a_bad_or_stale_signature_is_refused() {
    let r = router(false);
    r.backends.slack.borrow_mut().app = Some(app());
    let forged = run(r.handle(slack_request(&click("yes"), "1", "not-the-secret")));
    assert_eq!(forged.status, 401);
    assert_eq!(forged.body["code"], "slack.invalid_signature");
    // Fake clock: 1 s. Six minutes earlier is too old.
    let stale = run(r.handle(slack_request(&click("yes"), "-359", SECRET)));
    assert_eq!(stale.status, 401);
    assert!(r.backends.slack.borrow().replies.is_empty());
}

#[test]
fn other_interactions_are_acknowledged_and_ignored() {
    let r = router(false);
    r.backends.slack.borrow_mut().app = Some(app());
    let other = json!({"type": "view_submission", "user": {"id": "U1"}});
    let response = run(r.handle(slack_request(&other, "1", SECRET)));
    assert_eq!(response.status, 200);
    assert!(r.backends.slack.borrow().replies.is_empty());
}

#[test]
fn a_click_from_someone_without_an_account_gets_an_ephemeral_reply() {
    let r = router(false);
    let fake: &Fake = &r.backends;
    run(fake.seed());
    {
        let mut slack = fake.slack.borrow_mut();
        slack.app = Some(app());
        slack.users_info =
            Some(json!({"ok": true, "user": {"profile": {"email": "nobody@example.test"}}}));
    }
    let response = run(r.handle(slack_request(&click("yes"), "1", SECRET)));
    assert_eq!(response.status, 200);
    let replies = &fake.slack.borrow().replies;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, "https://hooks.slack.com/actions/T1/1/abc");
    assert_eq!(replies[0].1["response_type"], "ephemeral");
    assert!(
        replies[0].1["text"]
            .as_str()
            .unwrap()
            .contains("No Runbook account signs in with your Slack email"),
        "{:?}",
        replies[0].1
    );
}

#[test]
fn slack_does_not_share_the_email_without_the_scope() {
    let r = router(false);
    r.backends.slack.borrow_mut().app = Some(app());
    r.backends.slack.borrow_mut().users_info = Some(json!({"ok": true, "user": {"profile": {}}}));
    let response = run(r.handle(slack_request(&click("no"), "1", SECRET)));
    assert_eq!(response.status, 200);
    let text = r.backends.slack.borrow().replies[0].1["text"].clone();
    assert!(
        text.as_str().unwrap().contains("users:read.email"),
        "{text}"
    );
}

/// The single ephemeral reply's text.
fn only_reply(fake: &Fake) -> String {
    let replies = &fake.slack.borrow().replies;
    assert_eq!(replies.len(), 1, "{replies:?}");
    replies[0].1["text"].as_str().unwrap().to_owned()
}

#[test]
fn a_click_from_another_workspace_is_refused() {
    let r = router(false);
    let fake: &Fake = &r.backends;
    run(fake.seed());
    {
        let mut slack = fake.slack.borrow_mut();
        slack.app = Some(app());
        slack.users_info =
            Some(json!({"ok": true, "user": {"profile": {"email": "nobody@example.test"}}}));
    }
    let mut payload = click("yes");
    payload["user"]["team_id"] = json!("T2");
    let response = run(r.handle(slack_request(&payload, "1", SECRET)));
    assert_eq!(response.status, 200);
    assert!(
        only_reply(fake).contains("not the one The Runbook is connected to"),
        "{}",
        only_reply(fake)
    );

    // No team at all is refused too.
    fake.slack.borrow_mut().replies.clear();
    let mut payload = click("yes");
    payload["user"].as_object_mut().unwrap().remove("team_id");
    run(r.handle(slack_request(&payload, "1", SECRET)));
    assert!(only_reply(fake).contains("not the one The Runbook is connected to"));
}

#[test]
fn a_stranger_from_a_shared_channel_is_refused() {
    let r = router(false);
    let fake: &Fake = &r.backends;
    run(fake.seed());
    {
        let mut slack = fake.slack.borrow_mut();
        slack.app = Some(app());
        slack.users_info = Some(json!({"ok": true, "user": {
            "is_stranger": true,
            "profile": {"email": "nobody@example.test"},
        }}));
    }
    let response = run(r.handle(slack_request(&click("yes"), "1", SECRET)));
    assert_eq!(response.status, 200);
    assert!(
        only_reply(fake).contains("not the one The Runbook is connected to"),
        "{}",
        only_reply(fake)
    );
}
