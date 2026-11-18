//! MCP2: OAuth access tokens on `/mcp` (the resource server half), on
//! in-memory backends: the grant is the linked directory user as an agent,
//! with the consent's scopes narrowed to the user's role; tokens minted for
//! another resource, expired or unverified are refused; `/mcp` answers 401
//! with the RFC 9728 `resource_metadata` challenge.
#[path = "support/fake_backends.rs"]
mod fake_backends;
use fake_backends::{ORIGIN, request, router, run};
use runbook_api::{
    Credential, ResolvedSession,
    http::Headers,
    oauth::TokenIntrospector,
    session::{Unavailable, link},
};
use runbook_core::{
    auth_links::{AuthLinkStore, LinkRequest},
    credentials::{ActorKind, CredentialPrincipal, Scope},
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, OrgRole, UserRecord},
};
use runbook_model::{OrganizationId, ProjectId, ProjectRole, UserId};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn id<T: std::str::FromStr>(prefix: &str, n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("{prefix}_{n:026}").parse().unwrap()
}

/// The fake's owner (admin of project 1) plus a viewer, each linked to an
/// auth user (`ba_1`, `ba_2`).
fn seeded() -> runbook_api::router::Router<fake_backends::Fake> {
    let r = router(true);
    run(r.backends.seed());
    let org: OrganizationId = id("org", 1);
    let directory = r.backends.directory.clone();
    run(directory.commit(DirectoryCommit {
        org_id: org.clone(),
        expected_revision: 1,
        mutations: vec![
            DirectoryMutation::CreateUser(UserRecord {
                id: id("usr", 2),
                name: "Viewer".into(),
                active: true,
                role: OrgRole::Member,
            }),
            DirectoryMutation::GrantProjectRole {
                project_id: id::<ProjectId>("prj", 1),
                user_id: id("usr", 2),
                role: ProjectRole::Viewer,
            },
            DirectoryMutation::BindVerifiedEmail {
                user_id: id("usr", 1),
                email: "owner@example.test".into(),
            },
            DirectoryMutation::BindVerifiedEmail {
                user_id: id("usr", 2),
                email: "viewer@example.test".into(),
            },
        ],
    }))
    .unwrap();
    for (n, email) in [(1u8, "owner@example.test"), (2, "viewer@example.test")] {
        run(directory.link_identity(
            &org,
            LinkRequest {
                auth_user_id: format!("ba_{n}"),
                verified_email: email.into(),
                user_id: id::<UserId>("usr", n),
                name: "x".into(),
                invitation: None,
                now_ms: 1,
            },
        ))
        .unwrap();
    }
    r
}

const SECRET: &str = "trb_oat_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// An introspected token linked as the router links it (D45): its principal.
async fn grant<B: runbook_api::backends::Backends>(
    backends: &B,
    mut token: ResolvedSession,
) -> Result<Box<runbook_core::credentials::CredentialRecord>, (u16, &'static str)> {
    link(backends, &mut token).await?;
    Ok(token.principal.unwrap())
}

/// What the introspection answers for an active token of user 1 or 2.
fn token(user: u8, scope: &str) -> ResolvedSession {
    ResolvedSession {
        auth_user_id: format!("ba_{user}"),
        email: if user == 1 {
            "owner@example.test".into()
        } else {
            "viewer@example.test".into()
        },
        email_verified: true,
        name: None,
        picture_url: None,
        session_id: format!("oauth:token-{scope}"),
        expires_at_ms: i64::MAX,
        principal: None,
        scopes: Some(scope.to_owned()),
    }
}

#[test]
fn a_grant_is_the_users_agent_with_the_consented_scopes_within_the_role() {
    let r = seeded();
    let g = run(grant(
        &r.backends,
        token(1, "runbooks:read runbooks:publish offline_access"),
    ))
    .unwrap();
    assert_eq!(
        g.scopes,
        BTreeSet::from([Scope::RunbooksRead, Scope::RunbooksPublish])
    );
    assert_eq!(
        g.principal,
        CredentialPrincipal::User {
            user_id: id("usr", 1),
            actor: ActorKind::Agent
        }
    );
    assert_eq!(g.expires_at, Some(i64::MAX));
    // The viewer consented to more than the role allows: only reads remain.
    let v = run(grant(
        &r.backends,
        token(
            2,
            "runbooks:read runbooks:write runbooks:publish executions:start",
        ),
    ))
    .unwrap();
    assert_eq!(v.scopes, BTreeSet::from([Scope::RunbooksRead]));
    // A different token (session ID) is a different credential ID.
    let other = run(grant(&r.backends, token(1, "runbooks:read"))).unwrap();
    assert_ne!(other.id, g.id);
}

#[test]
fn a_person_without_access_is_refused_and_a_session_stays_human() {
    let r = seeded();
    // A person with no link, member or invitation: refused by D45.
    let mut stranger = token(1, "runbooks:read");
    stranger.auth_user_id = "ba_9".into();
    stranger.email = "stranger@example.test".into();
    assert_eq!(
        run(grant(&r.backends, stranger)).map(|_| ()).unwrap_err(),
        (403, "auth.forbidden")
    );
    // Without `scopes` (a browser session) nothing is narrowed: human, role scopes.
    let mut session = token(2, "runbooks:read");
    session.scopes = None;
    let human = run(grant(&r.backends, session)).unwrap();
    assert!(matches!(
        human.principal,
        CredentialPrincipal::User {
            actor: ActorKind::Human,
            ..
        }
    ));
    // Nothing granted that the role allows: an agent with no scopes.
    let none = run(grant(&r.backends, token(2, "offline_access"))).unwrap();
    assert!(none.scopes.is_empty());
}

#[test]
fn the_api_answers_a_grant_as_the_scopes_allow() {
    let r = seeded();
    let call = |user: u8, scope: &str, method: &str, path: &str, body: Value| {
        let mut q = request(method, path, None, body);
        q.credential = Some(Credential::Session(token(user, scope)));
        q.idempotency_key = Some("k-1".into());
        run(r.handle(q))
    };
    let orgs = call(1, "runbooks:read", "GET", "/v1/orgs", Value::Null);
    assert_eq!(orgs.status, 200, "{}", orgs.body);
    let project: ProjectId = id("prj", 1);
    let team = call(
        1,
        "runbooks:read",
        "POST",
        "/v1/orgs/org_00000000000000000000000001/teams",
        json!({"slug": "oncall", "title": "On call"}),
    );
    assert_eq!(team.status, 403, "no administration scope: {}", team.body);
    let me = call(2, "runbooks:read", "GET", "/v1/me", Value::Null);
    assert_eq!(me.status, 200, "{}", me.body);
    assert_eq!(me.body["credential"]["actor_kind"], "agent", "{}", me.body);
    assert_eq!(me.body["credential"]["scopes"], json!(["runbooks:read"]));
    let _ = project;
}

/// Review 7: an OAuth grant rides the session credential, but it is an
/// agent: it never approves a device login (D47: a person in a browser) and
/// never mints execution credentials (D46).
#[test]
fn an_oauth_grant_never_approves_device_logins_or_mints_execution_credentials() {
    let r = seeded();
    let call = |credential: ResolvedSession, method: &str, path: &str, body: Value| {
        let mut q = request(method, path, None, body);
        q.credential = Some(Credential::Session(credential));
        q.idempotency_key = Some("k-review-7".into());
        run(r.handle(q))
    };
    let every = "runbooks:read runbooks:write runbooks:publish executions:read executions:start executions:act skills:read";
    let approve = json!({"user_code": "ABCD-EFGH", "approve": true});
    let refused = call(
        token(1, every),
        "POST",
        runbook_api::device::APPROVE,
        approve.clone(),
    );
    assert_eq!(
        (refused.status, refused.body["code"].as_str()),
        (403, Some("auth.forbidden")),
        "{}",
        refused.body
    );
    // The same person's browser session gets past the credential check.
    let mut browser = token(1, every);
    browser.scopes = None;
    let as_browser = call(browser, "POST", runbook_api::device::APPROVE, approve);
    assert_ne!(
        as_browser.body["code"].as_str(),
        Some("auth.forbidden"),
        "{}",
        as_browser.body
    );
    let lookup = call(
        token(1, every),
        "POST",
        "/v1/device/lookup",
        json!({"user_code": "ABCD-EFGH"}),
    );
    assert_eq!(lookup.status, 403, "{}", lookup.body);
    let mint = call(
        token(1, every),
        "POST",
        "/v1/executions/exe_00000000000000000000000001/tokens",
        json!({}),
    );
    assert_eq!(mint.status, 403, "{}", mint.body);
}

/// MCP2 re-check 1: a person lists and disconnects their OAuth apps; only a
/// human personal credential with tokens:manage (or a browser session).
#[test]
fn a_person_lists_and_disconnects_connected_apps() {
    let r = seeded();
    let call = |credential: Credential, method: &str, path: &str| {
        let mut q = request(method, path, None, Value::Null);
        q.credential = Some(credential);
        run(r.handle(q))
    };
    let mut browser = token(1, "");
    browser.scopes = None;
    // No grants yet (the auth service answers none).
    let empty = call(Credential::Session(browser.clone()), "GET", "/v1/me/apps");
    assert_eq!((empty.status, &empty.body), (200, &json!({"items": []})));
    // What the auth service lists, mapped.
    r.backends.apps.borrow_mut().1 = Some(json!({"grants": [{
        "client_id": "abcDEF", "client_name": "Claude", "redirect_hosts": ["claude.ai"],
        "scopes": ["runbooks:read"], "created_at": 1, "last_used_at": 2, "expires_at": 3
    }]}));
    let listed = call(Credential::Session(browser.clone()), "GET", "/v1/me/apps");
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(
        listed.body,
        json!({"items": [{"client_id": "abcDEF", "client_name": "Claude", "redirect_hosts": ["claude.ai"],
            "scopes": ["runbooks:read"], "created_at_ms": 1, "last_used_at_ms": 2, "expires_at_ms": 3}]})
    );
    let calls = r.backends.apps.borrow().0.clone();
    assert_eq!(
        calls.last().unwrap(),
        &(
            "oauth/grants".to_owned(),
            json!({"auth_user_ids": ["ba_1"]})
        )
    );
    // Disconnect: the person's own auth users and the client.
    r.backends.apps.borrow_mut().1 = Some(json!({"revoked": true}));
    let gone = call(
        Credential::Session(browser.clone()),
        "DELETE",
        "/v1/me/apps/abcDEF",
    );
    assert_eq!(gone.status, 204, "{}", gone.body);
    assert_eq!(
        r.backends.apps.borrow().0.last().unwrap(),
        &(
            "oauth/grants/revoke".to_owned(),
            json!({"auth_user_ids": ["ba_1"], "client_id": "abcDEF"})
        )
    );
    r.backends.apps.borrow_mut().1 = Some(json!({"revoked": false}));
    let unknown = call(
        Credential::Session(browser.clone()),
        "DELETE",
        "/v1/me/apps/abcDEF",
    );
    assert_eq!(unknown.status, 404);
    let bad = call(Credential::Session(browser), "DELETE", "/v1/me/apps/a%2Fb");
    assert_ne!(bad.status, 204);
    // An OAuth grant (an agent) never manages connected apps.
    let agent = call(
        Credential::Session(token(1, "runbooks:read")),
        "GET",
        "/v1/me/apps",
    );
    assert_eq!(agent.status, 403, "{}", agent.body);
}

/// MCP2 re-check: an OAuth token whose person was removed from the
/// organization is refused on /mcp, and an auth service that cannot answer
/// is 503 (never invalid_token).
#[test]
fn mcp_refuses_a_removed_member_and_reports_an_unavailable_auth_service() {
    let r = seeded();
    let bearer = format!("Bearer {SECRET}");
    let org: OrganizationId = id("org", 1);
    let revision = run(r.backends.directory.snapshot(&org)).unwrap().revision;
    run(r.backends.directory.commit(DirectoryCommit {
        org_id: org,
        expected_revision: revision,
        mutations: vec![DirectoryMutation::SetUserActive {
            user_id: id("usr", 2),
            active: false,
        }],
    }))
    .unwrap();
    let removed = mcp(
        &r,
        Some(&Stub(Some(token(2, "runbooks:read")))),
        &[("authorization", &bearer)],
    );
    assert_eq!(
        removed.status,
        403,
        "{}",
        String::from_utf8_lossy(&removed.body)
    );
    struct Down;
    impl TokenIntrospector for Down {
        async fn introspect(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<ResolvedSession>, Unavailable> {
            Err(Unavailable)
        }
    }
    let down = mcp(&r, Some(&Down), &[("authorization", &bearer)]);
    assert_eq!(down.status, 503);
    assert_eq!(down.header_value("www-authenticate"), None);
}

/// A stub of the auth service's introspection: answers for this server's
/// `/mcp` only, as the auth service does.
struct Stub(Option<ResolvedSession>);
impl TokenIntrospector for Stub {
    async fn introspect(
        &self,
        _: &str,
        resource: &str,
    ) -> Result<Option<ResolvedSession>, Unavailable> {
        Ok(self
            .0
            .clone()
            .filter(|_| resource == format!("{ORIGIN}/mcp")))
    }
}
/// The auth service as if the token were minted for another resource.
struct Elsewhere;
impl TokenIntrospector for Elsewhere {
    async fn introspect(
        &self,
        _: &str,
        resource: &str,
    ) -> Result<Option<ResolvedSession>, Unavailable> {
        Ok((resource == "https://elsewhere.example/mcp").then(|| token(1, "runbooks:read")))
    }
}

fn mcp<I: TokenIntrospector>(
    r: &runbook_api::router::Router<fake_backends::Fake>,
    stub: Option<&I>,
    headers: &[(&str, &str)],
) -> runbook_api::http::HttpResponse {
    let mut h: Vec<(&str, &str)> = vec![("content-type", "application/json")];
    h.extend_from_slice(headers);
    let headers: Headers = h.into_iter().collect();
    let body = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
    run(runbook_api::mcp::handle(
        r,
        stub,
        "POST",
        &headers,
        body.to_string().as_bytes(),
        "req_1",
        10,
        &|| false,
    ))
}

#[test]
fn mcp_answers_oauth_tokens_and_challenges_with_the_metadata() {
    let r = seeded();
    let metadata = format!("{ORIGIN}/.well-known/oauth-protected-resource/mcp");
    let active = Stub(Some(token(1, "runbooks:read")));
    let inactive = Stub(None);
    // No bearer, or only a cookie: 401 naming the metadata.
    for headers in [vec![], vec![("cookie", "runbook.session_token=x.y")]] {
        let r = mcp(&r, Some(&active), &headers);
        assert_eq!(r.status, 401);
        assert_eq!(
            r.header_value("www-authenticate"),
            Some(format!("Bearer resource_metadata=\"{metadata}\"").as_str())
        );
    }
    let bearer = format!("Bearer {SECRET}");
    let ok = mcp(&r, Some(&active), &[("authorization", &bearer)]);
    assert_eq!(ok.status, 200, "{}", String::from_utf8_lossy(&ok.body));
    let refused = mcp(&r, Some(&inactive), &[("authorization", &bearer)]);
    assert_eq!(refused.status, 401);
    assert_eq!(
        refused.header_value("www-authenticate"),
        Some(format!("Bearer resource_metadata=\"{metadata}\", error=\"invalid_token\"").as_str())
    );
    // A token minted for another resource: this /mcp names itself.
    let elsewhere = mcp(&r, Some(&Elsewhere), &[("authorization", &bearer)]);
    assert_eq!(elsewhere.status, 401);
    // A browser session's answer (no grant) is not an OAuth token either,
    // nor is an unverified email.
    let mut session = token(1, "runbooks:read");
    session.scopes = None;
    let session = mcp(
        &r,
        Some(&Stub(Some(session))),
        &[("authorization", &bearer)],
    );
    assert_eq!(session.status, 401);
    let mut unverified = token(1, "runbooks:read");
    unverified.email_verified = false;
    let unverified = mcp(
        &r,
        Some(&Stub(Some(unverified))),
        &[("authorization", &bearer)],
    );
    assert_eq!(unverified.status, 401);
    // A person with no directory access: 403.
    let mut stranger = token(1, "runbooks:read");
    stranger.auth_user_id = "ba_9".into();
    stranger.email = "stranger@example.test".into();
    let stranger = mcp(
        &r,
        Some(&Stub(Some(stranger))),
        &[("authorization", &bearer)],
    );
    assert_eq!(stranger.status, 403);
    // Without an introspector (no auth service) the token is unknown, and
    // without sign-in there is no metadata to name.
    let unknown = mcp(&r, None::<&Stub>, &[("authorization", &bearer)]);
    assert_eq!(unknown.status, 401);
    assert_eq!(
        unknown.header_value("www-authenticate"),
        Some("Bearer error=\"invalid_token\"")
    );
    let plain = router(false);
    let r2 = mcp(&plain, Some(&active), &[]);
    assert_eq!(r2.header_value("www-authenticate"), Some("Bearer"));
}
