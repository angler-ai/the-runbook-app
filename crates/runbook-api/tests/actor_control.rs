use runbook_api::{Request, actors::*};
use runbook_core::{credentials::*, directory::*};
use runbook_model::*;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, future::Future};
fn id<T: std::str::FromStr>(prefix: &str, n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("{prefix}_{n:026}").parse().unwrap()
}
fn run(f: impl Future<Output = ()>) {
    let mut f = Box::pin(f);
    assert!(matches!(
        f.as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
        std::task::Poll::Ready(())
    ));
}
#[derive(Default)]
struct Fake {
    saved: RefCell<BTreeMap<String, ([u8; 32], Value)>>,
    calls: RefCell<usize>,
    last_principal: RefCell<Option<Principal>>,
    stale: RefCell<bool>,
}
impl ActorBackend for Fake {
    async fn step_run_execution(
        &self,
        org: &OrganizationId,
        step: &StepRunId,
    ) -> Result<Option<(ExecutionId, ProjectId)>, ApiError> {
        Ok((org == &id("org", 1) && step == &id("sr", 1)).then(|| (id("exe", 1), id("prj", 1))))
    }
    async fn execution_project(
        &self,
        org: &OrganizationId,
        exe: &ExecutionId,
    ) -> Result<Option<ProjectId>, ApiError> {
        Ok((org == &id("org", 1) && exe == &id("exe", 1)).then(|| id("prj", 1)))
    }
    async fn find_replay(
        &self,
        _: &ActorTarget,
        r: &runbook_api::actor_requests::ActorReplay,
    ) -> Result<Option<Value>, ApiError> {
        match self.saved.borrow().get(&r.key) {
            Some((hash, value)) if hash == &r.request_hash => Ok(Some(value.clone())),
            Some(_) => Err(ApiError::new(409, "request.idempotency_key_reused")),
            None => Ok(None),
        }
    }
    async fn execute(&self, i: ActorInvocation) -> Result<Value, ApiError> {
        if let Some(r) = &i.replay
            && let Some(value) = self.find_replay(&i.target, r).await?
        {
            return Ok(value);
        }
        if *self.stale.borrow() {
            return Err(ApiError::new(409, "claim.stale_generation"));
        }
        *self.calls.borrow_mut() += 1;
        *self.last_principal.borrow_mut() = Some(i.principal.clone());
        let value = json!({"ok":true});
        if let Some(r) = i.replay {
            self.saved
                .borrow_mut()
                .insert(r.key, (r.request_hash, value.clone()));
        }
        Ok(value)
    }
}
async fn fixture() -> ActorService<
    runbook_core::directory::memory::MemoryDirectory,
    runbook_core::credentials::memory::MemoryCredentials,
    Fake,
> {
    let d = runbook_core::directory::memory::MemoryDirectory::default();
    let c = runbook_core::credentials::memory::MemoryCredentials::default();
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org".into(),
        },
        DirectoryMutation::CreateProject(ProjectRecord {
            id: id("prj", 1),
            org_id: id("org", 1),
            slug: "api".into(),
            title: "API".into(),
        }),
    ];
    for n in 1..=3 {
        mutations.push(DirectoryMutation::CreateUser(UserRecord {
            id: id("usr", n),
            name: "User".into(),
            active: true,
            role: if n == 1 {
                OrgRole::Owner
            } else {
                OrgRole::Member
            },
        }));
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: id("prj", 1),
            user_id: id("usr", n),
            role: match n {
                1 => ProjectRole::Admin,
                2 => ProjectRole::Operator,
                _ => ProjectRole::Viewer,
            },
        });
    }
    d.commit(DirectoryCommit {
        org_id: id("org", 1),
        expected_revision: 0,
        mutations,
    })
    .await
    .unwrap();
    for n in 1..=5 {
        let secret = encode_secret(TokenKind::Personal, [n; 32]);
        c.insert(CredentialRecord {
            id: id("tok", n),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: id("usr", n.min(3)),
                actor: if n == 5 {
                    runbook_core::credentials::ActorKind::Agent
                } else {
                    runbook_core::credentials::ActorKind::Human
                },
            },
            org_id: id("org", 1),
            scopes: if n == 4 {
                [Scope::ExecutionsRead].into()
            } else {
                [Scope::ExecutionsAct, Scope::ExecutionsAdmin].into()
            },
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .unwrap();
    }
    ActorService {
        directory: d,
        credentials: c,
        backend: Fake::default(),
        evidence_schemes: vec![],
    }
}
fn req(n: u8, op: &str, key: Option<&str>, body: Value) -> Request {
    Request {
        method: "POST".into(),
        path: format!("/v1/step-runs/{}/{}", id::<StepRunId>("sr", 1), op),
        query: Default::default(),
        body,
        credential: Some(runbook_api::Credential::Bearer(encode_secret(
            TokenKind::Personal,
            [n; 32],
        ))),
        idempotency_key: key.map(str::to_owned),
        request_id: "req_test".into(),
        new_id: format!("{:026}", 9),
        if_match: None,
        client_ip: None,
    }
}
#[test]
fn actor_post_replay_precedes_stale_state_checks() {
    run(async {
        let s = fixture().await;
        let body = json!({"step_run_id":id::<StepRunId>("sr",1),"claim":{"id":id::<ClaimId>("clm",1),"generation":1},"status":"succeeded"});
        assert_eq!(
            s.handle(req(2, "report", None, body.clone())).await.status,
            428
        );
        let first = s.handle(req(2, "report", Some("key"), body.clone())).await;
        assert_eq!(first.status, 200);
        *s.backend.stale.borrow_mut() = true;
        let retry = s.handle(req(2, "report", Some("key"), body.clone())).await;
        assert_eq!(retry.body, first.body);
        assert_eq!(*s.backend.calls.borrow(), 1);
        assert_eq!(
            s.handle(req(2, "report", Some("key"), json!({"status":"failed"})))
                .await
                .status,
            409
        );
        let snap = s.directory.snapshot(&id("org", 1)).await.unwrap();
        s.directory
            .commit(DirectoryCommit {
                org_id: id("org", 1),
                expected_revision: snap.revision,
                mutations: vec![DirectoryMutation::SetUserActive {
                    user_id: id("usr", 2),
                    active: false,
                }],
            })
            .await
            .unwrap();
        assert_eq!(
            s.handle(req(2, "report", Some("key"), body)).await.status,
            403
        );
    });
}
#[test]
fn actor_routes_enforce_current_authorization() {
    run(async {
        let s = fixture().await;
        for n in [3, 4, 5] {
            assert_eq!(
                s.handle(req(n, "claim", Some("key"), json!({"session_id":"x"})))
                    .await
                    .status,
                403
            );
        }
        let mut missing = req(1, "claim", Some("key"), json!({}));
        missing.credential = None;
        assert_eq!(s.handle(missing).await.status, 401);
        let mut foreign = req(1, "claim", Some("key"), json!({}));
        foreign.path = foreign.path.replace(
            &id::<StepRunId>("sr", 1).to_string(),
            &id::<StepRunId>("sr", 2).to_string(),
        );
        assert_eq!(s.handle(foreign).await.status, 404);
        for n in [1, 2] {
            let r = s
                .handle(req(
                    n,
                    "release",
                    Some("release"),
                    json!({"reason":"Owner absent"}),
                ))
                .await;
            assert_eq!(r.status, if n == 1 { 200 } else { 403 });
        }
        assert_eq!(
            s.handle(req(1, "release", Some("empty"), json!({"reason":" "})))
                .await
                .status,
            422
        );
        let mut patch = req(2, "unused", None, json!({"action":"pause"}));
        patch.path = format!("/v1/executions/{}", id::<ExecutionId>("exe", 1));
        patch.method = "PATCH".into();
        assert_eq!(s.handle(patch).await.status, 200);
    });
}
#[test]
fn actor_errors_keep_problem_codes() {
    run(async {
        let s = fixture().await;
        *s.backend.stale.borrow_mut() = true;
        let r = s
            .handle(req(2, "claim", Some("key"), json!({"session_id":"x"})))
            .await;
        assert_eq!(r.status, 409);
        assert!(r.problem);
        assert_eq!(r.body["code"], "claim.stale_generation");
        assert_eq!(r.request_id, "req_test");
        let mut wrong = req(2, "claim", Some("key"), json!({}));
        wrong.method = "GET".into();
        assert_eq!(s.handle(wrong).await.status, 405);
    });
}

#[test]
fn actor_credential_restrictions_precede_backend_mutations() {
    run(async {
        let s = fixture().await;
        let template = s
            .credentials
            .find_by_hash(&secret_hash(&encode_secret(TokenKind::Personal, [1; 32])))
            .await
            .unwrap()
            .unwrap();
        for case in 0..5u8 {
            let n = 10 + case;
            let secret = encode_secret(TokenKind::Personal, [n; 32]);
            let mut record = template.clone();
            record.id = id("tok", n);
            record.hash = secret_hash(&secret);
            match case {
                0 => record.expires_at = Some(0),
                1 => record.revoked_at = Some(0),
                2 => record.projects = Some(Default::default()),
                3 => record.scopes = [Scope::ExecutionsRead].into(),
                _ => record.org_id = id("org", 2),
            }
            s.credentials.insert(record).await.unwrap();
            let r = s
                .handle(req(n, "claim", Some("key"), json!({"session_id":"x"})))
                .await;
            assert_eq!(
                r.status,
                match case {
                    0 | 1 => 401,
                    2 | 3 => 403,
                    _ => 404,
                }
            );
        }
        assert_eq!(*s.backend.calls.borrow(), 0);
        for agent in [false, true] {
            let n = if agent { 21 } else { 20 };
            let kind = if agent {
                TokenKind::Personal
            } else {
                TokenKind::Service
            };
            let secret = encode_secret(kind, [n; 32]);
            let mut record = template.clone();
            record.id = id("tok", n);
            record.hash = secret_hash(&secret);
            record.kind = kind;
            record.principal = if agent {
                CredentialPrincipal::User {
                    user_id: id("usr", 1),
                    actor: runbook_core::credentials::ActorKind::Agent,
                }
            } else {
                CredentialPrincipal::Service {
                    project_id: id("prj", 1),
                    role: ProjectRole::Admin,
                }
            };
            s.credentials.insert(record).await.unwrap();
            let mut request = req(
                n,
                "release",
                Some("release"),
                json!({"reason":"Cannot impersonate a human"}),
            );
            request.credential = Some(runbook_api::Credential::Bearer(secret.clone()));
            assert_eq!(s.handle(request).await.status, 403);
            let mut request = req(
                n,
                "claim",
                Some("claim"),
                json!({"session_id":"session","actor":{"kind":"agent","agent_name":"bot"},"user_id":id::<UserId>("usr",3)}),
            );
            request.credential = Some(runbook_api::Credential::Bearer(secret));
            assert_eq!(s.handle(request).await.status, 200);
            assert_eq!(
                *s.backend.last_principal.borrow(),
                Some(if agent {
                    Principal::Agent {
                        sponsor: id("usr", 1),
                    }
                } else {
                    Principal::Service {
                        token_id: id("tok", 20),
                        project_id: id("prj", 1),
                    }
                })
            );
        }
    });
}

#[test]
fn every_actor_post_requires_a_key_and_patch_does_not() {
    run(async {
        let s = fixture().await;
        for op in [
            "claim",
            "heartbeat",
            "logs",
            "report",
            "checklist",
            "release",
            "decision",
        ] {
            let body = if op == "release" {
                json!({"claim":{"id":id::<ClaimId>("clm",1),"generation":1}})
            } else {
                json!({})
            };
            let result = s.handle(req(1, op, None, body)).await;
            assert_eq!(result.status, 428, "{op}");
            assert_eq!(result.body["code"], "request.idempotency_key_required");
        }
        let mut signal = req(1, "unused", None, json!({"payload":{}}));
        signal.path = format!(
            "/v1/executions/{}/signals/wake",
            id::<ExecutionId>("exe", 1)
        );
        assert_eq!(s.handle(signal).await.status, 428);
        let mut patch = req(2, "unused", None, json!({"action":"resume"}));
        patch.method = "PATCH".into();
        patch.path = format!("/v1/executions/{}", id::<ExecutionId>("exe", 1));
        assert_eq!(s.handle(patch).await.status, 200);
    });
}
