//! `runbook sync push` / `pull` (2026-11-17) against a scripted fake server.
mod support;
use serde_json::json;
use support::*;

const ALPHA: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: alpha, title: A}\nspec:\n  steps:\n    - {id: w, title: W}\n";

fn folder(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, source) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    dir
}

#[test]
fn push_sends_the_folder_with_its_commit_and_prints_each_action() {
    let home = tempfile::tempdir().unwrap();
    let dir = folder(&[
        ("alpha.yaml", ALPHA),
        ("notes.md", "not a runbook"),
        ("beta.yml", ALPHA),
    ]);
    let (host, server) = serve(vec![reply(
        200,
        json!({"dry_run": false, "commit_sha": "abc", "branch": "main", "runbooks": [
            {"path": "x/alpha.yaml", "slug": "alpha", "runbook_id": "rb_01JABCDEFGHJKMNPQRSTVWXYZ4", "action": "create", "runbook_version": 1, "runbook_version_id": "rbv_01JABCDEFGHJKMNPQRSTVWXYZ6"},
            {"slug": "old", "runbook_id": "rb_01JABCDEFGHJKMNPQRSTVWXYZ5", "action": "archive"}
        ]}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &[
            "sync",
            "push",
            dir.path().to_str().unwrap(),
            "--project",
            "ops",
            "--commit",
            "abc",
            "--branch",
            "main",
            "--prune",
            "--idempotency-key",
            "k",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(request_line(&seen[0]).starts_with("POST /v1/projects/ops/sync"));
    assert_eq!(header(&seen[0], "Idempotency-Key"), Some("k-ops"));
    let body = body_of(&seen[0]);
    assert_eq!(body["commit_sha"], "abc");
    assert_eq!(body["branch"], "main");
    assert_eq!(body["prune"], true);
    assert_eq!(body["dry_run"], false);
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "only .yaml and .yml files: {body}");
    assert!(files[0]["path"].as_str().unwrap().ends_with("alpha.yaml"));
    assert_eq!(files[0]["source"], ALPHA);
    let text = out(&o);
    assert!(text.contains("ops: 2 runbooks"), "{text}");
    assert!(text.contains("create    alpha v1"), "{text}");
    assert!(text.contains("archive   old"), "{text}");
}

#[test]
fn a_refused_batch_lists_each_files_errors_and_exits_2() {
    let home = tempfile::tempdir().unwrap();
    let dir = folder(&[("alpha.yaml", ALPHA)]);
    let (host, server) = serve(vec![reply(
        422,
        json!({"code": "sync.invalid", "status": 422, "files": [
            {"path": "runbooks/alpha.yaml", "errors": [
                {"path": "/spec/steps/0/type", "code": "spec.unknown_key", "message": "unknown key", "line": 7, "column": 7}
            ]}
        ]}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &[
            "sync",
            "push",
            dir.path().to_str().unwrap(),
            "--project",
            "ops",
            "--dry-run",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(body_of(&seen[0])["dry_run"], true);
    let e = err(&o);
    assert!(e.contains("nothing was published"), "{e}");
    assert!(
        e.contains("runbooks/alpha.yaml:7:7: spec.unknown_key /spec/steps/0/type unknown key"),
        "{e}"
    );
}

#[test]
fn each_project_pushes_every_project_folder() {
    let home = tempfile::tempdir().unwrap();
    let dir = folder(&[
        ("app/alpha.yaml", ALPHA),
        ("data/alpha.yaml", ALPHA),
        ("README.md", "x"),
    ]);
    let ok = || reply(200, json!({"dry_run": true, "runbooks": []}));
    let (host, server) = serve(vec![ok(), ok()]);
    let o = cli(
        &host,
        home.path(),
        &[
            "sync",
            "push",
            dir.path().to_str().unwrap(),
            "--each-project",
            "--dry-run",
        ],
        &[
            ("RUNBOOK_TOKEN", PAT),
            ("RUNBOOK_PROJECT", "ignored"),
            ("RUNBOOK_TOKEN_APP", "trb_svc_app"),
        ],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(request_line(&seen[0]).starts_with("POST /v1/projects/app/sync"));
    assert!(request_line(&seen[1]).starts_with("POST /v1/projects/data/sync"));
    // Each project's own token when set (a service token is per project).
    assert_eq!(
        header(&seen[0], "Authorization"),
        Some("Bearer trb_svc_app")
    );
    assert_eq!(
        header(&seen[1], "Authorization"),
        Some(&*format!("Bearer {PAT}"))
    );
}

#[test]
fn pull_writes_each_published_runbook() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("ops");
    let (host, server) = serve(vec![
        reply(
            200,
            json!({"items": [
                {"runbook_id": "rb_01JABCDEFGHJKMNPQRSTVWXYZ4", "slug": "alpha", "origin": "ui", "archived": false, "latest_version": 3},
                {"runbook_id": "rb_01JABCDEFGHJKMNPQRSTVWXYZ5", "slug": "draft-only", "origin": "ui", "archived": false},
                {"runbook_id": "rb_01JABCDEFGHJKMNPQRSTVWXYZ6", "slug": "gone", "origin": "git", "archived": true, "latest_version": 1}
            ], "next_cursor": null}),
        ),
        reply(200, json!({"source": ALPHA, "runbook_version": 3})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["sync", "pull", target.to_str().unwrap(), "--project", "ops"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    let seen = server.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        request_line(&seen[1])
            .starts_with("GET /v1/runbooks/rb_01JABCDEFGHJKMNPQRSTVWXYZ4/versions/3")
    );
    assert_eq!(
        std::fs::read_to_string(target.join("alpha.yaml")).unwrap(),
        ALPHA
    );
    assert!(!target.join("gone.yaml").exists());
    assert!(
        err(&o).contains("skipped ops/draft-only: never published"),
        "{}",
        err(&o)
    );
}
