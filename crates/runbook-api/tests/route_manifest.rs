//! CF0: the route manifest (`runbook_api::routes::MANIFEST`) against the
//! router, the route tables of every module and the contract.
use runbook_api::{
    router::{Area, area},
    routes::{Host, Kind, MANIFEST, Route, Served},
};
use runbook_model::PublicRoute;
use std::collections::BTreeSet;

/// `{name}` and `{*rest}` become one placeholder, so `/x/{p}` and `/x/{project}`
/// are the same route.
fn normalize(path: &str) -> String {
    path.split('/')
        .map(|s| if s.starts_with('{') { "{}" } else { s })
        .collect::<Vec<_>>()
        .join("/")
}

/// A concrete path the pattern matches.
fn sample(path: &str) -> String {
    path.split('/')
        .map(|s| if s.starts_with('{') { "x1" } else { s })
        .collect::<Vec<_>>()
        .join("/")
}

fn key(method: &str, path: &str) -> (String, String) {
    (method.to_ascii_uppercase(), normalize(path))
}

fn manifest_keys(filter: impl Fn(&Route) -> bool) -> BTreeSet<(String, String)> {
    MANIFEST
        .iter()
        .filter(|r| filter(r))
        .map(|r| key(r.method, r.path))
        .collect()
}

#[test]
fn the_manifest_lists_each_route_once() {
    let mut seen = BTreeSet::new();
    for route in MANIFEST {
        assert!(
            seen.insert(key(route.method, route.path)),
            "{} {} is listed twice",
            route.method,
            route.path
        );
    }
}

/// Every router route reaches the area the manifest names, in the router's
/// own dispatch order. The directory service is also the 404 of unknown
/// paths, so its routes must be in its own table too.
#[test]
fn the_router_dispatches_every_route_to_its_area() {
    for route in MANIFEST {
        let Served::Router(expected) = route.served else {
            continue;
        };
        let got = area(route.method, &sample(route.path), false);
        assert_eq!(got, expected, "{} {}", route.method, route.path);
        if expected == Area::Directory {
            assert!(
                runbook_model::DIRECTORY_ROUTES
                    .iter()
                    .any(|r| r.method == route.method && normalize(r.path) == normalize(route.path)),
                "{} {} falls through to the directory, which does not serve it",
                route.method,
                route.path
            );
        }
    }
}

/// The routes hosts answer are the ones their transports intercept.
#[test]
fn host_routes_are_the_transports() {
    for route in MANIFEST {
        let Served::Host(host) = route.served else {
            continue;
        };
        match host {
            Host::EventStream => assert!(runbook_model::STREAM_ROUTES.iter().any(|r| key(
                r.method, r.path
            ) == key(
                route.method,
                route.path
            ))),
            Host::Mcp => assert_eq!(route.path, runbook_api::mcp::PATH),
            Host::AuthProxy => assert!(
                route.path.starts_with("/api/auth/") || runbook_api::oauth::proxied(route.path)
            ),
            Host::UserContent => assert!(
                route
                    .path
                    .starts_with(runbook_core::uploads::signing::UPLOAD_PATH_PREFIX)
            ),
        }
        assert!(
            matches!(route.kind, Kind::Public | Kind::Transport),
            "{route:?}"
        );
    }
}

/// Every route a module's own table serves is in the manifest.
#[test]
fn every_module_route_is_in_the_manifest() {
    let all = manifest_keys(|_| true);
    let tables: &[&[PublicRoute]] = &[
        runbook_model::DIRECTORY_ROUTES,
        runbook_model::TOKEN_ROUTES,
        runbook_model::MEMORY_ROUTES,
        runbook_model::STEP_LOG_ROUTES,
        runbook_model::INVITATION_ROUTES,
        runbook_model::DEVICE_ROUTES,
        runbook_model::EXECUTION_ROUTES,
        runbook_model::STREAM_ROUTES,
        runbook_model::WEBHOOK_ROUTES,
        runbook_model::AUTHORING_ROUTES,
        runbook_model::EVIDENCE_ROUTES,
        runbook_model::INBOX_ROUTES,
        runbook_model::ARCHIVE_ROUTES,
        runbook_model::ME_ROUTES,
        runbook_model::RUNNER_ROUTES,
    ];
    let mut wanted: Vec<(String, String)> = tables
        .iter()
        .flat_map(|t| t.iter())
        .map(|r| key(r.method, r.path))
        .collect();
    wanted.extend(runbook_api::actors::served_routes().map(|(method, path, _)| key(method, path)));
    wanted.extend(
        runbook_api::execution_credentials::ROUTES
            .iter()
            .map(|(method, path)| key(method, path)),
    );
    for route in wanted {
        assert!(
            all.contains(&route),
            "{route:?} is served but not in the manifest"
        );
    }
}

/// Both ways against `api/openapi.draft.json`: every implemented contract
/// operation is routed, and every routed operation is in the contract or
/// marked internal, deprecated or transport.
#[test]
fn the_manifest_and_the_contract_agree() {
    let contract = runbook_contract::openapi();
    let mut implemented = BTreeSet::new();
    for (path, ops) in contract["paths"].as_object().unwrap() {
        for (method, op) in ops.as_object().unwrap() {
            if !matches!(
                method.as_str(),
                "get" | "post" | "put" | "patch" | "delete" | "head" | "options"
            ) {
                continue;
            }
            if op["x-implementation"] == "planned" {
                // Planned operations answer 501 before the router (later
                // plans rule 5), on every host.
                assert!(
                    runbook_api::planned::recognizes(method, &sample(path)),
                    "{method} {path} is planned in the contract but not 501"
                );
                continue;
            }
            implemented.insert(key(method, path));
        }
    }
    let public = manifest_keys(|r| r.kind == Kind::Public);
    let missing: Vec<_> = implemented.difference(&public).collect();
    assert!(
        missing.is_empty(),
        "in the contract but not routed: {missing:?}"
    );
    let extra: Vec<_> = public.difference(&implemented).collect();
    assert!(
        extra.is_empty(),
        "routed as public but not in the contract: {extra:?}"
    );
    for route in MANIFEST {
        match route.kind {
            Kind::Public => {}
            Kind::Internal => assert!(route.path.starts_with("/internal/"), "{route:?}"),
            Kind::Deprecated => assert!(
                runbook_api::actors::DEPRECATED_ROUTES
                    .iter()
                    .any(|(m, p, _)| key(m, p) == key(route.method, route.path)),
                "{route:?}"
            ),
            Kind::Transport => assert!(matches!(route.served, Served::Host(_)), "{route:?}"),
        }
    }
}

/// MCP2: the OAuth discovery documents are routes too, proxied to the
/// auth service.
#[test]
fn oauth_discovery_routes_are_listed() {
    for path in runbook_api::oauth::PROXIED_PATHS {
        assert!(
            MANIFEST.iter().any(|r| r.method == "GET"
                && r.path == path
                && r.served == Served::Host(Host::AuthProxy)
                && r.kind == Kind::Transport),
            "{path}"
        );
    }
}
