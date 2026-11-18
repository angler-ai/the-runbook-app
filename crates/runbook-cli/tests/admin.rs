//! C2 pilot administration commands against the directory routes.
mod fake_api;
use fake_api::*;
use serde_json::json;

#[test]
fn user_create_resolves_the_single_org_and_posts_with_an_idempotency_key() {
    let home = tempfile::tempdir().unwrap();
    let user = json!({"id":USR,"name":"Alice","active":true,"role":"member"});
    let (host, server) = serve(vec![orgs(), Reply::json(201, user.clone())]);
    let o = cli(
        &host,
        home.path(),
        &["user", "create", "--name", "Alice", "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&o)).unwrap(),
        user
    );
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "GET /v1/orgs");
    assert_eq!(line(&seen[1]), format!("POST /v1/orgs/{ORG}/users"));
    assert_eq!(body_of(&seen[1]), json!({"name":"Alice"}));
    assert_eq!(
        header(&seen[1], "authorization"),
        Some(format!("Bearer {PAT}").as_str())
    );
    assert!(header(&seen[1], "idempotency-key").is_some_and(|k| !k.is_empty()));
}

#[test]
fn explicit_org_and_idempotency_key_skip_the_lookup() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![Reply::json(
        201,
        json!({"id":USR,"name":"Bob","active":true,"role":"member"}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &[
            "user",
            "create",
            "--name",
            "Bob",
            "--org",
            ORG,
            "--idempotency-key",
            "key-1",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(USR), "{}", out(&o));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(line(&seen[0]), format!("POST /v1/orgs/{ORG}/users"));
    assert_eq!(header(&seen[0], "idempotency-key"), Some("key-1"));
}

#[test]
fn several_orgs_without_org_flag_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let other = "org_01JABCDEFGHJKMNPQRSTVWXYZ9";
    let (host, server) = serve(vec![Reply::json(
        200,
        json!({"items":[{"id":ORG,"title":"A"},{"id":other,"title":"B"}],"next_cursor":null}),
    )]);
    let o = cli(
        &host,
        home.path(),
        &["team", "create", "--slug", "sre", "--title", "SRE"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--org"), "{}", err(&o));
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn user_deactivate_patches_active_false() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(
            200,
            json!({"id":USR,"name":"Alice","active":false,"role":"member"}),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["user", "deactivate", USR],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(
        out(&o).contains(&format!("deactivated {USR}")),
        "{}",
        out(&o)
    );
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("PATCH /v1/orgs/{ORG}/users/{USR}"));
    assert_eq!(body_of(&seen[1]), json!({"active":false}));
}

#[test]
fn invalid_ids_are_usage_errors_before_any_request() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);
    for args in [
        vec!["user", "deactivate", "alice"],
        vec!["project", "member", "add", PRJ, "alice"],
        vec!["project", "member", "add", PRJ, USR, "--role", "owner"],
        vec!["team", "member", "remove", TEAM, "bob"],
    ] {
        let o = cli(&host, home.path(), &args, &token(), None);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", err(&o));
    }
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn project_create_posts_slug_and_title() {
    let home = tempfile::tempdir().unwrap();
    let project = json!({"id":PRJ,"org_id":ORG,"slug":"ops","title":"Ops"});
    let (host, server) = serve(vec![orgs(), Reply::json(201, project.clone())]);
    let o = cli(
        &host,
        home.path(),
        &["project", "create", "--slug", "ops", "--title", "Ops"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(PRJ));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("POST /v1/orgs/{ORG}/projects"));
    assert_eq!(body_of(&seen[1]), json!({"slug":"ops","title":"Ops"}));
}

#[test]
fn project_member_add_defaults_to_viewer_and_takes_a_role() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        Reply::json(200, json!({"user_id":USR,"role":"viewer"})),
        Reply::json(200, json!({"user_id":USR,"role":"editor"})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["project", "member", "add", PRJ, USR],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &[
            "project", "member", "add", PRJ, USR, "--role", "editor", "--json",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&o)).unwrap(),
        json!({"user_id":USR,"role":"editor"})
    );
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("POST /v1/projects/{PRJ}/members"));
    assert_eq!(body_of(&seen[0]), json!({"user_id":USR,"role":"viewer"}));
    assert_eq!(body_of(&seen[1]), json!({"user_id":USR,"role":"editor"}));
    assert!(header(&seen[0], "idempotency-key").is_some());
}

#[test]
fn project_slug_is_resolved_through_the_project_list() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(
            200,
            json!({"items":[{"id":PRJ,"org_id":ORG,"slug":"ops","title":"Ops"}],"next_cursor":null}),
        ),
        Reply::empty(204),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["project", "member", "remove", "ops", USR],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("removed"), "{}", out(&o));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("GET /v1/orgs/{ORG}/projects"));
    assert_eq!(
        line(&seen[2]),
        format!("DELETE /v1/projects/{PRJ}/members/{USR}")
    );
}

#[test]
fn unknown_project_slug_fails_without_mutating() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(200, json!({"items":[],"next_cursor":null})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["project", "member", "add", "nope", USR],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("nope"), "{}", err(&o));
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn team_create_and_membership() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        orgs(),
        Reply::json(
            201,
            json!({"id":TEAM,"org_id":ORG,"slug":"sre","title":"SRE"}),
        ),
        orgs(),
        Reply::json(200, json!({"user_id":USR})),
        orgs(),
        Reply::empty(204),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["team", "create", "--slug", "sre", "--title", "SRE"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(TEAM));
    let o = cli(
        &host,
        home.path(),
        &["team", "member", "add", TEAM, USR],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = cli(
        &host,
        home.path(),
        &["team", "member", "remove", TEAM, USR, "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&o)).unwrap(),
        json!({"removed":true,"team_id":TEAM,"user_id":USR})
    );
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("POST /v1/orgs/{ORG}/teams"));
    assert_eq!(body_of(&seen[1]), json!({"slug":"sre","title":"SRE"}));
    assert_eq!(
        line(&seen[3]),
        format!("POST /v1/orgs/{ORG}/teams/{TEAM}/members")
    );
    assert_eq!(body_of(&seen[3]), json!({"user_id":USR}));
    assert_eq!(
        line(&seen[5]),
        format!("DELETE /v1/orgs/{ORG}/teams/{TEAM}/members/{USR}")
    );
}

#[test]
fn problems_map_to_stable_exit_codes() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = serve(vec![
        Reply::json(403, json!({"code":"auth.forbidden"})),
        Reply::json(429, json!({"code":"rate.limited"})),
    ]);
    let args = ["user", "create", "--name", "A", "--org", ORG];
    let o = cli(&host, home.path(), &args, &token(), None);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("auth.forbidden"), "{}", err(&o));
    let o = cli(&host, home.path(), &args, &token(), None);
    assert_eq!(o.status.code(), Some(4));
}

#[test]
fn admin_commands_need_credentials() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);
    let o = cli(
        &host,
        home.path(),
        &["user", "create", "--name", "A"],
        &[],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("runbook login"), "{}", err(&o));
    assert!(server.join().unwrap().is_empty());
}
