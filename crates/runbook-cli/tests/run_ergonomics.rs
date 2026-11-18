//! C2 `run start --wait|--follow` and `run report --format md|json|junit|pdf`.
mod fake_api;
use fake_api::*;
use serde_json::{Value, json};

const MARKDOWN: &str = "# Deploy\n\n- **Status:** succeeded\n- Result: rolled_back\n";

fn started() -> Reply {
    Reply::json(
        201,
        json!({"id":EXE,"status":"queued","runbook_version_id":"rbv_01JABCDEFGHJKMNPQRSTVWXYZ9","runbook_version":3}),
    )
}
fn start_args(extra: &[&'static str]) -> Vec<&'static str> {
    let mut args = vec![
        "run",
        "start",
        "deploy",
        "--project",
        "ops",
        "--idempotency-key",
        "k",
        "--interval-ms",
        "10",
    ];
    args.extend_from_slice(extra);
    args
}

#[test]
fn start_wait_polls_until_terminal_and_exits_3_on_failure() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        started(),
        Reply::json(200, json!({"id":EXE,"status":"running"})),
        Reply::json(200, json!({"id":EXE,"status":"failed"})),
    ]);
    let o = cli(&host, home.path(), &start_args(&["--wait"]), &token(), None);
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[0]), "POST /v1/projects/ops/executions");
    assert_eq!(line(&seen[1]), format!("GET /v1/executions/{EXE}"));
    assert_eq!(seen.len(), 3);
}

#[test]
fn start_wait_exits_0_for_a_successful_rollback_and_prints_one_json_document() {
    let home = tempfile::tempdir().unwrap();
    let done = json!({"id":EXE,"status":"succeeded","result":"rolled_back","runbook_version":3});
    let (host, _server) = serve(vec![started(), Reply::json(200, done.clone())]);
    let o = cli(
        &host,
        home.path(),
        &start_args(&["--wait", "--json"]),
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), done);
    let (host, _server) = serve(vec![
        started(),
        Reply::json(200, json!({"id":EXE,"status":"cancelled"})),
    ]);
    let o = cli(&host, home.path(), &start_args(&["--wait"]), &token(), None);
    assert_eq!(o.status.code(), Some(3), "cancelled is not success");
}

#[test]
fn start_follow_streams_events_until_completion() {
    let home = tempfile::tempdir().unwrap();
    let frames = "id: 1\nevent: ExecutionStarted\ndata: {\"seq\":1}\n\n\
        id: 2\nevent: ExecutionCompleted\ndata: {\"seq\":2,\"event\":{\"type\":\"ExecutionCompleted\",\"status\":\"failed\"}}\n\n";
    let (host, server) = serve(vec![
        started(),
        Reply::text(200, "text/event-stream", frames),
    ]);
    let o = cli(
        &host,
        home.path(),
        &start_args(&["--follow"]),
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(out(&o).contains("ExecutionStarted"), "{}", out(&o));
    assert!(out(&o).contains("ExecutionCompleted"));
    let seen = server.join().unwrap();
    assert_eq!(line(&seen[1]), format!("GET /v1/executions/{EXE}/events"));
}

#[test]
fn start_without_wait_does_not_poll_and_limits_exit_4() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![started()]);
    let o = cli(&host, home.path(), &start_args(&[]), &token(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(server.join().unwrap().len(), 1);
    let (host, server) = serve(vec![Reply::json(
        403,
        json!({"code":"limit.executions_monthly"}),
    )]);
    let o = cli(&host, home.path(), &start_args(&["--wait"]), &token(), None);
    assert_eq!(o.status.code(), Some(4));
    assert_eq!(
        server.join().unwrap().len(),
        1,
        "no polling after a refused start"
    );
}

#[test]
fn report_markdown_prints_the_text_body() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![Reply::text(
        200,
        "text/markdown; charset=utf-8",
        MARKDOWN,
    )]);
    let o = cli(
        &host,
        home.path(),
        &["run", "report", EXE, "--format", "md"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), MARKDOWN);
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/executions/{EXE}/report?format=markdown")
    );
    assert!(
        header(&seen[0], "accept")
            .unwrap()
            .contains("text/markdown")
    );
}

#[test]
fn report_json_format_and_errors() {
    let home = tempfile::tempdir().unwrap();
    let report = json!({"header":{"title":"Deploy","status":"succeeded"},"sections":[]});
    let (host, server) = serve(vec![
        Reply::json(200, report.clone()),
        Reply::json(404, json!({"code":"resource.not_found"})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &["run", "report", EXE, "--format", "json"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap(), report);
    let o = cli(
        &host,
        home.path(),
        &["run", "report", EXE, "--format", "markdown"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("resource.not_found"));
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/executions/{EXE}/report?format=json")
    );
    let o = cli(
        &host,
        home.path(),
        &["run", "report", EXE, "--format", "docx"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(2));
}

const JUNIT: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n</testsuites>\n";
const PDF: &str = "%PDF-1.7\n%fake body for the CLI\n%%EOF\n";

/// REP2: `--format junit` prints the XML body as served.
#[test]
fn report_junit_prints_the_xml_body() {
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![Reply::text(200, "application/xml", JUNIT)]);
    let o = cli(
        &host,
        home.path(),
        &["run", "report", EXE, "--format", "junit"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), JUNIT);
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/executions/{EXE}/report?format=junit")
    );
    assert!(
        header(&seen[0], "accept")
            .unwrap()
            .contains("application/xml")
    );
}

/// REP2: `--out` writes the exact bytes to a file and nothing to stdout;
/// without it a PDF goes to stdout only when stdout is not a terminal (as
/// here, a pipe).
#[test]
fn report_pdf_goes_to_a_file_or_a_pipe() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("run-report.pdf");
    let (host, server) = serve(vec![
        Reply::text(200, "application/pdf", PDF),
        Reply::text(200, "application/pdf", PDF),
        Reply::text(200, "application/xml", JUNIT),
        Reply::json(404, json!({"code":"resource.not_found"})),
    ]);
    let o = cli(
        &host,
        home.path(),
        &[
            "run",
            "report",
            EXE,
            "--format",
            "pdf",
            "--out",
            file.to_str().unwrap(),
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(std::fs::read(&file).unwrap(), PDF.as_bytes());
    assert!(o.stdout.is_empty(), "nothing on stdout with --out");
    let o = cli(
        &host,
        home.path(),
        &["run", "report", EXE, "--format", "pdf"],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(o.stdout, PDF.as_bytes());
    let xml = home.path().join("junit.xml");
    let o = cli(
        &host,
        home.path(),
        &[
            "run",
            "report",
            EXE,
            "--format",
            "junit",
            "--out",
            xml.to_str().unwrap(),
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(std::fs::read_to_string(&xml).unwrap(), JUNIT);
    // A failed request writes no file.
    let missing = home.path().join("missing.pdf");
    let o = cli(
        &host,
        home.path(),
        &[
            "run",
            "report",
            EXE,
            "--format",
            "pdf",
            "--out",
            missing.to_str().unwrap(),
        ],
        &token(),
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("resource.not_found"), "{}", err(&o));
    assert!(!missing.exists());
    let seen = server.join().unwrap();
    assert_eq!(
        line(&seen[0]),
        format!("GET /v1/executions/{EXE}/report?format=pdf")
    );
    assert!(
        header(&seen[0], "accept")
            .unwrap()
            .contains("application/pdf")
    );
}
