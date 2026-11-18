//! C1 webhook routes on memory stores: `webhooks:manage` and project-admin
//! authorization, foreign-project 404, the 20-subscription limit, one-time
//! secrets, D20 replay, rotation, SSRF rejection and test/redeliver.
use runbook_api::{
    Request,
    webhooks::{WebhookHost, WebhookService},
};
use runbook_core::{
    StoreError,
    credentials::{memory::MemoryCredentials, *},
    directory::{memory::MemoryDirectory, *},
    webhooks::{SealedSecret, SecretSealer, WebhookStore, memory::MemoryWebhooks},
};
use runbook_model::{ProjectId, ProjectRole, UserId};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory stores must be synchronous"),
    }
}
fn org(n: u8) -> runbook_model::OrganizationId {
    format!("org_{n:026}").parse().unwrap()
}
fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn project(n: u8) -> ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}

struct Sealer;
impl SecretSealer for Sealer {
    fn seal(&self, context: &str, secret: &str) -> Result<SealedSecret, StoreError> {
        Ok(SealedSecret(format!(
            "sealed:{context}:{}",
            secret.chars().rev().collect::<String>()
        )))
    }
    fn open(&self, context: &str, sealed: &SealedSecret) -> Result<String, StoreError> {
        sealed
            .0
            .strip_prefix(&format!("sealed:{context}:"))
            .map(|s| s.chars().rev().collect())
            .ok_or(StoreError::Backend("sealer"))
    }
}
struct Host {
    allow_private: bool,
    entropy: Cell<u8>,
}
impl WebhookHost for Host {
    type Sealer = Sealer;
    fn sealer(&self) -> Option<&Sealer> {
        Some(&Sealer)
    }
    fn random32(&self) -> Option<[u8; 32]> {
        self.entropy.set(self.entropy.get() + 1);
        Some([self.entropy.get(); 32])
    }
    fn target_policy(&self) -> runbook_core::webhooks::TargetPolicy {
        runbook_core::webhooks::TargetPolicy {
            allow_private: self.allow_private,
            allow_link_local: false,
        }
    }
    /// A name that resolves to a private address at creation time.
    async fn vet_target(&self, url: &str) -> Result<(), &'static str> {
        if url.contains("internal.example") && !self.allow_private {
            Err("webhooks.target_forbidden")
        } else {
            Ok(())
        }
    }
}

fn pat(n: u8, user_n: u8, org_n: u8, scopes: &[Scope]) -> (String, CredentialRecord) {
    let secret = encode_secret(TokenKind::Personal, [n; 32]);
    (
        secret.clone(),
        CredentialRecord {
            id: format!("tok_{n:026}").parse().unwrap(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: user(user_n),
                actor: ActorKind::Human,
            },
            org_id: org(org_n),
            scopes: scopes.iter().copied().collect(),
            projects: None,
            expires_at: Some(i64::MAX),
            revoked_at: None,
            last_used_at: None,
        },
    )
}

struct Fixture {
    api: WebhookService<MemoryDirectory, MemoryCredentials, MemoryWebhooks, Host>,
    admin: String,
    no_scope: String,
    viewer: String,
    service: String,
    next: Cell<u32>,
}
impl Fixture {
    fn new(allow_private: bool) -> Self {
        let d = MemoryDirectory::default();
        let c = MemoryCredentials::default();
        for n in [1u8, 2] {
            let mut mutations = vec![
                DirectoryMutation::CreateOrganization {
                    title: format!("Org{n}"),
                },
                DirectoryMutation::CreateUser(UserRecord {
                    id: user(n),
                    name: format!("Owner{n}"),
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
            ];
            if n == 1 {
                mutations.push(DirectoryMutation::CreateUser(UserRecord {
                    id: user(3),
                    name: "Viewer".into(),
                    active: true,
                    role: OrgRole::Member,
                }));
                mutations.push(DirectoryMutation::GrantProjectRole {
                    project_id: project(1),
                    user_id: user(3),
                    role: ProjectRole::Operator,
                });
            }
            ready(d.commit(DirectoryCommit {
                org_id: org(n),
                expected_revision: 0,
                mutations,
            }))
            .unwrap();
        }
        let (admin, a) = pat(1, 1, 1, &[Scope::WebhooksManage]);
        let (no_scope, b) = pat(2, 1, 1, &[Scope::RunbooksRead, Scope::ProjectAdmin]);
        let (viewer, v) = pat(3, 3, 1, &[Scope::WebhooksManage]);
        let service = encode_secret(TokenKind::Service, [4; 32]);
        let s = CredentialRecord {
            id: format!("tok_{:026}", 4).parse().unwrap(),
            kind: TokenKind::Service,
            hash: secret_hash(&service),
            principal: CredentialPrincipal::Service {
                project_id: project(1),
                role: ProjectRole::Admin,
            },
            org_id: org(1),
            scopes: [Scope::WebhooksManage].into(),
            projects: None,
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        };
        for record in [a, b, v, s] {
            ready(c.insert(record)).unwrap();
        }
        Self {
            api: WebhookService {
                directory: d,
                credentials: c,
                store: MemoryWebhooks::new(1_000),
                host: Host {
                    allow_private,
                    entropy: Cell::new(0),
                },
            },
            admin,
            no_scope,
            viewer,
            service,
            next: Cell::new(100),
        }
    }
    fn call(
        &self,
        method: &str,
        path: &str,
        body: Value,
        token: &str,
        key: Option<&str>,
    ) -> (u16, Value) {
        self.next.set(self.next.get() + 1);
        let n = self.next.get();
        let r = ready(self.api.handle(Request {
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
        }));
        let code = if r.problem {
            r.body["code"].clone()
        } else {
            Value::Null
        };
        (r.status, if r.problem { code } else { r.body })
    }
}

fn hook(name: &str) -> Value {
    json!({"name": name, "url": "https://hooks.example.com/runbook", "events": ["agent.work_available"]})
}
const BASE: &str = "/v1/projects/api/webhooks";

#[test]
fn crud_requires_webhooks_manage_on_an_accessible_project() {
    let f = Fixture::new(false);
    let (status, created) = f.call("POST", BASE, hook("relay"), &f.admin, Some("c1"));
    assert_eq!(status, 201, "{created}");
    let secret = created["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("whsec_"));
    let id = created["webhook"]["id"].as_str().unwrap().to_owned();
    // Scope missing, insufficient role, foreign project, no credential.
    assert_eq!(
        f.call("GET", BASE, Value::Null, &f.no_scope, None),
        (403, json!("auth.scope_missing"))
    );
    assert_eq!(
        f.call("POST", BASE, hook("x"), &f.no_scope, Some("k")),
        (403, json!("auth.scope_missing"))
    );
    assert_eq!(
        f.call("GET", BASE, Value::Null, &f.viewer, None),
        (403, json!("auth.forbidden"))
    );
    let foreign = format!("/v1/projects/{}/webhooks", project(2));
    assert_eq!(
        f.call("GET", &foreign, Value::Null, &f.admin, None),
        (404, json!("resource.not_found"))
    );
    assert_eq!(
        f.call("POST", &foreign, hook("x"), &f.admin, Some("k2")),
        (404, json!("resource.not_found"))
    );
    assert_eq!(
        f.call("GET", BASE, Value::Null, "trb_pat_invalid", None).0,
        401
    );
    // The service principal of the project can manage too.
    let (status, page) = f.call("GET", BASE, Value::Null, &f.service, None);
    assert_eq!(status, 200);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    // Reads never return secret material.
    let one = format!("{BASE}/{id}");
    let (status, view) = f.call("GET", &one, Value::Null, &f.admin, None);
    assert_eq!(status, 200);
    let text = view.to_string() + &page.to_string();
    assert!(!text.contains(&secret) && !text.contains("whsec_") && !text.contains("sealed:"));
    // Update, foreign-project ID lookups, delete.
    let (status, updated) = f.call(
        "PATCH",
        &one,
        json!({"events": ["*"], "tags": ["prod"], "enabled": false}),
        &f.admin,
        None,
    );
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["events"], json!(["*"]));
    assert_eq!(updated["enabled"], false);
    assert_eq!(
        f.call(
            "PATCH",
            &one,
            json!({"events": ["webhook.test"]}),
            &f.admin,
            None
        ),
        (422, json!("webhooks.events_invalid"))
    );
    assert_eq!(
        f.call("PATCH", &one, json!({"secret": "x"}), &f.admin, None),
        (422, json!("request.invalid"))
    );
    assert_eq!(
        f.call(
            "GET",
            &format!("{BASE}/whk_{:026}", 9),
            Value::Null,
            &f.admin,
            None
        ),
        (404, json!("resource.not_found"))
    );
    assert_eq!(
        f.call("DELETE", &one, Value::Null, &f.viewer, None),
        (403, json!("auth.forbidden"))
    );
    assert_eq!(f.call("DELETE", &one, Value::Null, &f.admin, None).0, 204);
    assert_eq!(f.call("DELETE", &one, Value::Null, &f.admin, None).0, 404);
    assert_eq!(
        f.call("PUT", &one, Value::Null, &f.admin, None),
        (405, json!("request.method_not_allowed"))
    );
}

#[test]
fn a_project_holds_at_most_twenty_subscriptions() {
    let f = Fixture::new(false);
    for n in 0..20 {
        let key = format!("k{n}");
        assert_eq!(
            f.call("POST", BASE, hook(&format!("h{n}")), &f.admin, Some(&key))
                .0,
            201
        );
    }
    assert_eq!(
        f.call("POST", BASE, hook("21st"), &f.admin, Some("k21")),
        (409, json!("webhooks.limit_reached"))
    );
}

#[test]
fn posts_follow_d20_and_secrets_are_shown_once() {
    let f = Fixture::new(false);
    assert_eq!(
        f.call("POST", BASE, hook("relay"), &f.admin, None),
        (428, json!("request.idempotency_key_required"))
    );
    let (_, created) = f.call("POST", BASE, hook("relay"), &f.admin, Some("same"));
    assert_eq!(
        f.call("POST", BASE, hook("relay"), &f.admin, Some("same")),
        (409, json!("webhooks.secret_already_issued"))
    );
    assert_eq!(
        f.call("POST", BASE, hook("other"), &f.admin, Some("same")),
        (409, json!("request.idempotency_key_reused"))
    );
    assert_eq!(
        f.call("GET", BASE, Value::Null, &f.admin, None).1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let id = created["webhook"]["id"].as_str().unwrap();
    let rotate = format!("{BASE}/{id}/rotate-secret");
    let (status, rotated) = f.call("POST", &rotate, Value::Null, &f.admin, Some("r1"));
    assert_eq!(status, 200);
    assert_ne!(rotated["secret"], created["secret"]);
    assert_eq!(
        rotated["webhook"]["previous_secret_expires_at_ms"],
        json!(1_000 + 86_400_000)
    );
    assert_eq!(
        f.call("POST", &rotate, Value::Null, &f.admin, Some("r1")),
        (409, json!("webhooks.secret_already_issued"))
    );
    // Test deliveries replay their saved response.
    let test = format!("{BASE}/{id}/test");
    let (status, first) = f.call("POST", &test, Value::Null, &f.admin, Some("t1"));
    assert_eq!(status, 202, "{first}");
    assert_eq!(first["event_type"], "webhook.test");
    assert_eq!(first["state"], "pending");
    assert_eq!(
        f.call("POST", &test, Value::Null, &f.admin, Some("t1")),
        (202, first.clone())
    );
    let (status, log) = f.call(
        "GET",
        &format!("{BASE}/{id}/deliveries"),
        Value::Null,
        &f.admin,
        None,
    );
    assert_eq!(status, 200);
    assert_eq!(log["items"].as_array().unwrap().len(), 1);
    // Redelivery copies the event under a new delivery ID.
    let original = first["id"].as_str().unwrap();
    let again = format!("{BASE}/{id}/deliveries/{original}/redeliver");
    let (status, redelivered) = f.call("POST", &again, Value::Null, &f.admin, Some("d1"));
    assert_eq!(status, 202, "{redelivered}");
    assert_eq!(redelivered["event_id"], first["event_id"]);
    assert_eq!(redelivered["redelivery_of"], first["id"]);
    assert_ne!(redelivered["id"], first["id"]);
    assert_eq!(
        f.call(
            "POST",
            &format!("{BASE}/{id}/deliveries/whd_{:026}/redeliver", 7),
            Value::Null,
            &f.admin,
            Some("d2")
        ),
        (404, json!("resource.not_found"))
    );
    let (_, log) = f.call(
        "GET",
        &format!("{BASE}/{id}/deliveries"),
        Value::Null,
        &f.admin,
        None,
    );
    assert_eq!(log["items"].as_array().unwrap().len(), 2);
    let stored = ready(f.api.store.get_subscription(
        &runbook_core::catalog::TenantScope {
            org_id: org(1),
            project_id: project(1),
        },
        &id.parse().unwrap(),
    ))
    .unwrap()
    .unwrap();
    // Stored only sealed; never the raw secret.
    let raw = serde_json::to_string(&stored).unwrap();
    assert!(!raw.contains(rotated["secret"].as_str().unwrap()));
}

#[test]
fn targets_must_be_public_https_unless_self_hosted() {
    let cloud = Fixture::new(false);
    for (url, code) in [
        ("http://hooks.example.com/x", "webhooks.url_https_required"),
        ("https://127.0.0.1/x", "webhooks.target_forbidden"),
        ("https://192.168.1.10/x", "webhooks.target_forbidden"),
        ("https://[fe80::1]/x", "webhooks.target_forbidden"),
        ("https://internal.example/x", "webhooks.target_forbidden"),
        (
            "https://user:pw@hooks.example.com/x",
            "webhooks.url_credentials",
        ),
    ] {
        let body = json!({"name": "x", "url": url, "events": ["*"]});
        assert_eq!(
            cloud.call("POST", BASE, body, &cloud.admin, Some(url)),
            (422, json!(code)),
            "{url}"
        );
    }
    let self_hosted = Fixture::new(true);
    let metadata = json!({"name": "x", "url": "http://169.254.169.254/latest", "events": ["*"]});
    assert_eq!(
        self_hosted.call("POST", BASE, metadata, &self_hosted.admin, Some("md")),
        (422, json!("webhooks.target_forbidden"))
    );
    for url in ["http://127.0.0.1:9000/x", "https://internal.example/x"] {
        let body = json!({"name": "x", "url": url, "events": ["*"]});
        assert_eq!(
            self_hosted
                .call("POST", BASE, body, &self_hosted.admin, Some(url))
                .0,
            201,
            "{url}"
        );
    }
}

#[test]
fn creation_validates_fields() {
    let f = Fixture::new(false);
    for (body, code) in [
        (
            json!({"name": "", "url": "https://h.example.com", "events": ["*"]}),
            "webhooks.name_invalid",
        ),
        (
            json!({"name": "x", "url": "https://h.example.com", "events": []}),
            "webhooks.events_invalid",
        ),
        (
            json!({"name": "x", "url": "https://h.example.com", "events": ["nope"]}),
            "webhooks.events_invalid",
        ),
        (
            json!({"name": "x", "url": "https://h.example.com", "events": ["*", "step.completed"]}),
            "webhooks.events_invalid",
        ),
        (
            json!({"name": "x", "url": "https://h.example.com", "events": ["*"], "tags": [""]}),
            "webhooks.tags_invalid",
        ),
        (
            json!({"name": "x", "url": "https://h.example.com", "events": ["*"], "runbooks": ["bad"]}),
            "request.invalid",
        ),
    ] {
        let key = body.to_string();
        assert_eq!(
            f.call("POST", BASE, body, &f.admin, Some(&key)),
            (422, json!(code))
        );
    }
}

#[test]
fn contract_catalog_matches_the_core_catalog() {
    assert_eq!(
        runbook_contract::openapi()["x-webhook-delivery"]["events"],
        json!(runbook_core::webhooks::CATALOG)
    );
}

/// C1: a subscription the system disabled carries a durable admin notice
/// on get and list until an admin re-enables it.
#[test]
fn auto_disable_leaves_an_admin_notice_until_reenabled() {
    let f = Fixture::new(false);
    let (status, created) = f.call("POST", BASE, hook("relay"), &f.admin, Some("c1"));
    assert_eq!(status, 201, "{created}");
    let id = created["webhook"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["webhook"]["admin_notice"], Value::Null);
    let scope = runbook_core::catalog::TenantScope {
        org_id: org(1),
        project_id: project(1),
    };
    let webhook = id.parse().unwrap();
    let mut stored = ready(f.api.store.get_subscription(&scope, &webhook))
        .unwrap()
        .unwrap();
    stored.record_attempt(false, 1_000, Some("timeout"));
    // The streak is continuous: attempts keep failing in between (F21).
    stored.record_attempt(false, 1_000 + 36 * 3_600_000, Some("timeout"));
    stored.record_attempt(false, 1_000 + 3 * 86_400_000, Some("http_503"));
    ready(f.api.store.save_subscription(&scope, stored, None)).unwrap();
    let path = format!("{BASE}/{id}");
    let (status, view) = f.call("GET", &path, Value::Null, &f.admin, None);
    assert_eq!(status, 200);
    assert_eq!(view["enabled"], false);
    let notice = &view["admin_notice"];
    assert_eq!(notice["kind"], "webhook.disabled");
    assert_eq!(notice["last_failure"], "http_503");
    assert_eq!(notice["failing_since_ms"], 1_000);
    assert_eq!(notice["at_ms"], 1_000 + 3 * 86_400_000);
    let (_, list) = f.call("GET", BASE, Value::Null, &f.admin, None);
    assert_eq!(&list["items"][0]["admin_notice"], notice);
    // Renaming keeps it; re-enabling acknowledges it.
    let (status, renamed) = f.call("PATCH", &path, json!({"name": "renamed"}), &f.admin, None);
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(&renamed["admin_notice"], notice);
    let (status, enabled) = f.call("PATCH", &path, json!({"enabled": true}), &f.admin, None);
    assert_eq!(status, 200, "{enabled}");
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["admin_notice"], Value::Null);
    assert_eq!(enabled["disabled_reason"], Value::Null);
}
