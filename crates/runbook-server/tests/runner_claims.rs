//! Postgres row-level probes for ready indexing, claims and release grants.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/runner_fixture.rs"]
mod runner_fixture;
use runbook_core::signing::{GRANT_TYP, GrantClaims, Jwks, verify};
use runner_fixture::{Fixture, Registered, now_ms, with_fixture};
use serde_json::{Value, json};

const URL: &str = "https://runner.example";

fn agent(f: &Fixture, reg: &Registered, version: &str, max_cost: f64) -> Value {
    json!({
        "agent_id": "agt_1", "name": "deploy-checker", "project_id": f.project, "group_id": reg.group,
        "status": "active", "version_id": version,
        "ceiling": {"profiles": ["fast", "deep"], "max_cost_usd": max_cost, "max_tokens": 200000,
            "timeout_s": 900,
            "credentials": [
                {"id": "cred_dd", "name": "datadog-readonly", "delivery": ["proxy"], "label": "read"},
                {"id": "cred_gh", "name": "github-write", "delivery": ["proxy", "token"], "label": "write"},
                {"id": "cred_model", "name": "model-key", "delivery": ["model"], "label": "read"}
            ],
            "tools": [{"server_id": "mcp_1", "name": "query-metrics", "label": "read"}],
            "hosts": [{"host": "api.datadoghq.com", "label": "read"}]}
    })
}
async fn sync(f: &Fixture, reg: &Registered, body: Value) {
    let put = f
        .call(
            "PUT",
            &format!("/v1/runner-installs/{}/agents/agt_1", reg.install),
            &reg.directory,
            body,
        )
        .await;
    assert_eq!(put.status, 204, "{}", put.body);
}

struct Run {
    reg: Registered,
    exe: String,
    check: String,
}
/// A registered runner with one ready agent step.
async fn setup(f: &Fixture) -> Run {
    let reg = f.register_runner(URL).await;
    f.allow_project(&reg).await;
    sync(f, &reg, agent(f, &reg, "v1", 1.0)).await;
    let source = json!({"apiVersion":"therunbook.app/v1","kind":"Runbook",
        "metadata":{"id":"checks","title":"Checks"},
        "spec":{"steps":[{"id":"check","title":"Check","actor":"agent",
            "connections":["datadog-readonly"],
            "agent":{"name":"deploy-checker","model":"deep","max_cost_usd":0.5}}]}});
    let published = f.publish(&f.token, &source.to_string()).await;
    assert_eq!(published.status, 201, "{}", published.body);
    let exe = f
        .start(&f.token, published.body["__runbook_id"].as_str().unwrap())
        .await;
    let view = execution(f, &exe).await;
    let id = |path: &str| {
        view["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == path)
            .unwrap_or_else(|| panic!("{path}: {view}"))["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    Run {
        check: id("check"),
        reg,
        exe,
    }
}
async fn execution(f: &Fixture, exe: &str) -> Value {
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
async fn step(f: &Fixture, run: &Run, path: &str) -> Value {
    execution(f, &run.exe).await["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["path"] == path)
        .unwrap()
        .clone()
}
async fn claim(f: &Fixture, token: &str, sr: &str) -> runner_fixture::Reply {
    f.call(
        "POST",
        &format!("/v1/step-runs/{sr}/claim"),
        token,
        json!({"session_id": "s1", "actor": {"kind": "agent", "agent_name": "deploy-checker"}}),
    )
    .await
}
async fn grant(
    f: &Fixture,
    run: &Run,
    sr: &str,
    claimed: &Value,
    extra: Value,
) -> runner_fixture::Reply {
    let mut body = json!({
        "kind": "capability", "claim_id": claimed["id"], "generation": claimed["generation"],
        "runner_id": run.reg.runner,
    });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    f.call(
        "POST",
        &format!("/v1/step-runs/{sr}/grants"),
        &run.reg.release,
        body,
    )
    .await
}
async fn jwks(f: &Fixture) -> Jwks {
    let reply = f
        .request("GET", "/.well-known/jwks.json", None, &[], Value::Null)
        .await;
    serde_json::from_value(reply.body).unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn ready_pages_across_postgres_rows_with_tied_timestamps() {
    with_fixture(async |f| {
        let run = setup(f).await;
        let sql = f.sql().await;
        sql.client().execute(
            "INSERT INTO runbook.runner_ready_steps
             SELECT 'sr_' || lpad(n::text, 26, '0'), execution_id, org_id, project_id, agent_id, 0, NULL
             FROM runbook.runner_ready_steps CROSS JOIN generate_series(1, 100) n WHERE step_run_id = $1",
            &[&run.check],
        ).await.unwrap();
        let path = format!("/v1/runner-installs/{}/ready?group={}&limit=100&project={}", run.reg.install, run.reg.group, f.project);
        let first = f.call("GET", &path, &run.reg.directory, Value::Null).await;
        assert_eq!(first.status, 200, "{}", first.body);
        assert_eq!(first.body["items"].as_array().unwrap().len(), 100);
        let cursor = first.body["next_cursor"].as_str().expect("another page");
        let next = f.call("GET", &format!("{path}&cursor={cursor}"), &run.reg.directory, Value::Null).await;
        assert_eq!(next.status, 200, "{}", next.body);
        assert_eq!(next.body["items"].as_array().unwrap().len(), 1);
        assert_eq!(next.body["items"][0]["step_run_id"], run.check);
        assert_eq!(next.body["next_cursor"], Value::Null);
        let foreign = f.call("GET", &format!("/v1/runner-installs/{}/ready?group={}&project=prj_00000000000000000000000099",
            run.reg.install, run.reg.group), &run.reg.directory, Value::Null).await;
        assert_eq!(foreign.status, 200, "{}", foreign.body);
        assert_eq!(foreign.body["items"], json!([]));
    }).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn grant_exp_equals_lease_deadline() {
    with_fixture(async |f| {
        let run = setup(f).await;
        let before = now_ms();
        let claimed = claim(f, &run.reg.claim, &run.check).await;
        assert_eq!(claimed.status, 200, "{}", claimed.body);
        let granted = grant(f, &run, &run.check, &claimed.body, json!({})).await;
        assert_eq!(granted.status, 200, "{}", granted.body);
        let expires_at = granted.body["expires_at"].as_i64().unwrap();
        let claims: GrantClaims = verify(
            &jwks(f).await,
            granted.body["grant"].as_str().unwrap(),
            GRANT_TYP,
            now_ms() / 1000,
        )
        .unwrap();
        assert_eq!(claims.exp, expires_at / 1000);
        // The lease is ten minutes from the claim.
        let lease = expires_at - before;
        assert!((540_000..=660_000).contains(&lease), "{lease}");
        let deadline: i64 = f
            .sql()
            .await
            .client()
            .query_one(
                "SELECT (body::jsonb ->> 'lease_deadline_at_ms')::bigint FROM runbook.execution_claim WHERE step_run_id = $1",
                &[&run.check],
            )
            .await
            .map(|row| row.get(0))
            .unwrap();
        assert_eq!(deadline, expires_at);
        assert_eq!(claims.aud, format!("runner:{}", run.reg.runner));
        assert_eq!(claims.sub, run.check);
        assert_eq!(claims.group_id, run.reg.group);
        assert_eq!(claims.project_id, f.project.as_str());
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn release_events_record_delivery_mode() {
    with_fixture(async |f| {
        let run = setup(f).await;
        let claimed = claim(f, &run.reg.claim, &run.check).await;
        let granted = grant(
            f,
            &run,
            &run.check,
            &claimed.body,
            json!({"kind": "credential", "credential_id": "cred_dd", "credential_version": 7, "delivery": "proxy"}),
        )
        .await;
        assert_eq!(granted.status, 200, "{}", granted.body);
        let view = step(f, &run, "check").await;
        let release = &view["runner"]["releases"][0];
        assert_eq!(release["credential_id"], "cred_dd");
        assert_eq!(release["credential_version"], 7);
        assert_eq!(release["delivery"], "proxy");
        assert_eq!(release["runner_id"], run.reg.runner.as_str());
        let revoked = f
            .call(
                "POST",
                &format!("/v1/step-runs/{}/grants/revoked", run.check),
                &run.reg.release,
                json!({"credential_id": "cred_dd", "reason": "lease_ended"}),
            )
            .await;
        assert_eq!(revoked.status, 204, "{}", revoked.body);
        let view = step(f, &run, "check").await;
        assert_eq!(view["runner"]["releases"][0]["revoked"], "lease_ended");
        // The record holds no secret material: only IDs and modes.
        let sql = f.sql().await;
        let rows = sql
            .client()
            .query(
                "SELECT body FROM runbook.execution_event WHERE execution_id = $1",
                &[&run.exe],
            )
            .await
            .unwrap();
        let events: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
        let joined = events.join("\n");
        assert!(joined.contains("CredentialReleased"), "{joined}");
        assert!(joined.contains("CredentialRevoked"));
        assert!(joined.contains("\"delivery\":\"proxy\""));
    })
    .await;
}

/// The run's stored events of one kind.
async fn event_count(f: &Fixture, exe: &str, kind: &str) -> usize {
    f.sql()
        .await
        .client()
        .query(
            "SELECT body FROM runbook.execution_event WHERE execution_id = $1",
            &[&exe],
        )
        .await
        .unwrap()
        .iter()
        .filter(|r| r.get::<_, String>(0).contains(&format!("\"{kind}\"")))
        .count()
}

/// Review L11: renewing a credential grant records its release once per
/// claim, credential, version and delivery mode.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn renewals_record_one_release_per_claim() {
    with_fixture(async |f| {
        let run = setup(f).await;
        let claimed = claim(f, &run.reg.claim, &run.check).await;
        assert_eq!(claimed.status, 200, "{}", claimed.body);
        let credential = |version: u64| {
            json!({"kind": "credential", "credential_id": "cred_dd", "credential_version": version, "delivery": "proxy"})
        };
        for _ in 0..3 {
            let granted = grant(f, &run, &run.check, &claimed.body, credential(1)).await;
            assert_eq!(granted.status, 200, "{}", granted.body);
        }
        assert_eq!(event_count(f, &run.exe, "CredentialReleased").await, 1);
        let view = step(f, &run, "check").await;
        assert_eq!(
            view["runner"]["releases"].as_array().unwrap().len(),
            1,
            "{view}"
        );
        // Another version is another release.
        let granted = grant(f, &run, &run.check, &claimed.body, credential(2)).await;
        assert_eq!(granted.status, 200, "{}", granted.body);
        assert_eq!(event_count(f, &run.exe, "CredentialReleased").await, 2);
        // Once revoked, asking again is a new release.
        let revoked = f
            .call(
                "POST",
                &format!("/v1/step-runs/{}/grants/revoked", run.check),
                &run.reg.release,
                json!({"credential_id": "cred_dd", "reason": "credential.revoked"}),
            )
            .await;
        assert_eq!(revoked.status, 204, "{}", revoked.body);
        let granted = grant(f, &run, &run.check, &claimed.body, credential(2)).await;
        assert_eq!(granted.status, 200, "{}", granted.body);
        assert_eq!(event_count(f, &run.exe, "CredentialReleased").await, 3);
    })
    .await;
}
