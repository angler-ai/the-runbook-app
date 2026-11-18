//! C2 `runbook webhook …` against the C1 webhook routes.
mod fake_api;
use fake_api::*;
use serde_json::{Value, json};

const SECRET: &str = "whsec_signing-secret-shown-once";
const URL: &str = "https://hooks.example.com/runbook";

fn view() -> Value {
    json!({"id":WHK,"project_id":PRJ,"name":"relay","url":URL,"events":["agent.work_available"],
        "runbooks":[],"tags":[],"enabled":true,"created_at_ms":1,"updated_at_ms":1,
        "secret_rotated_at_ms":1,"previous_secret_expires_at_ms":null,"failing_since_ms":null,
        "disabled_reason":null,"disabled_at_ms":null})
}
fn delivery(state: &str) -> Value {
    json!({"id":WHD,"webhook_id":WHK,"event_id":"evt_1","event_type":"webhook.test","execution_id":null,
        "state":state,"created_at_ms":1,"next_attempt_at_ms":null,"redelivery_of":null,"failure":null,"attempts":[]})
}
fn base(project: &str) -> String {
    format!("/v1/projects/{project}/webhooks")
}

#[test]
fn create_sends_the_subscription_and_prints_the_secret_once() {
    let home = tempfile::tempdir().unwrap();
    let created = json!({"webhook": view(), "secret": SECRET});
    let (host, server) = serve(vec![
        Reply::json(201, created.clone()),
        Reply::json(201, created.clone()),
    ]);
    let o = cli(
        &host,
        home.path(),
        &[
            "webhook",
            "create",
            "--project",
            "ops",
            "--name",
            "relay",
            "--url",
            URL,
            "--event",
            "agent.work_available",
            "--event",
            "approval.requested",
            "--runbook",
            RB,
            "--tag",
            "prod",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o).matches(SECRET).count(), 1, "{}", out(&o));
    assert!(out(&o).contains(WHK));
    assert!(!err(&o).contains(SECRET));
    let o = cli(
        &host,
        home.path(),
        &[
            "webhook",
            "create",
            "--project",
            PRJ,
            "--url",
            URL,
            "--event",
            "*",
            "--disabled",
            "--json",
            "--idempotency-key",
            "k1",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), created);
    assert_eq!(out(&o).matches(SECRET).count(), 1);
    assert!(!err(&o).contains(SECRET));

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("POST {}", base("ops")));
    assert_eq!(
        body_of(&seen[0]),
        json!({"name":"relay","url":URL,"events":["agent.work_available","approval.requested"],
            "runbooks":[RB],"tags":["prod"],"enabled":true})
    );
    assert!(header(&seen[0], "idempotency-key").is_some());
    // The name defaults to the URL.
    assert_eq!(
        body_of(&seen[1]),
        json!({"name":URL,"url":URL,"events":["*"],"runbooks":[],"tags":[],"enabled":false})
    );
    assert_eq!(header(&seen[1], "idempotency-key"), Some("k1"));
}

#[test]
fn invalid_arguments_are_usage_errors_before_any_request() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);
    for args in [
        vec!["webhook", "create", "--project", "ops", "--url", URL],
        vec![
            "webhook",
            "create",
            "--project",
            "ops",
            "--url",
            URL,
            "--event",
            "x",
            "--runbook",
            "deploy",
        ],
        vec![
            "webhook",
            "create",
            "--project",
            "Bad Project",
            "--url",
            URL,
            "--event",
            "x",
        ],
        vec!["webhook", "list"],
        vec!["webhook", "update", WHK, "--project", "ops"],
        vec![
            "webhook",
            "update",
            WHK,
            "--project",
            "ops",
            "--enable",
            "--disable",
        ],
        vec!["webhook", "delete", "whk_nope", "--project", "ops"],
        vec!["webhook", "redeliver", WHD, "--project", "ops"],
        vec![
            "webhook",
            "deliveries",
            WHK,
            "--project",
            "ops",
            "--limit",
            "0",
        ],
    ] {
        let o = cli(&host, home.path(), &args, &token(), None);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn list_prints_subscriptions_or_json() {
    let home = tempfile::tempdir().unwrap();
    let page = json!({"items":[view()]});
    let (host, server) = serve(vec![
        Reply::json(200, page.clone()),
        Reply::json(200, page.clone()),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["webhook", "list", "--project", "ops"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains(WHK) && out(&o).contains(URL),
        "{}",
        out(&o)
    );
    let o = cli(
        &host,
        home.path(),
        &["webhook", "list", "--project", "ops", "--json"],
        &token(),
        None,
    );
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), page);
    assert_eq!(
        line(&server.join().unwrap()[0]),
        format!("GET {}", base("ops"))
    );
}

#[test]
fn update_sends_only_the_given_fields() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![Reply::json(200, view()), Reply::json(200, view())]);
    let o = cli(
        &host,
        home.path(),
        &[
            "webhook",
            "update",
            WHK,
            "--project",
            "ops",
            "--event",
            "step.blocked",
            "--disable",
            "--clear-tags",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &[
            "webhook",
            "update",
            WHK,
            "--project",
            "ops",
            "--name",
            "new",
            "--url",
            URL,
            "--enable",
            "--runbook",
            RB,
            "--json",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), view());
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("PATCH {}/{WHK}", base("ops")));
    assert_eq!(
        body_of(&seen[0]),
        json!({"events":["step.blocked"],"enabled":false,"tags":[]})
    );
    assert_eq!(
        body_of(&seen[1]),
        json!({"name":"new","url":URL,"enabled":true,"runbooks":[RB]})
    );
}

#[test]
fn delete_test_and_redeliver() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        Reply::empty(204),
        Reply::json(202, delivery("pending")),
        Reply::json(202, delivery("pending")),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["webhook", "delete", WHK, "--project", "ops"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(&format!("deleted {WHK}")));
    let o = cli(
        &host,
        home.path(),
        &["webhook", "test", WHK, "--project", "ops"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(WHD), "{}", out(&o));
    let o = cli(
        &host,
        home.path(),
        &[
            "webhook",
            "redeliver",
            WHD,
            "--webhook",
            WHK,
            "--project",
            "ops",
            "--json",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        serde_json::from_str::<Value>(&out(&o)).unwrap(),
        delivery("pending")
    );
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("DELETE {}/{WHK}", base("ops")));
    assert_eq!(line(&seen[1]), format!("POST {}/{WHK}/test", base("ops")));
    assert!(header(&seen[1], "idempotency-key").is_some());
    assert_eq!(
        line(&seen[2]),
        format!("POST {}/{WHK}/deliveries/{WHD}/redeliver", base("ops"))
    );
    assert!(header(&seen[2], "idempotency-key").is_some());
}

#[test]
fn deliveries_page_through_the_log() {
    let home = tempfile::tempdir().unwrap();
    let page = json!({"items":[delivery("failed")],"next_cursor":"5.whd_next"});
    let (host, server) = serve(vec![Reply::json(200, page)]);
    let o = cli(
        &host,
        home.path(),
        &[
            "webhook",
            "deliveries",
            WHK,
            "--project",
            "ops",
            "--limit",
            "10",
            "--cursor",
            "9.whd_x",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains(WHD) && out(&o).contains("failed"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("next: 5.whd_next"), "{}", out(&o));
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!(
            "GET {}/{WHK}/deliveries?limit=10&cursor=9.whd_x",
            base("ops")
        )
    );
}

#[test]
fn rotate_secret_prints_the_new_secret_once_and_a_replay_fails() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        Reply::json(200, json!({"webhook": view(), "secret": SECRET})),
        Reply::json(409, json!({"code":"webhooks.secret_already_issued"})),
    ]);
    let args = [
        "webhook",
        "rotate-secret",
        WHK,
        "--project",
        "ops",
        "--idempotency-key",
        "r1",
    ];
    let o = cli(&host, home.path(), &args, &token(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o).matches(SECRET).count(), 1, "{}", out(&o));
    assert!(!err(&o).contains(SECRET));
    let o = cli(&host, home.path(), &args, &token(), None);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("webhooks.secret_already_issued"));
    assert!(!out(&o).contains(SECRET) && !err(&o).contains(SECRET));
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("POST {}/{WHK}/rotate-secret", base("ops"))
    );
    assert_eq!(header(&seen[0], "idempotency-key"), Some("r1"));
}

#[test]
fn reaching_the_subscription_limit_exits_4() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = serve(vec![
        Reply::json(409, json!({"code":"webhooks.limit_reached"})),
        Reply::json(429, json!({"code":"rate.limited"})),
    ]);
    let args = [
        "webhook",
        "create",
        "--project",
        "ops",
        "--url",
        URL,
        "--event",
        "*",
    ];
    assert_eq!(
        cli(&host, home.path(), &args, &token(), None).status.code(),
        Some(4)
    );
    let o = cli(
        &host,
        home.path(),
        &["webhook", "list", "--project", "ops"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(4));
}
