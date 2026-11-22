use super::{Args, object, ops, pattern, s, string_list, tool};
use crate::backend::{AsyncBackend, BackendError, HttpCall};
use crate::render::Outcome;
use serde_json::{Value, json};

const CATALOGS: &[(&str, &str, &str)] = &[
    ("provider", "providers", "model provider"),
    ("profile", "profiles", "model profile"),
    ("mcp_server", "mcp-servers", "upstream MCP server"),
    ("group", "groups", "runner group"),
];
const ACTIONS: &[(&str, &str, bool, bool)] = &[
    ("list", "GET", false, false),
    ("get", "GET", true, false),
    ("create", "POST", false, true),
    ("update", "PATCH", true, true),
    ("delete", "DELETE", true, false),
];

const ORG: &str = "^org_[0-7][0-9A-HJKMNP-TV-Z]{25}$";
const GROUP: &str = "^rg_[0-7][0-9A-HJKMNP-TV-Z]{25}$";

struct CatalogTool {
    name: String,
    verb: &'static str,
    method: &'static str,
    resource: &'static str,
    description: &'static str,
    id: bool,
    body: bool,
}

fn catalog_tools() -> impl Iterator<Item = CatalogTool> {
    CATALOGS.iter().flat_map(|&(noun, resource, description)| {
        ACTIONS.iter().filter_map(move |&(verb, method, id, body)| {
            if noun == "group" && matches!(verb, "create" | "delete") {
                return None;
            }
            let name = format!(
                "{verb}_runner_{noun}{}",
                if verb == "list" { "s" } else { "" }
            );
            Some(CatalogTool {
                name,
                verb,
                method,
                resource,
                description,
                id,
                body,
            })
        })
    })
}

pub(super) fn definitions() -> Vec<Value> {
    let install = || s("Runner install ID from runner_status");
    let label = || json!({"enum":["read","write","destructive"]});
    let mut definitions = Vec::new();
    for CatalogTool {
        name,
        verb,
        method,
        resource,
        description,
        id,
        body,
    } in catalog_tools()
    {
        let scope = if method == "GET" {
            "runner:read"
        } else {
            "runner:admin"
        };
        let mut about = format!("{verb} {description} configuration. Requires {scope}.");
        let mut properties = json!({"install_id": install()});
        let mut required = vec!["install_id"];
        if id {
            properties["id"] = s("Catalog entry ID");
            required.push("id");
        }
        if body {
            properties["change"] = object("Public configuration for this catalog entry");
            required.push("change");
            if resource == "providers" {
                about.push_str(" Refer to a model-provider credential by ID. Use runbook runner credential set for API keys.");
            }
        }
        let mut definition = tool(&name, &about, method == "GET", properties, &required);
        definition["annotations"]["destructiveHint"] = json!(method == "DELETE");
        definitions.push(definition);
    }
    definitions.extend([
        tool("get_runner_policy", "Read runner policy and budgets. Requires runner:read.",
            true, json!({"install_id": install()}), &["install_id"]),
        tool("update_runner_policy", "Update runner policy, budgets and project limits. Requires runner:admin.",
            false, json!({"install_id": install(), "change":object("Public runner policy fields")}),
            &["install_id", "change"]),
        tool("set_runner_group_projects", "Replace the projects allowed in a runner group. Requires a human org admin with runner:admin.",
            false, json!({"org_id":pattern(ORG, "Organization ID"), "group_id":pattern(GROUP, "Runner group ID"),
                "project_ids":string_list("Full list of allowed project IDs")}),
            &["org_id", "group_id", "project_ids"]),
        tool("discover_runner_mcp_tools", "Discover tools on the configured upstream MCP server. Requires runner:admin.",
            false, json!({"install_id": install(), "id":s("MCP server ID")}), &["install_id", "id"]),
        tool("pin_runner_mcp_snapshot", "Pin a discovered MCP tool snapshot. Requires runner:admin.",
            false, json!({"install_id": install(), "id":s("MCP server ID"), "snapshot_id":s("Discovered snapshot ID")}),
            &["install_id", "id", "snapshot_id"]),
        tool("label_runner_mcp_tools", "Replace the tool labels for an MCP server. Requires runner:admin.",
            false, json!({"install_id": install(), "id":s("MCP server ID"), "tool_labels":object("Map of tool names to read, write or destructive")}),
            &["install_id", "id", "tool_labels"]),
        tool("list_runner_credential_types", "List credential types and their secret fields. Requires runner:read.",
            true, json!({"install_id": install()}), &["install_id"]),
        tool("label_runner_credential", "Set a credential label through the vault. Requires a human org admin with runner:admin.",
            false, json!({"install_id": install(), "credential_id":s("Credential ID"), "label":label()}),
            &["install_id", "credential_id", "label"]),
        tool("share_runner_credential", "Replace project sharing for a credential through the vault. Requires runner:admin.",
            false, json!({"install_id": install(), "credential_id":s("Credential ID"),
                "projects":{"oneOf":[{"enum":["none","all"]},string_list("Full array of shared project IDs")]}}),
            &["install_id", "credential_id", "projects"]),
        tool("set_runner_agent_profiles", "Replace the profiles an agent may use. Requires runner:admin. Widening access requires human review.",
            false, json!({"install_id": install(), "project_id":s("Project ID"), "agent_id":s("Agent ID"),
                "profiles":string_list("Full list of allowed profile IDs")}),
            &["install_id", "project_id", "agent_id", "profiles"]),
    ]);
    definitions
}

type Operation = (&'static str, String, Value);

pub(super) fn proxy_operation(name: &str, a: &Args<'_>) -> Result<Operation, BackendError> {
    let id = || ops::runner_segment(a.str("id"));
    let credential =
        || ops::runner_segment(a.str("credential_id")).map(|id| format!("credentials/{id}"));
    if let Some(CatalogTool {
        method,
        resource,
        id: has_id,
        body: has_body,
        ..
    }) = catalog_tools().find(|tool| tool.name == name)
    {
        let path = if has_id {
            format!("{resource}/{}", id()?)
        } else {
            resource.into()
        };
        let body = if has_body {
            let body = a.object("change");
            ops::check_runner_metadata(resource, &body)?;
            body
        } else {
            Value::Null
        };
        return Ok((method, path, body));
    }
    Ok(match name {
        "get_runner_policy" => ("GET", "policy".into(), Value::Null),
        "update_runner_policy" => ("PUT", "policy".into(), a.object("change")),
        "discover_runner_mcp_tools" => {
            ("POST", format!("mcp-servers/{}/discover", id()?), json!({}))
        }
        "pin_runner_mcp_snapshot" => (
            "POST",
            format!("mcp-servers/{}/pin", id()?),
            json!({"snapshot_id":ops::runner_segment(a.str("snapshot_id"))?}),
        ),
        "label_runner_mcp_tools" => {
            let labels = a.object("tool_labels");
            ops::check_runner_tool_labels(&labels)?;
            (
                "PATCH",
                format!("mcp-servers/{}", id()?),
                json!({"tool_labels":labels}),
            )
        }
        "list_runner_credential_types" => ("GET", "credentials".into(), Value::Null),
        "label_runner_credential" => ("PATCH", credential()?, json!({"label":a.str("label")})),
        "share_runner_credential" => {
            let projects = a.value("projects");
            ops::check_runner_sharing(&projects)?;
            (
                "PATCH",
                credential()?,
                json!({"sharing":{"projects":projects}}),
            )
        }
        "set_runner_agent_profiles" => {
            let project = ops::runner_segment(a.str("project_id"))?;
            let agent = ops::runner_segment(a.str("agent_id"))?;
            let profiles = a.value("profiles");
            (
                "PATCH",
                format!("projects/{project}/agents/{agent}"),
                json!({"profiles":profiles}),
            )
        }
        _ => return Err(BackendError::Usage("unknown runner tool".into())),
    })
}

pub(super) async fn set_group_projects<B: AsyncBackend + ?Sized>(
    b: &mut B,
    a: &Args<'_>,
) -> Result<Outcome, BackendError> {
    let org = ops::runner_segment(a.str("org_id"))?;
    let group = ops::runner_segment(a.str("group_id"))?;
    let body = json!({"project_ids":a.value("project_ids")});
    let request = HttpCall::write(
        "PUT",
        format!("/v1/orgs/{org}/runner/groups/{group}/projects"),
        body,
        b.random_token()?,
    );
    let reply = b.call(request).await?;
    Ok(Outcome::new(json!({"ok":true})).data(reply.body))
}
