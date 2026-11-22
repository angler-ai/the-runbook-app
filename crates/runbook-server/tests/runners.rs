//! R0-2 on Postgres: the runner registry, runner tokens and runner scopes.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/runner_fixture.rs"]
mod runner_fixture;
use runbook_core::credentials::{ActorKind, Scope, secret_hash};
use runner_fixture::{with_fixture, with_fixture_config};
use serde_json::{Value, json};

const URL: &str = "https://runner.example";

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn bootstrap_requires_org_admin_and_runner_admin_scope() {
    with_fixture(async |f| {
        let path = format!("/v1/orgs/{}/runner/bootstrap-tokens", f.org);
        // The owner's bootstrap PAT has no runner scopes.
        let refused = f.call("POST", &path, &f.token, json!({"url": URL})).await;
        assert_eq!(refused.status, 403, "{}", refused.body);
        assert_eq!(refused.code(), "auth.scope_missing");
        // A member holding runner:admin is not an org admin.
        let member = f.member("Mo Member", Some("admin")).await;
        let member_pat = f
            .pat(
                &member,
                ActorKind::Human,
                &[Scope::RunnerRead, Scope::RunnerAdmin],
            )
            .await;
        let refused = f
            .call("POST", &path, &member_pat, json!({"url": URL}))
            .await;
        assert_eq!(refused.status, 403, "{}", refused.body);
        assert_eq!(refused.code(), "auth.forbidden");
        // An owner's agent-kind token is never an admin of the runner.
        let agent = f
            .pat(&f.user, ActorKind::Agent, &[Scope::RunnerAdmin])
            .await;
        let refused = f.call("POST", &path, &agent, json!({"url": URL})).await;
        assert_eq!(refused.status, 403, "{}", refused.body);
        // Another org's path is not found.
        let admin = f.admin_pat().await;
        let other = f
            .call(
                "POST",
                "/v1/orgs/org_00000000000000000000000099/runner/bootstrap-tokens",
                &admin,
                json!({"url": URL}),
            )
            .await;
        assert_eq!(other.status, 404);
        let created = f.call("POST", &path, &admin, json!({"url": URL})).await;
        assert_eq!(created.status, 201, "{}", created.body);
        let token = created.body["token"].as_str().unwrap();
        assert!(token.starts_with("trb_rbs_"));
        // Only the hash is stored.
        let sql = f.sql().await;
        let stored = sql
            .client()
            .query_one(
                "SELECT token_hash FROM runbook.runner_bootstrap_tokens",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(stored.get::<_, Vec<u8>>(0), secret_hash(token).as_bytes());
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn register_once_per_org() {
    with_fixture(async |f| {
        let admin = f.admin_pat().await;
        let path = format!("/v1/orgs/{}/runner/bootstrap-tokens", f.org);
        let boot = f.call("POST", &path, &admin, json!({"url": URL})).await;
        let token = boot.body["token"].as_str().unwrap().to_owned();
        let initial_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let body = json!({"url": URL, "version": "0.1.0", "contract_version": 1, "vault_identity_key": initial_key});
        // Another URL than the token was made for.
        let wrong = f
            .call(
                "POST",
                "/v1/runner-installs/register",
                &token,
                json!({"url": "https://elsewhere.example", "version": "0.1.0", "contract_version": 1}),
            )
            .await;
        assert_eq!(wrong.status, 403, "{}", wrong.body);
        assert_eq!(wrong.code(), "runner.url_mismatch");
        // An unsupported contract version.
        let old = f
            .call(
                "POST",
                "/v1/runner-installs/register",
                &token,
                json!({"url": URL, "version": "0.0.1", "contract_version": 99}),
            )
            .await;
        assert_eq!(old.status, 422);
        assert_eq!(old.code(), "runner.contract_unsupported");
        let first = f.call("POST", "/v1/runner-installs/register", &token, body.clone()).await;
        assert_eq!(first.status, 201, "{}", first.body);
        for key in ["directory_token", "release_token", "join_token"] {
            assert!(first.body[key].as_str().unwrap().starts_with("trb_r"));
        }
        assert!(first.body["install_id"].as_str().unwrap().starts_with("ri_"));
        let status_path = format!("/v1/orgs/{}/runner", f.org);
        let status = f.call("GET", &status_path, &admin, Value::Null).await;
        assert_eq!(status.body["vault_identity_key"], initial_key);
        let change_path = format!("{status_path}/vault-identity");
        let member = f.member("Member for vault identity", None).await;
        let member_token = f.pat(&member, ActorKind::Human, &[Scope::RunnerAdmin]).await;
        let next_key = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE";
        let replacement = json!({"public_key": next_key});
        let denied = f.call("PUT", &change_path, &member_token, replacement.clone()).await;
        assert_eq!(denied.status, 403, "{}", denied.body);
        let accepted = f.call("PUT", &change_path, &admin, replacement).await;
        assert_eq!(accepted.status, 204, "{}", accepted.body);
        let status = f.call("GET", &status_path, &admin, Value::Null).await;
        assert_eq!(status.body["vault_identity_key"], next_key);
        let sql = f.sql().await;
        let records = sql.client().query(
            "SELECT public_key, changed_by FROM runbook.runner_vault_identity_history WHERE install_id = $1 ORDER BY sequence",
            &[&first.body["install_id"].as_str().unwrap()],
        ).await.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].get::<_, &str>(0), initial_key);
        assert_eq!(records[1].get::<_, &str>(0), next_key);
        assert_eq!(records[1].get::<_, &str>(1), f.user.as_str());
        // The bootstrap token is single use.
        let again = f.call("POST", "/v1/runner-installs/register", &token, body.clone()).await;
        assert_eq!(again.status, 401, "{}", again.body);
        // A new bootstrap token is refused while an install exists, and a
        // second registration is refused too.
        let second = f.call("POST", &path, &admin, json!({"url": URL})).await;
        assert_eq!(second.status, 409, "{}", second.body);
        assert_eq!(second.code(), "runner.already_registered");
        let sql = f.sql().await;
        sql.client()
            .execute(
                "INSERT INTO runbook.runner_bootstrap_tokens (token_hash, org_id, url, created_at_ms, expires_at_ms) \
                 VALUES (sha256(convert_to('trb_rbs_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 'UTF8')), $1, $2, 0, 9223372036854775807)",
                &[&f.org.as_str(), &URL],
            )
            .await
            .unwrap();
        let dup = f
            .call(
                "POST",
                "/v1/runner-installs/register",
                "trb_rbs_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                body,
            )
            .await;
        assert_eq!(dup.status, 409, "{}", dup.body);
        assert_eq!(dup.code(), "runner.already_registered");
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn read_only_pat_cannot_admin_runner() {
    with_fixture(async |f| {
        let read_only = f.pat(&f.user, ActorKind::Human, &[Scope::RunnerRead]).await;
        let boot = f
            .call(
                "POST",
                &format!("/v1/orgs/{}/runner/bootstrap-tokens", f.org),
                &read_only,
                json!({"url": URL}),
            )
            .await;
        assert_eq!(boot.status, 403, "{}", boot.body);
        let read = f
            .call(
                "GET",
                &format!("/v1/orgs/{}/runner", f.org),
                &read_only,
                Value::Null,
            )
            .await;
        assert_eq!(read.status, 200, "{}", read.body);
        assert_eq!(read.body["status"], "none");
        // runner:admin does not imply runner:read.
        let admin_only = f
            .pat(&f.user, ActorKind::Human, &[Scope::RunnerAdmin])
            .await;
        let read = f
            .call(
                "GET",
                &format!("/v1/orgs/{}/runner", f.org),
                &admin_only,
                Value::Null,
            )
            .await;
        assert_eq!(read.status, 403);
        // A member reads the runner with runner:read.
        let member = f.member("Rita Reader", None).await;
        let member_pat = f.pat(&member, ActorKind::Human, &[Scope::RunnerRead]).await;
        let read = f
            .call(
                "GET",
                &format!("/v1/orgs/{}/runner", f.org),
                &member_pat,
                Value::Null,
            )
            .await;
        assert_eq!(read.status, 200, "{}", read.body);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn stale_heartbeat_marks_install_offline() {
    with_fixture(async |f| {
        let reg = f.register_runner(URL).await;
        let path = format!("/v1/orgs/{}/runner", f.org);
        let before = f.call("GET", &path, &reg.admin, Value::Null).await;
        assert_eq!(before.body["status"], "offline", "never heartbeated");
        f.heartbeat(&reg).await;
        let online = f.call("GET", &path, &reg.admin, Value::Null).await;
        assert_eq!(online.body["status"], "online");
        let sql = f.sql().await;
        sql.client()
            .execute(
                "UPDATE runbook.runner_installs SET last_heartbeat_at_ms = last_heartbeat_at_ms - 91000",
                &[],
            )
            .await
            .unwrap();
        let stale = f.call("GET", &path, &reg.admin, Value::Null).await;
        assert_eq!(stale.status, 200);
        assert_eq!(stale.body["status"], "offline", "{}", stale.body);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn bootstrap_url_rejects_metadata_and_private_hosts() {
    with_fixture(async |f| {
        let admin = f.admin_pat().await;
        let path = format!("/v1/orgs/{}/runner/bootstrap-tokens", f.org);
        for url in [
            "http://169.254.169.254/",
            "https://169.254.169.254/",
            "https://metadata.google.internal/",
            "https://10.0.0.5/",
            "https://127.0.0.1/",
            "https://[fd00:ec2::254]/",
            "http://runner.example/",
            "https://user:pass@runner.example/",
            "ftp://runner.example/",
        ] {
            let reply = f.call("POST", &path, &admin, json!({"url": url})).await;
            assert_eq!(reply.status, 422, "{url}: {}", reply.body);
            assert!(
                reply.code().starts_with("runner.url_"),
                "{url}: {}",
                reply.body
            );
        }
    })
    .await;
    // With private networks allowed, a private runner is accepted; a
    // metadata address never is.
    with_fixture_config(
        |config| config.runner.private_networks = true,
        async |f| {
            let admin = f.admin_pat().await;
            let path = format!("/v1/orgs/{}/runner/bootstrap-tokens", f.org);
            let ok = f
                .call(
                    "POST",
                    &path,
                    &admin,
                    json!({"url": "http://10.0.0.5:8080"}),
                )
                .await;
            assert_eq!(ok.status, 201, "{}", ok.body);
            for url in [
                "http://169.254.169.254/",
                "https://metadata.google.internal/",
            ] {
                let reply = f.call("POST", &path, &admin, json!({"url": url})).await;
                assert_eq!(reply.status, 422, "{url}: {}", reply.body);
            }
        },
    )
    .await;
}

#[path = "../../runbook-store-conformance/src/runners.rs"]
mod runner_store_conformance;

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn runner_registration_and_rejoin_conforms() {
    with_fixture(async |f| {
        let store = runbook_store_postgres::PostgresRunners::new(f.config.database.clone());
        runner_store_conformance::registration_and_rejoin(
            &store,
            runbook_core::catalog::TenantScope {
                org_id: f.org.clone(),
                project_id: f.project.clone(),
            },
        )
        .await;
    })
    .await;
}
