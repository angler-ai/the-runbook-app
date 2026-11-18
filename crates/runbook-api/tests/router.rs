//! CF0 review: the router itself, on in-memory backends. `/internal/*` is
//! never routed by `handle`, the host duties that need no transport are the
//! router's, and every area is reached through `handle`, in the order the
//! manifest pins.
#[path = "support/fake_backends.rs"]
mod fake_backends;
use fake_backends::{request, router, run};
use runbook_api::{
    problem::Response,
    router::{Area, area},
};
use serde_json::{Value, json};

const EXE_BEARER: &str = "trb_exe_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// Method, path, bearer, the area that answers, and its answer.
type Smoke<'a> = (
    &'a str,
    String,
    Option<&'a str>,
    Area,
    (u16, Option<&'a str>),
);

fn code(r: &Response) -> Option<&str> {
    r.body["code"].as_str()
}

/// What the directory service alone answers (the router's fallback).
fn directory_answer(r: runbook_api::Request) -> Response {
    let fake = router(true);
    run(runbook_api::Service::new(
        runbook_api::backends::Backends::directory(&fake.backends),
        runbook_api::backends::Backends::credentials(&fake.backends),
        fake.config.discovery.clone(),
    )
    .handle(r))
}

#[test]
fn internal_paths_never_reach_handle() {
    let router = router(true);
    for (method, path) in [
        ("POST", "/internal/can-register"),
        ("POST", "/internal/session"),
        ("GET", "/internal/mcp/authenticate"),
        ("GET", "/internal/"),
    ] {
        let r = run(router.handle(request(
            method,
            path,
            None,
            json!({"email": "a@example.test"}),
        )));
        assert_eq!(
            (r.status, code(&r)),
            (404, Some("resource.not_found")),
            "{method} {path}"
        );
    }
}

#[test]
fn can_register_is_its_own_entry_point() {
    let ask = || {
        request(
            "POST",
            "/internal/can-register",
            None,
            json!({"email": "a@example.test"}),
        )
    };
    let r = run(router(true).can_register(ask()));
    assert_eq!(r.status, 200, "{:?}", r.body);
    assert_eq!(r.body, json!({"allowed": false}));
    // Without sign-in the hook does not exist.
    let r = run(router(false).can_register(ask()));
    assert_eq!((r.status, code(&r)), (404, Some("resource.not_found")));
}

/// A body from a caller with no credential is 401 before any route (F45),
/// except where the body authenticates (refresh) or the device grant.
#[test]
fn an_anonymous_body_is_refused_before_routing() {
    let router = router(true);
    for (method, path) in [
        ("POST", "/v1/unknown"),
        ("POST", "/v1/orgs/org_00000000000000000000000001/projects"),
        ("PATCH", "/v1/memory/mem_00000000000000000000000001"),
    ] {
        let r = run(router.handle(request(method, path, None, json!({"x": 1}))));
        assert_eq!(
            (r.status, code(&r)),
            (401, Some("auth.required")),
            "{method} {path}"
        );
    }
    for (method, path) in [
        (
            "POST",
            "/v1/executions/exe_00000000000000000000000001/tokens/refresh",
        ),
        ("POST", "/v1/device/token"),
        ("POST", "/v1/device/code"),
    ] {
        let r = run(router.handle(request(method, path, None, json!({"x": 1}))));
        assert_ne!(code(&r), Some("auth.required"), "{method} {path}");
    }
    // No body, no refusal: the route decides.
    let r = run(router.handle(request("GET", "/.well-known/runbook", None, Value::Null)));
    assert_eq!(r.status, 200);
}

/// One request per area through `Router::handle`, as the seeded owner
/// (`OWNER`), an unknown execution credential or nobody. Each answer is
/// pinned, and none is the directory service's answer for that path (404
/// or 405), so a request that fell through to the directory, or reached
/// another area, fails here.
#[test]
fn every_area_is_reached_through_handle() {
    let router = router(true);
    let owner = run(router.backends.seed());
    let o = Some("OWNER");
    let x = Some(EXE_BEARER);
    let id = "00000000000000000000000001";
    let cases: Vec<Smoke> = vec![
        (
            "POST",
            "/v1/device/code".into(),
            None,
            Area::Device,
            (200, None),
        ),
        (
            "GET",
            format!("/v1/orgs/org_{id}/invitations"),
            o,
            Area::Invitations,
            (200, None),
        ),
        (
            "GET",
            format!("/v1/projects/prj_{id}/memory"),
            o,
            Area::Memory,
            (200, None),
        ),
        (
            "GET",
            format!("/v1/runbooks/rb_{id}/archives"),
            o,
            Area::Archives,
            (503, Some("storage.unavailable")),
        ),
        (
            "POST",
            format!("/v1/step-runs/sr_{id}/evidence"),
            o,
            Area::Evidence,
            (503, Some("uploads.not_configured")),
        ),
        (
            "POST",
            format!("/v1/executions/exe_{id}/tokens"),
            o,
            Area::ExecutionCredentials,
            (428, Some("request.idempotency_key_required")),
        ),
        (
            "GET",
            format!("/v1/executions/exe_{id}"),
            x,
            Area::ExecutionAccess,
            (401, Some("auth.invalid_credential")),
        ),
        ("GET", "/v1/tokens".into(), o, Area::Tokens, (200, None)),
        ("GET", "/v1/me".into(), o, Area::Me, (200, None)),
        (
            "GET",
            "/v1/me/tasks".into(),
            o,
            Area::MeTasks,
            (503, Some("storage.unavailable")),
        ),
        // Unknown to the owner as to the directory (404), so nobody asks.
        (
            "GET",
            format!("/v1/step-runs/sr_{id}"),
            None,
            Area::StepReads,
            (401, Some("auth.required")),
        ),
        (
            "GET",
            format!("/v1/projects/prj_{id}/webhooks"),
            o,
            Area::Webhooks,
            (200, None),
        ),
        (
            "POST",
            format!("/v1/step-runs/sr_{id}/claim"),
            None,
            Area::Actors,
            (401, Some("auth.required")),
        ),
        (
            "GET",
            format!("/v1/executions/exe_{id}"),
            o,
            Area::Executions,
            (599, Some("fake.executions")),
        ),
        (
            "GET",
            format!("/v1/projects/prj_{id}/runbooks"),
            o,
            Area::Authoring,
            (503, Some("storage.unavailable")),
        ),
        (
            "GET",
            "/.well-known/runbook".into(),
            None,
            Area::Directory,
            (200, None),
        ),
        ("GET", "/v1/orgs".into(), o, Area::Directory, (200, None)),
    ];
    let mut covered = std::collections::BTreeSet::new();
    let mut wrong = vec![];
    for (method, path, bearer, expected, answer) in &cases {
        let bearer = match bearer {
            Some("OWNER") => Some(owner.as_str()),
            other => *other,
        };
        let execution = bearer.is_some_and(|b| b.starts_with("trb_exe_"));
        assert_eq!(area(method, path, execution), *expected, "{method} {path}");
        covered.insert(format!("{expected:?}"));
        let r = run(router.handle(request(method, path, bearer, Value::Null)));
        if (r.status, code(&r)) != *answer {
            wrong.push(format!(
                "{method} {path} ({expected:?}): {} {:?}",
                r.status, r.body
            ));
        }
        if *expected != Area::Directory {
            let d = directory_answer(request(method, path, bearer, Value::Null));
            assert!(
                matches!(d.status, 404 | 405),
                "{method} {path}: the directory knows this path"
            );
            if (r.status, code(&r)) == (d.status, code(&d)) {
                wrong.push(format!("{method} {path}: answered like the directory"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    // `/internal/can-register` is reached only through `can_register`.
    assert_eq!(
        covered.len(),
        16,
        "an area has no smoke request: {covered:?}"
    );
}

/// D32: an execution credential goes to its allow-list, except on the
/// paths of the areas dispatched before it.
#[test]
fn execution_bearers_follow_the_dispatch_order() {
    for (method, path, expected) in [
        ("POST", "/v1/device/code", Area::Device),
        (
            "GET",
            "/v1/orgs/org_00000000000000000000000001/invitations",
            Area::Invitations,
        ),
        (
            "GET",
            "/v1/projects/prj_00000000000000000000000001/memory",
            Area::Memory,
        ),
        (
            "POST",
            "/v1/runbooks/rb_00000000000000000000000001/memory",
            Area::Memory,
        ),
        (
            "GET",
            "/v1/memory/mem_00000000000000000000000001",
            Area::Memory,
        ),
        (
            "GET",
            "/v1/runbooks/rb_00000000000000000000000001/archives",
            Area::Archives,
        ),
        (
            "POST",
            "/v1/step-runs/sr_00000000000000000000000001/evidence",
            Area::Evidence,
        ),
        (
            "POST",
            "/v1/step-runs/sr_00000000000000000000000001/evidence/evd_00000000000000000000000001/finalize",
            Area::Evidence,
        ),
        (
            "GET",
            "/v1/executions/exe_00000000000000000000000001/evidence",
            Area::Evidence,
        ),
        (
            "POST",
            "/v1/executions/exe_00000000000000000000000001/tokens",
            Area::ExecutionCredentials,
        ),
        (
            "POST",
            "/v1/executions/exe_00000000000000000000000001/tokens/refresh",
            Area::ExecutionCredentials,
        ),
        (
            "DELETE",
            "/v1/executions/exe_00000000000000000000000001/credentials/fam_00000000000000000000000001",
            Area::ExecutionCredentials,
        ),
        (
            "GET",
            "/v1/executions/exe_00000000000000000000000001",
            Area::ExecutionAccess,
        ),
        (
            "GET",
            "/v1/executions/exe_00000000000000000000000001/next",
            Area::ExecutionAccess,
        ),
        (
            "GET",
            "/v1/step-runs/sr_00000000000000000000000001",
            Area::ExecutionAccess,
        ),
        (
            "GET",
            "/v1/step-runs/sr_00000000000000000000000001/skill-files",
            Area::ExecutionAccess,
        ),
        (
            "POST",
            "/v1/step-runs/sr_00000000000000000000000001/claim",
            Area::ExecutionAccess,
        ),
        (
            "POST",
            "/v1/step-runs/sr_00000000000000000000000001/report",
            Area::ExecutionAccess,
        ),
        ("GET", "/v1/tokens", Area::ExecutionAccess),
        ("GET", "/v1/me", Area::ExecutionAccess),
        (
            "GET",
            "/v1/projects/prj_00000000000000000000000001/webhooks",
            Area::ExecutionAccess,
        ),
        ("GET", "/.well-known/runbook", Area::ExecutionAccess),
    ] {
        assert_eq!(area(method, path, true), expected, "{method} {path}");
    }
}

/// Which area answers a wrong method, and with what.
#[test]
fn wrong_methods_are_answered_by_their_area() {
    let router = router(true);
    for (method, path, expected, status) in [
        ("DELETE", "/v1/me/tasks", Area::MeTasks, 405),
        ("PUT", "/v1/tokens", Area::Tokens, 405),
        ("PUT", "/v1/me", Area::Me, 405),
        (
            "GET",
            "/v1/executions/exe_00000000000000000000000001/tokens",
            Area::ExecutionCredentials,
            405,
        ),
        (
            "GET",
            "/v1/step-runs/sr_00000000000000000000000001/evidence",
            Area::Evidence,
            405,
        ),
        (
            "DELETE",
            "/v1/projects/prj_00000000000000000000000001/memory",
            Area::Memory,
            405,
        ),
        (
            "PUT",
            "/v1/step-runs/sr_00000000000000000000000001",
            Area::StepReads,
            405,
        ),
        (
            "GET",
            "/v1/step-runs/sr_00000000000000000000000001/claim",
            Area::Actors,
            405,
        ),
        (
            "PUT",
            "/v1/projects/prj_00000000000000000000000001/webhooks",
            Area::Webhooks,
            405,
        ),
        (
            "PUT",
            "/v1/projects/prj_00000000000000000000000001/runbooks",
            Area::Authoring,
            405,
        ),
        ("DELETE", "/v1/orgs", Area::Directory, 405),
        // The device routes are POST only; anything else is unknown.
        ("GET", "/v1/device/code", Area::Directory, 404),
    ] {
        assert_eq!(area(method, path, false), expected, "{method} {path}");
        let r = run(router.handle(request(method, path, None, Value::Null)));
        assert_eq!(r.status, status, "{method} {path}: {:?}", r.body);
    }
}
