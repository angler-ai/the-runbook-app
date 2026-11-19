//! C3 authoring commands (`list`, `get`, `pull`, `push`, `publish`) against a
//! scripted fake server.
mod support;
use serde_json::{Value, json};
use support::*;

const RB: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ4";
const OTHER: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ5";
const RBV: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXYZ6";
const SOURCE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata:\n  id: post-deploy\n  title: Post deploy\n";

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn page(items: Vec<Value>) -> Value {
    json!({"items": items, "next_cursor": null})
}

fn entry(id: &str, slug: &str) -> Value {
    json!({"runbook_id": id, "slug": slug, "origin": "ui", "archived": false})
}

fn draft(revision: u64, source: &str) -> Reply {
    with_etag(
        200,
        json!({"runbook_id": RB, "source": source, "revision": revision}),
        &format!("\"draft-{RB}-{revision}\""),
    )
}

fn published() -> Value {
    json!({"runbook_id": RB, "runbook_version_id": RBV, "runbook_version": 2})
}

fn spec_file(dir: &std::path::Path, source: &str) -> String {
    let path = dir.join("post-deploy.yaml");
    std::fs::write(&path, source).unwrap();
    path.to_string_lossy().into_owned()
}

fn lines(seen: &[String]) -> Vec<String> {
    seen.iter().map(|r| request_line(r).to_owned()).collect()
}

fn is_write(line: &str) -> bool {
    !line.starts_with("GET ")
}

#[test]
fn push_updates_draft_with_if_match_then_publishes() {
    let home = tmp();
    let file = spec_file(home.path(), SOURCE);
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        problem(428, "request.if_match_required"),
        draft(3, "old"),
        with_etag(
            200,
            json!({"runbook_id": RB, "source": SOURCE, "revision": 4, "diagnostics": []}),
            &format!("\"draft-{RB}-4\""),
        ),
        draft(4, SOURCE),
        reply(201, published()),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["push", &file, "--project", "ops", "--idempotency-key", "k1"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let l = lines(&seen);
    assert!(l[0].starts_with("GET /v1/projects/ops/runbooks?limit=100"));
    // The scope probe sends no If-Match, so it can never publish.
    assert!(l[1].starts_with(&format!("POST /v1/runbooks/{RB}/versions")));
    assert_eq!(header(&seen[1], "If-Match"), None);
    assert!(l[3].starts_with(&format!("PATCH /v1/runbooks/{RB}/draft")));
    assert_eq!(
        header(&seen[3], "If-Match"),
        Some(format!("\"draft-{RB}-3\"").as_str())
    );
    assert_eq!(body_of(&seen[3]), json!({"source": SOURCE}));
    assert_eq!(header(&seen[3], "Idempotency-Key"), Some("k1-draft"));
    assert!(l[5].starts_with(&format!("POST /v1/runbooks/{RB}/versions")));
    assert_eq!(
        header(&seen[5], "If-Match"),
        Some(format!("\"draft-{RB}-4\"").as_str())
    );
    assert_eq!(header(&seen[5], "Idempotency-Key"), Some("k1-publish"));
    assert_eq!(seen.len(), 6);
    assert!(out(&o).contains("published post-deploy version 2"));
}

#[test]
fn push_creates_a_new_runbook_then_publishes() {
    let home = tmp();
    let file = spec_file(home.path(), SOURCE);
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(OTHER, "other")])),
        reply(200, page(vec![entry(OTHER, "other")])),
        problem(428, "request.if_match_required"),
        with_etag(
            201,
            json!({"runbook_id": RB, "source": SOURCE, "revision": 1, "diagnostics": []}),
            &format!("\"draft-{RB}-1\""),
        ),
        draft(1, SOURCE),
        reply(201, published()),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["push", &file, "--project", "ops", "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let l = lines(&seen);
    // Publish permission is confirmed in the project before creating.
    assert!(l[2].starts_with(&format!("POST /v1/runbooks/{OTHER}/versions")));
    assert!(l[3].starts_with("POST /v1/projects/ops/runbooks"));
    assert_eq!(body_of(&seen[3]), json!({"source": SOURCE}));
    assert!(l[5].starts_with(&format!("POST /v1/runbooks/{RB}/versions")));
    let printed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(printed["created"], true);
    assert_eq!(printed["published"]["runbook_version_id"], RBV);
}

#[test]
fn publishing_push_without_publish_scope_fails_before_changing_anything() {
    for existing in [true, false] {
        let home = tmp();
        let file = spec_file(home.path(), SOURCE);
        let mut script = vec![];
        if existing {
            script.push(reply(200, page(vec![entry(RB, "post-deploy")])));
        } else {
            script.push(reply(200, page(vec![entry(OTHER, "other")])));
            script.push(reply(200, page(vec![entry(OTHER, "other")])));
        }
        let mut denied = problem(403, "auth.scope_missing");
        denied.body["next_action"] = json!("ask_human_to_publish");
        script.push(denied);
        let (host, server) = serve(script);
        let o = cli(
            &host,
            home.path(),
            &["push", &file, "--project", "ops", "--json"],
            &[("RUNBOOK_TOKEN", PAT)],
        );
        let seen = server.join().unwrap();
        assert_eq!(o.status.code(), Some(1), "{}", err(&o));
        assert!(err(&o).contains("auth.scope_missing"), "{}", err(&o));
        assert!(err(&o).contains("--draft"), "{}", err(&o));
        let printed: Value = serde_json::from_str(&out(&o)).unwrap();
        assert_eq!(printed["error"]["status"], 403);
        assert_eq!(printed["next_action"], "ask_human_to_publish");
        // Only reads and the precondition-free probe: no draft, no version.
        let l = lines(&seen);
        let writes: Vec<_> = l.iter().filter(|l| is_write(l)).collect();
        assert_eq!(writes.len(), 1, "{l:?}");
        assert!(writes[0].contains("/versions"));
        assert_eq!(header(seen.last().unwrap(), "If-Match"), None);
    }
}

#[test]
fn draft_push_saves_with_if_match_and_never_publishes() {
    let home = tmp();
    let file = spec_file(home.path(), SOURCE);
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        draft(3, "old"),
        with_etag(
            200,
            json!({"runbook_id": RB, "source": SOURCE, "revision": 4, "diagnostics": []}),
            &format!("\"draft-{RB}-4\""),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &[
            "push",
            &file,
            "--project",
            "ops",
            "--draft",
            "--revision",
            "3",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let l = lines(&seen);
    assert_eq!(l.len(), 3, "{l:?}");
    assert!(l[2].starts_with(&format!("PATCH /v1/runbooks/{RB}/draft")));
    assert_eq!(
        header(&seen[2], "If-Match"),
        Some(format!("\"draft-{RB}-3\"").as_str())
    );
    assert!(!l.iter().any(|l| l.contains("/versions")));
    assert!(out(&o).contains("draft revision 4"));
}

#[test]
fn push_with_a_stale_revision_conflicts_without_writing() {
    let home = tmp();
    let file = spec_file(home.path(), SOURCE);
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        draft(5, "someone else's edit"),
    ]);
    let o = cli(
        &host,
        home.path(),
        &[
            "push",
            &file,
            "--project",
            "ops",
            "--draft",
            "--revision",
            "3",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("catalog.draft_conflict"), "{}", err(&o));
    assert!(!lines(&seen).iter().any(|l| is_write(l)));
}

#[test]
fn push_with_draft_problems_saves_but_does_not_publish() {
    let home = tmp();
    let file = spec_file(home.path(), SOURCE);
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        problem(428, "request.if_match_required"),
        draft(1, "old"),
        with_etag(
            200,
            json!({"runbook_id": RB, "source": SOURCE, "revision": 2,
                   "diagnostics": [{"path": "/steps", "code": "spec.required", "message": "steps is required"}]}),
            &format!("\"draft-{RB}-2\""),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["push", &file, "--project", "ops"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("spec.required"));
    assert_eq!(seen.len(), 4);
}

#[test]
fn push_into_an_empty_project_cannot_confirm_scope_and_changes_nothing() {
    let home = tmp();
    let file = spec_file(home.path(), SOURCE);
    let (host, server) = serve(vec![reply(200, page(vec![])), reply(200, page(vec![]))]);
    let o = cli(
        &host,
        home.path(),
        &["push", &file, "--project", "ops"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("--draft"));
    assert!(!lines(&seen).iter().any(|l| is_write(l)));
}

#[test]
fn draft_push_creates_without_a_scope_probe() {
    let home = tmp();
    let file = spec_file(home.path(), SOURCE);
    let (host, server) = serve(vec![
        reply(200, page(vec![])),
        with_etag(
            201,
            json!({"runbook_id": RB, "source": SOURCE, "revision": 1, "diagnostics": []}),
            &format!("\"draft-{RB}-1\""),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["push", &file, "--project", "ops", "--draft"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(lines(&seen).len(), 2);
    assert!(out(&o).contains("created post-deploy"));
}

#[test]
fn publish_without_scope_asks_a_human() {
    let home = tmp();
    let (host, server) = serve(vec![draft(4, SOURCE), problem(403, "auth.scope_missing")]);
    let o = cli(
        &host,
        home.path(),
        &["publish", RB, "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        header(&seen[1], "If-Match"),
        Some(format!("\"draft-{RB}-4\"").as_str())
    );
    let printed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(printed["next_action"], "ask_human_to_publish");
}

#[test]
fn publish_by_slug_publishes_the_current_draft() {
    let home = tmp();
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        draft(4, SOURCE),
        reply(201, published()),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["publish", "post-deploy", "--project", "ops"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(body_of(&seen[2]), json!({}));
    assert!(out(&o).contains("published draft revision 4"));
}

#[test]
fn pull_writes_the_draft_source_to_a_file() {
    let home = tmp();
    let target = home.path().join("out.yaml");
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        draft(7, SOURCE),
    ]);
    let o = cli(
        &host,
        home.path(),
        &[
            "pull",
            "post-deploy",
            "--project",
            "ops",
            "-o",
            target.to_str().unwrap(),
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), SOURCE);
    assert!(request_line(&seen[1]).starts_with(&format!("GET /v1/runbooks/{RB}/draft")));
    assert!(err(&o).contains("draft revision 7"));
}

#[test]
fn a_slug_without_a_project_is_a_usage_error() {
    let home = tmp();
    let (host, server) = serve(vec![]);
    let o = cli(
        &host,
        home.path(),
        &["get", "post-deploy"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    server.join().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn list_and_get_show_the_catalog() {
    let home = tmp();
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        reply(200, entry(RB, "post-deploy")),
        reply(
            200,
            json!({"runbook_id": RB, "runbook_version_id": RBV, "runbook_version": 2, "source": "secret-ish"}),
        ),
        reply(
            200,
            json!({
                "inputs": {"type": "object", "properties": {"service": {"type": "string"}}},
                "fields": {"type": "object"},
                "steps": [{"id": "smoke", "title": "Smoke tests", "kind": "task", "steps": []}],
                "finally": [],
            }),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["list", "--project", "ops"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(&format!("{RB}  post-deploy  ui")));
    let o = cli(
        &host,
        home.path(),
        &["get", RB, "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let printed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(printed["latest_version"]["runbook_version"], 2);
    assert!(!out(&o).contains("secret-ish"));
    assert!(request_line(&seen[2]).starts_with(&format!("GET /v1/runbooks/{RB}/versions/latest")));
    // CLI3: the input schema and step outline of that version.
    assert!(
        request_line(&seen[3]).starts_with(&format!("GET /v1/runbooks/{RB}/versions/{RBV}/form"))
    );
    assert_eq!(
        printed["input_schema"]["properties"]["service"]["type"],
        "string"
    );
    assert_eq!(
        printed["outline"],
        json!({"steps": [{"id": "smoke", "title": "Smoke tests", "kind": "task", "steps": []}], "finally": []})
    );
}

const TWO_STEPS: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata:\n  id: post-deploy\n  title: Post deploy\nspec:\n  steps:\n    - id: check\n      title: Check\n    - id: again\n      title: Again\n";

#[test]
fn push_warns_when_the_worst_case_is_over_the_servers_step_cap() {
    let home = tmp();
    let file = spec_file(home.path(), TWO_STEPS);
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        problem(428, "request.if_match_required"),
        draft(3, "old"),
        with_etag(
            200,
            json!({"runbook_id": RB, "source": TWO_STEPS, "revision": 4, "diagnostics": []}),
            &format!("\"draft-{RB}-4\""),
        ),
        draft(4, TWO_STEPS),
        reply(201, published()),
        reply(
            200,
            json!({"edition": "self_hosted", "limits": {"step_runs": 1}}),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["push", &file, "--project", "ops", "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(lines(&seen).last().unwrap().starts_with("GET /v1/meta"));
    assert!(
        err(&o).contains("warning: worst case 2 step runs is over this server's limit of 1"),
        "{}",
        err(&o)
    );
    let out: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        out["step_cap_warning"]
            .as_str()
            .unwrap()
            .contains("limit of 1")
    );
}

#[test]
fn push_under_the_cap_says_nothing() {
    let home = tmp();
    let file = spec_file(home.path(), TWO_STEPS);
    let (host, server) = serve(vec![
        reply(200, page(vec![entry(RB, "post-deploy")])),
        problem(428, "request.if_match_required"),
        draft(4, TWO_STEPS),
        draft(4, TWO_STEPS),
        reply(201, published()),
        reply(
            200,
            json!({"edition": "self_hosted", "limits": {"step_runs": 5}}),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["push", &file, "--project", "ops"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!err(&o).contains("warning"), "{}", err(&o));
}
