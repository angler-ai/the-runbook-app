//! Every MCP tool has a CLI command (PRD §10/§11): `references/cli.md` maps
//! each tool to a command, and that command (with its flags) exists in the
//! CLI's clap tree.
use super::Cli;
use clap::CommandFactory;

fn cli_md() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skills/therunbook/references/cli.md"
    );
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// The backticked `runbook …` command on the table row of `tool`.
fn command_for(md: &str, tool: &str) -> Option<String> {
    let cell = format!("`{tool}`");
    let row = md
        .lines()
        .find(|l| l.trim_start().starts_with('|') && l.contains(&cell))?;
    row.split('`')
        .skip(1)
        .step_by(2)
        .find(|span| span.starts_with("runbook "))
        .map(str::to_owned)
}

/// Walk the clap tree along the command words; check every `--flag`.
fn check(command: &str) -> Result<(), String> {
    let mut cmd = Cli::command();
    cmd.build();
    let words: Vec<&str> = command.split_whitespace().skip(1).collect();
    let mut walked = 0;
    for word in &words {
        if word.starts_with('<') || word.starts_with('-') || word.starts_with('[') {
            break;
        }
        cmd = cmd
            .find_subcommand(word)
            .cloned()
            .ok_or_else(|| format!("`{command}`: no subcommand '{word}'"))?;
        walked += 1;
    }
    if walked == 0 || cmd.has_subcommands() {
        return Err(format!("`{command}` does not name a leaf command"));
    }
    let flags = words
        .iter()
        .map(|w| w.trim_matches(|c| c == '[' || c == ']'))
        .filter(|w| w.starts_with("--"));
    for word in flags {
        let long = word
            .trim_start_matches("--")
            .split('=')
            .next()
            .unwrap_or("");
        let known = cmd.get_arguments().any(|a| a.get_long() == Some(long))
            || matches!(long, "host" | "context");
        if !known {
            return Err(format!("`{command}`: no flag --{long}"));
        }
    }
    Ok(())
}

/// MCP tools whose CLI commands a later package adds: MEM3 shipped the
/// memory tools, MEM4 adds `runbook memory` (plan 2026-11-12 §2.1). Their
/// rows must be mapped already; once the command exists this list must
/// lose the tool, so it cannot go stale.
const CLI_PENDING: &[&str] = &[];

#[test]
fn every_mcp_tool_has_cli_equivalent() {
    let md = cli_md();
    let mut problems = vec![];
    let tools = runbook_mcp::tool_definitions();
    assert!(!tools.is_empty());
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        match command_for(&md, name) {
            None => problems.push(format!("{name}: no CLI command in references/cli.md")),
            Some(command) if CLI_PENDING.contains(&name) => {
                if check(&command).is_ok() {
                    problems.push(format!(
                        "{name}: `{command}` exists; remove it from CLI_PENDING"
                    ));
                }
            }
            Some(command) => {
                if let Err(e) = check(&command) {
                    problems.push(format!("{name}: {e}"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_parity_check_rejects_unknown_commands_and_flags() {
    assert!(check("runbook step claim <sr> --session <id>").is_ok());
    assert!(check("runbook step teleport <sr>").is_err());
    assert!(check("runbook step").is_err());
    assert!(check("runbook step claim <sr> --no-such-flag").is_err());
    assert!(check("runbook push <file> --draft --project <p>").is_ok());
    assert!(check("runbook step claim <sr> [--nope <m>]").is_err());
}
