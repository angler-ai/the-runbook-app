//! MEM4: CLI memory commands tests against the fake server pattern.
mod fake_api;
use fake_api::*;
use serde_json::{Value, json};
use std::fs;

const MEM: &str = "mem_01JABCDEFGHJKMNPQRSTVWXYZ9";
const EXE_TOKEN: &str = "trb_exe_caller-execution-secret-padded-to-43c";

fn memory_entry(id: &str, status: &str, title: &str, rev: u32) -> Value {
    json!({
        "id": id,
        "scope": {"type": "runbook", "runbook_id": RB},
        "step_id": "step_1",
        "title": title,
        "body_markdown": "Test body",
        "tags": ["prod"],
        "status": status,
        "source": {"type": "human", "user_id": USR},
        "revision": rev,
        "expires_at": null,
        "created_at": 1000,
        "updated_at": 1000,
    })
}

fn page(items: Vec<Value>) -> Value {
    json!({
        "items": items,
        "next_cursor": null,
    })
}

#[test]
fn search_sends_path_query_and_formats_output() {
    let home = tempfile::tempdir().unwrap();
    let entry = memory_entry(MEM, "active", "Baseline Latency", 1);
    let (host, server) = serve(vec![
        Reply::json(200, page(vec![entry.clone()])),
        Reply::json(200, page(vec![entry.clone()])),
    ]);

    // Human output
    let o = cli(
        &host,
        home.path(),
        &[
            "memory", "search", RB, "baseline", "--step", "step_1", "--tag", "prod",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));
    assert!(out(&o).contains("active"));
    assert!(out(&o).contains("Baseline Latency"));

    // JSON output
    let o = cli(
        &host,
        home.path(),
        &["memory", "search", RB, "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["items"][0]["id"], MEM);

    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/runbooks/{RB}/memory?status=active&q=baseline&step_id=step_1&tags=prod")
    );
    assert_eq!(
        line(&seen[1]),
        format!("GET /v1/runbooks/{RB}/memory?status=active")
    );
}

#[test]
fn save_sends_body_and_idempotency_key() {
    let home = tempfile::tempdir().unwrap();
    let file_path = home.path().join("body.md");
    fs::write(&file_path, "Durable fact body").unwrap();

    let entry = memory_entry(MEM, "active", "Cache Size", 1);
    let (host, server) = serve(vec![
        Reply::json(201, entry.clone()),
        Reply::json(201, entry.clone()),
    ]);

    // Human output
    let o = cli(
        &host,
        home.path(),
        &[
            "memory",
            "save",
            RB,
            "--title",
            "Cache Size",
            "--body-file",
            file_path.to_str().unwrap(),
            "--step",
            "step_1",
            "--tag",
            "cache",
            "--idempotency-key",
            "k1",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));
    assert!(out(&o).contains("active"));
    assert!(out(&o).contains("Cache Size"));

    // JSON output
    let o = cli(
        &host,
        home.path(),
        &[
            "memory",
            "save",
            "--runbook",
            RB,
            "--title",
            "Cache Size",
            "--body-file",
            file_path.to_str().unwrap(),
            "--json",
            "--idempotency-key",
            "k2",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["id"], MEM);

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("POST /v1/runbooks/{RB}/memory"));
    assert_eq!(header(&seen[0], "idempotency-key"), Some("k1"));
    assert_eq!(
        body_of(&seen[0]),
        json!({
            "title": "Cache Size",
            "body_markdown": "Durable fact body",
            "tags": ["cache"],
            "step_id": "step_1"
        })
    );

    assert_eq!(line(&seen[1]), format!("POST /v1/runbooks/{RB}/memory"));
    assert_eq!(header(&seen[1], "idempotency-key"), Some("k2"));
    assert_eq!(
        body_of(&seen[1]),
        json!({
            "title": "Cache Size",
            "body_markdown": "Durable fact body",
            "tags": []
        })
    );
}

#[test]
fn save_from_stdin() {
    let home = tempfile::tempdir().unwrap();
    let entry = memory_entry(MEM, "active", "Stdin Title", 1);
    let (host, server) = serve(vec![Reply::json(201, entry)]);

    let o = cli(
        &host,
        home.path(),
        &[
            "memory",
            "save",
            RB,
            "--title",
            "Stdin Title",
            "--body-file",
            "-",
        ],
        &token(),
        Some("Markdown read from stdin directly"),
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("POST /v1/runbooks/{RB}/memory"));
    assert_eq!(
        body_of(&seen[0])["body_markdown"],
        "Markdown read from stdin directly"
    );
}

#[test]
fn save_with_an_execution_token_shows_the_returned_status() {
    let home = tempfile::tempdir().unwrap();
    let file_path = home.path().join("body.md");
    fs::write(&file_path, "Proposed body").unwrap();

    let entry = memory_entry(MEM, "proposed", "Proposed Fact", 1);
    let (host, server) = serve(vec![Reply::json(201, entry)]);

    let exe_env = [("RUNBOOK_TOKEN", EXE_TOKEN)];
    let o = cli(
        &host,
        home.path(),
        &[
            "memory",
            "save",
            RB,
            "--title",
            "Proposed Fact",
            "--body-file",
            file_path.to_str().unwrap(),
        ],
        &exe_env,
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));
    assert!(out(&o).contains("proposed"));
    assert!(out(&o).contains("Proposed Fact"));

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("POST /v1/runbooks/{RB}/memory"));
}

#[test]
fn list_proposed_sends_status_filter() {
    let home = tempfile::tempdir().unwrap();
    let entry = memory_entry(MEM, "proposed", "Pending Approval", 1);
    let (host, server) = serve(vec![
        Reply::json(200, page(vec![entry.clone()])),
        Reply::json(200, page(vec![entry.clone()])),
    ]);

    // Human output
    let o = cli(
        &host,
        home.path(),
        &["memory", "list", RB, "--proposed"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));
    assert!(out(&o).contains("proposed"));
    assert!(out(&o).contains("Pending Approval"));

    // JSON output
    let o = cli(
        &host,
        home.path(),
        &["memory", "list", "--runbook", RB, "--proposed", "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["items"][0]["status"], "proposed");

    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/runbooks/{RB}/memory?status=proposed")
    );
    assert_eq!(
        line(&seen[1]),
        format!("GET /v1/runbooks/{RB}/memory?status=proposed")
    );
}

#[test]
fn accept_fetches_revision_then_patches_with_if_match() {
    let home = tempfile::tempdir().unwrap();
    let current = memory_entry(MEM, "proposed", "Needs Accept", 3);
    let accepted = memory_entry(MEM, "active", "Needs Accept", 4);
    let (host, server) = serve(vec![
        Reply::json(200, current.clone()),
        Reply::json(200, accepted.clone()),
        Reply::json(200, current),
        Reply::json(200, accepted),
    ]);

    // Human output
    let o = cli(
        &host,
        home.path(),
        &["memory", "accept", MEM, "--revision", "3"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));
    assert!(out(&o).contains("active"));
    // What is accepted is shown first.
    assert!(err(&o).contains("accept: revision 3"), "{}", err(&o));

    // JSON output
    let o = cli(
        &host,
        home.path(),
        &["memory", "accept", MEM, "--revision", "3", "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["status"], "active");

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("GET /v1/memory/{MEM}?revision=3"));
    assert_eq!(line(&seen[1]), format!("PATCH /v1/memory/{MEM}"));
    assert_eq!(header(&seen[1], "if-match"), Some("\"r3\""));
    assert_eq!(body_of(&seen[1]), json!({"action": "accept"}));

    assert_eq!(line(&seen[2]), format!("GET /v1/memory/{MEM}?revision=3"));
    assert_eq!(line(&seen[3]), format!("PATCH /v1/memory/{MEM}"));
    assert_eq!(header(&seen[3], "if-match"), Some("\"r3\""));
    assert_eq!(body_of(&seen[3]), json!({"action": "accept"}));
}

#[test]
fn archive_fetches_revision_then_patches_with_if_match() {
    let home = tempfile::tempdir().unwrap();
    let current = memory_entry(MEM, "active", "Old Fact", 5);
    let archived = memory_entry(MEM, "archived", "Old Fact", 6);
    let (host, server) = serve(vec![Reply::json(200, current), Reply::json(200, archived)]);

    let o = cli(
        &host,
        home.path(),
        &["memory", "archive", MEM],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));
    assert!(out(&o).contains("archived"));

    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("GET /v1/memory/{MEM}"));
    assert_eq!(line(&seen[1]), format!("PATCH /v1/memory/{MEM}"));
    assert_eq!(header(&seen[1], "if-match"), Some("\"r5\""));
    assert_eq!(body_of(&seen[1]), json!({"action": "archive"}));
}

#[test]
fn not_found_exits_with_code_4() {
    let home = tempfile::tempdir().unwrap();
    let not_found = json!({
        "status": 404,
        "code": "resource.not_found",
        "message": "memory entry not found"
    });
    let (host, server) = serve(vec![
        Reply::json(404, not_found.clone()),
        Reply::json(404, not_found),
    ]);

    // Non-JSON
    let o = cli(
        &host,
        home.path(),
        &["memory", "accept", MEM, "--revision", "1"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(err(&o).contains("resource.not_found"));

    // JSON
    let o = cli(
        &host,
        home.path(),
        &["memory", "archive", MEM, "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["error"]["status"], 404);
    assert_eq!(parsed["error"]["code"], "resource.not_found");

    server.join().unwrap();
}

#[test]
fn stale_revision_exits_with_code_1() {
    let home = tempfile::tempdir().unwrap();
    let current = memory_entry(MEM, "proposed", "Title", 1);
    let conflict = json!({
        "status": 412,
        "code": "memory.conflict",
        "message": "revision mismatch"
    });
    let (host, server) = serve(vec![
        Reply::json(200, current.clone()),
        Reply::json(412, conflict.clone()),
        Reply::json(200, current),
        Reply::json(412, conflict),
    ]);

    // Non-JSON
    let o = cli(
        &host,
        home.path(),
        &["memory", "accept", MEM, "--revision", "1"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("memory.conflict"));

    // JSON
    let o = cli(
        &host,
        home.path(),
        &["memory", "archive", MEM, "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["error"]["status"], 412);
    assert_eq!(parsed["error"]["code"], "memory.conflict");

    server.join().unwrap();
}

#[test]
fn slug_with_project_resolves_across_two_pages() {
    let home = tempfile::tempdir().unwrap();
    let entry = memory_entry(MEM, "active", "Paged Fact", 1);
    let (host, server) = serve(vec![
        // Page 1 of runbooks
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": "rb_01JABCDEFGHJKMNPQRSTVWXYZ1", "slug": "other-rb"}
                ],
                "next_cursor": "cur_2"
            }),
        ),
        // Page 2 of runbooks
        Reply::json(
            200,
            json!({
                "items": [
                    {"id": RB, "slug": "target-rb"}
                ],
                "next_cursor": null
            }),
        ),
        // Memory search for target runbook
        Reply::json(200, page(vec![entry])),
    ]);

    let o = cli(
        &host,
        home.path(),
        &["memory", "search", "target-rb", "--project", PRJ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));

    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/projects/{PRJ}/runbooks?limit=100")
    );
    assert_eq!(
        line(&seen[1]),
        format!("GET /v1/projects/{PRJ}/runbooks?limit=100&cursor=cur_2")
    );
    assert_eq!(
        line(&seen[2]),
        format!("GET /v1/runbooks/{RB}/memory?status=active")
    );
}

#[test]
fn slug_without_project_exits_2() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);

    let o = cli(
        &host,
        home.path(),
        &["memory", "search", "my-slug"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("is a slug; pass --project to look it up, or use the rb_ ID"),
        "stderr: {}",
        err(&o)
    );
    server.join().unwrap();
}

#[test]
fn unknown_slug_exits_4() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![Reply::json(
        200,
        json!({
            "items": [],
            "next_cursor": null
        }),
    )]);

    let o = cli(
        &host,
        home.path(),
        &["memory", "search", "nonexistent-rb", "--project", PRJ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(4), "{}", err(&o));
    assert!(
        err(&o).contains("runbook 'nonexistent-rb' not found in project"),
        "stderr: {}",
        err(&o)
    );
    server.join().unwrap();
}

#[test]
fn rb_id_makes_no_list_call() {
    let home = tempfile::tempdir().unwrap();
    let entry = memory_entry(MEM, "active", "Direct ID Fact", 1);
    let (host, server) = serve(vec![Reply::json(200, page(vec![entry]))]);

    let o = cli(
        &host,
        home.path(),
        &["memory", "search", RB],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains(MEM));

    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/runbooks/{RB}/memory?status=active")
    );
}

#[test]
fn malformed_argument_refused_before_any_request() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![]);

    let o = cli(
        &host,
        home.path(),
        &["memory", "search", "../x", "--project", PRJ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("invalid runbook name: '../x'"),
        "stderr: {}",
        err(&o)
    );
    server.join().unwrap();
}

fn store_at(home: &std::path::Path) -> runbook_cli::credstore::StateDir {
    runbook_cli::credstore::StateDir::at(home.join("runbook").join("credentials"))
}

/// Install an execution pair for `host` as the active one.
fn install_pair(home: &std::path::Path, host: &str) {
    let store = store_at(home);
    store
        .save_execution(&runbook_cli::credstore::ExecutionCredential {
            host: host.into(),
            execution_id: EXE.into(),
            family_id: "fam_1".into(),
            generation: 1,
            access_token: EXE_TOKEN.into(),
            refresh_token: "trb_refresh_caller-refresh-secret-padded-43".into(),
            expires_at_ms: i64::MAX / 2,
            refresh_expires_at_ms: i64::MAX / 2,
        })
        .unwrap();
    store.set_active(host, EXE).unwrap();
}

fn save_args(file: &std::path::Path) -> Vec<String> {
    [
        "memory",
        "save",
        RB,
        "--title",
        "Fact",
        "--body-file",
        file.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Memory gate fixes: an agent holding an execution pair saves with it (from
/// its claim), even when a login is stored; the login is
/// used only when no pair is stored.
#[test]
fn save_prefers_the_stored_execution_pair_over_the_login() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("body.md");
    fs::write(&file, "Body").unwrap();
    let (host, server) = serve(vec![
        Reply::json(201, memory_entry(MEM, "active", "Fact", 1)),
        Reply::json(201, memory_entry(MEM, "proposed", "Fact", 1)),
    ]);
    store_at(home.path()).save_login(&host, PAT).unwrap();
    let args = save_args(&file);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // No pair: the login.
    let o = cli(&host, home.path(), &args, &[], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // With a pair: the pair, and the user is told it is a proposal.
    install_pair(home.path(), &host);
    let o = cli(&host, home.path(), &args, &[], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("proposal"), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(
        header(&seen[0], "authorization"),
        Some(format!("Bearer {PAT}").as_str())
    );
    assert_eq!(
        header(&seen[1], "authorization"),
        Some(format!("Bearer {EXE_TOKEN}").as_str())
    );
}

/// Memory gate fixes: stored text never reaches the terminal raw, and the
/// proposal listing shows what a reviewer needs (revision, source, step,
/// tags, the start of the body).
#[test]
fn listing_escapes_control_characters_and_shows_proposals_for_review() {
    let home = tempfile::tempdir().unwrap();
    let mut entry = memory_entry(MEM, "proposed", "Evil\u{1b}[2J\nFAKE  active  line", 2);
    entry["body_markdown"] = json!("Use the VPN.\u{1b}]0;title\u{7}\nThen check.");
    entry["source"] = json!({"kind": "agent", "token_id": TOK, "sponsor_user_id": USR});
    let (host, server) = serve(vec![Reply::json(200, page(vec![entry]))]);
    let o = cli(
        &host,
        home.path(),
        &["memory", "list", RB, "--proposed"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{7}'),
        "{text:?}"
    );
    assert!(text.contains("\\u{1b}[2J\\u{a}FAKE"), "{text}");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("mem_")).count(),
        1,
        "{text}"
    );
    assert!(text.contains("revision: 2"), "{text}");
    assert!(text.contains(&format!("agent token {TOK}")), "{text}");
    assert!(
        text.contains("step: step_1") && text.contains("tags: prod"),
        "{text}"
    );
    assert!(text.contains("body: Use the VPN."), "{text}");
    server.join().unwrap();
}

/// Memory gate fixes: `reject --all-proposed` rejects each of the
/// runbook's own proposals at the revision it listed (not the project's).
#[test]
fn reject_all_proposed_rejects_each_listed_revision() {
    let home = tempfile::tempdir().unwrap();
    let a = "mem_01JABCDEFGHJKMNPQRSTVWXYZA";
    let b = "mem_01JABCDEFGHJKMNPQRSTVWXYZB";
    let mut project = memory_entry("mem_01JABCDEFGHJKMNPQRSTVWXYZC", "proposed", "Project", 1);
    project["scope"] = json!({"kind": "project"});
    let (host, server) = serve(vec![
        Reply::json(
            200,
            page(vec![
                memory_entry(a, "proposed", "A", 1),
                memory_entry(b, "proposed", "B", 4),
                project,
            ]),
        ),
        Reply::json(200, memory_entry(a, "archived", "A", 2)),
        Reply::json(200, memory_entry(b, "archived", "B", 5)),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["memory", "reject", "--all-proposed", RB, "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["rejected"], json!([a, b]));
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/runbooks/{RB}/memory?status=proposed&limit=100")
    );
    assert_eq!(line(&seen[1]), format!("PATCH /v1/memory/{a}"));
    assert_eq!(header(&seen[1], "if-match"), Some("\"r1\""));
    assert_eq!(line(&seen[2]), format!("PATCH /v1/memory/{b}"));
    assert_eq!(header(&seen[2], "if-match"), Some("\"r4\""));
    assert_eq!(body_of(&seen[2]), json!({"action": "reject"}));
}

/// MEM6: a save is live by default; `--propose` asks for a person's review.
#[test]
fn save_is_live_unless_asked_to_propose() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("body.md");
    fs::write(&file, "Body").unwrap();
    let (host, server) = serve(vec![
        Reply::json(201, memory_entry(MEM, "active", "Fact", 1)),
        Reply::json(201, memory_entry(MEM, "proposed", "Fact", 1)),
    ]);
    let args = save_args(&file);
    let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = cli(&host, home.path(), &args, &token(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("future runs see it"), "{}", err(&o));
    args.push("--propose");
    let o = cli(&host, home.path(), &args, &token(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(err(&o).contains("proposal"), "{}", err(&o));
    let seen = server.join().unwrap();
    assert!(body_of(&seen[0]).get("propose").is_none());
    assert_eq!(body_of(&seen[1])["propose"], true);
}

/// MEM6: `memory edit` reads the entry, keeps what the flags leave alone,
/// and sends the edit fenced by the revision named; with a stored
/// execution pair it edits with the pair, as `save` does.
#[test]
fn edit_keeps_unchanged_fields_and_fences_the_revision() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("body.md");
    fs::write(&file, "Normal error rate is 0.7%").unwrap();
    let mut current = memory_entry(MEM, "active", "Baseline", 3);
    current["expires_at"] = json!("2030-01-01T00:00:00Z");
    let mut edited = memory_entry(MEM, "active", "Baseline", 4);
    edited["body_markdown"] = json!("Normal error rate is 0.7%");
    let (host, server) = serve(vec![
        Reply::json(200, current.clone()),
        Reply::json(200, edited.clone()),
        Reply::json(200, current),
        Reply::json(200, edited),
    ]);
    let args = [
        "memory",
        "edit",
        MEM,
        "--revision",
        "3",
        "--body-file",
        file.to_str().unwrap(),
        "--json",
    ];
    let o = cli(&host, home.path(), &args, &token(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["revision"], 4);
    install_pair(home.path(), &host);
    let o = cli(&host, home.path(), &args, &[], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("GET /v1/memory/{MEM}"));
    assert_eq!(line(&seen[1]), format!("PATCH /v1/memory/{MEM}"));
    assert_eq!(header(&seen[1], "if-match"), Some("\"r3\""));
    assert_eq!(
        body_of(&seen[1]),
        json!({"action": "edit", "entry": {
            "title": "Baseline",
            "body_markdown": "Normal error rate is 0.7%",
            "tags": ["prod"],
            "step_id": "step_1",
            "expires_at": "2030-01-01T00:00:00Z",
        }})
    );
    for request in &seen[2..] {
        assert_eq!(
            header(request, "authorization"),
            Some(format!("Bearer {EXE_TOKEN}").as_str())
        );
    }
}

/// MEM6: `memory history` lists each revision with its writer, escaped.
#[test]
fn history_lists_each_revision_and_its_writer() {
    let home = tempfile::tempdir().unwrap();
    let revision = |n: u32, action: &str, kind: &str, actor: &str| {
        json!({"revision": n, "action": action, "actor_kind": kind, "actor": actor,
               "at": "2026-11-13T10:00:00Z", "entry": memory_entry(MEM, "active", "Base\u{1b}line", n)})
    };
    let history = json!({"items": [
        revision(1, "create", "agent", &format!("agent:{TOK}:{USR}")),
        revision(2, "edit", "person", &format!("human:{USR}")),
    ], "next_cursor": "2", "pruned": 7});
    let (host, server) = serve(vec![Reply::json(200, history)]);
    let o = cli(
        &host,
        home.path(),
        &["memory", "history", MEM, "--limit", "2", "--cursor", "0"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("r1  create  agent"), "{text}");
    assert!(text.contains(TOK), "{text}");
    assert!(text.contains("r2  edit  person"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
    assert!(text.contains("next: 2"), "{text}");
    assert!(err(&o).contains("7 revisions pruned"), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/memory/{MEM}/revisions?limit=2&cursor=0")
    );
}

/// MEM6 review fix 6: `memory revert` restores a prior revision (a
/// person-only action that can unarchive), fenced by the current revision.
#[test]
fn revert_restores_a_prior_revision() {
    let home = tempfile::tempdir().unwrap();
    let current = memory_entry(MEM, "archived", "Changed", 3);
    let restored = memory_entry(MEM, "active", "Original", 4);
    let (host, server) = serve(vec![
        Reply::json(200, current).header("etag", "\"r3\""),
        Reply::json(200, restored),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["memory", "revert", MEM, "--to", "1"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("Original"), "{}", out(&o));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), format!("GET /v1/memory/{MEM}"));
    assert_eq!(line(&seen[1]), format!("PATCH /v1/memory/{MEM}"));
    assert_eq!(header(&seen[1], "if-match"), Some("\"r3\""));
    assert_eq!(
        body_of(&seen[1]),
        json!({"action": "restore", "revision": 1})
    );
}

/// MEM6 review fix 3: people lock an entry against agent edits and
/// unlock it, each at the current revision.
#[test]
fn lock_and_unlock_patch_at_the_current_revision() {
    let home = tempfile::tempdir().unwrap();
    let mut locked = memory_entry(MEM, "active", "Fact", 3);
    locked["locked"] = json!(true);
    let (host, server) = serve(vec![
        Reply::json(200, memory_entry(MEM, "active", "Fact", 2)),
        Reply::json(200, locked.clone()),
        Reply::json(200, locked),
        Reply::json(200, memory_entry(MEM, "active", "Fact", 4)),
    ]);
    for action in ["lock", "unlock"] {
        let o = cli(&host, home.path(), &["memory", action, MEM], &token(), None);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    }
    let seen = server.join().unwrap();
    assert_eq!(header(&seen[1], "if-match"), Some("\"r2\""));
    assert_eq!(body_of(&seen[1]), json!({"action": "lock"}));
    assert_eq!(header(&seen[3], "if-match"), Some("\"r3\""));
    assert_eq!(body_of(&seen[3]), json!({"action": "unlock"}));
}
