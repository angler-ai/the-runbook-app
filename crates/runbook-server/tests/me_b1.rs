//! B1 (web UI pilot backend): `GET /v1/me`, `DELETE /v1/me/token`,
//! `GET /v1/runbooks/{rb}/versions/{v}/form`, catalog title/latest, and
//! diagnostic spans, through the real HTTP router on Postgres.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::FutureExt;
use runbook_core::{credentials::*, directory::*};
use runbook_model::*;
use runbook_server::{
    config::ServerConfig,
    http::{Worker, router},
};
use runbook_store_postgres::{PostgresCredentials, PostgresDirectory, PostgresStore};
use serde_json::{Value, json};
use std::panic::AssertUnwindSafe;
use tower::ServiceExt;

fn id<T: std::str::FromStr>(prefix: &str, n: u32) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("{prefix}_{n:026}").parse().unwrap()
}
fn org(n: u32) -> OrganizationId {
    id("org", n)
}
fn proj(n: u32) -> ProjectId {
    id("prj", n)
}
fn user(n: u32) -> UserId {
    id("usr", n)
}
fn secret(kind: TokenKind, n: u8) -> String {
    encode_secret(kind, [n; 32])
}

struct Call {
    status: u16,
    body: Value,
    etag: Option<String>,
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
    key: Option<&str>,
    etag: Option<&str>,
) -> Call {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    if let Some(k) = key {
        b = b.header("idempotency-key", k);
    }
    if let Some(e) = etag {
        b = b.header("if-match", e);
    }
    let request = if body.is_null() {
        b.body(Body::empty()).unwrap()
    } else {
        b.header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    Call {
        status,
        body: if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        },
        etag,
    }
}

async fn seed(db: &database::TestDatabase) {
    let mut store = PostgresStore::connect(db.config()).await.unwrap();
    store.migrate().await.unwrap();
    let d = PostgresDirectory::new(db.config().clone());
    let c = PostgresCredentials::new(db.config().clone());
    // Two orgs so cross-tenant reads can be refused by identity, not luck.
    for (n, title) in [(1, "Acme"), (2, "Other")] {
        let team_id: TeamId = id("team", n);
        d.commit(DirectoryCommit {
            org_id: org(n),
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: title.into(),
                },
                DirectoryMutation::CreateUser(UserRecord {
                    id: user(n * 10 + 1),
                    name: if n == 1 {
                        "Alice".into()
                    } else {
                        "Mallory".into()
                    },
                    active: true,
                    role: OrgRole::Owner,
                }),
                DirectoryMutation::CreateUser(UserRecord {
                    id: user(n * 10 + 2),
                    name: "Bot".into(),
                    active: true,
                    role: OrgRole::Member,
                }),
                DirectoryMutation::CreateProject(ProjectRecord {
                    id: proj(n),
                    org_id: org(n),
                    slug: format!("project-{n}"),
                    title: "Project".into(),
                }),
                DirectoryMutation::CreateTeam(TeamRecord {
                    id: team_id.clone(),
                    org_id: org(n),
                    slug: "oncall".into(),
                    title: "Oncall".into(),
                }),
                DirectoryMutation::GrantProjectRole {
                    project_id: proj(n),
                    user_id: user(n * 10 + 1),
                    role: ProjectRole::Admin,
                },
                DirectoryMutation::GrantProjectRole {
                    project_id: proj(n),
                    user_id: user(n * 10 + 2),
                    role: ProjectRole::Operator,
                },
                DirectoryMutation::AddTeamMember {
                    team_id,
                    user_id: user(n * 10 + 1),
                },
            ],
        })
        .await
        .unwrap();
    }
    let scopes = [
        Scope::RunbooksRead,
        Scope::RunbooksWrite,
        Scope::RunbooksPublish,
        Scope::ExecutionsRead,
    ];
    // 1 human PAT (Alice), 2 agent PAT (Bot), 3 service, 4 revoked PAT,
    // 5 deletable PAT, 6 foreign human PAT (Mallory, org 2).
    let records = [
        (
            1u8,
            TokenKind::Personal,
            1,
            CredentialPrincipal::User {
                user_id: user(11),
                actor: runbook_core::credentials::ActorKind::Human,
            },
            None,
        ),
        (
            2,
            TokenKind::Personal,
            1,
            CredentialPrincipal::User {
                user_id: user(12),
                actor: runbook_core::credentials::ActorKind::Agent,
            },
            None,
        ),
        (
            3,
            TokenKind::Service,
            1,
            CredentialPrincipal::Service {
                project_id: proj(1),
                role: ProjectRole::Editor,
            },
            None,
        ),
        (
            4,
            TokenKind::Personal,
            1,
            CredentialPrincipal::User {
                user_id: user(11),
                actor: runbook_core::credentials::ActorKind::Human,
            },
            Some(1),
        ),
        (
            5,
            TokenKind::Personal,
            1,
            CredentialPrincipal::User {
                user_id: user(11),
                actor: runbook_core::credentials::ActorKind::Human,
            },
            None,
        ),
        (
            6,
            TokenKind::Personal,
            2,
            CredentialPrincipal::User {
                user_id: user(21),
                actor: runbook_core::credentials::ActorKind::Human,
            },
            None,
        ),
    ];
    for (n, kind, org_n, principal, revoked_at) in records {
        c.insert(CredentialRecord {
            id: id("tok", u32::from(n)),
            kind,
            hash: secret_hash(&secret(kind, n)),
            principal,
            org_id: org(org_n),
            scopes: scopes.into_iter().collect(),
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at,
            last_used_at: None,
        })
        .await
        .unwrap();
    }
}

async fn with_app(f: impl AsyncFnOnce(Router)) {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let config = ServerConfig::new(
        db.config().clone(),
        "https://api.example.test",
        "127.0.0.1:8080",
    )
    .unwrap();
    let result = AssertUnwindSafe(async {
        seed(&db).await;
        let worker = Worker::start(config).unwrap();
        let app = router(worker.client());
        f(app).await;
        drop(worker);
    })
    .catch_unwind()
    .await;
    db.finish(result).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn me_identity_for_each_credential_kind() {
    with_app(async |app| {
        let human = secret(TokenKind::Personal, 1);
        let me = call(&app, "GET", "/v1/me", Some(&human), Value::Null, None, None).await;
        assert_eq!(me.status, 200, "{}", me.body);
        assert_eq!(me.body["user"]["name"], "Alice");
        assert_eq!(me.body["user"]["id"], user(11).to_string());
        assert_eq!(me.body["org"]["title"], "Acme");
        assert_eq!(me.body["org"]["role"], "owner");
        assert_eq!(me.body["teams"][0]["slug"], "oncall");
        assert_eq!(me.body["credential"]["kind"], "personal");
        assert_eq!(me.body["credential"]["actor_kind"], "human");
        assert!(
            me.body["credential"]["scopes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "runbooks:read")
        );
        assert_eq!(me.body["credential"]["projects"], Value::Null);
        assert_eq!(me.body["credential"]["expires_at_ms"], i64::MAX);

        let agent = secret(TokenKind::Personal, 2);
        let me = call(&app, "GET", "/v1/me", Some(&agent), Value::Null, None, None).await;
        assert_eq!(me.status, 200, "{}", me.body);
        assert_eq!(me.body["user"]["name"], "Bot");
        assert_eq!(me.body["org"]["role"], "member");
        assert_eq!(me.body["credential"]["actor_kind"], "agent");
        assert_eq!(me.body["teams"], json!([]));

        let service = secret(TokenKind::Service, 3);
        let me = call(
            &app,
            "GET",
            "/v1/me",
            Some(&service),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(me.status, 200, "{}", me.body);
        assert_eq!(me.body["user"], Value::Null);
        assert_eq!(me.body["org"]["role"], Value::Null);
        assert_eq!(me.body["credential"]["kind"], "service");
        assert_eq!(me.body["credential"]["actor_kind"], Value::Null);

        let revoked = secret(TokenKind::Personal, 4);
        assert_eq!(
            call(
                &app,
                "GET",
                "/v1/me",
                Some(&revoked),
                Value::Null,
                None,
                None
            )
            .await
            .status,
            401
        );
        let execution = secret(TokenKind::Execution, 9);
        assert_eq!(
            call(
                &app,
                "GET",
                "/v1/me",
                Some(&execution),
                Value::Null,
                None,
                None
            )
            .await
            .status,
            401
        );
        assert_eq!(
            call(&app, "GET", "/v1/me", None, Value::Null, None, None)
                .await
                .status,
            401
        );
        assert_eq!(
            call(&app, "POST", "/v1/me", Some(&human), json!({}), None, None)
                .await
                .status,
            405
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn me_token_delete_revokes_only_presenting_pat() {
    with_app(async |app| {
        let pat = secret(TokenKind::Personal, 5);
        assert_eq!(
            call(&app, "GET", "/v1/me", Some(&pat), Value::Null, None, None)
                .await
                .status,
            200
        );
        let deleted = call(
            &app,
            "DELETE",
            "/v1/me/token",
            Some(&pat),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(deleted.status, 204, "{}", deleted.body);
        assert_eq!(
            call(&app, "GET", "/v1/me", Some(&pat), Value::Null, None, None)
                .await
                .status,
            401
        );
        // Alice's other PAT and the service token survive; service cannot self-delete.
        assert_eq!(
            call(
                &app,
                "GET",
                "/v1/me",
                Some(&secret(TokenKind::Personal, 1)),
                Value::Null,
                None,
                None
            )
            .await
            .status,
            200
        );
        let service = secret(TokenKind::Service, 3);
        assert_eq!(
            call(
                &app,
                "DELETE",
                "/v1/me/token",
                Some(&service),
                Value::Null,
                None,
                None
            )
            .await
            .status,
            403
        );
        assert_eq!(
            call(
                &app,
                "GET",
                "/v1/me",
                Some(&service),
                Value::Null,
                None,
                None
            )
            .await
            .status,
            200
        );
    })
    .await;
}

const FORM_SOURCE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: demo, title: Demo Title}\nspec:\n  inputs:\n    service: {type: enum, options: [api, web], required: true}\n    count: {type: integer}\n  fields:\n    severity: {type: enum, options: [none, major], default: none}\n  steps:\n    - id: work\n      title: Do work\n";

async fn publish_demo(app: &Router, token: &str, project_n: u32) -> String {
    let created = call(
        app,
        "POST",
        &format!("/v1/projects/{}/runbooks", proj(project_n)),
        Some(token),
        json!({"source": FORM_SOURCE}),
        Some("create-demo"),
        None,
    )
    .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let published = call(
        app,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        Some(token),
        json!({}),
        Some("publish-demo"),
        created.etag.as_deref(),
    )
    .await;
    assert_eq!(published.status, 201, "{}", published.body);
    rb
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn version_form_catalog_and_spans() {
    with_app(async |app| {
        let human = secret(TokenKind::Personal, 1);
        let rb = publish_demo(&app, &human, 1).await;
        for selector in ["latest", "1"] {
            let form = call(
                &app,
                "GET",
                &format!("/v1/runbooks/{rb}/versions/{selector}/form"),
                Some(&human),
                Value::Null,
                None,
                None,
            )
            .await;
            assert_eq!(form.status, 200, "{}", form.body);
            assert_eq!(
                form.body["inputs"]["properties"]["service"]["enum"],
                json!(["api", "web"])
            );
            assert_eq!(
                form.body["inputs"]["properties"]["count"]["type"],
                "integer"
            );
            assert_eq!(form.body["inputs"]["required"], json!(["service"]));
            assert_eq!(
                form.body["fields"]["properties"]["severity"]["enum"],
                json!(["none", "major"])
            );
            // CLI3: the step outline (titles as written, no instructions).
            assert_eq!(
                form.body["steps"],
                json!([{"id": "work", "title": "Do work", "kind": "task", "steps": []}])
            );
            assert_eq!(form.body["finally"], json!([]));
        }
        // Cross-tenant: org 2's runbook is invisible to org 1.
        let foreign = secret(TokenKind::Personal, 6);
        let foreign_rb = publish_demo(&app, &foreign, 2).await;
        let denied = call(
            &app,
            "GET",
            &format!("/v1/runbooks/{foreign_rb}/versions/latest/form"),
            Some(&human),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(denied.status, 404, "{}", denied.body);
        // Catalog rows carry the draft title and latest published number.
        let list = call(
            &app,
            "GET",
            &format!("/v1/projects/{}/runbooks", proj(1)),
            Some(&human),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(list.status, 200, "{}", list.body);
        let row = &list.body["items"][0];
        assert_eq!(row["title"], "Demo Title");
        assert_eq!(row["latest_version"], 1);
        let single = call(
            &app,
            "GET",
            &format!("/v1/runbooks/{rb}"),
            Some(&human),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(single.body["title"], "Demo Title");
        assert_eq!(single.body["latest_version"], 1);
        // Diagnostic spans survive validate and draft save.
        let invalid = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: [unclosed\n";
        let validated = call(
            &app,
            "POST",
            "/v1/validate",
            Some(&human),
            json!({"source": invalid}),
            None,
            None,
        )
        .await;
        assert_eq!(validated.status, 200, "{}", validated.body);
        let diags = validated.body["diagnostics"].as_array().unwrap();
        assert!(
            diags
                .iter()
                .any(|d| d["line"].as_u64().is_some_and(|l| l >= 1) && d["column"].is_number()),
            "{}",
            validated.body
        );
        let draft = call(
            &app,
            "GET",
            &format!("/v1/runbooks/{rb}/draft"),
            Some(&human),
            Value::Null,
            None,
            None,
        )
        .await;
        let saved = call(
            &app,
            "PATCH",
            &format!("/v1/runbooks/{rb}/draft"),
            Some(&human),
            json!({"source": invalid}),
            None,
            draft.etag.as_deref(),
        )
        .await;
        assert_eq!(saved.status, 200, "{}", saved.body);
        let diags = saved.body["diagnostics"].as_array().unwrap();
        assert!(
            diags.iter().any(|d| d["line"].is_number()),
            "{}",
            saved.body
        );
    })
    .await;
}
