//! MCP1 review: the worker-only step lookup, which no outside request can
//! reach, so it is tested here against an owned PostgreSQL database.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;

use crate::http::{Worker, router};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use runbook_core::credentials::{
    ActorKind, CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind,
    encode_secret, secret_hash,
};
use runbook_core::directory::{
    DirectoryCommit, DirectoryMutation, DirectoryStore, OrgRole, UserRecord,
};
use runbook_store_postgres::{DatabaseConfig, PostgresCredentials, PostgresDirectory};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tower::ServiceExt;

const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
metadata: {id: lookup, title: Lookup}\n\
spec:\n  steps:\n    - id: check\n      title: Check\n      actor: agent\n      instructions: Check.\n";

async fn call(
    app: &axum::Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
    etag: Option<&str>,
) -> (u16, Value, Option<String>) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"));
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    if let Some(etag) = etag {
        request = request.header("if-match", etag);
    }
    let body = if body.is_null() {
        Body::empty()
    } else {
        request = request.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        etag,
    )
}

async fn pat(
    database: &DatabaseConfig,
    n: u8,
    org: &str,
    user: &str,
    scopes: &[Scope],
    projects: Option<&str>,
) -> String {
    let secret = encode_secret(TokenKind::Personal, [n; 32]);
    PostgresCredentials::new(database.clone())
        .insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: user.parse().unwrap(),
                actor: ActorKind::Human,
            },
            org_id: org.parse().unwrap(),
            scopes: scopes.iter().copied().collect(),
            projects: projects.map(|p| BTreeSet::from([p.parse().unwrap()])),
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    secret
}

async fn lookup(database: &DatabaseConfig, token: &str, sr: &str) -> (u16, Value) {
    let server =
        crate::config::ServerConfig::new(database.clone(), "http://127.0.0.1:1", "127.0.0.1:1")
            .unwrap();
    let backends = crate::backends::PostgresBackends::new(
        &server,
        std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        None,
    );
    let request = runbook_api::Request {
        method: "GET".into(),
        path: runbook_api::mcp::PATH.into(),
        query: BTreeMap::from([("sr".to_owned(), sr.to_owned())]),
        body: Value::Null,
        credential: Some(runbook_api::Credential::Bearer(token.to_owned())),
        idempotency_key: None,
        request_id: "req_test".into(),
        new_id: "00000000000000000000000001".into(),
        if_match: None,
        client_ip: None,
    };
    let response = runbook_api::mcp::lookup_response(
        runbook_api::mcp::step_execution(&backends, &request).await,
        "req_test",
    );
    (response.status, response.body)
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn the_step_lookup_needs_read_access_to_the_steps_project() {
    use futures_util::FutureExt;
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        let config = crate::config::ServerConfig::new(
            db.config().clone(),
            "https://r.example",
            "127.0.0.1:0",
        )
        .unwrap();
        let issued = crate::bootstrap::run(&config, &Default::default())
            .await
            .unwrap();
        let mut bytes = vec![];
        issued.write_once(&mut bytes).unwrap();
        let issued: Value = serde_json::from_slice(&bytes).unwrap();
        let (owner, service) = (
            issued["pat"].as_str().unwrap(),
            issued["service"].as_str().unwrap(),
        );
        let (org, project) = (
            issued["org_id"].as_str().unwrap(),
            issued["project_id"].as_str().unwrap(),
        );
        let worker = Worker::start(config.clone()).unwrap();
        let app = router(worker.client());
        // A published runbook, a started run and its first step run.
        let (status, created, _) = call(
            &app,
            owner,
            "POST",
            &format!("/v1/projects/{project}/runbooks"),
            json!({"source": SPEC}),
            Some("c"),
            None,
        )
        .await;
        assert_eq!(status, 201, "{created}");
        let rb = created["runbook_id"].as_str().unwrap();
        let (_, _, etag) = call(
            &app,
            owner,
            "GET",
            &format!("/v1/runbooks/{rb}/draft"),
            Value::Null,
            None,
            None,
        )
        .await;
        let (status, published, _) = call(
            &app,
            owner,
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            json!({}),
            Some("p"),
            etag.as_deref(),
        )
        .await;
        assert_eq!(status, 201, "{published}");
        let (status, started, _) = call(
            &app,
            owner,
            "POST",
            &format!("/v1/projects/{project}/executions"),
            json!({"runbook": "lookup"}),
            Some("s"),
            None,
        )
        .await;
        assert_eq!(status, 201, "{started}");
        let exe = started["id"].as_str().unwrap().to_owned();
        let mut drainer = crate::drain::Drainer::new(
            config.clone(),
            crate::drain::DrainSettings {
                batch: 64,
                lease_ms: 30_000,
                idle: std::time::Duration::from_millis(10),
            },
        );
        for _ in 0..20 {
            if !drainer.drain_once().await.unwrap().busy() {
                break;
            }
        }
        let (status, minted, _) = call(
            &app,
            service,
            "POST",
            &format!("/v1/executions/{exe}/tokens"),
            json!({"session_id": "a"}),
            Some("m"),
            None,
        )
        .await;
        assert_eq!(status, 201, "{minted}");
        let (_, brief, _) = call(
            &app,
            minted["access_token"].as_str().unwrap(),
            "GET",
            &format!("/v1/executions/{exe}/next"),
            Value::Null,
            None,
            None,
        )
        .await;
        let sr = brief["ready_work"][0]["step_run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{brief}"))
            .to_owned();
        let (_, me, _) = call(&app, owner, "GET", "/v1/me", Value::Null, None, None).await;
        let user = me["user"]["id"].as_str().unwrap().to_owned();
        let database = db.config().clone();

        // The owner reads the project: the lookup answers.
        let (status, body) = lookup(&database, owner, &sr).await;
        assert_eq!(
            (status, body["execution_id"].as_str()),
            (200, Some(exe.as_str())),
            "{body}"
        );

        // Same organization, no read access to the step's project.
        let elsewhere = pat(
            &database,
            71,
            org,
            &user,
            &[Scope::ExecutionsRead, Scope::RunbooksRead],
            Some("prj_00000000000000000000000071"),
        )
        .await;
        let (status, body) = lookup(&database, &elsewhere, &sr).await;
        assert_eq!(status, 404, "{body}");
        // Same organization and project, but no executions:read.
        let no_read = pat(&database, 72, org, &user, &[Scope::RunbooksRead], None).await;
        let (status, body) = lookup(&database, &no_read, &sr).await;
        assert_eq!(status, 404, "{body}");

        // Another organization: the step does not exist for it.
        let foreign_org = format!("org_{:026}", 73);
        let foreign_user = format!("usr_{:026}", 73);
        PostgresDirectory::new(database.clone())
            .commit(DirectoryCommit {
                org_id: foreign_org.parse().unwrap(),
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Foreign".into(),
                    },
                    DirectoryMutation::CreateUser(UserRecord {
                        id: foreign_user.parse().unwrap(),
                        name: "Foreign owner".into(),
                        active: true,
                        role: OrgRole::Owner,
                    }),
                ],
            })
            .await
            .unwrap();
        let foreign = pat(
            &database,
            73,
            &foreign_org,
            &foreign_user,
            &[Scope::ExecutionsRead, Scope::RunbooksRead],
            None,
        )
        .await;
        let (status, body) = lookup(&database, &foreign, &sr).await;
        assert_eq!(status, 404, "{body}");
        drop(app);
        drop(worker);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}
