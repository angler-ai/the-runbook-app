//! DOCS1: the machine-readable reference the docs site renders. `runbook
//! __docs-reference` (hidden, see `main.rs`) prints it as JSON: every command
//! and argument from the CLI's own clap definitions, and every MCP tool,
//! resource template and prompt from `runbook-mcp`. `site/scripts/generate.mjs`
//! turns it into Markdown, so the generated pages can't drift from the code.
use clap::{Arg, ArgAction, Command};
use serde_json::{Value, json};

/// The whole reference: `{ "cli": <command tree>, "mcp": {...}, "spec":
/// {...}, "webhooks": {...} }`.
pub fn reference(cli: &Command) -> Value {
    json!({
        "cli": command(cli, &[]),
        "mcp": {
            "tools": runbook_mcp::tool_definitions(),
            "resource_templates": runbook_mcp::catalog::resource_templates(),
            "prompts": runbook_mcp::catalog::list_prompts(),
        },
        "spec": {
            "keys": spec_keys(),
            "diagnostics": diagnostics(),
        },
        "webhooks": webhooks(),
    })
}

// ------------------------------------------------------------ spec keys ----

/// Every word the capability matrix (`runbook_spec::capabilities`) knows,
/// in source order: the candidates probed at each level of the key tree.
fn vocabulary() -> Vec<&'static str> {
    const SOURCE: &str = include_str!("../../runbook-spec/src/capabilities.rs");
    let mut words: Vec<&'static str> = Vec::new();
    for (i, piece) in SOURCE.split('"').enumerate() {
        let word = i % 2 == 1
            && !piece.is_empty()
            && piece
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && piece.as_bytes()[0].is_ascii_alphabetic();
        if word && !words.contains(&piece) {
            words.push(piece);
        }
    }
    words
}

/// A key exists at `pointer` when the matrix doesn't refuse it as unknown.
/// Keys it refuses outright (removed or forbidden features) are left out;
/// a value-specific refusal (the probe value is `null`) still means the key
/// exists.
fn key_exists(pointer: &str) -> bool {
    match runbook_spec::classify(pointer, &Value::Null) {
        runbook_spec::Support::Rejected { code } => !matches!(
            code.as_str(),
            "spec.unknown_key" | "spec.unsupported_feature" | "spec.agents_dispatch_removed"
        ),
        _ => true,
    }
}

const PROBE: &str = "__docs_probe__";

/// The key tree under `pointer`, found by probing the capability matrix.
/// An object whose keys match one already described points back to it
/// (`same_as`), which is how nested `steps` and declarations stop recursing.
fn shape(
    pointer: &str,
    display: &str,
    vocab: &[&'static str],
    seen: &mut Vec<(String, String)>,
    depth: usize,
) -> Value {
    if depth > 24 {
        return json!({ "kind": "value" });
    }
    let probe = format!("{pointer}/{PROBE}");
    if key_exists(&probe) {
        if key_exists(&format!("{probe}/{PROBE}")) {
            return json!({ "kind": "any" });
        }
        let entry = shape(&probe, &format!("{display}.<name>"), vocab, seen, depth + 1);
        return json!({ "kind": "map", "entry": entry });
    }
    if key_exists(&format!("{pointer}/0")) {
        let item = shape(
            &format!("{pointer}/0"),
            &format!("{display}[]"),
            vocab,
            seen,
            depth + 1,
        );
        return json!({ "kind": "list", "item": item });
    }
    let keys: Vec<&str> = vocab
        .iter()
        .copied()
        .filter(|k| key_exists(&format!("{pointer}/{k}")))
        .collect();
    if keys.is_empty() {
        return json!({ "kind": "value" });
    }
    let fingerprint = keys.join(",");
    if let Some((_, first)) = seen.iter().find(|(f, _)| *f == fingerprint) {
        return json!({ "kind": "object", "same_as": first });
    }
    seen.push((fingerprint, display.to_owned()));
    let children: Vec<Value> = keys
        .iter()
        .map(|k| {
            let path = if display.is_empty() {
                (*k).to_owned()
            } else {
                format!("{display}.{k}")
            };
            json!({
                "name": k,
                "path": path,
                "shape": shape(&format!("{pointer}/{k}"), &path, vocab, seen, depth + 1),
            })
        })
        .collect();
    json!({ "kind": "object", "keys": children })
}

fn spec_keys() -> Value {
    shape("", "", &vocabulary(), &mut Vec::new(), 0)
}

// ---------------------------------------------------------- diagnostics ----

/// Every code in the compiler's message catalog (`runbook_spec::messages`),
/// with the sentence it prints when a diagnostic has no path.
fn diagnostics() -> Value {
    const SOURCE: &str = include_str!("../../runbook-spec/src/messages.rs");
    let mut out = Vec::new();
    for line in SOURCE.lines() {
        let Some(rest) = line.trim_start().strip_prefix('"') else {
            continue;
        };
        let Some((code, tail)) = rest.split_once('"') else {
            continue;
        };
        if tail.trim() != "=> {" || !code.contains('.') {
            continue;
        }
        out.push(json!({
            "code": code,
            "message": runbook_spec::messages::message_for(code, ""),
        }));
    }
    Value::Array(out)
}

// -------------------------------------------------------------- webhooks ----

/// The webhook event catalog and the delivery rules receivers rely on.
fn webhooks() -> Value {
    use runbook_core::webhooks as w;
    let backoff: Vec<i64> = (1..=10).map(w::backoff_ms).collect();
    json!({
        "events": w::CATALOG,
        "human_events": w::HUMAN_EVENTS,
        "test_event": w::TEST_EVENT,
        "all_events": w::ALL_EVENTS,
        "secret_prefix": w::SECRET_PREFIX,
        "signature_tolerance_s": w::SIGNATURE_TOLERANCE_S,
        "subscription_limit": w::SUBSCRIPTION_LIMIT,
        "delivery_timeout_ms": w::DELIVERY_TIMEOUT_MS,
        "retry_window_ms": w::RETRY_WINDOW_MS,
        "rotation_overlap_ms": w::ROTATION_OVERLAP_MS,
        "disable_after_ms": w::DISABLE_AFTER_MS,
        "delivery_log_retention_ms": w::DELIVERY_LOG_RETENTION_MS,
        "url_max_bytes": w::URL_MAX_BYTES,
        "backoff_ms": backoff,
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
    // Flags (`--json`) report `true`/`false` as possible values; they take
    // no value, so leave them out.
    let possible: Vec<String> = if a.is_hide_possible_values_set() || !takes_value {
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

    fn find<'a>(shape: &'a Value, path: &str) -> Option<&'a Value> {
        let keys = match shape["kind"].as_str()? {
            "object" => shape["keys"].as_array()?,
            "list" => return find(&shape["item"], path),
            "map" => return find(&shape["entry"], path),
            _ => return None,
        };
        keys.iter().find_map(|k| {
            if k["path"] == path {
                Some(&k["shape"])
            } else {
                find(&k["shape"], path)
            }
        })
    }

    #[test]
    fn probes_the_key_tree_from_the_capability_matrix() {
        let tree = spec_keys();
        assert_eq!(find(&tree, "metadata.id").unwrap()["kind"], "value");
        assert_eq!(find(&tree, "spec.inputs").unwrap()["kind"], "map");
        assert_eq!(find(&tree, "spec.vars").unwrap()["kind"], "any");
        let foreach = find(&tree, "spec.steps[].foreach.items").unwrap();
        assert_eq!(foreach["kind"], "value");
        // Nested step lists point back to the first step shape.
        let nested = find(&tree, "spec.steps[].steps").unwrap();
        assert_eq!(nested["item"]["same_as"], "spec.steps[]");
        // Removed and forbidden keys are left out.
        assert!(find(&tree, "spec.agents.dispatch").is_none());
        assert!(find(&tree, "spec.steps[].agent.model").is_none());
    }

    #[test]
    fn lists_the_message_catalog_and_webhook_catalog() {
        let codes = diagnostics();
        let unknown = codes
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["code"] == "spec.unknown_key")
            .expect("spec.unknown_key is in the catalog");
        assert!(!unknown["message"].as_str().unwrap().is_empty());
        let hooks = webhooks();
        assert!(
            hooks["events"]
                .as_array()
                .unwrap()
                .contains(&json!("agent.work_available"))
        );
        assert_eq!(hooks["backoff_ms"][0], 30_000);
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
