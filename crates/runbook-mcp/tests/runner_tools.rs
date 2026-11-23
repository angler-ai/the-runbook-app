use runbook_mcp::backend::{Backend, BackendError, HttpCall, HttpReply};
use runbook_mcp::{call_tool, tool_definitions};
use serde_json::{Value, json};

#[derive(Default)]
struct Runner {
    reply: Value,
    calls: Vec<HttpCall>,
}

impl Backend for Runner {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        self.calls.push(call);
        Ok(HttpReply {
            body: self.reply.clone(),
            etag: None,
        })
    }

    fn random_token(&mut self) -> Result<String, BackendError> {
        Ok("nonce".into())
    }

    fn web_origin(&self) -> Option<String> {
        Some("https://runbook.example".into())
    }
}

#[test]
fn mcp_credential_tool_never_accepts_value() {
    let tool = tool_definitions()
        .into_iter()
        .find(|def| def["name"] == "create_runner_credential")
        .unwrap();
    let schema = &tool["inputSchema"];
    assert!(schema["properties"].get("value").is_none());
    assert_eq!(schema["additionalProperties"], false);
    let mut runner = Runner {
        reply: json!({"id":"cred-1","status":"needs_secret"}),
        ..Default::default()
    };
    let mut args = json!({"install_id":"ri_1","name":"MCP","type":"mcp-bearer","config":{"url":"https://example.com/mcp"}});
    args["value"] = json!("unexpected input");
    let refused = call_tool(&mut runner, "create_runner_credential", &args);
    assert_eq!(refused["isError"], true);
    assert!(runner.calls.is_empty());
    args.as_object_mut().unwrap().remove("value");
    let result = call_tool(&mut runner, "create_runner_credential", &args);
    assert_eq!(runner.calls.len(), 1);
    assert_eq!(
        runner.calls[0].path,
        "/v1/runners/ri_1/proxy/runner/v1/credentials"
    );
    assert_eq!(
        runner.calls[0].body.as_ref().unwrap(),
        &json!({"name":"MCP","type":"mcp-bearer","config":{"url":"https://example.com/mcp"}})
    );
    let result = result.to_string();
    assert!(result.contains("https://runbook.example/settings/organization"));
    assert!(result.contains("runbook runner credential set cred-1 token --install ri_1"));
}

#[test]
fn mcp_widening_change_returns_proposal_id() {
    let mut runner = Runner {
        reply: json!({"id":"prop-1","expires_at":"2099-01-01T00:00:00Z"}),
        ..Default::default()
    };
    let result = call_tool(
        &mut runner,
        "update_runner_agent",
        &json!({
            "install_id":"ri_1","project_id":"prj_1","agent_id":"agent-1","change":{"credentials":["cred-1"]}
        }),
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"proposal_id\":\"prop-1\""), "{text}");
    assert_eq!(runner.calls[0].method, "PATCH");
    assert_eq!(
        runner.calls[0].path,
        "/v1/runners/ri_1/proxy/runner/v1/projects/prj_1/agents/agent-1"
    );
}

#[test]
fn configuration_tools_use_runner_admin_routes() {
    let cases = [
        ("get_runner_policy", "GET", "policy", json!({}), Value::Null),
        (
            "update_runner_policy",
            "PUT",
            "policy",
            json!({"change":{"org_concurrency":2}}),
            json!({"org_concurrency":2}),
        ),
        (
            "list_runner_groups",
            "GET",
            "groups",
            json!({}),
            Value::Null,
        ),
        (
            "get_runner_group",
            "GET",
            "groups/fixture",
            json!({"id":"fixture"}),
            Value::Null,
        ),
        (
            "update_runner_group",
            "PATCH",
            "groups/fixture",
            json!({"id":"fixture", "change":{"concurrency":2}}),
            json!({"concurrency":2}),
        ),
        (
            "discover_runner_mcp_tools",
            "POST",
            "mcp-servers/mcp/discover",
            json!({"id":"mcp"}),
            json!({}),
        ),
        (
            "pin_runner_mcp_snapshot",
            "POST",
            "mcp-servers/mcp/pin",
            json!({"id":"mcp","snapshot_id":"fixture"}),
            json!({"snapshot_id":"fixture"}),
        ),
        (
            "label_runner_mcp_tools",
            "PATCH",
            "mcp-servers/mcp",
            json!({"id":"mcp","tool_labels":{"lookup":"read"}}),
            json!({"tool_labels":{"lookup":"read"}}),
        ),
        (
            "list_runner_credential_types",
            "GET",
            "credentials",
            json!({}),
            Value::Null,
        ),
        (
            "label_runner_credential",
            "PATCH",
            "credentials/cred",
            json!({"credential_id":"cred","label":"write"}),
            json!({"label":"write"}),
        ),
        (
            "share_runner_credential",
            "PATCH",
            "credentials/cred",
            json!({"credential_id":"cred","projects":["prj_1"]}),
            json!({"sharing":{"projects":["prj_1"]}}),
        ),
        (
            "set_runner_agent_profiles",
            "PATCH",
            "projects/prj_1/agents/agent",
            json!({"project_id":"prj_1","agent_id":"agent","profiles":["profile"]}),
            json!({"profiles":["profile"]}),
        ),
    ];
    let check = |name: &str, method: &str, path: &str, mut args: Value, expected: Value| {
        args["install_id"] = json!("ri_1");
        let mut runner = Runner::default();
        let result = call_tool(&mut runner, name, &args);
        assert_ne!(result["isError"], true, "{name}: {result}");
        let [call] = runner.calls.as_slice() else {
            panic!("{name}: {:?}", runner.calls)
        };
        assert_eq!(call.method, method);
        assert_eq!(
            call.path,
            format!("/v1/runners/ri_1/proxy/runner/v1/{path}")
        );
        if name == "list_runner_credential_types" {
            assert_eq!(call.query, [("presets".into(), "true".into())]);
        }
        if method != "DELETE" {
            assert_eq!(call.body.as_ref().unwrap_or(&Value::Null), &expected);
        }
    };
    for (name, method, path, args, expected) in cases {
        check(name, method, path, args, expected);
    }
    for (noun, resource) in [
        ("provider", "providers"),
        ("profile", "profiles"),
        ("mcp_server", "mcp-servers"),
    ] {
        let change = json!({"name":"fixture"});
        let entry = format!("{resource}/fixture");
        for (name, method, path, args, body) in [
            (
                format!("list_runner_{noun}s"),
                "GET",
                resource,
                json!({}),
                Value::Null,
            ),
            (
                format!("get_runner_{noun}"),
                "GET",
                &entry,
                json!({"id":"fixture"}),
                Value::Null,
            ),
            (
                format!("create_runner_{noun}"),
                "POST",
                resource,
                json!({"change":change}),
                change.clone(),
            ),
            (
                format!("update_runner_{noun}"),
                "PATCH",
                &entry,
                json!({"id":"fixture","change":change}),
                change.clone(),
            ),
            (
                format!("delete_runner_{noun}"),
                "DELETE",
                &entry,
                json!({"id":"fixture"}),
                Value::Null,
            ),
        ] {
            check(&name, method, path, args, body);
        }
    }

    let mut runner = Runner::default();
    let result = call_tool(
        &mut runner,
        "set_runner_group_projects",
        &json!({
            "org_id":"org_00000000000000000000000001", "group_id":"rg_00000000000000000000000001",
            "project_ids":["prj_00000000000000000000000001"],
        }),
    );
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(runner.calls[0].method, "PUT");
    assert_eq!(
        runner.calls[0].path,
        "/v1/orgs/org_00000000000000000000000001/runner/groups/rg_00000000000000000000000001/projects"
    );
    assert_eq!(
        runner.calls[0].body,
        Some(json!({"project_ids":["prj_00000000000000000000000001"]}))
    );
}

#[test]
fn provider_tools_reject_a_plaintext_secret_before_dispatch() {
    let mut runner = Runner::default();
    let result = call_tool(
        &mut runner,
        "create_runner_provider",
        &json!({
            "install_id": "ri_1", "change": {"name":"provider","api_key":"fixture"}
        }),
    );
    assert_eq!(result["isError"], true);
    assert!(runner.calls.is_empty());
    assert!(result.to_string().contains("runner credential set"));
}
