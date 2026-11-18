//! UPD1: `runbook version` prints this CLI's release and the server's
//! release and schema from `/v1/meta` (public, no token sent).
mod support;
use serde_json::json;
use support::*;

fn meta() -> serde_json::Value {
    json!({
        "edition": "self_hosted",
        "version": "9.8.7",
        "schema": {"postgres": 31},
        "capabilities": {},
        "limits": {"max_request_bytes": 1, "source_bytes": null, "skill_bundle_bytes": null, "uploads": null},
        "usage": null
    })
}

#[test]
fn version_reports_the_cli_and_the_server_release() {
    let home = tempfile::tempdir().unwrap();
    let (host, seen) = serve(vec![reply(200, meta())]);
    let o = cli(&host, home.path(), &["version", "--json"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let body: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(body["cli"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["server"]["version"], "9.8.7");
    assert_eq!(body["server"]["schema"]["postgres"], 31);
    assert_eq!(body["server"]["edition"], "self_hosted");
    let requests = seen.join().unwrap();
    assert_eq!(request_line(&requests[0]), "GET /v1/meta HTTP/1.1");
    assert!(header(&requests[0], "authorization").is_none());

    let (host, _) = serve(vec![reply(200, meta())]);
    let o = cli(&host, home.path(), &["version"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&o),
        format!(
            "runbook {}\nserver 9.8.7 (self_hosted, schema postgres 31) at {host}\n",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn version_still_prints_the_cli_release_when_the_server_is_unreachable() {
    let home = tempfile::tempdir().unwrap();
    let o = cli("http://127.0.0.1:9", home.path(), &["version"], &[]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), format!("runbook {}\n", env!("CARGO_PKG_VERSION")));
    assert!(
        err(&o).contains("server version unavailable"),
        "{}",
        err(&o)
    );
}
