//! Unit and CLI integration tests for `runbook skill` commands and Claude Code plugin.
mod support;
use runbook_cli::skill::{
    AgentKind, ScopeKind, agent_hint, agent_skills_base_dir, safe_write_skill_files,
    validate_skill_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::*;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn root_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// 1. Unit Tests
// ---------------------------------------------------------------------------

#[test]
fn test_validate_skill_dir_missing_skill_md() {
    let dir = tmp();
    let sub = dir.path().join("my-skill");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("other.txt"), "hello").unwrap();

    let err = validate_skill_dir(&sub).unwrap_err();
    assert_eq!(err, 2);
}

#[test]
fn test_validate_skill_dir_missing_name_frontmatter() {
    let dir = tmp();
    let sub = dir.path().join("my-skill");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("SKILL.md"),
        "---\ndescription: A cool skill\n---\n# Content\n",
    )
    .unwrap();

    let err = validate_skill_dir(&sub).unwrap_err();
    assert_eq!(err, 2);
}

#[test]
fn test_validate_skill_dir_missing_description_frontmatter() {
    let dir = tmp();
    let sub = dir.path().join("my-skill");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("SKILL.md"),
        "---\nname: my-skill\n---\n# Content\n",
    )
    .unwrap();

    let err = validate_skill_dir(&sub).unwrap_err();
    assert_eq!(err, 2);
}

#[test]
fn test_validate_skill_dir_invalid_name_casing_or_chars() {
    let dir = tmp();
    let sub = dir.path().join("My-Skill");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("SKILL.md"),
        "---\nname: My-Skill\ndescription: Test\n---\n# Content\n",
    )
    .unwrap();

    let err = validate_skill_dir(&sub).unwrap_err();
    assert_eq!(err, 2);
}

#[test]
fn test_validate_skill_dir_name_mismatch_with_folder() {
    let dir = tmp();
    let sub = dir.path().join("folder-name");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("SKILL.md"),
        "---\nname: other-name\ndescription: Test\n---\n# Content\n",
    )
    .unwrap();

    let err = validate_skill_dir(&sub).unwrap_err();
    assert_eq!(err, 2);
}

#[test]
fn test_validate_skill_dir_valid() {
    let dir = tmp();
    let sub = dir.path().join("good-skill");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("SKILL.md"),
        "---\nname: good-skill\ndescription: A good skill\n---\n# Content\n",
    )
    .unwrap();
    let sub_ref = sub.join("references");
    fs::create_dir(&sub_ref).unwrap();
    fs::write(sub_ref.join("guide.md"), "# Guide\n").unwrap();

    let (name, desc, files) = validate_skill_dir(&sub).expect("validation should succeed");
    assert_eq!(name, "good-skill");
    assert_eq!(desc, "A good skill");
    assert_eq!(files.len(), 2);
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"SKILL.md"));
    assert!(paths.contains(&"references/guide.md"));
}

#[test]
fn test_path_safety_traversal_and_absolute() {
    let target = tmp();
    let files = vec![("../escaped.txt".to_string(), b"bad".to_vec())];
    let err = safe_write_skill_files(target.path(), &files, false).unwrap_err();
    assert_eq!(err, 2);

    let files_abs = vec![("/etc/passwd".to_string(), b"bad".to_vec())];
    let err = safe_write_skill_files(target.path(), &files_abs, false).unwrap_err();
    assert_eq!(err, 2);
}

#[test]
fn test_safe_write_skill_files_overwrite_protection() {
    let target = tmp();
    fs::write(target.path().join("existing.txt"), "present").unwrap();

    let files = vec![("SKILL.md".to_string(), b"# Skill".to_vec())];

    // Non-empty target without force fails
    let err = safe_write_skill_files(target.path(), &files, false).unwrap_err();
    assert_eq!(err, 2);

    // With force it succeeds
    safe_write_skill_files(target.path(), &files, true).expect("should succeed with force");
    assert!(target.path().join("SKILL.md").exists());
}

#[test]
fn test_agent_skills_base_dir() {
    let user_claude = agent_skills_base_dir(AgentKind::Claude, ScopeKind::User).unwrap();
    assert!(user_claude.ends_with(".claude/skills"));

    let user_codex = agent_skills_base_dir(AgentKind::Codex, ScopeKind::User).unwrap();
    assert!(user_codex.ends_with(".agents/skills"));

    let user_cursor = agent_skills_base_dir(AgentKind::Cursor, ScopeKind::User).unwrap();
    assert!(user_cursor.ends_with(".cursor/skills"));

    let proj_claude = agent_skills_base_dir(AgentKind::Claude, ScopeKind::Project).unwrap();
    assert!(proj_claude.ends_with(".claude/skills"));

    let proj_codex = agent_skills_base_dir(AgentKind::Codex, ScopeKind::Project).unwrap();
    assert!(proj_codex.ends_with(".agents/skills"));

    let proj_cursor = agent_skills_base_dir(AgentKind::Cursor, ScopeKind::Project).unwrap();
    assert!(proj_cursor.ends_with(".cursor/skills"));
}

#[test]
fn test_agent_hints() {
    assert_eq!(
        agent_hint(AgentKind::Claude, false),
        Some("hint: if the skills directory didn't exist before, run /reload-skills")
    );
    assert_eq!(agent_hint(AgentKind::Claude, true), None);
    assert_eq!(
        agent_hint(AgentKind::Codex, false),
        Some("hint: restart Codex if the skill doesn't appear (/skills lists them)")
    );
    assert_eq!(
        agent_hint(AgentKind::Cursor, false),
        Some("hint: restart Cursor")
    );
}

// ---------------------------------------------------------------------------
// 2. Claude Code Plugin Bundle Tests
// ---------------------------------------------------------------------------

#[test]
fn test_claude_plugin_bundle_integrity() {
    let root = root_dir();
    let plugin_dir = root.join("plugins/claude-code");
    assert!(plugin_dir.exists(), "plugins/claude-code must exist");

    // Check .claude-plugin/plugin.json
    let manifest_path = plugin_dir.join(".claude-plugin/plugin.json");
    assert!(manifest_path.exists());
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_slice(&manifest_bytes).expect("valid JSON");
    assert_eq!(manifest["name"], "therunbook");
    assert_eq!(manifest["version"], "0.1.0");
    assert!(
        manifest["description"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(manifest["author"]["name"], "Angler AI");

    // Check .mcp.json
    let mcp_path = plugin_dir.join(".mcp.json");
    assert!(mcp_path.exists());
    let mcp_bytes = fs::read(&mcp_path).unwrap();
    let mcp: Value = serde_json::from_slice(&mcp_bytes).expect("valid JSON");
    let server_entry = mcp["mcpServers"]["runbook"]
        .as_object()
        .or_else(|| mcp["mcpServers"]["therunbook"].as_object())
        .expect("server config in .mcp.json");
    assert_eq!(server_entry["command"], "runbook");
    assert_eq!(server_entry["args"], json!(["mcp", "serve"]));

    // Compare skills/therunbook/ in repo root and in plugin bundle
    let source_skill = root.join("skills/therunbook");
    let bundled_skill = plugin_dir.join("skills/therunbook");
    assert!(source_skill.exists());
    assert!(bundled_skill.exists());

    fn compare_dirs(src: &Path, dst: &Path) {
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let src_file = entry.path();
            let name = entry.file_name();
            let dst_file = dst.join(name);
            assert!(dst_file.exists(), "missing file in bundle: {:?}", dst_file);
            if src_file.is_dir() {
                compare_dirs(&src_file, &dst_file);
            } else {
                let src_bytes = fs::read(&src_file).unwrap();
                let dst_bytes = fs::read(&dst_file).unwrap();
                assert_eq!(
                    src_bytes, dst_bytes,
                    "file contents differ for {:?}",
                    src_file
                );
            }
        }
    }
    compare_dirs(&source_skill, &bundled_skill);

    // If claude binary is present, run claude plugin validate
    let local_claude =
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin/claude"));
    let claude_bin = if let Some(path) = local_claude.filter(|p| p.exists()) {
        Some(path)
    } else if let Ok(out) = Command::new("which").arg("claude").output() {
        if out.status.success() {
            Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        } else {
            None
        }
    } else {
        None
    };

    if let Some(claude) = claude_bin {
        let status = Command::new(claude)
            .args(["plugin", "validate", plugin_dir.to_str().unwrap()])
            .status()
            .expect("failed to run claude plugin validate");
        assert!(status.success(), "claude plugin validate failed");
    }
}

// ---------------------------------------------------------------------------
// 3. CLI Integration Tests with Fake HTTP Server
// ---------------------------------------------------------------------------

#[test]
fn test_cli_skill_list() {
    let home = tmp();
    let (host, _server) = serve(vec![
        // GET /.well-known/runbook handled automatically
        reply(
            200,
            json!({
                "items": [
                    {
                        "name": "check-error-rates",
                        "hash": "1111111111111111111111111111111111111111111111111111111111111111",
                        "description": "Checks service error rates",
                        "latest": true,
                        "usages": [
                            {
                                "runbook_id": "rb_01JABCDEFGHJKMNPQRSTVWXYZ4",
                                "version_id": "rbv_01JABCDEFGHJKMNPQRSTVWXYZ6",
                                "step_ids": ["check"]
                            }
                        ]
                    }
                ],
                "next_cursor": null
            }),
        ),
    ]);

    let o = cli(
        &host,
        home.path(),
        &["skill", "list", "--project", "default"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}, stderr: {}",
        out(&o),
        err(&o)
    );
    let stdout = out(&o);
    assert!(stdout.contains("check-error-rates"));
    assert!(stdout.contains("1111111111111111111111111111111111111111111111111111111111111111"));
    assert!(stdout.contains("rb_01JABCDEFGHJKMNPQRSTVWXYZ4"));

    // Also test --json
    let (host2, _server2) = serve(vec![reply(
        200,
        json!({
            "items": [
                {
                    "name": "check-error-rates",
                    "hash": "1111111111111111111111111111111111111111111111111111111111111111",
                    "description": "Checks service error rates",
                    "latest": true,
                    "usages": []
                }
            ],
            "next_cursor": null
        }),
    )]);
    let o_json = cli(
        &host2,
        home.path(),
        &["skill", "list", "--project", "default", "--json"],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o_json.status.code(), Some(0));
    let val: Value = serde_json::from_str(&out(&o_json)).unwrap();
    assert_eq!(val["items"][0]["name"], "check-error-rates");
}

#[test]
fn test_cli_skill_push_valid_and_validation_error() {
    let home = tmp();
    let skill_dir = home.path().join("my-skill");
    fs::create_dir(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: Test push skill\n---\n# My Skill\n",
    )
    .unwrap();

    let (host, server) = serve(vec![
        // GET /v1/meta to check limits.skill_bundle_bytes
        reply(
            200,
            json!({
                "version": "0.1.0",
                "schema": 1,
                "limits": {
                    "skill_bundle_bytes": 2097152
                }
            }),
        ),
        // POST /v1/projects/default/skills
        reply(
            200,
            json!({
                "hash": "2222222222222222222222222222222222222222222222222222222222222222",
                "name": "my-skill",
                "description": "Test push skill",
                "files": [
                    {
                        "path": "SKILL.md",
                        "size": 50,
                        "sha256": "3333333333333333333333333333333333333333333333333333333333333333"
                    }
                ],
                "latest": true,
                "usages": []
            }),
        ),
    ]);

    let o = cli(
        &host,
        home.path(),
        &[
            "skill",
            "push",
            skill_dir.to_str().unwrap(),
            "--project",
            "default",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}, stderr: {}",
        out(&o),
        err(&o)
    );
    assert!(out(&o).contains("uploaded my-skill"));

    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].starts_with("GET /v1/meta "));
    assert!(seen[1].starts_with("POST /v1/projects/default/skills "));

    // Validation failure: directory without SKILL.md fails locally before any network call
    let bad_dir = home.path().join("bad-dir");
    fs::create_dir(&bad_dir).unwrap();
    let o_bad = cli(
        &host,
        home.path(),
        &["skill", "push", bad_dir.to_str().unwrap()],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o_bad.status.code(), Some(2));
    assert!(err(&o_bad).contains("SKILL.md"));
}

#[test]
fn test_cli_skill_push_size_limit_exceeded() {
    let home = tmp();
    let skill_dir = home.path().join("big-skill");
    fs::create_dir(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: big-skill\ndescription: Big skill\n---\n# Big Content\n",
    )
    .unwrap();

    let (host, server) = serve(vec![
        // GET /v1/meta with small bundle limit
        reply(
            200,
            json!({
                "version": "0.1.0",
                "schema": 1,
                "limits": {
                    "skill_bundle_bytes": 10
                }
            }),
        ),
    ]);

    let o = cli(
        &host,
        home.path(),
        &[
            "skill",
            "push",
            skill_dir.to_str().unwrap(),
            "--project",
            "default",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(
        o.status.code(),
        Some(2),
        "stdout: {}, stderr: {}",
        out(&o),
        err(&o)
    );
    assert!(err(&o).contains("exceeds server limit"));
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 1); // Never called POST /v1/projects/.../skills
}

#[test]
fn test_cli_skill_pull_valid_and_overwrite() {
    let home = tmp();
    let out_dir = home.path().join("downloaded-skill");

    let file_content = "# Downloaded skill content\n";
    let b64_content = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        file_content.as_bytes(),
    );

    let (host, _server) = serve(vec![
        // 1. Resolve name to latest version
        reply(
            200,
            json!({
                "items": [
                    {
                        "name": "pulled-skill",
                        "hash": "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                        "description": "",
                        "latest": true,
                        "usages": []
                    }
                ],
                "next_cursor": null
            }),
        ),
        // 2. GET skill files with ?files=true
        reply(
            200,
            json!({
                "hash": "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                "name": "pulled-skill",
                "description": "",
                "files": [
                    {
                        "path": "SKILL.md",
                        "size": file_content.len(),
                        "sha256": "...",
                        "content": b64_content
                    }
                ],
                "latest": true,
                "usages": []
            }),
        ),
    ]);

    let o = cli(
        &host,
        home.path(),
        &[
            "skill",
            "pull",
            "pulled-skill",
            "--out",
            out_dir.to_str().unwrap(),
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}, stderr: {}",
        out(&o),
        err(&o)
    );
    assert!(out_dir.join("SKILL.md").exists());
    assert_eq!(
        fs::read_to_string(out_dir.join("SKILL.md")).unwrap(),
        file_content
    );

    // Pulling again without --force fails because target directory is non-empty
    let (host_retry, _server_retry) = serve(vec![
        reply(
            200,
            json!({
                "items": [
                    {
                        "name": "pulled-skill",
                        "hash": "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                        "description": "",
                        "latest": true,
                        "usages": []
                    }
                ],
                "next_cursor": null
            }),
        ),
        reply(
            200,
            json!({
                "hash": "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                "name": "pulled-skill",
                "description": "",
                "files": [
                    {
                        "path": "SKILL.md",
                        "size": file_content.len(),
                        "sha256": "...",
                        "content": b64_content
                    }
                ],
                "latest": true,
                "usages": []
            }),
        ),
    ]);
    let o2 = cli(
        &host_retry,
        home.path(),
        &[
            "skill",
            "pull",
            "pulled-skill",
            "--out",
            out_dir.to_str().unwrap(),
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o2.status.code(), Some(2));
    assert!(err(&o2).contains("is not empty; use --force to overwrite"));

    // Pulling again with --force succeeds
    let (host2, _server2) = serve(vec![
        reply(
            200,
            json!({
                "items": [
                    {
                        "name": "pulled-skill",
                        "hash": "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                        "description": "",
                        "latest": true,
                        "usages": []
                    }
                ],
                "next_cursor": null
            }),
        ),
        reply(
            200,
            json!({
                "hash": "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
                "name": "pulled-skill",
                "description": "",
                "files": [
                    {
                        "path": "SKILL.md",
                        "size": file_content.len(),
                        "sha256": "...",
                        "content": b64_content
                    }
                ],
                "latest": true,
                "usages": []
            }),
        ),
    ]);

    let o3 = cli(
        &host2,
        home.path(),
        &[
            "skill",
            "pull",
            "pulled-skill",
            "--out",
            out_dir.to_str().unwrap(),
            "--force",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o3.status.code(), Some(0));
}

#[test]
fn test_cli_skill_install_therunbook_offline() {
    let home = tmp();
    // Host is dummy and unreachable; install --therunbook does not talk to server
    let o = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--agent", "claude", "--therunbook"],
        &[],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}, stderr: {}",
        out(&o),
        err(&o)
    );

    let installed_dir = home.path().join(".claude/skills/therunbook");
    assert!(installed_dir.join("SKILL.md").exists());
    assert!(installed_dir.join("references/cli.md").exists());
    assert!(installed_dir.join("references/authoring.md").exists());

    // Hint printed
    let stdout = out(&o);
    assert!(stdout.contains("/reload-skills"));

    // Also test codex
    let o_codex = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--agent", "codex", "--therunbook"],
        &[],
    );
    assert_eq!(o_codex.status.code(), Some(0));
    assert!(
        home.path()
            .join(".agents/skills/therunbook/SKILL.md")
            .exists()
    );
    assert!(out(&o_codex).contains("restart Codex"));

    // Also test cursor
    let o_cursor = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--agent", "cursor", "--therunbook"],
        &[],
    );
    assert_eq!(o_cursor.status.code(), Some(0));
    assert!(
        home.path()
            .join(".cursor/skills/therunbook/SKILL.md")
            .exists()
    );
    assert!(out(&o_cursor).contains("restart Cursor"));
}

#[test]
fn test_cli_skill_install_project_skill() {
    let home = tmp();
    let file_content = "# Project skill content\n";
    let b64_content = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        file_content.as_bytes(),
    );

    let (host, _server) = serve(vec![
        reply(
            200,
            json!({
                "items": [
                    {
                        "name": "custom-ops",
                        "hash": "3333333333333333333333333333333333333333333333333333333333333333",
                        "description": "",
                        "latest": true,
                        "usages": []
                    }
                ],
                "next_cursor": null
            }),
        ),
        reply(
            200,
            json!({
                "hash": "3333333333333333333333333333333333333333333333333333333333333333",
                "name": "custom-ops",
                "description": "",
                "files": [
                    {
                        "path": "SKILL.md",
                        "size": file_content.len(),
                        "sha256": "...",
                        "content": b64_content
                    }
                ],
                "latest": true,
                "usages": []
            }),
        ),
    ]);

    let o = cli(
        &host,
        home.path(),
        &[
            "skill",
            "install",
            "--agent",
            "claude",
            "--skill",
            "custom-ops",
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "stdout: {}, stderr: {}",
        out(&o),
        err(&o)
    );
    assert!(
        home.path()
            .join(".claude/skills/custom-ops/SKILL.md")
            .exists()
    );
}

// ---------------------------------------------------------------------------
// Review follow-ups: paging, version selection, symlinks
// ---------------------------------------------------------------------------

const HASH_A: &str = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
const HASH_B: &str = "aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222";

fn skill_item(name: &str, hash: &str, latest: bool) -> Value {
    json!({"name": name, "hash": hash, "description": "d", "files": [], "latest": latest, "usages": []})
}

#[test]
fn pull_follows_next_cursor_to_find_the_skill() {
    let home = tmp();
    let out_dir = home.path().join("late-skill");
    let content = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"# late\n");
    let (host, server) = serve(vec![
        reply(
            200,
            json!({"items": [skill_item("other", HASH_B, true)], "next_cursor": "page2"}),
        ),
        reply(
            200,
            json!({"items": [skill_item("late-skill", HASH_A, true)], "next_cursor": null}),
        ),
        reply(
            200,
            json!({"name": "late-skill", "hash": HASH_A, "description": "d", "latest": true, "usages": [],
                   "files": [{"path": "SKILL.md", "size": 7, "sha256": "x", "content": content}]}),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &[
            "skill",
            "pull",
            "late-skill",
            "--out",
            out_dir.to_str().unwrap(),
        ],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", err(&o));
    assert_eq!(
        fs::read_to_string(out_dir.join("SKILL.md")).unwrap(),
        "# late\n"
    );
    let seen = server.join().unwrap();
    assert!(seen[1].contains("cursor=page2"), "{}", seen[1]);
    assert!(
        seen[2].contains(&format!("/skills/{HASH_A}?files=true")),
        "{}",
        seen[2]
    );
}

#[test]
fn an_ambiguous_or_empty_version_is_refused() {
    let items = vec![
        skill_item("s", HASH_A, false),
        skill_item("s", HASH_B, true),
    ];
    assert_eq!(
        runbook_cli::skill::select_skill_version(&items, "p", "s", Some("aaaa")).unwrap_err(),
        2
    );
    assert_eq!(
        runbook_cli::skill::select_skill_version(&items, "p", "s", Some("")).unwrap_err(),
        2
    );
    assert_eq!(
        runbook_cli::skill::select_skill_version(&items, "p", "s", Some("AAAA1")).unwrap()["hash"],
        HASH_A
    );
    assert_eq!(
        runbook_cli::skill::select_skill_version(&items, "p", "s", None).unwrap()["hash"],
        HASH_B
    );
    assert_eq!(
        runbook_cli::skill::select_skill_version(&items, "p", "s", Some("ffff")).unwrap_err(),
        1
    );
}

#[test]
fn a_server_without_file_contents_fails_the_pull() {
    let home = tmp();
    let out_dir = home.path().join("s");
    let (host, _server) = serve(vec![
        reply(
            200,
            json!({"items": [skill_item("s", HASH_A, true)], "next_cursor": null}),
        ),
        reply(
            200,
            json!({"name": "s", "hash": HASH_A, "description": "d", "latest": true, "usages": [],
                   "files": [{"path": "SKILL.md", "size": 7, "sha256": "x"}]}),
        ),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["skill", "pull", "s", "--out", out_dir.to_str().unwrap()],
        &[("RUNBOOK_TOKEN", PAT)],
    );
    assert_eq!(o.status.code(), Some(1), "stderr: {}", err(&o));
    assert!(!out_dir.exists());
}

#[cfg(unix)]
#[test]
fn push_refuses_a_symlink_in_the_skill_folder() {
    let dir = tmp();
    let sub = dir.path().join("my-skill");
    fs::create_dir(&sub).unwrap();
    fs::write(
        sub.join("SKILL.md"),
        "---\nname: my-skill\ndescription: d\n---\n",
    )
    .unwrap();
    let secret = dir.path().join("outside.txt");
    fs::write(&secret, "not for upload").unwrap();
    std::os::unix::fs::symlink(&secret, sub.join("link.txt")).unwrap();
    assert_eq!(validate_skill_dir(&sub).unwrap_err(), 2);

    // A directory symlink pointing at its parent would otherwise recurse forever.
    fs::remove_file(sub.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(&sub, sub.join("loop")).unwrap();
    assert_eq!(validate_skill_dir(&sub).unwrap_err(), 2);
}

#[test]
fn install_without_agent_targets_every_agent_found_and_updates_in_place() {
    let home = tmp();
    // Nothing set up yet: the CLI asks for --agent.
    let none = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--therunbook"],
        &[],
    );
    assert_eq!(none.status.code(), Some(2), "{}", err(&none));
    assert!(err(&none).contains("no agent found"), "{}", err(&none));

    // Claude Code and Codex are set up; Cursor is not.
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    let o = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--therunbook", "--json"],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    let agents: Vec<&str> = v["installed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["agent"].as_str().unwrap())
        .collect();
    assert_eq!(agents, ["claude", "codex"]);
    assert!(
        home.path()
            .join(".claude/skills/therunbook/SKILL.md")
            .exists()
    );
    assert!(
        home.path()
            .join(".agents/skills/therunbook/SKILL.md")
            .exists()
    );
    assert!(!home.path().join(".cursor").exists());

    // Running it again (after an upgrade) replaces the earlier install.
    let skill = home.path().join(".claude/skills/therunbook/SKILL.md");
    std::fs::write(&skill, "old\n").unwrap();
    let again = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--therunbook", "--agent", "claude"],
        &[],
    );
    assert_eq!(again.status.code(), Some(0), "{}", err(&again));
    assert_ne!(std::fs::read_to_string(&skill).unwrap(), "old\n");

    // A file the bundled skill does not have is somebody's edit: keep it
    // unless --force.
    std::fs::write(
        home.path().join(".claude/skills/therunbook/notes.md"),
        "mine\n",
    )
    .unwrap();
    let edited = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--therunbook", "--agent", "claude"],
        &[],
    );
    assert_eq!(edited.status.code(), Some(2));
    assert!(err(&edited).contains("--force"), "{}", err(&edited));
}

#[test]
fn install_therunbook_writes_every_file_of_the_skill_folder() {
    // A file added to skills/therunbook/ must also be embedded in the CLI.
    fn files(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, base, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let source = root_dir().join("skills/therunbook");
    let mut expected = Vec::new();
    files(&source, &source, &mut expected);
    expected.sort();

    let home = tmp();
    let o = cli(
        "http://127.0.0.1:9",
        home.path(),
        &["skill", "install", "--agent", "claude", "--therunbook"],
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let installed_dir = home.path().join(".claude/skills/therunbook");
    let mut installed = Vec::new();
    files(&installed_dir, &installed_dir, &mut installed);
    installed.sort();
    assert_eq!(installed, expected);
    for rel in &expected {
        assert_eq!(
            fs::read(source.join(rel)).unwrap(),
            fs::read(installed_dir.join(rel)).unwrap(),
            "{rel} differs from the embedded copy"
        );
    }
}
