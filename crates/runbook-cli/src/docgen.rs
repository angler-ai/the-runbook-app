//! DOCS1: the machine-readable reference the docs site renders. `runbook
//! __docs-reference` (hidden, see `main.rs`) prints it as JSON: every command
//! and argument from the CLI's own clap definitions, and every MCP tool,
//! resource template and prompt from `runbook-mcp`. `site/scripts/generate.mjs`
//! turns it into Markdown, so the generated pages can't drift from the code.
use clap::{Arg, ArgAction, Command};
use serde_json::{Value, json};

/// The whole reference: `{ "cli": <command tree>, "mcp": {...} }`.
pub fn reference(cli: &Command) -> Value {
    json!({
        "cli": command(cli, &[]),
        "mcp": {
            "tools": runbook_mcp::tool_definitions(),
            "resource_templates": runbook_mcp::catalog::resource_templates(),
            "prompts": runbook_mcp::catalog::list_prompts(),
        },
    })
}

fn text(s: Option<&clap::builder::StyledStr>) -> Option<String> {
    s.map(|s| s.to_string()).filter(|s| !s.is_empty())
}

fn command(cmd: &Command, parents: &[&str]) -> Value {
    let mut path: Vec<&str> = parents.to_vec();
    path.push(cmd.get_name());
    let args: Vec<Value> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set())
        .filter(|a| !matches!(a.get_id().as_str(), "help" | "version"))
        .map(arg)
        .collect();
    let subcommands: Vec<Value> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
        .map(|c| command(c, &path))
        .collect();
    json!({
        "name": cmd.get_name(),
        "path": path.join(" "),
        "about": text(cmd.get_about()),
        "long_about": text(cmd.get_long_about()),
        "aliases": cmd.get_visible_aliases().collect::<Vec<_>>(),
        "args": args,
        "subcommands": subcommands,
    })
}

fn arg(a: &Arg) -> Value {
    let takes_value = !matches!(
        a.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count | ArgAction::Help
    );
    let values = a
        .get_value_names()
        .map(|v| v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![a.get_id().as_str().to_uppercase()]);
    let possible: Vec<String> = if a.is_hide_possible_values_set() {
        vec![]
    } else {
        a.get_possible_values()
            .iter()
            .filter(|p| !p.is_hide_set())
            .map(|p| p.get_name().to_string())
            .collect()
    };
    let defaults: Vec<String> = if a.is_hide_default_value_set() {
        vec![]
    } else {
        a.get_default_values()
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect()
    };
    json!({
        "id": a.get_id().as_str(),
        "long": a.get_long(),
        "short": a.get_short().map(|c| c.to_string()),
        "positional": a.is_positional(),
        "takes_value": takes_value,
        "value_names": values,
        "multiple": matches!(a.get_action(), ArgAction::Append)
            || a.get_num_args().is_some_and(|n| n.max_values() > 1),
        "required": a.is_required_set(),
        "global": a.is_global_set(),
        "help": text(a.get_long_help().or(a.get_help())),
        "env": if a.is_hide_env_set() { None } else { a.get_env().map(|e| e.to_string_lossy().into_owned()) },
        "default": defaults,
        "possible_values": possible,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser, Subcommand};

    #[derive(Parser)]
    #[command(name = "demo")]
    struct Demo {
        #[arg(long, global = true, help = "Host URL")]
        host: Option<String>,
        #[command(subcommand)]
        cmd: Sub,
    }

    #[derive(Subcommand)]
    enum Sub {
        #[command(about = "Do it")]
        Go {
            #[arg(help = "Target")]
            target: String,
            #[arg(long, default_value_t = 3)]
            times: u32,
            #[arg(long, hide = true)]
            secret: bool,
        },
        #[command(hide = true)]
        Hidden,
    }

    #[test]
    fn walks_commands_and_skips_hidden_items() {
        let v = command(&Demo::command(), &[]);
        assert_eq!(v["path"], "demo");
        assert_eq!(v["args"][0]["long"], "host");
        assert_eq!(v["args"][0]["global"], true);
        let subs = v["subcommands"].as_array().unwrap();
        assert_eq!(subs.len(), 1, "hidden and help subcommands are skipped");
        let go = &subs[0];
        assert_eq!(go["path"], "demo go");
        assert_eq!(go["about"], "Do it");
        let ids: Vec<&str> = go["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["target", "times"], "hidden args are skipped");
        assert_eq!(go["args"][0]["positional"], true);
        assert_eq!(go["args"][0]["required"], true);
        assert_eq!(go["args"][1]["default"][0], "3");
    }
}
