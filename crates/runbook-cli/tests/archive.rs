//! ARC4: CLI archive commands tests against the fake server pattern.
mod fake_api;
use fake_api::*;
use serde_json::{Value, json};
use std::fs;

fn archive_item(date: &str, run_count: u64, size_bytes: u64, sha256: &str, stale: bool) -> Value {
    json!({
        "date": date,
        "run_count": run_count,
        "size_bytes": size_bytes,
        "sha256": sha256,
        "stale": stale,
    })
}

#[test]
fn archive_list_sends_path_query_and_formats_output() {
    let home = tempfile::tempdir().unwrap();
    let item1 = archive_item("2026-11-10", 5, 1024, "abc123sha", false);
    let item2 = archive_item("2026-11-11", 2, 512, "def456sha", true);
    let reply_body = json!({
        "items": [item1, item2],
    });

    let (host, server) = serve(vec![
        Reply::json(200, reply_body.clone()),
        Reply::json(200, reply_body.clone()),
    ]);

    // Human output
    let o = cli(
        &host,
        home.path(),
        &[
            "archive",
            "list",
            RB,
            "--from",
            "2026-11-10",
            "--to",
            "2026-11-11",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let stdout = out(&o);
    assert!(stdout.contains("2026-11-10  5 runs  1024B  abc123sha"));
    assert!(stdout.contains("2026-11-11  2 runs  512B  def456sha  stale"));

    // JSON output
    let o = cli(
        &host,
        home.path(),
        &["archive", "list", RB, "--json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let parsed: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(parsed["items"].as_array().unwrap().len(), 2);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        line(&requests[0]),
        format!("GET /v1/runbooks/{RB}/archives?from=2026-11-10&to=2026-11-11")
    );
    assert_eq!(
        line(&requests[1]),
        format!("GET /v1/runbooks/{RB}/archives")
    );
}

#[test]
fn archive_list_resolves_slug_with_project() {
    let home = tempfile::tempdir().unwrap();
    let runbook_list = json!({
        "items": [
            {"id": "rb_other", "slug": "other-slug"},
            {"id": RB, "slug": "my-target-runbook"}
        ],
        "next_cursor": null
    });
    let archives = json!({
        "items": [archive_item("2026-11-12", 1, 100, "sha", false)]
    });

    let (host, server) = serve(vec![
        Reply::json(200, runbook_list),
        Reply::json(200, archives),
    ]);

    let o = cli(
        &host,
        home.path(),
        &["archive", "list", "my-target-runbook", "--project", PRJ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("2026-11-12  1 runs  100B  sha"));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        line(&requests[0]),
        format!("GET /v1/projects/{PRJ}/runbooks?limit=100")
    );
    assert_eq!(
        line(&requests[1]),
        format!("GET /v1/runbooks/{RB}/archives")
    );
}

#[test]
fn archive_list_slug_missing_project_exits_2() {
    let home = tempfile::tempdir().unwrap();
    let o = cli(
        "http://127.0.0.1:9999",
        home.path(),
        &["archive", "list", "my-target-runbook"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("is a slug; pass --project to look it up, or use the rb_ ID"));
}

#[test]
fn archive_list_slug_not_found_exits_4() {
    let home = tempfile::tempdir().unwrap();
    let runbook_list = json!({
        "items": [{"id": "rb_other", "slug": "other-slug"}],
        "next_cursor": null
    });
    let (host, _server) = serve(vec![Reply::json(200, runbook_list)]);

    let o = cli(
        &host,
        home.path(),
        &["archive", "list", "missing-slug", "--project", PRJ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(4));
    assert!(err(&o).contains("runbook 'missing-slug' not found in project"));
}

#[test]
fn archive_list_server_404_exits_4() {
    let home = tempfile::tempdir().unwrap();
    let (host, _server) = serve(vec![Reply::json(
        404,
        json!({"code": "runbook_not_found", "message": "no such runbook"}),
    )]);

    let o = cli(&host, home.path(), &["archive", "list", RB], &token(), None);
    assert_eq!(o.status.code(), Some(4));
}

#[test]
fn archive_download_date_to_file() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let zip_bytes = b"PK\x03\x04fake-archive-zip-contents".to_vec();

    let (host, server) = serve(vec![Reply::raw(200, "application/zip", zip_bytes.clone())]);

    let out_file = work.path().join("downloaded.zip");
    let o = cli(
        &host,
        home.path(),
        &[
            "archive",
            "download",
            RB,
            "--date",
            "2026-11-12",
            "--out",
            out_file.to_str().unwrap(),
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(fs::read(&out_file).unwrap(), zip_bytes);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        line(&requests[0]),
        format!("GET /v1/runbooks/{RB}/archives/2026-11-12")
    );
}

#[test]
fn archive_download_bundle_range_with_evidence() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let zip_bytes = b"PK\x03\x04range-bundle-bytes".to_vec();

    let (host, server) = serve(vec![Reply::raw(200, "application/zip", zip_bytes.clone())]);

    let out_file = work.path().join("bundle.zip");
    let o = cli(
        &host,
        home.path(),
        &[
            "archive",
            "download",
            RB,
            "--from",
            "2026-11-01",
            "--to",
            "2026-11-07",
            "--include-evidence",
            "--out",
            out_file.to_str().unwrap(),
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(fs::read(&out_file).unwrap(), zip_bytes);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        line(&requests[0]),
        format!(
            "GET /v1/runbooks/{RB}/archives?format=zip&from=2026-11-01&to=2026-11-07&include_evidence=true"
        )
    );
}

#[test]
fn archive_download_uses_server_content_disposition() {
    let home = tempfile::tempdir().unwrap();
    let zip_bytes = b"PK\x03\x04content-disp-bytes".to_vec();

    let (host, server) = serve(vec![
        Reply::raw(200, "application/zip", zip_bytes.clone()).header(
            "Content-Disposition",
            "attachment; filename=\"server-provided-name.zip\"",
        ),
    ]);

    let work = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .current_dir(work.path())
        .arg("--host")
        .arg(&host)
        .args(["archive", "download", RB, "--date", "2026-11-12"])
        .env("XDG_CONFIG_HOME", home.path())
        .env("HOME", home.path())
        .env("RUNBOOK_TOKEN", PAT);
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));

    let created_file = work.path().join("server-provided-name.zip");
    assert!(created_file.exists());
    assert_eq!(fs::read(created_file).unwrap(), zip_bytes);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
}

#[test]
fn archive_download_validation_errors_exit_2() {
    let home = tempfile::tempdir().unwrap();

    // --date combined with --from
    let o = cli(
        "http://127.0.0.1:9999",
        home.path(),
        &[
            "archive",
            "download",
            RB,
            "--date",
            "2026-11-12",
            "--from",
            "2026-11-01",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--date cannot be combined with --from / --to"));

    // --from without --to
    let o = cli(
        "http://127.0.0.1:9999",
        home.path(),
        &["archive", "download", RB, "--from", "2026-11-01"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--from requires --to"));

    // --to without --from
    let o = cli(
        "http://127.0.0.1:9999",
        home.path(),
        &["archive", "download", RB, "--to", "2026-11-01"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--to requires --from"));

    // neither
    let o = cli(
        "http://127.0.0.1:9999",
        home.path(),
        &["archive", "download", RB],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("pass either --date <YYYY-MM-DD> or both --from and --to"));
}

#[test]
fn archive_download_409_rewrite_pending_with_retry_after() {
    let home = tempfile::tempdir().unwrap();
    let reply = Reply::json(
        409,
        json!({
            "code": "archive.rewrite_pending",
            "message": "archive is being rewritten"
        }),
    )
    .header("Retry-After", "900");

    let (host, server) = serve(vec![reply]);

    let o = cli(
        &host,
        home.path(),
        &[
            "archive",
            "download",
            RB,
            "--date",
            "2026-11-12",
            "--out",
            "out.zip",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains(
        "error: archive rewrite pending: retry after 900s (the daily job will rewrite it)"
    ));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
}

#[test]
fn archive_download_stdout_when_redirected() {
    let home = tempfile::tempdir().unwrap();
    let zip_bytes = b"PK\x03\x04streamed-stdout-bytes".to_vec();

    let (host, server) = serve(vec![Reply::raw(200, "application/zip", zip_bytes.clone())]);

    // cli() pipes stdout, so stdout is not a terminal; passing --out - should write bytes to stdout
    let o = cli(
        &host,
        home.path(),
        &[
            "archive",
            "download",
            RB,
            "--date",
            "2026-11-12",
            "--out",
            "-",
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(o.stdout, zip_bytes);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
}

#[test]
fn archive_download_hostile_filename_falls_back_to_default_name() {
    let home = tempfile::tempdir().unwrap();
    let zip_bytes = b"PK\x03\x04hostile-name-bytes".to_vec();

    let (host, server) = serve(vec![
        Reply::raw(200, "application/zip", zip_bytes.clone())
            .header("Content-Disposition", "attachment; filename=\".bashrc\""),
    ]);

    let work = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .current_dir(work.path())
        .arg("--host")
        .arg(&host)
        .args(["archive", "download", RB, "--date", "2026-11-12"])
        .env("XDG_CONFIG_HOME", home.path())
        .env("HOME", home.path())
        .env("RUNBOOK_TOKEN", PAT);
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", err(&output));

    // .bashrc must not exist
    assert!(!work.path().join(".bashrc").exists());

    // Landed under default name
    let default_file = work.path().join(format!("{RB}-2026-11-12.zip"));
    assert!(default_file.exists());
    assert_eq!(fs::read(default_file).unwrap(), zip_bytes);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
}

#[test]
fn archive_download_default_name_exists_refuses_overwrite_exit_1() {
    let home = tempfile::tempdir().unwrap();
    let zip_bytes = b"PK\x03\x04new-bytes".to_vec();

    let (host, _server) = serve(vec![Reply::raw(200, "application/zip", zip_bytes)]);

    let work = tempfile::tempdir().unwrap();
    let default_file = work.path().join(format!("{RB}-2026-11-12.zip"));
    fs::write(&default_file, b"existing-archive-content").unwrap();

    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_runbook"));
    command
        .current_dir(work.path())
        .arg("--host")
        .arg(&host)
        .args(["archive", "download", RB, "--date", "2026-11-12"])
        .env("XDG_CONFIG_HOME", home.path())
        .env("HOME", home.path())
        .env("RUNBOOK_TOKEN", PAT);
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(err(&output).contains("file exists; pass --out to choose a path"));

    // Content was not overwritten
    assert_eq!(
        fs::read(&default_file).unwrap(),
        b"existing-archive-content"
    );
}

#[test]
fn archive_download_explicit_out_overwrites_existing_file() {
    let home = tempfile::tempdir().unwrap();
    let zip_bytes = b"PK\x03\x04new-explicit-bytes".to_vec();

    let (host, server) = serve(vec![Reply::raw(200, "application/zip", zip_bytes.clone())]);

    let work = tempfile::tempdir().unwrap();
    let out_file = work.path().join("existing-target.zip");
    fs::write(&out_file, b"old-content-to-be-overwritten").unwrap();

    let o = cli(
        &host,
        home.path(),
        &[
            "archive",
            "download",
            RB,
            "--date",
            "2026-11-12",
            "--out",
            out_file.to_str().unwrap(),
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(fs::read(&out_file).unwrap(), zip_bytes);

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
}
