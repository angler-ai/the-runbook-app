pub mod admin;
pub mod agent;
pub mod api;
pub mod apps;
pub mod archive;
pub mod auth;
pub mod authoring;
pub mod cliutil;
pub mod config;
pub mod credstore;
pub mod diff;
pub mod evidence;
pub mod exec;
pub mod host;
pub mod keychain;
pub mod logins;
pub mod mcp;
pub mod me;
pub mod memory;
pub mod reports;
pub mod selector;
pub mod session;
pub mod skill;
pub mod sse;
pub mod tokens;
pub mod validate;
pub mod version;
pub mod webhooks;

use serde_json::{Map, Value};

/// `key=value` pairs; a value that parses as JSON keeps its type.
pub fn parse_key_value_pairs(pairs: &[String]) -> Result<Map<String, Value>, String> {
    let mut map = Map::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("invalid key=value pair: '{pair}'"))?;
        let parsed_val = match serde_json::from_str::<Value>(v) {
            Ok(json_val) => json_val,
            Err(_) => Value::String(v.to_string()),
        };
        map.insert(k.to_string(), parsed_val);
    }
    Ok(map)
}
