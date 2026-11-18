//! SEARCH2: the universal search answers only what the caller can read.
#[path = "support/fake_backends.rs"]
mod fake_backends;
use fake_backends::{request, router, run};
use serde_json::json;

#[test]
fn universal_search_is_authenticated_and_empty_without_a_query() {
    let fake = router(true);
    let token = run(fake.backends.seed());
    let router = fake;

    let anon = run(router.handle(request(
        "GET",
        "/v1/orgs/org_00000000000000000000000001/search?q=x",
        None,
        json!(null),
    )));
    assert_ne!(anon.status, 200, "{}", anon.body);

    let empty = run(router.handle(request(
        "GET",
        "/v1/orgs/org_00000000000000000000000001/search",
        Some(&token),
        json!(null),
    )));
    assert_eq!(empty.status, 200, "{}", empty.body);
    assert_eq!(empty.body["runbooks"], json!([]));
    assert_eq!(empty.body["runs"], json!([]));

    // A foreign org in the path is unknown to the caller.
    let foreign = run(router.handle(request(
        "GET",
        "/v1/orgs/org_00000000000000000000000009/search?q=run",
        Some(&token),
        json!(null),
    )));
    assert_eq!(foreign.status, 404, "{}", foreign.body);
}
