//! UPD1: `runbook version`: this CLI's release and the server's, from its
//! public `/v1/meta` (release and storage schema). No token is sent.
use clap::Args;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Args, Debug)]
pub struct VersionArgs {
    #[arg(long, help = "Print JSON")]
    pub json: bool,
}

/// Prints the CLI's release, then the server's. Exit 1 when the server's
/// cannot be read (the CLI's is still printed).
pub fn handle(host: &str, args: &VersionArgs) -> i32 {
    let cli = env!("CARGO_PKG_VERSION");
    let server = server_meta(host);
    if args.json {
        let server = server.as_ref().ok().map(|meta| {
            json!({
                "host": host,
                "version": meta.get("version").cloned().unwrap_or(Value::Null),
                "edition": meta.get("edition").cloned().unwrap_or(Value::Null),
                "schema": meta.get("schema").cloned().unwrap_or(Value::Null),
            })
        });
        println!("{}", json!({"cli": cli, "server": server}));
    } else {
        println!("runbook {cli}");
        if let Ok(meta) = &server {
            println!("server {} at {host}", describe(meta));
        }
    }
    match server {
        Ok(_) => 0,
        Err(reason) => {
            eprintln!("warning: server version unavailable from {host}: {reason}");
            1
        }
    }
}

/// `0.2.0 (self_hosted, schema postgres 31)`; a server older than UPD1
/// reports no version.
fn describe(meta: &Value) -> String {
    let version = meta["version"].as_str().unwrap_or("unknown (before UPD1)");
    let edition = meta["edition"].as_str().unwrap_or("unknown edition");
    let schema: Vec<String> = meta["schema"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(store, head)| format!("{store} {head}"))
        .collect();
    if schema.is_empty() {
        format!("{version} ({edition})")
    } else {
        format!("{version} ({edition}, schema {})", schema.join(", "))
    }
}

fn server_meta(host: &str) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(10))
        .build();
    let url = format!("{}/v1/meta", host.trim_end_matches('/'));
    match agent.get(&url).set("Accept", "application/json").call() {
        Ok(response) => response
            .into_json::<Value>()
            .map_err(|_| "the answer is not JSON".to_owned()),
        Err(ureq::Error::Status(status, _)) => Err(format!("HTTP {status}")),
        Err(_) => Err("cannot connect".to_owned()),
    }
}
