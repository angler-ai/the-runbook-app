//! R0-5: the runner proxy's allow-list (`src/runner_routes.json`) is the
//! runner's public route list, checked in; it must not drift from the
//! runner's own copy.
use runbook_api::runner_proxy::{route_for, routes};
use std::collections::BTreeSet;

#[test]
fn runner_routes_are_well_formed() {
    let list = routes();
    assert_eq!(list.prefix, "/runner/v1");
    let mut seen = BTreeSet::new();
    for route in &list.routes {
        assert!(
            matches!(
                route.method.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE"
            ),
            "{route:?}"
        );
        assert!(route.path.starts_with(&list.prefix), "{route:?}");
        assert!(seen.insert((&route.method, &route.path)), "{route:?} twice");
    }
    assert!(route_for("GET", "/runner/v1/groups").is_some());
    assert!(route_for("POST", "/runner/v1/credentials/cred_1/secrets/token").is_some());
    assert!(route_for("GET", "/runner/v1/transcripts/sr_1").is_some_and(|r| r.stream));
    assert!(route_for("DELETE", "/runner/v1/policy").is_none());
    assert!(route_for("GET", "/runner/v1/groups/../credentials").is_none());
    assert!(route_for("GET", "/internal/seal").is_none());
}

/// Every path parameter is one plain segment: `[A-Za-z0-9_.:-]+`, never
/// `.` or `..`, and never percent-encoded (`%2e`, `%2f`, `%5c` in any
/// case, or anything else).
#[test]
fn runner_route_parameters_are_plain_segments() {
    for ok in [
        "/runner/v1/groups/rg_01",
        "/runner/v1/groups/a.b:c-d_e",
        "/runner/v1/projects/prj_1/agents/agent.v2",
    ] {
        assert!(route_for("GET", ok).is_some(), "{ok}");
    }
    for bad in [
        "/runner/v1/groups/%2e%2e",
        "/runner/v1/groups/%2E%2E",
        "/runner/v1/groups/.",
        "/runner/v1/groups/..",
        "/runner/v1/groups/%2F",
        "/runner/v1/groups/a%2fb",
        "/runner/v1/groups/a%5Cb",
        "/runner/v1/groups/a%5cb",
        "/runner/v1/groups/.%2e",
        "/runner/v1/groups/%41",
        "/runner/v1/groups/a b",
        "/runner/v1/groups/a\\b",
        "/runner/v1/projects/../agents/x",
        "/runner/v1/projects/prj_1/agents/%2e",
    ] {
        assert!(route_for("GET", bad).is_none(), "{bad}");
    }
}

/// The runner's copy, when this checkout has the runner
/// (`runner/packages/shared/src/contracts/public-routes.json`).
#[test]
fn runner_routes_match_the_runner_contract() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../runner/packages/shared/src/contracts/public-routes.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "skipped: {} is missing (the runner is not in this checkout); \
             runner_routes.json cannot drift from it yet",
            path.display()
        );
        return;
    };
    let theirs: serde_json::Value = serde_json::from_str(&text).unwrap();
    let ours: serde_json::Value =
        serde_json::from_str(runbook_api::runner_proxy::ROUTES_JSON).unwrap();
    assert_eq!(
        ours, theirs,
        "crates/runbook-api/src/runner_routes.json drifted from the runner's public-routes.json; copy the runner's file over it"
    );
}
