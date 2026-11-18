//! NOTIFY1 endpoints: a person's email preference and the token-gated
//! one-click unsubscribe.
#[path = "support/fake_backends.rs"]
mod fake_backends;
use fake_backends::{request, router, run};
use runbook_core::notify::{EmailPreference, NotifyStore, unsubscribe_token};
use serde_json::json;

fn user_id() -> runbook_model::UserId {
    format!("usr_{:026}", 1).parse().unwrap()
}

#[test]
fn preferences_default_to_all_and_round_trip() {
    let fake = router(true);
    let backends = fake.backends.clone();
    let token = run(backends.seed());
    let router = fake;

    let got = run(router.handle(request(
        "GET",
        "/v1/me/email-preferences",
        Some(&token),
        json!(null),
    )));
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["preference"], "all");

    let put = run(router.handle(request(
        "PUT",
        "/v1/me/email-preferences",
        Some(&token),
        json!({ "preference": "approvals_only" }),
    )));
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(put.body["preference"], "approvals_only");
    let (stored, _) = run(backends.notify.email_preference(&user_id()))
        .unwrap()
        .unwrap();
    assert_eq!(stored, EmailPreference::ApprovalsOnly);

    // No credential: refused like every /v1/me route.
    let anon = run(router.handle(request(
        "GET",
        "/v1/me/email-preferences",
        None,
        json!(null),
    )));
    assert_eq!(anon.status, 401);
}

#[test]
fn unsubscribe_needs_the_users_token_and_turns_mail_off() {
    let fake = router(true);
    let backends = fake.backends.clone();
    run(backends.seed());
    let salt = run(fake
        .backends
        .notify
        .ensure_unsubscribe_salt(&user_id(), "salt-1".into()))
    .unwrap();
    let token = unsubscribe_token(&salt, &user_id());
    let router = fake;

    let wrong = run(router.handle(request(
        "POST",
        "/v1/unsubscribe",
        None,
        json!({ "token": format!("{}.{}", user_id(), "0".repeat(32)) }),
    )));
    assert_eq!(wrong.status, 404, "{}", wrong.body);

    let ok = run(router.handle(request(
        "POST",
        "/v1/unsubscribe",
        None,
        json!({ "token": token }),
    )));
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(ok.body["unsubscribed"], true);
    let (stored, stored_salt) = run(backends.notify.email_preference(&user_id()))
        .unwrap()
        .unwrap();
    assert_eq!(stored, EmailPreference::None);
    // The salt survives unsubscribing, so the same link stays valid.
    assert_eq!(stored_salt.as_deref(), Some("salt-1"));
    let again = run(router.handle(request(
        "POST",
        "/v1/unsubscribe",
        None,
        json!({ "token": token }),
    )));
    assert_eq!(again.status, 200);
}
