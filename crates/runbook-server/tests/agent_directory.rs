//! R0-3 on Postgres: the agent directory the runner syncs, publish-time
//! resolution of agent names, and the project's default agent.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/runner_fixture.rs"]
mod runner_fixture;
use runner_fixture::{Fixture, Registered, with_fixture};
use serde_json::{Value, json};

const URL: &str = "https://runner.example";

pub fn agent(f: &Fixture, reg: &Registered, id: &str, name: &str) -> Value {
    json!({
        "agent_id": id, "name": name, "project_id": f.project, "group_id": reg.group,
        "status": "active", "version_id": format!("{id}-v1"),
        "ceiling": {"profiles": ["fast", "deep"], "max_cost_usd": 1.0, "max_tokens": 200000,
            "timeout_s": 900,
            "credentials": [{"id": "cred_dd", "name": "datadog-readonly", "delivery": ["proxy"], "label": "read"}],
            "tools": [{"server_id": "mcp_1", "name": "query-metrics", "label": "read"}],
            "hosts": [{"host": "api.datadoghq.com", "label": "read"}]}
    })
}
async fn put(f: &Fixture, reg: &Registered, id: &str, body: Value) -> runner_fixture::Reply {
    f.call(
        "PUT",
        &format!("/v1/runner-installs/{}/agents/{id}", reg.install),
        &reg.directory,
        body,
    )
    .await
}
async fn runner_of(f: &Fixture, runbook: &str) -> Value {
    let exe = f.start(&f.token, runbook).await;
    let view = f
        .call(
            "GET",
            &format!("/v1/executions/{exe}"),
            &f.token,
            Value::Null,
        )
        .await;
    assert_eq!(view.status, 200, "{}", view.body);

    view.body["steps"][0]["runner"].clone()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn default_agent_applies_only_to_runbooks_published_after() {
    with_fixture(async |f| {
        let reg = f.register_runner(URL).await;
        f.allow_project(&reg).await;
        assert_eq!(
            put(f, &reg, "agt_1", agent(f, &reg, "agt_1", "deploy-checker"))
                .await
                .status,
            204
        );
        let unnamed = json!({"apiVersion":"therunbook.app/v1","kind":"Runbook",
            "metadata":{"id":"before","title":"Before"},
            "spec":{"steps":[{"id":"check","title":"Check","actor":"agent"}]}});
        let before = f.publish(&f.token, &unnamed.to_string()).await;
        assert_eq!(before.status, 201, "{}", before.body);
        let before_rb = before.body["__runbook_id"].as_str().unwrap().to_owned();
        assert_eq!(runner_of(f, &before_rb).await, Value::Null);

        let project = format!("/v1/projects/{}", f.project);
        let unknown = f
            .call(
                "PATCH",
                &project,
                &f.token,
                json!({"default_agent": "ghost"}),
            )
            .await;
        assert_eq!(unknown.status, 422, "{}", unknown.body);
        let set = f
            .call(
                "PATCH",
                &project,
                &f.token,
                json!({"default_agent": "deploy-checker"}),
            )
            .await;
        assert_eq!(set.status, 200, "{}", set.body);
        assert_eq!(set.body["default_agent"], "deploy-checker");
        // The earlier version is unchanged; a later publish resolves it.
        assert_eq!(runner_of(f, &before_rb).await, Value::Null);
        let mut after = unnamed.clone();
        after["metadata"]["id"] = json!("after");
        let after = f.publish(&f.token, &after.to_string()).await;
        assert_eq!(after.status, 201, "{}", after.body);
        let runner = runner_of(f, after.body["__runbook_id"].as_str().unwrap()).await;
        assert_eq!(runner["agent_id"], "agt_1");
        // A project member who is not an admin cannot set it.
        let member = f.member("Ed Editor", Some("editor")).await;
        let pat = f
            .pat(
                &member,
                runbook_core::credentials::ActorKind::Human,
                &runbook_core::credentials::Scope::ALL,
            )
            .await;
        let refused = f
            .call("PATCH", &project, &pat, json!({"default_agent": null}))
            .await;
        assert_eq!(refused.status, 403, "{}", refused.body);
        // Clearing it.
        let cleared = f
            .call("PATCH", &project, &f.token, json!({"default_agent": null}))
            .await;
        assert_eq!(cleared.status, 200, "{}", cleared.body);
    })
    .await;
}
