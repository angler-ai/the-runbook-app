//! R0-6 on Postgres: a report's cost is recorded per step and summed per
//! run; costs are finite and never negative.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/runner_fixture.rs"]
mod runner_fixture;
use runbook_core::credentials::{ActorKind, Scope};
use runner_fixture::{Fixture, with_fixture};
use serde_json::{Value, json};

async fn steps(f: &Fixture, exe: &str) -> Value {
    let view = f
        .call(
            "GET",
            &format!("/v1/executions/{exe}"),
            &f.token,
            Value::Null,
        )
        .await;
    assert_eq!(view.status, 200, "{}", view.body);
    view.body
}
fn id_of(view: &Value, path: &str) -> String {
    view["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["path"] == path)
        .unwrap_or_else(|| panic!("{path}: {view}"))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn claim(f: &Fixture, agent: &str, sr: &str) -> Value {
    let claimed = f
        .call(
            "POST",
            &format!("/v1/step-runs/{sr}/claim"),
            agent,
            json!({"session_id": "s1", "actor": {"kind": "agent", "agent_name": "ci"}}),
        )
        .await;
    assert_eq!(claimed.status, 200, "{}", claimed.body);
    claimed.body
}
async fn report(
    f: &Fixture,
    agent: &str,
    sr: &str,
    claim: &Value,
    cost: Value,
) -> runner_fixture::Reply {
    f.call(
        "POST",
        &format!("/v1/step-runs/{sr}/report"),
        agent,
        json!({"step_run_id": sr, "claim": claim, "status": "succeeded", "verdict": "pass",
               "summary": "done", "cost": cost}),
    )
    .await
}
async fn run(f: &Fixture) -> String {
    run_named(f, "costs").await
}
async fn run_named(f: &Fixture, slug: &str) -> String {
    let source = json!({"apiVersion":"therunbook.app/v1","kind":"Runbook",
        "metadata":{"id":slug,"title":"Costs"},
        "spec":{"steps":[{"id":"both","parallel":[
            {"id":"one","title":"One","actor":"agent"},
            {"id":"two","title":"Two","actor":"agent"}]}]}});
    let published = f.publish(&f.token, &source.to_string()).await;
    assert_eq!(published.status, 201, "{}", published.body);
    f.start(&f.token, published.body["__runbook_id"].as_str().unwrap())
        .await
}
async fn agent(f: &Fixture) -> String {
    f.pat(
        &f.user,
        ActorKind::Agent,
        &[Scope::ExecutionsAct, Scope::ExecutionsRead],
    )
    .await
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn report_cost_is_summed_per_run() {
    with_fixture(async |f| {
        let exe = run(f).await;
        let agent = agent(f).await;
        let view = steps(f, &exe).await;
        let (one, two) = (id_of(&view, "one"), id_of(&view, "two"));
        let c1 = claim(f, &agent, &one).await;
        let r1 = report(
            f,
            &agent,
            &one,
            &c1,
            json!({"model_usd": 0.25, "compute_usd": 0.05, "input_tokens": 1000,
                   "output_tokens": 200, "cached_tokens": 600}),
        )
        .await;
        assert_eq!(r1.status, 200, "{}", r1.body);
        let c2 = claim(f, &agent, &two).await;
        let r2 = report(
            f,
            &agent,
            &two,
            &c2,
            json!({"model_usd": 0.5, "compute_usd": 0.0, "input_tokens": 3000,
                   "output_tokens": 100, "cached_tokens": 0}),
        )
        .await;
        assert_eq!(r2.status, 200, "{}", r2.body);
        f.drain().await;
        let view = steps(f, &exe).await;
        let step = |path: &str| {
            view["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["path"] == path)
                .unwrap()
                .clone()
        };
        assert_eq!(step("one")["cost"]["model_usd"], 0.25);
        assert_eq!(step("one")["cost"]["cached_tokens"], 600);
        assert_eq!(step("two")["cost"]["input_tokens"], 3000);
        let total = &view["cost"];
        assert_eq!(total["model_usd"], 0.75, "{view}");
        assert!((total["compute_usd"].as_f64().unwrap() - 0.05).abs() < 1e-9);
        assert_eq!(total["input_tokens"], 4000);
        assert_eq!(total["output_tokens"], 300);
        assert_eq!(total["cached_tokens"], 600);
        // A report without a cost leaves none.
        let exe = run_named(f, "free").await;
        let view = steps(f, &exe).await;
        let one = id_of(&view, "one");
        let c = claim(f, &agent, &one).await;
        let r = f
            .call(
                "POST",
                &format!("/v1/step-runs/{one}/report"),
                &agent,
                json!({"step_run_id": one, "claim": c, "status": "succeeded", "verdict": "pass"}),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
        let view = steps(f, &exe).await;
        assert!(view.get("cost").is_none(), "{view}");
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn negative_or_nan_cost_is_rejected() {
    with_fixture(async |f| {
        let exe = run(f).await;
        let agent = agent(f).await;
        let view = steps(f, &exe).await;
        let one = id_of(&view, "one");
        let c = claim(f, &agent, &one).await;
        for cost in [
            json!({"model_usd": -0.01}),
            json!({"compute_usd": -1}),
            json!({"model_usd": "NaN"}),
            json!({"input_tokens": -5}),
        ] {
            let refused = report(f, &agent, &one, &c, cost.clone()).await;
            assert_eq!(refused.status, 422, "{cost}: {}", refused.body);
        }
        // A value past f64's range is not a number we accept either.
        let raw = format!(
            "{{\"step_run_id\":\"{one}\",\"claim\":{c},\"status\":\"succeeded\",\"cost\":{{\"model_usd\":1e400}}}}"
        );
        let refused = f.raw("POST", &format!("/v1/step-runs/{one}/report"), &agent, raw).await;
        assert_eq!(refused, 422);
        // The step is still claimed and takes a valid report.
        let ok = report(f, &agent, &one, &c, json!({"model_usd": 0.1})).await;
        assert_eq!(ok.status, 200, "{}", ok.body);
    })
    .await;
}
