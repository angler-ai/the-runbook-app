use runbook_api::{Request, Service, discovery::DiscoveryConfig};
use runbook_core::{credentials::*, directory::*, directory_operations::*};
use runbook_model::ProjectRole;
use serde_json::{Value, json};
fn org(n: u8) -> runbook_model::OrganizationId {
    format!("org_{n:026}").parse().unwrap()
}
fn user(n: u8) -> runbook_model::UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn project(n: u8) -> runbook_model::ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}
pub fn request(
    method: &str,
    path: &str,
    body: Value,
    token: &str,
    key: Option<&str>,
    n: u32,
) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        query: Default::default(),
        body,
        credential: Some(runbook_api::Credential::Bearer(token.into())),
        idempotency_key: key.map(str::to_owned),
        request_id: format!("req_{n:026}"),
        new_id: format!("{n:026}"),
        if_match: None,
        client_ip: None,
    }
}
pub async fn directory_routes_enforce_scope_and_roles<
    D: DirectoryOperations + Clone,
    C: CredentialStore + Clone,
>(
    d: D,
    c: C,
) {
    for n in [1, 2] {
        d.commit(DirectoryCommit {
            org_id: org(n),
            expected_revision: 0,
            mutations: vec![
                DirectoryMutation::CreateOrganization {
                    title: format!("Org{n}"),
                },
                DirectoryMutation::CreateUser(UserRecord {
                    id: user(n),
                    name: format!("User{n}"),
                    active: true,
                    role: OrgRole::Owner,
                }),
                DirectoryMutation::CreateProject(ProjectRecord {
                    id: project(n),
                    org_id: org(n),
                    slug: "api".into(),
                    title: "API".into(),
                }),
                DirectoryMutation::GrantProjectRole {
                    project_id: project(n),
                    user_id: user(n),
                    role: ProjectRole::Admin,
                },
            ],
        })
        .await
        .unwrap();
    }
    let secret = encode_secret(TokenKind::Personal, [1; 32]);
    let record = CredentialRecord {
        id: format!("tok_{:026}", 1).parse().unwrap(),
        kind: TokenKind::Personal,
        hash: secret_hash(&secret),
        principal: CredentialPrincipal::User {
            user_id: user(1),
            actor: ActorKind::Human,
        },
        org_id: org(1),
        scopes: [
            Scope::ProjectAdmin,
            Scope::RunbooksRead,
            Scope::RunbooksWrite,
        ]
        .into(),
        projects: None,
        expires_at: Some(i64::MAX),
        revoked_at: None,
        last_used_at: None,
    };
    c.insert(record.clone()).await.unwrap();
    let cfg = DiscoveryConfig {
        origin: "https://test.example".into(),
        minimum_cli_version: "0.1.0".into(),
        max_request_bytes: 67_108_864,
        version: runbook_api::VERSION.into(),
        schema: Default::default(),
        theme: None,
        step_runs: None,
    };
    let api = CheckedService::new(d.clone(), c.clone(), cfg.clone());
    let users = format!("/v1/orgs/{}/users", org(1));
    assert_eq!(
        api.handle(request(
            "POST",
            &users,
            json!({"name":"Alice"}),
            &secret,
            None,
            10
        ))
        .await
        .status,
        428
    );
    let mut created = vec![];
    for (n, name) in [(10, "Alice"), (11, "Bob"), (12, "Carol")] {
        let r = api
            .handle(request(
                "POST",
                &users,
                json!({"name":name}),
                &secret,
                Some(name),
                n,
            ))
            .await;
        assert_eq!(r.status, 201, "{:?}", r.body);
        created.push(r.body["id"].as_str().unwrap().to_owned());
    }
    let replay = api
        .handle(request(
            "POST",
            &users,
            json!({"name":"Alice"}),
            &secret,
            Some("Alice"),
            99,
        ))
        .await;
    assert_eq!(replay.body["id"], created[0]);
    assert_eq!(
        api.handle(request(
            "POST",
            &users,
            json!({"name":"Different"}),
            &secret,
            Some("Alice"),
            99
        ))
        .await
        .status,
        409
    );
    let teams = format!("/v1/orgs/{}/teams", org(1));
    let mut page = request("GET", &users, Value::Null, &secret, None, 90);
    page.query.insert("limit".into(), "1".into());
    let first = api.handle(page).await;
    assert_eq!(first.status, 200);
    assert_eq!(first.body["items"].as_array().unwrap().len(), 1);
    let cursor = first.body["next_cursor"].as_str().unwrap().to_owned();
    let mut next = request("GET", &users, Value::Null, &secret, None, 91);
    next.query.insert("cursor".into(), cursor.clone());
    let second = api.handle(next).await;
    assert_eq!(second.status, 200);
    assert_ne!(first.body["items"][0]["id"], second.body["items"][0]["id"]);
    let mut wrong_route = request("GET", &teams, Value::Null, &secret, None, 92);
    wrong_route.query.insert("cursor".into(), cursor.clone());
    assert_eq!(api.handle(wrong_route).await.status, 400);
    let mut team_ids = vec![];
    for (n, slug) in [(20, "oncall"), (21, "eng-leads")] {
        let r = api
            .handle(request(
                "POST",
                &teams,
                json!({"slug":slug,"title":slug}),
                &secret,
                Some(slug),
                n,
            ))
            .await;
        assert_eq!(r.status, 201);
        team_ids.push(r.body["id"].as_str().unwrap().to_owned());
    }
    let mut stale_page = request("GET", &users, Value::Null, &secret, None, 93);
    stale_page.query.insert("cursor".into(), cursor);
    assert_eq!(api.handle(stale_page).await.status, 409);
    let projects = format!("/v1/orgs/{}/projects", org(1));
    let p = api
        .handle(request(
            "POST",
            &projects,
            json!({"slug":"second","title":"Second"}),
            &secret,
            Some("project"),
            30,
        ))
        .await;
    assert_eq!(p.status, 201);
    for (n, id) in created.iter().enumerate() {
        let p = format!("/v1/projects/{}/members", project(1));
        assert_eq!(
            api.handle(request(
                "POST",
                &p,
                json!({"user_id":id,"role":"operator"}),
                &secret,
                Some(id),
                40 + n as u32
            ))
            .await
            .status,
            200
        );
        let team = format!("{teams}/{}/members", team_ids[usize::from(n == 2)]);
        assert_eq!(
            api.handle(request(
                "POST",
                &team,
                json!({"user_id":id}),
                &secret,
                Some(id),
                50 + n as u32
            ))
            .await
            .status,
            200
        );
    }
    let foreign = format!("/v1/orgs/{}/users", org(2));
    for method in ["GET", "POST"] {
        assert_eq!(
            api.handle(request(
                method,
                &foreign,
                json!({"name":"No"}),
                &secret,
                Some("foreign"),
                60
            ))
            .await
            .status,
            404
        );
    }
    let mut limited = record.clone();
    limited.id = format!("tok_{:026}", 2).parse().unwrap();
    let narrow = encode_secret(TokenKind::Personal, [2; 32]);
    limited.hash = secret_hash(&narrow);
    limited.projects = Some([project(1)].into());
    c.insert(limited).await.unwrap();
    assert_eq!(
        api.handle(request(
            "POST",
            &users,
            json!({"name":"Denied"}),
            &narrow,
            Some("denied"),
            60
        ))
        .await
        .status,
        403
    );
    // A project admin with only org-member status cannot administer the org.
    let alice: runbook_model::UserId = created[0].parse().unwrap();
    let snap = d.snapshot(&org(1)).await.unwrap();
    d.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: snap.revision,
        mutations: vec![DirectoryMutation::GrantProjectRole {
            project_id: project(1),
            user_id: alice.clone(),
            role: ProjectRole::Admin,
        }],
    })
    .await
    .unwrap();
    let mut other = record.clone();
    other.id = format!("tok_{:026}", 3).parse().unwrap();
    let other_secret = encode_secret(TokenKind::Personal, [3; 32]);
    other.hash = secret_hash(&other_secret);
    other.principal = CredentialPrincipal::User {
        user_id: alice.clone(),
        actor: ActorKind::Human,
    };
    c.insert(other).await.unwrap();
    assert_eq!(
        api.handle(request(
            "POST",
            &users,
            json!({"name":"Denied"}),
            &other_secret,
            Some("denied"),
            61
        ))
        .await
        .status,
        403
    );
    let visible = api
        .handle(request(
            "GET",
            &projects,
            Value::Null,
            &other_secret,
            None,
            61,
        ))
        .await;
    assert_eq!(visible.status, 200);
    assert_eq!(visible.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(visible.body["items"][0]["id"], project(1).as_str());
    assert_eq!(
        api.handle(request("GET", &users, Value::Null, &other_secret, None, 61))
            .await
            .status,
        200
    );
    let mut service = record.clone();
    service.id = format!("tok_{:026}", 4).parse().unwrap();
    let service_secret = encode_secret(TokenKind::Service, [4; 32]);
    service.kind = TokenKind::Service;
    service.hash = secret_hash(&service_secret);
    service.principal = CredentialPrincipal::Service {
        project_id: project(1),
        role: ProjectRole::Admin,
    };
    service.projects = Some([project(1)].into());
    c.insert(service).await.unwrap();
    assert_eq!(
        api.handle(request(
            "GET",
            &teams,
            Value::Null,
            &service_secret,
            None,
            61
        ))
        .await
        .status,
        200
    );
    assert_eq!(
        api.handle(request(
            "POST",
            &teams,
            json!({"slug":"no","title":"No"}),
            &service_secret,
            Some("no"),
            61
        ))
        .await
        .status,
        403
    );

    let snap = d.snapshot(&org(1)).await.unwrap();
    d.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: snap.revision,
        mutations: vec![DirectoryMutation::SetOrgRole {
            user_id: alice.clone(),
            role: OrgRole::Admin,
        }],
    })
    .await
    .unwrap();
    assert_eq!(
        api.handle(request(
            "POST",
            &teams,
            json!({"slug":"extra","title":"Extra"}),
            &other_secret,
            Some("cached"),
            62
        ))
        .await
        .status,
        201
    );
    assert_eq!(
        api.handle(request(
            "PATCH",
            &format!("{users}/{alice}"),
            json!({"active":false}),
            &secret,
            None,
            63
        ))
        .await
        .status,
        200
    );
    assert_eq!(
        api.handle(request(
            "POST",
            &teams,
            json!({"slug":"extra","title":"Extra"}),
            &other_secret,
            Some("cached"),
            64
        ))
        .await
        .status,
        403
    );
    let member = format!("{teams}/{}/members/{}", team_ids[0], created[1]);
    assert_eq!(
        api.handle(request("DELETE", &member, Value::Null, &secret, None, 65))
            .await
            .status,
        204
    );
    let restarted = CheckedService::new(d.clone(), c.clone(), cfg);
    let members = restarted
        .handle(request(
            "GET",
            &format!("{teams}/{}/members", team_ids[0]),
            Value::Null,
            &secret,
            None,
            66,
        ))
        .await;
    assert_eq!(members.status, 200);
    assert!(
        !members.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["user_id"] == created[1])
    );
    let vars = format!("/v1/projects/{}/vars", project(1));
    let value = json!({"declarations":{"region":{"type":"string"}},"values":{"region":"us-east"}});
    assert_eq!(
        api.handle(request("PUT", &vars, value.clone(), &secret, None, 70))
            .await
            .status,
        428
    );
    let mut put = request("PUT", &vars, value.clone(), &secret, None, 70);
    put.if_match = Some("\"vars-0\"".into());
    let saved = api.handle(put).await;
    assert_eq!(saved.status, 200);
    assert_eq!(saved.etag.as_deref(), Some("\"vars-1\""));
    let before = d.snapshot(&org(1)).await.unwrap();
    let rejected = d
        .apply_operation(DirectoryOperation {
            commit: DirectoryCommit {
                org_id: org(1),
                expected_revision: before.revision,
                mutations: vec![],
            },
            variables: Some(VariablesWrite {
                project_id: project(1),
                expected_revision: 0,
                variables: Default::default(),
            }),
            replay: None,
            response: StoredResponse {
                status: 200,
                body: Value::Null,
                etag: None,
            },
        })
        .await
        .unwrap();
    assert_eq!(rejected, OperationResult::VariablesConflict);
    assert_eq!(d.snapshot(&org(1)).await.unwrap().revision, before.revision);

    let mut stale = request("PUT", &vars, value.clone(), &secret, None, 70);
    stale.if_match = Some("\"vars-0\"".into());
    assert_eq!(api.handle(stale).await.status, 412);
    assert_eq!(
        restarted
            .handle(request("GET", &vars, Value::Null, &secret, None, 71))
            .await
            .body,
        value
    );
    let mut bad = request(
        "PUT",
        &vars,
        json!({"declarations":{"region":{"type":"string"}},"values":{"region":5}}),
        &secret,
        None,
        72,
    );
    bad.if_match = Some("\"vars-1\"".into());
    assert_eq!(api.handle(bad).await.status, 422);
    let mut sensitive = request(
        "PUT",
        &vars,
        json!({"declarations":{"region":{"type":"string"}},"values":{"region":"Bearer do-not-store"}}),
        &secret,
        None,
        72,
    );
    sensitive.if_match = Some("\"vars-1\"".into());
    let denied = api.handle(sensitive).await;
    assert_eq!(denied.status, 422);
    assert_eq!(denied.body["code"], "vars.secret_like");
    for declaration in [
        json!({"type":"string", "default":"Bearer do-not-store"}),
        json!({"type":"object", "properties":{"nested":{"type":"string", "default":"Bearer do-not-store"}}}),
        json!({"type":"object", "default":{"nested":"Bearer do-not-store"}, "properties":{"nested":{"type":"string"}}}),
    ] {
        let mut sensitive = request(
            "PUT",
            &vars,
            json!({"declarations":{"credential":declaration},"values":{}}),
            &secret,
            None,
            72,
        );
        sensitive.if_match = Some("\"vars-1\"".into());
        let denied = api.handle(sensitive).await;
        assert_eq!(denied.status, 422);
        assert_eq!(denied.body["code"], "vars.secret_like");
        assert_eq!(
            d.project_variables(&org(1), &project(1))
                .await
                .unwrap()
                .revision,
            1
        );
    }
    let invalid_replay = api
        .handle(request(
            "POST",
            &users,
            json!({"name":123}),
            &secret,
            Some("Alice"),
            99,
        ))
        .await;
    assert_eq!(invalid_replay.status, 409);
    assert_eq!(
        invalid_replay.body["code"],
        "request.idempotency_key_reused"
    );
    let mut delete = request(
        "DELETE",
        &format!("{vars}/region"),
        Value::Null,
        &secret,
        None,
        73,
    );
    delete.if_match = Some("\"vars-1\"".into());
    assert_eq!(api.handle(delete).await.status, 204);
    assert_eq!(
        restarted
            .handle(request("GET", &vars, Value::Null, &secret, None, 74))
            .await
            .body,
        json!({"declarations":{},"values":{}})
    );
}

struct CheckedService<D, C> {
    inner: Service<D, C>,
}
impl<D: DirectoryOperations, C: CredentialStore> CheckedService<D, C> {
    fn new(d: D, c: C, config: DiscoveryConfig) -> Self {
        Self {
            inner: Service::new(d, c, config),
        }
    }
    async fn handle(&self, r: Request) -> runbook_api::problem::Response {
        let route = runbook_model::DIRECTORY_ROUTES
            .iter()
            .find(|route| {
                route.method == r.method
                    && route.path.split('/').count() == r.path.split('/').count()
                    && route
                        .path
                        .split('/')
                        .zip(r.path.split('/'))
                        .all(|(a, b)| a.starts_with('{') || a == b)
            })
            .unwrap();
        let result = self.inner.handle(r).await;
        if result.status == 204 {
            assert!(result.body.is_null());
            return result;
        }
        let schema = if result.problem {
            "Problem"
        } else {
            route.response.unwrap()
        };
        let doc = runbook_contract::openapi();
        let root =
            json!({"$ref":format!("#/components/schemas/{schema}"),"components":doc["components"]});
        let validator = jsonschema::validator_for(&root).unwrap();
        if let Err(error) = validator.validate(&result.body) {
            panic!("response does not match {schema}: {error}");
        }
        result
    }
}

/// F47: an org admin can deactivate members and admins but never an owner
/// (the R38/D17 rank rule for tokens, applied to users). The owner's
/// credential keeps working after the refused attempt.
pub async fn admins_cannot_deactivate_owners<
    D: DirectoryOperations + Clone,
    C: CredentialStore + Clone,
>(
    d: D,
    c: C,
) {
    let (owner, admin, member) = (user(31), user(32), user(33));
    d.commit(DirectoryCommit {
        org_id: org(3),
        expected_revision: 0,
        mutations: vec![
            DirectoryMutation::CreateOrganization {
                title: "Org3".into(),
            },
            DirectoryMutation::CreateUser(UserRecord {
                id: owner.clone(),
                name: "Owner".into(),
                active: true,
                role: OrgRole::Owner,
            }),
            DirectoryMutation::CreateUser(UserRecord {
                id: admin.clone(),
                name: "Admin".into(),
                active: true,
                role: OrgRole::Admin,
            }),
            DirectoryMutation::CreateUser(UserRecord {
                id: member.clone(),
                name: "Member".into(),
                active: true,
                role: OrgRole::Member,
            }),
            // A second owner, so the last-owner invariant does not decide.
            DirectoryMutation::CreateUser(UserRecord {
                id: user(34),
                name: "Second owner".into(),
                active: true,
                role: OrgRole::Owner,
            }),
        ],
    })
    .await
    .unwrap();
    let mut secrets = vec![];
    for (n, who) in [(31u8, &owner), (32, &admin)] {
        let secret = encode_secret(TokenKind::Personal, [n; 32]);
        c.insert(CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: who.clone(),
                actor: ActorKind::Human,
            },
            org_id: org(3),
            scopes: [Scope::ProjectAdmin, Scope::RunbooksRead].into(),
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
        secrets.push(secret);
    }
    let (owner_pat, admin_pat) = (&secrets[0], &secrets[1]);
    let api = CheckedService::new(
        d.clone(),
        c.clone(),
        DiscoveryConfig {
            origin: "https://test.example".into(),
            minimum_cli_version: "0.1.0".into(),
            max_request_bytes: 67_108_864,
            version: runbook_api::VERSION.into(),
            schema: Default::default(),
            theme: None,
            step_runs: None,
        },
    );
    let path = |u: &runbook_model::UserId| format!("/v1/orgs/{}/users/{u}", org(3));
    let refused = api
        .handle(request(
            "PATCH",
            &path(&owner),
            json!({"active":false}),
            admin_pat,
            None,
            301,
        ))
        .await;
    assert_eq!(
        (refused.status, refused.body["code"].as_str()),
        (403, Some("auth.forbidden"))
    );
    let s = d.snapshot(&org(3)).await.unwrap();
    assert!(s.users[&owner].active, "the owner stays active");
    let listed = api
        .handle(request(
            "GET",
            &format!("/v1/orgs/{}/users", org(3)),
            Value::Null,
            owner_pat,
            None,
            302,
        ))
        .await;
    assert_eq!(listed.status, 200, "the owner's PAT still works");
    // Members, and the owner acting on an admin, are within rank.
    for (target, token, n) in [(&member, admin_pat, 303), (&admin, owner_pat, 304)] {
        let r = api
            .handle(request(
                "PATCH",
                &path(target),
                json!({"active":false}),
                token,
                None,
                n,
            ))
            .await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
}
