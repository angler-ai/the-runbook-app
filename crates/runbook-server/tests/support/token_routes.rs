//! B4.1/C2 `/v1/tokens` on PostgreSQL through the real router: create,
//! list and revoke; secrets shown once and never replayed; a new token
//! only narrows its caller; users and organizations are isolated.
use super::*;

async fn tokens(
    f: &Fixture,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    key: Option<&str>,
) -> Reply {
    call(&f.app, token, method, path, body, key, None).await
}
async fn create(f: &Fixture, token: &str, body: Value, key: &str) -> Reply {
    tokens(f, token, "POST", "/v1/tokens", body, Some(key)).await
}
async fn list(f: &Fixture, token: &str) -> Vec<Value> {
    let r = tokens(f, token, "GET", "/v1/tokens", Value::Null, None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body["next_cursor"].is_null());
    let text = r.body.to_string();
    assert!(!text.contains("trb_") && !text.contains("hash"), "{text}");
    r.body["items"].as_array().unwrap().clone()
}
fn ids(items: &[Value]) -> Vec<&str> {
    items.iter().map(|t| t["id"].as_str().unwrap()).collect()
}
/// `DELETE /v1/tokens/{id}` status (204 has no JSON body).
async fn revoke(f: &Fixture, token: &str, id: &str) -> u16 {
    let request = axum::http::Request::builder()
        .method("DELETE")
        .uri(format!("/v1/tokens/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = f.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.status().as_u16()
}
/// An executions read through the project routes, to prove a token works.
async fn reads(f: &Fixture, token: &str) -> u16 {
    tokens(
        f,
        token,
        "GET",
        &format!("/v1/projects/{}/executions", f.project),
        Value::Null,
        None,
    )
    .await
    .status
}
/// A directory user with a seed PAT inserted as an operator would: no route
/// issues a person's first credential (no login flow yet).
async fn seed_user(
    f: &Fixture,
    org: &OrganizationId,
    n: u8,
    role: OrgRole,
    project: Option<(&ProjectId, ProjectRole)>,
    scopes: &[Scope],
) -> (UserId, String) {
    let directory = PostgresDirectory::new(f.config.database.clone());
    let user: UserId = format!("usr_{n:026}").parse().unwrap();
    let mut mutations = vec![];
    let revision = match directory.snapshot(org).await {
        Ok(s) => s.revision,
        Err(_) => {
            mutations.push(DirectoryMutation::CreateOrganization {
                title: "Foreign".into(),
            });
            0
        }
    };
    mutations.push(DirectoryMutation::CreateUser(UserRecord {
        id: user.clone(),
        name: format!("user {n}"),
        active: true,
        role,
    }));
    if let Some((project, role)) = project {
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: project.clone(),
            user_id: user.clone(),
            role,
        });
    }
    directory
        .commit(DirectoryCommit {
            org_id: org.clone(),
            expected_revision: revision,
            mutations,
        })
        .await
        .unwrap();
    let secret = encode_secret(TokenKind::Personal, [n; 32]);
    PostgresCredentials::new(f.config.database.clone())
        .insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: user.clone(),
                actor: runbook_core::credentials::ActorKind::Human,
            },
            org_id: org.clone(),
            scopes: scopes.iter().copied().collect(),
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    (user, secret)
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn tokens_create_list_revoke_and_show_secrets_once() {
    with_fixture(async |f, _| {
        let body = json!({"name":"ci","scopes":["executions:read"],"projects":["default"]});
        assert_eq!(
            tokens(f, &f.token, "POST", "/v1/tokens", body.clone(), None)
                .await
                .status,
            428
        );
        let created = create(f, &f.token, body.clone(), "ci").await;
        assert_eq!(created.status, 201, "{}", created.body);
        let secret = created.body["secret"].as_str().unwrap().to_owned();
        assert!(secret.starts_with("trb_pat_") && secret.len() == "trb_pat_".len() + 43);
        let view = &created.body["token"];
        assert_eq!(view["name"], "ci");
        assert_eq!(view["kind"], "personal");
        assert_eq!(view["scopes"], json!(["executions:read"]));
        assert_eq!(view["projects"], json!([f.project]));
        assert_eq!(view["user_id"], f.user.as_str());
        assert_eq!(view["actor_kind"], "human");
        assert_eq!(view["created_by"], f.user.as_str());
        assert!(view["last_used_at_ms"].is_null());
        let text = view.to_string();
        assert!(!text.contains(&secret) && !text.contains("hash"), "{text}");
        let id = view["id"].as_str().unwrap().to_owned();

        // D20: the same request answers 409 without a second secret; a
        // different body under the key is a reused key.
        let replay = create(f, &f.token, body.clone(), "ci").await;
        assert_eq!(
            (replay.status, replay.body["code"].clone()),
            (409, json!("tokens.secret_already_issued"))
        );
        assert!(!replay.body.to_string().contains("trb_"));
        let reused = create(f, &f.token, json!({"name":"other"}), "ci").await;
        assert_eq!(
            (reused.status, reused.body["code"].clone()),
            (409, json!("request.idempotency_key_reused"))
        );

        // Listed as metadata; the new token authenticates and records use.
        let items = list(f, &f.token).await;
        assert!(ids(&items).contains(&id.as_str()), "{items:?}");
        assert_eq!(
            items.iter().filter(|t| t["name"] == "ci").count(),
            1,
            "the replay issued nothing"
        );
        assert_eq!(reads(f, &secret).await, 200);
        let listed = list(f, &f.token).await;
        let used = listed.iter().find(|t| t["id"] == id.as_str()).unwrap();
        assert!(used["last_used_at_ms"].as_i64().is_some(), "{used}");
        let mut expected = view_with(view);
        expected.insert("last_used_at_ms".into(), used["last_used_at_ms"].clone());
        assert_eq!(used, &Value::Object(expected), "only last use changed");
        // It cannot manage tokens: it lacks tokens:manage.
        let denied = tokens(f, &secret, "GET", "/v1/tokens", Value::Null, None).await;
        assert_eq!(
            (denied.status, denied.body["code"].clone()),
            (403, json!("auth.scope_missing"))
        );

        // Revoke: 204, then it stops authenticating and leaves the list.
        let path = format!("/v1/tokens/{id}");
        assert_eq!(revoke(f, &f.token, &id).await, 204);
        assert_eq!(reads(f, &secret).await, 401);
        assert!(!ids(&list(f, &f.token).await).contains(&id.as_str()));
        assert_eq!(revoke(f, &f.token, &id).await, 404);
        assert_eq!(revoke(f, &f.token, "tok_nope").await, 404);
        assert_eq!(
            tokens(f, &f.token, "PATCH", &path, json!({}), None)
                .await
                .status,
            405
        );
        assert_eq!(
            tokens(f, "trb_pat_bogus", "GET", "/v1/tokens", Value::Null, None)
                .await
                .status,
            401
        );
    })
    .await;
}
/// `view` with `last_used_at_ms` left for the caller to fill.
fn view_with(view: &Value) -> serde_json::Map<String, Value> {
    let mut map = view.as_object().unwrap().clone();
    map.remove("last_used_at_ms");
    map
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn tokens_only_narrow_their_caller() {
    with_fixture(async |f, _| {
        // A narrowed manager token: two scopes, one project, ten days.
        let soon = runbook_now() + 10 * 86_400_000;
        let narrow = create(f, &f.token, json!({"name":"narrow","scopes":["tokens:manage","executions:read"],"projects":[f.project],"expires_at_ms":soon}), "narrow").await;
        assert_eq!(narrow.status, 201, "{}", narrow.body);
        let narrow = narrow.body["secret"].as_str().unwrap().to_owned();
        for (n, (body, status, code)) in [
            (json!({"scopes":["runbooks:write"]}), 403, "tokens.exceeds_caller"),
            (json!({"expires_at_ms": soon + 86_400_000}), 403, "tokens.exceeds_caller"),
            (json!({"user_id": format!("usr_{:026}", 7)}), 403, "auth.forbidden"),
            (json!({"scopes":["runbooks:everything"]}), 422, "request.invalid"),
            (json!({"unknown": true}), 422, "request.invalid"),
            (json!({"projects":["no-such-project"]}), 404, "resource.not_found"),
            (json!({"kind":"service","scopes":["executions:act"],"projects":[f.project],"role":"viewer"}), 403, "tokens.exceeds_caller"),
        ]
        .into_iter()
        .enumerate()
        {
            let r = create(f, &narrow, body.clone(), &format!("bad-{n}")).await;
            assert_eq!((r.status, r.body["code"].clone()), (status, json!(code)), "{body}");
        }
        // Omitted options inherit the caller, never more; an agent-kind
        // token never carries tokens:manage (D17: people manage tokens).
        let child = create(f, &narrow, json!({"actor_kind":"agent"}), "child").await;
        assert_eq!(child.status, 201, "{}", child.body);
        assert_eq!(child.body["token"]["scopes"], json!(["executions:read"]));
        assert_eq!(child.body["token"]["projects"], json!([f.project]));
        assert_eq!(child.body["token"]["expires_at_ms"], json!(soon));
        assert_eq!(child.body["token"]["actor_kind"], "agent");
        let agent = child.body["secret"].as_str().unwrap().to_owned();
        let asked = create(f, &narrow, json!({"actor_kind":"agent","scopes":["tokens:manage"]}), "agent-manage").await;
        assert_eq!((asked.status, asked.body["code"].clone()), (422, json!("request.invalid")));
        // An agent-kind PAT cannot manage tokens even if it held the scope.
        let seeded_agent = encode_secret(TokenKind::Personal, [33; 32]);
        PostgresCredentials::new(f.config.database.clone())
            .insert(CredentialRecord {
                id: format!("tok_{:026}", 33).parse().unwrap(),
                kind: TokenKind::Personal,
                hash: secret_hash(&seeded_agent),
                principal: CredentialPrincipal::User { user_id: f.user.clone(), actor: runbook_core::credentials::ActorKind::Agent },
                org_id: f.org.clone(),
                scopes: [Scope::TokensManage, Scope::ExecutionsRead].into(),
                projects: None,
                expires_at: Some(i64::MAX),
                revoked_at: None,
                last_used_at: None,
            })
            .await
            .unwrap();
        for token in [&agent, &seeded_agent] {
            let r = tokens(f, token, "GET", "/v1/tokens", Value::Null, None).await;
            assert_eq!((r.status, r.body["code"].clone()), (403, json!("auth.forbidden")));
            assert_eq!(create(f, token, json!({}), "from-agent").await.status, 403);
        }
        // Tokens are independent records: revoking the issuing token
        // leaves the tokens it minted working.
        let narrow_id = list(f, &f.token).await.into_iter().find(|t| t["name"] == "narrow").unwrap()["id"].as_str().unwrap().to_owned();
        assert_eq!(revoke(f, &f.token, &narrow_id).await, 204);
        assert_eq!(reads(f, &narrow).await, 401);
        assert_eq!(reads(f, &agent).await, 200);

        // Service tokens: an org owner grants at most their own project role.
        let service = create(f, &f.token, json!({"kind":"service","name":"launcher","scopes":["executions:read","executions:act"],"projects":["default"],"role":"operator"}), "svc").await;
        assert_eq!(service.status, 201, "{}", service.body);
        let svc = service.body["secret"].as_str().unwrap().to_owned();
        assert!(svc.starts_with("trb_svc_"));
        assert_eq!(service.body["token"]["project_id"], f.project.as_str());
        assert_eq!(service.body["token"]["role"], "operator");
        assert!(service.body["token"]["user_id"].is_null());
        assert_eq!(reads(f, &svc).await, 200);
        assert!(ids(&list(f, &f.token).await).contains(&service.body["token"]["id"].as_str().unwrap()));
        // Service tokens never manage tokens (D17), even with the scope.
        let bootstrap_like = create(f, &f.token, json!({"kind":"service","scopes":["tokens:manage"],"projects":["default"],"role":"admin"}), "svc-manage").await;
        assert_eq!(bootstrap_like.status, 201, "{}", bootstrap_like.body);
        let svc_manage = bootstrap_like.body["secret"].as_str().unwrap().to_owned();
        assert_eq!(tokens(f, &svc_manage, "GET", "/v1/tokens", Value::Null, None).await.status, 403);
        // A plain member cannot mint service tokens.
        let (_, member) = seed_user(f, &f.org, 31, OrgRole::Member, Some((&f.project, ProjectRole::Admin)), &[Scope::TokensManage, Scope::ExecutionsRead]).await;
        let r = create(f, &member, json!({"kind":"service","scopes":["executions:read"],"projects":["default"],"role":"viewer"}), "member-svc").await;
        assert_eq!((r.status, r.body["code"].clone()), (403, json!("auth.forbidden")));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn tokens_are_isolated_across_users_and_organizations() {
    with_fixture(async |f, _| {
        let owners = create(f, &f.token, json!({"name":"owner-ci"}), "owner-ci").await;
        assert_eq!(owners.status, 201, "{}", owners.body);
        let owners_id = owners.body["token"]["id"].as_str().unwrap().to_owned();
        let (member_id, member) = seed_user(
            f,
            &f.org,
            41,
            OrgRole::Member,
            Some((&f.project, ProjectRole::Operator)),
            &[Scope::TokensManage, Scope::ExecutionsRead],
        )
        .await;
        let minted = create(
            f,
            &member,
            json!({"name":"member-ci","scopes":["executions:read"]}),
            "member-ci",
        )
        .await;
        assert_eq!(minted.status, 201, "{}", minted.body);
        assert_eq!(minted.body["token"]["user_id"], member_id.as_str());
        let member_ci = minted.body["secret"].as_str().unwrap().to_owned();
        let member_ci_id = minted.body["token"]["id"].as_str().unwrap().to_owned();
        // Each user lists only their own tokens; a member cannot revoke the
        // owner's (404, nothing disclosed).
        let mine = list(f, &member).await;
        assert!(
            mine.iter().all(|t| t["user_id"] == member_id.as_str()),
            "{mine:?}"
        );
        assert!(ids(&mine).contains(&member_ci_id.as_str()));
        assert!(!ids(&list(f, &f.token).await).contains(&member_ci_id.as_str()));
        assert_eq!(revoke(f, &member, &owners_id).await, 404);
        // The same idempotency key is per principal.
        assert_eq!(
            create(f, &member, json!({"name":"owner-ci"}), "owner-ci")
                .await
                .status,
            201
        );

        // Another organization sees none of this one's tokens.
        let foreign_org: OrganizationId = format!("org_{:026}", 42).parse().unwrap();
        let (_, foreign) = seed_user(
            f,
            &foreign_org,
            43,
            OrgRole::Owner,
            None,
            &[Scope::TokensManage],
        )
        .await;
        let theirs = list(f, &foreign).await;
        assert!(
            !ids(&theirs).contains(&owners_id.as_str())
                && !ids(&theirs).contains(&member_ci_id.as_str()),
            "{theirs:?}"
        );
        assert_eq!(revoke(f, &foreign, &owners_id).await, 404);
        let cross = create(f, &foreign, json!({"projects":[f.project]}), "cross").await;
        assert_eq!(cross.status, 404, "{}", cross.body);
        assert_eq!(reads(f, &member_ci).await, 200);

        // D17 by rank: an org admin revokes a member's token, never an
        // owner's; an owner revokes anyone's.
        let (_, admin) =
            seed_user(f, &f.org, 44, OrgRole::Admin, None, &[Scope::TokensManage]).await;
        let second = create(
            f,
            &member,
            json!({"name":"member-two","scopes":["executions:read"]}),
            "member-two",
        )
        .await;
        assert_eq!(second.status, 201, "{}", second.body);
        let second_id = second.body["token"]["id"].as_str().unwrap().to_owned();
        assert_eq!(revoke(f, &admin, &owners_id).await, 404);
        assert_eq!(revoke(f, &admin, &second_id).await, 204);
        assert_eq!(revoke(f, &f.token, &member_ci_id).await, 204);
        assert_eq!(reads(f, &member_ci).await, 401);
    })
    .await;
}

fn runbook_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
