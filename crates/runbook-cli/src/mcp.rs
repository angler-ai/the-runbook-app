//! `runbook mcp serve`: the stdio MCP server (PRD §11) over the CLI's
//! resolved host and credentials. Requests go through `CliBackend`, so the
//! server shares the CLI's credential precedence, stored execution pairs and
//! serialized refresh; no tool takes or returns a credential.
use crate::agent::Env;
use crate::host::CliBackend;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum McpCommands {
    #[command(about = "Run a stdio MCP server using the current context")]
    Serve,
}

pub fn handle(env: Env, command: McpCommands) -> i32 {
    match command {
        McpCommands::Serve => {
            let mut backend = CliBackend::new(env);
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            match runbook_mcp::serve(&mut backend, stdin.lock(), stdout.lock()) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
    }
}
