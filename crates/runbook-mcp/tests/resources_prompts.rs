//! MCP1 (F68): runbooks as MCP resources and the `run_runbook` prompt,
//! against a scripted backend. Both go through the same `Backend` as the
//! tools, so stdio and `/mcp` answer them alike.
use runbook_mcp::backend::{Backend, BackendError, HttpCall, HttpReply};
use runbook_mcp::handle_message;
use serde_json::{Value, json};

const ORG: &str = "org_01JABCDEFGHJKMNPQRSTVWXYZ0";
const PRJ: &str = "prj_01JABCDEFGHJKMNPQRSTVWXYZ1";
const RB: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ4";
const RB2: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ5";
const RB3: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ7";
const RBV: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXYZ6";

#[derive(Default)]
struct Fake {
    script: Vec<Result<HttpReply, BackendError>>,
    calls: Vec<HttpCall>,
}

impl Backend for Fake {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        self.calls.push(call);
        assert!(
            !self.script.is_empty(),
            "unexpected call {:?}",
            self.calls.last()
        );
        self.script.remove(0)
    }
    fn random_token(&mut self) -> Result<String, BackendError> {
        Ok("key0001".into())
    }
}

fn fake(script: Vec<Result<HttpReply, BackendError>>) -> Fake {
    Fake {
        script,
        calls: vec![],
    }
}
fn ok(body: Value) -> Result<HttpReply, BackendError> {
    Ok(HttpReply { body, etag: None })
}
fn problem(status: u16, code: &str) -> Result<HttpReply, BackendError> {
    Err(BackendError::problem(status, code, ""))
}
fn rpc(b: &mut Fake, method: &str, params: Value) -> Value {
    handle_message(
        b,
        &json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
    )
    .expect("a request gets a response")
}

fn catalog_page() -> Value {
    json!({"items": [
        {"runbook_id": RB, "slug": "deploy", "title": "Deploy", "origin": "api", "archived": false, "latest_version": 2},
        {"runbook_id": RB2, "slug": "draft-only", "title": "Draft", "origin": "api", "archived": false, "latest_version": null},
        {"runbook_id": RB3, "slug": "old", "title": "Old", "origin": "api", "archived": true, "latest_version": 1}
    ], "next_cursor": null})
}

#[test]
fn initialize_advertises_resources_and_prompts() {
    let mut b = fake(vec![]);
    let r = rpc(
        &mut b,
        "initialize",
        json!({"protocolVersion": "2025-06-18"}),
    );
    let caps = &r["result"]["capabilities"];
    assert_eq!(caps["tools"]["listChanged"], false);
    assert_eq!(caps["resources"]["listChanged"], false);
    assert_eq!(caps["resources"]["subscribe"], false);
    assert_eq!(caps["prompts"]["listChanged"], false);
}

#[test]
fn resources_list_walks_orgs_projects_and_runbooks_and_keeps_published_ones() {
    let mut b = fake(vec![
        ok(json!({"items": [{"id": ORG, "title": "Org"}], "next_cursor": null})),
        ok(
            json!({"items": [{"id": PRJ, "org_id": ORG, "slug": "default", "title": "Default"}], "next_cursor": null}),
        ),
        ok(catalog_page()),
    ]);
    let r = rpc(&mut b, "resources/list", json!({}));
    let resources = r["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1, "{r}");
    let item = &resources[0];
    assert_eq!(item["uri"], format!("runbook://runbooks/{RB}"));
    assert_eq!(item["name"], "deploy");
    assert_eq!(item["title"], "Deploy");
    assert_eq!(item["mimeType"], "application/yaml");
    assert!(item["description"].as_str().unwrap().contains("default"));
    assert!(r["result"].get("nextCursor").is_none());
    let paths: Vec<&str> = b.calls.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/v1/orgs".to_owned(),
            format!("/v1/orgs/{ORG}/projects"),
            format!("/v1/projects/{PRJ}/runbooks")
        ]
    );
    assert!(b.calls.iter().all(|c| c.method == "GET"));
}

#[test]
fn resources_list_is_empty_for_a_credential_that_reads_no_catalog() {
    // An execution credential is refused by the directory routes.
    let mut b = fake(vec![problem(403, "auth.forbidden")]);
    let r = rpc(&mut b, "resources/list", json!({}));
    assert_eq!(r["result"]["resources"], json!([]), "{r}");
}

#[test]
fn resources_list_skips_projects_the_credential_cannot_read() {
    let mut b = fake(vec![
        ok(json!({"items": [{"id": ORG, "title": "Org"}], "next_cursor": null})),
        ok(json!({"items": [
            {"id": PRJ, "org_id": ORG, "slug": "hidden", "title": "Hidden"},
            {"id": "prj_01JABCDEFGHJKMNPQRSTVWXYZ2", "org_id": ORG, "slug": "default", "title": "Default"}
        ], "next_cursor": null})),
        problem(403, "auth.scope_missing"),
        ok(catalog_page()),
    ]);
    let r = rpc(&mut b, "resources/list", json!({}));
    assert_eq!(r["result"]["resources"].as_array().unwrap().len(), 1, "{r}");
}

#[test]
fn resources_list_rejects_a_bad_cursor() {
    let mut b = fake(vec![]);
    let r = rpc(&mut b, "resources/list", json!({"cursor": "nope"}));
    assert_eq!(r["error"]["code"], -32602, "{r}");
}

#[test]
fn resources_read_returns_the_published_source() {
    let mut b = fake(vec![ok(json!({
        "runbook_id": RB, "runbook_version_id": RBV, "runbook_version": 2,
        "source": "apiVersion: therunbook.app/v1\n", "semantic_hash": "h", "snapshot_digest": "d"
    }))]);
    let uri = format!("runbook://runbooks/{RB}");
    let r = rpc(&mut b, "resources/read", json!({"uri": uri}));
    let contents = &r["result"]["contents"];
    assert_eq!(contents[0]["uri"], uri);
    assert_eq!(contents[0]["mimeType"], "application/yaml");
    assert_eq!(contents[0]["text"], "apiVersion: therunbook.app/v1\n");
    assert_eq!(contents[0]["_meta"]["runbook_version_id"], RBV);
    assert_eq!(contents[0]["_meta"]["runbook_version"], 2);
    assert_eq!(
        b.calls[0].path,
        format!("/v1/runbooks/{RB}/versions/latest")
    );
}

#[test]
fn resources_read_reports_unknown_uris_and_missing_runbooks() {
    let mut b = fake(vec![problem(404, "resource.not_found")]);
    let r = rpc(
        &mut b,
        "resources/read",
        json!({"uri": format!("runbook://runbooks/{RB}")}),
    );
    assert_eq!(r["error"]["code"], -32002, "{r}");
    for uri in [
        "runbook://runbooks/../../v1/orgs",
        "file:///etc/passwd",
        "runbook://runbooks/rb_x",
    ] {
        let mut b = fake(vec![]);
        let r = rpc(&mut b, "resources/read", json!({"uri": uri}));
        assert_eq!(r["error"]["code"], -32602, "{uri}: {r}");
    }
    let mut b = fake(vec![problem(403, "auth.forbidden")]);
    let r = rpc(
        &mut b,
        "resources/read",
        json!({"uri": format!("runbook://runbooks/{RB}")}),
    );
    assert_eq!(r["error"]["code"], -32603, "{r}");
    assert_eq!(r["error"]["data"]["code"], "auth.forbidden");
    assert_eq!(r["error"]["data"]["status"], 403);
}

#[test]
fn resource_templates_name_the_uri_scheme() {
    let mut b = fake(vec![]);
    let r = rpc(&mut b, "resources/templates/list", json!({}));
    assert_eq!(
        r["result"]["resourceTemplates"][0]["uriTemplate"],
        "runbook://runbooks/{runbook_id}"
    );
}

#[test]
fn prompts_list_offers_run_runbook() {
    let mut b = fake(vec![]);
    let r = rpc(&mut b, "prompts/list", json!({}));
    let prompts = r["result"]["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["name"], "run_runbook");
    let args: Vec<(&str, bool)> = prompts[0]["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["name"].as_str().unwrap(), a["required"] == true))
        .collect();
    assert_eq!(
        args,
        [("runbook", true), ("project", true), ("inputs", false)]
    );
}

#[test]
fn run_runbook_resolves_a_slug_and_gives_the_loop() {
    let mut b = fake(vec![ok(catalog_page())]);
    let r = rpc(
        &mut b,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": "deploy", "project": "default", "inputs": "{\"service\":\"api\"}"}}),
    );
    let messages = r["result"]["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 1, "{r}");
    assert_eq!(messages[0]["role"], "user");
    let text = messages[0]["content"]["text"].as_str().unwrap();
    // The start call, with the resolved slug, the inputs and a fresh key.
    let start = json!({"project": "default", "runbook": "deploy", "inputs": {"service": "api"}, "idempotency_key": "key0001"});
    assert!(text.contains(&start.to_string()), "{text}");
    assert!(text.contains(RB));
    // The loop as SKILL.md teaches it.
    for needle in [
        "start_execution",
        "get_next_steps",
        "claim_step",
        "get_step",
        "append_log",
        "heartbeat_step",
        "report_step",
        "request_human",
        "claim_ready_work",
        "wait",
        "exit",
        "credentials.revoked",
        "reconcile",
        "verdict",
        "RUNBOOK DATA",
        "runbook whoami --json",
        "idempotency",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    assert_eq!(b.calls[0].path, "/v1/projects/default/runbooks");
}

#[test]
fn run_runbook_resolves_an_id_to_its_slug() {
    let mut b = fake(vec![
        ok(
            json!({"runbook_id": RB, "slug": "deploy", "title": "Deploy", "origin": "api", "archived": false, "latest_version": 2}),
        ),
        ok(catalog_page()),
    ]);
    let r = rpc(
        &mut b,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": RB, "project": "default"}}),
    );
    let text = r["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    let start = json!({"project": "default", "runbook": "deploy", "inputs": {}, "idempotency_key": "key0001"});
    assert!(text.contains(&start.to_string()), "{text}");
    assert_eq!(b.calls[0].path, format!("/v1/runbooks/{RB}"));
}

#[test]
fn run_runbook_refuses_bad_arguments_and_unpublished_runbooks() {
    for arguments in [
        json!({"project": "default"}),
        json!({"runbook": "deploy"}),
        json!({"runbook": "deploy", "project": "default", "inputs": "[1]"}),
        json!({"runbook": "deploy", "project": "default", "inputs": "{not json"}),
        json!({"runbook": "../x", "project": "default"}),
    ] {
        let mut b = fake(vec![]);
        let r = rpc(
            &mut b,
            "prompts/get",
            json!({"name": "run_runbook", "arguments": arguments}),
        );
        assert_eq!(r["error"]["code"], -32602, "{arguments}: {r}");
    }
    let mut b = fake(vec![]);
    let r = rpc(&mut b, "prompts/get", json!({"name": "nope"}));
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // Draft only: nothing to start.
    let mut b = fake(vec![ok(catalog_page())]);
    let r = rpc(
        &mut b,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": "draft-only", "project": "default"}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // Unknown slug: an invalid argument too.
    let mut b = fake(vec![ok(catalog_page())]);
    let r = rpc(
        &mut b,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": "missing", "project": "default"}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // Any other API refusal is reported with its problem code.
    let mut b = fake(vec![problem(403, "auth.scope_missing")]);
    let r = rpc(
        &mut b,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": "deploy", "project": "default"}}),
    );
    assert_eq!(r["error"]["code"], -32603, "{r}");
    assert_eq!(r["error"]["data"]["code"], "auth.scope_missing");
}

#[test]
fn a_401_mid_walk_is_an_error_not_an_empty_list() {
    // The credential was valid for the org list, then stopped working.
    let mut b = fake(vec![
        ok(json!({"items": [{"id": ORG, "title": "Org"}], "next_cursor": null})),
        problem(401, "credentials.revoked"),
    ]);
    let r = rpc(&mut b, "resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32603, "{r}");
    assert_eq!(r["error"]["data"]["status"], 401);
    // And a 401 on the first call too.
    let mut b = fake(vec![problem(401, "auth.invalid_credential")]);
    let r = rpc(&mut b, "resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32603, "{r}");
}

#[test]
fn run_runbook_by_id_must_belong_to_the_named_project() {
    // The ID resolves, but project `other` has no runbook with that ID.
    let mut b = fake(vec![
        ok(
            json!({"runbook_id": RB, "slug": "deploy", "title": "Deploy", "origin": "api", "archived": false, "latest_version": 2}),
        ),
        ok(json!({"items": [
            {"runbook_id": RB2, "slug": "deploy", "title": "Deploy elsewhere", "origin": "api", "archived": false, "latest_version": 1}
        ], "next_cursor": null})),
    ]);
    let r = rpc(
        &mut b,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": RB, "project": "other"}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
    assert_eq!(b.calls[1].path, "/v1/projects/other/runbooks");
    // Not in the project at all.
    let mut b = fake(vec![
        ok(
            json!({"runbook_id": RB, "slug": "deploy", "title": "Deploy", "origin": "api", "archived": false, "latest_version": 2}),
        ),
        ok(json!({"items": [], "next_cursor": null})),
    ]);
    let r = rpc(
        &mut b,
        "prompts/get",
        json!({"name": "run_runbook", "arguments": {"runbook": RB, "project": "other"}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
}

/// A live catalog: one org, one project, and runbooks that can change
/// between two `resources/list` pages.
struct Live {
    ids: Vec<String>,
}
impl Backend for Live {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        let body = match call.path.as_str() {
            "/v1/orgs" => json!({"items": [{"id": ORG, "title": "Org"}], "next_cursor": null}),
            p if p == format!("/v1/orgs/{ORG}/projects") => {
                json!({"items": [{"id": PRJ, "org_id": ORG, "slug": "default", "title": "Default"}], "next_cursor": null})
            }
            _ => json!({"items": self.ids.iter().map(|id| json!({
                "runbook_id": id, "slug": id.to_lowercase().replace('_', "-"), "title": id,
                "origin": "api", "archived": false, "latest_version": 1
            })).collect::<Vec<_>>(), "next_cursor": null}),
        };
        Ok(HttpReply { body, etag: None })
    }
    fn random_token(&mut self) -> Result<String, BackendError> {
        Ok("k".into())
    }
}

fn rb_id(n: u32) -> String {
    format!("rb_01JABCDEFGHJKMNPQRST{n:06}")
}

#[test]
fn the_resources_cursor_survives_catalog_changes_between_pages() {
    let mut live = Live {
        ids: (100..250).map(rb_id).collect(),
    };
    let first = rpc_on(&mut live, json!({}));
    let page1: Vec<String> = uris(&first);
    assert_eq!(page1.len(), 100);
    let cursor = first["result"]["nextCursor"].clone();
    assert!(cursor.is_string(), "{first}");
    // Before page 2: a runbook sorting before the cursor is added, and two
    // already listed are deleted (an offset would now skip one).
    live.ids.insert(0, rb_id(1));
    live.ids.retain(|id| *id != rb_id(100) && *id != rb_id(101));
    let second = rpc_on(&mut live, json!({"cursor": cursor}));
    let page2: Vec<String> = uris(&second);
    assert!(second["result"].get("nextCursor").is_none(), "{second}");
    // Nothing repeated and nothing that existed throughout was skipped.
    for uri in &page2 {
        assert!(!page1.contains(uri), "{uri} listed twice");
    }
    let seen: Vec<&String> = page1.iter().chain(&page2).collect();
    for n in 102..250 {
        let uri = format!("runbook://runbooks/{}", rb_id(n));
        assert!(seen.contains(&&uri), "{uri} skipped");
    }
    // A malformed cursor is invalid params.
    let bad = rpc_on(&mut live, json!({"cursor": "org/../x"}));
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
}

fn rpc_on(b: &mut Live, params: Value) -> Value {
    handle_message(
        b,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list", "params": params}),
    )
    .unwrap()
}
fn uris(r: &Value) -> Vec<String> {
    r["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|x| x["uri"].as_str().unwrap().to_owned())
        .collect()
}
