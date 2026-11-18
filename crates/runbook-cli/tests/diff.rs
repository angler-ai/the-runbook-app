//! CLI3 `runbook diff <file|runbook@v1> [runbook@v2]`: a structured diff of
//! the canonical spec (steps added, removed and changed, with field-level
//! changes), plus a unified text diff with `--text`.
mod support;
use serde_json::{Value, json};
use support::*;

const RB: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ7";
const RBV1: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXY01";
const RBV2: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXY02";

const OLD: &str = "apiVersion: therunbook.app/v1
kind: Runbook
metadata:
  id: post-deploy
  title: Post deploy
spec:
  inputs:
    service: { type: string, required: true }
  steps:
    - id: smoke
      title: Smoke tests
      parallel:
        - id: health
          title: Health check
          instructions: Call /healthz.
        - id: login
          title: Log in
          instructions: Log in as the test user.
    - id: notify
      title: Notify
      instructions: Post in the channel.
";

/// Reformatted (flow style, other key order, a comment): the canonical
/// spec is what is compared, so only real changes show.
const NEW: &str = "# edited locally
apiVersion: therunbook.app/v1
kind: Runbook
metadata: { title: Post-deploy verification, id: post-deploy }
spec:
  inputs:
    service: { required: true, type: string }
    region: { type: string }
  steps:
    - id: smoke
      title: Smoke tests
      parallel:
        - id: health
          title: Health check
          instructions: Call /healthz and record the status code.
        - { id: login, title: Log in, instructions: Log in as the test user. }
        - id: metrics
          title: Collect metrics
          instructions: Export the dashboards.
";

fn version(rbv: &str, n: u32, source: &str) -> Reply {
    reply(
        200,
        json!({
            "runbook_id": RB, "runbook_version_id": rbv, "runbook_version": n,
            "branch": null, "commit_sha": null, "source": source,
            "semantic_hash": "00", "snapshot_digest": "00",
        }),
    )
}

fn path_of(request: &str) -> String {
    request_line(request)
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string()
}

fn expected_structure() -> (Value, Value) {
    let spec = json!([
        {"path": "/metadata/title", "op": "changed", "old": "Post deploy", "new": "Post-deploy verification"},
        {"path": "/spec/inputs/region", "op": "added", "new": {"type": "string"}},
        {"path": "/spec/steps", "op": "changed", "old": ["smoke", "notify"], "new": ["smoke"]},
    ]);
    let steps = json!({
        "added": [{"id": "metrics", "title": "Collect metrics", "path": "/spec/steps/0/parallel/2"}],
        "removed": [{"id": "notify", "title": "Notify", "path": "/spec/steps/1"}],
        "changed": [
            {"id": "health", "title": "Health check", "path": "/spec/steps/0/parallel/0", "changes": [
                {"path": "/instructions", "op": "changed", "old": "Call /healthz.", "new": "Call /healthz and record the status code."},
            ]},
            {"id": "smoke", "title": "Smoke tests", "path": "/spec/steps/0", "changes": [
                {"path": "/parallel", "op": "changed", "old": ["health", "login"], "new": ["health", "login", "metrics"]},
            ]},
        ],
    });
    (spec, steps)
}

#[test]
fn a_local_file_is_compared_with_the_latest_published_version() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("post-deploy.yaml");
    std::fs::write(&file, NEW).unwrap();
    let (host, server) = serve(vec![
        reply(
            200,
            json!({"items": [{"runbook_id": RB, "slug": "post-deploy", "origin": "ui", "archived": false}], "next_cursor": null}),
        ),
        version(RBV1, 3, OLD),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["diff", file.to_str().unwrap(), "--project", "ops", "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    let (spec, steps) = expected_structure();
    assert_eq!(v["identical"], false);
    assert_eq!(v["spec"], spec, "{v:#}");
    assert_eq!(v["steps"], steps, "{v:#}");
    assert_eq!(v["old"]["label"], "post-deploy@3");
    assert_eq!(v["old"]["runbook_version_id"], RBV1);
    assert_eq!(v["new"]["file"], file.to_str().unwrap());
    assert!(v.get("text").is_none());
    let seen = server.join().unwrap();
    assert!(path_of(&seen[0]).starts_with("/v1/projects/ops/runbooks?"));
    assert_eq!(
        path_of(&seen[1]),
        format!("/v1/runbooks/{RB}/versions/latest")
    );
}

#[test]
fn two_published_versions_with_a_unified_text_diff() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![version(RBV1, 1, OLD), version(RBV2, 2, NEW)]);
    let a = format!("{RB}@1");
    let b = format!("{RB}@2");
    let o = cli(
        &host,
        home.path(),
        &["diff", &a, &b, "--text", "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    let (spec, steps) = expected_structure();
    assert_eq!((v["spec"].clone(), v["steps"].clone()), (spec, steps));
    let text = v["text"].as_str().unwrap();
    assert!(
        text.starts_with(&format!("--- {RB}@1\n+++ {RB}@2\n")),
        "{text}"
    );
    assert!(text.contains("-  title: Post deploy\n"), "{text}");
    let seen = server.join().unwrap();
    assert_eq!(path_of(&seen[0]), format!("/v1/runbooks/{RB}/versions/1"));
    assert_eq!(path_of(&seen[1]), format!("/v1/runbooks/{RB}/versions/2"));

    // The human form lists the same changes, then the text diff.
    let (host, server) = serve(vec![version(RBV1, 1, OLD), version(RBV2, 2, NEW)]);
    let o = cli(
        &host,
        home.path(),
        &["diff", &a, &b, "--text"],
        &[("RUNBOOK_TOKEN", PAT), ("CI", "1")],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let human = out(&o);
    for needle in [
        "+ metrics",
        "- notify",
        "~ health",
        "/instructions",
        "/metadata/title",
        "+++ ",
    ] {
        assert!(human.contains(needle), "{needle}\n{human}");
    }
    server.join().unwrap();
}

#[test]
fn one_version_is_compared_with_the_latest() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![version(RBV1, 1, OLD), version(RBV1, 1, OLD)]);
    let a = format!("{RB}@{RBV1}");
    let o = cli(
        &host,
        home.path(),
        &["diff", &a, "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["identical"], true, "{v}");
    assert_eq!(v["spec"], json!([]));
    assert_eq!(
        v["steps"],
        json!({"added": [], "removed": [], "changed": []})
    );
    let seen = server.join().unwrap();
    assert_eq!(
        path_of(&seen[0]),
        format!("/v1/runbooks/{RB}/versions/{RBV1}")
    );
    assert_eq!(
        path_of(&seen[1]),
        format!("/v1/runbooks/{RB}/versions/latest")
    );
}

#[test]
fn bad_arguments_are_usage_errors_before_any_request() {
    let home = tempfile::tempdir().unwrap();
    let broken = home.path().join("broken.yaml");
    std::fs::write(&broken, "apiVersion: [unclosed").unwrap();
    let (host, server) = serve(vec![]);
    let token = [("RUNBOOK_TOKEN", PAT)];
    for args in [
        // A file without --project cannot find its runbook.
        vec!["diff", broken.to_str().unwrap(), "--json"],
        // A slug needs --project.
        vec!["diff", "post-deploy@1", "post-deploy@2", "--json"],
        // Not a version selector.
        vec!["diff", "rb_01JABCDEFGHJKMNPQRSTVWXYZ7@first", "--json"],
        // A missing file is not taken for a runbook name.
        vec!["diff", "./nope.yaml", "--project", "ops", "--json"],
        // Review (low): a directory is refused, not read as a file.
        vec![
            "diff",
            home.path().to_str().unwrap(),
            "--project",
            "ops",
            "--json",
        ],
    ] {
        let o = cli(&host, home.path(), &args, &token);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    let dir = home.path().to_str().unwrap();
    let o = cli(
        &host,
        home.path(),
        &["diff", dir, "--project", "ops", "--json"],
        &token,
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("is a directory"), "{}", err(&o));
    // A file that does not parse.
    let o = cli(
        &host,
        home.path(),
        &[
            "diff",
            broken.to_str().unwrap(),
            "--project",
            "ops",
            "--json",
        ],
        &token,
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn a_file_in_the_credential_store_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let creds = home.path().join("runbook").join("credentials");
    std::fs::create_dir_all(&creds).unwrap();
    let secret = creds.join("logins.json");
    std::fs::write(&secret, "{}").unwrap();
    let (host, server) = serve(vec![]);
    let o = cli(
        &host,
        home.path(),
        &[
            "diff",
            secret.to_str().unwrap(),
            "--project",
            "ops",
            "--json",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(server.join().unwrap().is_empty());
}
