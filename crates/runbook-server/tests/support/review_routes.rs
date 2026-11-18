//! Server and API review fixes (F46, F48, F51, F55) over HTTP and Postgres.
use super::*;
use std::time::Duration;

/// Another personal token of the fixture's user, read-only.
async fn second_reader(f: &Fixture, n: u8) -> String {
    let secret = encode_secret(TokenKind::Personal, [n; 32]);
    PostgresCredentials::new(f.config.database.clone())
        .insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: f.user.clone(),
                actor: runbook_core::credentials::ActorKind::Human,
            },
            org_id: f.org.clone(),
            scopes: [Scope::ExecutionsRead].into(),
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    secret
}

/// F48: one credential cannot hold every stream; all streams share one
/// LISTEN connection; a stream ends at its lifetime and frees its slot.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn streams_are_capped_per_credential_share_one_listener_and_expire() {
    with_fixture(async |f, db| {
        use futures_util::StreamExt;
        let rb = f.create("capped").await;
        f.publish(&rb, spec("capped", "Capped", false), "v1").await;
        let start = f.start(json!({"runbook":"capped"}), Some("capped")).await;
        assert_eq!(start.status, 201, "{}", start.body);
        let id = start.body["id"].as_str().unwrap().to_owned();
        let mut config = f.config.clone();
        config.transport.streams_per_credential = 1;
        config.transport.stream_lifetime = Duration::from_secs(2);
        let worker = Worker::start(config).unwrap();
        let app = router(worker.client());
        let other = second_reader(f, 71).await;
        let open = |token: &str| {
            app.clone().oneshot(
                Request::builder()
                    .uri(format!("/v1/executions/{id}/events"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let a = open(&f.token).await.unwrap();
        assert_eq!(a.status(), 200);
        let refused = open(&f.token).await.unwrap();
        assert_eq!(refused.status(), 429);
        let body = to_bytes(refused.into_body(), 65536).await.unwrap();
        let problem: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["code"], "stream.too_many");
        let b = open(&other).await.unwrap();
        assert_eq!(b.status(), 200, "another credential still streams");
        let raw = runbook_store_postgres::PostgresStore::connect(db.config())
            .await
            .unwrap();
        let listeners: i64 = raw
            .client()
            .query_one(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = current_database() AND query ILIKE 'LISTEN%'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(listeners, 1, "streams share one LISTEN connection");
        for response in [a, b] {
            let mut frames = response.into_body().into_data_stream();
            tokio::time::timeout(Duration::from_secs(8), async {
                while frames.next().await.is_some() {}
            })
            .await
            .expect("the stream closes at its lifetime");
        }
        let again = open(&f.token).await.unwrap();
        assert_eq!(again.status(), 200, "the slot is free once the stream ends");
        drop(again);
        drop(app);
        drop(worker);
    })
    .await;
}

const OPEN_RUN: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: review-open, title: Review}\nspec:\n  steps:\n    - {id: work, title: Work, actor: human}\n    - {id: later, title: Later, actor: human}\n";

/// Start and activate a run of OPEN_RUN; returns its store and ID.
async fn open_run(
    f: &Fixture,
    key: &str,
) -> (runbook_store_postgres::PostgresExecutionStore, ExecutionId) {
    use runbook_core::{
        catalog::TenantScope,
        work_queue::{WorkKind, WorkQueue},
    };
    if f.start(
        json!({"runbook":"review-open"}),
        Some(&format!("{key}-probe")),
    )
    .await
    .status
        == 404
    {
        let rb = f.create("review-open").await;
        f.publish(&rb, OPEN_RUN.into(), "review-open-v1").await;
    }
    let reply = f.start(json!({"runbook":"review-open"}), Some(key)).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let id: ExecutionId = reply.body["id"].as_str().unwrap().parse().unwrap();
    let scope = TenantScope {
        org_id: f.org.clone(),
        project_id: f.project.clone(),
    };
    let store = runbook_store_postgres::PostgresExecutionStore::new(
        f.config.database.clone(),
        scope.clone(),
    );
    for lease in store.claim_due(100, 60_000).await.unwrap() {
        if lease.id.kind == WorkKind::Activation {
            runbook_core::start::activate(
                &store,
                &PostgresDirectory::new(f.config.database.clone()),
                scope.clone(),
                &lease,
            )
            .await
            .unwrap();
        }
    }
    (store, id)
}

/// A user of the fixture's org with the given project role and a PAT
/// carrying every execution scope.
async fn member_pat(f: &Fixture, n: u8, role: ProjectRole) -> String {
    let user: UserId = format!("usr_{n:026}").parse().unwrap();
    let directory = PostgresDirectory::new(f.config.database.clone());
    let revision = directory.snapshot(&f.org).await.unwrap().revision;
    directory
        .commit(DirectoryCommit {
            org_id: f.org.clone(),
            expected_revision: revision,
            mutations: vec![
                DirectoryMutation::CreateUser(UserRecord {
                    id: user.clone(),
                    name: format!("Member {n}"),
                    active: true,
                    role: OrgRole::Member,
                }),
                DirectoryMutation::GrantProjectRole {
                    project_id: f.project.clone(),
                    user_id: user.clone(),
                    role,
                },
            ],
        })
        .await
        .unwrap();
    pat_for(f, n, &f.org, user).await
}

async fn pat_for(f: &Fixture, n: u8, org: &OrganizationId, user: UserId) -> String {
    let secret = encode_secret(TokenKind::Personal, [n; 32]);
    PostgresCredentials::new(f.config.database.clone())
        .insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: user,
                actor: runbook_core::credentials::ActorKind::Human,
            },
            org_id: org.clone(),
            scopes: [
                Scope::ExecutionsRead,
                Scope::ExecutionsStart,
                Scope::ExecutionsAct,
                Scope::ExecutionsAdmin,
            ]
            .into(),
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    secret
}

/// F51: only the minting principal or a human project admin revokes a
/// family; a family is addressed only through its own run; an execution
/// token never revokes.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn credential_family_revocation_refuses_everyone_else() {
    with_fixture(async |f, _| {
        let (_, id) = open_run(f, "revoke-a").await;
        let (_, other_run) = open_run(f, "revoke-b").await;
        let minted = call(
            &f.app,
            &f.token,
            "POST",
            &format!("/v1/executions/{id}/tokens"),
            json!({"session_id":"s1"}),
            Some("mint"),
            None,
        )
        .await;
        assert_eq!(minted.status, 201, "{}", minted.body);
        let family = minted.body["family_id"].as_str().unwrap().to_owned();
        let access = minted.body["access_token"].as_str().unwrap().to_owned();
        let works = async || {
            call(
                &f.app,
                &access,
                "GET",
                &format!("/v1/executions/{id}"),
                Value::Null,
                None,
                None,
            )
            .await
            .status
        };
        assert_eq!(works().await, 200);
        let operator = member_pat(f, 61, ProjectRole::Operator).await;
        let revoke = |exe: &ExecutionId| format!("/v1/executions/{exe}/credentials/{family}");
        for (token, path, status) in [
            (&operator, revoke(&id), 403),
            (&f.token, revoke(&other_run), 404),
            (&access, revoke(&id), 403),
        ] {
            let reply = call(&f.app, token, "DELETE", &path, Value::Null, None, None).await;
            assert_eq!(reply.status, status, "{path}: {}", reply.body);
            assert_eq!(works().await, 200, "the family still works after {path}");
        }
        let reply = call(
            &f.app,
            &f.token,
            "DELETE",
            &revoke(&id),
            Value::Null,
            None,
            None,
        )
        .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(works().await, 401);
    })
    .await;
}
async fn step_of(
    store: &runbook_store_postgres::PostgresExecutionStore,
    id: &ExecutionId,
    path: &str,
) -> StepRunId {
    use runbook_core::ExecutionStore;
    store
        .load(id)
        .await
        .unwrap()
        .projection
        .steps
        .into_iter()
        .find(|s| s.path == path)
        .unwrap()
        .step_run_id
}

async fn raw_get(f: &Fixture, token: &str, path: &str) -> axum::response::Response {
    f.app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Finish the run and purge it as retention would.
async fn purge(store: &runbook_store_postgres::PostgresExecutionStore, id: &ExecutionId) {
    use runbook_core::{
        ExecutionStore,
        catalog::{CatalogStore, DerivedUpdate},
        retention::RunStore,
    };
    let run = store.load(id).await.unwrap();
    let mut projection = run.projection;
    projection.sequence += 1;
    projection.status = ExecutionStatus::Succeeded;
    projection.completed_at_ms = Some(1);
    let mut summary = store.get_run(id).await.unwrap().unwrap();
    summary.sequence = projection.sequence;
    summary.status = ExecutionStatus::Succeeded;
    summary.completed_at_ms = Some(1);
    store
        .commit(runbook_core::ExecutionCommit {
            execution_id: id.clone(),
            expected_sequence: run.revision,
            events: vec![runbook_engine::EventEnvelope {
                execution_id: id.clone(),
                seq: projection.sequence,
                at_ms: 1,
                actor: ActorRef::System,
                event: runbook_engine::Event::ExecutionCompleted {
                    status: ExecutionStatus::Succeeded,
                    outputs: Default::default(),
                },
            }],
            projection,
            timers: vec![],
            outbox: vec![],
            idempotency: vec![],
            claims: vec![],
            credentials: vec![],
            derived_outbox: vec![DerivedUpdate {
                summary,
                assignments: vec![],
                release_concurrency: true,
                memory: vec![],
            }],
        })
        .await
        .unwrap();
    store.delete_execution(id, "retention").await.unwrap();
}

/// F46: the document's log link pages the redacted lines by sequence
/// (D33); execution tokens are refused like events; a purged run is 410.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn step_logs_page_by_sequence_from_the_document_link() {
    with_fixture(async |f, _| {
        let (store, id) = open_run(f, "logs").await;
        let sr = step_of(&store, &id, "work").await;
        let claim = call(&f.app, &f.token, "POST", &format!("/v1/step-runs/{sr}/claim"), json!({"session_id":"web"}), Some("claim"), None).await;
        assert_eq!(claim.status, 200, "{}", claim.body);
        let appended = call(&f.app, &f.token, "POST", &format!("/v1/step-runs/{sr}/logs"), json!({"claim":claim.body,"logs":["one","token=hidden","three"]}), Some("logs"), None).await;
        assert_eq!(appended.status, 200, "{}", appended.body);
        let report = call(&f.app, &f.token, "GET", &format!("/v1/executions/{id}/report"), Value::Null, None, None).await;
        let href = report.body["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["step_run_id"] == json!(sr))
            .unwrap()["logs"]["href"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(href, format!("/v1/step-runs/{sr}/logs"));
        let first = call(&f.app, &f.token, "GET", &format!("{href}?limit=2"), Value::Null, None, None).await;
        assert_eq!(first.status, 200, "{}", first.body);
        assert_eq!(
            first.body,
            json!({"lines":[{"seq":1,"line":"one"},{"seq":2,"line":"token=[REDACTED:token]"}],"next_cursor":"2","has_more":true})
        );
        let rest = call(&f.app, &f.token, "GET", &format!("{href}?cursor=2&limit=2"), Value::Null, None, None).await;
        assert_eq!(rest.body, json!({"lines":[{"seq":3,"line":"three"}],"next_cursor":"3","has_more":false}));
        let tail = call(&f.app, &f.token, "GET", &format!("{href}?cursor=3"), Value::Null, None, None).await;
        assert_eq!(tail.body, json!({"lines":[],"next_cursor":"3","has_more":false}));
        for bad in ["cursor=x", "cursor=-1", "limit=0", "limit=1001", "after=1"] {
            let reply = call(&f.app, &f.token, "GET", &format!("{href}?{bad}"), Value::Null, None, None).await;
            assert_eq!((reply.status, reply.body["code"].as_str()), (400, Some("request.invalid_query")), "{bad}");
        }
        // The Markdown export links the same route and is never sniffed.
        let markdown = raw_get(f, &f.token, &format!("/v1/executions/{id}/report?format=markdown")).await;
        assert_eq!(markdown.headers()["x-content-type-options"], "nosniff");
        let text = to_bytes(markdown.into_body(), 1_048_576).await.unwrap();
        assert!(String::from_utf8_lossy(&text).contains(&format!("[Logs]({href})")));
        // D32: execution tokens never read logs, even of their own run.
        let minted = call(&f.app, &f.token, "POST", &format!("/v1/executions/{id}/tokens"), json!({"session_id":"agent"}), Some("mint"), None).await;
        let access = minted.body["access_token"].as_str().unwrap();
        let refused = call(&f.app, access, "GET", &href, Value::Null, None, None).await;
        assert_eq!((refused.status, refused.body["code"].as_str()), (403, Some("auth.forbidden")));
        // Any project reader may read them (executions:read).
        let viewer = member_pat(f, 63, ProjectRole::Viewer).await;
        assert_eq!(call(&f.app, &viewer, "GET", &href, Value::Null, None, None).await.status, 200);
        purge(&store, &id).await;
        let expired = call(&f.app, &f.token, "GET", &href, Value::Null, None, None).await;
        assert_eq!((expired.status, expired.body["code"].as_str()), (410, Some("execution.history_expired")));
        let unknown = call(&f.app, &f.token, "GET", "/v1/step-runs/sr_00000000000000000000000063/logs", Value::Null, None, None).await;
        assert_eq!(unknown.status, 404);
    })
    .await;
}

/// F55: a fully scoped credential of another organization learns nothing
/// and changes nothing on any execution or step-run route.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn foreign_organizations_get_404_on_every_execution_route() {
    with_fixture(async |f, _| {
        use runbook_core::ExecutionStore;
        let (store, id) = open_run(f, "foreign").await;
        let sr = step_of(&store, &id, "work").await;
        let foreign_org: OrganizationId = format!("org_{:026}", 62).parse().unwrap();
        let foreign_user: UserId = format!("usr_{:026}", 62).parse().unwrap();
        PostgresDirectory::new(f.config.database.clone())
            .commit(DirectoryCommit {
                org_id: foreign_org.clone(),
                expected_revision: 0,
                mutations: vec![
                    DirectoryMutation::CreateOrganization {
                        title: "Foreign".into(),
                    },
                    DirectoryMutation::CreateUser(UserRecord {
                        id: foreign_user.clone(),
                        name: "Foreign owner".into(),
                        active: true,
                        role: OrgRole::Owner,
                    }),
                ],
            })
            .await
            .unwrap();
        let foreign = pat_for(f, 62, &foreign_org, foreign_user).await;
        let before = store.load(&id).await.unwrap().revision;
        let claim = json!({"id":"clm_00000000000000000000000001","generation":1});
        for (method, path, body) in [
            ("GET", format!("/v1/executions/{id}"), Value::Null),
            ("GET", format!("/v1/executions/{id}/next"), Value::Null),
            ("GET", format!("/v1/executions/{id}/report"), Value::Null),
            ("GET", format!("/v1/executions/{id}/events"), Value::Null),
            ("PATCH", format!("/v1/executions/{id}"), json!({"action":"pause"})),
            ("POST", format!("/v1/executions/{id}/signals/go"), json!({})),
            ("POST", format!("/v1/executions/{id}/tokens"), json!({"session_id":"x"})),
            ("POST", format!("/v1/executions/{id}/rerun"), json!({})),
            ("GET", format!("/v1/step-runs/{sr}"), Value::Null),
            ("GET", format!("/v1/step-runs/{sr}/skill-files"), Value::Null),
            ("GET", format!("/v1/step-runs/{sr}/logs"), Value::Null),
            ("POST", format!("/v1/step-runs/{sr}/claim"), json!({"session_id":"x"})),
            ("POST", format!("/v1/step-runs/{sr}/heartbeat"), json!({"claim":claim})),
            ("POST", format!("/v1/step-runs/{sr}/logs"), json!({"claim":claim,"logs":["x"]})),
            ("POST", format!("/v1/step-runs/{sr}/report"), json!({"step_run_id":sr,"claim":claim,"status":"succeeded","outputs":{},"summary":"x"})),
            ("POST", format!("/v1/step-runs/{sr}/release"), json!({"reason":"x"})),
            ("POST", format!("/v1/step-runs/{sr}/decisions"), json!({"option":"x"})),
            ("POST", format!("/v1/step-runs/{sr}/decision"), json!({"option":"x"})),
            ("POST", format!("/v1/step-runs/{sr}/overrides"), json!({"reason":"x"})),
        ] {
            let key = (method == "POST").then_some("foreign");
            let reply = call(&f.app, &foreign, method, &path, body, key, None).await;
            assert_eq!(
                (reply.status, reply.body["code"].as_str()),
                (404, Some("resource.not_found")),
                "{method} {path}"
            );
        }
        assert_eq!(store.load(&id).await.unwrap().revision, before, "no state change");
    })
    .await;
}
